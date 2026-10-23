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
