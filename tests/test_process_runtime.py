"""Constrain real worker IPC and lifetime using synthetic model implementations.

Loopback HTTP and child-process providers exercise protocol/cancellation ordering,
not native model quality, memory usage or production latency.
"""
from dataclasses import replace
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
import queue
import subprocess
import sys
import threading
import time

import pytest

from inklow.config import Config
from inklow.process_client import ProcessClient, worker_command


def until(predicate, timeout=5):
    """Return the first truthy observation, or fail when the seconds budget expires."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.01)
    raise AssertionError("Expected worker transition did not arrive")


class Handler(BaseHTTPRequestHandler):
    """Serve two flushed chunks, with an event-gated tail for cancellation cases."""

    def log_message(self, *args):
        pass

    def do_POST(self):
        """Record a request and tolerate a client disconnect while streaming."""
        length = int(self.headers.get("Content-Length") or 0)
        body = json.loads(self.rfile.read(length))
        self.server.requests.put(body)
        try:
            self.send_response(200)
            self.end_headers()
            prefix = body.get("prompt", "")
            self.wfile.write(json.dumps({"response": " first words "}).encode() + b"\n")
            self.wfile.flush()
            # The first chunk proves the stream is active before a test supersedes
            # it; release controls whether that obsolete stream can finish.
            if prefix == "blocked synthetic draft":
                self.server.release.wait(5)
            self.wfile.write(json.dumps({"response": "completed", "done": True, "done_reason": "stop"}).encode() + b"\n")
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass


@pytest.fixture
def peer():
    """Own an ephemeral loopback server and release stalled handlers on teardown."""
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    server.requests = queue.Queue()
    server.release = threading.Event()
    threading.Thread(target=server.serve_forever, daemon=True).start()
    yield server
    server.release.set()
    server.shutdown()
    server.server_close()


@pytest.fixture
def worker(peer, tmp_path, monkeypatch):
    """Own a lazy worker client targeting the peer in an isolated profile."""
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    client = ProcessClient()
    config = replace(Config(), backend="ollama", endpoint=f"http://127.0.0.1:{peer.server_port}", model="synthetic", idle_release_seconds=1)
    yield client, config
    client.close()


def final(client, identifier):
    """Collect events until this request finishes, failing immediately on errors."""
    seen = []
    def receive():
        events = client.poll()
        seen.extend(events)
        assert not any(event.get("type") == "error" for event in events), events
        return next((event for event in events if event.get("id") == identifier and event.get("type") == "result" and event.get("final")), None)
    result = until(receive)
    return result, seen


def test_worker_starts_only_on_request_and_really_exits_on_idle(worker, peer):
    client, config = worker
    assert client.status()["worker_pid"] is None
    identifier = client.submit(config, "synthetic ordinary draft", "")
    result, _ = final(client, identifier)
    assert result["text"] == " first words completed"
    pid = client.status()["worker_pid"]
    assert pid is not None and pid != os.getpid()
    assert peer.requests.get(timeout=2)["keep_alive"] == 0
    until(lambda: client.status()["worker_pid"] is None)
    if os.name == "posix":
        with pytest.raises(ProcessLookupError):
            os.kill(pid, 0)


def test_stuck_old_stream_is_killed_and_only_latest_pending_request_survives(worker, peer):
    client, config = worker
    old = client.submit(config, "blocked synthetic draft", "")
    until(lambda: any(event.get("type") == "result" for event in client.poll()))
    old_pid = client.status()["worker_pid"]
    middle = client.submit(config, "never submitted middle", "")
    latest = client.submit(config, "newest synthetic draft", "")
    result, events = final(client, latest)
    assert result["text"] == " first words completed"
    assert all(event.get("id") not in {old, middle} for event in events)
    assert client.status()["worker_pid"] != old_pid
    prompts = [peer.requests.get(timeout=2)["prompt"] for _ in range(2)]
    assert prompts == ["blocked synthetic draft", "newest synthetic draft"]
    peer.release.set()


def test_keep_ready_retains_worker_until_explicit_release_and_model_switch_replaces_it(worker):
    client, config = worker
    config = replace(config, idle_release_seconds=0)
    first = client.submit(config, "first synthetic draft", "")
    final(client, first)
    pid = client.status()["worker_pid"]
    time.sleep(1.2)
    assert client.status()["worker_pid"] == pid
    second = client.submit(replace(config, model="other-synthetic"), "second synthetic draft", "")
    final(client, second)
    assert client.status()["worker_pid"] != pid
    client.release()
    until(lambda: client.status()["worker_pid"] is None)


def test_same_model_cancellations_during_cold_prepare_keep_one_worker_and_latest_request(monkeypatch, tmp_path):
    # The actual worker protocol runs in a real child, but the expensive native
    # initializer is a deterministic slow fixture rather than a downloaded model.
    bootstrap = '''
import time
import os
import inklow.providers as providers
from inklow.process_worker import main
class SlowProvider:
    loads = 0
    generations = 0
    def prepare(self):
        type(self).loads += 1
        print("synthetic private model diagnostic", flush=True)
        os.write(2, b"synthetic private native diagnostic\\n")
        time.sleep(1.0)
    def stream(self, prefix, suffix, cancel, **kwargs):
        if not cancel.is_set():
            type(self).generations += 1
            yield " prepared " + str(type(self).loads) + " generation " + str(type(self).generations) + " " + prefix
    def close(self):
        pass
providers.create_provider = lambda config: SlowProvider()
raise SystemExit(main())
'''
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    monkeypatch.setattr("inklow.process_client.worker_command", lambda: [sys.executable, "-u", "-c", bootstrap])
    client = ProcessClient()
    config = replace(Config(), idle_release_seconds=0)
    try:
        client.submit(config, "old", "")
        until(lambda: any(event.get("type") == "loading" for event in client.poll()))
        pid = client.status()["worker_pid"]
        for index, prefix in enumerate(("middle one", "middle two", "newest")):
            client.cancel()
            # Framing and inactive server settings do not select new weights.
            current = replace(config, prompt_style="writing" if index % 2 else "continuation",
                              model="unused-" + str(index), endpoint="http://127.0.0.1:12345")
            newest = client.submit(current, prefix, "")
            time.sleep(0.15)
            assert client.status()["worker_pid"] == pid
        result, events = final(client, newest)
        assert result["text"] == " prepared 1 generation 1 newest"
        assert client.status()["worker_pid"] == pid
        assert all(event.get("id") == newest for event in events)
        client.release()
        until(lambda: client.status()["worker_pid"] is None)
        client.submit(config, "explicit pause during initialization", "")
        until(lambda: any(event.get("type") == "loading" for event in client.poll()))
        client.release()
        until(lambda: client.status()["worker_pid"] is None, timeout=1)
    finally:
        client.close()


def test_headless_runtime_import_cannot_load_qt_or_llama():
    code = "import sys; import inklow.runtime; assert not any(x.startswith(('PySide6', 'llama_cpp')) for x in sys.modules)"
    completed = subprocess.run([sys.executable, "-c", code], capture_output=True, timeout=5)
    assert completed.returncode == 0, completed.stderr.decode()


def test_frozen_windows_uses_console_companion_for_real_pipes(monkeypatch, tmp_path):
    executable = tmp_path / "BroTypist.exe"
    console = tmp_path / "BroTypist-cli.exe"
    console.touch()
    monkeypatch.setattr(sys, "frozen", True, raising=False)
    monkeypatch.setattr(sys, "platform", "win32")
    monkeypatch.setattr(sys, "executable", str(executable))
    assert worker_command() == [str(console), "_worker"]
    console.unlink()
    with pytest.raises(RuntimeError, match="companion"):
        worker_command()


@pytest.mark.parametrize("first_attempt_fails", [False, True])
def test_native_initialization_does_not_overlap_a_blocking_stdin_read(monkeypatch, tmp_path, first_attempt_fails):
    # Model Windows DLL/CRT initialization's standard-handle constraint using
    # a real worker pipe. The ordinary next-request read must wait for prepare.
    bootstrap = '''
import sys
import time
import threading
import inklow.providers as providers
from inklow.process_worker import main
original = sys.stdin
reading = threading.Event()
class Input:
    @property
    def buffer(self):
        return self
    def readline(self, limit):
        reading.set()
        try:
            return original.buffer.readline(limit)
        finally:
            reading.clear()
sys.stdin = Input()
class NativeProvider:
    attempts = 0
    def prepare(self):
        self.attempts += 1
        if FIRST_ATTEMPT_FAILS and self.attempts == 1:
            raise OSError("Synthetic model was not yet available")
        time.sleep(0.1)
        if reading.is_set():
            raise RuntimeError("Native initialization collided with pending standard input")
    def stream(self, prefix, suffix, cancel, **kwargs):
        yield " safely initialized after " + str(self.attempts) + " attempts"
    def close(self):
        pass
providers.create_provider = lambda config: NativeProvider()
raise SystemExit(main())
'''.replace("FIRST_ATTEMPT_FAILS", str(first_attempt_fails))
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    monkeypatch.setattr("inklow.process_client.worker_command", lambda: [sys.executable, "-u", "-c", bootstrap])
    client = ProcessClient()
    try:
        identifier = client.submit(Config(), "synthetic initialization", "")
        if first_attempt_fails:
            until(lambda: any(event.get("type") == "error" for event in client.poll()))
            identifier = client.submit(Config(), "synthetic same-model retry", "")
        result, _ = final(client, identifier)
        assert result["text"] == " safely initialized after " + ("2" if first_attempt_fails else "1") + " attempts"
    finally:
        client.close()


@pytest.mark.parametrize("action", ["cancel", "release"])
def test_cancellation_arriving_with_ready_cannot_authorize_stale_generation(monkeypatch, tmp_path, action):
    marker = tmp_path / "stale-generation"
    bootstrap = '''
import sys
from pathlib import Path
import inklow.providers as providers
from inklow.process_worker import main
class Provider:
    def prepare(self):
        pass
    def stream(self, prefix, suffix, cancel, **kwargs):
        Path(sys.argv[1]).write_text("generation started", encoding="utf-8")
        yield " stale generation"
    def close(self):
        pass
providers.create_provider = lambda config: Provider()
raise SystemExit(main())
'''
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    monkeypatch.setattr("inklow.process_client.worker_command", lambda: [sys.executable, "-u", "-c", bootstrap, str(marker)])
    client = ProcessClient()
    canceled = threading.Event()

    class CancelAtReady(queue.Queue):
        """Revoke authorization as ready is dequeued, before its caller handles it."""

        delay_next = False

        def get_nowait(self):
            """Leave a brief observation window for any incorrectly started child."""
            if self.delay_next:
                self.delay_next = False
                time.sleep(0.2)  # Let an incorrectly authorized child start.
            item = super().get_nowait()
            if item[1].get("type") == "ready":
                getattr(client, action)()
                canceled.set()
                self.delay_next = True
            return item

    client._incoming = CancelAtReady(maxsize=64)
    try:
        client.submit(replace(Config(), idle_release_seconds=0), "obsolete synthetic prefix", "")
        until(canceled.is_set)
        until(lambda: client.status()["state"] == ("idle" if action == "release" else "ready"))
        if action == "release":
            assert client.status()["worker_pid"] is None
        assert not marker.exists()
    finally:
        client.close()


def test_cancel_during_prompt_evaluation_keeps_the_warm_worker(monkeypatch, tmp_path):
    # Escape or a new keystroke cancels while the model evaluates a long prompt
    # (no token yet to observe cancellation). That must not cost a cold reload.
    bootstrap = '''
import time
import inklow.providers as providers
from inklow.process_worker import main
class Provider:
    def prepare(self):
        pass
    def stream(self, prefix, suffix, cancel, **kwargs):
        if prefix == "long synthetic prompt":
            time.sleep(1.0)
            if cancel.is_set():
                return
        yield " evaluated " + prefix
    def close(self):
        pass
providers.create_provider = lambda config: Provider()
raise SystemExit(main())
'''
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    monkeypatch.setattr("inklow.process_client.worker_command", lambda: [sys.executable, "-u", "-c", bootstrap])
    client = ProcessClient()
    config = replace(Config(), idle_release_seconds=0)
    try:
        final(client, client.submit(config, "warm-up", ""))
        pid = client.status()["worker_pid"]
        client.submit(config, "long synthetic prompt", "")
        until(lambda: client.status()["state"] == "generating")
        client.cancel()
        time.sleep(0.6)
        assert client.status()["worker_pid"] == pid
        result, _ = final(client, client.submit(config, "next draft", ""))
        assert result["text"] == " evaluated next draft"
        assert client.status()["worker_pid"] == pid
    finally:
        client.close()


def test_explicit_release_barriers_survive_racing_submissions(worker, peer):
    client, config = worker
    config = replace(config, idle_release_seconds=0)
    final(client, client.submit(config, "first synthetic draft", ""))
    original_pid = client.status()["worker_pid"]
    assert peer.requests.get(timeout=2)["prompt"] == "first synthetic draft"
    # Hold only the supervisor: model completion itself uses a real pipe/peer.
    # Two privacy/pause boundaries must not be cleared by a subsequent submit.
    with client._condition:
        client.release()
        obsolete = client.submit(config, "revoked synthetic draft", "")
        client.release()
        latest = client.submit(config, "newly authorized synthetic draft", "")
    result, events = final(client, latest)
    assert result["text"] == " first words completed"
    assert client.status()["worker_pid"] != original_pid
    assert all(event.get("id") != obsolete for event in events)
    assert peer.requests.get(timeout=2)["prompt"] == "newly authorized synthetic draft"
    assert peer.requests.empty()


@pytest.mark.parametrize("transition", ["release", "quit", "model"])
def test_explicit_lifecycle_transition_interrupts_cold_initialization(monkeypatch, tmp_path, transition):
    bootstrap = '''
import time
import inklow.providers as providers
from inklow.process_worker import main
class Provider:
    def prepare(self):
        if self.config.model_path == "blocked-synthetic.gguf":
            time.sleep(5)
    def stream(self, prefix, suffix, cancel, **kwargs):
        yield " authorized " + prefix
    def close(self):
        pass
providers.create_provider = lambda config: Provider()
raise SystemExit(main())
'''
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    monkeypatch.setattr("inklow.process_client.worker_command", lambda: [sys.executable, "-u", "-c", bootstrap])
    client = ProcessClient()
    try:
        config = replace(Config(), model_path="blocked-synthetic.gguf", idle_release_seconds=0)
        old = client.submit(config, "obsolete", "")
        until(lambda: any(event.get("type") == "loading" for event in client.poll()))
        pid = client.status()["worker_pid"]
        if transition == "model":
            latest = client.submit(replace(config, model_path="ready-synthetic.gguf"), "current", "")
            result, events = final(client, latest)
            assert result["text"] == " authorized current"
            assert client.status()["worker_pid"] != pid
            assert all(event.get("id") != old for event in events)
        else:
            client.close() if transition == "quit" else client.release()
            until(lambda: client.status()["worker_pid"] is None, timeout=1)
            assert not client.poll()
    finally:
        client.close()


def test_completed_native_allocation_survives_request_framing_changes(monkeypatch, tmp_path):
    bootstrap = '''
import inklow.providers as providers
from inklow.process_worker import main
class Provider:
    loads = 0
    def prepare(self):
        type(self).loads += 1
    def stream(self, prefix, suffix, cancel, **kwargs):
        yield " " + self.config.prompt_style + " context " + str(self.config.context_chars) + " loads " + str(type(self).loads)
    def close(self):
        pass
providers.create_provider = lambda config: Provider()
raise SystemExit(main())
'''
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    monkeypatch.setattr("inklow.process_client.worker_command", lambda: [sys.executable, "-u", "-c", bootstrap])
    client = ProcessClient()
    try:
        config = replace(Config(), idle_release_seconds=0)
        initial, _ = final(client, client.submit(config, "first synthetic draft", ""))
        pid = client.status()["worker_pid"]
        changed = replace(config, prompt_style="writing", context_chars=4000, max_tokens=64)
        result, _ = final(client, client.submit(changed, "second synthetic draft", ""))
        assert initial["text"] == " continuation context 2000 loads 1"
        assert result["text"] == " writing context 4000 loads 1"
        assert client.status()["worker_pid"] == pid
    finally:
        client.close()


def test_configuration_preparation_survives_typing_and_never_generates(monkeypatch, tmp_path):
    bootstrap = '''
import time
import inklow.providers as providers
from inklow.process_worker import main
class Provider:
    def prepare(self):
        time.sleep(0.3)
    def stream(self, prefix, suffix, cancel, **kwargs):
        yield "actual:" + prefix
    def close(self):
        pass
providers.create_provider = lambda config: Provider()
raise SystemExit(main())
'''
    model = tmp_path / "local.gguf"
    model.touch()
    monkeypatch.setattr("inklow.process_client.worker_command",
                        lambda: [sys.executable, "-u", "-c", bootstrap])
    client = ProcessClient()
    config = replace(Config(), model_path=str(model), idle_release_seconds=1)
    try:
        prepared = client.prepare(config)
        until(lambda: client.status()["worker_pid"])
        pid = client.status()["worker_pid"]
        client.cancel()
        latest = client.submit(config, "current writing", "")
        result, events = final(client, latest)
        assert result["text"] == "actual:current writing"
        assert client.status()["worker_pid"] == pid
        assert not any(event["type"] == "result" and event["id"] == prepared for event in events)
        until(lambda: client.status()["worker_pid"] is None)
        client.prepare(config)
        until(lambda: client.status()["worker_pid"])
        client.release()
        until(lambda: client.status()["worker_pid"] is None, timeout=1)
        assert not any(event["type"] == "result" for event in client.poll())
    finally:
        client.close()


def test_preparation_acknowledges_without_proceed_or_generation(monkeypatch, tmp_path):
    bootstrap = '''
import inklow.providers as providers
from inklow.process_worker import main
class Provider:
    def prepare(self):
        pass
    def stream(self, *args, **kwargs):
        raise AssertionError("preparation evaluated writing")
    def close(self):
        pass
providers.create_provider = lambda config: Provider()
raise SystemExit(main())
'''
    model = tmp_path / "local.gguf"
    model.touch()
    monkeypatch.setattr("inklow.process_client.worker_command",
                        lambda: [sys.executable, "-u", "-c", bootstrap])
    client = ProcessClient()
    try:
        identifier = client.prepare(replace(Config(), model_path=str(model), idle_release_seconds=0))
        seen = []
        def acknowledged():
            seen.extend(client.poll())
            return any(event["type"] == "done" for event in seen)
        until(acknowledged)
        assert any(event["type"] == "prepared" and event["id"] == identifier for event in seen)
        assert not any(event["type"] in {"ready", "result", "error"} for event in seen)
        assert client.status()["model_loaded"]
    finally:
        client.close()


def test_new_preparation_survives_an_older_release_and_typing_cancel(monkeypatch, tmp_path):
    bootstrap = '''
import inklow.providers as providers
from inklow.process_worker import main
class Provider:
    def prepare(self):
        pass
    def stream(self, *args, **kwargs):
        raise AssertionError("configuration-only preparation evaluated writing")
    def close(self):
        pass
providers.create_provider = lambda config: Provider()
raise SystemExit(main())
'''
    model = tmp_path / "local.gguf"
    model.touch()
    monkeypatch.setattr("inklow.process_client.worker_command",
                        lambda: [sys.executable, "-u", "-c", bootstrap])
    client = ProcessClient()
    config = replace(Config(), model_path=str(model), idle_release_seconds=0)
    try:
        # Queue these transitions atomically so the supervisor sees the exact
        # settings/pause -> new focus -> typing ordering, without timing sleeps.
        with client._condition:
            client.release()
            client.prepare(config)
            client.cancel()
        until(lambda: client.status()["model_loaded"], timeout=2)
        assert not any(event["type"] in {"ready", "result", "error"}
                       for event in client.poll())
    finally:
        client.close()
