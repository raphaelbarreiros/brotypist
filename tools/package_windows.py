"""Package an already-built x86_64 onedir bundle as a per-user MSI and ZIP.

Requires Windows and the build-only WiX dotnet tool pinned to 5.0.2. No model,
settings, service, autostart entry or runtime installer engine is included.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import tomllib
import uuid
import xml.etree.ElementTree as ET
import zipfile

ROOT = Path(__file__).resolve().parents[1]
APP_ID = "com.inklow.Inklow"
WIX_VERSION = "5.0.2"
NAMESPACE = uuid.uuid5(uuid.NAMESPACE_URL, "https://inklow.com/installer/windows/x86_64")
UPGRADE_CODE = str(uuid.uuid5(NAMESPACE, "upgrade")).upper()
WXS_NAMESPACE = "http://wixtoolset.org/schemas/v4/wxs"
PRIVATE_SUFFIXES = {".gguf", ".ggml", ".safetensors", ".pt", ".pth", ".db", ".sqlite", ".sqlite3"}
PRIVATE_NAMES = {"settings.json", "config.json", "secrets.yaml", ".env", "history.enc"}


def sha256(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def stable_guid(identity: str) -> str:
    return str(uuid.uuid5(NAMESPACE, identity.casefold())).upper()


def identifier(prefix: str, relative: str) -> str:
    return prefix + hashlib.sha256(relative.casefold().encode("utf-8")).hexdigest()[:32]


def verify_pe_x64(path: Path) -> None:
    with path.open("rb") as stream:
        if stream.read(2) != b"MZ":
            raise ValueError(f"Not a Windows executable: {path}")
        stream.seek(0x3C)
        offset = stream.read(4)
        if len(offset) != 4:
            raise ValueError(f"Truncated executable: {path}")
        stream.seek(struct.unpack("<I", offset)[0])
        if stream.read(6) != b"PE\0\0\x64\x86":
            raise ValueError(f"Expected an x86_64 PE executable: {path}")


def inspect_bundle(bundle: Path, version: str) -> tuple[dict, list[dict]]:
    marker = json.loads((bundle / ".inklow-build.json").read_text(encoding="utf-8"))
    expected = {"application": APP_ID, "platform": "win32", "version": version, "architecture": "x86_64"}
    if not isinstance(marker, dict) or any(marker.get(key) != value for key, value in expected.items()):
        raise ValueError(f"Bundle metadata does not match {expected}")
    source_commit = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, check=True, text=True,
        capture_output=True, timeout=15,
    ).stdout.strip()
    if not re.fullmatch(r"[0-9a-f]{40,64}", source_commit) or marker.get("source_commit") != source_commit:
        raise ValueError("Bundle source_commit does not match this checkout's HEAD")
    project = tomllib.loads((ROOT / "pyproject.toml").read_text(encoding="utf-8"))
    if project["project"]["version"] != version:
        raise ValueError("Requested version differs from pyproject.toml")
    for name in ("Inklow.exe", "Inklow-cli.exe"):
        verify_pe_x64(bundle / name)
    if not (bundle / "_internal").is_dir():
        raise ValueError("Expected the entire PyInstaller onedir payload, including _internal")
    result = subprocess.run(
        [str(bundle / "Inklow-cli.exe"), "--version"], check=True, capture_output=True,
        text=True, encoding="utf-8", timeout=30,
    )
    if result.stdout.strip() != f"Inklow {version}":
        raise ValueError(f"Bundle executable version does not match {version}: {result.stdout!r}")
    files = []
    seen = set()
    for path in sorted(bundle.rglob("*"), key=lambda item: item.relative_to(bundle).as_posix().casefold()):
        relative = path.relative_to(bundle).as_posix()
        # MSI and ZIP must carry regular files, never links to outside the build.
        if path.is_symlink() or (hasattr(path, "is_junction") and path.is_junction()):
            raise ValueError(f"Refusing a linked payload path: {relative}")
        if not path.is_file():
            if not path.is_dir():
                raise ValueError(f"Refusing a non-regular payload path: {relative}")
            continue
        if path.suffix.casefold() in PRIVATE_SUFFIXES or path.name.casefold() in PRIVATE_NAMES:
            raise ValueError(f"Refusing model weights or user state in payload: {relative}")
        if relative.casefold() in seen:
            raise ValueError(f"Case-insensitive payload collision: {relative}")
        seen.add(relative.casefold())
        files.append({"path": relative, "size": path.stat().st_size, "sha256": sha256(path)})
    return marker, files


def write_payload_source(bundle: Path, files: list[dict], target: Path) -> None:
    """One stable file component per path, with HKCU keypaths for per-user MSI.

    Directory cleanup is empty-only. It never traverses or removes app settings,
    model caches, history, or files a user has added beside the application.
    """
    ET.register_namespace("", WXS_NAMESPACE)

    def element(parent: ET.Element, tag: str, **attributes: str) -> ET.Element:
        return ET.SubElement(parent, f"{{{WXS_NAMESPACE}}}{tag}", attributes)

    root = ET.Element(f"{{{WXS_NAMESPACE}}}Wix")
    fragment = element(root, "Fragment")
    install = element(fragment, "DirectoryRef", Id="INSTALLFOLDER")
    directories = {"": install}
    for item in files:
        parent = Path(item["path"]).parent.as_posix()
        if parent == ".":
            continue
        current = ""
        for part in parent.split("/"):
            previous, current = current, f"{current}/{part}".lstrip("/")
            if current not in directories:
                directories[current] = element(
                    directories[previous], "Directory", Id=identifier("dir_", current), Name=part,
                )
    group = element(fragment, "ComponentGroup", Id="InklowPayload")
    for relative in directories:
        directory_id = "INSTALLFOLDER" if not relative else identifier("dir_", relative)
        component = element(
            group, "Component", Id=identifier("cleanup_", relative), Directory=directory_id,
            Guid=stable_guid("directory/" + relative), Bitness="always64",
        )
        element(
            component, "RegistryValue", Root="HKCU", Key=r"Software\Inklow\Installer\Directories",
            Name=identifier("d_", relative), Value="1", Type="integer", KeyPath="yes",
        )
        element(component, "RemoveFolder", Id=identifier("remove_", relative), On="uninstall")
    for item in files:
        relative = item["path"]
        parent = Path(relative).parent.as_posix()
        directory_id = "INSTALLFOLDER" if parent == "." else identifier("dir_", parent)
        component = element(
            group, "Component", Id=identifier("cmp_", relative), Directory=directory_id,
            Guid=stable_guid("file/" + relative), Bitness="always64",
        )
        element(component, "File", Id=identifier("file_", relative), Source=str(bundle / relative), Name=Path(relative).name)
        element(
            component, "RegistryValue", Root="HKCU", Key=r"Software\Inklow\Installer\Files",
            Name=identifier("f_", relative), Value="1", Type="integer", KeyPath="yes",
        )
    ET.indent(root)
    ET.ElementTree(root).write(target, encoding="utf-8", xml_declaration=True)


def package(bundle: Path, output: Path, version: str, report: Path) -> dict:
    if sys.platform != "win32":
        raise RuntimeError("Build the Windows MSI on native Windows; this tool does not cross-build")
    if not re.fullmatch(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", version):
        raise ValueError("MSI releases require a numeric major.minor.patch version")
    major, minor, patch = map(int, version.split("."))
    if major > 255 or minor > 255 or patch > 65535:
        raise ValueError("Version exceeds Windows Installer's 255.255.65535 limits")
    if bundle.is_symlink() or (hasattr(bundle, "is_junction") and bundle.is_junction()):
        raise ValueError("Refusing a linked bundle directory")
    bundle = bundle.resolve(strict=True)
    output = output.resolve()
    report = report.resolve()
    if output == bundle or output.is_relative_to(bundle) or report.is_relative_to(bundle):
        raise ValueError("Packaging output and reports must be outside the application payload")
    metadata, files = inspect_bundle(bundle, version)
    wix = shutil.which("wix")
    if wix is None:
        raise RuntimeError(f"Install the build-only tool: dotnet tool install --global wix --version {WIX_VERSION}")
    tool_version = subprocess.run([wix, "--version"], check=True, capture_output=True, text=True, timeout=30).stdout.strip()
    if tool_version.split("+")[0] != WIX_VERSION:
        raise RuntimeError(f"Expected WiX {WIX_VERSION}, found {tool_version!r}")
    stem = f"Inklow-{version}-windows-x86_64"
    output.mkdir(parents=True, exist_ok=True)
    destinations = {extension: output / f"{stem}.{extension}" for extension in ("msi", "zip")}
    if any(path.exists() for path in destinations.values()):
        raise FileExistsError("Refusing to overwrite a release MSI or ZIP; choose an empty output directory")
    report.parent.mkdir(parents=True, exist_ok=True)
    product_code = stable_guid(f"product/{version}")
    with tempfile.TemporaryDirectory(prefix=".inklow-windows-package-", dir=report.parent) as temporary:
        staging = Path(temporary)
        product = staging / "Product.wxs"
        shutil.copyfile(ROOT / "packaging" / "windows" / "Product.wxs.in", product)
        payload = staging / "Payload.wxs"
        write_payload_source(bundle, files, payload)
        msi = staging / f"{stem}.msi"
        subprocess.run([
            wix, "build", str(product), str(payload), "-arch", "x64", "-o", str(msi),
            "-d", f"Version={version}", "-d", f"ProductCode={product_code}",
            "-d", f"UpgradeCode={UPGRADE_CODE}", "-d", f"SourceCommit={metadata['source_commit']}",
            "-d", f"RegistrationGuid={stable_guid('registration')}", "-d", f"BundlePath={bundle}",
        ], cwd=staging, check=True, timeout=600)
        portable = staging / f"{stem}.zip"
        with zipfile.ZipFile(portable, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=6) as archive:
            for item in files:
                archive.write(bundle / item["path"], f"Inklow/{item['path']}")
        # Fail if an input was changed while WiX/ZIP read it, before publishing.
        for item in files:
            if sha256(bundle / item["path"]) != item["sha256"]:
                raise RuntimeError(f"Payload changed during packaging: {item['path']}")
        for extension, artifact in (("msi", msi), ("zip", portable)):
            shutil.copyfile(artifact, destinations[extension])
    receipt = {
        "application": APP_ID, "version": version, "source_commit": metadata["source_commit"],
        "platform": "windows", "architecture": "x86_64", "wix_version": WIX_VERSION,
        "signing": "unsigned", "scope": "perUser", "product_code": product_code,
        "upgrade_code": UPGRADE_CODE, "install_directory": r"%LOCALAPPDATA%\Programs\Inklow",
        "start_menu_shortcut": r"%APPDATA%\Microsoft\Windows\Start Menu\Programs\Inklow.lnk",
        "artifacts": {key: {"path": str(path), "sha256": sha256(path)} for key, path in destinations.items()},
        "files": files,
    }
    report.write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    return receipt


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", type=Path, required=True, help="Already built dist/Inklow directory")
    parser.add_argument("--output", type=Path, default=ROOT / "release")
    parser.add_argument("--version", required=True)
    parser.add_argument("--report", type=Path, default=ROOT / "reports" / "windows-package.json")
    args = parser.parse_args()
    try:
        receipt = package(args.bundle, args.output, args.version, args.report)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        parser.exit(1, f"Windows packaging: {error}\n")
    print(json.dumps({"artifacts": receipt["artifacts"], "receipt": str(args.report)}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
