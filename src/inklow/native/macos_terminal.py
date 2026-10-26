"""Bounded, append-only native terminal input.

Terminal AXValue is scrollback and is never requested. The only recognized
surface is the source-bound Codex 0.162.1 single-row natural-language composer
in Terminal.app. Other CLIs, shell input, iTerm copy/broadcast state, wrapping,
hidden paste elements and unfamiliar layouts remain unsupported.
"""
from __future__ import annotations

import ctypes
from dataclasses import dataclass, replace
import hashlib
import math
import os
import re
import stat
import sys
import time

import unicodedata

from ..core import Snapshot
from .base import index_to_utf16, utf16_to_index
TERMINAL = "com.apple.Terminal"
ITERM = "com.googlecode.iterm2"
SUPPORTED_APPS = frozenset({TERMINAL, ITERM})
_AGENT_NAMES = frozenset({"claude", "codex", "opencode", "pi", "kilo"})
_SHELL_NAMES = frozenset({"zsh", "bash", "fish", "sh", "dash", "ksh", "tcsh"})
_TTY = re.compile(r"/dev/tty[A-Za-z0-9]+\Z")
# rust-v0.162.1: chatwidget/status_surfaces.rs::model_with_reasoning_display_name
# and status_controls.rs::status_line_reasoning_effort_label. Model display
# names are catalog/provider data; only the known default status layout and
# built-in effort labels are recognized, not custom status/tier/effort layouts.
_CODEX_FOOTER = re.compile(
    r"  [^\s·]+(?: [^\s·]+)* "
    r"(?:default|minimal|low|medium|high|xhigh|max|ultra|persistent) · [^\r\n]+")
# rust-v0.162.1: bottom_pane/chat_composer.rs::next_large_paste_placeholder
# emits [Pasted Content N chars], followed by " #2" etc. for duplicates.
# The same source documents [Image #N] attachment labels. AX exposes neither
# hidden payload nor element identity, so even a literal reserved label is
# ambiguous. Ordinary brackets do not imply hidden content.
_CODEX_HIDDEN_ELEMENT = re.compile(r"\[(?:Pasted Content [0-9]+ chars|Image #[0-9]+)\]")
_TIMEOUT = 0.6
# AEDataModel.h and AppleEvents.h: wait, no interaction/reconnect/recording,
# and independently prohibit a new TCC consent prompt during a read.
_READ_OPTIONS = 0x00021093


def terminal_policy_exclusions(config) -> tuple[str, ...]:
    """Replace only the factory terminal block after explicit macOS opt-in.

    A customized exclusion list remains authoritative, even when it contains
    the same terminal names. Saved settings and other platforms are untouched.
    """
    from ..config import Config
    excluded = tuple(config.excluded_apps)
    factory = Config.__dataclass_fields__["excluded_apps"].default
    if (sys.platform == "darwin" and config.terminal_completion
            and excluded == factory):
        return tuple(item for item in excluded if item not in ("terminal", "iterm"))
    return excluded


def _code(value: str) -> int:
    return int.from_bytes(value.encode("ascii"), "big")


class _AEDesc(ctypes.Structure):
    _pack_ = 2  # AEDataModel.h uses #pragma pack(push, 2), including arm64.
    _fields_ = [("descriptorType", ctypes.c_uint32), ("dataHandle", ctypes.c_void_p)]


class _BSDInfo(ctypes.Structure):
    _fields_ = [(name, ctypes.c_uint32) for name in (
        "flags", "status", "xstatus", "pid", "ppid", "uid", "gid", "ruid",
        "rgid", "svuid", "svgid", "reserved")]
    _fields_ += [("comm", ctypes.c_char * 16), ("name", ctypes.c_char * 32)]
    _fields_ += [(name, ctypes.c_uint32) for name in (
        "nfiles", "pgid", "jobc", "ttydev", "tpgid")]
    _fields_ += [("nice", ctypes.c_int32), ("start_sec", ctypes.c_uint64),
                ("start_usec", ctypes.c_uint64)]


@dataclass(frozen=True)
class ProcessIdentity:
    pid: int
    ppid: int
    pgid: int
    started: tuple[int, int]
    executable: str

    @property
    def name(self) -> str:
        return os.path.basename(self.executable)


@dataclass(frozen=True)
class TerminalMetadata:
    app_id: str
    host_pid: int
    window_id: int
    tab_id: str
    session_id: str
    tty: str
    columns: int
    rows: int
    app_version: str = ""
    font_name: str = ""
    font_size: float = 0.0
    at_shell_prompt: bool | None = None
    tmux_role: str = ""
    pane_id: str = ""
    process: ProcessIdentity | None = None

    @property
    def candidate(self) -> str:
        # Executable basename is classification metadata only, never a prompt
        # recognizer. Generic node/bun/python launchers are deliberately absent.
        name = self.process.name if self.process else ""
        if self.tmux_role or name in {"tmux", "zellij"}:
            return "multiplexer"
        if name in _AGENT_NAMES:
            return "agent"
        if name in _SHELL_NAMES:
            return "shell"
        return "unknown"


class _Automation:
    """Native AppleEvents addressed to a running PID; cannot launch an app."""

    @staticmethod
    def _running(app_id: str) -> tuple[int, ...]:
        from AppKit import NSRunningApplication
        return tuple(int(app.processIdentifier()) for app in
                     NSRunningApplication.runningApplicationsWithBundleIdentifier_(app_id)
                     if not app.isTerminated())

    def permission(self, app_id: str, *, request: bool = False) -> str:
        if app_id not in SUPPORTED_APPS:
            return "unsupported"
        if sys.platform != "darwin":
            return "unavailable"
        try:
            running = self._running(app_id)
            if not running:
                return "not-running"
            if len(running) != 1:
                return "unavailable"
            ae = ctypes.CDLL("/System/Library/Frameworks/CoreServices.framework/CoreServices")
            ae.AECreateDesc.argtypes = [ctypes.c_uint32, ctypes.c_void_p,
                                       ctypes.c_ssize_t, ctypes.POINTER(_AEDesc)]
            ae.AECreateDesc.restype = ctypes.c_int16
            ae.AEDisposeDesc.argtypes = [ctypes.POINTER(_AEDesc)]
            ae.AEDisposeDesc.restype = ctypes.c_int16
            ae.AEDeterminePermissionToAutomateTarget.argtypes = [
                ctypes.POINTER(_AEDesc), ctypes.c_uint32, ctypes.c_uint32, ctypes.c_bool]
            ae.AEDeterminePermissionToAutomateTarget.restype = ctypes.c_int32
            target = _AEDesc()
            data = running[0].to_bytes(4, sys.byteorder, signed=True)
            if ae.AECreateDesc(_code("kpid"), data, len(data), ctypes.byref(target)):
                return "unavailable"
            try:
                result = ae.AEDeterminePermissionToAutomateTarget(
                    ctypes.byref(target), _code("core"), _code("getd"), request)
            finally:
                ae.AEDisposeDesc(ctypes.byref(target))
            return {0: "granted", -1744: "not-determined", -1743: "denied",
                    -600: "not-running"}.get(result, "unavailable")
        except Exception:
            return "unavailable"

    def read_session(self, pid: int, app_id: str) -> dict:
        if app_id not in SUPPORTED_APPS or not isinstance(pid, int) or pid <= 0:
            raise ValueError("unsupported-target")
        from Foundation import NSAppleEventDescriptor as D
        deadline = time.monotonic() + _TIMEOUT
        if pid not in self._running(app_id):
            raise ValueError("not-running")
        data = pid.to_bytes(4, sys.byteorder, signed=True)
        target = D.descriptorWithDescriptorType_bytes_length_(_code("kpid"), data, len(data))

        def spec(container, desired, form, selector):
            result = D.recordDescriptor()
            for key, value in (("want", D.descriptorWithTypeCode_(_code(desired))),
                               ("from", container),
                               ("form", D.descriptorWithEnumCode_(_code(form))),
                               ("seld", selector)):
                result.setDescriptor_forKeyword_(value, _code(key))
            result = result.coerceToDescriptorType_(_code("obj "))
            if result is None:
                raise ValueError("specifier-unavailable")
            return result

        def prop(container, name):
            return spec(container, "prop", "prop", D.descriptorWithTypeCode_(_code(name)))

        def send(event_class, event_id, direct, parameters=()):
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise ValueError("metadata-timeout")
            event = D.appleEventWithEventClass_eventID_targetDescriptor_returnID_transactionID_(
                _code(event_class), _code(event_id), target, -1, 0)
            event.setParamDescriptor_forKeyword_(direct, _code("----"))
            for key, value in parameters:
                event.setParamDescriptor_forKeyword_(value, _code(key))
            # PID addressing cannot launch an application. The no-consent flag
            # also closes the race between preflight and this particular read.
            reply, error = event.sendEventWithOptions_timeout_error_(
                _READ_OPTIONS, min(0.15, remaining), None)
            if error is not None or reply is None:
                raise ValueError("metadata-unavailable")
            failure = reply.paramDescriptorForKeyword_(_code("errn"))
            if failure is not None and failure.int32Value():
                raise ValueError("metadata-unavailable")
            result = reply.paramDescriptorForKeyword_(_code("----"))
            if result is None:
                raise ValueError("metadata-unavailable")
            return result

        def get(container, name):
            return send("core", "getd", prop(container, name))

        root = D.nullDescriptor()
        version = get(root, "vers").stringValue()
        if app_id == TERMINAL:
            window = spec(root, "cwin", "indx", D.descriptorWithInt32_(1))
            session = prop(window, "tcnt")
            settings = prop(session, "tcst")
            result = dict(window_id=get(window, "ID  ").int32Value(),
                          tab_id="", session_id="", tty=get(session, "ttty").stringValue(),
                          columns=get(session, "ccol").int32Value(),
                          rows=get(session, "crow").int32Value(),
                          font_name=get(settings, "font").stringValue(),
                          font_size=get(settings, "ptsz").doubleValue())
            # Terminal has no stable tab ID in its dictionary. TTY is retained
            # with process generation, never replaced with a title-derived ID.
            result["tab_id"] = result["session_id"] = result["tty"]
            if not get(session, "tbsl").booleanValue():
                raise ValueError("inactive-session")
        else:
            window = prop(root, "Crwn")
            tab = prop(window, "Crtb")
            session = prop(tab, "Wcsn")

            def variable(name):
                return send("Itrm", "varb", session,
                            (("Namd", D.descriptorWithString_(name)),)).stringValue() or ""

            result = dict(window_id=get(window, "ID  ").int32Value(),
                          tab_id=str(get(tab, "Indx").int32Value()),
                          session_id=get(session, "ID  ").stringValue(),
                          tty=get(session, "Stty").stringValue(),
                          columns=get(session, "Cols").int32Value(),
                          rows=get(session, "Rows").int32Value(),
                          at_shell_prompt=bool(get(session, "Iaps").booleanValue()),
                          job_pid=variable("jobPid"), tmux_role=variable("tmuxRole"),
                          pane_id=variable("tmuxWindowPane"))
            # iTerm's SDEF does not expose effective per-session font/spacing.
            # Do not substitute a profile preference or a guessed monospace font.
        result["app_version"] = version
        if get(window, "ID  ").int32Value() != result["window_id"]:
            raise ValueError("session-changed")
        tty_property = "ttty" if app_id == TERMINAL else "Stty"
        if get(session, tty_property).stringValue() != result["tty"]:
            raise ValueError("session-changed")
        if app_id == ITERM and get(session, "ID  ").stringValue() != result["session_id"]:
            raise ValueError("session-changed")
        if pid not in self._running(app_id):
            raise ValueError("not-running")
        return result


class _Processes:
    """libproc metadata only; no process arguments, environment or terminal I/O."""

    def foreground(self, tty: str, job_pid: int = 0) -> ProcessIdentity | None:
        if sys.platform != "darwin" or not isinstance(tty, str) or not _TTY.fullmatch(tty):
            return None
        try:
            device = os.stat(tty)
            if not stat.S_ISCHR(device.st_mode):
                return None
            lib = ctypes.CDLL("/usr/lib/libproc.dylib")
            lib.proc_listpids.argtypes = [ctypes.c_uint32, ctypes.c_uint32,
                                         ctypes.c_void_p, ctypes.c_int]
            lib.proc_listpids.restype = ctypes.c_int
            lib.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64,
                                        ctypes.c_void_p, ctypes.c_int]
            lib.proc_pidinfo.restype = ctypes.c_int
            lib.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
            lib.proc_pidpath.restype = ctypes.c_int
            pids = (ctypes.c_int * 256)()
            size = lib.proc_listpids(3, device.st_rdev & 0xFFFFFFFF, pids, ctypes.sizeof(pids))
            if size <= 0 or size >= ctypes.sizeof(pids) or size % ctypes.sizeof(ctypes.c_int):
                return None
            candidates = []
            for pid in pids[:size // ctypes.sizeof(ctypes.c_int)]:
                if pid <= 0 or (job_pid and pid != job_pid):
                    continue
                before = _BSDInfo()
                if lib.proc_pidinfo(pid, 3, 0, ctypes.byref(before), ctypes.sizeof(before)) != ctypes.sizeof(before):
                    continue
                if (before.pid != pid or before.uid != os.getuid() or not before.start_sec
                        or before.ttydev != device.st_rdev & 0xFFFFFFFF
                        or not before.tpgid or before.pgid != before.tpgid):
                    continue
                path = ctypes.create_string_buffer(4096)
                if lib.proc_pidpath(pid, path, len(path)) <= 0:
                    continue
                after = _BSDInfo()
                if lib.proc_pidinfo(pid, 3, 0, ctypes.byref(after), ctypes.sizeof(after)) != ctypes.sizeof(after):
                    continue
                stable = ("pid", "ppid", "pgid", "uid", "ttydev", "tpgid",
                          "start_sec", "start_usec", "name", "comm")
                if any(getattr(before, key) != getattr(after, key) for key in stable):
                    continue
                executable = os.fsdecode(path.value)
                if not executable.startswith("/"):
                    continue
                candidates.append(ProcessIdentity(pid, before.ppid, before.pgid,
                                                  (before.start_sec, before.start_usec), executable))
            return candidates[0] if len(candidates) == 1 else None
        except Exception:
            return None


class TerminalAdapter:
    """Native session ownership, bounded composer capture and Unicode append.

    Appearance recovery is deliberately outside ``_read``: authorization and
    acceptance re-read the input without recursively requesting pixel recovery.
    """

    def __init__(self):
        self._automation = _Automation()
        self._processes = _Processes()
        self.diagnostic = "terminal-unverified"
        self._lease = None

    def permission_state(self, app_id: str) -> str:
        return self._automation.permission(app_id)

    def request_access(self, app_id: str) -> str:
        """Only an explicit UI action may call this prompt-capable method."""
        return self._automation.permission(app_id, request=True)

    def _reject(self, bridge, reason):
        self.diagnostic = reason
        bridge._set_status("Terminal completion unavailable: " + reason + ".")
        return None

    def inspect(self, bridge, foreground) -> TerminalMetadata | None:
        """Resolve only the given, still-foreground host's native metadata."""
        try:
            pid, executable, info = foreground
            app_id = info["app_id"]
            if app_id not in SUPPORTED_APPS:
                return self._reject(bridge, "unsupported-terminal")
            if (bridge.secure_input_active() or getattr(bridge, "_dead_key", False)
                    or not bridge._allowed(app_id, info.get("domain", ""), executable)):
                return self._reject(bridge, "privacy-or-input-policy")
            if bridge._foreground() != foreground:
                return self._reject(bridge, "foreground-changed")
            state = self.permission_state(app_id)
            if state != "granted":
                return self._reject(bridge, "automation-" + state)
            raw = self._automation.read_session(pid, app_id)
            tty = raw.get("tty")
            if (not isinstance(tty, str) or not _TTY.fullmatch(tty)
                    or not isinstance(raw.get("session_id"), str) or not raw["session_id"]
                    or len(raw["session_id"]) > 256
                    or not 0 < int(raw["window_id"]) < 2 ** 31
                    or not 1 <= int(raw["columns"]) <= 10000
                    or not 1 <= int(raw["rows"]) <= 10000):
                return self._reject(bridge, "invalid-session-metadata")
            job_pid = raw.get("job_pid", "")
            if job_pid and (not str(job_pid).isascii() or not str(job_pid).isdecimal()):
                return self._reject(bridge, "invalid-process-metadata")
            if int(job_pid or 0) < 0 or int(job_pid or 0) >= 2 ** 31:
                return self._reject(bridge, "invalid-process-metadata")
            process = self._processes.foreground(tty, int(job_pid or 0))
            font_size = float(raw.get("font_size", 0))
            font_name = raw.get("font_name", "")
            if (not isinstance(font_name, str) or len(font_name) > 256
                    or not math.isfinite(font_size) or not 0 <= font_size <= 300):
                return self._reject(bridge, "invalid-font-metadata")
            role = raw.get("tmux_role", "")
            pane = raw.get("pane_id", "")
            if role not in {"", "client", "gateway"} or not isinstance(pane, str) or len(pane) > 32:
                return self._reject(bridge, "invalid-pane-metadata")
            if bridge._foreground() != foreground or bridge.secure_input_active():
                return self._reject(bridge, "foreground-changed")
            self.diagnostic = "metadata-only"
            return TerminalMetadata(
                app_id=app_id, host_pid=pid, window_id=int(raw["window_id"]),
                tab_id=str(raw["tab_id"]), session_id=raw["session_id"], tty=tty,
                columns=int(raw["columns"]), rows=int(raw["rows"]),
                app_version=str(raw.get("app_version", "")), font_name=font_name,
                font_size=font_size, at_shell_prompt=raw.get("at_shell_prompt"),
                tmux_role=role, pane_id=pane, process=process)
        except Exception:
            return self._reject(bridge, "native-metadata-unavailable")

    @staticmethod
    def _parameter(bridge, field, name, value):
        error, result = bridge.ax.AXUIElementCopyParameterizedAttributeValue(
            field, name, value, None)
        if error or result is None:
            raise ValueError("native-input-range-unavailable")
        return result

    @staticmethod
    def _range(bridge, value):
        if value is None:
            raise ValueError("native-input-range-unavailable")
        valid, pair = bridge.ax.AXValueGetValue(value, bridge.ax.kAXValueCFRangeType, None)
        if not valid or len(pair) != 2:
            raise ValueError("native-input-range-unavailable")
        start, length = int(pair[0]), int(pair[1])
        if start < 0 or length < 0:
            raise ValueError("native-input-range-unavailable")
        return start, length

    def _field(self, bridge, metadata):
        app = bridge.ax.AXUIElementCreateApplication(metadata.host_pid)
        field = bridge._attribute(app, "AXFocusedUIElement")
        if (field is None or bridge._attribute(field, "AXRole") != "AXTextArea"
                or bridge._attribute(field, "AXSubrole") == "AXSecureTextField"
                or bridge._attribute(field, "AXProtectedContent")
                or bridge._attribute(field, "AXHidden")
                or bridge._attribute(field, "AXFocused") is False
                or bridge._attribute(field, "AXEnabled") is False):
            raise ValueError("native-input-focus-unverified")
        marked = bridge._attribute(field, "AXMarkedTextRange")
        if marked is None:
            if bridge._ime_kind != "layout":
                raise ValueError("composition-unobservable")
        elif self._range(bridge, marked)[1]:
            raise ValueError("composition-active")
        if self._range(bridge, bridge._attribute(field, "AXSelectedTextRange"))[1]:
            raise ValueError("terminal-selection-active")
        window = bridge._attribute(field, "AXWindow")
        if window is None or bridge._attribute(app, "AXFocusedWindow") != window:
            raise ValueError("native-window-focus-unverified")
        if bridge._native_window_number(metadata.host_pid, bridge._field_bounds(window)) != metadata.window_id:
            raise ValueError("native-window-identity-unverified")
        return field

    def _row(self, bridge, field, line, columns):
        value = self._parameter(bridge, field, "AXRangeForLine", line)
        start, length = self._range(bridge, value)
        # A physical row is bounded before requesting any characters. Never
        # convert a range provider's whole-document answer into a content read.
        if not 0 < length <= min(4096, columns * 2 + 1):
            raise ValueError("native-row-unbounded")
        native = bridge.ax.AXValueCreate(bridge.ax.kAXValueCFRangeType, (start, length))
        text = self._parameter(bridge, field, "AXStringForRange", native)
        if not isinstance(text, str) or index_to_utf16(text, len(text)) != length:
            raise ValueError("native-row-inconsistent")
        if not text.endswith("\n") or "\n" in text[:-1] or "\r" in text:
            raise ValueError("wrapped-or-incomplete-input-row")
        return start, length, text[:-1]

    @staticmethod
    def _printable(text):
        # Formatting controls (including bidi isolates), surrogates and Unicode
        # line separators are unsafe even when str.isprintable differs by OS.
        return all(not unicodedata.category(char).startswith("C")
                   and unicodedata.category(char) not in {"Zl", "Zp"} for char in text)

    def _read(self, bridge, foreground):
        """Read one true current input row and bounded adjacent UI guards."""
        try:
            if (not bridge._terminal_completion or bridge._stopped.is_set()
                    or not bridge.ax.AXIsProcessTrusted()):
                raise ValueError("terminal-opt-in-or-access-required")
            metadata = self.inspect(bridge, foreground)
            if metadata is None:
                return None
            if metadata.app_id == ITERM:
                raise ValueError("iterm-copy-and-broadcast-state-unobservable")
            if metadata.candidate != "agent" or metadata.process is None:
                raise ValueError("foreground-input-owner-unsupported")
            # Pin Codex's source renderer, not the incidental OS app release:
            # native SDEF/range/glyph/window facts must independently validate.
            if (not metadata.process.executable.endswith(
                    "/.codex/packages/standalone/releases/"
                    "0.162.1-aarch64-apple-darwin/bin/codex")
                    or metadata.process.pid != metadata.process.pgid):
                raise ValueError("agent-renderer-version-unverified")
            field = self._field(bridge, metadata)
            selection = self._range(bridge, bridge._attribute(field, "AXSelectedTextRange"))
            if selection[1]:
                raise ValueError("terminal-selection-active")
            location = selection[0]
            line = bridge._attribute(field, "AXInsertionPointLineNumber")
            if not isinstance(line, int) or line < 1:
                raise ValueError("native-cursor-line-unavailable")
            if int(self._parameter(bridge, field, "AXLineForIndex", location)) != line:
                raise ValueError("native-cursor-line-inconsistent")
            start, length, row = self._row(bridge, field, line, metadata.columns)
            if not row.startswith(("› ", "» ")):
                raise ValueError("codex-main-composer-unrecognized")
            text = row[2:]
            # Append-only means the native cursor must be at the actual row end,
            # not in the placeholder, a recalled fragment or a wrapped textarea.
            if location != start + length - 1:
                raise ValueError("codex-cursor-not-at-draft-end")
            caret = utf16_to_index(text, location - start - 2)
            if (not text or not self._printable(text)
                    or any(unicodedata.bidirectional(char) in {"R", "AL", "AN"}
                           for char in text)
                    or text.startswith(("/", "!", "›", "»"))
                    or any(token.startswith(("@", "$")) for token in text.split())
                    or _CODEX_HIDDEN_ELEMENT.search(text)):
                raise ValueError("codex-nonplain-or-popup-input")
            # In this source renderer main composer rows have blank padding and
            # a distinct status row. Views/search/approvals own a different
            # cursor; suggestion menus touch the adjacent padding. No transcript
            # scan or generic prompt/AI detector is used.
            above = self._row(bridge, field, line - 1, metadata.columns)
            below = self._row(bridge, field, line + 1, metadata.columns)
            footer = self._row(bridge, field, line + 2, metadata.columns)
            if (above[2].strip() or below[2].strip()
                    or not self._printable(footer[2])
                    or not _CODEX_FOOTER.fullmatch(footer[2])):
                raise ValueError("codex-modal-or-layout-unverified")
            # The immediate preceding row must be physically adjacent, not a
            # provider alias to a stale line somewhere in scrollback.
            if not (above[0] + above[1] == start
                    and start + length == below[0]
                    and below[0] + below[1] == footer[0]):
                raise ValueError("native-composer-rows-inconsistent")
            total = bridge._attribute(field, "AXNumberOfCharacters")
            if not isinstance(total, int) or footer[0] + footer[1] > total:
                raise ValueError("native-input-extent-inconsistent")
            window = bridge._attribute(field, "AXWindow")
            field_rect = bridge._field_bounds(field)
            window_bounds = bridge._field_bounds(window) if window is not None else None
            # Collapsed Terminal ranges can be (0,1600,0,0). Derive the exact
            # row-end from a genuine non-newline glyph box, never that sentinel.
            glyph_units = index_to_utf16(text[-1:], 1)
            glyph = bridge._range_bounds(field, location - glyph_units, glyph_units)
            whole = bridge._range_bounds(field, start + 2, location - start - 2)
            if (not field_rect or not window_bounds or not glyph or not whole
                    or glyph[2] <= 0 or whole[2] <= 0
                    or abs(glyph[1] - whole[1]) > 0.5
                    or abs(glyph[3] - whole[3]) > 0.5
                    or abs(glyph[0] + glyph[2] - whole[0] - whole[2]) > 0.5):
                raise ValueError("native-glyph-geometry-unverified")
            rect = (glyph[0] + glyph[2], glyph[1], 1.0, glyph[3])
            clip = bridge._visible_clip(field, field_rect)
            if (clip is None or rect[0] < clip[0] or rect[0] + 1 > clip[0] + clip[2]
                    or rect[1] < clip[1] or rect[1] + rect[3] > clip[1] + clip[3] + 1):
                raise ValueError("native-input-offscreen")
            identity = hashlib.sha256(repr(
                (metadata, hash(field), hash(window))).encode()).hexdigest()
            revision = hashlib.sha256(repr(
                (selection, line, start, length, above, below, footer,
                 field_rect, window_bounds, rect)).encode()).hexdigest()
            snapshot = Snapshot(metadata.app_id, "terminal:" + identity, text, caret,
                                caret_rect=rect, multiline=False, field_rect=field_rect,
                                window_id=f"{metadata.host_pid}:{hash(window)}",
                                window_revision=revision, window_pid=metadata.host_pid,
                                window_bounds=window_bounds, can_replace=False)
            if (self._field(bridge, metadata) != field
                    or self.inspect(bridge, foreground) != metadata
                    or self._range(bridge, bridge._attribute(
                        field, "AXSelectedTextRange")) != selection
                    or self._row(bridge, field, line, metadata.columns) != (start, length, row)
                    or self._row(bridge, field, line - 1, metadata.columns) != above
                    or self._row(bridge, field, line + 1, metadata.columns) != below
                    or self._row(bridge, field, line + 2, metadata.columns) != footer
                    or self._field(bridge, metadata) != field
                    or bridge._foreground() != foreground
                    or bridge.secure_input_active() or bridge._dead_key
                    or bridge._stopped.is_set() or not bridge._terminal_completion):
                raise ValueError("terminal-input-changed")
            return snapshot, field, metadata, location, clip
        except Exception as error:
            reason = str(error) if isinstance(error, ValueError) else "native-input-unavailable"
            return self._reject(bridge, reason)

    @staticmethod
    def _register_terminal_font(metadata):
        # Terminal's private font is an existing system resource, not a bundled
        # dependency or an arbitrary path obtained from application metadata.
        if metadata.font_name != "SFMonoTerminal-Regular":
            return
        from AppKit import NSFont
        if NSFont.fontWithName_size_(metadata.font_name, metadata.font_size) is None:
            from Foundation import NSURL
            import CoreText
            url = NSURL.fileURLWithPath_(
                "/System/Applications/Utilities/Terminal.app/Contents/Resources/"
                "Fonts/SFMono-Terminal.ttf")
            CoreText.CTFontManagerRegisterFontsForURL(
                url, CoreText.kCTFontManagerScopeProcess, None)

    def capture(self, bridge, foreground):
        current = self._read(bridge, foreground)
        if current is None:
            self._lease = None
            return None
        snapshot, field, metadata, location, clip = current
        self._lease = snapshot
        render = None
        try:
            self._register_terminal_font(metadata)
            span = snapshot.text[-32:]
            half = len(span) // 2
            if half and span[half:].strip():
                whole = bridge._range_bounds(
                    field, location - index_to_utf16(span, len(span)),
                    index_to_utf16(span, len(span)))
                part_text = span[half:]
                part = bridge._range_bounds(
                    field, location - index_to_utf16(part_text, len(part_text)),
                    index_to_utf16(part_text, len(part_text)))
                def cells(value):
                    return sum(0 if unicodedata.category(char).startswith("M") else
                               2 if unicodedata.east_asian_width(char) in {"W", "F"} else 1
                               for char in value)
                count, part_count = cells(span), cells(part_text)
                advance = whole[2] / count if whole and count else 0.0
                fixed = (advance if part and part_count
                         and abs(part[2] - advance * part_count) <= 0.5 else 0.0)
                evidence = {"font_name": metadata.font_name, "font_size": metadata.font_size,
                            "fixed_advance": fixed,
                            "samples": [{"text": span, "rect": whole},
                                        {"text": part_text, "rect": part}],
                            "caret_rect": snapshot.caret_rect, "clip_rect": clip}
                # Cell boxes alone cannot establish a baseline. Only the
                # existing opt-in local pixel verifier may supply InlineRender.
                render = bridge._styled_render(snapshot, None, evidence)
        except Exception:
            pass  # Genuine input may exist even when appearance cannot be proved.
        if not self.context_matches(bridge, snapshot):
            self._lease = None
            return None
        if render is not None and render.valid(snapshot.caret_rect) and not render.suffix:
            snapshot = replace(snapshot, inline_render=render)
        self._lease = snapshot
        self.diagnostic = ("codex-native-input" if snapshot.inline_safe
                           else "codex-font-pixels-unverified")
        return snapshot, field

    def context_matches(self, bridge, snapshot):
        if (self._lease is None or snapshot.key != self._lease.key
                or snapshot.window_revision != self._lease.window_revision):
            return False
        current = self._read(bridge, bridge._foreground())
        return (current is not None and current[0].key == snapshot.key
                and current[0].window_revision == snapshot.window_revision)

    def append(self, bridge, snapshot, text):
        if (not isinstance(text, str) or not text or not self._printable(text)
                or snapshot.protected or snapshot.selection_length
                or snapshot.can_replace or snapshot.caret != len(snapshot.text)):
            return False
        with bridge._lock:
            try:
                if not self.context_matches(bridge, snapshot):
                    return False
                q = bridge.q
                if not q.CGPreflightPostEventAccess():
                    return False
                flags = q.CGEventSourceFlagsState(q.kCGEventSourceStateCombinedSessionState)
                if flags & (q.kCGEventFlagMaskCommand | q.kCGEventFlagMaskControl
                            | q.kCGEventFlagMaskAlternate):
                    return False
                chunks, chunk, units = [], "", 0
                for char in text:
                    width = 2 if ord(char) > 0xFFFF else 1
                    if units + width > 20:
                        chunks.append((chunk, units))
                        chunk, units = "", 0
                    chunk += char
                    units += width
                if chunk:
                    chunks.append((chunk, units))
                # Permission/modifier queries can race native ownership too.
                if not self.context_matches(bridge, snapshot) or not bridge._commit_input():
                    return False
                self._lease = None  # A suggestion is a single-use append lease.
                for chunk, units in chunks:
                    bridge._post_key(0, 0, (chunk, units))
                return True
            except Exception:
                return False
