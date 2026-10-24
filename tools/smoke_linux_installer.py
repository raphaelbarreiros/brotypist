"""Install/remove the real .deb and exercise its frozen app plus portable archive.

Run as the desktop/CI user inside dbus-run-session + xvfb-run; sudo is used only
for apt/dpkg. Uses disposable external user state. No model download, inference,
interactive typing proof, real home mutation, or accessibility grants.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import time

APP_ID = "com.inklow.Inklow"


def command(arguments: list[str], *, environment: dict | None = None, timeout: int = 180) -> str:
    result = subprocess.run(arguments, env=environment, text=True, capture_output=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f"Command failed ({result.returncode}): {arguments}\n{result.stdout}\n{result.stderr}")
    return result.stdout.strip()


def isolated_environment(home: Path) -> dict:
    home.mkdir(parents=True, exist_ok=True)
    environment = dict(os.environ)
    for name in ("PYTHONHOME", "PYTHONPATH", "GI_TYPELIB_PATH", "LD_LIBRARY_PATH", "WAYLAND_DISPLAY", "QT_PLUGIN_PATH", "QT_QPA_PLATFORM_PLUGIN_PATH"):
        environment.pop(name, None)
    environment.update(HOME=str(home), INKLOW_HOME=str(home / ".config/inklow"),
                       XDG_CONFIG_HOME=str(home / ".config"), XDG_CACHE_HOME=str(home / ".cache"),
                       XDG_DATA_HOME=str(home / ".local/share"), XDG_SESSION_TYPE="x11",
                       QT_LINUX_ACCESSIBILITY_ALWAYS_ON="1", NO_AT_BRIDGE="0")
    return environment


def launch_settings(executable: Path, environment: dict, log: Path, *, offscreen: bool) -> dict:
    environment = dict(environment, QT_QPA_PLATFORM="offscreen" if offscreen else "xcb")
    with log.open("w", encoding="utf-8") as stream:
        process = subprocess.Popen([str(executable), "--settings", "--no-tray", "--paused"],
                                   env=environment, stdout=stream, stderr=subprocess.STDOUT)
        started = time.monotonic()
        mapped = False
        try:
            while time.monotonic() - started < 15:
                if process.poll() is not None:
                    raise RuntimeError(f"Settings exited ({process.returncode}): {log.read_text(encoding='utf-8')}")
                if not offscreen:
                    tree = command(["xwininfo", "-root", "-tree"], environment=environment)
                    if '"Inklow Settings"' in tree:
                        mapped = True
                        break
                elif time.monotonic() - started >= 4:
                    break
                time.sleep(0.2)
            if not offscreen and not mapped:
                raise RuntimeError(f"Installed settings window did not map under X11: {log.read_text(encoding='utf-8')}")
            return {"pid": process.pid, "mode": environment["QT_QPA_PLATFORM"],
                    "window_mapped": mapped, "survived_seconds": round(time.monotonic() - started, 2),
                    "termination": "SIGTERM by smoke after observation, not an app quit test"}
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=10)


def exercise(executable: Path, home: Path, report: Path, version: str) -> dict:
    environment = isolated_environment(home)
    observed_version = command([str(executable), "--version"], environment=environment)
    if observed_version != f"Inklow {version}":
        raise RuntimeError(f"Unexpected installed version: {observed_version}")
    doctor = json.loads(command([str(executable), "doctor"], environment=environment))
    if not doctor.get("native", "").startswith("Linux AT-SPI/X11 ready."):
        raise RuntimeError(f"Frozen native adapter did not initialize: {doctor}")
    if doctor.get("version") != version:
        raise RuntimeError("Doctor metadata does not match the release version")
    return {"version": observed_version, "doctor": doctor,
            "offscreen_settings": launch_settings(executable, isolated_environment(home / "offscreen"),
                                                    report.with_suffix(".offscreen.log"), offscreen=True),
            "x11_settings": launch_settings(executable, isolated_environment(home / "x11"),
                                             report.with_suffix(".x11.log"), offscreen=False)}


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def smoke(deb: Path, archive: Path, version: str, report: Path) -> dict:
    if sys.platform != "linux" or os.geteuid() == 0:
        raise RuntimeError("Run on native Linux as an unprivileged CI/desktop user with passwordless sudo")
    if not os.environ.get("DISPLAY") or not os.environ.get("DBUS_SESSION_BUS_ADDRESS"):
        raise RuntimeError("Run inside dbus-run-session -- xvfb-run -a (X11 and a session bus are required)")
    command(["sudo", "-n", "true"])
    installed = subprocess.run(["dpkg-query", "-W", "-f=${db:Status-Status}", "inklow"],
                               text=True, capture_output=True)
    if installed.returncode == 0 and installed.stdout.strip() == "installed":
        raise RuntimeError("Refusing to replace an existing Inklow installation; use a disposable CI runner")
    if Path("/opt/inklow").exists() or Path("/usr/bin/inklow").exists():
        raise RuntimeError("Refusing to overwrite an existing /opt/inklow or /usr/bin/inklow")
    metadata = command(["dpkg-deb", "-f", str(deb), "Package", "Version", "Architecture"])
    expected = {"Package": "inklow", "Version": version, "Architecture": "amd64"}
    observed = dict(line.split(": ", 1) for line in metadata.splitlines())
    if observed != expected:
        raise RuntimeError(f"Unexpected Debian package metadata: {observed}")
    report.parent.mkdir(parents=True, exist_ok=True)
    receipt = {"package": str(deb), "portable_archive": str(archive), "metadata": observed,
               "interactive_keyboard_compatibility": "NOT TESTED: package smoke is not native typing proof"}
    with tempfile.TemporaryDirectory(prefix="inklow-installer-smoke-") as temporary:
        state = Path(temporary)
        home = state / "home"
        sentinels = {}
        for relative, content in {
            ".config/inklow/settings.json": '{"global_enabled":false,"record_history":false}\n',
            ".cache/inklow/models/preserve.gguf": "synthetic external model sentinel, not model weights\n",
            ".local/share/inklow/history.enc": "synthetic external encrypted-history sentinel\n",
        }.items():
            path = home / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8")
            sentinels[path] = digest(path)
        attempted_install = False
        try:
            attempted_install = True
            command(["sudo", "-n", "env", "DEBIAN_FRONTEND=noninteractive", "apt-get", "install", "-y", str(deb)], timeout=600)
            for path, checksum in sentinels.items():
                if digest(path) != checksum:
                    raise RuntimeError(f"Installation modified external user state: {path}")
            desktop = Path(f"/usr/share/applications/{APP_ID}.desktop").read_text(encoding="utf-8")
            if "Exec=/usr/bin/inklow\n" not in desktop or f"Icon={APP_ID}\n" not in desktop:
                raise RuntimeError("Installed desktop entry does not target installed integration")
            command(["desktop-file-validate", f"/usr/share/applications/{APP_ID}.desktop"])
            if not Path(f"/usr/share/icons/hicolor/256x256/apps/{APP_ID}.png").is_file():
                raise RuntimeError("Installed application icon is missing")
            receipt["installed"] = exercise(Path("/usr/bin/inklow"), home, report.with_name("linux-installed"), version)
            # Extract away from build/install destinations; the complete onedir
            # must remain runnable after relocation, without system Python paths.
            relocated = state / "relocated portable app"
            relocated.mkdir()
            with tarfile.open(archive, "r:gz") as stream:
                stream.extractall(relocated, filter="data")
            receipt["portable"] = exercise(relocated / "Inklow/launch-inklow", state / "portable-home",
                                            report.with_name("linux-portable"), version)
        finally:
            if attempted_install:
                command(["sudo", "-n", "dpkg", "--purge", "inklow"])
        if Path("/opt/inklow").exists() or Path("/usr/bin/inklow").exists():
            raise RuntimeError("Uninstall left application paths behind")
        for relative in (f"/usr/share/applications/{APP_ID}.desktop", f"/usr/share/icons/hicolor/256x256/apps/{APP_ID}.png"):
            if Path(relative).exists():
                raise RuntimeError(f"Uninstall left owned desktop integration: {relative}")
        for path, checksum in sentinels.items():
            if digest(path) != checksum:
                raise RuntimeError(f"Uninstall modified external user state: {path}")
        receipt["external_state_preserved_after_install_and_purge"] = [str(path.relative_to(home)) for path in sentinels]
        receipt["uninstall"] = "dpkg --purge completed; app, launcher and desktop/icon integration removed"
    receipt["result"] = "passed"
    report.write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    return receipt


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--deb", type=Path, required=True)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--report", type=Path, default=Path("reports/linux-installer-smoke.json"))
    args = parser.parse_args()
    try:
        result = smoke(args.deb.resolve(strict=True), args.archive.resolve(strict=True), args.version, args.report.absolute())
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError, tarfile.TarError) as error:
        parser.exit(1, f"Linux installer smoke: {error}\n")
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
