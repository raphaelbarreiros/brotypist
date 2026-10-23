"""Render Inklow's original ink-and-flow mark using the installed Qt toolkit.

Run with the project's Python: python tools/generate_icons.py
PNG and ICO generation is platform independent. macOS additionally regenerates
app.icns with Apple's iconutil; other platforms retain the committed ICNS.

Design: a quiet, deep-ink tile, an ivory fountain nib and a single sea-glass
continuation. The negative-space slit follows the writing direction. Tray
artwork removes the tile and color; pause bars stay legible at menu-bar size.
"""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PySide6.QtCore import QPointF, QRectF, Qt
from PySide6.QtGui import (
    QColor, QGuiApplication, QImage, QLinearGradient, QPainter, QPainterPath, QPen,
)

ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / "src" / "inklow" / "assets"
APP_SIZES = (16, 24, 32, 48, 64, 128, 256, 512, 1024)
TRAY_SIZES = (18, 22, 24, 36, 44, 48)


def nib() -> QPainterPath:
    body = QPainterPath()
    body.moveTo(300, 694)
    body.lineTo(359, 435)
    body.quadTo(363, 417, 378, 405)
    body.lineTo(570, 264)
    body.quadTo(585, 253, 598, 266)
    body.lineTo(755, 423)
    body.quadTo(768, 436, 757, 451)
    body.lineTo(616, 643)
    body.quadTo(604, 658, 586, 662)
    body.closeSubpath()
    hole = QPainterPath()
    hole.addEllipse(QPointF(522, 472), 37, 37)
    slit = QPainterPath()
    slit.moveTo(289, 706)
    slit.lineTo(513, 459)
    slit.lineTo(535, 481)
    slit.lineTo(310, 715)
    slit.closeSubpath()
    return body.subtracted(hole.united(slit))


def flow() -> QPainterPath:
    line = QPainterPath()
    line.moveTo(322, 748)
    line.cubicTo(450, 803, 652, 818, 752, 690)
    return line


def render(size: int, *, tray: bool = False, paused: bool = False,
           contrast: bool = False) -> QImage:
    # Supersample the smallest sizes to keep the diagonals balanced.
    scale = 4 if size < 128 else 1
    image = QImage(size * scale, size * scale, QImage.Format.Format_ARGB32)
    image.fill(Qt.GlobalColor.transparent)
    painter = QPainter(image)
    painter.setRenderHint(QPainter.RenderHint.Antialiasing)
    painter.scale(image.width() / 1024, image.height() / 1024)
    if not tray:
        background = QLinearGradient(120, 80, 860, 980)
        background.setColorAt(0, QColor("#223F4A"))
        background.setColorAt(0.55, QColor("#142C38"))
        background.setColorAt(1, QColor("#0B1925"))
        painter.setPen(Qt.PenStyle.NoPen)
        painter.setBrush(background)
        painter.drawRoundedRect(QRectF(64, 64, 896, 896), 206, 206)
        painter.setPen(QPen(QColor(255, 255, 255, 30), 3))
        painter.setBrush(Qt.BrushStyle.NoBrush)
        painter.drawRoundedRect(QRectF(66, 66, 892, 892), 204, 204)
    else:
        # Enlarge the unframed silhouette to use the tray's scarce pixel area.
        painter.translate(-195, -201)
        painter.scale(1.38, 1.38)
        if paused:
            painter.translate(18, -8)
            painter.scale(0.80, 0.80)
    mark = nib()
    if tray:
        color = QColor("#F8FAFC") if contrast else QColor("#000000")
        painter.setBrush(color)
        painter.setPen(QPen(QColor("#17232D"), 16) if contrast else Qt.PenStyle.NoPen)
    else:
        ivory = QLinearGradient(400, 260, 670, 670)
        ivory.setColorAt(0, QColor("#FFFDF4"))
        ivory.setColorAt(1, QColor("#E4EDE6"))
        painter.setBrush(ivory)
        painter.setPen(Qt.PenStyle.NoPen)
    painter.drawPath(mark)
    stroke = QColor("#89D7C3") if not tray else color
    if tray and contrast:
        painter.setPen(QPen(QColor("#17232D"), 62, Qt.PenStyle.SolidLine,
                            Qt.PenCapStyle.RoundCap))
        painter.drawPath(flow())
    painter.setPen(QPen(stroke, 40 if tray else 34, Qt.PenStyle.SolidLine,
                        Qt.PenCapStyle.RoundCap))
    painter.setBrush(Qt.BrushStyle.NoBrush)
    painter.drawPath(flow())
    if tray and paused:
        painter.setPen(QPen(QColor("#17232D"), 14) if contrast else Qt.PenStyle.NoPen)
        painter.setBrush(color)
        for x in (831, 951):
            painter.drawRoundedRect(QRectF(x, 618, 70, 225), 18, 18)
    painter.end()
    if scale != 1:
        image = image.scaled(size, size, Qt.AspectRatioMode.IgnoreAspectRatio,
                             Qt.TransformationMode.SmoothTransformation)
    return image


def save_png(image: QImage, target: Path) -> None:
    target.parent.mkdir(parents=True, exist_ok=True)
    if not image.save(str(target), "PNG"):
        raise OSError(f"Could not write {target}")


def write_ico(target: Path) -> None:
    # ICO permits PNG payloads, preserving alpha and avoiding Pillow as a build
    # dependency. The 256px directory width/height fields are encoded as zero.
    sizes = (16, 24, 32, 48, 64, 128, 256)
    images = [(ASSETS / "icons" / f"app-{size}.png").read_bytes() for size in sizes]
    offset = 6 + 16 * len(images)
    directory = bytearray(struct.pack("<HHH", 0, 1, len(images)))
    for size, image in zip(sizes, images):
        directory.extend(struct.pack("<BBBBHHII", size % 256, size % 256,
                                     0, 0, 1, 32, len(image), offset))
        offset += len(image)
    target.write_bytes(bytes(directory) + b"".join(images))


def write_icns(target: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="inklow-icons-") as directory:
        iconset = Path(directory) / "Inklow.iconset"
        iconset.mkdir()
        for points in (16, 32, 128, 256, 512):
            for density in (1, 2):
                suffix = "@2x" if density == 2 else ""
                png = ASSETS / "icons" / f"app-{points * density}.png"
                (iconset / f"icon_{points}x{points}{suffix}.png").write_bytes(png.read_bytes())
        subprocess.run(["/usr/bin/iconutil", "--convert", "icns", "--output", str(target),
                        str(iconset)], check=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skip-icns", action="store_true",
                        help="Render portable assets only, retaining the existing ICNS")
    args = parser.parse_args()
    application = QGuiApplication.instance() or QGuiApplication([])
    for size in APP_SIZES:
        save_png(render(size), ASSETS / "icons" / f"app-{size}.png")
    save_png(render(1024), ASSETS / "app.png")
    for paused in (False, True):
        state = "paused" if paused else "active"
        for contrast in (False, True):
            variant = "contrast" if contrast else "template"
            for size in TRAY_SIZES:
                save_png(render(size, tray=True, paused=paused, contrast=contrast),
                         ASSETS / "tray" / f"{state}-{variant}-{size}.png")
    write_ico(ASSETS / "app.ico")
    if sys.platform == "darwin" and not args.skip_icns:
        write_icns(ASSETS / "app.icns")
    # Keep Qt alive through the last raster operation.
    del application
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
