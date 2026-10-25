"""Exercise the real Qt clipboard marshal with only the clipboard boundary replaced."""
import sys
import threading
import time

import pytest

if sys.platform == "darwin":
    pytest.skip("macOS uses NSPasteboard without the Qt shell", allow_module_level=True)

from PySide6.QtCore import QThread
from PySide6.QtWidgets import QApplication

from inklow import qt_ui, system


@pytest.fixture
def clipboard(monkeypatch):
    app = QApplication.instance() or QApplication([])
    app.setQuitOnLastWindowClosed(False)
    reads = []
    consent = [True]

    class ClipboardBoundary:
        def text(self):
            reads.append(threading.get_ident())
            assert QThread.currentThread() == app.thread()
            return "Fixture update arrives Friday"

    # Never obtain the platform clipboard, including on failure paths.
    boundary = ClipboardBoundary()
    monkeypatch.setattr(QApplication, "clipboard", lambda *args: boundary)
    reader = qt_ui.ClipboardReader(app)
    yield app, reader, reads, consent, boundary
    app.processEvents()
    reader.deleteLater()
    app.processEvents()


def read_in_worker(reader, authorized):
    done = threading.Event()
    result = []

    def run():
        try:
            result.append(reader.read(authorized=authorized))
        except Exception as error:
            result.append(error)
        finally:
            done.set()

    worker = threading.Thread(target=run, daemon=True)
    worker.start()
    return worker, done, result


def finish(app, worker, done):
    deadline = time.monotonic() + 2
    while not done.is_set() and time.monotonic() < deadline:
        app.processEvents()
        done.wait(0.001)
    assert done.is_set(), "Clipboard worker did not finish within its bound"
    worker.join()


def test_worker_clipboard_read_runs_only_on_application_thread(clipboard):
    app, reader, reads, consent, _ = clipboard
    gui_thread = threading.get_ident()
    checks = []

    def authorized():
        checks.append(threading.get_ident())
        return consent[0]

    worker, done, result = read_in_worker(reader, authorized)
    finish(app, worker, done)
    assert result == ["Fixture update arrives Friday"]
    assert reads == [gui_thread]
    assert gui_thread in checks and worker.ident in checks


def test_revocation_while_queued_prevents_clipboard_access(clipboard):
    app, reader, reads, consent, _ = clipboard
    checked = threading.Event()

    def authorized():
        allowed = consent[0]
        checked.set()
        return allowed

    worker, done, result = read_in_worker(reader, authorized)
    assert checked.wait(1)
    consent[0] = False
    finish(app, worker, done)
    assert result == [""]
    assert reads == []


def test_revocation_during_gui_read_discards_clipboard_content(clipboard, monkeypatch):
    app, reader, reads, consent, boundary = clipboard

    def revoke():
        reads.append(threading.get_ident())
        consent[0] = False
        return "Fixture update must be discarded"

    monkeypatch.setattr(boundary, "text", revoke)
    worker, done, result = read_in_worker(reader, lambda: consent[0])
    finish(app, worker, done)
    assert len(reads) == 1
    assert result == [""]


def test_unserviced_gui_queue_times_out_and_never_reads_late(clipboard):
    app, reader, reads, _, _ = clipboard
    # Deliberately do not process GUI events until every caller has returned.
    # Repeated requests must not accumulate queued reads during a GUI stall.
    for _ in range(3):
        worker, done, result = read_in_worker(reader, lambda: True)
        assert done.wait(1)
        worker.join()
        assert result == [""]
    app.processEvents()
    assert reads == []
    # Draining the expired operation must leave the reader usable.
    worker, done, result = read_in_worker(reader, lambda: True)
    finish(app, worker, done)
    assert result == ["Fixture update arrives Friday"]
    assert len(reads) == 1


def test_clipboard_error_never_exposes_native_exception_content(clipboard, monkeypatch):
    app, reader, _, _, boundary = clipboard

    def fail():
        raise RuntimeError("Fixture private clipboard content")

    monkeypatch.setattr(boundary, "text", fail)
    worker, done, result = read_in_worker(reader, lambda: True)
    finish(app, worker, done)
    assert len(result) == 1 and isinstance(result[0], RuntimeError)
    assert str(result[0]) == "Clipboard context could not be read."


def test_direct_qt_system_reader_refuses_worker_thread(clipboard):
    from types import SimpleNamespace
    _, _, reads, _, _ = clipboard
    worker, done, result = read_in_worker(
        SimpleNamespace(read=system.clipboard_text), lambda: True)
    assert done.wait(1)
    worker.join()
    assert len(result) == 1 and isinstance(result[0], RuntimeError)
    assert str(result[0]) == "Clipboard context requires the application thread."
    assert reads == []
