"""Desktop contract regression scenarios; see desktop test mapping artifact."""
from dataclasses import replace

import pytest

from PySide6.QtWidgets import QApplication, QFileDialog, QMessageBox

from inklow.config import Config
from inklow.desktop import InklowWindow
from inklow.ui_worker import PredictionRequest


@pytest.fixture
def window(monkeypatch, tmp_path):
    # Empty model never loads native weights, and no native bridge is enabled.
    app = QApplication.instance() or QApplication([])
    monkeypatch.setattr("inklow.config.config_path", lambda: tmp_path / "settings.json")
    widget = InklowWindow(Config(model_path="", global_enabled=False), no_tray=True)
    widget.show()
    yield widget
    widget.shutdown_on_quit()
    widget.hide()
    widget.deleteLater()
    app.processEvents()


def test_visible_window_accepts_final_close_after_native_resources_stop(window, monkeypatch):
    monkeypatch.setattr(window, "_finish_shutdown", lambda: None)
    window._begin_shutdown()
    assert window._worker.wait(5000)
    assert window.close()
    assert not window.isVisible()


def test_untouched_editor_does_not_prompt_to_save_before_quit(window, monkeypatch):
    def unexpected_prompt(*args):
        pytest.fail("An untouched editor must not require saving")
    monkeypatch.setattr(QMessageBox, "warning", unexpected_prompt)
    assert window._confirm_unsaved()
    window.editor.insertPlainText("A real edit")
    monkeypatch.setattr(QMessageBox, "warning", lambda *args: QMessageBox.StandardButton.Cancel)
    assert not window._confirm_unsaved()


def test_new_action_cancel_preserves_unsaved_document(window, monkeypatch):
    """R1/S1: a destructive native action must honor Cancel."""
    window.editor.insertPlainText("Notas do café — preserve me")
    monkeypatch.setattr(QMessageBox, "warning", lambda *args: QMessageBox.StandardButton.Cancel)
    window.actions["new"].trigger()
    assert window.editor.toPlainText() == "Notas do café — preserve me"
    assert window.editor.document().isModified()
    assert window._document_path is None


def test_save_as_writes_unicode_only_on_explicit_action(window, monkeypatch, tmp_path):
    """R1/S2/S3: real atomic save, followed by cancelled Save As."""
    target = tmp_path / "writing.txt"
    window.editor.insertPlainText("Olá 🌱\nNext paragraph")
    assert not target.exists()
    monkeypatch.setattr(QFileDialog, "getSaveFileName", lambda *args: (str(target), ""))
    window.actions["save_as"].trigger()
    assert target.read_bytes() == "Olá 🌱\nNext paragraph".encode("utf-8")
    assert not window.editor.document().isModified()
    window.editor.insertPlainText(" unsaved")
    changed = window.editor.toPlainText()
    monkeypatch.setattr(QFileDialog, "getSaveFileName", lambda *args: ("", ""))
    window.actions["save_as"].trigger()
    assert target.read_bytes() == "Olá 🌱\nNext paragraph".encode("utf-8")
    assert window.editor.toPlainText() == changed
    assert window.editor.document().isModified()
    assert window._document_path == target


def test_typing_through_stream_keeps_remainder_and_rejects_old_partial(window, monkeypatch):
    """R3/S4: partial → matching typing → late partial → acceptance."""
    monkeypatch.setattr(window, "_editor_is_active", lambda: True)
    window.editor.insertPlainText("Let us meet")
    window.config = replace(window.config, model_path="unused-test-model.gguf")
    window._ready = True
    window._provider_error = ""
    snapshot = window.editor.snapshot()
    window._set_candidate(snapshot, "editor")
    window._debounce.stop()
    generation = window._state.generation
    request = PredictionRequest(generation, window._revision, "editor", snapshot)
    window._busy_generation = generation
    window._on_result(request, " tomorrow morning", "", False)
    assert window.editor._ghost == " tomorrow morning"
    assert window._busy_generation == generation
    window.editor.insertPlainText(" tom")
    assert window._state.text == "orrow morning"
    assert window.editor._ghost == "orrow morning"
    assert window._state.generation > generation
    window._on_result(request, " yesterday instead", "", True)
    assert window.editor._ghost == "orrow morning"
    assert window._accept_editor(full=True)
    assert window.editor.toPlainText() == "Let us meet tomorrow morning"
    assert window.editor._ghost == ""
    window._on_result(request, " stale text", "", True)
    assert window.editor.toPlainText() == "Let us meet tomorrow morning"
    assert window.editor._ghost == ""


def test_resident_close_hides_without_discarding_document(window, monkeypatch):
    """R2/S5: tray-backed close is not Quit or destructive document close."""
    window.editor.insertPlainText("Keep this unsaved")
    monkeypatch.setattr(window, "_tray_usable", lambda: True)
    window.close()
    assert not window.isVisible()
    assert not window._closing
    assert window.editor.toPlainText() == "Keep this unsaved"
    assert window.editor.document().isModified()
    window.show_editor()
    assert window.isVisible()


def test_close_without_tray_cancel_stays_visible(window, monkeypatch):
    """R2/S6: no invisible, unrecoverable background process after Cancel."""
    window.editor.insertPlainText("Do not lose this")
    monkeypatch.setattr(QMessageBox, "warning", lambda *args: QMessageBox.StandardButton.Cancel)
    window.close()
    assert window.isVisible()
    assert not window._closing
    assert window.editor.toPlainText() == "Do not lose this"
