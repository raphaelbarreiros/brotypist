"""Supervise private inference pipes without importing models or GUI toolkits.

Request serials reject superseded writing; process epochs reject output from
retired children. All process creation and teardown belong to the supervisor.
"""
from __future__ import annotations

from collections import deque
import json
from pathlib import Path
import queue
import subprocess
import sys
import threading
import time

from .config import provider_identity

MAX_MESSAGE_BYTES = 256 * 1024
_PROVIDER_FIELDS = ("backend", "model_path", "endpoint", "model", "acceleration", "prompt_style", "max_tokens", "context_chars")
# A cancelled stream stops at its next token; an unresponsive one is killed.
_CANCEL_GRACE = 0.3
# Prompt evaluation yields no token to check cancellation against. Waiting for
# it is far cheaper than killing the warm model and paying a cold load.
_PROMPT_EVALUATION_GRACE = 3.0


def worker_command() -> list[str]:
    """Choose the source or frozen worker entry point, including Windows CLI."""
    executable = Path(sys.executable)
    if getattr(sys, "frozen", False):
        if sys.platform == "win32":
            executable = executable.with_name("BroTypist-cli.exe")
            if not executable.is_file():
                raise RuntimeError("The BroTypist-cli.exe worker companion is missing. Reinstall the complete application.")
        return [str(executable), "_worker"]
    return [str(executable), "-m", "inklow", "_worker"]


class ProcessClient:
    """Own a lazy worker with one active request and one replaceable pending one.

    Public calls coordinate through a condition or nonblocking cancellation/
    release signals. ``on_event`` runs on the supervisor thread outside the
    condition; GUI callers must marshal their own UI work to the UI thread.
    """

    def __init__(self):
        self._condition = threading.Condition()
        self._events = deque(maxlen=256)
        self._incoming = queue.Queue(maxsize=64)
        self._pending = None
        self._serial = 0
        self._active = None
        self._prepare_id = None
        self._cancel_since = None
        self._cancel_sent = False
        self._cancel_requested = threading.Event()
        # Each explicit release is a lifecycle barrier, unlike superseding
        # typing. Keep its request watermark even if submit races the supervisor.
        self._releases = queue.SimpleQueue()
        self._stopped = False
        self._thread = None
        self._process = None
        self._terminating = None
        self._epoch = 0
        self._configured = None
        self._idle_timeout = 300
        self._last_activity = time.monotonic()
        self._state = "idle"
        self._message = ""
        self._loaded = False
        self._initializing = False
        self._evaluating = False
        self.on_event = None

    def _start_thread(self):
        """Start the supervisor once while the caller holds the condition."""
        if self._thread is None:
            self._thread = threading.Thread(target=self._run, daemon=True, name="inklow-worker-supervisor")
            self._thread.start()

    def prepare(self, config) -> int | None:
        """Queue fixed GGUF framing without sending or evaluating writing.

        Return the request ID, or None for an ineligible model, existing work,
        or an already prepared identity. A stopped eligible client raises
        RuntimeError; preparation does not supersede a writing request.
        """
        if config.backend != "gguf" or not Path(config.model_path).expanduser().is_file():
            return None
        with self._condition:
            if self._stopped:
                raise RuntimeError("Inference has stopped.")
            identity = provider_identity(config)
            if self._pending is not None or self._active is not None or (
                    self._loaded and self._configured == identity):
                return None
            self._serial += 1
            identifier = self._serial
            data = json.dumps({"type": "prepare", "id": identifier,
                               "config": {key: getattr(config, key) for key in _PROVIDER_FIELDS}}).encode() + b"\n"
            self._pending = (identifier, identity, data, True)
            self._idle_timeout = config.idle_release_seconds
            self._start_thread()
            self._condition.notify()
            return identifier

    def submit(self, config, prefix: str, suffix: str, **options) -> int:
        """Replace pending writing and return its correlation ID.

        Prefix/suffix are bounded before JSON encoding; oversized encoded
        frames raise ValueError and stopped clients raise RuntimeError.
        Results arrive through poll(), never synchronously from this call.
        """
        with self._condition:
            if self._stopped:
                raise RuntimeError("Inference has stopped.")
            identifier = self._serial + 1
            frame = {"type": "generate", "id": identifier,
                     "config": {key: getattr(config, key) for key in _PROVIDER_FIELDS},
                     "prefix": prefix[-8000:], "suffix": suffix[:8000], **options}
            data = json.dumps(frame, ensure_ascii=False).encode() + b"\n"
            if len(data) > MAX_MESSAGE_BYTES:
                raise ValueError("The writing request exceeds the private worker message limit.")
            self._serial = identifier
            self._pending = (identifier, provider_identity(config), data, False)
            self._idle_timeout = config.idle_release_seconds
            self._cancel_requested.clear()
            self._events.clear()
            self._start_thread()
            self._condition.notify()
            return identifier

    def set_idle_timeout(self, seconds: int):
        """Set inactivity release in seconds; nonpositive values disable it."""
        with self._condition:
            self._idle_timeout = seconds
            self._condition.notify()

    def cancel(self):
        """Request cancellation without waiting for process or pipe operations."""
        # Keyboard decisions must never wait for the supervisor's process/pipe
        # lock (which can be held while terminating an unresponsive child).
        self._cancel_requested.set()

    def release(self):
        """Queue worker teardown through the current request watermark."""
        self._releases.put(self._serial)

    def status(self) -> dict:
        """Return a state snapshot, including any child still being terminated."""
        with self._condition:
            process = self._process or self._terminating
            return {"state": self._state, "message": self._message,
                    "worker_pid": process.pid if process is not None and process.poll() is None else None,
                    "model_loaded": self._loaded}

    def poll(self) -> list[dict]:
        """Drain events, discarding them while cancellation or release is pending."""
        with self._condition:
            result = [] if self._cancel_requested.is_set() or not self._releases.empty() else list(self._events)
            self._events.clear()
            return result

    def close(self):
        """Stop accepting work and wait up to one second for supervisor shutdown."""
        with self._condition:
            self._stopped = True
            self._pending = None
            self._condition.notify()
        thread = self._thread
        if thread is not None:
            thread.join(timeout=1.0)

    def _kill(self):
        """Retire the current epoch and attempt bounded child termination.

        The caller holds the condition; blocking waits and pipe closes release
        it so public calls can proceed. A still-live child remains tracked as
        ``_terminating`` rather than being reported as already gone.
        """
        process = self._process or self._terminating
        self._terminating = process
        self._process = None
        self._active = None
        self._prepare_id = None
        self._cancel_since = None
        self._cancel_sent = False
        self._configured = None
        self._loaded = False
        self._initializing = False
        self._evaluating = False
        self._epoch += 1
        if process is not None:
            self._condition.release()
            try:
                try:
                    process.terminate()
                    process.wait(timeout=0.15)
                except subprocess.TimeoutExpired:
                    process.kill()
                    try:
                        process.wait(timeout=0.15)
                    except subprocess.TimeoutExpired:
                        pass
                except OSError:
                    pass
                for pipe in (process.stdin, process.stdout):
                    if pipe is not None:
                        try:
                            pipe.close()
                        except OSError:
                            pass
            finally:
                self._condition.acquire()
                if process.poll() is not None:
                    self._terminating = None

    def _read(self, process, epoch):
        """Read bounded JSON frames on a pipe thread and tag them with its epoch."""
        try:
            while True:
                line = process.stdout.readline(MAX_MESSAGE_BYTES + 1)
                if not line:
                    break
                if len(line) > MAX_MESSAGE_BYTES or not line.endswith(b"\n"):
                    raise ValueError("Invalid worker response")
                value = json.loads(line)
                if not isinstance(value, dict) or len(value.get("text", "")) > 4096:
                    raise ValueError("Invalid worker response")
                # Backpressure bounds memory; stopped epochs never block shutdown.
                while True:
                    try:
                        self._incoming.put((epoch, value), timeout=0.05)
                        with self._condition:
                            self._condition.notify_all()
                        break
                    except queue.Full:
                        if self._stopped or epoch != self._epoch:
                            return
        except (OSError, ValueError, TypeError):
            pass
        finally:
            try:
                self._incoming.put((epoch, {"type": "exit"}), timeout=0.05)
            except queue.Full:
                pass

    def _spawn(self):
        """Spawn a private child outside the condition, then attach its reader."""
        flags = subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0
        self._condition.release()
        try:
            process = subprocess.Popen(worker_command(), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                       stderr=subprocess.DEVNULL, bufsize=65536, close_fds=True,
                                       creationflags=flags)
        finally:
            self._condition.acquire()
        self._process = process
        self._epoch += 1
        threading.Thread(target=self._read, args=(self._process, self._epoch), daemon=True, name="inklow-worker-pipe").start()
        self._state, self._message = "loading", "Loading the local model…"
        self._initializing = True

    def _send(self, data):
        """Write one frame outside the condition; caller serializes pipe access."""
        pipe = self._process.stdin
        self._condition.release()
        try:
            pipe.write(data)
            pipe.flush()
        finally:
            self._condition.acquire()

    def _run(self):
        """Advance lifecycle barriers, worker events and latest pending work."""
        try:
            while True:
                notify = False
                with self._condition:
                    if self._stopped:
                        return
                    now = time.monotonic()
                    # Release is stronger than superseding typing: tear down
                    # the old worker, but preserve requests after its watermark.
                    release_through = None
                    while True:
                        try:
                            boundary = self._releases.get_nowait()
                        except queue.Empty:
                            break
                        release_through = boundary if release_through is None else max(release_through, boundary)
                    if release_through is not None:
                        if self._serial <= release_through:
                            self._serial = release_through + 1
                        if self._pending is not None and self._pending[0] <= release_through:
                            self._pending = None
                        self._events.clear()
                        self._kill()
                        self._state, self._message = "idle", "Model released."
                    if self._cancel_requested.is_set():
                        self._cancel_requested.clear()
                        self._serial += 1
                        if self._pending is not None and not self._pending[3]:
                            self._pending = None
                        self._events.clear()
                        if (self._active is not None and self._active != self._prepare_id
                                and self._cancel_since is None):
                            self._cancel_since = now
                    while True:
                        try:
                            epoch, event = self._incoming.get_nowait()
                        except queue.Empty:
                            break
                        # Request IDs alone cannot distinguish an old reader
                        # from the current child after a lifecycle transition.
                        if epoch != self._epoch:
                            continue
                        identifier, kind = event.get("id"), event.get("type")
                        if kind == "exit":
                            if self._active == self._serial:
                                self._events.append({"type": "error", "id": self._active, "message": "The inference worker exited. Check local model installation."})
                                self._state, self._message = "error", "The inference worker exited."
                            self._kill()
                            continue
                        if kind == "done" and identifier == self._active:
                            self._initializing = False
                            self._evaluating = False
                            self._loaded = bool(event.get("model_loaded", self._loaded))
                            self._active = None
                            self._cancel_since = None
                            self._cancel_sent = False
                            self._last_activity = now
                            if self._state != "error":
                                self._state, self._message = "ready", "Ready."
                            if identifier == self._prepare_id:
                                self._prepare_id = None
                        if kind == "result" and identifier == self._active:
                            self._evaluating = False
                        if kind == "loading" and identifier == self._active:
                            self._initializing = True
                        if kind == "prepared" and identifier == self._active:
                            self._initializing = False
                            self._loaded = bool(event.get("model_loaded", True))
                            self._state, self._message = "ready", "Local model prepared; waiting for writing."
                        if kind == "ready":
                            # Loading is not permission to evaluate writing.
                            # Only the still-current request receives proceed.
                            self._initializing = False
                            self._loaded = bool(event.get("model_loaded", True))
                            if self._cancel_since is not None:
                                self._cancel_since = now
                            if (identifier == self._serial and self._releases.empty()
                                    and not self._cancel_requested.is_set()):
                                self._send(json.dumps({"type": "proceed", "id": identifier}).encode() + b"\n")
                                self._evaluating = identifier == self._active
                        if (identifier != self._serial or self._cancel_requested.is_set()
                                or not self._releases.empty()):
                            continue
                        if kind == "ready":
                            self._loaded = bool(event.get("model_loaded", True))
                            self._state, self._message = "generating", "Generating a local suggestion…"
                        elif kind == "loading":
                            self._state, self._message = "loading", "Loading the local model…"
                        elif kind == "error":
                            self._state, self._message = "error", event.get("message", "Inference failed.")
                        self._events.append(event)
                        notify = True
                    if (self._pending is not None and self._active is not None
                            and self._pending[1] != self._configured):
                        self._kill()
                    if (self._active is not None and self._active != self._prepare_id
                            and (self._pending is not None or self._active != self._serial)
                            and not self._cancel_sent):
                        self._send(b'{"type":"cancel"}\n')
                        self._cancel_sent = True
                        if self._cancel_since is None:
                            self._cancel_since = now
                    # Ordinary cancellation preserves cold initialization.
                    # Once ready, prompt evaluation gets a longer grace than
                    # token streaming because it has no per-token callbacks.
                    if (not self._initializing and self._cancel_since is not None
                            and now - self._cancel_since >= (
                                _PROMPT_EVALUATION_GRACE if self._evaluating else _CANCEL_GRACE)):
                        self._kill()
                        self._state = "idle"
                    if self._pending is not None and self._active is None:
                        identifier, identity, data, preparing = self._pending
                        self._pending = None
                        if self._process is not None and identity != self._configured:
                            self._kill()
                        if preparing:
                            self._prepare_id = identifier
                        if self._process is None:
                            self._spawn()
                        self._configured = identity
                        # Spawn releases the condition; recheck lifecycle and
                        # freshness signals before handing over any draft.
                        if (self._stopped or not self._releases.empty()
                                or (identifier != self._prepare_id and (
                                    self._cancel_requested.is_set() or identifier != self._serial))):
                            if not self._stopped and identifier in (self._serial, self._prepare_id):
                                self._pending = (identifier, identity, data, preparing)
                            continue
                        self._active = identifier
                        self._cancel_sent = False
                        self._last_activity = now
                        self._send(data)
                    elif (self._process is not None and self._active is None and self._idle_timeout > 0
                          and now - self._last_activity >= self._idle_timeout):
                        self._kill()
                        self._state, self._message = "idle", "Model released after inactivity."
                    if not notify:
                        self._condition.wait(timeout=0.02)
                if notify and self.on_event is not None:
                    self.on_event()
        except Exception:
            with self._condition:
                self._state, self._message = "error", "Cannot start the private inference worker. Check the installation."
                self._events.append({"type": "error", "id": self._serial, "message": self._message})
        finally:
            with self._condition:
                self._kill()
                self._thread = None
                if not self._stopped and self._pending is not None:
                    self._start_thread()
