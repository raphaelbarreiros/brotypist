"""Package an already-built amd64 Linux onedir into .deb and portable .tar.gz.

Requires dpkg-dev. Native shared-library dependencies are derived from the ELF
payload, not guessed from the Python environment. No model download or rebuild.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
APP_ID = "com.inklow.Inklow"
FORBIDDEN_SUFFIXES = {".gguf", ".ggml", ".safetensors", ".pt", ".pth", ".db", ".sqlite", ".sqlite3"}
FORBIDDEN_NAMES = {"settings.json", "config.json", "secrets.yaml", ".env", "history.enc"}


def run(command: list[str], **kwargs) -> str:
    result = subprocess.run(command, text=True, capture_output=True, timeout=300, **kwargs)
    if result.returncode:
        raise RuntimeError(f"{command[0]} exited {result.returncode}:\n{result.stderr}\n{result.stdout}")
    return result.stdout.strip()


def validate_bundle(bundle: Path, version: str) -> None:
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError("--version must be a numeric major.minor.patch release")
    if bundle.is_symlink() or not bundle.is_dir():
        raise ValueError("--bundle must be a real Inklow onedir directory")
    marker = bundle / ".inklow-build.json"
    identity = json.loads(marker.read_text(encoding="utf-8"))
    if identity.get("application") != APP_ID or identity.get("platform") != "linux":
        raise ValueError("Bundle is not marked as an Inklow Linux build")
    source_commit = run(["git", "-C", str(ROOT), "rev-parse", "HEAD"])
    if (identity.get("version") != version or identity.get("architecture") != "x86_64"
            or identity.get("source_commit") != source_commit):
        raise ValueError("Bundle version, architecture, or source differs from the packaging checkout")
    executable = bundle / "Inklow"
    with executable.open("rb") as stream:
        header = stream.read(20)
    if len(header) != 20 or header[:6] != b"\x7fELF\x02\x01" or struct.unpack_from("<H", header, 18)[0] != 62:
        raise ValueError("Release bundle must be an ELF64 little-endian amd64 executable")
    environment = dict(os.environ)
    for name in ("PYTHONPATH", "PYTHONHOME", "GI_TYPELIB_PATH", "LD_LIBRARY_PATH"):
        environment.pop(name, None)
    if run([str(executable), "--version"], env=environment) != f"Inklow {version}":
        raise ValueError("--version differs from frozen application metadata")
    for path in bundle.rglob("*"):
        if path.is_symlink() and not path.resolve().is_relative_to(bundle.resolve()):
            raise ValueError(f"Bundle symlink escapes application: {path}")
        if path.is_file() and (path.suffix.lower() in FORBIDDEN_SUFFIXES or path.name.lower() in FORBIDDEN_NAMES):
            raise ValueError(f"Refusing private/model payload: {path.relative_to(bundle)}")
    internal = bundle / "_internal"
    for namespace in ("Atspi-2.0", "GLib-2.0", "GObject-2.0"):
        if not (internal / "gi_typelibs" / f"{namespace}.typelib").is_file():
            raise ValueError(f"Frozen accessibility typelib missing: {namespace}")
    if not any(internal.glob("gi/_gi*.so")) or not any(internal.glob("libatspi.so*")):
        raise ValueError("Frozen GI extension or native AT-SPI library missing")


def copy_application(bundle: Path, target: Path) -> None:
    shutil.copytree(bundle, target, symlinks=True)
    # build_app's entries point to its original destination. Portable users run
    # launch-inklow directly; .deb integration is rendered for installed paths.
    for entry in (target / "Inklow.desktop", target / "share/applications" / f"{APP_ID}.desktop"):
        entry.unlink(missing_ok=True)
    for path in target.rglob("*"):
        if not path.is_symlink():
            path.chmod(0o755 if path.is_dir() or path.stat().st_mode & 0o111 else 0o644)
    target.chmod(0o755)


def shared_library_dependencies(app: Path, work: Path) -> str:
    debian = work / "debian"
    debian.mkdir()
    (debian / "control").write_text(
        "Source: inklow\nSection: utils\nPriority: optional\nMaintainer: Inklow contributors <noreply@localhost>\n\n"
        "Package: inklow\nArchitecture: amd64\nDescription: Local-first predictive typing\n", encoding="utf-8")
    elfs = []
    library_directories = set()
    for path in app.rglob("*"):
        if path.is_file() and not path.is_symlink():
            with path.open("rb") as stream:
                header = stream.read(18)
            if header[:4] == b"\x7fELF" and len(header) >= 18:
                # Only dynamic executables/shared objects; never data/object files.
                if struct.unpack_from("<H", header, 16)[0] in (2, 3):
                    elfs.append(path)
                    library_directories.add(path.parent)
    command = ["dpkg-shlibdeps", "--ignore-missing-info", "-O"]
    command.extend(f"-l{path}" for path in sorted(library_directories))
    command.extend(f"-e{path}" for path in sorted(elfs))
    output = run(command, cwd=work)
    dependency_line = next((line for line in output.splitlines() if line.startswith("shlibs:Depends=")), None)
    if not dependency_line:
        raise RuntimeError("dpkg-shlibdeps did not emit native runtime dependencies")
    dependencies = dependency_line.partition("=")[2]
    # at-spi2-core supplies the session accessibility bus/registry, not Python.
    # Qt's xcb plugin dlopens the cursor helper, so it is not always ELF-linked.
    return ", ".join(filter(None, [dependencies, "libc6 (>= 2.39)", "at-spi2-core (>= 2.52)",
                                     "dbus-user-session", "libxcb-cursor0"]))


def package(bundle: Path, output: Path, version: str) -> tuple[Path, Path]:
    if sys.platform != "linux" or run(["dpkg", "--print-architecture"]) != "amd64":
        raise RuntimeError("Build Linux release packages on native Debian/Ubuntu amd64")
    bundle = bundle.expanduser().absolute()
    validate_bundle(bundle, version)
    output = output.expanduser().absolute()
    if output == bundle or output.is_relative_to(bundle):
        raise ValueError("Release output must be outside the application bundle")
    output.mkdir(parents=True, exist_ok=True)
    stem = f"Inklow-{version}-linux-amd64"
    deb, archive = output / f"{stem}.deb", output / f"{stem}.tar.gz"
    if deb.exists() or archive.exists():
        raise FileExistsError("Release artifact already exists; choose an empty output directory")
    with tempfile.TemporaryDirectory(prefix=".inklow-package-", dir=output) as temporary:
        work = Path(temporary)
        root = work / "deb-root"
        app = root / "opt/inklow"
        app.parent.mkdir(parents=True)
        copy_application(bundle, app)
        binary = root / "usr/bin/inklow"
        binary.parent.mkdir(parents=True)
        binary.write_text('#!/bin/sh\nexec /opt/inklow/launch-inklow "$@"\n', encoding="utf-8")
        binary.chmod(0o755)
        applications = root / "usr/share/applications"
        applications.mkdir(parents=True)
        template = (ROOT / "packaging/linux" / f"{APP_ID}.desktop.in").read_text(encoding="utf-8")
        entry = template.replace("@EXEC@", "/usr/bin/inklow").replace("@ICON@", APP_ID)
        (applications / f"{APP_ID}.desktop").write_text(entry, encoding="utf-8")
        icons = root / "usr/share/icons/hicolor"
        icons.parent.mkdir(parents=True)
        shutil.copytree(app / "share/icons/hicolor", icons)
        dependencies = shared_library_dependencies(app, work)
        installed_size = (sum(path.stat().st_size for path in root.rglob("*") if path.is_file()) + 1023) // 1024
        control = root / "DEBIAN"
        control.mkdir()
        metadata = (ROOT / "packaging/linux/control.in").read_text(encoding="utf-8")
        metadata = metadata.replace("@VERSION@", version).replace("@SIZE@", str(installed_size)).replace("@DEPENDS@", dependencies)
        (control / "control").write_text(metadata, encoding="utf-8")
        # No maintainer scripts: dpkg owns only app/integration paths and can
        # never remove or rewrite users' external models, settings or history.
        staged_deb = work / deb.name
        run(["dpkg-deb", "--build", "--root-owner-group", str(root), str(staged_deb)])
        portable = work / "portable/Inklow"
        portable.parent.mkdir()
        copy_application(bundle, portable)
        staged_archive = work / archive.name
        def root_owner(info: tarfile.TarInfo) -> tarfile.TarInfo:
            info.uid = info.gid = 0
            info.uname = info.gname = "root"
            return info
        with tarfile.open(staged_archive, "w:gz", format=tarfile.PAX_FORMAT) as stream:
            stream.add(portable, arcname="Inklow", filter=root_owner)
        staged_deb.rename(deb)
        staged_archive.rename(archive)
    return deb, archive


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", type=Path, required=True)
    parser.add_argument("--output", type=Path, default=Path("release"))
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    try:
        artifacts = package(args.bundle, args.output, args.version)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        parser.exit(1, f"Linux packaging: {error}\n")
    for artifact in artifacts:
        print(artifact)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
