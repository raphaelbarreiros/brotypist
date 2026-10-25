#!/usr/bin/env python3
"""Rasterize the supplied Ghost Caret SVG artwork without added visual effects.

macOS uses CoreGraphics and iconutil without importing Qt. Other platforms use
Qt SVG rendering and retain the committed ICNS. Logical sizes at or below 32pt
use the canonical simplified artwork, including explicit @2x PNG representations.
These are flat derivatives of the handoff SVGs, not compiled Icon Composer assets.
"""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET

if sys.platform == "darwin":
    import Quartz as CG
    from Foundation import NSMutableData
else:
    os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
    from PySide6.QtCore import QRectF, Qt
    from PySide6.QtGui import (
        QGuiApplication, QImage, QPainter,
    )
    from PySide6.QtSvg import QSvgRenderer

ROOT = Path(__file__).resolve().parents[1]
SOURCES = ROOT / "design" / "icons"
ASSETS = ROOT / "src" / "inklow" / "assets"
APP_SIZES = (16, 24, 32, 48, 64, 128, 256, 512, 1024)
COMPACT_POINTS = (16, 24, 32)
TRAY_SIZES = (18, 22, 24, 36, 44, 48)
APP_VARIANTS = ("normal", "dark", "tinted")
TRAY_STATES = ("active", "paused", "loading", "off")




def render(size: int, *, tray: bool = False, state: str = "active",
           contrast: bool = False, variant: str = "normal",
           points: int | None = None) -> QImage:
    """Render a square app tile or an 18:16 tray glyph, without text/fonts."""
    if size <= 0:
        raise ValueError("Icon size must be positive")
    if state not in TRAY_STATES:
        raise ValueError(f"Unknown tray state: {state!r}")
    if variant not in APP_VARIANTS:
        raise ValueError(f"Unknown app variant: {variant!r}")
    if tray:
        source = f"tray-{state}-{'contrast' if contrast else 'template'}.svg"
    else:
        small = "-small" if (points if points is not None else size) <= 32 else ""
        appearance = "" if variant == "normal" else f"-{variant}"
        source = f"app-icon{small}{appearance}.svg"
    renderer = QSvgRenderer(str(SOURCES / source))
    if not renderer.isValid():
        raise ValueError(f"Invalid or missing icon source: {source}")

    # Render tiny geometry at 4×, preserving fractional glyph coordinates.
    scale = 4 if size < 128 else 1
    height = round(size * 16 / 18) if tray else size
    image = QImage(size * scale, height * scale, QImage.Format.Format_ARGB32)
    image.fill(Qt.GlobalColor.transparent)
    painter = QPainter(image)
    painter.setRenderHint(QPainter.RenderHint.Antialiasing)
    if tray:
        renderer.render(painter, QRectF(0, 0, image.width(), image.height()))
    else:
        renderer.render(painter, QRectF(0, 0, image.width(), image.height()))
    painter.end()
    if scale != 1:
        image = image.scaled(size, height, Qt.AspectRatioMode.IgnoreAspectRatio,
                             Qt.TransformationMode.SmoothTransformation)
    return image


def native_bitmap(width: int, height: int):
    """Allocate an sRGB bitmap without AppKit windows or a Qt application."""
    context = CG.CGBitmapContextCreate(
        None, width, height, 8, width * 4,
        CG.CGColorSpaceCreateWithName(CG.kCGColorSpaceSRGB),
        CG.kCGImageAlphaPremultipliedLast | CG.kCGBitmapByteOrder32Big)
    if context is None:
        raise MemoryError(f"Could not allocate {width}×{height} icon bitmap")
    CG.CGContextSetAllowsAntialiasing(context, True)
    CG.CGContextSetShouldAntialias(context, True)
    CG.CGContextClearRect(context, ((0, 0), (width, height)))
    return context


def native_png(image) -> bytes:
    data = NSMutableData.data()
    destination = CG.CGImageDestinationCreateWithData(data, "public.png", 1, None)
    if destination is None:
        raise OSError("Could not create the native PNG encoder")
    CG.CGImageDestinationAddImage(destination, image, None)
    if not CG.CGImageDestinationFinalize(destination):
        raise OSError("Could not encode the native icon PNG")
    return bytes(data)




def render_native_tray(size: int, state: str, contrast: bool) -> bytes:
    """Render our rect/circle-only tray SVG sources using CoreGraphics."""
    if size <= 0 or state not in TRAY_STATES:
        raise ValueError("Invalid tray icon size or state")
    appearance = "contrast" if contrast else "template"
    source = ET.parse(SOURCES / f"tray-{state}-{appearance}.svg").getroot()
    height = round(size * 16 / 18)
    sample = 4
    context = native_bitmap(size * sample, height * sample)
    CG.CGContextSetShouldAntialias(context, True)
    CG.CGContextTranslateCTM(context, 0, height * sample)
    CG.CGContextScaleCTM(context, size * sample / 18, -height * sample / 16)

    def color(value: str, style: dict) -> tuple[float, float, float]:
        if value == "currentColor":
            value = style.get("color", "#000000")
        if len(value) != 7 or not value.startswith("#"):
            raise ValueError(f"Unsupported tray SVG color: {value!r}")
        return tuple(int(value[index:index + 2], 16) / 255 for index in (1, 3, 5))

    def draw(element, inherited: dict, opacity: float) -> None:
        tag = element.tag.rsplit("}", 1)[-1]
        if tag in {"title", "desc"}:
            return
        style = {**inherited, **element.attrib}
        opacity *= float(element.attrib.get("opacity", "1"))
        if tag in {"svg", "g"}:
            for child in element:
                draw(child, style, opacity)
            return
        CG.CGContextBeginPath(context)
        if tag == "rect":
            rectangle = ((float(style.get("x", 0)), float(style.get("y", 0))),
                         (float(style["width"]), float(style["height"])))
            rx = float(style.get("rx", 0))
            path = CG.CGPathCreateWithRoundedRect(
                rectangle, rx, float(style.get("ry", rx)), None)
            CG.CGContextAddPath(context, path)
        elif tag == "circle":
            x, y, radius = (float(style[key]) for key in ("cx", "cy", "r"))
            CG.CGContextAddEllipseInRect(
                context, ((x - radius, y - radius), (radius * 2, radius * 2)))
        else:
            raise ValueError(f"Unsupported tray SVG geometry: {tag!r}")
        fill, stroke = style.get("fill", "#000000"), style.get("stroke", "none")
        CG.CGContextSetAlpha(context, opacity)
        if fill != "none":
            CG.CGContextSetRGBFillColor(context, *color(fill, style), 1)
        if stroke != "none":
            CG.CGContextSetRGBStrokeColor(context, *color(stroke, style), 1)
            CG.CGContextSetLineWidth(context, float(style.get("stroke-width", 1)))
            CG.CGContextSetLineJoin(context, CG.kCGLineJoinRound)
        if fill != "none" and stroke != "none":
            mode = CG.kCGPathFillStroke
        elif fill != "none":
            mode = CG.kCGPathFill
        elif stroke != "none":
            mode = CG.kCGPathStroke
        else:
            return
        CG.CGContextDrawPath(context, mode)

    draw(source, {}, 1)
    output = native_bitmap(size, height)
    CG.CGContextSetInterpolationQuality(output, CG.kCGInterpolationHigh)
    CG.CGContextDrawImage(output, ((0, 0), (size, height)),
                          CG.CGBitmapContextCreateImage(context))
    return native_png(CG.CGBitmapContextCreateImage(output))


def render_app(size: int, *, variant: str = "normal",
               points: int | None = None) -> bytes | QImage:
    """Rasterize canonical artwork without adding enclosure lighting or bevels."""
    if sys.platform != "darwin":
        return render(size, variant=variant, points=points)
    if size <= 0 or variant not in APP_VARIANTS:
        raise ValueError("Invalid app icon size or appearance")
    logical_size = points if points is not None else size
    small = "-small" if logical_size <= 32 else ""
    appearance = "" if variant == "normal" else f"-{variant}"
    root = ET.parse(SOURCES / f"app-icon{small}{appearance}.svg").getroot()
    scale = 4 if size < 128 else 1
    context = native_bitmap(size * scale, size * scale)
    CG.CGContextTranslateCTM(context, 0, size * scale)
    CG.CGContextScaleCTM(context, size * scale / 100, -size * scale / 100)
    gradients = {node.get("id"): node for node in root.iter()
                 if node.tag.rsplit("}", 1)[-1] == "linearGradient"}

    def rgba(value, alpha=1):
        value = value.lstrip("#")
        if len(value) == 3:
            value = "".join(c * 2 for c in value)
        return tuple(int(value[n:n + 2], 16) / 255 for n in (0, 2, 4)) + (alpha,)

    def draw(node, inherited=None, opacity=1):
        style = dict(inherited or {})
        style.update(node.attrib)
        opacity *= float(node.get("opacity", 1))
        tag = node.tag.rsplit("}", 1)[-1]
        if tag in ("svg", "g"):
            for child in node:
                draw(child, style, opacity)
            return
        if tag != "rect":
            return
        rect = ((float(style.get("x", 0)), float(style.get("y", 0))),
                (float(style["width"]), float(style["height"])))
        radius = float(style.get("rx", 0))
        path = CG.CGPathCreateWithRoundedRect(rect, radius, radius, None)
        fill = style.get("fill", "#000000")
        CG.CGContextSaveGState(context)
        CG.CGContextSetAlpha(context, opacity)
        if fill.startswith("url(#"):
            gradient = gradients[fill[5:-1]]
            stops = list(gradient)
            components = sum((rgba(stop.get("stop-color"),
                                   float(stop.get("stop-opacity", 1))) for stop in stops), ())
            locations = [float(stop.get("offset").rstrip("%")) /
                         (100 if stop.get("offset").endswith("%") else 1) for stop in stops]
            colors = CG.CGGradientCreateWithColorComponents(
                CG.CGColorSpaceCreateWithName(CG.kCGColorSpaceSRGB),
                components, locations, len(stops))
            CG.CGContextAddPath(context, path)
            CG.CGContextClip(context)
            CG.CGContextDrawLinearGradient(context, colors, (50, 0), (50, 100), 0)
        elif fill != "none":
            CG.CGContextSetRGBFillColor(context, *rgba(fill, float(style.get("fill-opacity", 1))))
            CG.CGContextAddPath(context, path)
            CG.CGContextFillPath(context)
        if style.get("stroke", "none") != "none":
            CG.CGContextSetRGBStrokeColor(context, *rgba(
                style["stroke"], float(style.get("stroke-opacity", 1))))
            CG.CGContextSetLineWidth(context, float(style.get("stroke-width", 1)))
            CG.CGContextAddPath(context, path)
            CG.CGContextStrokePath(context)
        CG.CGContextRestoreGState(context)

    draw(root)
    image = CG.CGBitmapContextCreateImage(context)
    if scale != 1:
        output = native_bitmap(size, size)
        CG.CGContextSetInterpolationQuality(output, CG.kCGInterpolationHigh)
        CG.CGContextDrawImage(output, ((0, 0), (size, size)), image)
        image = CG.CGBitmapContextCreateImage(output)
    return native_png(image)


def save_png(image: bytes | QImage, target: Path) -> None:
    target.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(image, bytes):
        target.write_bytes(image)
    elif not image.save(str(target), "PNG"):
        raise OSError(f"Could not write {target}")


def write_ico(target: Path) -> None:
    # ICO permits PNG payloads, preserving alpha without a Pillow dependency.
    # The 256px directory width/height fields are encoded as zero.
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
    with tempfile.TemporaryDirectory(prefix="brotypist-icons-") as directory:
        iconset = Path(directory) / "BroTypist.iconset"
        iconset.mkdir()
        for points in (16, 32, 128, 256, 512):
            for density in (1, 2):
                suffix = "@2x" if density == 2 else ""
                # 32pt @2x is still a small icon, despite being 64 pixels wide.
                save_png(render_app(points * density, points=points),
                         iconset / f"icon_{points}x{points}{suffix}.png")
        subprocess.run(["/usr/bin/iconutil", "--convert", "icns", "--output", str(target),
                        str(iconset)], check=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skip-icns", action="store_true",
                        help="Retain the existing ICNS; still regenerate PNG and ICO")
    args = parser.parse_args()
    application = None if sys.platform == "darwin" else (
        QGuiApplication.instance() or QGuiApplication([]))
    for variant in APP_VARIANTS:
        appearance = "" if variant == "normal" else f"-{variant}"
        for size in APP_SIZES:
            save_png(render_app(size, variant=variant),
                     ASSETS / "icons" / f"app{appearance}-{size}.png")
        # A 32pt icon at 2× needs the compact drawing, not app-64.png's full
        # I-beam and two pills. Keep both physical-size representations.
        for points in COMPACT_POINTS:
            save_png(render_app(points * 2, variant=variant, points=points),
                     ASSETS / "icons" / f"app{appearance}-{points}@2x.png")
    save_png(render_app(1024), ASSETS / "app.png")
    for state in TRAY_STATES:
        for contrast in (False, True):
            appearance = "contrast" if contrast else "template"
            for size in TRAY_SIZES:
                image = (render_native_tray(size, state, contrast)
                         if sys.platform == "darwin"
                         else render(size, tray=True, state=state, contrast=contrast))
                save_png(image, ASSETS / "tray" / f"{state}-{appearance}-{size}.png")
    write_ico(ASSETS / "app.ico")
    if sys.platform == "darwin" and not args.skip_icns:
        write_icns(ASSETS / "app.icns")
    # Keep Qt alive on portable platforms through the last raster operation.
    del application
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
