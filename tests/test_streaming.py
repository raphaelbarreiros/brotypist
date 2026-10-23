"""Scenarios C1-C6 from the inference upgrade plan, against a real loopback peer."""
from dataclasses import replace
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import queue
import threading

import pytest
from PySide6.QtCore import Qt
from PySide6.QtWidgets import QApplication

from inklow.config import Config
from inklow.core import Snapshot
from inklow.providers import ServerProvider
from inklow.ui_worker import InferenceWorker, PredictionRequest

TIMEOUT = 5


def ndjson(**event) -> bytes:
    return json.dumps(event).encode() + b"\n"


def sse(text: str, finish_reason=None) -> bytes:
    event = {"choices": [{"text": text, "finish_reason": finish_reason}]}
    return b"data: " + json.dumps(event).encode() + b"\n\n"


def send(handler, *frames, status=200, headers=()):
    """Write frames in order; an Event frame pauses the response until released."""
    try:
        handler.send_response(status)
        for name, value in headers:
            handler.send_header(name, value)
        handler.end_headers()
        for frame in frames:
            if isinstance(frame, threading.Event):
                frame.wait(TIMEOUT)
            else:
                handler.wfile.write(frame)
                handler.wfile.flush()
    except (BrokenPipeError, ConnectionResetError):
        pass  # The client closed a canceled stream.


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def handle_request(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = json.loads(self.rfile.read(length)) if length else {}
        self.server.seen.put((self.path, body))
        self.server.respond(self, self.path, body)

    do_GET = do_POST = handle_request


@pytest.fixture
def server():
    peer = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    peer.daemon_threads = True
    peer.seen = queue.Queue()
    thread = threading.Thread(target=peer.serve_forever, daemon=True)
    thread.start()
    yield peer
    peer.shutdown()
    peer.server_close()


def config_for(server, backend="ollama", **changes):
    endpoint = f"http://127.0.0.1:{server.server_address[1]}"
    values = dict(backend=backend, endpoint=endpoint, model="base") | changes
    return replace(Config(), **values)


def drain(server):
    seen = []
    while not server.seen.empty():
        seen.append(server.seen.get_nowait())
    return seen


def seen_prompts(server):
    return [body.get("prompt") for _, body in drain(server)]


def test_ollama_stream_emits_whole_words_before_unsafe_tail_arrives(server, monkeypatch):
    # C1: early whole words; split sentinel and tab never surface; proxies ignored.
    monkeypatch.setenv("HTTP_PROXY", "http://127.0.0.1:9")
    monkeypatch.setenv("http_proxy", "http://127.0.0.1:9")
    gate = threading.Event()
    server.respond = lambda handler, path, body: send(
        handler, ndjson(response=" café"), ndjson(response=" amanh"),
        ndjson(response="ã\tdepois"), gate, ndjson(response=" <"), ndjson(response="|im"),
        ndjson(response="_end|> vazou\x1b[2J"), ndjson(response="", done=True))
    stream = ServerProvider(config_for(server)).stream("Encontro no", ".", threading.Event())
    try:
        assert next(stream) == " café"
        assert next(stream) == " café amanhã"
    finally:
        gate.set()
    rest = list(stream)
    assert rest[-1] == " café amanhã depois "
    for text in rest:
        assert " café amanhã".startswith(text) or text.startswith(" café amanhã depois")
        assert not any(mark in text for mark in ("<", "|", "\t", "vazou", "\x1b"))
    assert server.seen.get(timeout=TIMEOUT)[1]["raw"] is True


def test_openai_stream_drops_controls_suffix_echo_and_token_limited_fragment(server):
    # C1: SSE comments ignored; caret suffix is not duplicated; length cut keeps whole words.
    frames = {
        "Lunch at": [b": keep-alive\n\n", sse(" the usual"), sse(" noon tomorrow"), b"data: [DONE]\n\n"],
        "Note:": [sse(" ok\x1b[2K"), sse("go\x08 next\u2028hidden text")],
        "Please send": [sse(" the rep", "length")],
    }
    server.respond = lambda handler, path, body: send(handler, *frames[body["prompt"]])
    provider = ServerProvider(config_for(server, backend="openai"))

    lunch = list(provider.stream("Lunch at", " noon tomorrow.", threading.Event()))
    assert lunch[-1] == " the usual"
    assert all("noon" not in text for text in lunch)
    assert provider.complete("Note:", "") == " ok[2Kgo next"
    assert provider.complete("Please send", "") == " the"
    assert server.seen.get(timeout=TIMEOUT)[0] == "/v1/completions"


def test_canceled_stream_stops_without_publishing_later_tokens(server):
    # C2 (provider): cancellation is observed before later frames become visible.
    gate = threading.Event()
    server.respond = lambda handler, path, body: send(
        handler, ndjson(response=" first second "), gate,
        ndjson(response="third fourth "), ndjson(response="", done=True))
    cancel = threading.Event()
    stream = ServerProvider(config_for(server)).stream("Quick", "", cancel)
    assert next(stream) == " first second"
    cancel.set()
    gate.set()
    assert list(stream) == []


@pytest.mark.parametrize("respond, message", [
    (lambda handler: send(handler, status=307, headers=[("Location", "/elsewhere")]), "redirect"),
    (lambda handler: send(handler, b"{" + b" " * (1024 * 1024 + 8)), "1 MiB"),
    (lambda handler: send(handler, b"not json\n"), "valid text completion"),
    (lambda handler: send(handler, ndjson(response=7)), "valid text completion"),
])
def test_unsafe_or_invalid_server_response_is_refused(server, respond, message):
    # C6: redirects are never followed, bodies are bounded, malformed text is rejected.
    server.respond = lambda handler, path, body: respond(handler)
    with pytest.raises(RuntimeError, match=message):
        ServerProvider(config_for(server)).complete("Bounded response", "")
    assert [path for path, _ in drain(server)] == ["/api/generate"]


class WorkerHarness:
    def __init__(self):
        QApplication.instance() or QApplication([])
        self.events = queue.Queue()
        self.worker = InferenceWorker()
        direct = Qt.ConnectionType.DirectConnection
        self.worker.provider_ready.connect(
            lambda revision, error: self.events.put(("ready", revision, error)), direct)
        self.worker.result_ready.connect(
            lambda request, text, error, final: self.events.put(("result", request, text, error, final)), direct)
        self.worker.start()

    def next(self):
        return self.events.get(timeout=TIMEOUT)

    def stop(self):
        self.worker.request_stop()
        assert self.worker.wait(TIMEOUT * 1000)
        remaining = []
        while not self.events.empty():
            remaining.append(self.events.get_nowait())
        return remaining


@pytest.fixture
def harness():
    harness = WorkerHarness()
    yield harness
    if harness.worker.isRunning():
        harness.stop()


def request(generation, prefix, revision=1):
    return PredictionRequest(generation, revision, "editor", Snapshot("writer", "body", prefix, len(prefix)))


def ollama_peer(server, scripts, prepare_gates=None):
    def respond(handler, path, body):
        if "prompt" not in body:
            gate = (prepare_gates or {}).get(body["model"])
            if gate is not None:
                gate.wait(TIMEOUT)
            if body["model"] == "missing":
                send(handler, json.dumps({"error": "model \"missing\" not found\n\x1b[31m"}).encode(), status=404)
            else:
                send(handler, ndjson(model=body["model"], done=True, done_reason="load"))
        else:
            send(handler, *scripts[body["prompt"]])
    server.respond = respond


def test_cancel_then_resubmit_keeps_new_request_and_silences_old_stream(server, harness):
    # C2: cancellation while an old stream is blocked must not drop the next request.
    gate = threading.Event()
    ollama_peer(server, {
        "Dear team, the first": [ndjson(response=" draft is "), gate, ndjson(response="stale words "),
                                 ndjson(response="", done=True)],
        "Dear team, the second": [ndjson(response=" review starts"), ndjson(response=" today", done=True,
                                                                              done_reason="stop")],
    })
    harness.worker.configure(config_for(server), 1)
    assert harness.next() == ("ready", 1, "")
    old, new = request(1, "Dear team, the first"), request(2, "Dear team, the second")
    harness.worker.submit(old)
    assert harness.next() == ("result", old, " draft is", "", False)
    harness.worker.cancel_prediction()
    harness.worker.submit(new)
    gate.set()
    events = []
    while not events or not events[-1][4]:
        events.append(harness.next())
    assert all(event[1] is new and not event[3] for event in events)
    assert events[-1][2] == " review starts today"
    assert [event[4] for event in events].count(True) == 1
    assert harness.stop() == []


def test_newest_pending_request_replaces_intermediate_requests(server, harness):
    # C2: one running + newest pending; superseded work never reaches the server or GUI.
    gate = threading.Event()
    ollama_peer(server, {
        "Status: running": [ndjson(response=" old "), gate, ndjson(response="old ", done=True)],
        "Status: newest": [ndjson(response=" shipped", done=True, done_reason="stop")],
    })
    harness.worker.configure(config_for(server), 1)
    assert harness.next() == ("ready", 1, "")
    running, intermediate, newest = (request(1, "Status: running"), request(2, "Status: middle"),
                                     request(3, "Status: newest"))
    harness.worker.submit(running)
    assert harness.next()[1] is running
    harness.worker.submit(intermediate)
    harness.worker.submit(newest)
    gate.set()
    final = harness.next()
    while not final[4]:
        assert final[1] is newest
        final = harness.next()
    assert final == ("result", newest, " shipped", "", True)
    assert harness.stop() == []
    assert seen_prompts(server) == [None, "Status: running", "Status: newest"]


def test_configuration_change_during_noncancellable_prepare_publishes_only_current_revision(server, harness):
    # C3: readiness comes after prepare and only for the latest configuration.
    gate = threading.Event()
    ollama_peer(server, {"Hello there": [ndjson(response=" friend", done=True, done_reason="stop")]},
                prepare_gates={"first": gate})
    harness.worker.configure(config_for(server, model="first"), 1)
    first_prepare = server.seen.get(timeout=TIMEOUT)[1]
    harness.worker.configure(config_for(server, model="second"), 2)
    stale = request(1, "Hello there", revision=1)
    harness.worker.submit(stale)
    gate.set()
    assert harness.next() == ("ready", 2, "")
    current = request(1, "Hello there", revision=2)
    harness.worker.submit(current)
    assert harness.next() == ("result", current, " friend", "", False)
    assert harness.next() == ("result", current, " friend", "", True)
    assert harness.stop() == []
    second_prepare = server.seen.get(timeout=TIMEOUT)[1]
    assert (first_prepare["model"], second_prepare["model"]) == ("first", "second")
    assert seen_prompts(server) == ["Hello there"]


def test_prepare_failure_reports_sanitized_local_server_reason(server, harness):
    # C3: actual setup failure surfaces as readiness error without control characters.
    ollama_peer(server, {})
    harness.worker.configure(config_for(server, model="missing"), 1)
    kind, revision, error = harness.next()
    assert (kind, revision) == ("ready", 1)
    assert "HTTP 404" in error and 'model "missing" not found' in error
    assert all(char.isprintable() for char in error)
