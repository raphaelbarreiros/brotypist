"""Explicit, cancellable model downloads outside the GUI thread."""
from __future__ import annotations

from pathlib import Path
from threading import Event

from PySide6.QtCore import QThread, Qt, Signal
from PySide6.QtWidgets import (
    QComboBox, QDialog, QDialogButtonBox, QFileDialog, QHBoxLayout, QLabel,
    QProgressBar, QPushButton, QVBoxLayout,
)

from .models import DEFAULT_PRESET, PRESETS, download_model, get_preset, is_installed, model_path


def _size(size: int) -> str:
    return f"{size / (1024 ** 3):.2f} GiB" if size >= 1024 ** 3 else f"{size / (1024 ** 2):.0f} MiB"


class ModelDownload(QThread):
    progress = Signal(object, object)
    completed = Signal(str, str)

    def __init__(self, preset_id: str, parent=None):
        super().__init__(parent)
        self.preset_id = preset_id
        self.cancel = Event()

    def run(self) -> None:
        try:
            path = download_model(self.preset_id, progress=self.progress.emit, cancel=self.cancel)
        except Exception as exc:
            self.completed.emit("", str(exc) or type(exc).__name__)
        else:
            self.completed.emit(str(path), "")


class ModelManager(QDialog):
    model_selected = Signal(str)
    download_finished = Signal()

    def __init__(self, current_path: str, parent=None):
        super().__init__(parent)
        self.setWindowTitle("Inklow models")
        self.setMinimumWidth(570)
        self._worker: ModelDownload | None = None
        self._close_after_cancel = False
        layout = QVBoxLayout(self)
        intro = QLabel("Download a local writing model, or use your own GGUF. Only the model is downloaded; your writing never leaves this computer.")
        intro.setWordWrap(True)
        layout.addWidget(intro)
        self.presets = QComboBox()
        for preset in PRESETS:
            self.presets.addItem(f"{preset.label} — {_size(preset.size_bytes)}", preset.id)
        self.presets.setCurrentIndex(max(0, self.presets.findData(DEFAULT_PRESET)))
        layout.addWidget(self.presets)
        self.description = QLabel()
        self.description.setTextFormat(Qt.TextFormat.PlainText)
        self.description.setWordWrap(True)
        layout.addWidget(self.description)
        self.status = QLabel()
        self.status.setTextFormat(Qt.TextFormat.PlainText)
        self.status.setWordWrap(True)
        layout.addWidget(self.status)
        self.progress = QProgressBar()
        self.progress.setRange(0, 1000)
        self.progress.setValue(0)
        self.progress.setAccessibleName("Model download progress")
        layout.addWidget(self.progress)
        row = QHBoxLayout()
        self.download = QPushButton("Download model")
        self.download.clicked.connect(self._download)
        row.addWidget(self.download)
        self.cancel_button = QPushButton("Cancel download")
        self.cancel_button.clicked.connect(self.cancel_download)
        self.cancel_button.setEnabled(False)
        row.addWidget(self.cancel_button)
        self.use = QPushButton("Use installed model")
        self.use.clicked.connect(self._use)
        row.addWidget(self.use)
        layout.addLayout(row)
        self.custom = QPushButton("Choose custom GGUF…")
        self.custom.clicked.connect(self._custom)
        layout.addWidget(self.custom)
        self.current = QLabel()
        self.current.setTextFormat(Qt.TextFormat.PlainText)
        self.current.setWordWrap(True)
        self.current.setTextInteractionFlags(Qt.TextInteractionFlag.TextSelectableByMouse)
        layout.addWidget(self.current)
        self.set_current_path(current_path)
        buttons = QDialogButtonBox(QDialogButtonBox.StandardButton.Close)
        buttons.rejected.connect(self.reject)
        layout.addWidget(buttons)
        self.presets.currentIndexChanged.connect(self._refresh)
        self._refresh()

    @property
    def downloading(self) -> bool:
        return self._worker is not None and self._worker.isRunning()

    def set_current_path(self, path: str) -> None:
        warning = "Tiny model: limited quality — choose Balanced in Models.\n" if get_preset("tiny").filename.casefold() in path.casefold() else ""
        self.current.setText(warning + f"Current model: {path or 'None selected'}")
        self._refresh()

    def _refresh(self, *args) -> None:
        preset = get_preset(self.presets.currentData())
        self.description.setText(preset.description)
        installed = is_installed(preset)
        self.use.setEnabled(installed and not self.downloading)
        self.download.setEnabled(not self.downloading)
        self.presets.setEnabled(not self.downloading)
        self.custom.setEnabled(not self.downloading)
        self.cancel_button.setEnabled(self.downloading)
        if not self.downloading:
            self.status.setText(f"Installed: {model_path(preset)}" if installed else f"Not installed. Download size: {_size(preset.size_bytes)}. Enough free space is required.")

    def _download(self) -> None:
        if self.downloading:
            return
        self.progress.setValue(0)
        self._download_error = ""
        self._close_after_cancel = False
        self._worker = ModelDownload(self.presets.currentData(), self)
        self._worker.progress.connect(self._progress)
        self._worker.completed.connect(self._completed)
        self._worker.finished.connect(self._finished)
        self._worker.start()
        self._refresh()
        self.status.setText("Downloading model… The previous model remains selected.")

    def _progress(self, received: int, total: int) -> None:
        self.progress.setValue(min(1000, int(received * 1000 / total)) if total else 0)
        self.status.setText(f"Downloaded {_size(received)} of {_size(total)}. Verifying the full file before use." if received == total else f"Downloaded {_size(received)} of {_size(total)}")

    def _completed(self, path: str, error: str) -> None:
        self._download_error = error
        if path:
            self.progress.setValue(1000)

    def _finished(self) -> None:
        worker = self._worker
        self._worker = None
        self._refresh()
        error = getattr(self, "_download_error", "")
        self.status.setText(f"Download not applied: {error}" if error else "Download verified. Choose Use installed model to select it.")
        if worker is not None:
            worker.deleteLater()
        self.download_finished.emit()
        if self._close_after_cancel:
            super().reject()

    def cancel_download(self) -> None:
        if self._worker is not None:
            self._worker.cancel.set()
            self.cancel_button.setEnabled(False)
            self.status.setText("Cancelling download… The previous model remains selected.")

    def wait_for_download(self) -> None:
        if self._worker is not None:
            self._worker.wait()

    def _use(self) -> None:
        preset = get_preset(self.presets.currentData())
        if is_installed(preset):
            self.model_selected.emit(str(model_path(preset)))

    def _custom(self) -> None:
        filename, _ = QFileDialog.getOpenFileName(self, "Choose a local GGUF model", "", "GGUF models (*.gguf);;All files (*)")
        if not filename:
            return
        try:
            with Path(filename).open("rb") as stream:
                if stream.read(4) != b"GGUF":
                    raise ValueError("The selected file is not a GGUF model.")
        except (OSError, ValueError) as exc:
            self.status.setText(str(exc))
            return
        self.model_selected.emit(filename)

    def reject(self) -> None:
        if self.downloading:
            self._close_after_cancel = True
            self.cancel_download()
        else:
            super().reject()

    def closeEvent(self, event) -> None:
        if self.downloading:
            self.reject()
            event.ignore()
        else:
            event.accept()
