"""Windows UI Automation with exact native Edit and Unicode-input insertion.

Native Edit controls use EM_REPLACESEL. Other writable UIA Edit/Document
providers must expose collapsed TextPattern selection and TextEditPattern
composition state before SendInput(KEYEVENTF_UNICODE) is allowed. No clipboard,
whole-document SetValue, or browser DOM fallback is used.
"""
from __future__ import annotations

import ctypes as C
from ctypes import wintypes as W
from contextlib import contextmanager
import os
import threading

from ..core import Snapshot
from .base import BridgeBase, Shortcut, utf16_to_index


class GUITHREADINFO(C.Structure):
    _fields_ = [("cbSize", W.DWORD), ("flags", W.DWORD), ("hwndActive", W.HWND),
                ("hwndFocus", W.HWND), ("hwndCapture", W.HWND), ("hwndMenuOwner", W.HWND),
                ("hwndMoveSize", W.HWND), ("hwndCaret", W.HWND), ("rcCaret", W.RECT)]


class KBDLLHOOKSTRUCT(C.Structure):
    _fields_ = [("vkCode", W.DWORD), ("scanCode", W.DWORD), ("flags", W.DWORD),
                ("time", W.DWORD), ("dwExtraInfo", C.c_size_t)]


class KEYBDINPUT(C.Structure):
    _fields_ = [("wVk", W.WORD), ("wScan", W.WORD), ("dwFlags", W.DWORD),
                ("time", W.DWORD), ("dwExtraInfo", C.c_size_t)]


class MOUSEINPUT(C.Structure):
    _fields_ = [("dx", W.LONG), ("dy", W.LONG), ("mouseData", W.DWORD),
                ("dwFlags", W.DWORD), ("time", W.DWORD), ("dwExtraInfo", C.c_size_t)]


class INPUTUNION(C.Union):
    _fields_ = [("ki", KEYBDINPUT), ("mi", MOUSEINPUT)]


class INPUT(C.Structure):
    _fields_ = [("type", W.DWORD), ("data", INPUTUNION)]


class WindowsBridge(BridgeBase):
    def __init__(self) -> None:
        super().__init__()
        import comtypes
        import comtypes.client
        self.com = comtypes
        self.client = comtypes.client
        self.uia = self.client.GetModule("UIAutomationCore.dll")
        self.user = C.WinDLL("user32", use_last_error=True)
        self.kernel = C.WinDLL("kernel32", use_last_error=True)
        self.imm = C.WinDLL("imm32", use_last_error=True)
        self._hook = None
        self._hook_thread_id = 0
        self._thread: threading.Thread | None = None
        self._configure_api()
        self._set_status(
            "Windows UI Automation ready. Supports native Edit and writable UIA TextPattern controls "
            "with observable selection/composition. Password/read-only or incomplete providers are excluded. "
            "Run target apps at the same privilege level; modern app compatibility depends on their UIA provider."
        )

    def _configure_api(self) -> None:
        u, k, im = self.user, self.kernel, self.imm
        u.GetForegroundWindow.restype = W.HWND
        u.GetWindowThreadProcessId.argtypes = [W.HWND, C.POINTER(W.DWORD)]
        u.GetWindowThreadProcessId.restype = W.DWORD
        u.GetGUIThreadInfo.argtypes = [W.DWORD, C.POINTER(GUITHREADINFO)]
        u.GetGUIThreadInfo.restype = W.BOOL
        u.GetClassNameW.argtypes = [W.HWND, W.LPWSTR, C.c_int]
        u.GetClassNameW.restype = C.c_int
        u.GetWindowLongW.argtypes = [W.HWND, C.c_int]
        u.GetWindowLongW.restype = W.LONG
        u.IsWindowEnabled.argtypes = [W.HWND]
        u.IsWindowEnabled.restype = W.BOOL
        u.SendMessageTimeoutW.argtypes = [W.HWND, W.UINT, C.c_size_t, C.c_ssize_t, W.UINT, W.UINT, C.POINTER(C.c_size_t)]
        u.SendMessageTimeoutW.restype = C.c_ssize_t
        u.ClientToScreen.argtypes = [W.HWND, C.POINTER(W.POINT)]
        u.ClientToScreen.restype = W.BOOL
        k.OpenProcess.argtypes = [W.DWORD, W.BOOL, W.DWORD]
        k.OpenProcess.restype = W.HANDLE
        k.QueryFullProcessImageNameW.argtypes = [W.HANDLE, W.DWORD, W.LPWSTR, C.POINTER(W.DWORD)]
        k.QueryFullProcessImageNameW.restype = W.BOOL
        k.CloseHandle.argtypes = [W.HANDLE]
        k.GetCurrentThreadId.restype = W.DWORD
        k.GetModuleHandleW.argtypes = [W.LPCWSTR]
        k.GetModuleHandleW.restype = W.HMODULE
        self._hook_type = C.WINFUNCTYPE(C.c_ssize_t, C.c_int, C.c_size_t, C.c_ssize_t)
        u.SetWindowsHookExW.argtypes = [C.c_int, self._hook_type, W.HINSTANCE, W.DWORD]
        u.SetWindowsHookExW.restype = W.HANDLE
        u.UnhookWindowsHookEx.argtypes = [W.HANDLE]
        u.UnhookWindowsHookEx.restype = W.BOOL
        u.CallNextHookEx.argtypes = [W.HANDLE, C.c_int, C.c_size_t, C.c_ssize_t]
        u.CallNextHookEx.restype = C.c_ssize_t
        u.GetAsyncKeyState.argtypes = [C.c_int]
        u.GetAsyncKeyState.restype = C.c_short
        u.PostThreadMessageW.argtypes = [W.DWORD, W.UINT, C.c_size_t, C.c_ssize_t]
        u.PostThreadMessageW.restype = W.BOOL
        u.GetMessageW.argtypes = [C.POINTER(W.MSG), W.HWND, W.UINT, W.UINT]
        u.GetMessageW.restype = C.c_int
        u.PeekMessageW.argtypes = [C.POINTER(W.MSG), W.HWND, W.UINT, W.UINT, W.UINT]
        u.PeekMessageW.restype = W.BOOL
        im.ImmGetContext.argtypes = [W.HWND]
        im.ImmGetContext.restype = W.HANDLE
        im.ImmGetCompositionStringW.argtypes = [W.HANDLE, W.DWORD, W.LPVOID, W.DWORD]
        im.ImmGetCompositionStringW.restype = W.LONG
        im.ImmReleaseContext.argtypes = [W.HWND, W.HANDLE]
        im.ImmReleaseContext.restype = W.BOOL
        u.SendInput.argtypes = [W.UINT, C.POINTER(INPUT), C.c_int]
        u.SendInput.restype = W.UINT

    @contextmanager
    def _automation(self):
        # COM interfaces never cross threads/apartments. Each operation creates
        # and releases its own automation client on the calling thread.
        self.com.CoInitialize()
        try:
            automation = self.client.CreateObject(self.uia.CUIAutomation, interface=self.uia.IUIAutomation)
            yield automation
        finally:
            self.com.CoUninitialize()

    def _message(self, hwnd, message: int, wparam: int = 0, lparam: int = 0) -> int:
        result = C.c_size_t()
        if not self.user.SendMessageTimeoutW(hwnd, message, wparam, lparam, 0x0003, 200, C.byref(result)):
            raise OSError("Target edit is unresponsive or blocked by Windows integrity isolation")
        return int(result.value)

    def _process_name(self, pid: int) -> str | None:
        handle = self.kernel.OpenProcess(0x1000, False, pid)
        if not handle:
            return None
        try:
            buffer, length = C.create_unicode_buffer(32768), W.DWORD(32768)
            if not self.kernel.QueryFullProcessImageNameW(handle, 0, buffer, C.byref(length)):
                return None
            return buffer.value
        finally:
            self.kernel.CloseHandle(handle)

    def _focus(self):
        foreground = self.user.GetForegroundWindow()
        pid = W.DWORD()
        thread_id = self.user.GetWindowThreadProcessId(foreground, C.byref(pid))
        if not thread_id or pid.value == os.getpid():
            return None
        info = GUITHREADINFO()
        info.cbSize = C.sizeof(info)
        if not self.user.GetGUIThreadInfo(thread_id, C.byref(info)) or not info.hwndFocus:
            return None
        return info, int(pid.value)

    def _capture(self, automation):
        if self._stopped.is_set():
            return None
        focus = self._focus()
        if focus is None:
            return None
        info, pid = focus
        hwnd = info.hwndFocus
        app = self._process_name(pid)
        if app is None or self._excluded(app):
            return None
        element = automation.GetFocusedElement()
        # IsPassword is queried before any text range/value. Modern controls may
        # have no individual HWND, so their UIA runtime id is used for identity.
        if (element is None or element.CurrentIsPassword or not element.CurrentIsEnabled
                or not element.CurrentHasKeyboardFocus or int(element.CurrentProcessId) != pid
                or element.CurrentControlType not in (50004, 50030)):
            return None
        name = C.create_unicode_buffer(256)
        native_edit = (int(element.CurrentNativeWindowHandle or 0) == int(hwnd)
                       and self.user.GetClassNameW(hwnd, name, len(name))
                       and (name.value.lower() == "edit" or name.value.lower().startswith("windowsforms10.edit.")))
        if not native_edit:
            return self._capture_uia(element, pid, app)
        style = self.user.GetWindowLongW(hwnd, -16)
        if style & (0x0020 | 0x0800) or not self.user.IsWindowEnabled(hwnd):  # ES_PASSWORD | ES_READONLY
            return None
        # Native Edit's UTF-16 selection offsets match WM_GETTEXT exactly.
        pattern = element.GetCurrentPattern(10002).QueryInterface(self.uia.IUIAutomationValuePattern)
        if pattern.CurrentIsReadOnly:
            return None
        context = self.imm.ImmGetContext(hwnd)
        if context:
            try:
                if self.imm.ImmGetCompositionStringW(context, 0x0008, None, 0) > 0:  # GCS_COMPSTR
                    return None
            finally:
                self.imm.ImmReleaseContext(hwnd, context)
        native_length = self._message(hwnd, 0x000E)  # WM_GETTEXTLENGTH
        if native_length > 1_000_000:
            return None
        buffer = C.create_unicode_buffer(native_length + 1)
        copied = self._message(hwnd, 0x000D, len(buffer), C.addressof(buffer))  # WM_GETTEXT
        if copied != native_length:
            return None
        text = buffer.value
        start, end = W.DWORD(), W.DWORD()
        self._message(hwnd, 0x00B0, C.addressof(start), C.addressof(end))  # EM_GETSEL
        caret, selection_end = utf16_to_index(text, start.value), utf16_to_index(text, end.value)
        rect = None
        if info.hwndCaret == hwnd and info.rcCaret.bottom > info.rcCaret.top:
            point = W.POINT(info.rcCaret.left, info.rcCaret.top)
            if self.user.ClientToScreen(hwnd, C.byref(point)):
                rect = (float(point.x), float(point.y), float(max(1, info.rcCaret.right - info.rcCaret.left)),
                        float(info.rcCaret.bottom - info.rcCaret.top))
        return Snapshot(app, f"{pid}:{int(hwnd)}", text, caret, selection_end - caret, rect), hwnd

    def _capture_uia(self, element, pid: int, app: str):
        # TextEditPattern is required to establish that no IME composition is
        # active. Merely lacking IMM state is not evidence for TSF-backed apps.
        try:
            editing = element.GetCurrentPattern(10032).QueryInterface(self.uia.IUIAutomationTextEditPattern)
            if editing.GetActiveComposition():
                return None
            pattern = element.GetCurrentPattern(10014).QueryInterface(self.uia.IUIAutomationTextPattern)
            document = pattern.DocumentRange
            try:
                value = element.GetCurrentPattern(10002).QueryInterface(self.uia.IUIAutomationValuePattern)
                readonly = value.CurrentIsReadOnly
            except Exception:
                readonly = document.GetAttributeValue(40015)  # UIA_IsReadOnlyAttributeId
            # Unsupported/mixed attribute sentinels are not interpreted as False.
            if not isinstance(readonly, (bool, int)) or readonly != 0:
                return None
            selection = pattern.GetSelection()
            if selection.Length != 1:
                return None
            selected = selection.GetElement(0)
            if selected.CompareEndpoints(0, selected, 1) != 0:
                return None
            # Prefix ranges avoid guessing whether Character units are UTF-16,
            # graphemes, or Unicode scalar values in this particular provider.
            prefix_range = document.Clone()
            prefix_range.MoveEndpointByRange(1, selected, 0)
            prefix = prefix_range.GetText(1_000_001)
            text = document.GetText(1_000_001)
            if len(text) > 1_000_000 or not text.startswith(prefix):
                return None
            caret = len(prefix)
            rect = None
            bounds = selected.GetBoundingRectangles()
            if len(bounds) != 4:
                try:
                    second = element.GetCurrentPattern(10024).QueryInterface(self.uia.IUIAutomationTextPattern2)
                    active, caret_range = second.GetCaretRange()
                    if not active or caret_range.CompareEndpoints(0, selected, 0) != 0:
                        return None
                    bounds = caret_range.GetBoundingRectangles()
                except Exception:
                    bounds = ()
            if len(bounds) == 4 and bounds[3] > 0:
                rect = (float(bounds[0]), float(bounds[1]), max(1.0, float(bounds[2])), float(bounds[3]))
            runtime_id = tuple(element.GetRuntimeId())
            if not runtime_id:
                return None
            field_id = f"{pid}:uia:" + ":".join(str(part) for part in runtime_id)
            return Snapshot(app, field_id, text, caret, 0, rect), None
        except Exception:
            # Missing TextEdit/Text/selection/read-only capability fails closed,
            # never a synthetic empty snapshot or a whole-document write.
            return None

    def _send_unicode(self, text: str) -> bool:
        if any(self.user.GetAsyncKeyState(key) & 0x8000 for key in (0x11, 0x12, 0x5B, 0x5C)):
            return False
        encoded = text.encode("utf-16-le", errors="strict")
        count = len(encoded) // 2
        events = (INPUT * (count * 2))()
        for index in range(count):
            unit = int.from_bytes(encoded[index * 2:index * 2 + 2], "little")
            events[index * 2].type = events[index * 2 + 1].type = 1  # INPUT_KEYBOARD
            events[index * 2].data.ki = KEYBDINPUT(0, unit, 0x0004, 0, 0)
            events[index * 2 + 1].data.ki = KEYBDINPUT(0, unit, 0x0006, 0, 0)
        # Windows inserts this batch serially without interspersed user input.
        # UIPI may reject it; there is intentionally no privilege/clipboard bypass.
        inserted = self.user.SendInput(len(events), events, C.sizeof(INPUT))
        if inserted != len(events):
            self._set_status("Windows rejected or partially inserted Unicode input. Dismiss the suggestion and check app privilege levels before continuing.")
            return False
        return True

    def capture(self) -> Snapshot | None:
        with self._lock:
            try:
                with self._automation() as automation:
                    result = self._capture(automation)
                    return result[0] if result else None
            except Exception:
                return None

    def insert(self, snapshot: Snapshot, text: str) -> bool:
        if not text or "\x00" in text or snapshot.protected or snapshot.selection_length:
            return False
        with self._lock:
            try:
                with self._automation() as automation:
                    current = self._capture(automation)
                    if current is None or current[0].key != snapshot.key:
                        return False
                    if current[1] is not None:
                        buffer = C.create_unicode_buffer(text)
                        # TRUE preserves the control's native undo history.
                        self._message(current[1], 0x00C2, 1, C.addressof(buffer))  # EM_REPLACESEL
                        return True
                    focused = automation.GetFocusedElement()
                    if focused is None or focused.CurrentIsPassword or not focused.CurrentHasKeyboardFocus:
                        return False
                    identity = f"{int(focused.CurrentProcessId)}:uia:" + ":".join(str(part) for part in focused.GetRuntimeId())
                    if identity != snapshot.field_id:
                        return False
                    return self._send_unicode(text)
            except Exception:
                return False

    def start_shortcuts(self, callback: Shortcut) -> None:
        super().start_shortcuts(callback)
        with self._lock:
            if self._thread is not None or self._stopped.is_set():
                return
            self._thread = threading.Thread(target=self._event_loop, name="inklow-win-hook", daemon=True)
            self._thread.start()

    def _event_loop(self) -> None:
        u = self.user

        def hook(code, message, pointer):
            if code >= 0 and message in (0x0100, 0x0104):  # WM_KEYDOWN, WM_SYSKEYDOWN
                event = C.cast(pointer, C.POINTER(KBDLLHOOKSTRUCT)).contents
                # Ignore injected input and modified navigation (except Shift+Tab).
                modifiers = any(u.GetAsyncKeyState(key) & 0x8000 for key in (0x11, 0x12, 0x5B, 0x5C))
                if not modifiers and not event.flags & 0x10:
                    shift = bool(u.GetAsyncKeyState(0x10) & 0x8000)
                    action = ("all" if shift else "word") if event.vkCode == 0x09 else None
                    if event.vkCode == 0x1B and not shift:
                        action = "dismiss"
                    if action and self._dispatch(action):
                        return 1
            return u.CallNextHookEx(None, code, message, pointer)

        self._hook_callback = self._hook_type(hook)
        message = W.MSG()
        # Create the message queue before advertising the thread id.
        u.PeekMessageW(C.byref(message), None, 0, 0, 0)
        with self._lock:
            self._hook_thread_id = self.kernel.GetCurrentThreadId()
        try:
            while not self._stopped.is_set():
                with self._lock:
                    enabled = self._enabled
                if enabled and self._hook is None:
                    self._hook = u.SetWindowsHookExW(13, self._hook_callback, self.kernel.GetModuleHandleW(None), 0)
                    if not self._hook:
                        self._set_status("Windows keyboard hook was denied. Restart Inklow and use non-elevated target applications.")
                elif not enabled and self._hook is not None:
                    u.UnhookWindowsHookEx(self._hook)
                    self._hook = None
                result = u.GetMessageW(C.byref(message), None, 0, 0)
                if result <= 0:
                    break
                # Only thread-control messages are used; the low-level hook is
                # dispatched by GetMessage itself, with no Qt-thread dependency.
        finally:
            if self._hook is not None:
                u.UnhookWindowsHookEx(self._hook)
                self._hook = None
            with self._lock:
                self._hook_thread_id = 0

    def set_shortcuts_enabled(self, enabled: bool) -> None:
        super().set_shortcuts_enabled(enabled)
        with self._lock:
            thread_id = self._hook_thread_id
        if thread_id:
            self.user.PostThreadMessageW(thread_id, 0x8001, 0, 0)

    def stop(self) -> None:
        super().stop()
        with self._lock:
            thread_id = self._hook_thread_id
        if thread_id:
            self.user.PostThreadMessageW(thread_id, 0x0012, 0, 0)  # WM_QUIT
