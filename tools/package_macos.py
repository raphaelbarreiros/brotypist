"""Package an already-built, signed BroTypist.app using Apple's native tools."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import plistlib
import re
import subprocess
import sys
import tempfile

APP_ID = "com.inklow.Inklow"


def run(*args: str) -> str:
    return subprocess.run(args, check=True, text=True, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, timeout=300).stdout.strip()


def inspect_bundle(bundle: Path, version: str) -> dict:
    if bundle.name != "BroTypist.app" or bundle.is_symlink() or not bundle.is_dir():
        raise ValueError("Expected a real BroTypist.app directory")
    with (bundle / "Contents/Info.plist").open("rb") as stream:
        metadata = plistlib.load(stream)
    if metadata.get("CFBundleIdentifier") != APP_ID:
        raise ValueError("Unexpected application identifier")
    if metadata.get("CFBundleShortVersionString") != version:
        raise ValueError("Requested version differs from CFBundleShortVersionString")
    executable_name = metadata.get("CFBundleExecutable")
    if not isinstance(executable_name, str) or Path(executable_name).name != executable_name:
        raise ValueError("Invalid CFBundleExecutable")
    executable = bundle / "Contents/MacOS" / executable_name
    marker = bundle / "Contents/Resources/.inklow-build.json"
    identity = json.loads(marker.read_text(encoding="utf-8"))
    if identity.get("application") != APP_ID or identity.get("platform") != "darwin":
        raise ValueError("Missing native BroTypist build ownership")
    source_commit = run("git", "-C", str(Path(__file__).resolve().parents[1]), "rev-parse", "HEAD")
    if identity.get("version") != version or identity.get("source_commit") != source_commit:
        raise ValueError("Bundle version/source provenance differs from the packaging checkout")
    forbidden = {".gguf", ".ggml", ".safetensors", ".pt", ".pth", ".db", ".sqlite", ".sqlite3"}
    private_names = {"settings.json", "config.json", "secrets.yaml", ".env", "history.enc"}
    for item in bundle.rglob("*"):
        if item.is_symlink() and not item.resolve().is_relative_to(bundle.resolve()):
            raise ValueError(f"Bundle symlink escapes payload: {item}")
        if item.is_file() and (item.suffix.lower() in forbidden or item.name.lower() in private_names):
            raise ValueError(f"Private/model payload refused: {item}")
    architectures = set(run("/usr/bin/lipo", "-archs", str(executable)).split())
    labels = {frozenset({"arm64"}): "arm64", frozenset({"x86_64"}): "x86_64",
              frozenset({"arm64", "x86_64"}): "universal2"}
    arch = labels.get(frozenset(architectures))
    if arch is None:
        raise ValueError(f"Unsupported executable architectures: {sorted(architectures)}")
    if identity.get("architecture") != arch:
        raise ValueError("Executable architecture differs from its build marker")
    run("/usr/bin/codesign", "--verify", "--deep", "--strict", str(bundle))
    details = subprocess.run(["/usr/bin/codesign", "--display", "--verbose=4", str(bundle)],
                             check=True, capture_output=True, text=True, timeout=30)
    signature = details.stdout + details.stderr
    return {"version": version, "architecture": arch, "source_commit": source_commit,
            "executable": executable_name,
            "signature": "ad-hoc" if "Signature=adhoc" in signature else "identity-signed",
            "notarization": "not checked; no notarization claim"}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("Packaging requires macOS with hdiutil, ditto, lipo and codesign")
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.-]+)?", args.version):
        parser.error("Invalid release version")
    bundle = args.bundle.absolute()
    info = inspect_bundle(bundle, args.version)
    output = args.output.absolute()
    if output.is_relative_to(bundle):
        parser.error("Output must be outside the application bundle")
    output.mkdir(parents=True, exist_ok=True)
    stem = f"BroTypist-{args.version}-macos-{info['architecture']}"
    destinations = [output / f"{stem}.dmg", output / f"{stem}.zip"]
    for destination in destinations:
        if os.path.lexists(destination):
            raise FileExistsError(f"Refusing to overwrite: {destination}")
    # Build both artifacts privately. Exclusive creation also protects against a
    # competing invocation publishing into the same output directory.
    with tempfile.TemporaryDirectory(prefix=".inklow-package-", dir=output) as temporary:
        root = Path(temporary)
        stage = root / "volume"
        stage.mkdir()
        run("/usr/bin/ditto", str(bundle), str(stage / "BroTypist.app"))
        (stage / "Applications").symlink_to("/Applications", target_is_directory=True)
        dmg, archive = root / "app.dmg", root / "app.zip"
        run("/usr/bin/hdiutil", "create", "-volname", "BroTypist", "-srcfolder", str(stage),
            "-format", "UDZO", "-fs", "HFS+", str(dmg))
        run("/usr/bin/ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", str(bundle), str(archive))
        for source, destination in zip((dmg, archive), destinations):
            os.link(source, destination)
    print(json.dumps({**info, "artifacts": [str(path) for path in destinations]}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
