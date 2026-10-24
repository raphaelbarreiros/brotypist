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
import re
import signal
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


def owned_process(pid: int, launched_pid: int) -> bool:
    """Allow a frozen bootloader child, but never another app's titled window."""
    seen = set()
    while pid > 1 and pid not in seen:
        if pid == launched_pid:
            return True
        seen.add(pid)
        try:
            status = Path(f"/proc/{pid}/status").read_text(encoding="utf-8")
        except OSError:
            return False
        parent = re.search(r"^PPid:\s+(\d+)$", status, re.MULTILINE)
        if parent is None:
            return False
        pid = int(parent[1])
    return False


def mapped_settings(tree: str, launched_pid: int, environment: dict) -> dict | None:
    for match in re.finditer(r'^\s*(0x[0-9a-fA-F]+)\s', tree, re.MULTILINE):
        window = match[1]
        properties = command(["xprop", "-id", window, "_NET_WM_PID", "_NET_WM_WINDOW_TYPE", "_NET_WM_NAME"],
                             environment=environment, timeout=3)
        owner = re.search(r"_NET_WM_PID\(CARDINAL\)\s*=\s*(\d+)", properties)
        if owner is None or not owned_process(int(owner[1]), launched_pid):
            continue
        if not any(kind in properties for kind in ("_NET_WM_WINDOW_TYPE_NORMAL", "_NET_WM_WINDOW_TYPE_DIALOG")):
            continue
        info = command(["xwininfo", "-id", window], environment=environment, timeout=3)
        width = re.search(r"^\s*Width:\s*(\d+)$", info, re.MULTILINE)
        height = re.search(r"^\s*Height:\s*(\d+)$", info, re.MULTILINE)
        if ("Map State: IsViewable" in info and width and height
                and int(width[1]) > 0 and int(height[1]) > 0):
            title = re.search(r'_NET_WM_NAME\([^)]*\)\s*=\s*"(.*)"', properties)
            return {"id": window, "pid": int(owner[1]), "title": title[1] if title else None,
                    "map_state": "IsViewable", "width": int(width[1]), "height": int(height[1])}
    return None


def startup_diagnostics(process: subprocess.Popen, environment: dict, log: Path, error: Exception) -> Path:
    """Capture the failing native state before terminating the exact app tree."""
    def read_proc(path: Path, maximum: int = 16384) -> str:
        try:
            return path.read_text(encoding="utf-8", errors="replace")[:maximum]
        except OSError as failure:
            return f"unavailable: {failure}"

    def observe(arguments: list[str]) -> str:
        try:
            return command(arguments, environment=environment, timeout=3)[:262144]
        except (OSError, RuntimeError, subprocess.SubprocessError) as failure:
            return f"unavailable: {failure}"

    processes = []
    for directory in Path("/proc").iterdir():
        if directory.name.isdecimal() and owned_process(int(directory.name), process.pid):
            processes.append(int(directory.name))
    diagnostics = {"error": str(error), "launched_pid": process.pid, "returncode": process.poll(),
                   "x_tree": observe(["xwininfo", "-root", "-tree"]),
                   "process_threads": observe(["ps", "-L", "-p", ",".join(map(str, processes or [process.pid])),
                                                "-o", "pid,ppid,tid,stat,wchan:32,etime,comm,args"]),
                   "processes": {}}
    for pid in sorted(processes):
        root = Path(f"/proc/{pid}")
        try:
            executable = str((root / "exe").resolve(strict=True))
        except OSError as failure:
            executable = f"unavailable: {failure}"
        tasks = {}
        try:
            threads = sorted((root / "task").iterdir(), key=lambda item: int(item.name))[:64]
        except OSError:
            threads = []
        for thread in threads:
            tasks[thread.name] = {name: read_proc(thread / name)
                                  for name in ("status", "wchan", "syscall", "stack")}
        maps = read_proc(root / "maps", maximum=262144)
        diagnostics["processes"][str(pid)] = {
            "executable": executable, "threads": tasks,
            "native_libraries": [line for line in maps.splitlines()
                                 if any(name in line for name in ("libQt6", "libglib", "libgobject", "libatspi", "libdbus", "/_gi."))],
        }
    path = log.with_suffix(".diagnostics.json")
    path.write_text(json.dumps(diagnostics, indent=2) + "\n", encoding="utf-8")
    return path


def append_distro_bindings() -> None:
    # Do not export PYTHONPATH: distro packages must not shadow this CI Python's
    # installed Qt or source dependencies, or leak into the frozen-app exercise.
    bindings = "/usr/lib/python3/dist-packages"
    if bindings not in sys.path and Path(bindings).is_dir():
        sys.path.append(bindings)


def inspect_atspi_window(pid: int) -> dict:
    """Query only the launched app's settings metadata, never editable values."""
    append_distro_bindings()
    import pyatspi
    pyatspi.setTimeout(200, 500)
    desktop = pyatspi.Registry.getDesktop(0)
    desktop.clearCache()
    for index in range(min(desktop.childCount, 256)):
        application = desktop.getChildAtIndex(index)
        if application is None or int(application.get_process_id()) != pid:
            continue
        application.clearCache()
        for child in range(min(application.childCount, 256)):
            window = application.getChildAtIndex(child)
            if window is None:
                continue
            window.clearCache()
            if window.getRole() not in (pyatspi.ROLE_FRAME, pyatspi.ROLE_DIALOG, pyatspi.ROLE_WINDOW):
                continue
            state = window.getState()
            if not all(state.contains(flag) for flag in
                       (pyatspi.STATE_VISIBLE, pyatspi.STATE_SHOWING, pyatspi.STATE_ENABLED)):
                raise RuntimeError("Own-process settings AT-SPI window is not visible/showing/enabled")
            children = int(window.childCount)
            if children <= 0:
                raise RuntimeError("Own-process settings AT-SPI window has no accessible controls")
            native = ""
            sections = False
            stack = [window]
            remaining = 512
            while stack and remaining:
                element = stack.pop()
                remaining -= 1
                name = str(element.name or "")
                sections = sections or element.getRole() == pyatspi.ROLE_LIST
                if name.startswith("Linux AT-SPI/X11 ready."):
                    native = name
                if native and sections:
                    break
                for index in range(min(element.childCount, 128)):
                    descendant = element.getChildAtIndex(index)
                    if descendant is not None:
                        stack.append(descendant)
            if not native or not sections:
                raise RuntimeError("Own-process settings did not expose its ready Linux bridge and settings controls through AT-SPI")
            return {"pid": pid, "title": window.name, "role": window.getRoleName(),
                    "accessible_children": children, "visible_showing_enabled": True,
                    "settings_sections": sections, "native_bridge": native}
    raise RuntimeError(f"AT-SPI did not expose a usable settings window for PID {pid}")


def launch_settings(executable: Path, environment: dict, log: Path, *, offscreen: bool) -> dict:
    environment = dict(environment, QT_QPA_PLATFORM="offscreen" if offscreen else "xcb")
    with log.open("w", encoding="utf-8") as stream:
        process = subprocess.Popen([str(executable), "--settings", "--no-tray", "--paused", "--pane", "setup"],
                                   env=environment, stdout=stream, stderr=subprocess.STDOUT,
                                   start_new_session=True)
        started = time.monotonic()
        window = None
        try:
            while time.monotonic() - started < 15:
                if process.poll() is not None:
                    raise RuntimeError(f"Settings exited ({process.returncode}): {log.read_text(encoding='utf-8')}")
                if not offscreen:
                    tree = command(["xwininfo", "-root", "-tree"], environment=environment, timeout=3)
                    window = mapped_settings(tree, process.pid, environment)
                    if window is not None:
                        break
                elif time.monotonic() - started >= 4:
                    break
                time.sleep(0.2)
            if not offscreen and window is None:
                raise RuntimeError(f"Own-process settings window did not map under X11: {log.read_text(encoding='utf-8')}")
            accessibility = None
            if window is not None:
                accessibility = json.loads(command(
                    [sys.executable, str(Path(__file__).resolve()), "--inspect-atspi-window", str(window["pid"])],
                    environment=environment, timeout=5))
            if process.poll() is not None:
                raise RuntimeError(f"Settings exited during native observation ({process.returncode})")
            return {"pid": process.pid, "mode": environment["QT_QPA_PLATFORM"],
                    "window_mapped": window is not None, "window": window, "accessibility": accessibility,
                    "survived_seconds": round(time.monotonic() - started, 2),
                    "termination": "SIGTERM by smoke after observation, not an app quit test"}
        except Exception as error:
            diagnostics = startup_diagnostics(process, environment, log, error)
            raise RuntimeError(f"{error}\nNative startup diagnostics: {diagnostics}") from error
        finally:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
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
    offscreen = launch_settings(executable, isolated_environment(home / "offscreen"),
                                report.with_suffix(".offscreen.log"), offscreen=True)
    x11 = launch_settings(executable, isolated_environment(home / "x11"),
                          report.with_suffix(".x11.log"), offscreen=False)
    return {"version": observed_version, "doctor": doctor,
            "offscreen_settings": offscreen, "x11_settings": x11}


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
    parser.add_argument("--deb", type=Path)
    parser.add_argument("--archive", type=Path)
    parser.add_argument("--version")
    parser.add_argument("--inspect-atspi-window", type=int, help=argparse.SUPPRESS)
    parser.add_argument("--report", type=Path, default=Path("reports/linux-installer-smoke.json"))
    args = parser.parse_args()
    try:
        if args.inspect_atspi_window is not None:
            print(json.dumps(inspect_atspi_window(args.inspect_atspi_window)))
            return 0
        if not all((args.deb, args.archive, args.version)):
            parser.error("--deb, --archive and --version are required for installer smoke")
        result = smoke(args.deb.resolve(strict=True), args.archive.resolve(strict=True), args.version, args.report.absolute())
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError, tarfile.TarError) as error:
        parser.exit(1, f"Linux installer smoke: {error}\n")
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
