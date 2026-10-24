"""Explicit OS actions and opt-in ephemeral context, independent of the UI toolkit."""
from __future__ import annotations

import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile
import threading
import time

_AUTOSTART_ID = "com.inklow.Inklow"
_INPUT_METHOD_ID = _AUTOSTART_ID + ".InputMethod"
_battery_lock = threading.Lock()
_battery_cache = (0.0, False)
_screen_lock = threading.Lock()


def _launch_arguments() -> list[str]:
    if getattr(sys, "frozen", False):
        return [sys.executable, "--background"]
    return [sys.executable, "-m", "inklow", "--background"]


def _desktop_command(arguments: list[str]) -> str:
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
    """Change only this user's explicit Inklow login entry; never start a copy."""
    if type(enabled) is not bool:
        raise ValueError("Launch at login must be on or off.")
    if os.environ.get("INKLOW_HOME"):
        raise RuntimeError("Login items cannot be changed from an isolated test profile. Open the normal installed Inklow app.")
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
    arguments = _launch_arguments()
    if sys.platform == "darwin":
        content = plistlib.dumps({
            "Label": _AUTOSTART_ID, "ProgramArguments": arguments,
            "RunAtLoad": True, "KeepAlive": False,
            "ProcessType": "Interactive", "LimitLoadToSessionType": "Aqua",
        })
    else:
        command = _desktop_command(arguments)
        content = ("[Desktop Entry]\nType=Application\nName=Inklow\n"
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
    content = ("[Desktop Entry]\nType=Application\nName=Inklow\n"
               "GenericName=Predictive typing input method\n"
               "Comment=Local predictions for supported KDE Wayland text inputs\n"
               f"Exec={command}\nIcon={icon}\nTerminal=false\nNoDisplay=true\n"
               "X-KDE-Wayland-VirtualKeyboard=true\nX-Inklow-Owned=true\n")
    _atomic_write(path, content.encode("utf-8"))
    return path


def wayland_input_method_selected() -> bool:
    """Avoid a competing normal autostart when KWin owns this application's launch."""
    if not sys.platform.startswith("linux") or not (
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
        raise RuntimeError("Open KDE System Settings → Keyboard → Virtual Keyboard and choose Inklow.")
    subprocess.Popen([command, "kcm_virtualkeyboard"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def _open(target: str) -> None:
    if sys.platform == "darwin":
        subprocess.run(["/usr/bin/open", target], check=True, timeout=15)
    elif sys.platform == "win32":
        os.startfile(target)
    else:
        subprocess.Popen(["xdg-open", target], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def open_permissions(kind: str = "accessibility") -> None:
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
        raise RuntimeError("Enable desktop accessibility. X11 uses AT-SPI keyboard access; KDE Wayland requires explicitly selecting Inklow as the native input method in Setup. OS access is never granted automatically.")


def reveal_models() -> None:
    from .models import model_path, get_preset
    folder = model_path(get_preset("balanced")).parent
    folder.mkdir(parents=True, exist_ok=True)
    _open(str(folder))


def clipboard_text() -> str:
    """Called only after explicit opt-in and safe native field validation."""
    if sys.platform == "darwin":
        from AppKit import NSPasteboard, NSPasteboardTypeString
        value = NSPasteboard.generalPasteboard().stringForType_(NSPasteboardTypeString)
        return str(value or "")[:4000]
    from PySide6.QtWidgets import QApplication
    app = QApplication.instance()
    if app is None:
        raise RuntimeError("Clipboard context requires the running menu application.")
    return app.clipboard().text()[:4000]


def on_battery() -> bool:
    global _battery_cache
    now = time.monotonic()
    with _battery_lock:
        if now - _battery_cache[0] < 15:
            return _battery_cache[1]
        value = False
        if sys.platform == "darwin":
            result = subprocess.run(["/usr/bin/pmset", "-g", "batt"], capture_output=True, text=True, timeout=3, check=True)
            value = "Now drawing from 'Battery Power'" in result.stdout
        elif sys.platform == "win32":
            import ctypes
            from ctypes import wintypes
            class PowerStatus(ctypes.Structure):
                _fields_ = [("ACLineStatus", wintypes.BYTE), ("BatteryFlag", wintypes.BYTE),
                            ("BatteryLifePercent", wintypes.BYTE), ("SystemStatusFlag", wintypes.BYTE),
                            ("BatteryLifeTime", wintypes.DWORD), ("BatteryFullLifeTime", wintypes.DWORD)]
            status = PowerStatus()
            if ctypes.windll.kernel32.GetSystemPowerStatus(ctypes.byref(status)):
                value = status.ACLineStatus == 0
        else:
            supplies = Path("/sys/class/power_supply")
            if supplies.exists():
                for supply in supplies.iterdir():
                    try:
                        if (supply / "type").read_text().strip() == "Battery" and (supply / "status").read_text().strip() == "Discharging":
                            value = True
                            break
                    except (OSError, UnicodeError):
                        continue
        _battery_cache = (now, value)
        return value


def _screen_field_allowed(snapshot) -> bool:
    """Recheck current PID/field protection without reading its text value."""
    import ApplicationServices as AX
    from AppKit import NSWorkspace, NSAccessibilityRoleDescription
    app = NSWorkspace.sharedWorkspace().frontmostApplication()
    if app is None or snapshot.protected:
        return False
    pid = int(app.processIdentifier())
    if not snapshot.field_id.startswith(f"{pid}:") or pid == os.getpid():
        return False
    def get(element, attribute):
        error, value = AX.AXUIElementCopyAttributeValue(element, attribute, None)
        return value if not error else None
    field = get(AX.AXUIElementCreateApplication(pid), "AXFocusedUIElement")
    if field is None or snapshot.field_id != f"{pid}:{hash(field)}":
        return False
    return (get(field, "AXRole") in ("AXTextField", "AXTextArea")
            and get(field, "AXSubrole") != "AXSecureTextField"
            and not get(field, "AXProtectedContent")
            and get(field, "AXRoleDescription") != NSAccessibilityRoleDescription("AXTextField", "AXSecureTextField")
            and bool(get(field, "AXFocused")))


def screen_text(snapshot) -> str:
    """Opt-in OCR of only the still-focused app window; pixels never touch disk."""
    if sys.platform != "darwin":
        raise RuntimeError("Screen OCR is currently available on macOS only. Surrounding text and optional clipboard context work on this platform.")
    import objc
    with objc.autorelease_pool():
        return _screen_text_native(snapshot)


def _screen_text_native(snapshot) -> str:
    import Quartz
    if not Quartz.CGPreflightScreenCaptureAccess():
        raise RuntimeError("Screen context needs Screen Recording permission. Enable it in Setup, or switch off screen context.")
    if not _screen_field_allowed(snapshot):
        return ""
    # Serialize requests: no backlog of stale screenshots, no concurrent OCR models.
    if not _screen_lock.acquire(blocking=False):
        return ""
    try:
        import ScreenCaptureKit as SC
        import Vision
        pid = int(snapshot.field_id.split(":", 1)[0])
        rect = snapshot.caret_rect
        if rect is None:
            return ""
        x, y, _, _ = rect
        candidates = [w for w in Quartz.CGWindowListCopyWindowInfo(Quartz.kCGWindowListOptionOnScreenOnly, Quartz.kCGNullWindowID)
                      if w.get("kCGWindowOwnerPID") == pid and w.get("kCGWindowLayer") == 0]
        selected = next((w for w in candidates
                         if w["kCGWindowBounds"]["X"] <= x <= w["kCGWindowBounds"]["X"] + w["kCGWindowBounds"]["Width"]
                         and w["kCGWindowBounds"]["Y"] <= y <= w["kCGWindowBounds"]["Y"] + w["kCGWindowBounds"]["Height"]), None)
        if selected is None:
            return ""
        ready, captured = threading.Event(), threading.Event()
        state = {}
        def content_done(content, error):
            state["content"], state["error"] = content, error
            ready.set()
        SC.SCShareableContent.getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler_(True, True, content_done)
        if not ready.wait(3):
            raise RuntimeError("The OS did not return screen context in time. Try again or disable screen context.")
        if state.get("error") or state.get("content") is None:
            raise RuntimeError("The OS refused the current application's screen context. Check Screen Recording permission.")
        window = next((w for w in state["content"].windows() if int(w.windowID()) == selected["kCGWindowNumber"]), None)
        if window is None or not _screen_field_allowed(snapshot):
            return ""
        content_filter = SC.SCContentFilter.alloc().initWithDesktopIndependentWindow_(window)
        configuration = SC.SCStreamConfiguration.alloc().init()
        bounds = selected["kCGWindowBounds"]
        scale = min(1.0, 1600 / max(bounds["Width"], bounds["Height"]))
        configuration.setWidth_(max(1, int(bounds["Width"] * scale)))
        configuration.setHeight_(max(1, int(bounds["Height"] * scale)))
        configuration.setShowsCursor_(False)
        def image_done(image, error):
            state["image"], state["error"] = image, error
            captured.set()
        SC.SCScreenshotManager.captureImageWithFilter_configuration_completionHandler_(content_filter, configuration, image_done)
        if not captured.wait(3):
            raise RuntimeError("Screen capture timed out. Disable screen context to keep suggestions responsive.")
        image = state.get("image")
        if image is None or state.get("error"):
            raise RuntimeError("The OS could not capture this application's window.")
        if not _screen_field_allowed(snapshot):
            return ""
        request = Vision.VNRecognizeTextRequest.alloc().init()
        request.setRecognitionLevel_(Vision.VNRequestTextRecognitionLevelFast)
        request.setUsesLanguageCorrection_(False)
        # Captured pixels are upright. nil options avoids OC_PythonDictionary's
        # missing-key exception in Vision on current macOS/PyObjC.
        handler = Vision.VNImageRequestHandler.alloc().initWithCGImage_orientation_options_(image, 1, None)
        success, error = handler.performRequests_error_([request], None)
        if not success:
            raise RuntimeError("Local screen text recognition failed. Screen context can be disabled independently.")
        texts = []
        for observation in request.results() or []:
            alternatives = observation.topCandidates_(1)
            if alternatives:
                texts.append(str(alternatives[0].string()))
        return "\n".join(texts)[:4000] if _screen_field_allowed(snapshot) else ""
    finally:
        _screen_lock.release()
