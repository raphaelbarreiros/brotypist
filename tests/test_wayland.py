"""Native Wayland consumer contracts with synthetic AT-SPI/compositor peers.

R1: pre-read privacy and stale-context rejection; R2: stable context identity;
R3: exact shortcut consumption; R5: last-frame delivery and menu closure.
Only OS/protocol peers are fake; capture, insertion, key routing and rendering
are production methods. C transport bounds/physical key forwarding need the
isolated native smoke, not a Python reimplementation of the C validation.
"""
import ctypes
from types import SimpleNamespace

import pytest

from inklow.config import Config
from inklow.native.base import BridgeBase
from inklow.native.wayland import WaylandBridge, _Event
from inklow.runtime import Controller


class Accessible:
    def __init__(self, *, children=(), flags=(), role="window", path="/field"):
        self.children = children
        self.flags = set(flags)
        self.role = role
        self.path = path
        self.name = "writer"

    @property
    def childCount(self):
        return len(self.children)

    def getChildAtIndex(self, index):
        return self.children[index]

    def clearCache(self):
        pass

    def getState(self):
        return SimpleNamespace(contains=self.flags.__contains__)

    def getRole(self):
        return self.role

    def getAttributes(self):
        return self.attributes

    def get_process_id(self):
        return 2147483647


class NativePeer:
    """A synthetic external field and native transport, never a bridge double."""
    def __init__(self):
        self.text = "Dear team, the update"
        self.executable = "/usr/bin/writer"
        self.caret = len(self.text)
        self.selection = 0
        self.revision, self.serial, self.context = 7, 19, 101
        self.ready = True
        self.surrounding = self.text
        self.reads = self.interfaces = 0
        self.after_read = None
        self.commits = []
        self.accept_commits = True
        self.frames = []
        self.present_result = 1
        self.hidden = 0
        self.field = Accessible(role="entry", flags=("focused", "editable", "enabled", "sensitive"))
        self.field.attributes = []
        self.field.queryEditableText = lambda: SimpleNamespace(
            insertText=lambda *args: pytest.fail("Wayland must not edit through AT-SPI"))
        self.field.queryText = self.query_text
        window = Accessible(children=(self.field,), flags=("active",))
        self.desktop = Accessible(children=(Accessible(children=(window,)),))

    def query_text(self):
        self.interfaces += 1
        return self

    @property
    def characterCount(self):
        return len(self.text)

    @property
    def caretOffset(self):
        return self.caret

    def getNSelections(self):
        return self.selection

    def getText(self, start, end):
        self.reads += 1
        text = self.text
        if self.after_read:
            self.after_read()
        return text

    def inklow_wl_state(self, client, revision, serial):
        revision._obj.value = self.revision
        serial._obj.value = self.serial
        return int(self.ready)

    def inklow_wl_context(self, client):
        return self.context

    def inklow_wl_matches(self, client, revision, serial, text, length, cursor):
        encoded = self.surrounding.encode("utf-8")
        return int(text == encoded and length == len(encoded)
                   and cursor == len(self.text[:self.caret].encode("utf-8")))

    def inklow_wl_commit(self, client, revision, serial, text):
        # Deliberately permissive: production guards must prevent stale writes.
        value = text.decode("utf-8")
        if not self.accept_commits:
            self.commits.append(value)
            return 1  # The protocol accepted the request, but the app filtered it.
        self.commits.append(value)
        self.text = self.text[:self.caret] + value + self.text[self.caret:]
        self.caret += len(value)
        self.surrounding = self.text
        self.revision += 1
        self.serial += 1
        return 1

    def inklow_wl_hide(self, client):
        self.hidden += 1

    def inklow_wl_flush(self, client):
        return 1

    def inklow_wl_present(self, client, pixels, width, height, stride, scale):
        self.frames.append(ctypes.string_at(pixels, stride * height))
        return self.present_result

    def inklow_wl_close(self, client):
        self.closed = True


@pytest.fixture
def native(monkeypatch):
    peer = NativePeer()
    bridge = WaylandBridge.__new__(WaylandBridge)
    BridgeBase.__init__(bridge)
    bridge._available = True
    bridge._desktop = peer.desktop
    bridge.atspi = SimpleNamespace(**{
        name: value for name, value in (
            ("ROLE_ENTRY", "entry"), ("ROLE_TEXT", "text"), ("ROLE_PASSWORD_TEXT", "password"),
            *(("STATE_" + flag.upper(), flag) for flag in
              ("focused", "editable", "enabled", "sensitive", "active", "defunct", "multi_line")))})
    bridge._library, bridge._client = peer, object()
    bridge._read_notifier = bridge._write_notifier = None
    bridge._dispatching = bridge._close_pending = False
    bridge._native_depth = 0
    bridge._ui_callback = None
    bridge._needs_context_state = bridge._context_sync_pending = False
    bridge._context_synced = (0, 0)
    bridge._context_calls = set()
    bridge._panel_snapshot = None
    bridge._panel_text = ""
    bridge._panel_paused = bridge._pending_render = False
    bridge._menu = None
    bridge._menu_page, bridge._menu_page_size, bridge._menu_selected = 0, 1, -1
    bridge._regions, bridge._pressed = [], None
    monkeypatch.setattr("inklow.native.linux.os.readlink", lambda path: peer.executable)
    return bridge, peer


def event(bridge, **values):
    return bridge._event(None, ctypes.pointer(_Event(**values)))


def test_native_field_identity_survives_revisions_but_not_context_reactivation(native):
    # R2: revisions move the anchor, not the field lifetime.
    bridge, peer = native
    first = bridge.capture()
    assert first is not None
    assert first.text == "Dear team, the update"
    assert first.caret_rect is None and first.field_rect is None and first.has_anchor
    assert bridge.insert(first, " arrives ")
    second = bridge.capture()
    assert second.field_id == first.field_id
    assert second.native_anchor != first.native_anchor
    assert second.text == "Dear team, the update arrives "
    assert bridge.insert(second, "tomorrow")
    assert peer.text == "Dear team, the update arrives tomorrow"
    assert peer.commits == [" arrives ", "tomorrow"]
    peer.context += 1
    reactivated = bridge.capture()
    assert reactivated.field_id != second.field_id


@pytest.mark.parametrize("change", ["revision", "serial", "context", "surrounding", "selection", "purpose"])
def test_stale_or_unsafe_native_offer_never_reaches_commit(native, change):
    # R1: even identical visible text cannot authorize a stale protocol write.
    bridge, peer = native
    offered = bridge.capture()
    original = peer.text
    if change in ("revision", "serial", "context"):
        setattr(peer, change, getattr(peer, change) + 1)
    elif change == "surrounding":
        peer.surrounding = "Another native field"
    elif change == "selection":
        peer.selection = 1
    else:
        peer.ready = False
    assert not bridge.insert(offered, " must not appear")
    assert peer.text == original
    assert peer.commits == []


@pytest.mark.parametrize("guard", ["unknown-purpose", "password-role", "protected-attribute", "selection", "excluded-app", "unobservable-domain"])
def test_privacy_gates_precede_field_value_reads(native, guard):
    # R1: forbidden reads are errors, not merely discarded return values.
    bridge, peer = native
    if guard == "unknown-purpose":
        peer.ready = False
    elif guard == "password-role":
        peer.field.role = "password"
    elif guard == "protected-attribute":
        peer.field.attributes = ["protected:true"]
    elif guard == "selection":
        peer.selection = 1
    elif guard == "excluded-app":
        bridge.configure(Config(excluded_apps=("writer",)))
    else:
        peer.executable = "/usr/bin/firefox"
        bridge.configure(Config(app_rules={"domain:private.example": {"enabled": False}}))
    peer.getText = lambda *args: pytest.fail("Protected field value was read")
    assert bridge.capture() is None
    assert peer.reads == 0 and peer.commits == []
    if guard in ("unknown-purpose", "password-role", "protected-attribute", "excluded-app", "unobservable-domain"):
        assert peer.interfaces == 0


@pytest.mark.parametrize("race", ["revision", "focus", "protection"])
def test_capture_revalidates_native_and_accessible_state_after_value_read(native, race):
    bridge, peer = native
    def change():
        if race == "revision":
            peer.revision += 1
        elif race == "focus":
            peer.field.flags.remove("focused")
        else:
            peer.field.attributes = ["text-input-type:password"]
    peer.after_read = change
    assert bridge.capture() is None
    assert peer.reads == 1 and peer.commits == []


@pytest.mark.parametrize("name,key,modifiers,action,handled", [
    (b"Tab", 23, 0, "word", False),
    (b"Tab", 23, 0, "word", True),
    (b"ISO_Left_Tab", 23, 1, None, False),
    (b"Tab", 23, 8, None, False),
    (b"Tab", 23, 128, None, False),
    (b"1", 10, 5, "all", True),
])
def test_native_key_consumption_preserves_modifiers_and_false_callbacks(native, name, key, modifiers, action, handled):
    # R3: callback False preserves the original key; reserved/AltGr never dispatch.
    bridge, peer = native
    calls = []
    bridge.configure(Config(shortcuts={"word": "Tab", "all": "Ctrl+Shift+1"}))
    bridge.start_shortcuts(lambda value: calls.append(value) or handled)
    bridge.set_shortcuts_enabled(True)
    result = event(bridge, type=2, state=1, generation=peer.revision,
                   serial=peer.serial, name=name, key=key, modifiers=modifiers)
    assert result == int(handled)
    assert calls == ([action] if action else [])
    assert peer.text == "Dear team, the update" and peer.commits == []


def test_inactive_sensitive_and_old_generation_keys_never_call_python_consumer(native):
    bridge, peer = native
    calls = []
    bridge.configure(Config(shortcuts={"word": "Tab"}))
    bridge.start_shortcuts(lambda action: calls.append(action) or True)
    for enabled, ready, generation in ((False, True, peer.revision),
                                       (True, False, peer.revision),
                                       (True, True, peer.revision - 1)):
        bridge.set_shortcuts_enabled(enabled)
        peer.ready = ready
        assert event(bridge, type=2, state=1, generation=generation, name=b"Tab", key=23) == 0
    assert calls == [] and peer.commits == []


def test_native_controller_consumes_keys_only_for_confirmed_text(native, tmp_path, monkeypatch):
    # R2/R3: real key routing, Controller and bridge, not an acceptance callback echo.
    bridge, peer = native
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    controller = Controller(Config(record_history=False, shortcuts={"word": "Tab", "all": "AboveTab"}), bridge)
    controller.start()
    def press(name=b"Tab", key=23):
        return event(bridge, type=2, state=1, generation=peer.revision,
                     serial=peer.serial, name=name, key=key)
    try:
        assert press() == 0
        assert peer.text == "Dear team, the update" and peer.commits == []
        snapshot = bridge.capture()
        assert controller._suggestion.offer(controller._suggestion.generation, snapshot, " arrives tomorrow")
        assert press() == 1
        assert peer.text == "Dear team, the update arrives "
        assert controller._suggestion.text == "tomorrow"
        assert press(b"grave", 49) == 1
        assert peer.text == "Dear team, the update arrives tomorrow"
        assert peer.commits == [" arrives ", "tomorrow"]
        assert press() == 0
        assert peer.text == "Dear team, the update arrives tomorrow"
        assert controller.stats()["totals"]["characters"] == len(" arrives tomorrow")
    finally:
        controller.stop()


def test_filtered_native_commit_leaves_tab_unconsumed_and_statistics_empty(native, tmp_path, monkeypatch):
    # R1/R3: successful wire submission is not application-level acceptance.
    bridge, peer = native
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    peer.accept_commits = False
    controller = Controller(Config(record_history=False, shortcuts={"word": "Tab"}), bridge)
    controller.start()
    try:
        snapshot = bridge.capture()
        assert controller._suggestion.offer(controller._suggestion.generation, snapshot, " arrives tomorrow")
        assert event(bridge, type=2, state=1, generation=peer.revision,
                     serial=peer.serial, name=b"Tab", key=23) == 0
        assert peer.text == "Dear team, the update"
        assert peer.commits == [" arrives "]
        assert controller._suggestion.text == ""
        assert controller.stats()["totals"] == {"completions": 0, "words": 0, "characters": 0}
        assert event(bridge, type=2, state=1, generation=peer.revision,
                     serial=peer.serial, name=b"Tab", key=23) == 0
        assert peer.commits == [" arrives "]
    finally:
        controller.stop()


@pytest.fixture
def qt_app(monkeypatch):
    monkeypatch.setenv("QT_QPA_PLATFORM", "offscreen")
    widgets = pytest.importorskip("PySide6.QtWidgets")
    app = widgets.QApplication.instance() or widgets.QApplication([])
    from PySide6.QtCore import QRect
    screen = SimpleNamespace(availableGeometry=lambda: QRect(0, 0, 640, 240),
                             devicePixelRatio=lambda: 1)
    monkeypatch.setattr(widgets.QApplication, "primaryScreen", lambda: screen)
    yield app


def click_region(bridge, peer, action):
    region = next(rect for rect, name, enabled in bridge._regions if name == action and enabled)
    x, y, _, _ = region
    for pressed in (1, 0):
        event(bridge, type=3, generation=peer.revision, serial=peer.serial,
              key=272, state=pressed, x=x + 1, y=y + 1)


def test_native_busy_buffers_deliver_latest_frame_without_stale_hit_actions(native, qt_app):
    # R5: pending frames disable previous hit regions until compositor release.
    bridge, peer = native
    calls = []
    bridge.configure(Config(shortcuts={"word": "Tab", "all": "AboveTab"}))
    bridge.start_shortcuts(lambda action: calls.append(action) or True)
    bridge.set_shortcuts_enabled(True)
    anchor = bridge.capture().native_anchor
    assert bridge.present_native(anchor, " first candidate")
    stale = next(rect for rect, name, active in bridge._regions if name == "all")
    first_frame = peer.frames[-1]
    peer.present_result = 2
    assert bridge.present_native(anchor, " intermediate candidate")
    assert bridge.present_native(anchor, " final accepted candidate")
    assert bridge._regions == []
    x, y, _, _ = stale
    for pressed in (1, 0):
        event(bridge, type=3, generation=peer.revision, serial=peer.serial,
              key=272, state=pressed, x=x + 1, y=y + 1)
    assert calls == []
    final_frame = peer.frames[-1]
    assert first_frame != final_frame
    peer.present_result = 1
    event(bridge, type=5)
    assert peer.frames[-1] == final_frame
    assert not bridge._pending_render
    click_region(bridge, peer, "all")
    assert calls == ["all"]


def test_native_menu_pages_skip_disabled_items_and_restore_shortcuts_on_close(native, qt_app):
    # R5/R3: off-page enabled actions remain reachable; Escape is not acceptance.
    bridge, peer = native
    calls = []
    bridge.configure(Config(shortcuts={"word": "Tab"}))
    bridge.bind_ui(calls.append)
    bridge.start_shortcuts(lambda action: calls.append(action) or True)
    bridge.set_shortcuts_enabled(True)
    assert bridge.present_native(bridge.capture().native_anchor, " candidate")
    actions = [(f"item:{index}", f"Synthetic action {index}", index % 2 == 0) for index in range(12)]
    assert bridge.show_native_menu(actions)
    assert event(bridge, type=2, state=1, generation=peer.revision, name=b"End") == 1
    assert bridge._menu_page > 0
    assert event(bridge, type=2, state=1, generation=peer.revision, name=b"Return") == 1
    assert calls == ["item:10"]
    assert bridge._menu is None and bridge._panel_snapshot is None
    assert bridge.present_native(bridge.capture().native_anchor, " candidate")
    assert bridge.show_native_menu(actions)
    assert event(bridge, type=2, state=1, generation=peer.revision, name=b"Escape") == 1
    assert calls == ["item:10", "menu_closed"]
    assert event(bridge, type=2, state=1, generation=peer.revision, name=b"Tab", key=23) == 1
    assert calls == ["item:10", "menu_closed", "word"]
    assert peer.commits == []


@pytest.mark.parametrize("phase", [1, 2])
@pytest.mark.parametrize("transition", ["ready", "inactive", "unsupported", "focus-changed", "sensitive", "stopped"])
def test_initial_context_resync_preserves_capture_isolation(native, qt_app, monkeypatch, transition, phase):
    from PySide6 import QtDBus
    bridge, peer = native
    peer.ready = False
    bridge._needs_context_state = phase
    assert bridge.capture() is None and peer.reads == 0
    pending = []

    class Signal:
        def connect(self, callback):
            self.callback = callback

    class Watcher:
        def __init__(self, message, parent):
            self.message = message
            self.finished = Signal()
            pending.append(self)

        def reply(self):
            return SimpleNamespace(type=lambda: QtDBus.QDBusMessage.MessageType.ReplyMessage,
                                   arguments=lambda: self.arguments)

        def deleteLater(self):
            pass

    bus = SimpleNamespace(isConnected=lambda: True, asyncCall=lambda message, timeout: message)
    monkeypatch.setattr(QtDBus.QDBusConnection, "sessionBus", lambda: bus)
    monkeypatch.setattr(QtDBus, "QDBusPendingCallWatcher", Watcher)
    bridge._sync_context_state()
    replies = 0
    while pending:
        watcher = pending.pop(0)
        if replies == 0:
            if transition == "focus-changed":
                peer.context += 1
            elif transition == "sensitive":
                bridge._needs_context_state = False
            elif transition == "stopped":
                bridge.stop()
        arguments = watcher.message.arguments()
        if watcher.message.member() == "Set":
            # The external peer is permissive: production must prevent activation
            # after focus loss, teardown, or an ineligible compositor response.
            watcher.arguments = []
            if bridge._needs_context_state == 1:
                # Purpose arrives before safe surrounding text can be retained.
                event(bridge, type=1, state=2)
            else:
                peer.ready = True
                event(bridge, type=1, state=0)
        else:
            key = arguments[1]
            value = not ((key == "active" and transition == "inactive")
                         or (key == "activeClientSupportsTextInput" and transition == "unsupported"))
            watcher.arguments = [QtDBus.QDBusVariant(value)]
        replies += 1
        watcher.finished.callback(watcher)
        qt_app.processEvents()

    captured = bridge.capture()
    if transition == "ready":
        assert captured is not None and captured.text == peer.text and captured.native_anchor
    else:
        assert not peer.ready and captured is None and peer.reads == 0
