"""Explicit, pinned model acquisition. No typing content or settings are touched."""
from __future__ import annotations

from dataclasses import dataclass
import hashlib
import os
from pathlib import Path
from threading import Event
import tempfile
from typing import Callable
from urllib.parse import urlsplit
import urllib.request

from platformdirs import user_cache_path


@dataclass(frozen=True)
class ModelPreset:
    id: str
    label: str
    description: str
    repo: str
    revision: str
    filename: str
    size_bytes: int
    sha256: str
    prompt_style: str = "continuation"


DEFAULT_PRESET = "balanced"
PRESETS = (
    ModelPreset(
        id="balanced", label="Balanced — Qwen2.5 1.5B Writing",
        description="Recommended guided writing model. Multilingual, short draft continuations; 1.12 GB download. GPU acceleration recommended.",
        repo="Qwen/Qwen2.5-1.5B-Instruct-GGUF",
        revision="91cad51170dc346986eccefdc2dd33a9da36ead9",
        filename="qwen2.5-1.5b-instruct-q4_k_m.gguf", size_bytes=1117320736,
        sha256="6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e",
        prompt_style="writing",
    ),
    ModelPreset(
        id="tiny", label="Tiny — SmolLM2 135M (legacy/testing)",
        description="101 MB English starter for low-resource machines and tests. Limited suggestion quality; not recommended for everyday writing.",
        repo="ggml-org/SmolLM2-135M-GGUF",
        revision="1076b85d135017971845b19b8d07df1479155988",
        filename="SmolLM2-135M-Q4_K_M.gguf", size_bytes=101016128,
        sha256="e3131339bf4e8065265593d4fd8f7bb7ff2d3abff1edb5618aa1197b89cad9f5",
    ),
)


def get_preset(preset_id: str) -> ModelPreset:
    for preset in PRESETS:
        if preset.id == preset_id:
            return preset
    raise ValueError(f"Unknown model preset: {preset_id}")


def model_path(preset: ModelPreset) -> Path:
    return user_cache_path("inklow", appauthor=False) / "models" / preset.filename


def is_installed(preset: ModelPreset) -> bool:
    """Cheap inventory only; download_model verifies SHA-256 before returning."""
    path = model_path(preset)
    try:
        return path.is_file() and path.stat().st_size == preset.size_bytes
    except OSError:
        return False


def _check_cancel(cancel: Event | None) -> None:
    if cancel is not None and cancel.is_set():
        raise RuntimeError("Model download cancelled.")


def _digest(path: Path, cancel: Event | None) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            _check_cancel(cancel)
            digest.update(chunk)
    return digest.hexdigest()


class _HTTPSRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if urlsplit(newurl).scheme != "https":
            raise RuntimeError("Refusing a non-HTTPS model download redirect.")
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def download_model(
    preset_id: str = DEFAULT_PRESET,
    progress: Callable[[int, int], None] | None = None,
    cancel: Event | None = None,
) -> Path:
    """Return verified weights, preserving the old model on cancel or failure."""
    preset = get_preset(preset_id)
    target = model_path(preset)
    _check_cancel(cancel)
    target.parent.mkdir(parents=True, exist_ok=True)
    if progress is not None:
        progress(0, preset.size_bytes)
    if is_installed(preset) and _digest(target, cancel) == preset.sha256:
        if progress is not None:
            progress(preset.size_bytes, preset.size_bytes)
        return target
    fd, temporary = tempfile.mkstemp(prefix=".download-", dir=target.parent)
    try:
        count = 0
        digest = hashlib.sha256()
        url = f"https://huggingface.co/{preset.repo}/resolve/{preset.revision}/{preset.filename}"
        request = urllib.request.Request(url, headers={"User-Agent": "inklow/0.2"})
        opener = urllib.request.build_opener(_HTTPSRedirects())
        with os.fdopen(fd, "wb") as destination:
            with opener.open(request, timeout=30) as response:
                while True:
                    _check_cancel(cancel)
                    chunk = response.read(1024 * 1024)
                    if not chunk:
                        break
                    count += len(chunk)
                    if count > preset.size_bytes:
                        raise RuntimeError("Model download exceeds its verified size.")
                    digest.update(chunk)
                    destination.write(chunk)
                    if progress is not None:
                        progress(count, preset.size_bytes)
        _check_cancel(cancel)
        if count != preset.size_bytes or digest.hexdigest() != preset.sha256:
            raise RuntimeError("Model integrity verification failed; the existing model was not replaced.")
        os.replace(temporary, target)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    return target
