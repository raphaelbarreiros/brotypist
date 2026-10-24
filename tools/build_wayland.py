"""Build the KDE Wayland input-method library from installed system protocols.

Usage: python tools/build_wayland.py
Requires a C compiler, pkg-config, wayland-scanner, wayland-protocols,
libwayland-client and libxkbcommon development files. Never downloads or installs
packages. Generated protocol files live only in a temporary build directory.
"""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_OUTPUT = ROOT / "src" / "inklow" / "native" / "libinklow_wayland.so"


def run(command: list[str]) -> str:
    result = subprocess.run(command, check=False, capture_output=True, text=True)
    if result.returncode:
        detail = result.stderr.strip() or result.stdout.strip()
        raise RuntimeError(f"Command failed: {shlex.join(command)}\n{detail}")
    return result.stdout.strip()


def build(output: Path = DEFAULT_OUTPUT) -> Path:
    """Compile atomically; an unsuccessful build leaves the old library intact."""
    if not sys.platform.startswith("linux"):
        raise RuntimeError("The native Wayland input method must be built on Linux.")
    compiler = shlex.split(os.environ.get("CC", "cc"))
    if not compiler or not shutil.which(compiler[0]):
        raise RuntimeError("Missing C compiler; install a system C toolchain or set CC.")
    pkg_config = shutil.which("pkg-config")
    scanner = shutil.which("wayland-scanner")
    if not pkg_config or not scanner:
        raise RuntimeError("Missing pkg-config or wayland-scanner; install the system Wayland development tools.")
    packages = ["wayland-client", "xkbcommon", "wayland-protocols"]
    missing = [package for package in packages if subprocess.run(
        [pkg_config, "--exists", package], check=False, stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    ).returncode]
    if missing:
        raise RuntimeError("Missing pkg-config development metadata: " + ", ".join(missing)
                           + ". Install the corresponding system development packages.")
    protocol_root = Path(run([pkg_config, "--variable=pkgdatadir", "wayland-protocols"]))
    protocol = protocol_root / "unstable" / "input-method" / "input-method-unstable-v1.xml"
    if not protocol.is_file():
        raise RuntimeError(f"Installed wayland-protocols is missing its input-method-v1 XML: {protocol}")
    flags = shlex.split(run([pkg_config, "--cflags", "--libs", "wayland-client", "xkbcommon"]))
    output = Path(output).absolute()
    if output.is_symlink():
        raise RuntimeError(f"Refusing a symlink output library: {output}")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".inklow-wayland-", dir=output.parent) as temporary:
        directory = Path(temporary)
        header = directory / "input-method-client-protocol.h"
        source = directory / "input-method-protocol.c"
        library = directory / output.name
        run([scanner, "client-header", str(protocol), str(header)])
        run([scanner, "private-code", str(protocol), str(source)])
        command = [
            *compiler, "-std=c11", "-O2", "-fPIC", "-fvisibility=hidden",
            "-Wall", "-Wextra", "-Werror", "-Wl,-z,relro,-z,now", "-Wl,--no-undefined",
            *shlex.split(os.environ.get("CPPFLAGS", "")),
            *shlex.split(os.environ.get("CFLAGS", "")),
            "-shared", "-I", str(directory),
            str(ROOT / "src" / "inklow" / "native" / "wayland_client.c"), str(source),
            "-o", str(library), *flags,
            *shlex.split(os.environ.get("LDFLAGS", "")),
        ]
        print(shlex.join(command), flush=True)
        run(command)
        library.chmod(0o755)
        os.replace(library, output)
    return output


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT,
                        help="destination shared library (default: native/libinklow_wayland.so)")
    args = parser.parse_args()
    try:
        print(build(args.output))
    except (OSError, RuntimeError) as error:
        print(f"Wayland native build failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
