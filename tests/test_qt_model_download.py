"""The model dialog must observe verified completion independently of Settings."""
from hashlib import sha256
import sys
import threading
import time

import pytest

if sys.platform == "darwin":
    pytest.skip("macOS uses the native AppKit shell", allow_module_level=True)

from PySide6.QtTest import QTest
from PySide6.QtWidgets import QApplication, QWidget

from inklow import models
from inklow.config import Config
from inklow.native.base import BridgeBase
from inklow.qt_ui import ModelManager
from inklow.runtime import Controller


def test_download_becomes_usable_without_visible_parent_or_agent_tick(monkeypatch, tmp_path):
    app = QApplication.instance() or QApplication([])
    app.setQuitOnLastWindowClosed(False)
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path / "profile"))
    monkeypatch.setattr(models, "user_cache_path", lambda *args, **kwargs: tmp_path / "cache")
    payload = b"synthetic integrity fixture" * 100
    split = len(payload) * 14 // 100
    preset = models.ModelPreset("fixture", "Fixture", "Download integrity only, never loaded as a model",
                               "fixture/model", "immutable", "fixture.gguf", len(payload), sha256(payload).hexdigest())
    monkeypatch.setattr(models, "PRESETS", (preset,))
    partial, finish = threading.Event(), threading.Event()

    class Response:
        offset = 0

        def __enter__(self):
            return self

        def __exit__(self, *args):
            return False

        def read(self, size):
            if self.offset == 0:
                self.offset = split
                return payload[:split]
            if self.offset == split:
                partial.set()
                if not finish.wait(5):
                    raise TimeoutError("Test did not release the remaining response")
                self.offset = len(payload)
                return payload[split:]
            return b""

    class Source:
        def open(self, request, timeout):
            return Response()

    monkeypatch.setattr(models.urllib.request, "build_opener", lambda *args: Source())
    controller = Controller(Config(global_enabled=False, statistics_enabled=False), BridgeBase())
    parent = QWidget()
    dialog = ModelManager(controller, parent)
    try:
        dialog.show()
        app.processEvents()
        assert dialog.isVisible() and not parent.isVisible()
        controller.download_model(preset.id)
        assert partial.wait(2)
        dialog.refresh()
        assert 0 < dialog.progress.value() < dialog.progress.maximum()
        assert not dialog.use.isEnabled()
        finish.set()
        controller._download_thread.join(timeout=3)
        assert not controller._download_thread.is_alive()
        assert models.model_path(preset).read_bytes() == payload
        assert controller.status()["download"]["state"] == "complete"
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline and not dialog.use.isEnabled():
            QTest.qWait(20)
        # No calls to Controller.tick(), Settings.refresh_status() or the
        # model dialog's refresh methods after the verified transfer finishes.
        assert dialog.use.isEnabled()
        assert dialog.progress.value() == dialog.progress.maximum()
        assert not dialog.cancel.isEnabled()
    finally:
        finish.set()
        controller.stop()
        dialog.close()
        dialog.deleteLater()
        parent.deleteLater()
        app.processEvents()
