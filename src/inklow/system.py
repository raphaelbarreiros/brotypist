"""Explicit OS actions and opt-in ephemeral context, independent of the UI toolkit."""
from __future__ import annotations

import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time

_AUTOSTART_ID = "com.inklow.Inklow"
_INPUT_METHOD_ID = _AUTOSTART_ID + ".InputMethod"
_battery_lock = threading.Lock()
_battery_cache = None


def _launch_arguments() -> list[str]:
    """Build argv for this interpreter or frozen executable, not a shell command."""
    if getattr(sys, "frozen", False):
        return [sys.executable, "--background"]
    return [sys.executable, "-m", "inklow", "--background"]


def _desktop_command(arguments: list[str]) -> str:
    """Encode desktop Exec arguments, including literal percent field codes."""
    def quote(argument: str) -> str:
        if "\n" in argument or "\r" in argument:
            raise ValueError("Desktop launcher arguments cannot contain line breaks.")
        return '"' + argument.replace("\\", "\\\\").replace('"', '\\"').replace("`", "\\`").replace("$", "\\$").replace("%", "%%") + '"'
    return " ".join(quote(argument) for argument in arguments)


def _autostart_path() -> Path:
    if sys.platform == "darwin":
        return Path.home() / "Library/LaunchAgents" / (_AUTOSTART_ID + ".plist")
    return Path(os.environ.get("XDG_CONFIG_HOME", str(Path.home() / ".config"))) / "autostart" / (_AUTOSTART_ID + ".desktop")


def _atomic_write(path: Path, content: bytes) -> None:
    """Replace one integration file via a private sibling on the same filesystem."""
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    descriptor, name = tempfile.mkstemp(prefix=".inklow-", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(content)
        os.replace(name, path)
    finally:
        if os.path.exists(name):
            os.unlink(name)


def launch_at_login_state() -> bool:
    """Read login-entry state; isolated profiles always report disabled."""
    if os.environ.get("INKLOW_HOME"):
        return False
    if sys.platform == "win32":
        import winreg
        try:
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Run") as key:
                value, _ = winreg.QueryValueEx(key, "Inklow")
                return bool(value)
        except FileNotFoundError:
            return False
    path = _autostart_path()
    if not path.is_file():
        return False
    if sys.platform == "darwin":
        try:
            data = plistlib.loads(path.read_bytes())
            return data.get("Label") == _AUTOSTART_ID and bool(data.get("RunAtLoad"))
        except (ValueError, plistlib.InvalidFileException):
            return False
    return "X-Inklow-Owned=true" in path.read_text(encoding="utf-8")


def set_launch_at_login(enabled: bool) -> bool:
    """Change only this user's explicit BroTypist login entry; never start a copy."""
    if type(enabled) is not bool:
        raise ValueError("Launch at login must be on or off.")
    if os.environ.get("INKLOW_HOME"):
        raise RuntimeError("Login items cannot be changed from an isolated test profile. Open the normal installed BroTypist app.")
    if sys.platform == "win32":
        import winreg
        with winreg.CreateKey(winreg.HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Run") as key:
            if enabled:
                winreg.SetValueEx(key, "Inklow", 0, winreg.REG_SZ, subprocess.list2cmdline(_launch_arguments()))
            else:
                try:
                    winreg.DeleteValue(key, "Inklow")
                except FileNotFoundError:
                    pass
        return True
    path = _autostart_path()
    if path.exists() and not launch_at_login_state():
        raise RuntimeError(f"A different or invalid startup entry already exists at {path}. It was not overwritten.")
    if not enabled:
        path.unlink(missing_ok=True)
        return True
    arguments = [*_launch_arguments(), *(["--login-startup"] if sys.platform.startswith("linux") else [])]
    if sys.platform == "darwin":
        content = plistlib.dumps({
            "Label": _AUTOSTART_ID, "ProgramArguments": arguments,
            "RunAtLoad": True, "KeepAlive": False,
            "ProcessType": "Interactive", "LimitLoadToSessionType": "Aqua",
        })
    else:
        command = _desktop_command(arguments)
        content = ("[Desktop Entry]\nType=Application\nName=BroTypist\n"
                   f"Exec={command}\nTerminal=false\nX-Inklow-Owned=true\n").encode("utf-8")
    _atomic_write(path, content)
    return True


def install_wayland_input_method() -> Path:
    """Install only this user's launcher; never select it or alter KWin settings."""
    if not sys.platform.startswith("linux"):
        raise RuntimeError("KDE Wayland input-method setup is available on Linux only.")
    if os.environ.get("INKLOW_HOME"):
        raise RuntimeError("Desktop integration cannot be installed from an isolated profile.")
    native = Path(__file__).parent / "native" / "libinklow_wayland.so"
    if not native.is_file():
        raise RuntimeError("The native Wayland client is missing. Build it with python tools/build_wayland.py, or install a package containing it.")
    path = Path(os.environ.get("XDG_DATA_HOME", str(Path.home() / ".local/share"))) / "applications" / (_INPUT_METHOD_ID + ".desktop")
    if path.is_symlink() or (path.exists() and "X-Inklow-Owned=true" not in path.read_text(encoding="utf-8")):
        raise RuntimeError(f"A different input-method entry already exists at {path}; it was not overwritten.")
    command = _desktop_command([*_launch_arguments(), "--wayland-input-method"])
    icon = str(Path(__file__).parent / "assets/icons/app-256.png").replace("\\", "\\\\").replace("\n", "\\n").replace("\r", "\\r")
    content = ("[Desktop Entry]\nType=Application\nName=BroTypist\n"
               "GenericName=Predictive typing input method\n"
               "Comment=Manual controls for supported KDE Wayland text inputs; ordinary inline completion unavailable\n"
               f"Exec={command}\nIcon={icon}\nTerminal=false\nNoDisplay=true\n"
               "X-KDE-Wayland-VirtualKeyboard=true\nX-Inklow-Owned=true\n")
    _atomic_write(path, content.encode("utf-8"))
    return path


def wayland_input_method_selected() -> bool:
    """Defer only login startup to KWin; never consult it for an isolated profile."""
    if os.environ.get("INKLOW_HOME") or not sys.platform.startswith("linux") or not (
            os.environ.get("WAYLAND_DISPLAY") or os.environ.get("XDG_SESSION_TYPE", "").lower() == "wayland"):
        return False
    reader = shutil.which("kreadconfig6")
    if not reader:
        return False
    try:
        selected = subprocess.check_output(
            [reader, "--file", "kwinrc", "--group", "Wayland", "--key", "InputMethod"],
            text=True, stderr=subprocess.DEVNULL, timeout=3).strip()
    except (OSError, subprocess.SubprocessError):
        return False
    if Path(selected).name != _INPUT_METHOD_ID + ".desktop":
        return False
    if Path(selected).is_absolute():
        return Path(selected).is_file()
    roots = [os.environ.get("XDG_DATA_HOME", str(Path.home() / ".local/share"))]
    roots.extend(os.environ.get("XDG_DATA_DIRS", "/usr/local/share:/usr/share").split(":"))
    return any((Path(root) / "applications" / selected).is_file() for root in roots if root)


def open_wayland_settings() -> None:
    """Open the user's native selector; permission and method selection stay manual."""
    if not sys.platform.startswith("linux"):
        raise RuntimeError("KDE Wayland settings are available on Linux only.")
    command = shutil.which("kcmshell6")
    if not command:
        raise RuntimeError("Open KDE System Settings → Keyboard → Virtual Keyboard and choose BroTypist.")
    subprocess.Popen([command, "kcm_virtualkeyboard"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def _open(target: str) -> None:
    """Ask the OS to open a target; Linux dispatch does not wait for completion."""
    if sys.platform == "darwin":
        subprocess.run(["/usr/bin/open", target], check=True, timeout=15)
    elif sys.platform == "win32":
        os.startfile(target)
    else:
        subprocess.Popen(["xdg-open", target], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def open_permissions(kind: str = "accessibility") -> None:
    """Open a native permissions page without granting access on the user's behalf."""
    if sys.platform == "darwin":
        targets = {
            "accessibility": "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
            "input_monitoring": "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent",
            "screen_recording": "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
            "keyboard": "x-apple.systempreferences:com.apple.preference.keyboard",
        }
        if kind not in targets:
            raise ValueError("Unknown permission settings page.")
        _open(targets[kind])
    elif sys.platform == "win32":
        _open("ms-settings:privacy")
    else:
        raise RuntimeError("Enable desktop accessibility. X11 uses AT-SPI keyboard access; KDE Wayland requires explicitly selecting BroTypist as the native input method in Privacy or Welcome. OS access is never granted automatically.")


def reveal_models() -> None:
    """Create and reveal the configured model storage directory."""
    from .models import model_path, get_preset
    folder = model_path(get_preset("balanced")).parent
    folder.mkdir(parents=True, exist_ok=True)
    _open(str(folder))


def clipboard_text(*, authorized) -> str:
    """Return at most 4,000 characters while live consent remains granted.

    ``authorized`` is rechecked around the native read so revocation discards
    the result. Qt callers must marshal onto the GUI thread or get RuntimeError.
    """
    if not authorized():
        return ""
    if sys.platform == "darwin":
        from AppKit import NSPasteboard, NSPasteboardTypeString
        if not authorized():
            return ""
        value = NSPasteboard.generalPasteboard().stringForType_(NSPasteboardTypeString)
        return str(value or "")[:4000] if authorized() else ""
    from PySide6.QtCore import QThread
    from PySide6.QtWidgets import QApplication
    app = QApplication.instance()
    if app is None:
        raise RuntimeError("Clipboard context requires the running menu application.")
    if QThread.currentThread() != app.thread():
        raise RuntimeError("Clipboard context requires the application thread.")
    if not authorized():
        return ""
    value = app.clipboard().text()
    return value[:4000] if authorized() else ""


def _unknown_power_status() -> dict:
    return {"available": False, "has_battery": False, "on_battery": False,
            "charging": None, "percent": None}


def _mac_power_status(output: str) -> dict:
    """Parse pmset output, retaining unknown fields for incomplete battery reports."""
    reading = _unknown_power_status()
    source = re.search(r"^Now drawing from '(Battery Power|AC Power)'\s*$", output, re.MULTILINE)
    if source is None:
        return reading
    reading["on_battery"] = source[1] == "Battery Power"
    batteries = [line for line in output.splitlines()
                 if re.match(r"\s*-InternalBattery-\d+\b", line)]
    reading["has_battery"] = bool(batteries) or reading["on_battery"]
    if not batteries:
        # A battery-powered report without its battery row is incomplete, not a
        # desktop Mac and not a valid low-battery sample.
        remainder = (output[:source.start()] + output[source.end():]).strip()
        reading["available"] = not reading["on_battery"] and remainder in {"", "No batteries."}
        return reading
    if len(batteries) != 1:
        # Do not invent an average for an unfamiliar multi-pack report.
        return reading
    record = re.match(r"\s*-InternalBattery-\d+\s+\(id=\d+\)\s+(.+)$", batteries[0])
    if record is None:
        return reading
    if re.search(r"\bpresent:\s*false\b", record[1]):
        reading["has_battery"] = False
        reading["available"] = not reading["on_battery"]
        return reading
    fields = [field.strip() for field in record[1].split(";")]
    if len(fields) < 2:
        return reading
    percentage = re.fullmatch(r"(\d{1,3})%", fields[0])
    if percentage and 0 <= int(percentage[1]) <= 100:
        reading["percent"] = int(percentage[1])
    # pmset can attach presence metadata directly to the charging state when
    # there is no time-remaining field (for example, an optimized-charge hold).
    states = {re.sub(r"\s+present:\s*(?:true|false)\s*$", "", field).strip()
              for field in fields[1:]}
    if states & {"charging", "finishing charge"}:
        if reading["on_battery"]:
            return reading
        reading["charging"] = True
    elif states & {"discharging", "charged", "not charging"}:
        reading["charging"] = False
    reading["available"] = True
    return reading


def _sample_power_status() -> dict:
    """Take an uncached native sample; the shared cache handles probe failures."""
    if sys.platform == "darwin":
        result = subprocess.run(["/usr/bin/pmset", "-g", "batt"], capture_output=True,
                                text=True, timeout=3, check=True)
        return _mac_power_status(result.stdout)
    reading = _unknown_power_status()
    if sys.platform == "win32":
        import ctypes
        from ctypes import wintypes
        class PowerStatus(ctypes.Structure):
            _fields_ = [("ACLineStatus", wintypes.BYTE), ("BatteryFlag", wintypes.BYTE),
                        ("BatteryLifePercent", wintypes.BYTE), ("SystemStatusFlag", wintypes.BYTE),
                        ("BatteryLifeTime", wintypes.DWORD), ("BatteryFullLifeTime", wintypes.DWORD)]
        status = PowerStatus()
        if ctypes.windll.kernel32.GetSystemPowerStatus(ctypes.byref(status)):
            reading["available"] = status.ACLineStatus in (0, 1)
            reading["on_battery"] = status.ACLineStatus == 0
            reading["has_battery"] = status.BatteryFlag != 255 and not status.BatteryFlag & 128
            if reading["has_battery"]:
                reading["charging"] = bool(status.BatteryFlag & 8)
                if status.BatteryLifePercent <= 100:
                    reading["percent"] = int(status.BatteryLifePercent)
        return reading
    supplies = Path("/sys/class/power_supply")
    if not supplies.exists():
        return reading
    batteries = []
    incomplete = False
    for supply in supplies.iterdir():
        try:
            if (supply / "type").read_text().strip() != "Battery":
                continue
            reading["has_battery"] = True
            state = (supply / "status").read_text().strip()
            batteries.append((supply, state))
            if state == "Discharging":
                reading["on_battery"] = True
        except (OSError, UnicodeError):
            incomplete = True
    reading["available"] = not incomplete and all(
        state in {"Discharging", "Charging", "Full", "Not charging"}
        for _, state in batteries)
    if batteries and reading["available"]:
        reading["charging"] = any(state == "Charging" for _, state in batteries)
        if len(batteries) == 1:
            try:
                percent = int((batteries[0][0] / "capacity").read_text().strip())
                if 0 <= percent <= 100:
                    reading["percent"] = percent
            except (OSError, UnicodeError, ValueError):
                pass
    return reading


def power_status() -> dict:
    """Return a truthful copy of one shared, at-most-once-per-15s OS sample.

    ``available`` means the power source was read successfully. A missing or
    unrecognized charge/percentage remains None; no battery never means 100%.
    Failure replaces stale data with unknown and is cached at the same cadence.
    """
    global _battery_cache
    with _battery_lock:
        now = time.monotonic()
        if _battery_cache is not None and 0 <= now - _battery_cache[0] < 15:
            return dict(_battery_cache[1])
        try:
            reading = _sample_power_status()
        except (OSError, UnicodeError, subprocess.SubprocessError):
            reading = _unknown_power_status()
        # Cache from completion so even a slow/failing sample cannot cause a
        # burst of subprocess polls from the UI and runtime's separate readers.
        _battery_cache = (time.monotonic(), reading)
        return dict(reading)


def on_battery() -> bool:
    """Compatibility reader for runtime policy; shares the UI's cached sample."""
    return power_status()["on_battery"]


def screen_capture_granted() -> bool:
    """Screen Recording preflight for opt-in screenshot features; never prompts."""
    if sys.platform != "darwin":
        return False
    from .native.macos_vision import capture_granted
    return capture_granted()


def request_screen_capture_permission() -> bool:
    """Explicit user action only. macOS may show its prompt once and usually needs a relaunch."""
    if sys.platform != "darwin":
        return False
    from .native.macos_vision import request_capture_permission
    return request_capture_permission()


def macos_inline_prediction_enabled() -> bool | None:
    """Read-only state of macOS inline predictive text; None when unknown."""
    if sys.platform != "darwin":
        return None
    import platform
    from Foundation import NSUserDefaults
    domain = NSUserDefaults.standardUserDefaults().persistentDomainForName_("NSGlobalDomain")
    value = domain.get("NSAutomaticInlinePredictionEnabled") if domain is not None else None
    if value is not None:
        return bool(value)
    try:
        major = int(platform.mac_ver()[0].split(".")[0])
    except ValueError:
        return None
    return True if major >= 14 else None  # Sonoma introduced it, enabled by default.


def screen_text(snapshot, *, bridge, authorized, max_chars=2400, accessibility=True,
                screenshot_context=False) -> dict:
    """Opt-in focused-window context: Accessibility first, then on-device OCR.

    Screenshots are used only when separately enabled, Accessibility produced no
    text, and the exact focused window is still current before and after the
    capture. Pixels and recognized text are never written, uploaded or logged.
    """
    empty = {"text": "", "provenance": "", "status": "unavailable",
             "window_id": snapshot.window_id, "revision": snapshot.window_revision}
    if (snapshot.app_id in {"com.apple.Terminal", "com.googlecode.iterm2"}
            and snapshot.field_id.startswith("terminal:")):
        # Terminal consent covers the current unsent input, never scrollback.
        # Appearance may inspect its cropped glyph pixels; context OCR may not.
        if "inklow.native.macos_vision" in sys.modules:
            sys.modules["inklow.native.macos_vision"].clear_text_cache()
        return {**empty, "status": "blocked"}
    consent = bool(authorized())
    if (not screenshot_context or not consent or snapshot.protected) and "inklow.native.macos_vision" in sys.modules:
        sys.modules["inklow.native.macos_vision"].clear_text_cache()
    if (not (accessibility or screenshot_context) or not snapshot.window_id
            or not snapshot.window_revision or not consent):
        return {**empty, "status": "blocked"}
    status = "unavailable"
    reader = getattr(bridge, "focused_context", None)
    # A blocked accessibility result is a privacy boundary, not an OCR fallback.
    if accessibility and reader is not None:
        result = reader(snapshot, authorized=authorized, max_chars=min(max_chars, 4000))
        if not authorized():
            return {**empty, "status": "blocked"}
        if isinstance(result, dict):
            if (result.get("window_id") != snapshot.window_id
                    or result.get("revision") != snapshot.window_revision):
                return {**empty, "status": "stale"}
            text = result.get("text", "")
            status = result.get("status", "empty")
            if status == "ready" and isinstance(text, str) and text.strip():
                return {**empty, "text": text[:max_chars], "provenance": "accessibility", "status": "ready"}
            status = status if status in {"empty", "blocked", "unavailable", "error"} else "empty"
            if status == "blocked":
                return {**empty, "status": "blocked"}
    current = getattr(bridge, "context_matches", None)
    if not screenshot_context or sys.platform != "darwin" or current is None:
        return {**empty, "status": status}
    if snapshot.protected:
        return {**empty, "status": "blocked"}
    from .native.macos_vision import recognize_window_text
    # OCR owns the capture-time focus checks; recheck before publishing its text.
    result = recognize_window_text(snapshot, authorized=authorized,
                                   current=lambda: bool(current(snapshot)), max_chars=min(max_chars, 4000))
    if not authorized():
        return {**empty, "status": "blocked"}
    if not current(snapshot):
        return {**empty, "status": "stale"}
    if result.get("status") == "ready" and result.get("text", "").strip():
        return {**empty, "text": result["text"][:max_chars], "provenance": "screenshot", "status": "ready"}
    return {**empty, "status": result.get("status") if result.get("status") in {
        "empty", "blocked", "stale", "unavailable", "error", "screen_permission"} else "error"}


