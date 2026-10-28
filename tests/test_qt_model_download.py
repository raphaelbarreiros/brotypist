"""Constrain model-dialog refresh and Quit ownership using synthetic transfers.

Downloads use temporary weights and controlled responses; the shutdown case
uses a real worker child and a loopback HTTP peer that the shell must not stop.
"""
from hashlib import sha256
import sys
import threading
import time

import pytest

if sys.platform == "darwin":
    pytest.skip("macOS uses the native AppKit shell", allow_module_level=True)

from PySide6.QtTest import QTest
from PySide6.QtWidgets import QApplication, QMessageBox, QWidget

from inklow import models
from inklow.config import Config
from inklow.native.base import BridgeBase
from inklow import qt_ui
from inklow.qt_ui import ModelManager
from inklow.runtime import Controller


@pytest.mark.parametrize("license_required", [False, True])
def test_download_becomes_usable_without_visible_parent_or_agent_tick(monkeypatch, tmp_path, license_required):
    app = QApplication.instance() or QApplication([])
    app.setQuitOnLastWindowClosed(False)
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path / "profile"))
    monkeypatch.setattr(models, "user_cache_path", lambda *args, **kwargs: tmp_path / "cache")
    payload = b"synthetic integrity fixture" * 100
    split = len(payload) * 14 // 100
    preset = models.ModelPreset("fixture", "Fixture", "Download integrity only, never loaded as a model",
                               "fixture/model", "immutable", "fixture.gguf", len(payload), sha256(payload).hexdigest(),
                               requires_license_acceptance=license_required,
                               license_name="Fixture terms", license_url="https://example.invalid/fixture-terms")
    monkeypatch.setattr(models, "PRESETS", (preset,))
    partial, finish = threading.Event(), threading.Event()

    class Response:
        """Pause after a partial transfer so progress is observed before completion."""

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
        prompts = []
        if license_required:
            monkeypatch.setattr(QMessageBox, "question", lambda *_: QMessageBox.StandardButton.No)
            dialog.download.click()
            assert controller._download_thread is None
            assert not models.model_path(preset).exists()
        def accept(*arguments):
            prompts.append(arguments[2])
            return QMessageBox.StandardButton.Yes
        monkeypatch.setattr(QMessageBox, "question", accept)
        dialog.download.click()
        assert len(prompts) == 1
        if license_required:
            assert preset.license_url in prompts[0] and "accepts these terms" in prompts[0]
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


def test_model_capacity_constraint_is_visible_and_not_downloadable(monkeypatch, tmp_path):
    app = QApplication.instance() or QApplication([])
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path / "profile"))
    monkeypatch.setattr(models, "user_cache_path", lambda *args, **kwargs: tmp_path / "cache")
    preset = models.ModelPreset("capacity", "Capacity fixture", "Not a real model", "fixture/model",
                               "immutable", "fixture.gguf", 32, "0" * 64,
                               minimum_memory_bytes=8 * 1024**3)
    monkeypatch.setattr(models, "PRESETS", (preset,))
    monkeypatch.setattr(models, "physical_memory_bytes", lambda: 4 * 1024**3)
    controller = Controller(Config(global_enabled=False), BridgeBase())
    dialog = ModelManager(controller)
    try:
        assert "Requires at least 8 GB" in dialog.description.text()
        assert not dialog.download.isEnabled()
        assert not dialog.use.isEnabled()
        dialog.download.click()
        assert controller._download_thread is None
    finally:
        controller.stop()
        dialog.close()
        dialog.deleteLater()
        app.processEvents()


def quit_from_settings(app, shell):
    """Invoke visible Quit inside the event loop; timeout is a failing exit code."""
    from PySide6.QtCore import QTimer
    shell.show_settings("General")
    timeout = QTimer()
    timeout.setSingleShot(True)
    timeout.timeout.connect(lambda: app.exit(87))
    timeout.start(5000)
    QTimer.singleShot(0, shell.settings.quit_button.click)
    try:
        assert app.exec() == 0
    finally:
        timeout.stop()
    assert shell._stopped
    assert not shell.timer.isActive()


def dispose_shell(app, shell):
    """Stop shell owners and release their Qt objects without quitting the shared app."""
    shell.stop()
    app.aboutToQuit.disconnect(shell.stop)
    for window in (shell.settings, shell.welcome, shell.overlay, shell.accessory, shell.tray, shell.menu):
        if window:
            window.deleteLater()
    app.processEvents()


def test_visible_quit_reaps_real_pending_model_child_but_not_external_server(monkeypatch, tmp_path):
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
    from inklow.config import Config
    class Handler(BaseHTTPRequestHandler):
        """Hold a loopback request open while the shell cancels its owned client."""

        def log_message(self, *_arguments):
            pass
        def do_POST(self):
            self.rfile.read(int(self.headers.get("Content-Length", "0")))
            self.server.entered.set()
            self.server.release.wait(5)
            try:
                self.send_response(200)
                self.end_headers()
            except (BrokenPipeError, ConnectionResetError):
                pass
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    server.entered, server.release = threading.Event(), threading.Event()
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    app = QApplication.instance() or QApplication([])
    app.setQuitOnLastWindowClosed(False)
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path / "profile"))
    monkeypatch.setattr(qt_ui, "Controller", lambda config: Controller(config, BridgeBase()))
    config = Config(backend="ollama", endpoint=f"http://127.0.0.1:{server.server_port}",
                    model="synthetic-shutdown-peer", global_enabled=False, statistics_enabled=False,
                    idle_release_seconds=0)
    shell = qt_ui.MenuAgent(app, config, no_tray=True)
    try:
        shell.controller.start()
        shell.controller._client.submit(config, "synthetic isolated shutdown document", "")
        assert server.entered.wait(5)
        process = shell.controller._client._process
        assert process is not None and process.poll() is None
        shell.controller._client.submit(config, "newer isolated pending request", "")
        quit_from_settings(app, shell)
        assert process.poll() is not None
        assert shell.controller._client.status()["worker_pid"] is None
        assert shell.controller.bridge._stopped.is_set()
        assert server_thread.is_alive()
        assert not server.release.is_set()
    finally:
        server.release.set()
        dispose_shell(app, shell)
        server.shutdown()
        server.server_close()


def test_visible_quit_cancels_download_removes_partial_and_preserves_old_weights(monkeypatch, tmp_path):
    app = QApplication.instance() or QApplication([])
    app.setQuitOnLastWindowClosed(False)
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path / "profile"))
    monkeypatch.setattr(models, "user_cache_path", lambda *args, **kwargs: tmp_path / "cache")
    payload = b"synthetic integrity fixture"
    preset = models.ModelPreset("shutdown", "Shutdown fixture", "Never loaded as a model", "fixture/model",
                               "immutable", "fixture.gguf", len(payload), sha256(payload).hexdigest())
    monkeypatch.setattr(models, "PRESETS", (preset,))
    monkeypatch.setattr(qt_ui, "Controller", lambda config: Controller(config, BridgeBase()))
    shell = qt_ui.MenuAgent(app, Config(global_enabled=False, statistics_enabled=False), no_tray=True)
    target = models.model_path(preset)
    target.parent.mkdir(parents=True)
    original = b"existing weights preserved on cancel"
    target.write_bytes(original)
    partial = threading.Event()
    class Response:
        """Keep partial bytes pending until Quit signals the download cancellation."""

        offset = 0
        def __enter__(self):
            return self
        def __exit__(self, *_arguments):
            return False
        def read(self, _size):
            if not self.offset:
                self.offset = 1
                return payload[:8]
            partial.set()
            if not shell.controller._download_cancel.wait(3):
                raise TimeoutError("Quit did not cancel the owned download")
            return b""
    class Source:
        def open(self, _request, timeout):
            return Response()
    monkeypatch.setattr(models.urllib.request, "build_opener", lambda *args: Source())
    try:
        shell.controller.download_model(preset.id)
        assert partial.wait(2)
        thread = shell.controller._download_thread
        assert list(target.parent.glob(".download-*"))
        quit_from_settings(app, shell)
        assert shell.controller._download_cancel.is_set()
        assert not thread.is_alive()
        assert not list(target.parent.glob(".download-*"))
        assert target.read_bytes() == original
    finally:
        shell.controller.cancel_download()
        dispose_shell(app, shell)
