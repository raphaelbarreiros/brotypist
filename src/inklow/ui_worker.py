"""One inference thread; at most one running request and one newest pending request."""
from __future__ import annotations

from dataclasses import dataclass
from threading import Condition

from PySide6.QtCore import QThread, Signal

from .config import Config
from .core import Snapshot
from .providers import create_provider


@dataclass(frozen=True)
class PredictionRequest:
    generation: int
    revision: int
    channel: str
    snapshot: Snapshot


class InferenceWorker(QThread):
    provider_ready = Signal(int, str)
    result_ready = Signal(object, str, str)

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setObjectName("Inklow inference")
        self._condition = Condition()
        self._config: Config | None = None
        self._revision = 0
        self._pending: PredictionRequest | None = None
        self._stopping = False

    def configure(self, config: Config, revision: int) -> None:
        with self._condition:
            self._config = config
            self._revision = revision
            self._pending = None
            self._condition.notify()

    def submit(self, request: PredictionRequest) -> None:
        with self._condition:
            if not self._stopping and request.revision == self._revision:
                self._pending = request
                self._condition.notify()

    def clear_pending(self) -> None:
        with self._condition:
            self._pending = None

    def request_stop(self) -> None:
        with self._condition:
            self._stopping = True
            self._pending = None
            self._condition.notify()

    def run(self) -> None:
        provider = None
        loaded_revision = -1
        setup_error = ""
        try:
            while True:
                with self._condition:
                    self._condition.wait_for(
                        lambda: self._stopping
                        or (self._config is not None and self._revision != loaded_revision)
                        or self._pending is not None
                    )
                    if self._stopping:
                        break
                    revision = self._revision
                    config = self._config
                    needs_setup = revision != loaded_revision
                    request = None if needs_setup else self._pending
                    if not needs_setup:
                        self._pending = None
                if needs_setup:
                    if provider is not None:
                        try:
                            provider.close()
                        except Exception:
                            pass
                        provider = None
                    setup_error = ""
                    try:
                        if config is not None:
                            provider = create_provider(config)
                    except Exception as exc:
                        setup_error = str(exc) or type(exc).__name__
                    loaded_revision = revision
                    self.provider_ready.emit(revision, setup_error)
                    continue
                if request is None:
                    continue
                with self._condition:
                    if self._stopping or request.revision != self._revision:
                        continue
                if provider is None:
                    self.result_ready.emit(request, "", setup_error or "Predictions are paused.")
                    continue
                try:
                    text = provider.complete(request.snapshot.prefix, request.snapshot.suffix)
                    if not isinstance(text, str):
                        raise TypeError("The model returned an invalid completion. Check the selected backend.")
                    self.result_ready.emit(request, text, "")
                except Exception as exc:
                    self.result_ready.emit(request, "", str(exc) or type(exc).__name__)
        finally:
            if provider is not None:
                try:
                    provider.close()
                except Exception:
                    pass
