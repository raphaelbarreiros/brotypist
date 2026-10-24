"""Build a native Inklow application with the current Python and local backend.

Install the project's local and build extras first, then run this tool on each
native OS. This is an onedir application: keep the Windows/Linux directory
intact. Builds never include model weights, user settings or a model download.
An existing destination is refused unless --replace identifies our own build.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
APP_ID = "com.inklow.Inklow"
MARKER = ".inklow-build.json"


def marker_path(bundle: Path, platform: str) -> Path:
    if platform == "darwin":
        return bundle / "Contents" / "Resources" / MARKER
    return bundle / MARKER


def ensure_destination(destination: Path, *, replace: bool, platform: str) -> None:
    """Refuse symlinks and unowned paths, including with explicit replacement."""
    if destination.is_symlink():
        raise ValueError(f"Refusing a symlink destination: {destination}")
    if not destination.exists():
        return
    if not replace:
        raise FileExistsError(f"Destination exists: {destination}; choose another path or use --replace for an Inklow build")
    marker = marker_path(destination, platform)
    if marker.is_symlink() or not marker.is_file():
        raise ValueError(f"Refusing to replace an unmarked destination: {destination}")
    try:
        identity = json.loads(marker.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise ValueError(f"Invalid Inklow build marker: {marker}") from error
    if not isinstance(identity, dict) or identity.get("application") != APP_ID or identity.get("platform") != platform:
        raise ValueError(f"Refusing to replace a different application: {destination}")


def desktop_value(value: str) -> str:
    """Escape a desktop-entry string value (not an Exec argument)."""
    return value.replace("\\", "\\\\").replace("\n", "\\n").replace("\r", "\\r").replace("\t", "\\t")


def desktop_exec(path: Path) -> str:
    # Exec values follow desktop-entry quoting, not shell/shlex quoting. Double
    # percent avoids accidental field-code expansion in a directory name.
    value = str(path).replace("%", "%%")
    for character in ("\\", '"', "`", "$"):
        value = value.replace(character, "\\" + character)
    return desktop_value('"' + value + '"')


def prepare_linux(bundle: Path, destination: Path) -> None:
    """Create a movable shell launcher and desktop integration payload."""
    launcher = bundle / "launch-inklow"
    shutil.copy2(ROOT / "packaging" / "linux" / "launch-inklow", launcher)
    launcher.chmod(0o755)
    icon_root = bundle / "share" / "icons" / "hicolor"
    for size in (16, 24, 32, 48, 64, 128, 256, 512, 1024):
        target = icon_root / f"{size}x{size}" / "apps" / f"{APP_ID}.png"
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / "src" / "inklow" / "assets" / "icons" / f"app-{size}.png", target)
    template = (ROOT / "packaging" / "linux" / f"{APP_ID}.desktop.in").read_text(encoding="utf-8")
    icon = destination / "share" / "icons" / "hicolor" / "256x256" / "apps" / f"{APP_ID}.png"
    entry = template.replace("@EXEC@", desktop_exec(destination / "launch-inklow"))
    entry = entry.replace("@ICON@", desktop_value(str(icon)))
    applications = bundle / "share" / "applications"
    applications.mkdir(parents=True, exist_ok=True)
    (applications / f"{APP_ID}.desktop").write_text(entry, encoding="utf-8")
    (bundle / "Inklow.desktop").write_text(entry, encoding="utf-8")
    (bundle / "Inklow.desktop").chmod(0o755)


def reject_private_payload(bundle: Path) -> None:
    """Fail closed if a wheel or custom hook unexpectedly supplies private data."""
    forbidden = {".gguf", ".ggml", ".safetensors", ".pt", ".pth", ".db", ".sqlite", ".sqlite3"}
    settings = {"settings.json", "config.json", "secrets.yaml", ".env", "history.enc"}
    for path in bundle.rglob("*"):
        if path.is_file() and (path.suffix.lower() in forbidden or path.name.lower() in settings):
            raise RuntimeError(f"Refusing a model/settings payload in the application: {path.relative_to(bundle)}")


def publish(bundle: Path, destination: Path, *, replace: bool, platform: str) -> None:
    """Promote only a complete build, retaining the old one if promotion fails."""
    ensure_destination(destination, replace=replace, platform=platform)
    # The caller's TemporaryDirectory is on the destination filesystem. No
    # unrelated application directory is ever removed, even with --replace.
    previous = bundle.parent / "previous-build"
    if destination.exists():
        destination.rename(previous)
    try:
        bundle.rename(destination)
    except OSError:
        if previous.exists():
            previous.rename(destination)
        raise
    if previous.exists():
        shutil.rmtree(previous)


def build(destination: Path, *, replace: bool = False) -> Path:
    platform = sys.platform
    if platform not in ("darwin", "win32", "linux"):
        raise RuntimeError(f"Unsupported native build platform: {platform}")
    # Do not resolve the final component: ensure_destination must see symlinks.
    destination = destination.expanduser()
    destination = destination.parent.resolve() / destination.name
    if platform == "darwin" and destination.suffix != ".app":
        raise ValueError("The macOS destination must end in .app")
    if destination == ROOT or ROOT.is_relative_to(destination):
        raise ValueError("The build destination must not contain the project source tree")
    ensure_destination(destination, replace=replace, platform=platform)
    for package in ("PyInstaller", "llama_cpp"):
        if importlib.util.find_spec(package) is None:
            raise RuntimeError(f"Missing {package}; install this project's local and build extras in the current interpreter")
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".inklow-build-", dir=destination.parent) as temporary:
        staging = Path(temporary)
        subprocess.run([
            sys.executable, "-m", "PyInstaller", "--noconfirm", "--clean",
            "--distpath", str(staging / "dist"), "--workpath", str(staging / "work"),
            str(ROOT / "Inklow.spec"),
        ], cwd=ROOT, check=True)
        bundle = staging / "dist" / ("Inklow.app" if platform == "darwin" else "Inklow")
        if not bundle.is_dir():
            raise RuntimeError("PyInstaller did not create the expected native application")
        if platform == "linux":
            prepare_linux(bundle, destination)
        reject_private_payload(bundle)
        marker = marker_path(bundle, platform)
        marker.parent.mkdir(parents=True, exist_ok=True)
        marker.write_text(json.dumps({"application": APP_ID, "platform": platform}, indent=2) + "\n", encoding="utf-8")
        if platform == "darwin":
            # Finalize the sealed ownership marker with the same signing
            # identity as PyInstaller's executable and collected libraries.
            identity = os.environ.get("INKLOW_CODESIGN_IDENTITY") or "-"
            command = ["/usr/bin/codesign", "--force", "--sign", identity]
            if identity != "-":
                command.extend(["--options", "runtime"])
            subprocess.run([*command, str(bundle)], check=True)
            subprocess.run(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(bundle)], check=True)
        publish(bundle, destination, replace=replace, platform=platform)
    return destination


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    default = ROOT / "dist" / ("Inklow.app" if sys.platform == "darwin" else "Inklow")
    parser.add_argument("--output", type=Path, default=default,
                        help="Final .app or application directory (default: dist/Inklow[.app])")
    parser.add_argument("--replace", action="store_true",
                        help="Replace only a destination marked as an Inklow build")
    args = parser.parse_args(argv)
    try:
        result = build(args.output, replace=args.replace)
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Inklow build: {error}\n")
    print(f"Native application: {result}")
    if sys.platform == "linux":
        print(f"Launch: {result / 'launch-inklow'}")
        print(f"Desktop entry: {result / 'share' / 'applications' / (APP_ID + '.desktop')}")
        print("Keep the directory intact. Rebuild its desktop entry if you move it.")
    elif sys.platform == "win32":
        print("Keep Inklow.exe, Inklow-cli.exe and their _internal directory together.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
