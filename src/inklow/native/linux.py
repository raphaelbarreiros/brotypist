"""AT-SPI editable text on X11, with synchronous, replayable X11 key grabs."""
from __future__ import annotations

import os
import select
import socket
import threading

from ..core import Snapshot
from .base import BridgeBase, Shortcut


class LinuxBridge(BridgeBase):
    def __init__(self) -> None:
        super().__init__()
        self._thread: threading.Thread | None = None
        self._wake_read = None
        self._wake_write = None
        self._desktop = None
        self._available = False
        if os.environ.get("XDG_SESSION_TYPE", "").lower() == "wayland" or os.environ.get("WAYLAND_DISPLAY"):
            self._set_status(
                "Global typing is unavailable under Wayland, including XWayland applications. "
                "Use Inklow's built-in editor, or sign in to an X11 desktop session."
            )
            return
        if not os.environ.get("DISPLAY"):
            self._set_status("No X11 DISPLAY is available. Start Inklow in an X11 desktop session; otherwise use the built-in editor.")
            return
        try:
            import pyatspi
            from Xlib import X, XK, display
        except ImportError as exc:
            self._set_status(
                f"Linux accessibility dependency missing ({exc.name}). Install your distribution's "
                "python3-pyatspi and python3-gi packages plus Inklow's linux extra (python-xlib). "
                "A virtualenv must be created with --system-site-packages to see distro accessibility bindings."
            )
            return
        self.atspi, self.x, self.xk, self._display_type = pyatspi, X, XK, display.Display
        try:
            pyatspi.setTimeout(200, 500)
            self._desktop = pyatspi.Registry.getDesktop(0)
            if self._desktop is None:
                raise RuntimeError("AT-SPI registry returned no desktop")
            # Confirm authorization now; all later X requests belong to the hook
            # thread's independent connection.
            connection = display.Display()
            connection.close()
        except Exception as exc:
            self._set_status(
                f"X11/AT-SPI connection unavailable ({type(exc).__name__}). Enable desktop accessibility, "
                "ensure at-spi2-core and the session D-Bus are running, and run Inklow as the logged-in desktop user."
            )
            return
        self._wake_read, self._wake_write = socket.socketpair()
        self._wake_read.setblocking(False)
        self._wake_write.setblocking(False)
        self._available = True
        self._set_status(
            "Linux AT-SPI/X11 ready. Supports focused editable Entry/Text controls exposing Text and EditableText. "
            "Enable toolkit accessibility (GTK: GTK_MODULES=gail:atk-bridge where required; Qt: QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1) "
            "before starting target apps. Password fields are excluded."
        )

    def _application_identity(self, app):
        pid = int(app.get_process_id())
        if pid <= 0 or pid == os.getpid():
            return None
        name = str(app.name or "")
        try:
            executable = os.readlink(f"/proc/{pid}/exe")
        except OSError:
            executable = ""
        if not (name or executable) or self._excluded(name, executable):
            return None
        return pid, executable or name

    def _focused(self):
        a = self.atspi
        desktop = self._desktop
        desktop.clearCache()
        remaining = 2000
        for app_index in range(min(desktop.childCount, 256)):
            try:
                app = desktop.getChildAtIndex(app_index)
                if app is None:
                    continue
                app.clearCache()
                identity = self._application_identity(app)
                if identity is None:
                    continue
                # The active top-level ancestor prevents stale focused states in
                # background applications from being treated as the user's focus.
                stack = [(app, (app_index,), False)]
                while stack and remaining > 0:
                    element, path, active = stack.pop()
                    remaining -= 1
                    element.clearCache()
                    state = element.getState()
                    if state.contains(a.STATE_DEFUNCT):
                        continue
                    active = active or state.contains(a.STATE_ACTIVE)
                    if len(path) == 2 and not active:
                        continue  # Do not walk text trees in background windows.
                    if active and state.contains(a.STATE_FOCUSED):
                        return element, path, identity
                    children = min(element.childCount, 512)
                    for child_index in reversed(range(children)):
                        child = element.getChildAtIndex(child_index)
                        if child is not None:
                            stack.append((child, path + (child_index,), active))
            except Exception:
                continue
        return None

    def _capture(self):
        if not self._available or self._stopped.is_set():
            return None
        focused = self._focused()
        if focused is None:
            return None
        element, path, (pid, app_id) = focused
        a = self.atspi
        role, state = element.getRole(), element.getState()
        # Role/state/attributes are examined before obtaining the Text interface
        # or retrieving any value. Unknown/custom roles are deliberately refused.
        if role == a.ROLE_PASSWORD_TEXT or role not in (a.ROLE_ENTRY, a.ROLE_TEXT):
            return None
        required = (a.STATE_FOCUSED, a.STATE_EDITABLE, a.STATE_ENABLED, a.STATE_SENSITIVE)
        if not all(state.contains(flag) for flag in required):
            return None
        attrs = dict(item.split(":", 1) for item in element.getAttributes() if ":" in item)
        for key, value in attrs.items():
            key, value = key.casefold(), value.casefold()
            if (("password" in key or "protected" in key) and value not in ("false", "0", "no")) or value == "password":
                return None
            if key in ("preedit", "composing", "composition-active") and value not in ("false", "0", "no", ""):
                return None
        editable = element.queryEditableText()
        text_iface = element.queryText()
        count = int(text_iface.characterCount)
        if count < 0 or count > 1_000_000:
            return None
        caret = int(text_iface.caretOffset)
        selections = int(text_iface.getNSelections())
        selection_length = 0
        if selections:
            if selections != 1:
                return None
            start, end = text_iface.getSelection(0)
            selection_length = abs(end - start)
            if selection_length:
                return None
        text = text_iface.getText(0, -1)
        # AT-SPI uses Unicode character offsets, not UTF-16. A provider using
        # different units is unsupported, rather than guessed or silently sliced.
        if not isinstance(text, str) or len(text) != count or not 0 <= caret <= count:
            return None
        rect = None
        try:
            x, y, width, height = text_iface.getCharacterExtents(caret, a.DESKTOP_COORDS)
            if height > 0:
                rect = (float(x), float(y), float(max(1, width)), float(height))
        except Exception:
            pass
        object_path = getattr(element, "path", None)
        field_id = f"{pid}:{object_path if object_path else hash(element)}"
        return Snapshot(app_id, field_id, text, caret, selection_length, rect), editable, text_iface

    def capture(self) -> Snapshot | None:
        with self._lock:
            try:
                result = self._capture()
                return result[0] if result else None
            except Exception:
                return None

    def insert(self, snapshot: Snapshot, text: str) -> bool:
        if not text or "\x00" in text or snapshot.protected or snapshot.selection_length:
            return False
        with self._lock:
            try:
                current = self._capture()
                if current is None or current[0].key != snapshot.key:
                    return False
                # AT-SPI positions are characters, but insertion length is UTF-8 bytes.
                if not current[1].insertText(snapshot.caret, text, len(text.encode("utf-8"))):
                    return False
                # Insertion succeeded; caret placement is best-effort because
                # returning False now could cause a caller to insert twice.
                try:
                    current[2].setCaretOffset(snapshot.caret + len(text))
                except Exception:
                    pass
                return True
            except Exception:
                return False

    def start_shortcuts(self, callback: Shortcut) -> None:
        super().start_shortcuts(callback)
        with self._lock:
            if not self._available or self._thread is not None or self._stopped.is_set():
                return
            self._thread = threading.Thread(target=self._event_loop, name="inklow-x11", daemon=True)
            self._thread.start()

    def _wake(self) -> None:
        if self._wake_write is not None:
            try:
                self._wake_write.send(b"x")
            except (BlockingIOError, OSError):
                pass

    def set_shortcuts_enabled(self, enabled: bool) -> None:
        super().set_shortcuts_enabled(enabled)
        self._wake()

    def _event_loop(self) -> None:
        X, XK = self.x, self.xk
        connection = None
        grabbed = False
        try:
            connection = self._display_type()
            root = connection.screen().root
            tab = connection.keysym_to_keycode(XK.string_to_keysym("Tab"))
            escape = connection.keysym_to_keycode(XK.string_to_keysym("Escape"))
            if not tab or not escape:
                raise RuntimeError("X11 keymap has no Tab/Escape keys")
            num_key = connection.keysym_to_keycode(XK.string_to_keysym("Num_Lock"))
            num_mask = 0
            for index, keys in enumerate(connection.get_modifier_mapping()):
                if num_key and num_key in keys:
                    num_mask |= 1 << index
            locks = {0, X.LockMask, num_mask, X.LockMask | num_mask}
            shortcuts = [(tab, modifier | lock) for modifier in (0, X.ShiftMask) for lock in locks]
            shortcuts += [(escape, lock) for lock in locks]
            while not self._stopped.is_set():
                with self._lock:
                    enabled = self._enabled
                if enabled and not grabbed:
                    errors = []
                    for code, modifiers in shortcuts:
                        root.grab_key(code, modifiers, False, X.GrabModeAsync, X.GrabModeSync,
                                      onerror=lambda error, request: errors.append(error))
                    connection.sync()
                    if errors:
                        for code, modifiers in shortcuts:
                            root.ungrab_key(code, modifiers)
                        connection.flush()
                        super().set_shortcuts_enabled(False)
                        self._set_status(
                            "An X11 shortcut is already owned by another application. Global interception is disabled. "
                            "Release conflicting Tab/Shift+Tab/Escape bindings, then pause/resume global typing."
                        )
                    else:
                        grabbed = True
                elif not enabled and grabbed:
                    for code, modifiers in shortcuts:
                        root.ungrab_key(code, modifiers)
                    connection.ungrab_keyboard(X.CurrentTime)
                    connection.flush()
                    grabbed = False
                while connection.pending_events():
                    event = connection.next_event()
                    if event.type != X.KeyPress:
                        continue
                    action = None
                    modifiers = event.state & ~(X.LockMask | num_mask)
                    if event.detail == tab and modifiers in (0, X.ShiftMask):
                        action = "all" if modifiers & X.ShiftMask else "word"
                    elif event.detail == escape and not modifiers:
                        action = "dismiss"
                    if action and self._dispatch(action):
                        connection.allow_events(X.AsyncKeyboard, event.time)
                        connection.ungrab_keyboard(event.time)
                    else:
                        # Replay to the original client, preserving both shortcut
                        # delivery and X11's original modifier state.
                        connection.allow_events(X.ReplayKeyboard, event.time)
                    connection.flush()
                ready, _, _ = select.select([connection.fileno(), self._wake_read], [], [], 0.25)
                if self._wake_read in ready:
                    try:
                        self._wake_read.recv(4096)
                    except BlockingIOError:
                        pass
        except Exception as exc:
            self._set_status(
                f"X11 keyboard grabs unavailable ({type(exc).__name__}). Check DISPLAY/Xauthority and conflicting shortcuts; "
                "Wayland is unsupported. Use the built-in editor until corrected."
            )
        finally:
            if connection is not None:
                try:
                    connection.ungrab_keyboard(X.CurrentTime)
                    connection.close()  # Releases every passive grab on disconnect.
                except Exception:
                    pass
            for channel in (self._wake_read, self._wake_write):
                if channel is not None:
                    try:
                        channel.close()
                    except OSError:
                        pass

    def stop(self) -> None:
        super().stop()
        self._wake()
        if self._thread is None:
            for channel in (self._wake_read, self._wake_write):
                if channel is not None:
                    channel.close()
