"""Explicit, pinned model acquisition. No typing content or settings are touched."""
from __future__ import annotations

from contextlib import contextmanager, nullcontext
from dataclasses import dataclass
from functools import cache
import hashlib
import os
from pathlib import Path
import socket
import subprocess
import sys
from threading import Event, Lock
import tempfile
from typing import Callable
from urllib.parse import urlsplit
import urllib.request

from platformdirs import user_cache_path


@dataclass(frozen=True)
class ModelPreset:
    """Pinned weight identity, license policy and physical-memory floor in bytes."""

    id: str
    label: str
    description: str
    repo: str
    revision: str
    filename: str
    size_bytes: int
    sha256: str
    prompt_style: str = "continuation"
    license_name: str = "Apache-2.0"
    license_url: str = "https://www.apache.org/licenses/LICENSE-2.0"
    requires_license_acceptance: bool = False
    minimum_memory_bytes: int = 0


DEFAULT_PRESET = "gemma4-e2b"
PRESETS = (
    ModelPreset(
        id="gemma4-e2b", label="Gemma 4 E2B",
        description="On-device multilingual writing; 2.84 GB verified download. Apple Silicon Metal recommended.",
        repo="ggml-org/gemma-4-E2B-it-GGUF",
        revision="6938c58e698502037401a5f19f6905f2fb2f7dee",
        filename="gemma-4-E2B-it-Q4_0.gguf", size_bytes=2841481184,
        sha256="8e30dff3ac4c8434c49a7036fa15564bdbb6044e42bf04550bf1a096ad7e6a52",
        prompt_style="writing", minimum_memory_bytes=8 * 1024**3,
    ),
    ModelPreset(
        id="gemma4-e4b", label="Gemma 4 E4B",
        description="Larger multilingual writer; 4.59 GB download. More memory and generation time than E2B.",
        repo="ggml-org/gemma-4-E4B-it-GGUF",
        revision="a69f018d7678a2b9f5e705f8903d6fb29b9b25dc",
        filename="gemma-4-E4B-it-Q4_0.gguf", size_bytes=4590807392,
        sha256="a555b900214b477d8880e7832e0b8925e139b0159640036b09fe472b6f2097f2",
        prompt_style="writing", minimum_memory_bytes=16 * 1024**3,
    ),
    ModelPreset(
        id="gemma4-26b-a4b", label="Gemma 4 26B-A4B",
        description="Mixture-of-experts writer; 14.62 GB of weights despite fewer active parameters. Requires a high-memory Mac.",
        repo="ggml-org/gemma-4-26B-A4B-it-GGUF",
        revision="9cf8c8db0f83fb4582f15381f8fb1a7c1b0b9ca2",
        filename="gemma-4-26B-A4B-it-Q4_0.gguf", size_bytes=14618145824,
        sha256="d208665ab1cd3a69f7a9a4bc59430e8448c8093d9b06334f566ac59d6d504a03",
        prompt_style="writing", minimum_memory_bytes=32 * 1024**3,
    ),
    ModelPreset(
        id="gemma3-1b", label="Gemma 3 1B",
        description="Smaller multilingual writer; 806 MB download. Subject to the Gemma Terms of Use.",
        repo="ggml-org/gemma-3-1b-it-GGUF",
        revision="f9c28bcd85737ffc5aef028638d3341d49869c27",
        filename="gemma-3-1b-it-Q4_K_M.gguf", size_bytes=806058240,
        sha256="8ccc5cd1f1b3602548715ae25a66ed73fd5dc68a210412eea643eb20eb75a135",
        prompt_style="writing", license_name="Gemma Terms of Use",
        license_url="https://ai.google.dev/gemma/terms",
        requires_license_acceptance=True, minimum_memory_bytes=4 * 1024**3,
    ),
    ModelPreset(
        id="gemma3-4b", label="Gemma 3 4B",
        description="Multilingual writer; 2.49 GB text weights. Subject to the Gemma Terms of Use.",
        repo="ggml-org/gemma-3-4b-it-GGUF",
        revision="d0976223747697cb51e056d85c532013931fe52e",
        filename="gemma-3-4b-it-Q4_K_M.gguf", size_bytes=2489757856,
        sha256="882e8d2db44dc554fb0ea5077cb7e4bc49e7342a1f0da57901c0802ea21a0863",
        prompt_style="writing", license_name="Gemma Terms of Use",
        license_url="https://ai.google.dev/gemma/terms",
        requires_license_acceptance=True, minimum_memory_bytes=8 * 1024**3,
    ),
    ModelPreset(
        id="qwen3-1.7b", label="Qwen 3 1.7B",
        description="Compact multilingual writer in non-thinking mode; 1.28 GB download.",
        repo="ggml-org/Qwen3-1.7B-GGUF",
        revision="daeb8e2d528a760970442092f6bf1e55c3b659eb",
        filename="Qwen3-1.7B-Q4_K_M.gguf", size_bytes=1282439264,
        sha256="d2387ca2dbfee2ffabce7120d3770dadca0b293052bc2f0e138fdc940d9bc7b5",
        prompt_style="writing", minimum_memory_bytes=4 * 1024**3,
    ),
    ModelPreset(
        id="qwen3-4b", label="Qwen 3 4B",
        description="Multilingual writer in non-thinking mode; 2.50 GB download.",
        repo="Qwen/Qwen3-4B-GGUF",
        revision="bc640142c66e1fdd12af0bd68f40445458f3869b",
        filename="Qwen3-4B-Q4_K_M.gguf", size_bytes=2497280256,
        sha256="7485fe6f11af29433bc51cab58009521f205840f5b4ae3a32fa7f92e8534fdf5",
        prompt_style="writing", minimum_memory_bytes=8 * 1024**3,
    ),
    ModelPreset(
        id="qwen3-8b", label="Qwen 3 8B",
        description="Larger multilingual writer in non-thinking mode; 5.03 GB download.",
        repo="Qwen/Qwen3-8B-GGUF",
        revision="7c41481f57cb95916b40956ab2f0b139b296d974",
        filename="Qwen3-8B-Q4_K_M.gguf", size_bytes=5027783488,
        sha256="d98cdcbd03e17ce47681435b5150e34c1417f50b5c0019dd560e4882c5745785",
        prompt_style="writing", minimum_memory_bytes=16 * 1024**3,
    ),
    ModelPreset(
        id="qwen3-30b-a3b", label="Qwen 3 30B-A3B",
        description="Mixture-of-experts writer; 18.56 GB of weights. Requires a high-memory Mac.",
        repo="Qwen/Qwen3-30B-A3B-GGUF",
        revision="e4d4bafdfb96a411a163846265362aceb0b9c63a",
        filename="Qwen3-30B-A3B-Q4_K_M.gguf", size_bytes=18556685824,
        sha256="0d003f6662faee786ed5da3e31b29c978de5ae5d275c8794c606a7f3c01aa8f5",
        prompt_style="writing", minimum_memory_bytes=32 * 1024**3,
    ),
    ModelPreset(
        id="balanced", label="Balanced — Qwen2.5 1.5B Writing",
        description="Previous guided writing model; 1.12 GB download. Kept available without changing existing selections.",
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
    """Resolve a configured preset ID, raising ValueError for unknown IDs."""
    for preset in PRESETS:
        if preset.id == preset_id:
            return preset
    raise ValueError(f"Unknown model preset: {preset_id}")


@cache
def physical_memory_bytes() -> int | None:
    """Physical capacity, not available memory or an inference-speed promise."""
    try:
        if sys.platform == "darwin":
            return int(subprocess.check_output(
                ["/usr/sbin/sysctl", "-n", "hw.memsize"], text=True, timeout=2).strip())
        return int(os.sysconf("SC_PHYS_PAGES")) * int(os.sysconf("SC_PAGE_SIZE"))
    except (OSError, ValueError, subprocess.SubprocessError):
        return None


def recommended_preset(memory_bytes: int | None = None) -> ModelPreset:
    """Prefer the measured writing profile, not the largest downloadable file."""
    capacity = physical_memory_bytes() if memory_bytes is None else memory_bytes
    return get_preset("balanced" if capacity is not None and capacity < 8 * 1024**3 else DEFAULT_PRESET)


def model_availability(preset: ModelPreset, memory_bytes: int | None = None) -> str:
    """Return a memory-capacity warning, or an empty string if not ruled out."""
    capacity = physical_memory_bytes() if memory_bytes is None else memory_bytes
    if capacity is not None and capacity < preset.minimum_memory_bytes:
        return f"Requires at least {preset.minimum_memory_bytes // 1024**3} GB of physical memory."
    return ""


def model_path(preset: ModelPreset) -> Path:
    """Return the per-user cache destination without creating or verifying it."""
    return user_cache_path("inklow", appauthor=False) / "models" / preset.filename


def is_installed(preset: ModelPreset) -> bool:
    """Cheap inventory only; download_model verifies SHA-256 before returning."""
    path = model_path(preset)
    try:
        return path.is_file() and path.stat().st_size == preset.size_bytes
    except OSError:
        return False


class DownloadCancellation(Event):
    """Interrupt an owned urllib body read without waiting for its IO lock."""

    def __init__(self):
        super().__init__()
        self._interrupt_lock = Lock()
        self._socket = None

    def _interrupt(self):
        """Shut down the registered socket while holding ``_interrupt_lock``."""
        if self.is_set() and self._socket is not None:
            try:
                self._socket.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass  # The owned connection may already have closed.

    def set(self):
        """Signal cancellation and interrupt an attached response body read."""
        super().set()
        with self._interrupt_lock:
            self._interrupt()

    @contextmanager
    def response(self, response):
        """Temporarily attach one response socket; the caller still closes it."""
        # urllib's HTTPResponse owns a BufferedReader over SocketIO. In-memory
        # response doubles need only the normal cooperative Event checks.
        raw = getattr(getattr(response, "fp", None), "raw", None)
        connection = getattr(raw, "_sock", None)
        with self._interrupt_lock:
            self._socket = connection if isinstance(connection, socket.socket) else None
            self._interrupt()
        try:
            yield
        finally:
            with self._interrupt_lock:
                self._socket = None


def _check_cancel(cancel: Event | None) -> None:
    """Raise RuntimeError at a cooperative cancellation boundary."""
    if cancel is not None and cancel.is_set():
        raise RuntimeError("Model download cancelled.")


def _digest(path: Path, cancel: Event | None) -> str:
    """Hash a file in bounded chunks, honoring cancellation between reads."""
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            _check_cancel(cancel)
            digest.update(chunk)
    return digest.hexdigest()


class _HTTPSRedirects(urllib.request.HTTPRedirectHandler):
    """Permit CDN redirects only while the model transfer remains HTTPS."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if urlsplit(newurl).scheme != "https":
            raise RuntimeError("Refusing a non-HTTPS model download redirect.")
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def download_model(
    preset_id: str = DEFAULT_PRESET,
    progress: Callable[[int, int], None] | None = None,
    cancel: Event | None = None,
    *,
    accept_license: bool = False,
) -> Path:
    """Return verified cached or downloaded weights without changing settings.

    ``progress`` runs synchronously with downloaded and expected byte counts;
    an already verified cache entry reports completion without a transfer.
    ``cancel`` is checked between reads and during cache hashing. A
    DownloadCancellation can also interrupt an attached response body read.
    License and memory-policy failures, cancellation and integrity failures
    raise RuntimeError; transport, filesystem and callback errors propagate.
    Only a fully verified temporary file replaces the existing destination.
    """
    preset = get_preset(preset_id)
    unavailable = model_availability(preset)
    if unavailable:
        raise RuntimeError(unavailable)
    if preset.requires_license_acceptance and not accept_license:
        raise RuntimeError(f"Read and accept {preset.license_name} before downloading: {preset.license_url}. "
                           "CLI downloads require --accept-model-license.")
    target = model_path(preset)
    _check_cancel(cancel)
    target.parent.mkdir(parents=True, exist_ok=True)
    if progress is not None:
        progress(0, preset.size_bytes)
    if is_installed(preset) and _digest(target, cancel) == preset.sha256:
        if progress is not None:
            progress(preset.size_bytes, preset.size_bytes)
        return target
    # Stage beside the target so verification precedes an atomic replacement.
    fd, temporary = tempfile.mkstemp(prefix=".download-", dir=target.parent)
    try:
        count = 0
        digest = hashlib.sha256()
        url = f"https://huggingface.co/{preset.repo}/resolve/{preset.revision}/{preset.filename}"
        request = urllib.request.Request(url, headers={"User-Agent": "inklow/0.2"})
        opener = urllib.request.build_opener(_HTTPSRedirects())
        with os.fdopen(fd, "wb") as destination:
            with opener.open(request, timeout=30) as response, (
                    cancel.response(response) if isinstance(cancel, DownloadCancellation) else nullcontext()):
                read = getattr(response, "read1", response.read)
                while True:
                    _check_cancel(cancel)
                    try:
                        chunk = read(64 * 1024)
                    except OSError:
                        _check_cancel(cancel)
                        raise
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
