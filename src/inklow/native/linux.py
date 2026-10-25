"""AT-SPI editable text on X11, with synchronous, replayable X11 key grabs."""
from __future__ import annotations

import os
import select
import socket
import threading

from ..core import InlineRender, Snapshot
from .base import BridgeBase, Shortcut


class LinuxBridge(BridgeBase):
    def __init__(self, *, accessibility_only: bool = False) -> None:
        super().__init__()
        self._thread: threading.Thread | None = None
        self._wake_read = None
        self._wake_write = None
        self._desktop = None
        self._available = False
        if accessibility_only:
            try:
                import pyatspi
                self._connect_accessibility(pyatspi)
            except ImportError:
                self._set_status(
                    "Linux accessibility dependency missing. Install your distribution's python3-pyatspi "
                    "and python3-gi packages; virtualenvs need --system-site-packages."
                )
                return
            except Exception:
                self._set_status("AT-SPI unavailable. Enable desktop accessibility and the session D-Bus.")
                return
            self._available = True
            return
        if os.environ.get("XDG_SESSION_TYPE", "").lower() == "wayland" or os.environ.get("WAYLAND_DISPLAY"):
            self._set_status(
                "KDE Wayland requires BroTypist to be launched as the explicitly selected native input method. "
                "Install its entry in Privacy or Welcome, Quit, then choose BroTypist in KDE Virtual Keyboard settings. "
                "Other Wayland compositors and XWayland-only controls are unsupported; X11 sessions use the AT-SPI adapter."
            )
            return
        if not os.environ.get("DISPLAY"):
            self._set_status("No X11 DISPLAY is available. Start BroTypist in an X11 desktop session.")
            return
        try:
            import pyatspi
            from Xlib import X, XK, display
        except ImportError as exc:
            self._set_status(
                f"Linux accessibility dependency missing ({exc.name}). Install your distribution's "
                "python3-pyatspi and python3-gi packages plus BroTypist's Linux dependency (python-xlib). "
                "A virtualenv must be created with --system-site-packages to see distro accessibility bindings."
            )
            return
        self.atspi, self.x, self.xk, self._display_type = pyatspi, X, XK, display.Display
        try:
            self._connect_accessibility(pyatspi)
            # Confirm authorization now; all later X requests belong to the hook
            # thread's independent connection.
            connection = display.Display()
            connection.close()
        except Exception as exc:
            self._set_status(
                f"X11/AT-SPI connection unavailable ({type(exc).__name__}). Enable desktop accessibility, "
                "ensure at-spi2-core and the session D-Bus are running, and run BroTypist as the logged-in desktop user."
            )
            return
        self._wake_read, self._wake_write = socket.socketpair()
        self._wake_read.setblocking(False)
        self._wake_write.setblocking(False)
        self._available = True
        self._set_status(
            "Linux AT-SPI/X11 ready. Supports focused editable Entry/Text controls exposing Text and EditableText. "
            "Inline additionally requires explicit inactive IME state, font/direction attributes and matching visible native metrics; unsupported providers stay silent. "
            "Enable toolkit accessibility (GTK: GTK_MODULES=gail:atk-bridge where required; Qt: QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1) "
            "before starting target apps. Password fields are excluded."
            " Browser URLs are not observable through documented window metadata; domain rules cannot apply here."
        )

    def _connect_accessibility(self, pyatspi) -> None:
        """Initialize shared metadata access without importing or opening Xlib."""
        self.atspi = pyatspi
        pyatspi.setTimeout(200, 500)
        self._desktop = pyatspi.Registry.getDesktop(0)
        if self._desktop is None:
            raise RuntimeError("AT-SPI registry returned no desktop")

    def _application_identity(self, app, include_disabled: bool = False):
        pid = int(app.get_process_id())
        if pid <= 0 or pid == os.getpid():
            return None
        name = str(app.name or "")
        try:
            executable = os.readlink(f"/proc/{pid}/exe")
        except OSError:
            executable = ""
        if not (name or executable) or (not include_disabled and not self._allowed(executable or name, "", name, executable)):
            return None
        return pid, executable or name

    def _focused(self, include_disabled: bool = False):
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
                identity = self._application_identity(app, include_disabled)
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

    def foreground(self) -> dict:
        with self._lock:
            try:
                if not self._available:
                    return super().foreground()
                focused = self._focused(include_disabled=True)
                if focused is None:
                    return super().foreground()
                _, _, (_, app_id) = focused
                # There is no generic browser-window URL property in AT-SPI.
                # Editable address bars and page DOM are not metadata sources.
                return {"app_id": app_id, "name": app_id.rsplit("/", 1)[-1], "domain": "",
                        "browser": self._is_browser(app_id), "domain_observable": False}
            except Exception:
                return super().foreground()

    def _capture(self, *, geometry: bool = True, reject_selection: bool = False):
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
        if reject_selection and selections:
            return None
        selection_length = 0
        if selections:
            if selections != 1:
                return None
            start, end = (int(offset) for offset in text_iface.getSelection(0))
            caret = min(start, end)
            selection_length = abs(end - start)
        text = text_iface.getText(0, -1)
        # AT-SPI uses Unicode character offsets, not UTF-16. A provider using
        # different units is unsupported, rather than guessed or silently sliced.
        if (not isinstance(text, str) or len(text) != count or not 0 <= caret <= count
                or not 0 <= selection_length <= count - caret):
            return None
        rect = None
        if geometry:
            try:
                x, y, width, height = text_iface.getCharacterExtents(caret, a.DESKTOP_COORDS)
                if height > 0:
                    rect = (float(x), float(y), float(max(1, width)), float(height))
            except Exception:
                pass
        object_path = getattr(element, "path", None)
        window = self._window_ancestor(element)
        window_id = f"{pid}:{getattr(window, 'path', None) or hash(window)}" if window is not None else ""
        render = self._inline_metrics(element, text_iface, text, caret, rect, attrs) if geometry and not selection_length else None
        field_id = f"{pid}:{object_path if object_path else hash(element)}"
        return Snapshot(app_id, field_id, text, caret, selection_length, rect,
                        multiline=state.contains(a.STATE_MULTI_LINE), inline_render=render,
                        window_id=window_id, window_revision=window_id), editable, text_iface

    def _window_ancestor(self, element):
        for _ in range(16):
            if element is None:
                return None
            state = element.getState()
            if state.contains(self.atspi.STATE_ACTIVE):
                return element
            element = getattr(element, "parent", None)
        return None

    def _inline_metrics(self, element, interface, text, caret, rect, attrs):
        # AT-SPI has no universal IME-state property. Only providers explicitly
        # advertising an inactive composition can provide a safe overlay.
        if (not rect or not caret or text[caret - 1] in "\r\n"
                or (text[caret:] and not text[caret:].startswith(("\r", "\n")))
                or not any(attrs.get(key, "").casefold() in ("false", "0", "no")
                           for key in ("preedit", "composing", "composition-active"))):
            return None
        try:
            values, start, end = interface.getAttributeRun(caret - 1, True)
            run = dict(values) if isinstance(values, dict) else dict(item.split(":", 1) for item in values if ":" in item)
            if not start <= caret - 1 < end or run.get("direction") != "ltr":
                return None
            name, size = run.get("family-name", ""), float(run.get("size", 0))
            if not name or not 1 <= size <= 300 or run.get("rise", "0") != "0":
                return None
            import gi
            gi.require_version("Pango", "1.0")
            gi.require_version("PangoCairo", "1.0")
            from gi.repository import Pango, PangoCairo
            description = Pango.FontDescription()
            description.set_family(name)
            description.set_size(round(size * Pango.SCALE))
            weight = int(run.get("weight", "400"))
            italic = run.get("style", "normal") == "italic"
            description.set_weight(weight)
            description.set_style(Pango.Style.ITALIC if italic else Pango.Style.NORMAL)
            context = PangoCairo.FontMap.get_default().create_context()
            host_font = context.load_font(description)
            if host_font is None or host_font.describe().get_family().casefold() != name.casefold():
                return None
            metrics = context.get_metrics(description, Pango.Language.get_default())
            ascent, descent = metrics.get_ascent() / Pango.SCALE, metrics.get_descent() / Pango.SCALE
            glyph = interface.getCharacterExtents(caret - 1, self.atspi.DESKTOP_COORDS)
            bounds = element.queryComponent().getExtents(self.atspi.DESKTOP_COORDS)
            clip = tuple(float(getattr(bounds, key)) for key in ("x", "y", "width", "height"))
            parent = element
            for _ in range(16):
                if parent is None:
                    break
                state = parent.getState()
                if (not state.contains(self.atspi.STATE_VISIBLE)
                        or not state.contains(self.atspi.STATE_SHOWING)):
                    return None
                bounds = parent.queryComponent().getExtents(self.atspi.DESKTOP_COORDS)
                if bounds.width > 0 and bounds.height > 0:
                    left, top = max(clip[0], bounds.x), max(clip[1], bounds.y)
                    right = min(clip[0] + clip[2], bounds.x + bounds.width)
                    bottom = min(clip[1] + clip[3], bounds.y + bounds.height)
                    if right <= left or bottom <= top:
                        return None
                    clip = (float(left), float(top), float(right - left), float(bottom - top))
                if state.contains(self.atspi.STATE_ACTIVE):
                    break
                parent = getattr(parent, "parent", None)
            if (abs(glyph[3] - ascent - descent) > 1
                    or abs(glyph[1] - rect[1]) > 1 or abs(glyph[0] + glyph[2] - rect[0]) > 1.5):
                return None
            render = InlineRender(name, size, float(glyph[1]) + ascent, clip, ascent, descent,
                                  source="linux-atspi-font-run", font_weight=weight, italic=italic)
            return render if render.valid(rect) else None
        except Exception:
            return None

    def context_matches(self, snapshot):
        try:
            if not self._available or self._stopped.is_set():
                return False
            focused = self._focused()
            if focused is None:
                return False
            element, _, (pid, app_id) = focused
            state, a = element.getState(), self.atspi
            window = self._window_ancestor(element)
            identity = f"{pid}:{getattr(element, 'path', None) or hash(element)}"
            window_id = f"{pid}:{getattr(window, 'path', None) or hash(window)}" if window is not None else ""
            attrs = dict(item.split(":", 1) for item in element.getAttributes() if ":" in item)
            protected = any((("password" in key.casefold() or "protected" in key.casefold())
                             and value.casefold() not in ("false", "0", "no")) or value.casefold() == "password"
                            for key, value in attrs.items())
            return (app_id == snapshot.app_id and identity == snapshot.field_id
                    and window_id == snapshot.window_id and bool(window_id) and not protected
                    and element.getRole() in (a.ROLE_ENTRY, a.ROLE_TEXT)
                    and all(state.contains(flag) for flag in
                            (a.STATE_FOCUSED, a.STATE_EDITABLE, a.STATE_ENABLED, a.STATE_SENSITIVE,
                             a.STATE_VISIBLE, a.STATE_SHOWING)))
        except Exception:
            return False

    def focused_context(self, snapshot, *, authorized, max_chars=2400):
        result = {"text": "", "provenance": "accessibility",
                  "window_id": snapshot.window_id, "revision": snapshot.window_revision,
                  "status": "unavailable"}
        if not authorized() or not self.context_matches(snapshot):
            return {**result, "status": "blocked"}
        try:
            import time
            focused = self._focused()
            if focused is None:
                return {**result, "status": "blocked"}
            element, _, (pid, app_id) = focused
            window = self._window_ancestor(element)
            field_id = f"{pid}:{getattr(element, 'path', None) or hash(element)}"
            window_id = f"{pid}:{getattr(window, 'path', None) or hash(window)}" if window is not None else ""
            if (app_id != snapshot.app_id or field_id != snapshot.field_id
                    or window_id != snapshot.window_id or not window_id):
                return {**result, "status": "blocked"}
            stack, chunks, remaining = [(window, 0)], [], min(12000, max(0, max_chars))
            deadline, a = time.monotonic() + 0.15, self.atspi
            for _ in range(128):
                if not stack or remaining <= 0 or time.monotonic() >= deadline:
                    break
                element, depth = stack.pop()
                if not authorized():
                    return {**result, "status": "blocked"}
                state, role = element.getState(), element.getRole()
                if (role == a.ROLE_PASSWORD_TEXT or state.contains(a.STATE_DEFUNCT)
                        or not state.contains(a.STATE_VISIBLE) or not state.contains(a.STATE_SHOWING)):
                    continue
                attrs = dict(item.split(":", 1) for item in element.getAttributes() if ":" in item)
                if any((("password" in key.casefold() or "protected" in key.casefold())
                        and value.casefold() not in ("false", "0", "no")) or value.casefold() == "password"
                       for key, value in attrs.items()):
                    continue
                editable = state.contains(a.STATE_EDITABLE)
                if not editable and role in (a.ROLE_LABEL, a.ROLE_TEXT):
                    if not authorized():
                        return {**result, "status": "blocked"}
                    try:
                        value = element.queryText().getText(0, remaining)
                    except (NotImplementedError, AttributeError):
                        value = str(element.name or "")[:remaining]
                    if value and value != snapshot.text:
                        chunks.append(value)
                        remaining -= len(value) + 1
                if depth < 8 and not editable:
                    stack.extend((element.getChildAtIndex(index), depth + 1)
                                 for index in range(min(element.childCount, 64)))
            if not authorized() or not self.context_matches(snapshot):
                return {**result, "status": "blocked"}
            text = "\n".join(chunks)[:max_chars]
            return {**result, "text": text, "status": "ready" if text else "empty"}
        except Exception:
            return {**result, "status": "error"}

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

    def replace_range(self, snapshot: Snapshot, start: int, length: int, text: str) -> bool:
        if not self._valid_replacement(snapshot, start, length, text):
            return False
        with self._lock:
            try:
                current = self._capture()
                if current is None or current[0].key != snapshot.key:
                    return False
                if length == 0 and start == snapshot.caret and snapshot.selection_length == 0:
                    return self.insert(snapshot, text)
                # EditableText only offers delete-then-insert, which can lose
                # original text if insertion fails. No destructive fallback.
                self._set_status(
                    "This AT-SPI control cannot safely replace a selection or emoji suffix: "
                    "atomic range replacement is unavailable. Ordinary completion insertion remains supported."
                )
                return False
            except Exception:
                return False

    def configure(self, config) -> None:
        super().configure(config)
        self._wake()

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
        grabbed: list[tuple[int, int]] = []
        try:
            connection = self._display_type()
            root = connection.screen().root
            modifier_masks = {"Ctrl": X.ControlMask, "Alt": X.Mod1Mask,
                              "Shift": X.ShiftMask, "Super": X.Mod4Mask}
            num_key = connection.keysym_to_keycode(XK.string_to_keysym("Num_Lock"))
            num_mask = 0
            for index, keys in enumerate(connection.get_modifier_mapping()):
                if num_key and num_key in keys:
                    num_mask |= 1 << index
            locks = {0, X.LockMask, num_mask, X.LockMask | num_mask}
            active_bindings = None
            while not self._stopped.is_set():
                with self._lock:
                    enabled = self._enabled
                    bindings = tuple(self._shortcuts.values())
                if grabbed and (not enabled or bindings != active_bindings):
                    for code, modifiers in grabbed:
                        root.ungrab_key(code, modifiers)
                    connection.ungrab_keyboard(X.CurrentTime)
                    connection.flush()
                    grabbed = []
                if enabled and not grabbed:
                    key_names = {}
                    shortcuts = set()
                    for binding in (*bindings, "Escape", *(str(number) for number in range(1, 10))):
                        if not binding:
                            continue
                        parts = binding.split("+")
                        key = parts[-1]
                        if key == "Tab" and "Alt" in parts[:-1]:
                            continue
                        code = 49 if key == "AboveTab" else connection.keysym_to_keycode(
                            XK.string_to_keysym({"Space": "space", "Return": "Return"}.get(key, key)))
                        if not code:
                            continue
                        key_names[code] = key
                        modifier = 0
                        for name in parts[:-1]:
                            modifier |= modifier_masks[name]
                        shortcuts.update((code, modifier | lock) for lock in locks)
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
                            "Release conflicting configured bindings, then pause/resume global typing."
                        )
                    else:
                        grabbed = list(shortcuts)
                        active_bindings = bindings
                while connection.pending_events():
                    event = connection.next_event()
                    if event.type != X.KeyPress:
                        continue
                    state = event.state & ~(X.LockMask | num_mask)
                    modifiers = tuple(name for name, mask in modifier_masks.items() if state & mask)
                    known_mask = X.ControlMask | X.Mod1Mask | X.ShiftMask | X.Mod4Mask
                    action = (self._key_action(key_names.get(event.detail, ""), modifiers, platform="linux")
                              if not state & ~known_mask else None)
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
                "Wayland is unsupported."
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
