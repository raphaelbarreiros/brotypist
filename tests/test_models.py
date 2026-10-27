"""Model replacement must preserve working weights on cancellation/corruption."""
from hashlib import sha256
from io import BytesIO
from threading import Event

import pytest

from inklow import models


@pytest.fixture
def model_source(monkeypatch, tmp_path):
    payload = b"verified replacement weights"
    preset = models.ModelPreset(
        "fixture", "Fixture", "Synthetic weights for integrity boundaries only",
        "fixture/model", "immutable", "fixture.gguf", len(payload), sha256(payload).hexdigest())
    monkeypatch.setattr(models, "PRESETS", (preset,))
    monkeypatch.setattr(models, "user_cache_path", lambda *args, **kwargs: tmp_path)
    target = models.model_path(preset)
    target.parent.mkdir(parents=True)
    previous = b"previous working model"
    target.write_bytes(previous)
    return preset, payload, target, previous


def serve(monkeypatch, payload):
    class ResponseSource:
        def open(self, request, timeout):
            return BytesIO(payload)
    monkeypatch.setattr(models.urllib.request, "build_opener", lambda *args: ResponseSource())


def test_corrupt_model_download_preserves_previous_weights_and_removes_partial(monkeypatch, model_source):
    preset, payload, target, previous = model_source
    serve(monkeypatch, b"x" * len(payload))
    with pytest.raises(RuntimeError):
        models.download_model(preset.id)
    assert target.read_bytes() == previous
    assert list(target.parent.glob(".download-*")) == []


def test_cancellation_after_last_download_chunk_does_not_replace_model(monkeypatch, model_source):
    preset, payload, target, previous = model_source
    serve(monkeypatch, payload)
    cancel = Event()
    def cancel_after_bytes(received, total):
        if received:
            cancel.set()
    with pytest.raises(RuntimeError):
        models.download_model(preset.id, progress=cancel_after_bytes, cancel=cancel)
    assert target.read_bytes() == previous
    assert list(target.parent.glob(".download-*")) == []


def test_cancellation_interrupts_stalled_body_read_and_removes_partial(monkeypatch, model_source):
    import http.client
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
    import threading

    preset, payload, target, previous = model_source
    release, reading = threading.Event(), threading.Event()

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_GET(self):
            self.send_response(200)
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload[:8])
            self.wfile.flush()
            release.wait(5)
            try:
                self.wfile.write(payload[8:])
            except (BrokenPipeError, ConnectionResetError):
                pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()

    class Response:
        def __init__(self):
            self.connection = http.client.HTTPConnection("127.0.0.1", server.server_port, timeout=5)
            self.connection.request("GET", "/")
            self.response = self.connection.getresponse()
            self.fp = self.response.fp
            self.reads = 0

        def __enter__(self):
            return self

        def __exit__(self, *_args):
            self.response.close()
            self.connection.close()

        def read(self, size):
            return self.read1(size)

        def read1(self, size):
            self.reads += 1
            if self.reads == 2:
                reading.set()
            return self.response.read1(size)

    class Source:
        def open(self, _request, timeout):
            return Response()

    monkeypatch.setattr(models.urllib.request, "build_opener", lambda *_args: Source())
    cancel, errors = models.DownloadCancellation(), []

    def download():
        try:
            models.download_model(preset.id, cancel=cancel)
        except RuntimeError as error:
            errors.append(str(error))

    thread = threading.Thread(target=download, daemon=True)
    try:
        thread.start()
        assert reading.wait(3), "The owned response never entered its stalled read"
        cancel.set()
        thread.join(timeout=2)
        assert not thread.is_alive()
        assert errors == ["Model download cancelled."]
        assert target.read_bytes() == previous
        assert list(target.parent.glob(".download-*")) == []
    finally:
        cancel.set()
        release.set()
        thread.join(timeout=5)
        server.shutdown()
        server.server_close()
