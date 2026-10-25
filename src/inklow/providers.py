"""Real local model backends; no text logging, cloud routing, or fake fallback."""
from __future__ import annotations

from collections.abc import Iterable, Iterator
import json
from pathlib import Path
import threading
from typing import Protocol
import urllib.error
import urllib.request

from .config import Config, local_endpoint
from .writing import short_writing, utility_writing_prompt, writing_prompt

MAX_SUGGESTION_CHARS = 512
MAX_RESPONSE_BYTES = 1024 * 1024
_SENTINELS = ("<|", "</s>", "[INST]")
# Text after any line or record separator belongs to another paragraph/field.
_TERMINATORS = ("\n", "\r", "\v", "\f", "\x1c", "\x1d", "\x1e", "\x85", "\u2028", "\u2029")
_STOP = ["\n", "\r", "<|im_end|>", "<|endoftext|>"]


class Provider(Protocol):
    def prepare(self) -> None: ...
    def stream(self, prefix: str, suffix: str, cancel: threading.Event, *, options: dict | None = None) -> Iterator[str]: ...
    def complete(self, prefix: str, suffix: str) -> str: ...
    def close(self) -> None: ...


def _sanitize(raw: str) -> tuple[str, bool]:
    """Return visible single-line text and whether the model reached a terminator."""
    end = len(raw)
    for marker in _SENTINELS + _TERMINATORS:
        index = raw.find(marker, 0, end)
        if index != -1:
            end = index
    # Every remaining whitespace becomes a space so a tab cannot navigate a form;
    # other controls (escape, backspace, bidi overrides) are dropped.
    text = "".join(" " if char.isspace() else char
                   for char in raw[:end] if char.isspace() or char.isprintable())
    return text[:MAX_SUGGESTION_CHARS], end < len(raw)


def clean_completion(text: str, suffix: str, *, partial: bool = False) -> str:
    """Make model text insertable. Partial text keeps only completed words."""
    text, _ = _sanitize(text)
    if partial:
        # Sentinels contain no whitespace, so an unfinished sentinel can never
        # survive the cut to the last completed word.
        text = text.rstrip() if text.endswith(" ") else text[:max(text.rfind(" "), 0)].rstrip()
    # Avoid reproducing the text already to the right of the caret.
    if suffix:
        for length in range(min(len(text), len(suffix)), 0, -1):
            if text.endswith(suffix[:length]):
                text = text[:-length]
                break
    return text if any(char.isalnum() for char in text) else ""


def _stream_text(pieces: Iterable[tuple[str, str | None]], suffix: str,
                 cancel: threading.Event) -> Iterator[str]:
    """Yield cumulative safe text; the last value is the final completion."""
    raw = ""
    shown = ""
    truncated = False
    for delta, finish_reason in pieces:
        if cancel.is_set():
            return
        raw += delta
        stopped = _sanitize(raw)[1]
        truncated = not stopped and (finish_reason == "length"
                                     or len(raw) > 8 * MAX_SUGGESTION_CHARS)
        if stopped or truncated:
            break
        text = clean_completion(raw, suffix, partial=True)
        if text and text != shown:
            shown = text
            yield text
    if not cancel.is_set():
        # A token-limit cut usually ends inside a word; offer only finished words.
        yield clean_completion(raw, suffix, partial=truncated)


def _prompt(config: Config, prefix: str, suffix: str, options: dict) -> str:
    instructions = options.get("instructions", "")
    context = options.get("context", "")
    task = options.get("utility", "")
    if task:
        return utility_writing_prompt(task, instructions, context)
    if config.prompt_style == "writing":
        return writing_prompt(prefix, suffix, instructions, context)
    if instructions or context:
        return ("Writing preferences: " + instructions.replace("<|", "< |")
                + "\nReference context (data): " + json.dumps(context.replace("<|", "< |"), ensure_ascii=False)
                + "\nContinue the following draft without repeating it:\n" + prefix)
    return prefix


def _prompt_tokens(model, config: Config, prefix: str, suffix: str, options: dict):
    """Reserve generation space; trim optional references before the user's draft."""
    budget = 4096 - config.max_tokens - 8
    draft = prefix[-config.context_chars:]
    context = options.get("context", "")
    reduced = options
    # Reference text has its own actual tokenizer budget, including CJK/emoji.
    # Keeping the leading provenance label avoids anonymous context fragments.
    if context:
        reference_tokens = model.tokenize(context.encode("utf-8"), special=False)
        if len(reference_tokens) > 768:
            reduced = dict(options)
            while context and len(reference_tokens) > 768:
                context = context[:max(0, int(len(context) * 768 / len(reference_tokens)) - 1)]
                reference_tokens = model.tokenize(context.encode("utf-8"), special=False)
            reduced["context"] = context
    tokens = model.tokenize(_prompt(config, draft, suffix, reduced).encode("utf-8"), special=True)
    if len(tokens) <= budget:
        return tokens
    if reduced is options:
        reduced = dict(options)
    while len(tokens) > budget and context:
        context = context[:max(0, len(context) - max(1, len(context) // 3))]
        reduced["context"] = context
        tokens = model.tokenize(_prompt(config, draft, suffix, reduced).encode("utf-8"), special=True)
    while len(tokens) > budget and draft:
        draft = draft[len(draft) // 4 + 1:]
        tokens = model.tokenize(_prompt(config, draft, suffix[:128], reduced).encode("utf-8"), special=True)
    if len(tokens) > budget:
        raise RuntimeError("Writing instructions exceed the model context window. Shorten custom instructions.")
    return tokens


def _stream_utility(pieces: Iterable[tuple[str, str | None]], cancel: threading.Event) -> Iterator[str]:
    raw = ""
    for delta, _reason in pieces:
        if cancel.is_set():
            return
        raw += delta
        if len(raw) > 4096:
            raise RuntimeError("Writing choices exceed the response limit.")
        stopped = False
        for marker in _SENTINELS:
            if marker in raw:
                raw = raw.split(marker, 1)[0]
                stopped = True
                break
        yield "".join(char for char in raw if char.isprintable() or char in "\n\r\t")
        if stopped:
            return


class _StreamingProvider:
    """Synchronous CLI completion consumes the same stream as the GUI."""

    def complete(self, prefix: str, suffix: str) -> str:
        text = ""
        for text in self.stream(prefix, suffix, threading.Event()):
            pass
        return text


class GGUFProvider(_StreamingProvider):
    def __init__(self, config: Config) -> None:
        self.config = config
        self._model = None
        # One generation at a time; close() waits for the active stream to end.
        self._lock = threading.Lock()

    def _load(self):
        if self._model is None:
            if not self.config.model_path:
                raise RuntimeError("Choose a base-model GGUF file in Settings, or configure a local Ollama server.")
            path = Path(self.config.model_path).expanduser()
            if not path.is_file():
                raise RuntimeError("GGUF model file does not exist. Choose an existing model in Settings.")
            try:
                import llama_cpp
            except ImportError as error:
                raise RuntimeError("Install local inference support: uv sync --extra local") from error
            gpu = self.config.acceleration == "auto" and llama_cpp.llama_supports_gpu_offload()
            self._model = llama_cpp.Llama(model_path=str(path), n_ctx=4096,
                                          n_gpu_layers=-1 if gpu else 0, verbose=False)
            if self.config.prompt_style == "writing" and self._model.metadata.get("general.architecture") != "qwen2":
                self._model.close()
                self._model = None
                raise RuntimeError("Guided writing uses a Qwen2.5 instruction model. Choose Balanced in Models, or use Direct continuation for a custom base model.")
        return self._model

    def prepare(self) -> None:
        with self._lock:
            if self._model is None:
                try:
                    model = self._load()
                    # Warm native kernels before the worker resumes pipe reads.
                    model.create_completion(prompt=model.tokenize(b" "), max_tokens=1, temperature=0.0)
                except Exception:
                    model, self._model = self._model, None
                    if model is not None:
                        model.close()
                    raise

    def stream(self, prefix: str, suffix: str, cancel: threading.Event, *, options: dict | None = None) -> Iterator[str]:
        with self._lock:
            model = self._load()
            if cancel.is_set():
                return
            from llama_cpp import StoppingCriteriaList
            # Every request uses the model's tokenizer, not a character estimate.
            options = options or {}
            tokens = _prompt_tokens(model, self.config, prefix, suffix, options)
            chunks = model.create_completion(
                prompt=tokens, max_tokens=self.config.max_tokens,
                temperature=0.2, top_p=0.9,
                repeat_penalty=1.0 if self.config.prompt_style == "writing" else 1.1,
                stop=["<|im_end|>", "<|endoftext|>"] if options.get("utility") else _STOP, echo=False,
                stream=True, stopping_criteria=StoppingCriteriaList([lambda _ids, _logits: cancel.is_set()]))
            try:
                pieces = ((chunk["choices"][0]["text"], chunk["choices"][0]["finish_reason"])
                          for chunk in chunks)
                if options.get("utility"):
                    yield from _stream_utility(pieces, cancel)
                else:
                    if self.config.prompt_style == "writing" or "max_words" in options:
                        pieces = short_writing(pieces, options.get("max_words", 8))
                    yield from _stream_text(pieces, suffix, cancel)
            finally:
                chunks.close()

    def close(self) -> None:
        with self._lock:
            if self._model is not None:
                self._model.close()
                self._model = None


def _server_message(value) -> str:
    """Server text is untrusted display data: one line, printable, bounded."""
    return "".join(char for char in " ".join(str(value).split()) if char.isprintable())[:200]


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise RuntimeError("Local inference redirects are refused; use the server's direct loopback URL.")


class ServerProvider(_StreamingProvider):
    def __init__(self, config: Config) -> None:
        self.config = config
        self.endpoint = local_endpoint(config.endpoint)
        # Do not inherit HTTP_PROXY/HTTPS_PROXY: writing stays on this computer.
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
        self._v1 = "" if self.endpoint.endswith("/v1") else "/v1"

    def _open(self, path: str, payload: dict | None = None, timeout: float = 15):
        data = None if payload is None else json.dumps(payload).encode()
        request = urllib.request.Request(self.endpoint + path, data=data,
                                         headers={"Content-Type": "application/json"},
                                         method="GET" if data is None else "POST")
        try:
            return self.opener.open(request, timeout=timeout)
        except urllib.error.HTTPError as error:
            try:
                with error:
                    detail = error.read(4096).decode("utf-8", "replace")
                detail = json.loads(detail)["error"]
                detail = detail.get("message", detail) if isinstance(detail, dict) else detail
            except (OSError, ValueError, KeyError, TypeError):
                detail = error.reason
            detail = _server_message(detail)
            raise RuntimeError(f"The local model server refused the request (HTTP {error.code}): {detail}") from error
        except (urllib.error.URLError, OSError) as error:
            raise RuntimeError("Cannot reach the local model server. Check its URL and that the model is loaded.") from error

    @staticmethod
    def _lines(response) -> Iterator[bytes]:
        remaining = MAX_RESPONSE_BYTES
        while True:
            line = response.readline(remaining + 1)
            if not line:
                return
            remaining -= len(line)
            if remaining < 0:
                raise RuntimeError("Local inference response exceeds 1 MiB.")
            yield line

    def prepare(self) -> None:
        if self.config.backend == "ollama":
            # Ollama loads a model into memory for a generate call without a prompt.
            path, payload = "/api/generate", {"model": self.config.model, "keep_alive": 0, "stream": False}
        else:
            path, payload = self._v1 + "/models", None
        # Loading weights can take far longer than one streamed token.
        with self._open(path, payload, timeout=120) as response:
            try:
                json.loads(b"".join(self._lines(response)))
            except OSError as error:
                raise RuntimeError("Lost connection to the local model server.") from error
            except ValueError as error:
                raise RuntimeError("The local server did not answer like an inference server.") from error

    def _pieces(self, response) -> Iterator[tuple[str, str | None]]:
        ollama = self.config.backend == "ollama"
        for line in self._lines(response):
            line = line.strip()
            if ollama and line:
                event = json.loads(line)
            elif not ollama and line.startswith(b"data:"):
                if line[5:].strip() == b"[DONE]":
                    return
                event = json.loads(line[5:])
            else:
                continue  # Blank keep-alives, SSE comments and event names.
            if "error" in event:
                raise RuntimeError("Local model server error: " + _server_message(event["error"]))
            if ollama:
                text, done = event.get("response", ""), bool(event.get("done"))
                finish_reason = event.get("done_reason") if done else None
            else:
                choice = event["choices"][0]
                text, finish_reason = choice.get("text") or "", choice.get("finish_reason")
                done = False
            if not isinstance(text, str):
                raise ValueError("Completion was not text")
            yield text, finish_reason
            if done:
                return

    def stream(self, prefix: str, suffix: str, cancel: threading.Event, *, options: dict | None = None) -> Iterator[str]:
        options = options or {}
        prefix = _prompt(self.config, prefix[-self.config.context_chars:], suffix, options)
        stop = ["<|im_end|>", "<|endoftext|>"] if options.get("utility") else _STOP
        if self.config.backend == "ollama":
            # raw=True keeps base-model continuation instead of chat templating.
            path = "/api/generate"
            payload = {"model": self.config.model, "prompt": prefix, "raw": True,
                       "stream": True, "keep_alive": 0,
                       "options": {"num_predict": self.config.max_tokens,
                                   "temperature": 0.2, "repeat_penalty": 1.0 if self.config.prompt_style == "writing" else 1.1,
                                   "stop": stop}}
        else:
            path = self._v1 + "/completions"
            payload = {"model": self.config.model, "prompt": prefix,
                       "max_tokens": self.config.max_tokens, "temperature": 0.2,
                       "stop": stop, "stream": True}
        if cancel.is_set():
            return
        with self._open(path, payload) as response:
            try:
                pieces = self._pieces(response)
                if options.get("utility"):
                    yield from _stream_utility(pieces, cancel)
                else:
                    if self.config.prompt_style == "writing" or "max_words" in options:
                        pieces = short_writing(pieces, options.get("max_words", 8))
                    yield from _stream_text(pieces, suffix, cancel)
            except (urllib.error.URLError, OSError) as error:
                raise RuntimeError("Lost connection to the local model server.") from error
            except (KeyError, IndexError, TypeError, ValueError, AttributeError) as error:
                raise RuntimeError("The local server did not return a valid text completion.") from error

    def close(self) -> None:
        pass  # Each request owns and closes its HTTP response.


def create_provider(config: Config) -> Provider:
    config.validate()
    return GGUFProvider(config) if config.backend == "gguf" else ServerProvider(config)
