#!/usr/bin/env python3
"""Render BroTypist's original Ghost Caret SVGs and Icon Composer documents.

Run with the project's Python: python tools/generate_icons.py
macOS uses Icon Composer's ictool for the real system icon mask, CoreGraphics
for PNG processing/tray glyphs, and iconutil for ICNS; Qt is never imported there.
Other platforms use Qt's portable mask and retain the committed ICNS.
The serif-free single-pill document is used at 32 points and below, even @2x.

Native document structure follows Xcode's installed macOS Icon Composer Icon
template and the observed image-name-specializations/group fields documented at
https://github.com/peterpoliwoda/icon-composer-template. Artwork is authored only
from the supplied DESIGN.md geometry; no third-party artwork is incorporated.
"""
from __future__ import annotations

import argparse
from functools import cache
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET

if sys.platform == "darwin":
    import Quartz as CG
    from Foundation import NSData, NSMutableData
else:
    os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
    from PySide6.QtCore import QRectF, Qt
    from PySide6.QtGui import (
        QColor, QGuiApplication, QImage, QPainter, QPainterPath, QPen,
    )
    from PySide6.QtSvg import QSvgRenderer

ROOT = Path(__file__).resolve().parents[1]
SOURCES = ROOT / "design" / "icons"
ASSETS = ROOT / "src" / "inklow" / "assets"
APP_SIZES = (16, 24, 32, 48, 64, 128, 256, 512, 1024)
TRAY_SIZES = (18, 22, 24, 36, 44, 48)
APP_VARIANTS = ("normal", "dark", "tinted")
TRAY_STATES = ("active", "paused", "loading", "off")


def tile_path(inset: float = 0) -> QPainterPath:
    """Portable mask only; native Icon Composer imports remain full bleed."""
    path = QPainterPath()
    path.moveTo(30, inset)
    path.lineTo(70, inset)
    path.cubicTo(92, inset, 100 - inset, 8, 100 - inset, 30)
    path.lineTo(100 - inset, 70)
    path.cubicTo(100 - inset, 92, 92, 100 - inset, 70, 100 - inset)
    path.lineTo(30, 100 - inset)
    path.cubicTo(8, 100 - inset, inset, 92, inset, 70)
    path.lineTo(inset, 30)
    path.cubicTo(inset, 8, 8, inset, 30, inset)
    path.closeSubpath()
    return path


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
        painter.scale(image.width() / 100, image.height() / 100)
        painter.setClipPath(tile_path())
        renderer.render(painter, QRectF(0, 0, 100, 100))
        # One output pixel, not one design unit; keep the hairline inside.
        width = 100 / size
        border = QColor("#FFFFFF")
        border.setAlphaF(0.12 if variant == "dark" else 0.08)
        painter.setPen(QPen(border, width))
        painter.setBrush(Qt.BrushStyle.NoBrush)
        painter.drawPath(tile_path(width / 2))
    painter.end()
    if scale != 1:
        image = image.scaled(size, height, Qt.AspectRatioMode.IgnoreAspectRatio,
                             Qt.TransformationMode.SmoothTransformation)
    return image


@cache
def icon_composer_tool() -> Path:
    """Resolve the exporter belonging to the selected Xcode, not actool's alias."""
    developer = Path(subprocess.check_output(
        ["/usr/bin/xcode-select", "-p"], text=True).strip())
    candidates = (
        developer.parent / "Applications" / "Icon Composer.app" / "Contents"
        / "Executables" / "ictool",
        Path("/Applications/Icon Composer.app/Contents/Executables/ictool"),
    )
    for candidate in candidates:
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return candidate
    raise FileNotFoundError(
        "Icon Composer's ictool is required for macOS system-mask exports. "
        "Select an Xcode installation containing Icon Composer with xcode-select.")


def native_bitmap(width: int, height: int):
    """Allocate an sRGB bitmap without AppKit windows or a Qt application."""
    context = CG.CGBitmapContextCreate(
        None, width, height, 8, width * 4,
        CG.CGColorSpaceCreateWithName(CG.kCGColorSpaceSRGB),
        CG.kCGImageAlphaPremultipliedLast | CG.kCGBitmapByteOrder32Big)
    if context is None:
        raise MemoryError(f"Could not allocate {width}×{height} icon bitmap")
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


def native_inner_stroke(image, variant: str) -> bytes:
    """Derive a one-pixel inner hairline from Apple's exported alpha mask."""
    width, height = CG.CGImageGetWidth(image), CG.CGImageGetHeight(image)
    bounds = ((0, 0), (width, height))
    mask_context = native_bitmap(width, height)
    CG.CGContextSetRGBFillColor(mask_context, 1, 1, 1, 1)
    CG.CGContextFillRect(mask_context, bounds)
    CG.CGContextSetBlendMode(mask_context, CG.kCGBlendModeDestinationIn)
    CG.CGContextDrawImage(mask_context, bounds, image)
    mask = CG.CGBitmapContextCreateImage(mask_context)
    eroded_context = native_bitmap(width, height)
    CG.CGContextDrawImage(eroded_context, bounds, mask)
    CG.CGContextSetBlendMode(eroded_context, CG.kCGBlendModeDestinationIn)
    for x, y in ((-1, 0), (1, 0), (0, -1), (0, 1)):
        CG.CGContextDrawImage(eroded_context, ((x, y), (width, height)), mask)
        # Drawing a shifted image does not composite outside its destination rect.
        if x:
            CG.CGContextClearRect(
                eroded_context, ((0 if x > 0 else width - 1, 0), (1, height)))
        else:
            CG.CGContextClearRect(
                eroded_context, ((0, 0 if y > 0 else height - 1), (width, 1)))
    CG.CGContextSetBlendMode(mask_context, CG.kCGBlendModeDestinationOut)
    CG.CGContextDrawImage(mask_context, bounds, CG.CGBitmapContextCreateImage(eroded_context))
    output = native_bitmap(width, height)
    CG.CGContextDrawImage(output, bounds, image)
    CG.CGContextSetAlpha(output, 0.12 if variant == "dark" else 0.08)
    CG.CGContextDrawImage(output, bounds, CG.CGBitmapContextCreateImage(mask_context))
    return native_png(CG.CGBitmapContextCreateImage(output))


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
    """Use Apple's actual enclosure on macOS; never silently replace its mask."""
    if sys.platform != "darwin":
        return render(size, variant=variant, points=points)
    if size <= 0 or variant not in APP_VARIANTS:
        raise ValueError("Invalid app icon size or appearance")
    logical_size = points if points is not None else size
    document = "BroTypist-small.icon" if logical_size <= 32 else "BroTypist.icon"
    rendition = {"normal": "Default", "dark": "Dark", "tinted": "TintedDark"}[variant]
    with tempfile.TemporaryDirectory(prefix="brotypist-native-icon-") as directory:
        output = Path(directory) / "icon.png"
        subprocess.run([
            str(icon_composer_tool()), str(SOURCES / document),
            "--export-image", "--output-file", str(output),
            "--platform", "macOS", "--rendition", rendition,
            "--width", str(logical_size), "--height", str(logical_size),
            "--scale", str(size / logical_size),
        ], check=True)
        payload = output.read_bytes()
        data = NSData.dataWithBytes_length_(payload, len(payload))
        source = CG.CGImageSourceCreateWithData(data, None)
        image = CG.CGImageSourceCreateImageAtIndex(source, 0, None) if source else None
        if image is None or CG.CGImageGetWidth(image) != size or CG.CGImageGetHeight(image) != size:
            raise ValueError(f"Icon Composer returned an invalid {size}px export")
    return native_inner_stroke(image, variant)


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
