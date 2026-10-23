"""Document painting and a non-activating system-wide suggestion window."""
from __future__ import annotations

import sys
from typing import Callable

from PySide6.QtCore import QPoint, QPointF, QRectF, Qt, Signal
from PySide6.QtGui import QColor, QFont, QFontDatabase, QPainter, QTextLayout, QTextOption
from PySide6.QtWidgets import QApplication, QPlainTextEdit, QWidget

from .core import Snapshot


INK = "#275DAD"
GHOST = "#697F9D"


def _codepoint_offset(text: str, utf16_offset: int) -> int:
    return len(text.encode("utf-16-le")[: utf16_offset * 2].decode("utf-16-le", errors="ignore"))


class WritingEditor(QPlainTextEdit):
    activity = Signal()
    focus_changed = Signal(bool)
    composition_changed = Signal(bool)

    def __init__(self, parent=None):
        super().__init__(parent)
        self._ghost = ""
        self.composing = False
        self.accept_suggestion: Callable[[bool], bool] = lambda full: False
        self.dismiss_suggestion: Callable[[], bool] = lambda: False
        self.setObjectName("writingEditor")
        self.setAccessibleName("Writing document")
        self.setAccessibleDescription("Tab accepts the next suggested word. Shift plus Tab accepts the full suggestion. Escape dismisses it.")
        self.setPlaceholderText("Start writing. Your words stay here; suggestions come from a model on this computer.")
        self.setTabChangesFocus(False)
        self.setLineWrapMode(QPlainTextEdit.LineWrapMode.WidgetWidth)
        font = QFont()
        available = QFontDatabase.families()
        for family in ("Georgia", "Noto Serif", "DejaVu Serif"):
            if family in available:
                font.setFamily(family)
                break
        font.setPointSizeF(17)
        self.setFont(font)
        self.document().setDocumentMargin(28)
        self.textChanged.connect(self.activity)
        self.cursorPositionChanged.connect(self.activity)
        self.selectionChanged.connect(self.activity)
        self.verticalScrollBar().valueChanged.connect(lambda _: self.viewport().update())
        self.horizontalScrollBar().valueChanged.connect(lambda _: self.viewport().update())

    def snapshot(self) -> Snapshot:
        text = self.toPlainText()
        cursor = self.textCursor()
        caret = _codepoint_offset(text, cursor.position())
        anchor = _codepoint_offset(text, cursor.anchor())
        return Snapshot(
            app_id="inklow.editor",
            field_id=f"editor:{id(self)}",
            text=text,
            caret=caret,
            selection_length=abs(anchor - caret),
        )

    def set_ghost(self, text: str) -> None:
        if text != self._ghost:
            self._ghost = text
            self.viewport().update()

    def keyPressEvent(self, event) -> None:
        modifiers = event.modifiers()
        if not self.composing and self._ghost:
            if event.key() in (Qt.Key.Key_Tab, Qt.Key.Key_Backtab):
                if modifiers in (Qt.KeyboardModifier.NoModifier, Qt.KeyboardModifier.ShiftModifier):
                    full = modifiers == Qt.KeyboardModifier.ShiftModifier or event.key() == Qt.Key.Key_Backtab
                    if self.accept_suggestion(full):
                        event.accept()
                        return
            elif event.key() == Qt.Key.Key_Escape and modifiers == Qt.KeyboardModifier.NoModifier:
                if self.dismiss_suggestion():
                    event.accept()
                    return
        super().keyPressEvent(event)

    def inputMethodEvent(self, event) -> None:
        composing = bool(event.preeditString())
        # Invalidate before Qt commits any text or moves the preedit cursor.
        self.composing = composing
        self.composition_changed.emit(composing)
        super().inputMethodEvent(event)
        self.activity.emit()

    def focusInEvent(self, event) -> None:
        super().focusInEvent(event)
        self.focus_changed.emit(True)

    def focusOutEvent(self, event) -> None:
        self.set_ghost("")
        self.focus_changed.emit(False)
        super().focusOutEvent(event)

    def paintEvent(self, event) -> None:
        super().paintEvent(event)
        if (not self._ghost or not self.hasFocus() or not self.window().isActiveWindow()
                or self.composing or self.textCursor().hasSelection()):
            return
        caret = self.cursorRect()
        if not self.viewport().rect().intersects(caret):
            return
        inline = self.textCursor().atBlockEnd()
        margin = self.document().documentMargin()
        origin_x = float(caret.left())
        origin_y = float(caret.top() if inline else caret.bottom() + 4)
        available = self.viewport().width() - origin_x - margin
        if available < 35:
            origin_x = margin
            origin_y = float(caret.bottom() + 2)
            available = self.viewport().width() - 2 * margin
        layout = QTextLayout(self._ghost, self.font())
        option = QTextOption()
        option.setWrapMode(QTextOption.WrapMode.WrapAtWordBoundaryOrAnywhere)
        layout.setTextOption(option)
        layout.beginLayout()
        height = 0.0
        for index in range(3):
            line = layout.createLine()
            if not line.isValid():
                break
            x = origin_x if index == 0 else margin
            line.setLineWidth(max(20.0, available if index == 0 else self.viewport().width() - 2 * margin))
            line.setPosition(QPointF(x, origin_y + height))
            height += line.height()
        layout.endLayout()
        painter = QPainter(self.viewport())
        painter.setClipRect(self.viewport().rect())
        if not inline:
            # Mid-line predictions float below the caret, never replace or cover
            # the suffix on the current line and never enter the document/undo stack.
            background = QRectF(origin_x - 4, origin_y - 2, max(20.0, available) + 8, height + 4)
            painter.fillRect(background, QColor("#F3F6FB"))
            painter.setPen(QColor("#D6E1F0"))
            painter.drawRect(background)
        painter.setPen(QColor(GHOST))
        layout.draw(painter, QPointF(0, 0))


class SuggestionOverlay(QWidget):
    """A painted tool window with neither keyboard focus nor mouse interception."""

    def __init__(self):
        flags = (Qt.WindowType.Tool | Qt.WindowType.FramelessWindowHint
                 | Qt.WindowType.WindowStaysOnTopHint
                 | Qt.WindowType.WindowDoesNotAcceptFocus
                 | Qt.WindowType.WindowTransparentForInput)
        super().__init__(None, flags)
        self.setAttribute(Qt.WidgetAttribute.WA_ShowWithoutActivating)
        self.setAttribute(Qt.WidgetAttribute.WA_TransparentForMouseEvents)
        self.setAttribute(Qt.WidgetAttribute.WA_TranslucentBackground)
        self.setFocusPolicy(Qt.FocusPolicy.NoFocus)
        self.setAccessibleName("Inklow suggestion")
        self._text = ""
        self._layout: QTextLayout | None = None
        self._hint = "Tab: next word     Shift+Tab: all     Esc: dismiss"
        font = QFont()
        font.setPointSizeF(12)
        self.setFont(font)

    def show_suggestion(self, text: str, rect: tuple[float, float, float, float]) -> bool:
        if not text:
            self.hide()
            return False
        anchor, screen = self._logical_anchor(rect)
        if screen is None:
            self.hide()
            return False
        bounds = screen.availableGeometry()
        width = min(480, max(200, bounds.width() - 24))
        self._text = text
        layout = QTextLayout(text, self.font())
        option = QTextOption()
        option.setWrapMode(QTextOption.WrapMode.WrapAtWordBoundaryOrAnywhere)
        layout.setTextOption(option)
        layout.beginLayout()
        height = 0.0
        for _ in range(3):
            line = layout.createLine()
            if not line.isValid():
                break
            line.setLineWidth(width - 24)
            line.setPosition(QPointF(12, 10 + height))
            height += line.height()
        layout.endLayout()
        self._layout = layout
        self.resize(width, int(height + 43))
        x = min(max(anchor.x(), bounds.left() + 6), bounds.right() - self.width() - 6)
        y = anchor.y() + 5
        if y + self.height() > bounds.bottom() - 6:
            y = anchor.y() - self.height() - max(8, int(rect[3]))
        y = max(bounds.top() + 6, min(y, bounds.bottom() - self.height() - 6))
        self.move(x, y)
        self.show()
        self.update()
        return True

    @staticmethod
    def _logical_anchor(rect):
        x, y, width, height = rect
        screens = QApplication.screens()
        if sys.platform == "darwin":
            point = QPoint(round(x), round(y + height))
            return point, QApplication.screenAt(point)
        # Qt on Windows retains native monitor origins but scales their sizes;
        # X11 uses a uniformly scaled virtual desktop. Respect both conventions.
        for screen in screens:
            geometry = screen.geometry()
            scale = screen.devicePixelRatio()
            left = geometry.x() if sys.platform == "win32" else geometry.x() * scale
            top = geometry.y() if sys.platform == "win32" else geometry.y() * scale
            if (left <= x < left + geometry.width() * scale
                    and top <= y < top + geometry.height() * scale):
                point = QPoint(round(geometry.x() + (x - left) / scale),
                               round(geometry.y() + (y + height - top) / scale))
                return point, screen
        return QPoint(), None

    def paintEvent(self, event) -> None:
        painter = QPainter(self)
        painter.setRenderHint(QPainter.RenderHint.Antialiasing)
        painter.setPen(QColor("#C7D4E8"))
        painter.setBrush(QColor("#F7F9FD"))
        painter.drawRoundedRect(QRectF(self.rect()).adjusted(0.5, 0.5, -0.5, -0.5), 5, 5)
        painter.setPen(QColor(INK))
        if self._layout is not None:
            self._layout.draw(painter, QPointF(0, 0))
        font = QFont(self.font())
        font.setPointSizeF(9)
        painter.setFont(font)
        painter.setPen(QColor("#526379"))
        painter.drawText(QRectF(12, self.height() - 27, self.width() - 24, 20),
                         Qt.AlignmentFlag.AlignVCenter, self._hint)
