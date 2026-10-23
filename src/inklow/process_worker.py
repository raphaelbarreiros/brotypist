"""Private anonymous-pipe inference worker, dispatched by ``inklow _worker``.

Only this process may import llama_cpp. Protocol stdout is a duplicated fd;
Python/native model diagnostics are discarded, never written to a draft log.
"""
from __future__ import annotations

from dataclasses import fields
import json
import os
import sys
import threading

from .config import Config

MAX_MESSAGE_BYTES = 256 * 1024
MAX_RESULT_CHARS = 4096


def _validate_request(value: dict) -> tuple[int, Config, dict]:
    identifier = value.get("id")
    if type(identifier) is not int or identifier < 1:
        raise ValueError("Invalid request identifier")
    raw = value.get("config")
    if not isinstance(raw, dict) or set(raw) - {item.name for item in fields(Config)}:
        raise ValueError("Invalid worker configuration")
    config = Config(**raw)
    config.validate()
    payload = {}
    limits = {"prefix": 8000, "suffix": 8000, "instructions": 26000, "context": 8000, "utility": 24000}
    for key, limit in limits.items():
        text = value.get(key, "")
        if not isinstance(text, str) or len(text) > limit:
            raise ValueError("Invalid request text bound")
        payload[key] = text
    words = value.get("max_words", 8)
    if type(words) is not int or not 1 <= words <= 32:
        raise ValueError("Invalid completion length")
    payload["max_words"] = words
    return identifier, config, payload


def main() -> int:
    protocol = os.fdopen(os.dup(sys.stdout.fileno()), "wb", buffering=0)
    null = os.open(os.devnull, os.O_WRONLY)
    os.dup2(null, 1)
    os.dup2(null, 2)
    os.close(null)
    output_lock = threading.Lock()
    condition = threading.Condition()
    pending = None
    active_cancel = threading.Event()
    stopped = False

    def send(value: dict):
        data = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode() + b"\n"
        if len(data) > MAX_MESSAGE_BYTES:
            return
        with output_lock:
            view = memoryview(data)
            while view:
                written = protocol.write(view)
                if not written:
                    raise BrokenPipeError("Worker pipe closed")
                view = view[written:]

    def generate():
        nonlocal pending, active_cancel
        from .providers import create_provider
        provider = None
        configured = None
        provider_ready = False
        try:
            while True:
                with condition:
                    condition.wait_for(lambda: pending is not None or stopped)
                    if stopped:
                        return
                    identifier, config, payload = pending
                    pending = None
                    cancel = active_cancel = threading.Event()
                try:
                    identity = tuple(getattr(config, name) for name in ("backend", "model_path", "endpoint", "model", "acceleration", "prompt_style"))
                    if configured != identity:
                        provider_ready = False
                        if provider is not None:
                            provider.close()
                        provider = create_provider(config)
                        configured = identity
                        send({"type": "loading", "id": identifier})
                        if config.backend == "gguf":
                            provider.prepare()
                            provider_ready = True
                    provider.config = config
                    if cancel.is_set():
                        continue
                    send({"type": "ready", "id": identifier, "model_loaded": provider_ready})
                    text = ""
                    options = {key: value for key, value in payload.items() if key not in {"prefix", "suffix"}}
                    for text in provider.stream(payload["prefix"], payload["suffix"], cancel, options=options):
                        if cancel.is_set():
                            break
                        if not isinstance(text, str) or len(text) > MAX_RESULT_CHARS:
                            raise ValueError("Invalid model response")
                        send({"type": "result", "id": identifier, "text": text, "final": False})
                    if not cancel.is_set():
                        send({"type": "result", "id": identifier, "text": text, "final": True})
                except Exception:
                    if not cancel.is_set():
                        # A backend/server exception can echo writing. Report a
                        # fixed actionable diagnostic, never its arbitrary body.
                        send({"type": "error", "id": identifier, "message": "Local inference failed. Check the chosen model file, local inference installation, and local server availability."})
                finally:
                    send({"type": "done", "id": identifier, "model_loaded": provider_ready})
        finally:
            if provider is not None:
                provider.close()

    thread = threading.Thread(target=generate, daemon=True, name="inference")
    thread.start()
    try:
        while True:
            line = sys.stdin.buffer.readline(MAX_MESSAGE_BYTES + 1)
            if not line:
                break
            if len(line) > MAX_MESSAGE_BYTES or not line.endswith(b"\n"):
                return 2
            try:
                value = json.loads(line)
                if not isinstance(value, dict):
                    raise ValueError("Invalid protocol frame")
                kind = value.get("type")
                if kind == "generate":
                    request = _validate_request(value)
                    with condition:
                        active_cancel.set()
                        superseded = pending[0] if pending is not None else None
                        pending = request
                        condition.notify()
                    if superseded is not None:
                        send({"type": "done", "id": superseded})
                elif kind == "cancel":
                    with condition:
                        active_cancel.set()
                        superseded = pending[0] if pending is not None else None
                        pending = None
                        condition.notify()
                    if superseded is not None:
                        send({"type": "done", "id": superseded})
                elif kind == "stop":
                    break
                else:
                    raise ValueError("Unknown protocol message")
            except (ValueError, TypeError, KeyError):
                return 2
    finally:
        with condition:
            stopped = True
            active_cancel.set()
            pending = None
            condition.notify()
        thread.join(timeout=0.2)
        protocol.close()
    return 0
