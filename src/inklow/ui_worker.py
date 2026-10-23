"""One inference thread; at most one running request and one newest pending request."""
from __future__ import annotations

from contextlib import closing
from dataclasses import dataclass
from threading import Condition, Event

from PySide6.QtCore import QThread, Signal

from .config import Config
from .core import Snapshot
from .providers import Provider, create_provider


@dataclass(frozen=True)
class PredictionRequest:
    generation: int
    revision: int
    channel: str
    snapshot: Snapshot


class InferenceWorker(QThread):
    # Emitted only after the provider for that revision finished prepare().
    provider_ready = Signal(int, str)
    # request, cumulative text, error, final. Partials precede exactly one final.
    result_ready = Signal(object, str, str, bool)

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setObjectName("Inklow inference")
        self._condition = Condition()
        self._config: Config | None = None
        self._revision = 0
        self._pending: tuple[PredictionRequest, Event] | None = None
        self._active: tuple[PredictionRequest, Event] | None = None
        self._stopping = False

    def _cancel_locked(self) -> None:
        for job in (self._active, self._pending):
            if job is not None:
                job[1].set()
        self._pending = None

    def configure(self, config: Config, revision: int) -> None:
        with self._condition:
            self._config = config
            self._revision = revision
            self._cancel_locked()
            self._condition.notify()

    def submit(self, request: PredictionRequest) -> None:
        with self._condition:
            if self._stopping or request.revision != self._revision:
                return
            if (self._active is not None and self._active[0] == request
                    and not self._active[1].is_set()):
                return  # Already generating exactly this request.
            # The newest request supersedes both running and queued work.
            self._cancel_locked()
            self._pending = (request, Event())
            self._condition.notify()

    def cancel_prediction(self) -> None:
        """Cancel running and queued work; later submit() calls are unaffected."""
        with self._condition:
            self._cancel_locked()

    def request_stop(self) -> None:
        with self._condition:
            self._stopping = True
            self._cancel_locked()
            self._condition.notify()

    def _publish(self, request: PredictionRequest, cancel: Event, text: str,
                 error: str, final: bool) -> None:
        with self._condition:
            if not self._stopping and not cancel.is_set() and request.revision == self._revision:
                self.result_ready.emit(request, text, error, final)

    def _predict(self, provider: Provider | None, setup_error: str,
                 request: PredictionRequest, cancel: Event) -> None:
        if provider is None:
            self._publish(request, cancel, "", setup_error or "Predictions are paused.", True)
            return
        text = ""
        try:
            with closing(provider.stream(request.snapshot.prefix, request.snapshot.suffix, cancel)) as chunks:
                for text in chunks:
                    if not isinstance(text, str):
                        raise TypeError("The model returned an invalid completion. Check the selected backend.")
                    if cancel.is_set():
                        return
                    self._publish(request, cancel, text, "", False)
        except Exception as exc:
            self._publish(request, cancel, "", str(exc) or type(exc).__name__, True)
            return
        self._publish(request, cancel, text, "", True)

    def run(self) -> None:
        provider: Provider | None = None
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
                    needs_setup = config is not None and revision != loaded_revision
                    job = None
                    if not needs_setup:
                        job = self._active = self._pending
                        self._pending = None
                if needs_setup:
                    # Never closed while generating: only this thread streams.
                    if provider is not None:
                        try:
                            provider.close()
                        except Exception:
                            pass
                        provider = None
                    setup_error = ""
                    try:
                        provider = create_provider(config)
                        # Native loading may not be interruptible; a newer
                        # revision is noticed and prepared after it returns.
                        provider.prepare()
                    except Exception as exc:
                        setup_error = str(exc) or type(exc).__name__
                        if provider is not None:
                            try:
                                provider.close()
                            except Exception:
                                pass
                            provider = None
                    loaded_revision = revision
                    with self._condition:
                        if not self._stopping and revision == self._revision:
                            self.provider_ready.emit(revision, setup_error)
                    continue
                try:
                    self._predict(provider, setup_error, *job)
                finally:
                    with self._condition:
                        if self._active is job:
                            self._active = None
        finally:
            if provider is not None:
                try:
                    provider.close()
                except Exception:
                    pass
