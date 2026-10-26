"""Native terminal boundary regressions, with no live terminal or key posting.

The stateful AX fixture exposes bounded physical rows only; it raises if code
requests scrollback/AXValue. Running-code signature, source-version,
cursor/selection, geometry, privacy, process-generation, single-use append and
cancellation are independent oracles. Parent runs these tests and actual
disposable Terminal verification; no test result is implied by these fixtures.
"""
from dataclasses import replace
import ctypes
import os
import sys
from types import SimpleNamespace

import pytest

from inklow.native import macos_terminal as terminal


class Automation:
    def __init__(self):
        self.calls = []
        self.state = "granted"
        self.metadata = dict(window_id=17, tab_id="0", session_id="owned-session",
                             tty="/dev/ttys041", columns=100, rows=32,
                             app_version="3.7.6", job_pid="351")

    def permission(self, app_id, *, request=False):
        self.calls.append(("permission", app_id, request))
        return self.state

    def read_session(self, pid, app_id):
        self.calls.append(("metadata", pid, app_id))
        return dict(self.metadata)


class Bridge:
    def __init__(self):
        self.front = (41, "/Applications/iTerm.app/Contents/MacOS/iTerm2",
                      {"app_id": terminal.ITERM, "domain": ""})
        self.allowed = True
        self.secure = False
        self._dead_key = False
        self.status = []

    def _foreground(self):
        return self.front

    def _allowed(self, *args):
        return self.allowed

    def secure_input_active(self):
        return self.secure

    def _set_status(self, value):
        self.status.append(value)

    def _attribute(self, *args):
        raise AssertionError("Metadata inspection must not read an AX value")


@pytest.fixture
def setup():
    adapter, bridge, automation = terminal.TerminalAdapter(), Bridge(), Automation()
    process = terminal.ProcessIdentity(351, 350, 351, (1729000000, 19), "/opt/bin/codex")
    calls = []
    adapter._automation = automation
    adapter._processes = SimpleNamespace(
        foreground=lambda tty, pid: calls.append((tty, pid)) or process)
    return adapter, bridge, automation, process, calls


def test_native_metadata_alone_does_not_claim_input_ownership(setup):
    adapter, bridge, automation, process, calls = setup
    result = adapter.inspect(bridge, bridge.front)
    assert result.process == process
    assert result.tty == "/dev/ttys041" and result.session_id == "owned-session"
    assert result.app_version == "3.7.6" and result.candidate == "agent"
    assert result.font_name == "" and result.font_size == 0
    assert result.at_shell_prompt is None
    assert calls == [("/dev/ttys041", 351)]
    assert automation.calls == [("permission", terminal.ITERM, False),
                                ("metadata", 41, terminal.ITERM)]
    assert adapter.diagnostic == "metadata-only"


@pytest.mark.parametrize("condition", ["denied", "secure", "composing", "unsupported", "focus"])
def test_privacy_or_changed_foreground_prevents_even_metadata_reads(setup, condition):
    adapter, bridge, automation, _, calls = setup
    foreground = bridge.front
    if condition == "denied":
        bridge.allowed = False
    elif condition == "secure":
        bridge.secure = True
    elif condition == "composing":
        bridge._dead_key = True
    elif condition == "unsupported":
        foreground = (41, "other", {"app_id": "org.example.NotTerminal", "domain": ""})
    else:
        bridge.front = (42, "other", {"app_id": terminal.ITERM, "domain": ""})
    assert adapter.inspect(bridge, foreground) is None
    assert automation.calls == [] and calls == []
    assert bridge.status and "owned-session" not in bridge.status[-1]


@pytest.mark.parametrize("state", ["denied", "not-determined", "unavailable", "not-running"])
def test_permission_denial_never_escalates_to_a_prompt(setup, state):
    adapter, bridge, automation, _, calls = setup
    automation.state = state
    assert adapter.inspect(bridge, bridge.front) is None
    assert automation.calls == [("permission", terminal.ITERM, False)]
    assert not calls
    assert adapter.diagnostic == "automation-" + state


def test_only_explicit_request_is_prompt_capable(setup):
    adapter, _, automation, _, _ = setup
    assert adapter.permission_state(terminal.ITERM) == "granted"
    assert adapter.request_access(terminal.ITERM) == "granted"
    assert automation.calls == [("permission", terminal.ITERM, False),
                                ("permission", terminal.ITERM, True)]


@pytest.mark.parametrize("key,value", [("tty", "/dev/ttys041/../../file"),
                                       ("columns", 0), ("rows", 10001),
                                       ("session_id", ""), ("window_id", -1),
                                       ("job_pid", "351 --prompt private"),
                                       ("job_pid", str(2 ** 40)),
                                       ("font_size", float("nan")),
                                       ("tmux_role", "invented")])
def test_malformed_native_metadata_is_rejected_without_content_in_status(setup, key, value):
    adapter, bridge, automation, _, _ = setup
    automation.metadata[key] = value
    assert adapter.inspect(bridge, bridge.front) is None
    assert "private" not in bridge.status[-1] and "owned-session" not in bridge.status[-1]


def test_focus_change_during_automation_discards_the_record(setup):
    adapter, bridge, automation, _, _ = setup
    read = automation.read_session

    def changed(*args):
        value = read(*args)
        bridge.front = (42, "other", {"app_id": terminal.ITERM, "domain": ""})
        return value

    automation.read_session = changed
    assert adapter.inspect(bridge, bridge.front) is None
    assert adapter.diagnostic == "foreground-changed"


@pytest.mark.parametrize("name,candidate", [("node", "unknown"), ("bun", "unknown"),
                                           ("python3", "unknown"), ("zsh", "shell"),
                                           ("tmux", "multiplexer"), ("zellij", "multiplexer")])
def test_generic_interpreters_and_multiplexers_are_not_agent_prompts(setup, name, candidate):
    adapter, bridge, _, process, _ = setup
    adapter._processes.foreground = lambda *args: replace(process, executable="/opt/bin/" + name)
    assert adapter.inspect(bridge, bridge.front).candidate == candidate


def test_native_tmux_keeps_pane_identity_without_claiming_remote_process_ownership(setup):
    adapter, bridge, automation, _, _ = setup
    automation.metadata.update(tmux_role="client", pane_id="%8")
    result = adapter.inspect(bridge, bridge.front)
    assert result.pane_id == "%8" and result.tmux_role == "client"
    assert result.candidate == "multiplexer"


class NativeCall:
    def __init__(self, function):
        self.function = function

    def __call__(self, *args):
        return self.function(*args)


@pytest.mark.parametrize("mode", ["valid", "ambiguous", "reused", "wrong-group", "truncated"])
def test_libproc_resolves_only_one_stable_foreground_generation(monkeypatch, mode):
    count = {}

    def listpids(kind, device, buffer, size):
        assert (kind, device) == (3, 0x1234)
        buffer[0] = 351
        if mode == "truncated":
            return size
        if mode == "ambiguous":
            buffer[1] = 352
            return 8
        return 4

    def info(pid, flavor, argument, pointer, size):
        assert flavor == 3 and argument == 0
        count[pid] = count.get(pid, 0) + 1
        record = pointer._obj
        record.pid, record.ppid, record.uid = pid, 350, 501
        record.pgid, record.tpgid = (99 if mode == "wrong-group" else 351), 351
        record.ttydev = 0x1234
        record.start_sec = 1729000000 + (mode == "reused" and count[pid] > 1)
        record.start_usec = 19
        record.nfiles = count[pid]  # Harmless descriptor-count changes are not PID reuse.
        return size

    def path(pid, buffer, size):
        buffer.value = b"/opt/bin/codex"
        return len(buffer.value)

    lib = SimpleNamespace(proc_listpids=NativeCall(listpids), proc_pidinfo=NativeCall(info),
                          proc_pidpath=NativeCall(path))
    monkeypatch.setattr(terminal, "sys", SimpleNamespace(platform="darwin"))
    monkeypatch.setattr(terminal.ctypes, "CDLL", lambda name: lib)
    monkeypatch.setattr(terminal, "os", SimpleNamespace(
        stat=lambda tty: SimpleNamespace(st_mode=0o020600, st_rdev=0x1234),
        getuid=lambda: 501, fsdecode=os.fsdecode, path=os.path))
    result = terminal._Processes().foreground("/dev/ttys041")
    if mode == "valid":
        assert result == terminal.ProcessIdentity(351, 350, 351, (1729000000, 19), "/opt/bin/codex")
    else:
        assert result is None


def test_appleevent_descriptor_abi_matches_packed_sdk():
    assert terminal._AEDesc.dataHandle.offset == 4
    assert ctypes.sizeof(terminal._AEDesc) == 4 + ctypes.sizeof(ctypes.c_void_p)


def test_read_event_options_independently_forbid_permission_prompt_and_recording():
    assert terminal._READ_OPTIONS & 0x20000  # AppleEvents.h kAEDoNotPromptForUserConsent
    assert terminal._READ_OPTIONS & 0x1000  # AEDataModel.h kAEDontRecord
    assert terminal._READ_OPTIONS & 0x30 == 0x10  # NeverInteract, not CanInteract


@pytest.mark.parametrize("app_id", [terminal.TERMINAL, terminal.ITERM])
def test_native_appleevents_query_only_allowlisted_metadata_for_running_pid(monkeypatch, app_id):
    events = []
    code = terminal._code
    values = {"vers": "3.7.6", "ttty": "/dev/ttys041", "Stty": "/dev/ttys041",
              "ccol": 100, "crow": 32, "font": "Menlo-Regular", "ptsz": 13,
              "tbsl": True, "Cols": 100, "Rows": 32, "Indx": 0, "Iaps": True}
    variables = {"jobPid": "351", "tmuxRole": "", "tmuxWindowPane": ""}

    class Descriptor:
        def __init__(self, value=None):
            self.value, self.fields = value, {}

        @classmethod
        def recordDescriptor(cls):
            return cls()

        @classmethod
        def nullDescriptor(cls):
            return cls()

        descriptorWithTypeCode_ = classmethod(lambda cls, value: cls(value))
        descriptorWithEnumCode_ = classmethod(lambda cls, value: cls(value))
        descriptorWithInt32_ = classmethod(lambda cls, value: cls(value))
        descriptorWithString_ = classmethod(lambda cls, value: cls(value))

        @classmethod
        def descriptorWithDescriptorType_bytes_length_(cls, kind, data, size):
            assert kind == code("kpid") and size == 4
            assert int.from_bytes(data, sys.byteorder, signed=True) == 41
            return cls(("pid", 41))

        @classmethod
        def appleEventWithEventClass_eventID_targetDescriptor_returnID_transactionID_(
                cls, event_class, event_id, target, return_id, transaction):
            assert target.value == ("pid", 41) and return_id == -1 and transaction == 0
            return cls((event_class, event_id))

        def setDescriptor_forKeyword_(self, value, keyword):
            self.fields[keyword] = value

        setParamDescriptor_forKeyword_ = setDescriptor_forKeyword_

        def coerceToDescriptorType_(self, kind):
            assert kind == code("obj ")
            return self

        def int32Value(self):
            return int(self.value)

        def doubleValue(self):
            return float(self.value)

        def booleanValue(self):
            return bool(self.value)

        def stringValue(self):
            return self.value

        def paramDescriptorForKeyword_(self, keyword):
            return self.fields.get(keyword)

        def sendEventWithOptions_timeout_error_(self, options, timeout, error):
            assert options == terminal._READ_OPTIONS and 0 < timeout <= 0.15 and error is None
            if self.value == (code("core"), code("getd")):
                spec = self.fields[code("----")]
                assert spec.fields[code("want")].value == code("prop")
                assert spec.fields[code("form")].value == code("prop")
                name = spec.fields[code("seld")].value.to_bytes(4, "big").decode("ascii")
                events.append(("property", name))
                if name == "ID  ":
                    container = spec.fields[code("from")]
                    selector = container.fields.get(code("seld"))
                    value = "owned-session" if selector and selector.value == code("Wcsn") else 17
                else:
                    value = values[name]  # Any other property, including contents/history, fails.
            else:
                assert self.value == (code("Itrm"), code("varb"))
                name = self.fields[code("Namd")].value
                events.append(("variable", name))
                value = variables[name]  # No commandLine, lastCommand, titles or selection.
            reply = Descriptor()
            reply.fields[code("----")] = Descriptor(value)
            return reply, None

    monkeypatch.setitem(sys.modules, "Foundation", SimpleNamespace(NSAppleEventDescriptor=Descriptor))
    automation = terminal._Automation()
    monkeypatch.setattr(automation, "_running", lambda target: (41,))
    metadata = automation.read_session(41, app_id)
    assert metadata["window_id"] == 17 and metadata["tty"] == "/dev/ttys041"
    if app_id == terminal.TERMINAL:
        assert metadata["font_name"] == "Menlo-Regular" and metadata["font_size"] == 13
        assert metadata["session_id"] == "/dev/ttys041"
        assert {value for kind, value in events if kind == "property"} == {
            "vers", "ID  ", "ttty", "ccol", "crow", "font", "ptsz", "tbsl"}
    else:
        assert metadata["session_id"] == "owned-session" and metadata["job_pid"] == "351"
        assert {value for kind, value in events if kind == "variable"} == set(variables)
        assert "font_name" not in metadata  # The native SDEF has no effective font getter.


@pytest.mark.parametrize("prompt_requested", [False, True])
def test_native_permission_query_never_prompts_unless_explicitly_requested(monkeypatch, prompt_requested):
    calls = []

    def create(kind, data, size, target):
        assert kind == terminal._code("kpid") and size == 4
        assert int.from_bytes(data, sys.byteorder, signed=True) == 41
        calls.append("create")
        return 0

    def determine(target, event_class, event_id, prompt):
        assert (event_class, event_id) == (terminal._code("core"), terminal._code("getd"))
        calls.append(("prompt", prompt))
        return -1744

    lib = SimpleNamespace(AECreateDesc=NativeCall(create),
                          AEDisposeDesc=NativeCall(lambda target: calls.append("dispose")),
                          AEDeterminePermissionToAutomateTarget=NativeCall(determine))
    monkeypatch.setattr(terminal, "sys", SimpleNamespace(platform="darwin", byteorder=sys.byteorder))
    monkeypatch.setattr(terminal.ctypes, "CDLL", lambda name: lib)
    automation = terminal._Automation()
    monkeypatch.setattr(automation, "_running", lambda target: (41,))
    assert automation.permission(terminal.ITERM, request=prompt_requested) == "not-determined"
    assert calls == ["create", ("prompt", prompt_requested), "dispose"]


class InputBridge(Bridge):
    """Stateful native boundary: range reads, cursor and actual glyph boxes."""

    def __init__(self):
        super().__init__()
        import threading
        self.front = (41, "/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal",
                      {"app_id": terminal.TERMINAL, "domain": ""})
        self._terminal_completion = True
        self._ime_kind = "layout"
        self._stopped = threading.Event()
        self._lock = threading.RLock()
        self.field, self.window = object(), object()
        # Independent on-screen CG inventory, not derived from Automation's
        # scriptable window ID or from the AX object's hash.
        self.cg_windows = {(41, (0.0, 0.0, 840.0, 600.0)): 17}
        self.prompt = "› "
        self.text = "Thank you for your help"
        self.selection_length = 0
        self.marked = None
        self.enabled, self.focused, self.trusted = True, True, True
        self.reads, self.posts = [], []
        self.cursor_delta = 0
        self.extra_above = ""
        self.attachment_guard = ""
        self.footer = "  gpt-5.4 default · /private/disposable"
        self.glyph_width = 7.0
        self.allow_commit, self.commits = True, 0
        self.flags, self.post_permission = 0, True
        self.ax = SimpleNamespace(
            kAXValueCFRangeType=4,
            AXIsProcessTrusted=lambda: self.trusted,
            AXValueCreate=lambda kind, pair: tuple(pair),
            AXValueGetValue=lambda value, kind, unused: (True, value),
            AXUIElementCreateApplication=lambda pid: "app",
            AXUIElementCopyParameterizedAttributeValue=self.parameter)
        self.q = SimpleNamespace(
            CGPreflightPostEventAccess=lambda: self.post_permission,
            CGEventSourceFlagsState=lambda state: self.flags,
            kCGEventSourceStateCombinedSessionState=0,
            kCGEventFlagMaskCommand=1, kCGEventFlagMaskControl=2,
            kCGEventFlagMaskAlternate=4)

    def rows(self):
        strings = {18: self.attachment_guard + "\n", 19: self.extra_above + "\n",
                   20: self.prompt + self.text + "\n", 21: "\n", 22: self.footer + "\n"}
        offset, result = 1000, {}
        for line, string in strings.items():
            length = terminal.index_to_utf16(string, len(string))
            result[line] = (offset, length, string)
            offset += length
        return result

    def _attribute(self, element, name):
        self.reads.append(("attribute", name))
        if name == "AXValue":
            raise AssertionError("Terminal scrollback is never input")
        rows = self.rows()
        start, length, _ = rows[20]
        values = {"AXFocusedUIElement": self.field, "AXRole": "AXTextArea",
                  "AXSubrole": None, "AXProtectedContent": False, "AXHidden": False,
                  "AXFocused": self.focused, "AXEnabled": self.enabled,
                  "AXMarkedTextRange": self.marked, "AXSelectedText": "",
                  "AXSelectedTextRange": (start + length - 1 + self.cursor_delta,
                                          self.selection_length),
                  "AXInsertionPointLineNumber": 20,
                  "AXNumberOfCharacters": rows[22][0] + rows[22][1],
                  "AXWindow": self.window, "AXFocusedWindow": self.window}
        return values.get(name)

    def parameter(self, field, name, value, unused):
        assert field is self.field
        self.reads.append((name, value))
        if name == "AXLineForIndex":
            return 0, 20
        if name == "AXRangeForLine":
            return 0, self.rows()[value][:2]
        if name == "AXStringForRange":
            for start, length, string in self.rows().values():
                if value == (start, length):
                    return 0, string
            raise AssertionError("Only exact bounded physical rows may be read")
        raise AssertionError("Unexpected native read")

    def _field_bounds(self, element):
        return (0.0, 0.0, 840.0, 600.0)

    def _native_window_number(self, pid, bounds):
        return self.cg_windows.get((pid, bounds))

    def _visible_clip(self, field, rect):
        return rect

    def _range_bounds(self, field, start, length):
        assert length > 0  # The collapsed range is not valid Terminal geometry.
        line_start = self.rows()[20][0]
        return (10 + (start - line_start) * 7.0, 418.0,
                length * self.glyph_width, 15.0)

    def _styled_render(self, snapshot, render, evidence):
        return None

    def _commit_input(self):
        self.commits += 1
        return self.allow_commit

    def _post_key(self, code, flags, unicode):
        assert code == flags == 0
        self.posts.append(unicode)


@pytest.fixture
def composer(setup, monkeypatch):
    adapter, _, automation, process, _ = setup
    bridge = InputBridge()
    automation.metadata.update(app_version="2.15", font_name="SFMonoTerminal-Regular",
                               font_size=12.0)
    process = replace(process, executable=(
        "/Users/owned/.codex/packages/standalone/releases/"
        "0.162.1-aarch64-apple-darwin/bin/codex"))
    adapter._processes.foreground = lambda *args: process
    adapter._code_identity = SimpleNamespace(accepts=lambda current: current == process)
    monkeypatch.setattr(adapter, "_register_terminal_font", lambda metadata: None)
    return adapter, bridge, automation, process


def test_current_input_excludes_history_prompt_and_private_footer(composer):
    adapter, bridge, _, _ = composer
    snapshot, field = adapter.capture(bridge, bridge.front)
    assert field is bridge.field
    assert snapshot.text == bridge.text and snapshot.caret == len(bridge.text)
    assert not snapshot.can_replace and not snapshot.on_demand_only
    assert snapshot.caret_rect == (10 + 7 * (2 + len(bridge.text)), 418, 1, 15)
    assert "/private" not in repr(snapshot) and "/dev/tty" not in repr(snapshot)
    assert not snapshot.inline_safe  # Cell geometry does not invent a baseline.
    assert adapter.context_matches(bridge, snapshot)
    assert {name for name, _ in bridge.reads if name == "AXValue"} == set()
    assert all(length <= 201 for name, value in bridge.reads
               if name == "AXStringForRange" for _, length in [value])


@pytest.mark.parametrize("footer", [
    "  gpt-5.4 default · /private/disposable",
    "  provider/model-v2 minimal · ~/work",
    "  Custom Model low · ~/work with spaces",
    "  openai/gpt-5.4 medium · /private/disposable",
    "  Custom Model high · ~/work",
    "  provider/model-v2 xhigh · ~/work",
    "  codex-auto-thorough max · ~/work",
    "  Custom Model ultra · ~/work",
    "  provider/model-v2 persistent · ~/work",
])
def test_source_footer_model_and_reasoning_are_data_not_input_ownership(composer, footer):
    adapter, bridge, _, _ = composer
    bridge.footer = footer
    snapshot = adapter.capture(bridge, bridge.front)[0]
    assert snapshot.text == bridge.text and snapshot.caret == len(bridge.text)
    assert footer.strip() not in repr(snapshot)
    assert adapter.context_matches(bridge, snapshot)


@pytest.mark.parametrize("footer", [
    "  Shell mode",
    "  gpt-5.4 · /private/disposable",
    "  high · /private/disposable",
    "  gpt-5.4 invented · /private/disposable",
    "  gpt-5.4 none · /private/disposable",  # None renders as default in this source.
    "  gpt-5.4 high · ",
    "gpt-5.4 high · /private/disposable",
    "  gpt-5.4 high - /private/disposable",
    "  Press Enter to approve",
])
def test_unknown_or_instructional_footer_is_not_the_default_composer_layout(composer, footer):
    adapter, bridge, _, _ = composer
    bridge.footer = footer
    assert adapter.capture(bridge, bridge.front) is None
    assert adapter.diagnostic == "codex-modal-or-layout-unverified"
    assert not bridge.posts


@pytest.mark.parametrize("text", [
    "Please explain [optional] sections",
    "Please describe array[index]",
    "Please finish this [unfinished",
    "Please explain the closing ] character",
    "Please explain [Pasted Content] as a phrase",
])
def test_literal_brackets_are_not_hidden_composer_elements(composer, text):
    adapter, bridge, _, _ = composer
    bridge.text = text
    snapshot = adapter.capture(bridge, bridge.front)[0]
    assert snapshot.text == text
    assert adapter.append(bridge, snapshot, " clearly")
    assert "".join(text for text, _ in bridge.posts) == " clearly"


@pytest.mark.parametrize("label", [
    "[Pasted Content 1001 chars]",
    "[Pasted Content 1001 chars] #2",
    "[Pasted Content 1005 chars] #12",
    "[Image #1]",
    "[Image #12]",
])
def test_source_hidden_payload_labels_never_expose_partial_drafts(composer, label):
    adapter, bridge, _, _ = composer
    bridge.text = "Please inspect " + label + " before answering"
    assert adapter.capture(bridge, bridge.front) is None
    assert adapter.diagnostic == "codex-nonplain-or-popup-input"
    assert not bridge.posts


@pytest.mark.parametrize("identity", ["metadata-other-window", "native-other-window", "missing"])
def test_independent_native_window_must_own_scriptable_session_before_text_read(composer, identity):
    adapter, bridge, automation, _ = composer
    if identity == "metadata-other-window":
        automation.metadata["window_id"] = 18
    elif identity == "native-other-window":
        bridge.cg_windows[(41, bridge._field_bounds(bridge.window))] = 18
    else:
        bridge.cg_windows.clear()
    assert adapter.capture(bridge, bridge.front) is None
    assert adapter.diagnostic == "native-window-identity-unverified"
    assert not any(name == "AXStringForRange" for name, _ in bridge.reads)
    assert not bridge.posts and bridge.commits == 0


@pytest.mark.parametrize("prompt,owned", [
    ("› ", True), ("» ", True), ("! ", False), ("> ", False), ("❯ ", False),
])
def test_only_source_natural_language_prompt_can_own_current_native_cursor(composer, prompt, owned):
    adapter, bridge, _, _ = composer
    bridge.prompt = prompt
    bridge.footer = "  provider/custom-model high · ~/work"
    captured = adapter.capture(bridge, bridge.front)
    if owned:
        assert captured[0].text == bridge.text
    else:
        assert captured is None
        assert adapter.diagnostic == "codex-main-composer-unrecognized"
    assert not bridge.posts


@pytest.mark.parametrize("adjacent", [
    "Select Model",
    "Would you like to run the following command?",
    "[Image #1]",
])
def test_variable_model_footer_does_not_authorize_an_adjacent_popup_or_attachment(composer, adjacent):
    adapter, bridge, _, _ = composer
    bridge.footer = "  provider/custom-model high · ~/work"
    bridge.extra_above = adjacent
    assert adapter.capture(bridge, bridge.front) is None
    assert adapter.diagnostic == "codex-modal-or-layout-unverified"
    assert not bridge.posts


@pytest.mark.parametrize("state", [
    "selection", "middle", "placeholder", "shell", "slash", "mention", "skill",
    "hidden-paste", "approval", "footer", "focus", "secure", "dead-key",
    "ime", "marked", "opt-out", "untrusted", "wrapped", "glyph"])
def test_unowned_or_unobservable_input_never_becomes_a_snapshot(composer, state):
    adapter, bridge, _, _ = composer
    if state == "selection":
        bridge.selection_length = 2
    elif state == "middle":
        bridge.cursor_delta = -2
    elif state == "placeholder":
        bridge.text, bridge.cursor_delta = "Ask Codex to do anything", -23
    elif state == "shell":
        bridge.text = "!echo private"
    elif state == "slash":
        bridge.text = "/review"
    elif state == "mention":
        bridge.text = "Please inspect @private"
    elif state == "skill":
        bridge.text = "Please use $skill"
    elif state == "hidden-paste":
        bridge.text = "Please inspect [Pasted Content 1001 chars]"
    elif state == "approval":
        bridge.extra_above = "Would you like to run the following command?"
    elif state == "footer":
        bridge.footer = "  Shell mode"
    elif state == "focus":
        bridge.focused = False
    elif state == "secure":
        bridge.secure = True
    elif state == "dead-key":
        bridge._dead_key = True
    elif state == "ime":
        bridge._ime_kind = "input-method"
    elif state == "marked":
        bridge.marked = (0, 2)
    elif state == "opt-out":
        bridge._terminal_completion = False
    elif state == "untrusted":
        bridge.trusted = False
    elif state == "wrapped":
        bridge.text = "word " * 100
    else:
        bridge.glyph_width = 0
    assert adapter.capture(bridge, bridge.front) is None
    assert not bridge.posts


@pytest.mark.parametrize("change", ["tty", "generation", "draft", "font", "window",
                                   "cursor", "footer", "policy"])
def test_lease_revalidation_rejects_context_changes(composer, change):
    adapter, bridge, automation, process = composer
    snapshot = adapter.capture(bridge, bridge.front)[0]
    if change == "tty":
        automation.metadata["tty"] = "/dev/ttys099"
    elif change == "generation":
        adapter._processes.foreground = lambda *args: replace(process, started=(1729000001, 19))
    elif change == "draft":
        bridge.text += " changed"
    elif change == "font":
        automation.metadata["font_size"] = 14
    elif change == "window":
        bridge.window = object()
    elif change == "cursor":
        bridge.cursor_delta = -1
    elif change == "footer":
        bridge.footer = "  gpt-5.4 default · /different/private"
    else:
        bridge.allowed = False
    assert not adapter.context_matches(bridge, snapshot)
    assert not adapter.append(bridge, snapshot, " safely")
    assert bridge.posts == [] and bridge.commits == 0


@pytest.mark.parametrize("text", ["\n", "\r", "\t", "\x1b", "\x00", "\x7f", "\x85",
                                 "\u2028", "\u2029", "\u202e", "\ud800"])
def test_append_rejects_all_control_and_format_text_before_native_operations(composer, text):
    adapter, bridge, _, _ = composer
    snapshot = adapter.capture(bridge, bridge.front)[0]
    bridge.reads.clear()
    assert not adapter.append(bridge, snapshot, "suffix" + text)
    assert bridge.posts == [] and bridge.commits == 0 and bridge.reads == []


def test_explicit_unicode_append_is_chunked_without_return_or_replacement(composer):
    adapter, bridge, _, _ = composer
    snapshot = adapter.capture(bridge, bridge.front)[0]
    suffix = " café 😀" * 6
    assert adapter.append(bridge, snapshot, suffix)
    assert "".join(text for text, units in bridge.posts) == suffix
    assert all(terminal.index_to_utf16(text, len(text)) == units <= 20
               for text, units in bridge.posts)
    assert bridge.commits == 1
    assert not adapter.append(bridge, snapshot, " again")  # Consumed lease.


@pytest.mark.parametrize("reason", ["cancelled", "permission", "command", "control", "alt"])
def test_cancelled_or_modified_append_never_posts(composer, reason):
    adapter, bridge, _, _ = composer
    snapshot = adapter.capture(bridge, bridge.front)[0]
    if reason == "cancelled":
        bridge.allow_commit = False
    elif reason == "permission":
        bridge.post_permission = False
    else:
        bridge.flags = {"command": 1, "control": 2, "alt": 4}[reason]
    assert not adapter.append(bridge, snapshot, " suffix")
    assert bridge.posts == []


def test_context_check_does_not_recursively_schedule_appearance(composer):
    adapter, bridge, _, _ = composer
    calls = []

    def appearance(snapshot, render, evidence):
        calls.append(snapshot)
        assert adapter.context_matches(bridge, snapshot)
        assert len(evidence["samples"]) == 2
        return None

    bridge._styled_render = appearance
    assert adapter.capture(bridge, bridge.front) is not None
    assert len(calls) == 1


def test_permission_query_cannot_authorize_a_changed_input_before_commit(composer):
    adapter, bridge, _, _ = composer
    snapshot = adapter.capture(bridge, bridge.front)[0]

    def changed():
        bridge.text += " modified"
        return True

    bridge.q.CGPreflightPostEventAccess = changed
    assert not adapter.append(bridge, snapshot, " suffix")
    assert bridge.commits == 0 and bridge.posts == []


@pytest.mark.parametrize("kind", ["iterm", "unknown-version", "unknown-binary", "shell", "mux"])
def test_metadata_candidates_do_not_bypass_supported_owner_contract(composer, kind):
    adapter, bridge, automation, process = composer
    if kind == "iterm":
        bridge.front = (41, "/Applications/iTerm.app/Contents/MacOS/iTerm2",
                        {"app_id": terminal.ITERM, "domain": ""})
    elif kind == "unknown-version":
        adapter._processes.foreground = lambda *args: replace(
            process, executable=process.executable.replace("0.162.1", "0.162.2"))
    else:
        name = {"unknown-binary": "codex", "shell": "zsh", "mux": "tmux"}[kind]
        adapter._processes.foreground = lambda *args: replace(
            process, executable="/unverified/bin/" + name)
    assert adapter.capture(bridge, bridge.front) is None
    assert not any(name == "AXStringForRange" for name, _ in bridge.reads)


def test_same_length_edit_during_native_read_is_not_a_stable_capture(composer):
    adapter, bridge, _, _ = composer
    parameter = bridge.parameter
    reads = 0

    def changed(field, name, value, unused):
        nonlocal reads
        result = parameter(field, name, value, unused)
        if name == "AXStringForRange":
            reads += 1
            if reads == 4:
                bridge.text = "x" + bridge.text[1:]
        return result

    bridge.ax.AXUIElementCopyParameterizedAttributeValue = changed
    assert adapter.capture(bridge, bridge.front) is None
    assert bridge.posts == [] and bridge.commits == 0


@pytest.mark.parametrize("version", ["2.14", "2.16"])
def test_terminal_app_version_does_not_replace_native_contract_validation(composer, version):
    adapter, bridge, automation, _ = composer
    automation.metadata["app_version"] = version
    snapshot = adapter.capture(bridge, bridge.front)[0]
    assert snapshot.text == bridge.text
    assert adapter.context_matches(bridge, snapshot)
    bridge.cg_windows.clear()
    assert not adapter.append(bridge, snapshot, " suffix")
    assert not bridge.posts and bridge.commits == 0


@pytest.mark.parametrize("consistent", [True, False])
def test_fixed_advance_requires_two_consistent_native_cell_spans(composer, consistent):
    adapter, bridge, _, _ = composer
    bridge.text = "Please review this report before I submit"
    bounds = bridge._range_bounds
    if not consistent:
        def inconsistent(field, start, length):
            rect = bounds(field, start, length)
            return (*rect[:2], rect[2] + 2, rect[3]) if length == 16 else rect

        bridge._range_bounds = inconsistent
    evidence = []
    bridge._styled_render = lambda snapshot, render, sample: evidence.append(sample)
    snapshot = adapter.capture(bridge, bridge.front)[0]
    assert evidence[0]["fixed_advance"] == (7.0 if consistent else 0.0)
    assert evidence[0]["samples"][0]["rect"][2] == 32 * 7.0
    assert evidence[0]["samples"][1]["rect"][2] == 16 * 7.0 + (0 if consistent else 2)
    assert not snapshot.inline_safe  # Advance evidence alone cannot supply a baseline.


@pytest.mark.parametrize("failure", [
    None, "number", "dictionary", "lookup", "missing-code", "signature", "exception",
])
def test_running_code_validation_uses_exact_pid_and_releases_all_native_references(
        monkeypatch, failure):
    calls, released = [], []

    def number(allocator, kind, value):
        assert allocator is None and kind == 3 and value._obj.value == 351
        return 0 if failure == "number" else 101

    def dictionary(allocator, keys, values, count, key_callbacks, value_callbacks):
        assert allocator is key_callbacks is value_callbacks is None
        assert count == 1 and keys[0] == 99 and values[0] == 101
        return 0 if failure == "dictionary" else 102

    def guest(host, attributes, flags, result):
        calls.append("lookup")
        assert host is None and attributes == 102 and flags == 0
        if failure == "exception":
            raise RuntimeError("native lookup failed")
        if failure != "missing-code":
            result._obj.value = 103
        return -67050 if failure == "lookup" else 0

    def validate(code, flags, requirement):
        calls.append("validate")
        assert code.value == 103 and flags == 0 and requirement == 104
        return -67050 if failure == "signature" else 0

    identity = terminal._CodeIdentity()
    identity._native = (
        SimpleNamespace(SecCodeCopyGuestWithAttributes=guest, SecCodeCheckValidity=validate),
        SimpleNamespace(CFNumberCreate=number, CFDictionaryCreate=dictionary,
                        CFRelease=lambda ref: released.append(
                            ref.value if isinstance(ref, ctypes.c_void_p) else ref)),
        99, 104)
    monkeypatch.setattr(terminal, "sys", SimpleNamespace(platform="darwin"))
    process = terminal.ProcessIdentity(351, 350, 351, (1729000000, 19), "/any/codex")
    assert identity.accepts(process) is (failure is None)
    expected = ([103] if failure not in {"number", "dictionary", "missing-code", "exception"}
                else []) + ([102] if failure not in {"number", "dictionary"} else [])
    expected += [101] if failure != "number" else []
    assert released == expected
    assert ("validate" in calls) is (failure in {None, "signature"})


@pytest.mark.parametrize("pid,pgid", [(0, 0), (-1, -1), (2 ** 31, 2 ** 31), (351, 350)])
def test_running_code_identity_never_looks_up_invalid_or_nonleader_pid(monkeypatch, pid, pgid):
    identity = terminal._CodeIdentity()
    monkeypatch.setattr(terminal, "sys", SimpleNamespace(platform="darwin"))
    monkeypatch.setattr(identity, "_load", lambda: pytest.fail("Unexpected native lookup"))
    assert not identity.accepts(
        terminal.ProcessIdentity(pid, 350, pgid, (1729000000, 19), "/any/codex"))


def test_unavailable_security_framework_cannot_fall_back_to_release_path(composer, monkeypatch):
    adapter, bridge, _, process = composer
    identity = terminal._CodeIdentity()
    monkeypatch.setattr(terminal, "sys", SimpleNamespace(platform="darwin"))
    monkeypatch.setattr(identity, "_load", lambda: (_ for _ in ()).throw(OSError("unavailable")))
    adapter._code_identity = identity
    assert adapter.capture(bridge, bridge.front) is None
    assert adapter.diagnostic == "agent-renderer-version-unverified"
    assert not any(name == "AXStringForRange" for name, _ in bridge.reads)
    assert not bridge.posts


def test_release_path_without_verified_running_signature_is_not_an_owner(composer):
    adapter, bridge, _, _ = composer
    adapter._code_identity = SimpleNamespace(accepts=lambda current: False)
    assert adapter.capture(bridge, bridge.front) is None
    assert adapter.diagnostic == "agent-renderer-version-unverified"
    assert not any(name == "AXStringForRange" for name, _ in bridge.reads)


@pytest.mark.parametrize("path", ["/opt/homebrew/bin/codex", "/owned/copied/agent-runner"])
def test_relocated_verified_release_is_not_tied_to_installer_path(composer, path):
    adapter, bridge, _, process = composer
    relocated = replace(process, executable=path)
    adapter._processes.foreground = lambda *args: relocated
    adapter._code_identity = SimpleNamespace(accepts=lambda current: current == relocated)
    snapshot = adapter.capture(bridge, bridge.front)[0]
    assert snapshot.text == bridge.text and adapter.append(bridge, snapshot, " safely")
    assert "".join(text for text, _ in bridge.posts) == " safely"


@pytest.mark.parametrize("stage", ["during-row-read", "before-append", "before-commit"])
def test_losing_running_signature_validity_discards_capture_or_append(composer, stage):
    adapter, bridge, _, _ = composer
    valid = True
    adapter._code_identity = SimpleNamespace(accepts=lambda current: valid)
    if stage == "during-row-read":
        parameter = bridge.parameter

        def revoked(*args):
            nonlocal valid
            value = parameter(*args)
            if args[1] == "AXStringForRange":
                valid = False
            return value

        bridge.ax.AXUIElementCopyParameterizedAttributeValue = revoked
        assert adapter.capture(bridge, bridge.front) is None
    else:
        snapshot = adapter.capture(bridge, bridge.front)[0]
        if stage == "before-append":
            valid = False
        else:
            def revoked():
                nonlocal valid
                valid = False
                return True

            bridge.q.CGPreflightPostEventAccess = revoked
        assert not adapter.append(bridge, snapshot, " safely")
    assert not bridge.posts and bridge.commits == 0


@pytest.mark.parametrize("surface", ["prefix", "whole", "glyph", "field", "window", "clip"])
@pytest.mark.parametrize("invalid", [float("nan"), float("inf"), -float("inf")])
def test_nonfinite_native_geometry_is_never_a_caret(composer, surface, invalid):
    adapter, bridge, _, _ = composer
    bounds, field_bounds = bridge._range_bounds, bridge._field_bounds
    line_start = bridge.rows()[20][0]
    location = line_start + bridge.rows()[20][1] - 1
    targets = {"prefix": (line_start, 2), "whole": (line_start + 2, len(bridge.text)),
               "glyph": (location - 1, 1)}

    def altered(field, start, length):
        rect = bounds(field, start, length)
        return (invalid, *rect[1:]) if (start, length) == targets.get(surface) else rect

    bridge._range_bounds = altered
    if surface in {"field", "window"}:
        target = bridge.field if surface == "field" else bridge.window
        bridge._field_bounds = lambda element: (
            (invalid, 0, 840, 600) if element is target else field_bounds(element))
        if surface == "window":
            bridge._native_window_number = lambda *args: 17
    elif surface == "clip":
        bridge._visible_clip = lambda *args: (invalid, 0, 840, 600)
    assert adapter.capture(bridge, bridge.front) is None
    assert not bridge.posts


@pytest.mark.parametrize("kind", ["prefix-different-row", "prefix-gap", "whole-outside",
                                  "clip-outside-field", "field-outside-window"])
def test_entire_prompt_and_input_must_share_real_visible_host_geometry(composer, kind):
    adapter, bridge, _, _ = composer
    bounds, field_bounds = bridge._range_bounds, bridge._field_bounds
    line_start = bridge.rows()[20][0]

    def altered(field, start, length):
        rect = bounds(field, start, length)
        if start == line_start and kind.startswith("prefix"):
            return (rect[0], rect[1] + 15, *rect[2:]) if kind.endswith("row") else (
                rect[0] - 1, *rect[1:])
        if kind == "whole-outside" and start == line_start + 2:
            return (-2, rect[1], rect[2] + rect[0] + 2, rect[3])
        return rect

    bridge._range_bounds = altered
    if kind == "clip-outside-field":
        bridge._visible_clip = lambda *args: (-1, 0, 841, 600)
    elif kind == "field-outside-window":
        bridge._field_bounds = lambda element: (
            (-1, 0, 841, 600) if element is bridge.field else field_bounds(element))
    assert adapter.capture(bridge, bridge.front) is None
    assert not bridge.posts and bridge.commits == 0


def test_clip_change_invalidates_existing_append_lease(composer):
    adapter, bridge, _, _ = composer
    snapshot = adapter.capture(bridge, bridge.front)[0]
    bridge._visible_clip = lambda field, rect: (1.0, 0.0, 839.0, 600.0)
    assert not adapter.context_matches(bridge, snapshot)
    assert not adapter.append(bridge, snapshot, " safely")
    assert not bridge.posts and bridge.commits == 0


@pytest.mark.parametrize("label", ["  [Image #1]", "  [Image #12]  ",
                                   "  [Pasted Content 1001 chars]"])
def test_source_remote_attachment_above_blank_separator_is_not_plain_input(composer, label):
    adapter, bridge, _, _ = composer
    bridge.attachment_guard = label
    assert bridge.extra_above == ""  # The old one-row padding guard is insufficient.
    assert adapter.capture(bridge, bridge.front) is None
    assert adapter.diagnostic == "codex-hidden-attachment-row"
    assert not bridge.posts and bridge.commits == 0


def test_bounded_adjacent_attachment_guard_is_never_promoted_to_draft(composer):
    adapter, bridge, _, _ = composer
    bridge.attachment_guard = "previous synthetic response, not current input"
    snapshot = adapter.capture(bridge, bridge.front)[0]
    assert snapshot.text == bridge.text
    assert "previous synthetic" not in repr(snapshot)
    bridge.attachment_guard = "[Image #1]"
    assert not adapter.append(bridge, snapshot, " safely")
    assert not bridge.posts and bridge.commits == 0


@pytest.mark.parametrize("role", ["client", "gateway"])
def test_mux_metadata_cannot_authorize_even_an_otherwise_verified_binary(composer, role):
    adapter, bridge, automation, _ = composer
    automation.metadata.update(tmux_role=role, pane_id="%8")
    adapter._code_identity = SimpleNamespace(accepts=lambda current: True)
    assert adapter.capture(bridge, bridge.front) is None
    assert adapter.diagnostic == "foreground-input-owner-unsupported"
    assert not any(name == "AXStringForRange" for name, _ in bridge.reads)
