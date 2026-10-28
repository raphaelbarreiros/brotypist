#!/usr/bin/env python3
"""Rasterize the supplied Ghost Caret SVG artwork without added visual effects.

macOS uses CoreGraphics and iconutil without importing Qt. Other platforms use
Qt SVG rendering and retain the committed ICNS. Logical sizes at or below 32pt
use the canonical simplified artwork, including explicit @2x PNG representations.
The native build also compiles the layered Icon Composer document with actool.
Default generation overwrites repository icon assets and Composer sources;
--native-output instead confines compilation outputs to the supplied directory.
"""
from __future__ import annotations

import argparse
import os
import copy
import json
from pathlib import Path
import plistlib
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
    """Return a Qt raster on non-macOS platforms with a live QGuiApplication.

    Size is physical pixel width; points selects compact app artwork separately
    from resolution. Tray height follows its 18:16 canvas. Invalid arguments
    or an unreadable SVG raise ValueError.
    """
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
    """Allocate a clear macOS sRGB bitmap in pixels; raise MemoryError on failure."""
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
    """Encode a native CGImage as PNG bytes, raising OSError if encoding fails."""
    data = NSMutableData.data()
    destination = CG.CGImageDestinationCreateWithData(data, "public.png", 1, None)
    if destination is None:
        raise OSError("Could not create the native PNG encoder")
    CG.CGImageDestinationAddImage(destination, image, None)
    if not CG.CGImageDestinationFinalize(destination):
        raise OSError("Could not encode the native icon PNG")
    return bytes(data)




def render_native_tray(size: int, state: str, contrast: bool) -> bytes:
    """Return PNG bytes for the authored macOS rect/circle tray SVG subset.

    Size is physical pixel width. Unsupported geometry or color raises
    ValueError rather than silently producing a partial tray glyph.
    """
    if size <= 0 or state not in TRAY_STATES:
        raise ValueError("Invalid tray icon size or state")
    appearance = "contrast" if contrast else "template"
    source = ET.parse(SOURCES / f"tray-{state}-{appearance}.svg").getroot()
    height = round(size * 16 / 18)
    sample = 4
    context = native_bitmap(size * sample, height * sample)
    CG.CGContextSetShouldAntialias(context, True)
    # SVG coordinates start at the top-left; flip the native bitmap's Y axis
    # before drawing the supersampled 18:16 source canvas.
    CG.CGContextTranslateCTM(context, 0, height * sample)
    CG.CGContextScaleCTM(context, size * sample / 18, -height * sample / 16)

    def color(value: str, style: dict) -> tuple[float, float, float]:
        """Resolve inherited currentColor or a six-digit RGB literal."""
        if value == "currentColor":
            value = style.get("color", "#000000")
        if len(value) != 7 or not value.startswith("#"):
            raise ValueError(f"Unsupported tray SVG color: {value!r}")
        return tuple(int(value[index:index + 2], 16) / 255 for index in (1, 3, 5))

    def draw(element, inherited: dict, opacity: float) -> None:
        """Paint supported geometry with inherited style and cumulative opacity."""
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
    """Return native PNG bytes on macOS, otherwise a Qt image, from app artwork.

    Size is pixel width; points chooses the compact drawing at up to 32pt.
    The native renderer handles the authored rectangles and vertical gradients,
    not arbitrary SVG features; non-rectangle leaf elements are skipped.
    """
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
        """Decode the artwork's short/full RGB hex literals plus explicit alpha."""
        value = value.lstrip("#")
        if len(value) == 3:
            value = "".join(c * 2 for c in value)
        return tuple(int(value[n:n + 2], 16) / 255 for n in (0, 2, 4)) + (alpha,)

    def draw(node, inherited=None, opacity=1):
        """Paint rectangle leaves with per-shape graphics state and inherited style."""
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
    """Create parent directories and overwrite target with PNG bytes or a Qt image."""
    target.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(image, bytes):
        target.write_bytes(image)
    elif not image.save(str(target), "PNG"):
        raise OSError(f"Could not write {target}")


def write_ico(target: Path) -> None:
    """Overwrite target with an ICO assembled from already-generated app PNGs."""
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
    """Build a temporary logical-size iconset and invoke macOS iconutil.

    Temporary PNGs are removed on exit; filesystem and subprocess errors
    propagate to the caller rather than preserving a successful build claim.
    """
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


def write_composer_sources() -> None:
    """Overwrite Composer layers and manifest from the authored full-size SVGs.

    Rectangle order defines enclosure/caret/suggestion membership. This is an
    artwork-specific split, not a general SVG-to-Composer conversion.
    """
    namespace = "http://www.w3.org/2000/svg"
    ET.register_namespace("", namespace)
    document = SOURCES / "BroTypist.icon"
    assets = document / "Assets"
    assets.mkdir(parents=True, exist_ok=True)
    names = ("enclosure", "caret", "suggestions")
    for variant in APP_VARIANTS:
        appearance = "" if variant == "normal" else f"-{variant}"
        source = ET.parse(SOURCES / f"app-icon{appearance}.svg").getroot()
        shapes = [node for node in source if node.tag.rsplit("}", 1)[-1] == "rect"]
        partitions = (shapes[:2], shapes[2:5], shapes[5:])
        for name, shapes_in_layer in zip(names, partitions):
            # All layers use the same full canvas; no scale/translation,
            # alpha, color, radius or geometry is inferred or redesigned.
            layer = ET.Element(f"{{{namespace}}}svg", {
                **source.attrib, "width": "1024", "height": "1024",
            })
            for node in source:
                if node.tag.rsplit("}", 1)[-1] == "defs":
                    layer.append(copy.deepcopy(node))
            for node in shapes_in_layer:
                layer.append(copy.deepcopy(node))
            ET.ElementTree(layer).write(
                assets / f"{name}{appearance}.svg", encoding="unicode")
    # Composer serializes front-to-back, opposite SVG's paint order.
    layers = [{
        "name": name.title(),
        "image-name-specializations": [
            {"value": f"{name}.svg"},
            {"appearance": "dark", "value": f"{name}-dark.svg"},
            {"appearance": "tinted", "value": f"{name}-tinted.svg"},
        ],
        "glass": False,
    } for name in reversed(names)]
    manifest = {
        "fill": {"solid": "srgb:0.00000,0.00000,0.00000,0.00000"},
        "groups": [{
            "name": "Ghost Caret",
            "layers": layers,
            "specular": False,
            "shadow": {"kind": "none", "opacity": 0},
            "translucency": {"enabled": False, "value": 0},
            "blur-material": 0,
        }],
        "supported-platforms": {"squares": ["macOS"]},
    }
    (document / "icon.json").write_text(
        json.dumps(manifest, indent=2) + "\n", encoding="utf-8")


def compile_native_icons(output: Path) -> dict:
    """Compile Apple-owned runtime resources into the caller's build workspace.

    These resources target macOS 26; the app's deployment target is unchanged.
    Older consumers retain the original classic handoff ICNS, not actool's
    synthesized legacy drawing.
    Return the generated bundle metadata. Native tool failures propagate;
    missing Assets.car or an unexpected icon name raises RuntimeError.
    """
    if sys.platform != "darwin":
        raise RuntimeError("Icon Composer compilation requires a native macOS build")
    output.mkdir(parents=True, exist_ok=True)
    metadata = output / "icon-info.plist"
    subprocess.run([
        "/usr/bin/xcrun", "actool",
        str(SOURCES / "BroTypist.icon"),
        "--compile", str(output),
        "--platform", "macosx", "--target-device", "mac",
        "--minimum-deployment-target", "26.0",
        "--app-icon", "BroTypist",
        "--output-partial-info-plist", str(metadata),
        "--output-format", "human-readable-text", "--warnings", "--notices",
    ], check=True)
    with metadata.open("rb") as stream:
        info = plistlib.load(stream)
    if not (output / "Assets.car").is_file() or info.get("CFBundleIconName") != "BroTypist":
        raise RuntimeError("actool did not produce the native app icon and its bundle metadata")
    return info


def main() -> int:
    """Generate repository assets or run a source-only/native-output build mode."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skip-icns", action="store_true",
                        help="Retain the existing ICNS; still regenerate PNG and ICO")
    parser.add_argument("--composer-only", action="store_true",
                        help="Regenerate layered .icon source documents only")
    parser.add_argument("--native-output", type=Path,
                        help="Compile native resources only, into this scoped directory")
    args = parser.parse_args()
    if args.composer_only:
        write_composer_sources()
        return 0
    if args.native_output is not None:
        compile_native_icons(args.native_output.resolve())
        return 0
    write_composer_sources()
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
