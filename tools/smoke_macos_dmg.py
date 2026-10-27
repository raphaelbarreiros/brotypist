"""Mount, install into a disposable directory, launch and remove a macOS DMG.

GUI observation uses only the launched PID's window geometry, never screenshots,
Accessibility, Apple Events or screen-recording grants. --require-gui makes a
missing logged-in GUI session/PyObjC an error rather than an explicit skip.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time

from package_macos import inspect_bundle, run


def gui_smoke(executable: Path, environment: dict[str, str], root: Path, timeout: float) -> dict:
    # NSRunningApplication.terminate is a normal application quit request. It
    # neither targets another BroTypist instance nor requires automation permission.
    import AppKit
    import Quartz

    process = None
    application = None
    quit_requested = False
    visible = False
    log_path = root / "gui.log"
    with log_path.open("wb") as log:
        try:
            process = subprocess.Popen([str(executable), "--paused", "--settings", "--no-tray", "--pane", "general"],
                                       env=environment, stdout=log, stderr=log)
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise RuntimeError(f"GUI exited before window observation (code {process.returncode})")
                application = AppKit.NSRunningApplication.runningApplicationWithProcessIdentifier_(process.pid)
                windows = Quartz.CGWindowListCopyWindowInfo(Quartz.kCGWindowListOptionOnScreenOnly,
                                                           Quartz.kCGNullWindowID) or []
                for window in windows:
                    bounds = window.get("kCGWindowBounds", {})
                    if (window.get("kCGWindowOwnerPID") == process.pid
                            and window.get("kCGWindowLayer") == 0
                            and bounds.get("Width", 0) >= 600 and bounds.get("Height", 0) >= 400):
                        visible = True
                        break
                if visible:
                    break
                time.sleep(0.1)
            if not visible:
                raise RuntimeError("No visible settings-sized window observed for the isolated application PID")
        finally:
            if process is not None and process.poll() is None:
                application = AppKit.NSRunningApplication.runningApplicationWithProcessIdentifier_(process.pid)
                if application is not None:
                    quit_requested = bool(application.terminate())
                try:
                    process.wait(timeout=timeout)
                except subprocess.TimeoutExpired as error:
                    # Preserve the isolated app/profile for a human to quit. No
                    # terminate()/kill() on Popen: those are not normal GUI quit.
                    raise RuntimeError(f"Normal application quit timed out; isolated PID {process.pid} and files preserved at {root}") from error
    if process is None or not quit_requested or process.returncode != 0:
        raise RuntimeError("Isolated GUI did not complete a successful normal quit")
    return {"status": "passed", "visible_own_pid_window": visible,
            "arguments": ["--paused", "--settings", "--no-tray", "--pane", "general"],
            "normal_quit": True, "exit_code": process.returncode,
            "typing_in_other_apps": "not exercised", "screenshots": "not captured"}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dmg", required=True, type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--receipt", type=Path, default=Path("reports/macos-dmg-smoke.json"))
    parser.add_argument("--require-gui", action="store_true")
    parser.add_argument("--timeout", type=float, default=30)
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("Native macOS is required")
    if not 1 <= args.timeout <= 120:
        parser.error("--timeout must be between 1 and 120 seconds")
    if os.path.lexists(args.receipt):
        raise FileExistsError(f"Refusing to overwrite receipt: {args.receipt}")
    dmg = args.dmg.resolve(strict=True)
    root = Path(tempfile.mkdtemp(prefix="inklow-dmg-smoke-"))
    mount = root / "mount"
    mount.mkdir()
    receipt = {"dmg": str(dmg), "version": args.version, "checks": {},
               "normal_user_profile": "not accessed", "notarization": "not assessed"}
    checks = receipt["checks"]
    mounted = False
    preserve = False
    failure = None
    try:
        # hdiutil's plist output is deliberately not a receipt: it can contain
        # environment paths unrelated to installer acceptance.
        run("/usr/bin/hdiutil", "attach", "-readonly", "-nobrowse", "-noautoopen",
            "-mountpoint", str(mount), str(dmg))
        mounted = True
        checks["read_only_mount"] = "passed"
        app = mount / "BroTypist.app"
        link = mount / "Applications"
        if not link.is_symlink() or os.readlink(link) != "/Applications":
            raise ValueError("DMG must contain an Applications -> /Applications symlink")
        checks["drag_install_layout"] = "passed"
        info = inspect_bundle(app, args.version)
        checks["mounted_bundle"] = info
        installed = root / "installed/BroTypist.app"
        installed.parent.mkdir()
        run("/usr/bin/ditto", str(app), str(installed))
        checks["copied_bundle"] = inspect_bundle(installed, args.version)
        profile = root / "profile"
        profile.mkdir()
        settings = {"setup_completed": True, "global_enabled": False, "launch_at_login": False,
                    "clipboard_context": False, "screen_context": False, "record_history": False,
                    "statistics_enabled": False, "show_accessory": False, "model_path": ""}
        settings_path = profile / "settings.json"
        settings_path.write_text(json.dumps(settings), encoding="utf-8")
        before = settings_path.read_bytes()
        sentinel = profile / "user-data-preservation.txt"
        sentinel.write_text("preserve on uninstall\n", encoding="utf-8")
        environment = {**os.environ, "INKLOW_HOME": str(profile), "PYTHONDONTWRITEBYTECODE": "1"}
        executable = installed / "Contents/MacOS" / info["executable"]
        result = subprocess.run([str(executable), "--version"], env=environment,
                                capture_output=True, text=True, check=True, timeout=args.timeout)
        if result.stdout.strip() != f"BroTypist {args.version}":
            raise ValueError("Copied executable --version did not match release metadata")
        checks["copied_executable_version"] = result.stdout.strip()
        gui_available = os.stat("/dev/console").st_uid == os.getuid() and os.getuid() != 0
        gui_reason = "Requires an unlocked current-user Aqua console session and PyObjC AppKit/Quartz"
        try:
            import AppKit
            import Quartz
            foreground = AppKit.NSWorkspace.sharedWorkspace().frontmostApplication()
            session = Quartz.CGSessionCopyCurrentDictionary() or {}
            locked = (foreground is None or str(foreground.bundleIdentifier()) == "com.apple.loginwindow"
                      or bool(session.get("CGSSessionScreenIsLocked", False)))
            if locked:
                gui_available = False
                gui_reason = "Current-user Aqua desktop is locked; no GUI or typing pass claimed"
        except ImportError:
            gui_available = False
        if gui_available:
            try:
                checks["gui"] = gui_smoke(executable, environment, root, args.timeout)
            except Exception:
                # Never remove a possibly still-running executable or profile.
                preserve = True
                raise
        else:
            checks["gui"] = {"status": "skipped", "reason": gui_reason}
            if args.require_gui:
                raise RuntimeError(checks["gui"]["reason"])
        if settings_path.read_bytes() != before:
            raise RuntimeError("Untouched isolated settings changed during smoke")
        checks["untouched_settings"] = "passed"
        shutil.rmtree(installed)
        if settings_path.read_bytes() != before or sentinel.read_text(encoding="utf-8") != "preserve on uninstall\n":
            raise RuntimeError("Removing installed bundle did not preserve isolated user data")
        checks["uninstall_preserves_user_data"] = "passed"
        receipt["status"] = "passed"
    except Exception as error:
        receipt["status"] = "failed"
        receipt["error"] = str(error)
        failure = error
    finally:
        # Also detach if attach mounted successfully but failed afterwards.
        if mounted or os.path.ismount(mount):
            try:
                run("/usr/bin/hdiutil", "detach", str(mount))
                checks["detach"] = "passed"
            except Exception as error:
                checks["detach"] = "failed"
                receipt["status"] = "failed"
                receipt["detach_error"] = str(error)
                failure = failure or error
                preserve = True
        if preserve:
            receipt["preserved_disposable_directory"] = str(root)
        else:
            shutil.rmtree(root)
        args.receipt.parent.mkdir(parents=True, exist_ok=True)
        with args.receipt.open("x", encoding="utf-8") as stream:
            json.dump(receipt, stream, indent=2)
            stream.write("\n")
    print(json.dumps(receipt, indent=2))
    return 1 if failure else 0


if __name__ == "__main__":
    raise SystemExit(main())
