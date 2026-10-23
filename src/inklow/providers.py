"""Real local model backends; no text logging, cloud routing, or fake fallback."""
from __future__ import annotations

import json
from pathlib import Path
import threading
from typing import Protocol
import urllib.error
import urllib.request

from .config import Config, local_endpoint


class Provider(Protocol):
    def complete(self, prefix: str, suffix: str) -> str: ...
    def close(self) -> None: ...


def clean_completion(text: str, suffix: str) -> str:
    # Suggestions never add new paragraphs, control characters or model sentinels.
    for sentinel in ("<|", "</s>", "[INST]"):
        text = text.split(sentinel, 1)[0]
    text = text.split("\n", 1)[0].split("\r", 1)[0]
    # A generated tab must not navigate a form before the remaining text lands.
    text = text.replace("\t", " ")
    text = "".join(char for char in text if char.isprintable())[:512]
    # Avoid reproducing the text already to the right of the caret.
    if suffix:
        for length in range(min(len(text), len(suffix)), 0, -1):
            if text.endswith(suffix[:length]):
                text = text[:-length]
                break
    return text if text.strip() else ""


class GGUFProvider:
    def __init__(self, config: Config) -> None:
        self.config = config
        self._model = None
        self._lock = threading.RLock()

    def complete(self, prefix: str, suffix: str) -> str:
        with self._lock:
            if self._model is None:
                if not self.config.model_path:
                    raise RuntimeError("Choose a base-model GGUF file in Settings, or configure a local Ollama server.")
                path = Path(self.config.model_path).expanduser()
                if not path.is_file():
                    raise RuntimeError("GGUF model file does not exist. Choose an existing model in Settings.")
                try:
                    from llama_cpp import Llama
                except ImportError as error:
                    raise RuntimeError("Install local inference support: uv sync --extra local") from error
                self._model = Llama(model_path=str(path), n_ctx=4096,
                                    n_gpu_layers=0, verbose=False)
            # Token-bound the prompt, not just character-bound: Unicode can expand.
            tokens = self._model.tokenize(prefix[-self.config.context_chars:].encode("utf-8"))
            tokens = tokens[-(4096 - self.config.max_tokens - 8):]
            result = self._model.create_completion(
                prompt=tokens, max_tokens=self.config.max_tokens,
                temperature=0.2, top_p=0.9, repeat_penalty=1.1,
                stop=["\n", "\r", "<|im_end|>", "<|endoftext|>"], echo=False)
            return clean_completion(result["choices"][0]["text"], suffix)

    def close(self) -> None:
        with self._lock:
            if self._model is not None:
                self._model.close()
                self._model = None


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise RuntimeError("Local inference redirects are refused; use the server's direct loopback URL.")


class ServerProvider:
    def __init__(self, config: Config) -> None:
        self.config = config
        self.endpoint = local_endpoint(config.endpoint)
        # Do not inherit HTTP_PROXY/HTTPS_PROXY: writing stays on this computer.
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

    def complete(self, prefix: str, suffix: str) -> str:
        prefix = prefix[-self.config.context_chars:]
        if self.config.backend == "ollama":
            path = "/api/generate"
            payload = {"model": self.config.model, "prompt": prefix, "raw": True,
                       "stream": False, "options": {"num_predict": self.config.max_tokens,
                       "temperature": 0.2, "stop": ["\n", "\r", "<|im_end|>"]}}
        else:
            path = "/completions" if self.endpoint.endswith("/v1") else "/v1/completions"
            payload = {"model": self.config.model, "prompt": prefix,
                       "max_tokens": self.config.max_tokens, "temperature": 0.2,
                       "stop": ["\n", "\r", "<|im_end|>"], "stream": False}
        request = urllib.request.Request(self.endpoint + path, data=json.dumps(payload).encode(),
                                         headers={"Content-Type": "application/json"}, method="POST")
        try:
            with self.opener.open(request, timeout=15) as response:
                body = response.read(1024 * 1024 + 1)
                if len(body) > 1024 * 1024:
                    raise RuntimeError("Local inference response exceeds 1 MiB.")
                result = json.loads(body)
            if self.config.backend == "ollama":
                text = result["response"]
            else:
                text = result["choices"][0]["text"]
            if not isinstance(text, str):
                raise ValueError("Completion was not text")
            return clean_completion(text, suffix)
        except (urllib.error.URLError, OSError) as error:
            raise RuntimeError("Cannot reach the local model server. Check its URL and that the model is loaded.") from error
        except (KeyError, IndexError, TypeError, ValueError) as error:
            raise RuntimeError("The local server did not return a valid text completion.") from error

    def close(self) -> None:
        pass  # urllib response resources are closed per request.


def create_provider(config: Config) -> Provider:
    config.validate()
    return GGUFProvider(config) if config.backend == "gguf" else ServerProvider(config)
