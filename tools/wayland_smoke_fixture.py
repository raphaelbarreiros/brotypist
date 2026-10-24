#!/usr/bin/env python3
"""Private helpers for smoke_wayland.py, not a shipped application test hook.

Protocol references (requests are sent only to the owned virtual socket):
https://wayland.freedesktop.org/docs/html/ch04.html
https://github.com/KDE/plasma-wayland-protocols/blob/master/src/protocols/fake-input.xml
https://github.com/KDE/kwin/blob/master/src/backends/fakeinput/fakeinputbackend.cpp
https://github.com/KDE/kwin/blob/master/src/scripting/scripting.cpp

KWin may deny fake input. Authentication uses this fixture's own name, never
Orca or a privileged desktop identity. A denied/missing protocol is incomplete
proof, not a successful keyboard test or an excuse to use QTest instead.
"""
from __future__ import annotations

import ctypes
from dataclasses import asdict
import json
import os
from pathlib import Path
import re
import shlex
import socket
import stat
import struct
import subprocess
import sys
import time
import traceback

INTERFACE = "org.inklow.WaylandSmoke"
IM_SERVICE = INTERFACE + ".IM"
TARGET_SERVICE = INTERFACE + ".Target"
SOCKET = "inklow-smoke"
REFUSED = " FORBIDDEN-NATIVE-COMMIT"
PREFIX = "The three steps for planting a tree are to"


class ProofUnavailable(RuntimeError):
    pass


def isolation(*, display: bool = False) -> tuple[Path, Path]:
    root = Path(os.environ["INKLOW_SMOKE_ROOT"]).resolve(strict=True)
    marker = json.loads((root / "owner.json").read_text())
    if marker != {"token": os.environ.get("INKLOW_SMOKE_TOKEN"), "uid": os.getuid()}:
        raise RuntimeError("Not an owned smoke environment")
    if root.stat().st_uid != os.getuid() or stat.S_IMODE(root.stat().st_mode) != 0o700:
        raise RuntimeError("Smoke root ownership/mode is not private")
    for key in ("HOME", "XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_CONFIG_DIRS",
                "XDG_DATA_HOME", "XDG_CACHE_HOME", "INKLOW_HOME"):
        Path(os.environ[key]).resolve(strict=True).relative_to(root)
    if not os.environ.get("DBUS_SESSION_BUS_ADDRESS") or os.environ.get("DISPLAY"):
        raise RuntimeError("Missing private session bus, or inherited X11 display")
    if display:
        name = os.environ.get("WAYLAND_DISPLAY")
        if name != SOCKET or not stat.S_ISSOCK((root / "runtime" / name).stat().st_mode):
            raise RuntimeError("Only the owned virtual KWin socket is allowed")
    return root, Path(os.environ["INKLOW_SMOKE_OUTPUT"])


def session() -> int:
    root, output = isolation()
    fixture = Path(__file__).resolve()
    for role in ("im", "target"):
        launcher = root / (role + ".sh")
        launcher.write_text("#!/bin/sh\nexec " + shlex.join([sys.executable, str(fixture), role]) + "\n")
        launcher.chmod(0o700)
    command = [os.environ["INKLOW_SMOKE_KWIN"], "--virtual", "--width", "1280",
               "--height", "800", "--socket", SOCKET, "--no-lockscreen",
               "--no-global-shortcuts", "--no-kactivities", "--inputmethod",
               str(root / "im.sh"), "--exit-with-session", str(root / "target.sh")]
    (output / "kwin-command.json").write_text(json.dumps(command, indent=2) + "\n")
    # KWin chooses its own QPA integration. Its clients explicitly use wayland.
    env = dict(os.environ)
    env.pop("QT_QPA_PLATFORM", None)
    with (output / "kwin.log").open("wb") as log:
        process = subprocess.Popen(command, env=env, stdout=log, stderr=subprocess.STDOUT)
        try:
            return process.wait(timeout=int(env["INKLOW_SMOKE_TIMEOUT"]) - 5)
        except subprocess.TimeoutExpired:
            process.terminate()
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=2)
            return 1


class FakeInput:
    """Small real Wayland wire client, no compiler or Python protocol package.

    Only wl_display, wl_registry and org_kde_kwin_fake_input v4 are used; these
    have no FD-bearing events. IDs are never reused, and each operation has a
    finite display.sync roundtrip. Input delivery still needs a target oracle.
    """
    def __init__(self):
        root, _ = isolation(display=True)
        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.socket.settimeout(2)
        self.socket.connect(str(root / "runtime" / SOCKET))
        self.next_id = 3
        self.globals = {}
        self.send(1, 1, struct.pack("=I", 2))  # wl_display.get_registry
        self.sync()
        advertised = self.globals.get("org_kde_kwin_fake_input")
        if advertised is None or advertised[1] < 4:
            self.close()
            raise ProofUnavailable("Owned KWin did not advertise fake-input keyboard v4")
        self.proxy = self.allocate()
        self.send(2, 0, struct.pack("=I", advertised[0]) + self.string("org_kde_kwin_fake_input")
                  + struct.pack("=II", 4, self.proxy))
        self.send(self.proxy, 0, self.string("Inklow isolated Wayland smoke")
                  + self.string("Synthetic input into our owned virtual compositor only"))
        self.sync()

    def allocate(self):
        identifier = self.next_id
        self.next_id += 1
        return identifier

    @staticmethod
    def string(text):
        data = text.encode() + b"\0"
        return struct.pack("=I", len(data)) + data + b"\0" * (-len(data) % 4)

    def send(self, identifier, opcode, payload=b""):
        self.socket.sendall(struct.pack("=II", identifier, ((8 + len(payload)) << 16) | opcode) + payload)

    def receive(self, count):
        result = bytearray()
        while len(result) < count:
            data = self.socket.recv(count - len(result))
            if not data:
                raise ProofUnavailable("Owned Wayland fixture connection closed")
            result.extend(data)
        return bytes(result)

    def sync(self):
        callback = self.allocate()
        self.send(1, 0, struct.pack("=I", callback))
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            identifier, word = struct.unpack("=II", self.receive(8))
            size, opcode = word >> 16, word & 0xffff
            if size < 8 or size > 65535 or size % 4:
                raise RuntimeError("Invalid Wayland event size")
            payload = self.receive(size - 8)
            if identifier == 1 and opcode == 0:
                raise ProofUnavailable("Owned compositor returned a Wayland protocol error")
            if identifier == 2 and opcode == 0:
                name, length = struct.unpack_from("=II", payload)
                interface = payload[8:8 + length - 1].decode()
                version = struct.unpack_from("=I", payload, 8 + ((length + 3) & ~3))[0]
                self.globals[interface] = name, version
            if identifier == callback and opcode == 0:
                return
        raise ProofUnavailable("Owned Wayland sync deadline expired")

    def key(self, code, pressed):
        self.send(self.proxy, 10, struct.pack("=II", code, int(pressed)))

    def chord(self, code, modifiers=()):
        for modifier in modifiers:
            self.key(modifier, True)
        self.key(code, True)
        self.key(code, False)
        for modifier in reversed(modifiers):
            self.key(modifier, False)
        self.sync()

    def close(self):
        self.socket.close()


def im() -> int:
    _, output = isolation(display=True)
    os.environ["QT_QPA_PLATFORM"] = "wayland"
    # Importing Qt classes is fine; constructing QApplication before production
    # prepare_input_method is not. Record this ordering inside the observer.
    from PySide6.QtCore import ClassInfo, QObject, QTimer, Slot
    from PySide6.QtDBus import QDBusConnection
    from PySide6.QtGui import QImage
    from PySide6.QtWidgets import QApplication
    from inklow import qt_ui
    from inklow.__main__ import main as production_main

    inherited_fd = os.environ.get("WAYLAND_SOCKET")
    if inherited_fd is None or QApplication.instance() is not None:
        raise RuntimeError("KWin did not supply a private fd before QApplication")
    fd_number = int(inherited_fd)
    fd_socket = stat.S_ISSOCK(os.fstat(fd_number).st_mode)
    original_init, original_start = qt_ui.MenuAgent.__init__, qt_ui.MenuAgent.start

    @ClassInfo(**{"D-Bus Interface": INTERFACE})
    class Observer(QObject):
        def __init__(self, agent):
            super().__init__(agent.app)
            self.agent = agent
            self.saved = {}
            self.calls = []
            self.frames = []
            self.decisions = []
            self.worker = None
            self.entry = {"private_fd_received": fd_socket,
                          "private_fd_environment_removed": "WAYLAND_SOCKET" not in os.environ,
                          "qt_exists_after_preparation": QApplication.instance() is not None,
                          "bridge": type(agent.controller.bridge).__name__}
            bridge = agent.controller.bridge
            original_call = bridge._native_call

            def observe_call(name, *arguments):
                # Copy the actual production frame before it goes out of scope;
                # never generate an imitation prediction or alter the result.
                image = None
                if name == "present":
                    _, address, width, height, stride, scale = arguments
                    data = ctypes.string_at(address, stride * height)
                    image = QImage(data, width, height, stride,
                                   QImage.Format.Format_ARGB32_Premultiplied).copy()
                    image.setDevicePixelRatio(scale)
                result = original_call(name, *arguments)
                if image is not None:
                    image.save(str(output / "panel-latest.png"))
                    self.frames.append({"result": int(result), "width": width, "height": height,
                                        "kind": "production-client-pixels-not-screen-capture"})
                    self.frames[:] = self.frames[-64:]
                if name == "commit":
                    self.calls.append({"anchor": f"{arguments[1]}:{arguments[2]}",
                                       "text": arguments[3].decode("utf-8"), "result": int(result)})
                return result

            bridge._native_call = observe_call
            original_invoke = agent.controller.invoke

            def observe_invoke(action):
                result = original_invoke(action)
                self.decisions.append({"action": action, "accepted": bool(result)})
                self.decisions[:] = self.decisions[-128:]
                return result

            agent.controller.invoke = observe_invoke

        def state(self):
            controller = self.agent.controller
            bridge = controller.bridge
            snapshot = bridge.capture()
            status = controller.status()
            process = controller._client._process or controller._client._terminating
            if process is not None:
                self.worker = process
            return {"entry": self.entry, "status": status,
                    "native_state": bridge._state(),
                    "snapshot": asdict(snapshot) if snapshot else None,
                    "suggestion": controller._suggestion.text,
                    "suggestion_snapshot": asdict(controller._suggestion.snapshot) if controller._suggestion.snapshot else None,
                    "panel": {"text": bridge._panel_text, "pending": bridge._pending_render,
                              "anchor": bridge._panel_snapshot.native_anchor if bridge._panel_snapshot else "",
                              "menu": bridge._menu is not None, "rows": bridge._menu,
                              "selected": bridge._menu_selected, "page": bridge._menu_page},
                    "frames": self.frames, "commits": self.calls, "decisions": self.decisions,
                    "pid": os.getpid()}

        @Slot(str, result=str)
        def request(self, payload):
            try:
                data = json.loads(payload)
                if data.pop("token", None) != os.environ["INKLOW_SMOKE_TOKEN"]:
                    raise RuntimeError("Invalid private fixture token")
                action = data["op"]
                result = None
                if action == "state":
                    result = self.state()
                elif action == "invoke":
                    result = self.agent.controller.invoke(data["action"])
                elif action == "save":
                    snapshot = self.agent.controller.bridge.capture()
                    if snapshot is None:
                        raise RuntimeError("No eligible real native snapshot to save")
                    self.saved[data["name"]] = snapshot
                    result = asdict(snapshot)
                elif action == "insert_saved":
                    result = self.agent.controller.bridge.insert(self.saved[data["name"]], REFUSED)
                elif action == "menu":
                    self.agent._native_action("menu")
                    result = self.agent._native_menu_active
                elif action == "menu_close":
                    self.agent._native_action("menu_closed")
                    result = not self.agent._native_menu_active
                elif action == "stop":
                    self.agent.stop()
                    process = self.worker
                    result = {"status": self.agent.controller.status(),
                              "worker_pid": process.pid if process else None,
                              "worker_returncode": process.poll() if process else None,
                              "native_client_closed": self.agent.controller.bridge._client is None,
                              "read_notifier_removed": self.agent.controller.bridge._read_notifier is None}
                    (output / "im-stop.json").write_text(json.dumps(result, indent=2) + "\n")
                    QTimer.singleShot(150, self.agent.app.quit)
                else:
                    raise ValueError("Unknown fixture operation")
                return json.dumps({"ok": True, "value": result})
            except Exception as error:
                return json.dumps({"ok": False, "error": f"{type(error).__name__}: {error}"})

    def observed_init(agent, *args, **kwargs):
        original_init(agent, *args, **kwargs)
        observer = Observer(agent)
        bus = QDBusConnection.sessionBus()
        if not bus.registerService(IM_SERVICE) or not bus.registerObject(
                "/Smoke", observer, QDBusConnection.RegisterOption.ExportAllSlots):
            raise RuntimeError("Cannot register observer on private bus")
        agent._smoke_observer = observer

    def observed_start(agent, *args, **kwargs):
        original_start(agent, *args, **kwargs)
        if agent.settings is not None:
            agent.settings.hide()  # Only our isolated app's settings window.

    qt_ui.MenuAgent.__init__ = observed_init
    qt_ui.MenuAgent.start = observed_start
    return production_main(["--wayland-input-method", "--model-path", os.environ["INKLOW_SMOKE_MODEL"],
                            "--acceleration", "cpu", "--background", "--no-tray"])


def target() -> int:
    root, output = isolation(display=True)
    os.environ["QT_QPA_PLATFORM"] = "wayland"
    from PySide6.QtCore import ClassInfo, QEvent, QObject, Qt, QTimer, Slot
    from PySide6.QtDBus import QDBus, QDBusConnection, QDBusMessage
    from PySide6.QtWidgets import QApplication, QLabel, QLineEdit, QVBoxLayout, QWidget

    app = QApplication(["inklow-wayland-smoke-target"])
    app.setApplicationName("Inklow synthetic Wayland target")
    app.setDesktopFileName("org.inklow.WaylandSmoke.Target")
    app.setQuitOnLastWindowClosed(False)
    report = {"result": "failed", "checks": [], "limits": [], "scenes": [],
              "injection": "not-yet-proven", "target_pid": os.getpid(),
              "image_semantics": "Widget/panel PNGs are generated client pixels; mapped inputMethod scene rows are separate compositor evidence"}
    bus = QDBusConnection.sessionBus()
    events = []
    commits = []

    class Field(QLineEdit):
        def event(self, event):
            if event.type() in (QEvent.Type.KeyPress, QEvent.Type.KeyRelease):
                events.append({"field": self.objectName(), "type": "press" if event.type() == QEvent.Type.KeyPress else "release",
                               "key": int(event.key()), "modifiers": int(event.modifiers().value),
                               "text": event.text(), "scan": event.nativeScanCode(),
                               "autorepeat": event.isAutoRepeat()})
                # Keep test focus fixed while recording genuine compositor Tab
                # delivery. No QTest/sendEvent/direct shortcut calls are used.
                if event.key() in (Qt.Key.Key_Tab, Qt.Key.Key_Backtab):
                    return True
            return super().event(event)

        def inputMethodEvent(self, event):
            if event.commitString():
                commits.append({"field": self.objectName(), "text": event.commitString(),
                                "replace_start": event.replacementStart(),
                                "replace_length": event.replacementLength()})
            super().inputMethodEvent(event)

    window = QWidget()
    window.setWindowTitle("Inklow synthetic Wayland target")
    window.resize(900, 300)
    layout = QVBoxLayout(window)
    layout.addWidget(QLabel("Disposable synthetic fields only — no personal applications"))
    fields = []
    for name in ("writing", "other"):
        field = Field()
        field.setObjectName(name)
        field.setAccessibleName("Synthetic " + name)
        layout.addWidget(field)
        fields.append(field)
    writing, other = fields
    window.show()
    window.activateWindow()
    writing.setFocus(Qt.FocusReason.OtherFocusReason)

    @ClassInfo(**{"D-Bus Interface": INTERFACE})
    class SceneSink(QObject):
        @Slot(str, result=str)
        def Scene(self, payload):
            value = json.loads(payload)
            report["scenes"].append(value)
            return "recorded"

    sink = SceneSink(app)
    if not bus.registerService(TARGET_SERVICE) or not bus.registerObject(
            "/Smoke", sink, QDBusConnection.RegisterOption.ExportAllSlots):
        raise RuntimeError("Cannot register target scene sink on private bus")

    def bus_call(service, path, interface, method, *arguments):
        message = QDBusMessage.createMethodCall(service, path, interface, method)
        message.setArguments(list(arguments))
        # The observer queries this target's AT-SPI connection while replying.
        # Keep its GUI/D-Bus events running instead of deadlocking the oracle.
        reply = bus.call(message, QDBus.CallMode.BlockWithGui, 1500)
        if reply.type() == QDBusMessage.MessageType.ErrorMessage:
            raise RuntimeError(f"{service}.{method}: {reply.errorName()}: {reply.errorMessage()}")
        return reply.arguments()

    def rpc(op, **arguments):
        payload = {"token": os.environ["INKLOW_SMOKE_TOKEN"], "op": op, **arguments}
        reply = json.loads(bus_call(IM_SERVICE, "/Smoke", INTERFACE, "request", json.dumps(payload))[0])
        if not reply["ok"]:
            raise RuntimeError(reply["error"])
        return reply["value"]

    def state_or_none():
        try:
            return rpc("state")
        except RuntimeError:
            return None

    def check(name, condition, **evidence):
        report["checks"].append({"name": name, "passed": bool(condition), **evidence})
        if not condition:
            raise AssertionError(name)

    def wait_for(predicate, label, seconds=12):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            value = predicate()
            if value:
                return value
            yield 0.1
        raise AssertionError("Deadline: " + label)

    def eligible(expected, field=writing):
        state = state_or_none()
        if (state and state["snapshot"] and state["snapshot"]["text"] == expected
                and state["snapshot"]["caret"] == len(expected) and field.hasFocus()):
            return state
        return None

    def prepare(text=PREFIX, field=writing):
        writing.setEchoMode(QLineEdit.EchoMode.Normal)
        writing.setInputMethodHints(Qt.InputMethodHint.ImhNone)
        field.setText(text[:-1])
        field.setCursorPosition(len(text) - 1)
        window.activateWindow()
        field.setFocus(Qt.FocusReason.OtherFocusReason)
        app.inputMethod().show()
        # Initial surrounding precedes content_type in KWin. Wait for the
        # safe type, then make a genuine widget edit; never weaken the guard.
        yield 0.4
        field.insert(text[-1:])
        app.inputMethod().update(Qt.InputMethodQuery.ImSurroundingText | Qt.InputMethodQuery.ImCursorPosition | Qt.InputMethodQuery.ImAnchorPosition)
        return (yield from wait_for(lambda: eligible(text, field), "matching safe native + AT-SPI context"))

    def offer():
        check("R6 production force command accepts safe context", rpc("invoke", action="force"))

        def ready():
            state = rpc("state")
            if state["status"]["state"] == "error":
                raise AssertionError(state["status"]["message"])
            if (state["suggestion"] and state["status"]["state"] == "ready"
                    and state["status"]["model_loaded"] and state["status"]["worker_pid"]
                    and state["panel"]["text"] == state["suggestion"]
                    and not state["panel"]["pending"] and state["frames"]
                    and state["frames"][-1]["result"] == 1):
                return state
            return None
        return (yield from wait_for(ready, "real provider offer + attached native frame", 100))

    def scene(label, im_pid):
        before = len(report["scenes"])
        script = root / ("scene-" + label + ".js")
        # The private socket may attribute an input-panel client to KWin's PID.
        # This entire compositor is owned; include its native input-method role.
        script.write_text("""var rows = [];
var owned = %s;
var windows = workspace.stackingOrder;
for (var i = 0; i < windows.length; ++i) {
    var w = windows[i];
    if (owned.indexOf(w.pid) < 0 && !w.inputMethod) continue;
    var g = w.frameGeometry;
    rows.push({pid:w.pid, inputMethod:w.inputMethod, hidden:w.hidden,
               opacity:w.opacity, minimized:w.minimized,
               geometry:[g.x,g.y,g.width,g.height]});
}
callDBus(%s, '/Smoke', %s, 'Scene', JSON.stringify({label:%s, rows:rows}));
""" % (json.dumps([os.getpid(), im_pid]), json.dumps(TARGET_SERVICE),
       json.dumps(INTERFACE), json.dumps(label)))
        loaded = bus_call("org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting", "loadScript",
                          str(script), "inklow-smoke-" + label)
        identifier = int(loaded[0])
        if identifier < 0:
            raise ProofUnavailable("Owned KWin scripting API rejected scene observer")
        path = "/Scripting/Script" + str(identifier)
        try:
            bus_call("org.kde.KWin", path, "org.kde.kwin.Script", "run")
            value = yield from wait_for(lambda: report["scenes"][-1] if len(report["scenes"]) > before else None,
                                        "owned compositor scene callback", 5)
            return value
        finally:
            bus_call("org.kde.KWin", path, "org.kde.kwin.Script", "stop")

    injector = None

    def scenarios():
        nonlocal injector
        initial = yield from wait_for(state_or_none, "KWin-launched production MenuAgent observer", 25)
        check("R6 real entrypoint prepares private fd before Qt", all(initial["entry"].get(key) for key in
              ("private_fd_received", "private_fd_environment_removed", "qt_exists_after_preparation"))
              and initial["entry"]["bridge"] == "WaylandBridge", entry=initial["entry"])
        report["initial_status"] = initial["status"]
        support = str(bus_call("org.kde.KWin", "/KWin", "org.kde.KWin", "supportInformation")[0])
        renderer_lines = [line for line in support.splitlines() if re.search(
            r"renderer|compositing type|kwin version|qt version", line, re.IGNORECASE)]
        check("R6 owned compositor uses software rendering",
              bool(re.search(r"llvmpipe|softpipe|software rasterizer|QPainter", support, re.IGNORECASE)),
              renderer=renderer_lines)
        keyboard_limit = None
        try:
            injector = FakeInput()
            writing.setText("")
            writing.setFocus()
            yield 0.3
            start = len(events)
            injector.chord(30)  # Linux KEY_A, not an XKB/Qt synthetic invocation.
            try:
                yield from wait_for(lambda: writing.text() == "a", "fixture key reaches native Qt target", 3)
            except AssertionError as error:
                raise ProofUnavailable("fake-input advertised but native KEY_A delivery was not established") from error
            delivered = events[start:]
            check("R3 compositor injection calibration", len([e for e in delivered if e["type"] == "press" and e["text"] == "a"]) == 1,
                  text=writing.text(), events=delivered)
            report["injection"] = "real-org_kde_kwin_fake_input-to-owned-KWin-to-native-Qt"
        except (ProofUnavailable, OSError) as error:
            keyboard_limit = str(error)
            report["limits"].append(keyboard_limit)
            report["injection"] = "unproven-no-QTest-fallback"
            if injector is not None:
                injector.close()
            injector = None

        before = yield from prepare()
        check("R6 bridge captures exact synthetic writing", before["snapshot"]["text"] == PREFIX,
              snapshot=before["snapshot"])
        offered = yield from offer()
        report["provider_offer"] = offered
        # Separate production pixels from compositor-owned mapped geometry.
        panel = output / "panel-latest.png"
        if panel.exists():
            (output / "panel-offer.png").write_bytes(panel.read_bytes())
        window.grab().save(str(output / "target-before.png"))
        try:
            mapped = yield from scene("offer", offered["pid"])
            visible = [row for row in mapped["rows"] if row["inputMethod"] and not row["hidden"]
                       and not row["minimized"] and row["opacity"] > 0
                       and row["geometry"][2] > 0 and row["geometry"][3] > 0]
            check("R6 compositor maps input-method panel", bool(visible), scene=mapped,
                  limit="Mapped scene metadata is not a full compositor screenshot")
        except (RuntimeError, ProofUnavailable) as error:
            report["limits"].append("Mapped scene evidence unavailable: " + str(error))

        # R5: actual compositor keyboard-grab navigation, not _menu_key calls.
        check("R5 production native menu opens", rpc("menu"))
        menu_state = yield from wait_for(lambda: rpc("state") if rpc("state")["panel"]["menu"]
                                         and not rpc("state")["panel"]["pending"] else None,
                                         "attached real native menu")
        if panel.exists():
            (output / "panel-menu.png").write_bytes(panel.read_bytes())
        if injector is not None:
            enabled = [index for index, row in enumerate(menu_state["panel"]["rows"]) if row[2]]
            check("R5 native menu exposes actionable entries", len(enabled) > 1)
            for code, expected in ((107, enabled[-1]), (102, enabled[0]), (108, enabled[1]), (103, enabled[0])):
                injector.chord(code)  # Linux End, Home, Down, Up.
                yield from wait_for(lambda expected=expected: rpc("state")["panel"]["selected"] == expected,
                                    "native menu selection follows compositor key")
            injector.chord(1)  # KEY_ESC closes the menu, not the input context.
        else:
            check("R5 direct menu close (not keyboard proof)", rpc("menu_close"))
        yield from wait_for(lambda: not rpc("state")["panel"]["menu"], "native menu dismissed")
        check("R5 dismissal preserves offered completion", rpc("state")["suggestion"] == offered["suggestion"])

        snapshot = offered["snapshot"]
        suggestion = offered["suggestion"]
        first_match = re.match(r"\s*\S+\s*", suggestion)
        check("R2 provider produced an actual word", first_match is not None, offered=suggestion)
        word = first_match.group()
        remaining = suggestion[len(word):]
        if not remaining and word and not word[-1].isspace() and any(c.isalnum() for c in word):
            word += " "
        event_start, im_start = len(events), len(commits)
        if injector is not None:
            injector.chord(15)  # KEY_TAB: acceptance must consume both edges.
        else:
            check("R6 direct Controller word invocation (not keyboard proof)", rpc("invoke", action="word"))
        yield from wait_for(lambda: writing.text() == PREFIX + word, "exact first native word commit")
        yield 0.25
        check("R6 actual Qt input-method commit equals offered word", commits[im_start:] == [
              {"field": "writing", "text": word, "replace_start": 0, "replace_length": 0}],
              expected=PREFIX + word, actual=writing.text(), input_method_events=commits[im_start:])
        if injector is not None:
            check("R3 consumed Tab does not reach target", not [e for e in events[event_start:]
                  if e["key"] in (int(Qt.Key.Key_Tab), int(Qt.Key.Key_Backtab))], events=events[event_start:])
        advanced = yield from wait_for(lambda: eligible(PREFIX + word), "postcommit native revision")
        check("R2 same field survives native revision", advanced["snapshot"]["field_id"] == snapshot["field_id"]
              and advanced["snapshot"]["native_anchor"] != snapshot["native_anchor"],
              before=snapshot, after=advanced["snapshot"])
        if remaining:
            yield from wait_for(lambda: rpc("state")["suggestion"] == remaining, "remaining offer survives native revision")
            check("R2 full remaining acceptance", rpc("invoke", action="all"))
            yield from wait_for(lambda: writing.text() == PREFIX + word + remaining, "exact continued native completion")
            check("R2 full resulting synthetic string", writing.text() == PREFIX + word + remaining,
                  expected=PREFIX + word + remaining, actual=writing.text())
        else:
            report["limits"].append("Real model supplied one word only; repeated-acceptance remainder was not exercisable")
        window.grab().save(str(output / "target-after.png"))

        # R1/R4: each saved snapshot comes from a real compositor revision.
        yield from prepare("Synthetic stale revision boundary")
        saved = rpc("save", name="stale")
        writing.insert(" changed")
        yield from wait_for(lambda: eligible("Synthetic stale revision boundary changed"), "edited revision")
        start = len(commits)
        refused = rpc("insert_saved", name="stale")
        yield 0.2
        check("R1 stale revision refuses before actual commit", refused is False and not commits[start:]
              and writing.text() == "Synthetic stale revision boundary changed", saved=saved,
              actual=writing.text())

        yield from prepare(PREFIX)
        rpc("save", name="selection")
        # Keep a real active offer while changing the native selection.
        yield from offer()
        writing.setSelection(0, 9)
        app.inputMethod().update(Qt.InputMethodQuery.ImSurroundingText | Qt.InputMethodQuery.ImCursorPosition | Qt.InputMethodQuery.ImAnchorPosition | Qt.InputMethodQuery.ImCurrentSelection)
        yield from wait_for(lambda: rpc("state")["snapshot"] is None, "native selected-text refusal")
        start = len(commits)
        refused = rpc("insert_saved", name="selection")
        accepted = rpc("invoke", action="all")
        yield 0.2
        check("R1 selection refuses capture and acceptance", refused is False and accepted is False
              and not commits[start:] and writing.text() == PREFIX
              and writing.selectedText() == PREFIX[:9], actual=writing.text(), selected=writing.selectedText())

        yield from prepare(PREFIX)
        rpc("save", name="protected")
        yield from offer()
        writing.setEchoMode(QLineEdit.EchoMode.Password)
        writing.setInputMethodHints(Qt.InputMethodHint.ImhHiddenText | Qt.InputMethodHint.ImhSensitiveData
                                    | Qt.InputMethodHint.ImhNoPredictiveText)
        app.inputMethod().update(Qt.InputMethodQuery.ImEnabled | Qt.InputMethodQuery.ImHints)
        # Qt can stop reporting IM updates for password controls. AT-SPI must
        # independently exclude them even while the last native revision exists.
        yield from wait_for(lambda: rpc("state")["snapshot"] is None,
                            "real password control refuses capture")
        start = len(commits)
        refused = rpc("insert_saved", name="protected")
        accepted = rpc("invoke", action="all")
        forced = rpc("invoke", action="force")
        yield 0.2
        protected = rpc("state")
        check("R1 real password refuses capture, commit and generation", refused is False and accepted is False
              and forced is False and not commits[start:] and writing.text() == PREFIX
              and not protected["panel"]["anchor"] and not protected["suggestion"], state=protected)

        # Exercise native sensitive-data rejection on re-entering an ordinary
        # editable control; password exclusion above is independently AT-SPI-gated.
        yield from prepare(PREFIX)
        rpc("save", name="sensitive")
        yield from offer()
        writing.setInputMethodHints(Qt.InputMethodHint.ImhSensitiveData | Qt.InputMethodHint.ImhNoPredictiveText)
        app.inputMethod().update(Qt.InputMethodQuery.ImHints)
        report["sensitive_target"] = {"hints": int(writing.inputMethodHints().value),
                                     "query_hints": int(writing.inputMethodQuery(Qt.InputMethodQuery.ImHints))}
        other.setFocus(Qt.FocusReason.OtherFocusReason)
        yield 0.3
        writing.setFocus(Qt.FocusReason.OtherFocusReason)
        app.inputMethod().show()
        yield from wait_for(lambda: rpc("state")["native_state"] is None and rpc("state")["snapshot"] is None,
                            "native sensitive-data hint clears readiness")
        start = len(commits)
        refused = rpc("insert_saved", name="sensitive")
        accepted = rpc("invoke", action="all")
        forced = rpc("invoke", action="force")
        yield 0.2
        sensitive = rpc("state")
        check("R1 native sensitive hint refuses capture, commit and generation",
              refused is False and accepted is False and forced is False
              and not commits[start:] and writing.text() == PREFIX
              and not sensitive["panel"]["anchor"] and not sensitive["suggestion"], state=sensitive)
        if injector is not None:
            start = len(events)
            injector.chord(15)
            yield 0.3
            delivered = [e for e in events[start:] if e["key"] in (int(Qt.Key.Key_Tab), int(Qt.Key.Key_Backtab))]
            check("R3 sensitive transition forwards original Tab exactly once", [e["type"] for e in delivered] == ["press", "release"],
                  events=delivered)

        yield from prepare("Synthetic original forwarding boundary")
        rpc("invoke", action="dismiss")
        if injector is not None:
            for label, modifiers, expected_modifier in (("Tab", (), 0), ("Shift+Tab", (42,), int(Qt.KeyboardModifier.ShiftModifier.value))):
                start = len(events)
                injector.chord(15, modifiers)
                yield 0.3
                delivered = [e for e in events[start:] if e["key"] in (int(Qt.Key.Key_Tab), int(Qt.Key.Key_Backtab))]
                check("R3 inactive " + label + " forwards once with modifiers", [e["type"] for e in delivered] == ["press", "release"]
                      and all(e["modifiers"] == expected_modifier for e in delivered), events=delivered)

        yield from prepare("Synthetic focus context refusal boundary")
        saved = rpc("save", name="focus")
        other_before = yield from prepare("Synthetic other focused native field", other)
        start = len(commits)
        refused = rpc("insert_saved", name="focus")
        yield 0.2
        check("R1 old native field context cannot commit to new focus", refused is False and not commits[start:]
              and other.text() == "Synthetic other focused native field"
              and writing.text() == "Synthetic focus context refusal boundary"
              and other_before["snapshot"]["field_id"] != saved["field_id"],
              before=saved, after=other_before["snapshot"])
        report["final_native_state"] = rpc("state")
        stopped = rpc("stop")
        check("R4 owned worker and native client stop", stopped["native_client_closed"]
              and stopped["read_notifier_removed"] and stopped["status"]["worker_pid"] is None
              and stopped["worker_pid"] is not None and stopped["worker_returncode"] is not None,
              stop=stopped)
        report["result"] = "incomplete" if report["limits"] else "passed"

    generator = scenarios()
    finished = False

    def finish(error=None):
        nonlocal finished
        if finished:
            return
        finished = True
        if error is not None:
            report["result"] = "incomplete" if isinstance(error, ProofUnavailable) else "failed"
            report["limits"].append(f"{type(error).__name__}: {error}")
            report["traceback"] = traceback.format_exc()
            try:
                report["failure_native_state"] = rpc("state")
                report["failure_stop"] = rpc("stop")
            except Exception as stop_error:
                report["limits"].append("Observer shutdown: " + str(stop_error))
        if injector is not None:
            injector.close()
        report["target_key_events"] = events
        report["target_input_method_events"] = commits
        (output / "target-report.json").write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n")
        window.close()
        app.exit({"passed": 0, "incomplete": 2}.get(report["result"], 1))

    def step():
        try:
            delay = next(generator)
            QTimer.singleShot(max(1, round(delay * 1000)), step)
        except StopIteration:
            finish()
        except Exception as error:
            finish(error)

    QTimer.singleShot(0, step)
    QTimer.singleShot((int(os.environ["INKLOW_SMOKE_TIMEOUT"]) - 10) * 1000,
                      lambda: finish(TimeoutError("Target's finite whole-run deadline expired")))
    return app.exec()


if __name__ == "__main__":
    roles = {"session": session, "im": im, "target": target}
    if len(sys.argv) != 2 or sys.argv[1] not in roles:
        raise SystemExit("Private fixture; use smoke_wayland.py instead")
    raise SystemExit(roles[sys.argv[1]]())
