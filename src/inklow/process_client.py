"""Lazy process supervisor. Parent never imports a model or a GUI toolkit."""
from __future__ import annotations

from collections import deque
import json
from pathlib import Path
import queue
import subprocess
import sys
import threading
import time

MAX_MESSAGE_BYTES = 256 * 1024
_PROVIDER_FIELDS = ("backend", "model_path", "endpoint", "model", "acceleration", "prompt_style", "max_tokens", "context_chars")


def worker_command() -> list[str]:
    executable = Path(sys.executable)
    if getattr(sys, "frozen", False):
        if sys.platform == "win32":
            executable = executable.with_name("Inklow-cli.exe")
            if not executable.is_file():
                raise RuntimeError("The Inklow-cli.exe worker companion is missing. Reinstall the complete application.")
        return [str(executable), "_worker"]
    return [str(executable), "-m", "inklow", "_worker"]


def _identity(config) -> tuple:
    return tuple(getattr(config, key) for key in ("backend", "model_path", "endpoint", "model", "acceleration", "prompt_style"))


class ProcessClient:
    def __init__(self):
        self._condition = threading.Condition()
        self._events = deque(maxlen=256)
        self._incoming = queue.Queue(maxsize=64)
        self._pending = None
        self._serial = 0
        self._active = None
        self._cancel_since = None
        self._cancel_sent = False
        self._cancel_requested = threading.Event()
        self._release_requested = threading.Event()
        self._stopped = False
        self._thread = None
        self._process = None
        self._terminating = None
        self._epoch = 0
        self._configured = None
        self._idle_timeout = 60
        self._last_activity = time.monotonic()
        self._state = "idle"
        self._message = ""
        self._loaded = False
        self._initializing = False

    def _start_thread(self):
        if self._thread is None:
            self._thread = threading.Thread(target=self._run, daemon=True, name="inklow-worker-supervisor")
            self._thread.start()

    def submit(self, config, prefix: str, suffix: str, **options) -> int:
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
            self._pending = (identifier, _identity(config), data)
            self._idle_timeout = config.idle_release_seconds
            self._cancel_requested.clear()
            self._release_requested.clear()
            self._events.clear()
            self._start_thread()
            self._condition.notify()
            return identifier

    def set_idle_timeout(self, seconds: int):
        with self._condition:
            self._idle_timeout = seconds
            self._condition.notify()

    def cancel(self):
        # Keyboard decisions must never wait for the supervisor's process/pipe
        # lock (which can be held while terminating an unresponsive child).
        self._cancel_requested.set()

    def release(self):
        self._release_requested.set()

    def status(self) -> dict:
        with self._condition:
            process = self._process or self._terminating
            return {"state": self._state, "message": self._message,
                    "worker_pid": process.pid if process is not None and process.poll() is None else None,
                    "model_loaded": self._loaded}

    def poll(self) -> list[dict]:
        with self._condition:
            result = list(self._events)
            self._events.clear()
            return result

    def close(self):
        with self._condition:
            self._stopped = True
            self._pending = None
            self._condition.notify()
        thread = self._thread
        if thread is not None:
            thread.join(timeout=1.0)

    def _kill(self):
        process = self._process or self._terminating
        self._terminating = process
        self._process = None
        self._active = None
        self._cancel_since = None
        self._cancel_sent = False
        self._configured = None
        self._loaded = False
        self._initializing = False
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
        pipe = self._process.stdin
        self._condition.release()
        try:
            pipe.write(data)
            pipe.flush()
        finally:
            self._condition.acquire()

    def _run(self):
        try:
            while True:
                with self._condition:
                    if self._stopped:
                        return
                    now = time.monotonic()
                    if self._release_requested.is_set():
                        self._release_requested.clear()
                        self._cancel_requested.clear()
                        self._serial += 1
                        self._pending = None
                        self._events.clear()
                        self._kill()
                        self._state, self._message = "idle", "Model released."
                    if self._cancel_requested.is_set():
                        self._cancel_requested.clear()
                        self._serial += 1
                        self._pending = None
                        self._events.clear()
                        if self._active is not None and self._cancel_since is None:
                            self._cancel_since = now
                    while True:
                        try:
                            epoch, event = self._incoming.get_nowait()
                        except queue.Empty:
                            break
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
                            self._loaded = bool(event.get("model_loaded", self._loaded))
                            self._active = None
                            self._cancel_since = None
                            self._cancel_sent = False
                            self._last_activity = now
                            if self._state != "error":
                                self._state, self._message = "ready", "Ready."
                        if kind == "loading" and identifier == self._active:
                            self._initializing = True
                        if kind == "ready":
                            self._initializing = False
                            self._loaded = bool(event.get("model_loaded", True))
                            if self._cancel_since is not None:
                                self._cancel_since = now
                            if (identifier == self._serial and not self._release_requested.is_set()
                                    and not self._cancel_requested.is_set()):
                                self._send(json.dumps({"type": "proceed", "id": identifier}).encode() + b"\n")
                        if identifier != self._serial:
                            continue
                        if kind == "ready":
                            self._loaded = bool(event.get("model_loaded", True))
                            self._state, self._message = "generating", "Generating a local suggestion…"
                        elif kind == "loading":
                            self._state, self._message = "loading", "Loading the local model…"
                        elif kind == "error":
                            self._state, self._message = "error", event.get("message", "Inference failed.")
                        self._events.append(event)
                    if (self._pending is not None and self._active is not None
                            and self._pending[1] != self._configured):
                        self._kill()
                    if (self._active is not None and (self._pending is not None or self._active != self._serial)
                            and not self._cancel_sent):
                        self._send(b'{"type":"cancel"}\n')
                        self._cancel_sent = True
                        if self._cancel_since is None:
                            self._cancel_since = now
                    if (not self._initializing and self._cancel_since is not None
                            and now - self._cancel_since >= 0.3):
                        self._kill()
                        self._state = "idle"
                    if self._pending is not None and self._active is None:
                        identifier, identity, data = self._pending
                        self._pending = None
                        if self._process is not None and identity != self._configured:
                            self._kill()
                        if self._process is None:
                            self._spawn()
                        self._configured = identity
                        if (self._stopped or self._release_requested.is_set() or self._cancel_requested.is_set()
                                or identifier != self._serial):
                            continue
                        self._active = identifier
                        self._cancel_sent = False
                        self._last_activity = now
                        self._send(data)
                    elif (self._process is not None and self._active is None and self._idle_timeout > 0
                          and now - self._last_activity >= self._idle_timeout):
                        self._kill()
                        self._state, self._message = "idle", "Model released after inactivity."
                    self._condition.wait(timeout=0.02)
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
