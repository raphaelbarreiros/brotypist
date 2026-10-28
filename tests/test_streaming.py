"""Exercise streaming sanitization, cancellation and response limits over loopback.

The HTTP peer sends scripted frames, not predictions from a running model.
"""
from dataclasses import replace
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import queue
import threading

import pytest

from inklow.config import Config
from inklow.providers import ServerProvider

TIMEOUT = 5


def ndjson(**event) -> bytes:
    """Encode one newline-delimited Ollama response event."""
    return json.dumps(event).encode() + b"\n"


def sse(text: str, finish_reason=None) -> bytes:
    """Encode one OpenAI-compatible completion event with its SSE separator."""
    event = {"choices": [{"text": text, "finish_reason": finish_reason}]}
    return b"data: " + json.dumps(event).encode() + b"\n\n"


def send(handler, *frames, status=200, headers=()):
    """Write flushed frames in order, pausing Event frames for at most TIMEOUT seconds."""
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
    """Record requests before dispatching to the test's scripted response callback."""
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
    """Own an ephemeral loopback listener; each test supplies its response callback."""
    peer = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    peer.daemon_threads = True
    peer.seen = queue.Queue()
    thread = threading.Thread(target=peer.serve_forever, daemon=True)
    thread.start()
    yield peer
    peer.shutdown()
    peer.server_close()


def config_for(server, backend="ollama", **changes):
    """Point a provider configuration at this test's dynamically assigned local port."""
    endpoint = f"http://127.0.0.1:{server.server_address[1]}"
    values = dict(backend=backend, endpoint=endpoint, model="base") | changes
    return replace(Config(), **values)


def drain(server):
    """Drain already-recorded requests; this does not wait for in-flight handlers."""
    seen = []
    while not server.seen.empty():
        seen.append(server.seen.get_nowait())
    return seen




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


@pytest.mark.parametrize("respond", [
    lambda handler: send(handler, status=307, headers=[("Location", "/elsewhere")]),
    lambda handler: send(handler, b"{" + b" " * (1024 * 1024 + 8)),
    lambda handler: send(handler, b"not json\n"),
    lambda handler: send(handler, ndjson(response=7)),
])
def test_unsafe_or_invalid_server_response_is_refused(server, respond):
    # C6: redirects are never followed, bodies are bounded, malformed text is rejected.
    server.respond = lambda handler, path, body: respond(handler)
    with pytest.raises(RuntimeError):
        ServerProvider(config_for(server)).complete("Bounded response", "")
    assert [path for path, _ in drain(server)] == ["/api/generate"]


