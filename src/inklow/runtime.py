"""Menu-agent controller. Native operations belong to tick/the shortcut decision.

No widgets, sockets, model imports, or persistent drafts live in this process.
"""
from __future__ import annotations

from collections import deque
from dataclasses import asdict, replace
import math
from pathlib import Path
import re
import sqlite3
import sys
import threading
import time
import uuid
import unicodedata

from .config import Config, provider_identity, save_config
from .core import SuggestionState
from .features import completion_limits, current_word_only, include_punctuation, mid_word
from .process_client import ProcessClient
from .storage import LocalStorage

_CONTEXT_TTL = 2.0
_DOCS_PROBE_INTERVAL = 2.0
_CONTEXT_MESSAGES = {
    "off": "Optional context is off.",
    "pending": "Reading eligible text from the focused window.",
    "ready": "Optional reference text is available.",
    "empty": "No relevant optional reference text is available.",
    "unavailable": "This field does not expose safe focused-window Accessibility context.",
    "blocked": "Optional context was blocked by current focus or privacy policy.",
    "stale": "Focused-window context changed and was discarded.",
    "error": "Optional context could not be read. Check Accessibility permissions or turn it off.",
    "screen_permission": "Screen Recording permission is needed for screenshot context.",
}
# The per-app/website shortcut toggle is a brief override, not a saved rule.
_TEMPORARY_TOGGLE_SECONDS = 300
# Renders whose font, baseline or colours were measured from an on-device
# window screenshot carry this marker (native.macos_vision sources).
_VISION_SOURCE_MARKER = "+vision"
_CHOOSER_MODES = frozenset({"alternatives", "synonyms", "emoji"})
_COMMAND_ACTIONS = frozenset({"force", "alternatives", "synonyms", "toggle", "app_toggle", "settings"})
_SHORTER = {"long": "medium", "medium": "short", "short": "short", "word": "word"}


class ShutdownPending(RuntimeError):
    """Cancellation succeeded; owned background threads are still unwinding."""


class Controller:
    def __init__(self, config: Config, bridge=None):
        config.validate()
        self.config = config
        if bridge is None:
            from .native import create_bridge
            bridge = create_bridge()
        self.bridge = bridge
        self._rewrites_supported = bool(getattr(
            bridge, "supports_atomic_replacement", not sys.platform.startswith("linux")))
        # A native key path that can replay a consumed shortcut also promises
        # drawn-presentation acknowledgments: keys are consumed only for text
        # the overlay confirmed it drew (macOS AppKit).
        self._fast_keys = callable(getattr(bridge, "replay_action", None))
        self._client = ProcessClient()
        self._storage = LocalStorage()
        self._lock = threading.RLock()
        self._arm_lock = threading.Lock()
        self._armed = {}
        self._presented = {}
        self._deferred = deque(maxlen=32)
        self._events = deque(maxlen=128)
        self._notifications = deque(maxlen=128)
        self._records = deque(maxlen=128)
        self._suggestion = SuggestionState()
        self._candidate = None
        self._candidate_since = 0.0
        self._candidate_revision = None
        self._capture_revision = None
        self._model_activity = None
        self._attempted = None
        self._failed_correction_key = None
        self._request = None
        self._preparing = None
        self._prepared = deque(maxlen=1)
        self._context_pending = None
        self._capture_after = 0.0
        self._capture_interval = 0.1
        self._input_revision = None
        self._context_wake = threading.Event()
        self._context_thread = None
        self._screen_pending = None
        self._screen_job = None
        self._screen_wake = threading.Event()
        self._screen_ready = threading.Event()
        self._screen_thread = None
        self._screen_cache = None
        self._context_status = "off"
        self._choices = []
        self._choice_snapshot = None
        self._choices_anchor = None
        self._replacement = None
        self._correction = None
        self._advance = None
        self._spelling = None
        self._mode = "completion"
        self._completion_counted = False
        self._started = False
        self._stopped = False
        self._shutdown_done = set()
        self._browser_apps = set()
        self._temporary_apps = {}
        self._escape_app = ""
        self._field_pause = None
        self._paused_until = 0.0
        self._app = ""
        self._domain = ""
        self._docs_help = None
        self._docs_domain = ""
        self._docs_scope = None
        self._docs_revision = None
        self._docs_probe_after = 0.0
        self._on_battery = False
        self._battery_checked = 0.0
        self._clipboard_reader = None
        self._screen_reader = None
        self._battery_reader = None
        self._error = ""
        self._commands = deque(maxlen=32)
        self._download = None
        self._download_cancel = None
        self._download_thread = None
        self._session_key = None
        self._session_id = ""
        self._session_text = ""
        self._session_authored = bytearray()
        self._session_domain = ""
        self._session_accepted = False
        self._history_suppressed = None
        self._history_mutating = False
        self.show_settings = None
        self.on_suggestion = None
        self.on_clear = None
        self._wake_ui = None

    def set_wake_callback(self, callback):
        """Install a nonblocking dispatcher onto the UI's owning thread."""
        self._wake_ui = callback
        self._client.on_event = self._notify_ui if callback is not None else None

    def _notify_ui(self):
        callback = self._wake_ui
        if callback is not None and not self._stopped:
            callback()

    def start(self):
        with self._lock:
            if self._started or self._stopped:
                return
            self.bridge.configure(self._native_config())
            self.bridge.start_shortcuts(self.fast_invoke if self._fast_keys else self.invoke)
            input_callback = getattr(self.bridge, "set_input_callback", None)
            if input_callback is not None:
                input_callback(self._notify_ui)
            self.bridge.set_shortcuts_enabled(True)
            self._started = True
        if self._recording_configured(self.config):
            try:
                self._storage.enable_history()
            except RuntimeError as error:
                with self._lock:
                    self._error = str(error)

    @staticmethod
    def _recording_configured(config):
        return config.record_history or any(rule.get("record_history") is True for rule in config.app_rules.values())

    def _emit(self, event):
        self._events.append(event)
        if event["type"] in {"suggestion", "clear"}:
            self._notifications.append(event)

    def drain_events(self):
        with self._lock:
            events = list(self._events)
            self._events.clear()
            return events

    @staticmethod
    def _domain_key(domain):
        return domain.casefold().rstrip(".") if domain else ""

    def _rules(self, app, domain):
        app_rule = self.config.app_rules.get("app:" + app, {})
        domain_rule = self.config.app_rules.get("domain:" + self._domain_key(domain), {}) if domain else {}
        return app_rule, domain_rule

    def _overrides(self):
        now = time.monotonic()
        overrides = {scope: enabled for scope, (enabled, expires) in self._temporary_apps.items() if now < expires}
        if self._escape_app:
            overrides["app:" + self._escape_app] = False
        return overrides

    def _native_config(self):
        overrides = self._overrides()
        if not overrides:
            return self.config
        rules = {key: dict(value) for key, value in self.config.app_rules.items()}
        for scope, enabled in overrides.items():
            rules.setdefault(scope, {})["enabled"] = enabled
        return replace(self.config, app_rules=rules)

    def _temporary(self, scope, now):
        entry = self._temporary_apps.get(scope)
        return entry[0] if entry is not None and now < entry[1] else None

    def _policy_exclusions(self):
        if sys.platform == "darwin":
            from .native.macos_terminal import terminal_policy_exclusions
            return terminal_policy_exclusions(self.config)
        return self.config.excluded_apps

    def _allowed(self, app, domain, name=""):
        if not app:
            return False
        identifiers = (app.casefold(), name.casefold())
        if any(exclusion.casefold() in identifier for exclusion in self._policy_exclusions() for identifier in identifiers):
            return False
        if app == self._escape_app:
            return False
        now = time.monotonic()
        app_override = self._temporary("app:" + app, now)
        if app_override is False:
            return False
        app_rule, domain_rule = self._rules(app, domain)
        enabled = app_rule.get("enabled", self.config.enabled) if app_override is None else app_override
        enabled = domain_rule.get("enabled", enabled)
        if domain:
            domain_override = self._temporary("domain:" + self._domain_key(domain), now)
            if domain_override is not None:
                enabled = domain_override
        return enabled

    def _record_allowed(self, app, domain):
        if (not domain and app in self._browser_apps and any(
                key.startswith("domain:") and rule.get("record_history") is False
                for key, rule in self.config.app_rules.items())):
            return False
        result = self.config.record_history
        for rule in self._rules(app, domain):
            result = rule.get("record_history", result)
        return result and self._allowed(app, domain)

    def _active(self):
        return (not self._stopped and self.config.global_enabled
                and time.monotonic() >= self._paused_until)

    def _foreground(self):
        try:
            info = self.bridge.foreground() or {}
        except Exception:
            self._clear_docs_help()
            return None
        app = str(info.get("app_id", ""))
        domain = str(info.get("domain", "")).casefold().rstrip(".")
        if app:
            if self._escape_app and app != self._escape_app:
                # Escape's "until you switch apps" suppression ends on a switch.
                self._escape_app = ""
                self.bridge.configure(self._native_config())
            self._app, self._domain = app, domain
            if info.get("browser"):
                self._browser_apps.add(app)
        if (info.get("browser") and (not domain or not info.get("domain_observable", bool(domain))) and any(
                key.startswith("domain:") and rule.get("enabled") is False
                for key, rule in self.config.app_rules.items())):
            self._clear_docs_help()
            return None
        allowed = self._allowed(app, domain, str(info.get("name", "")))
        if (not allowed or (self._docs_help is not None
                            and (app != self._docs_help["app_id"] or domain != self._docs_domain))):
            self._clear_docs_help()
        return info if allowed else None

    @staticmethod
    def _selected_word(snapshot):
        if (snapshot is None or snapshot.protected or not snapshot.can_replace
                or not 0 < snapshot.selection_length <= 80
                or snapshot.caret < 0 or snapshot.caret + snapshot.selection_length > len(snapshot.text)):
            return ""
        text = snapshot.text[snapshot.caret:snapshot.caret + snapshot.selection_length]
        normalized = unicodedata.normalize("NFC", text)
        return text if re.fullmatch(
            r"[^\W\d_]+(?:[’'-][^\W\d_]+)*(?: [^\W\d_]+(?:[’'-][^\W\d_]+)*){0,4}", normalized, re.UNICODE) else ""

    def _user_revision(self):
        """O(1) count of user input (not our own insertions); None disables arming."""
        reader = getattr(self.bridge, "user_input_revision", None)
        try:
            value = reader() if reader is not None else None
        except Exception:
            return None
        return value if type(value) is int else None

    def _capture(self):
        if self._foreground() is None or not self._active():
            return None
        revision = self._user_revision()
        try:
            snapshot = self.bridge.capture()
        except Exception:
            return None
        if snapshot is None or snapshot.protected or snapshot.app_id != self._app:
            return None
        domain = getattr(snapshot, "domain", "").casefold().rstrip(".")
        if not self._allowed(snapshot.app_id, domain):
            return None
        # The metadata captured before the text must still identify the domain.
        if self._domain != domain:
            return None
        if not (0 <= snapshot.caret <= len(snapshot.text)):
            return None
        render = snapshot.inline_render
        if (render is not None and _VISION_SOURCE_MARKER in render.source
                and not self.config.screenshot_appearance):
            # Screenshot-derived appearance needs its own explicit opt-in.
            snapshot = replace(snapshot, inline_render=None)
        self._capture_revision = revision
        return snapshot

    def _clear_docs_help(self):
        if self._docs_help is not None:
            self._docs_help = None
            self._docs_domain = ""
            self._emit({"type": "docs_help", "hint": None})

    def _docs_paused(self):
        pause = self._field_pause
        return bool(pause is not None and time.monotonic() < pause[1]
                    and pause[0][0] == self._app and pause[0][2] == self._domain)

    def _docs_context(self, info):
        """Revoke capabilities cheaply; never perform Docs discovery here."""
        revision_reader = getattr(self.bridge, "input_revision", None)
        secure_reader = getattr(self.bridge, "secure_input_active", None)
        try:
            revision = revision_reader() if revision_reader else self._user_revision()
            secure = bool(secure_reader()) if secure_reader else False
        except Exception:
            revision, secure = None, True
        scope = (tuple(info.get(key) for key in (
            "app_id", "domain", "pid", "window_id", "document_id")), id(self.config)) if info else None
        if scope != self._docs_scope or revision != self._docs_revision or secure:
            if self._docs_help is not None:
                self._docs_probe_after = max(
                    self._docs_probe_after, time.monotonic() + _DOCS_PROBE_INTERVAL)
            self._clear_docs_help()
        self._docs_scope, self._docs_revision = scope, revision
        eligible = bool(info and not secure and self._active() and not self._docs_paused())
        if not eligible:
            self._clear_docs_help()
        return eligible

    def _refresh_docs_help(self, snapshot, info):
        # Discovery has its own positive/negative TTL, not the typing capture
        # cadence. Input invalidates the visible token, never this probe budget.
        reader = getattr(self.bridge, "docs_hint", None)
        if snapshot is not None or reader is None or not self._docs_context(info):
            self._clear_docs_help()
            return
        now = time.monotonic()
        if now < self._docs_probe_after:
            return
        self._docs_probe_after = now + _DOCS_PROBE_INTERVAL
        scope, revision = self._docs_scope, self._docs_revision
        try:
            hint = reader()
        except Exception:
            hint = None
        if (not self._docs_context(info) or self._docs_scope != scope
                or self._docs_revision != revision):
            return
        if (not isinstance(hint, dict) or not hint.get("token")
                or hint.get("app_id") != self._app or hint.get("support") != "unknown"):
            self._clear_docs_help()
            return
        if hint != self._docs_help or self._domain != self._docs_domain:
            self._docs_help = dict(hint)
            self._docs_domain = self._domain
            self._emit({"type": "docs_help", "hint": dict(hint)})

    def enable_docs_accessibility(self, token):
        """Consume an explicit current-document action, never a typing shortcut."""
        with self._lock:
            hint = self._docs_help
            unknown = {"status": "unknown", "diagnostic": "stale-capability", "help_url": ""}
            info = self._foreground()
            if (not self._docs_context(info) or hint is None or token != hint["token"]
                    or self._docs_help is not hint):
                self._clear_docs_help()
                return unknown
            enable = getattr(self.bridge, "enable_docs_accessibility", None)
            self._clear_docs_help()
            self._capture_after = 0.0
            if enable is None:
                return unknown
            try:
                result = enable(token)
            except Exception:
                return unknown
            if not isinstance(result, dict) or result.get("status") not in {"enabled", "already_enabled", "unknown"}:
                return unknown
            return result

    def _disarm(self, *kinds):
        with self._arm_lock:
            self._armed = {kind: entry for kind, entry in self._armed.items() if kinds and kind not in kinds}
        self._presented = {kind: entry for kind, entry in self._presented.items() if kinds and kind not in kinds}

    def _clear(self, *, hide_accessory=False, cancel=True, cancel_context=True):
        self._clear_docs_help()
        if cancel:
            self._client.cancel()
        self._request = None
        self._preparing = None
        self._context_pending = None
        self._prepared.clear()
        if cancel_context:
            self._screen_pending = None
            self._screen_job = None
            self._context_status = "blocked" if self._window_context_enabled() or self.config.clipboard_context else "off"
        self._suggestion.invalidate()
        self._choices = []
        self._choice_snapshot = None
        self._choices_anchor = None
        self._replacement = None
        self._correction = None
        self._advance = None
        self._disarm()
        self._emit({"type": "clear", "hide_accessory": hide_accessory})

    def _clear_choices(self):
        if not self._choices:
            return
        self._choices = []
        self._choice_snapshot = None
        self._choices_anchor = None
        self._replacement = None
        self._mode = "completion"
        self._disarm("choices")
        self._emit({"type": "clear", "panel": "chooser"})

    def _authored_spans(self):
        spans = []
        start = None
        for index, authored in enumerate(self._session_authored):
            if authored and start is None:
                start = index
            elif not authored and start is not None:
                spans.append((start, index))
                start = None
        if start is not None:
            spans.append((start, len(self._session_authored)))
        return tuple(spans) if len(spans) <= 1024 else ()

    def _update_session_text(self, text, *, authored):
        """Carry provenance through one contiguous observed edit.

        Initial/native-loaded drafts are unknown. Only small observed edits are
        evidence of typing/corrections; bulk replacements/pastes abstain. Every
        app-generated insertion/replacement is explicitly marked non-authored.
        Unchanged characters retain their provenance, including generated text.
        """
        previous = self._session_text
        if previous == text:
            return
        start = 0
        while start < min(len(previous), len(text)) and previous[start] == text[start]:
            start += 1
        tail = 0
        while (tail < min(len(previous), len(text)) - start
               and previous[len(previous) - tail - 1] == text[len(text) - tail - 1]):
            tail += 1
        old_end, new_end = len(previous) - tail, len(text) - tail
        observed = authored and max(old_end - start, new_end - start) <= 64
        self._session_authored[start:old_end] = bytes([int(observed)]) * (new_end - start)
        self._session_text = text

    def _queue_session(self):
        app = self._session_key[0]
        self._records.append(("history", self._session_id, app, self._session_domain,
                              self._session_text, self._authored_spans()))

    def _finish_session(self):
        if self._session_key and (self._session_accepted or self.config.record_unaccepted):
            app = self._session_key[0]
            if self._record_allowed(app, self._session_domain):
                self._queue_session()
        self._session_key = None
        self._session_text = ""
        self._session_authored.clear()
        self._session_accepted = False

    def _observe_session(self, snapshot):
        key = (snapshot.app_id, snapshot.field_id, getattr(snapshot, "domain", ""))
        if self._history_suppressed == (key, snapshot.text):
            return
        self._history_suppressed = None
        if not self._record_allowed(snapshot.app_id, getattr(snapshot, "domain", "")):
            self._finish_session()
            return
        if len(snapshot.text) > 32000:
            self._finish_session()
            return
        if key != self._session_key:
            self._finish_session()
            self._session_key = key
            self._session_id = uuid.uuid4().hex
            self._session_domain = getattr(snapshot, "domain", "")
            self._session_text = snapshot.text
            self._session_authored = bytearray(len(snapshot.text))
        else:
            self._update_session_text(snapshot.text, authored=True)

    def _observe(self, snapshot):
        now = time.monotonic()
        if snapshot is None:
            self._screen_cache = None
            if self._candidate is not None:
                self._finish_session()
                self._clear(hide_accessory=True)
            self._candidate = None
            self._attempted = None
            return
        if snapshot.selection_length:
            # Selected ranges are never normal completion/history contexts.
            self._finish_session()
        elif snapshot.eligible(1):
            self._observe_session(snapshot)
        previous = self._candidate
        if previous is not None and previous.key == snapshot.key:
            self._candidate = snapshot
            previous_revision, self._candidate_revision = self._candidate_revision, self._capture_revision
            geometry_changed = (previous.caret_rect != snapshot.caret_rect or previous.field_rect != snapshot.field_rect
                                or previous.native_anchor != snapshot.native_anchor
                                or previous.inline_render != snapshot.inline_render
                                or previous.native_inline != snapshot.native_inline)
            # User input (e.g. a click) without a text change disarmed the drawn
            # text; redraw it so the overlay can confirm it for the new revision.
            rearm = self._fast_keys and previous_revision != self._candidate_revision and bool(self._presented)
            if geometry_changed or rearm:
                if ((snapshot.has_anchor and not previous.has_anchor)
                        or (snapshot.inline_safe and not previous.inline_safe)):
                    self._attempted = None
                if self._correction is not None:
                    self._publish_correction(snapshot, self._correction[1])
                elif self._suggestion.snapshot is not None:
                    self._suggestion.offer(self._suggestion.generation, snapshot, self._suggestion.text)
                    self._publish_completion()
                elif self._choices:
                    self._publish_choices(snapshot, self._choices, self._mode, self._replacement)
                self._publish_caret(snapshot)
            self._queue_window_context(snapshot)
            return
        previous_since = self._candidate_since
        self._candidate_since = now
        if (previous is not None and previous.app_id == snapshot.app_id
                and previous.field_id == snapshot.field_id):
            timestamp_reader = getattr(self.bridge, "input_timestamp", None)
            try:
                input_at = timestamp_reader() if timestamp_reader is not None else 0.0
            except Exception:
                input_at = 0.0
            # AX may publish a key's new value after its native input notification.
            # Count the typing pause from that input, not the later polling tick.
            if (type(input_at) in (int, float) and 0 <= now - input_at <= 0.2
                    and input_at >= previous_since):
                self._candidate_since = input_at
        self._attempted = None
        self._candidate = snapshot
        self._candidate_revision = self._capture_revision
        if self._correction is None and self._suggestion.advance(snapshot):
            self._client.cancel()
            self._request = None
            self._publish_completion()
        else:
            self._clear()
        self._publish_caret(snapshot)
        self._queue_window_context(snapshot)

    def _publish_caret(self, snapshot):
        if snapshot.has_anchor:
            self._emit({"type": "caret", "rect": list(snapshot.caret_rect) if snapshot.caret_rect else None,
                        "field_rect": list(snapshot.field_rect) if snapshot.field_rect else None,
                        "native_anchor": snapshot.native_anchor})
        else:
            self._emit({"type": "clear", "hide_accessory": True})

    @staticmethod
    def _window_key(snapshot):
        return (snapshot.app_id, getattr(snapshot, "domain", ""),
                snapshot.window_id, snapshot.window_revision)

    def _window_context_enabled(self):
        return bool(self.config.screen_context or self.config.screenshot_context)

    def _provenance_allowed(self, provenance):
        return (provenance == "" or (provenance == "accessibility" and self.config.screen_context)
                or (provenance == "screenshot" and self.config.screenshot_context))

    def _context_current(self, token, snapshot, kind):
        expected = self._screen_job if kind == "screen" else self._preparing
        domain = getattr(snapshot, "domain", "")
        if kind == "personalization":
            # History is read only under the recording consent that applies now.
            enabled = (not self._history_mutating and bool(self.config.personalization_strength)
                       and self._record_allowed(snapshot.app_id, domain))
        elif kind.startswith("screen"):
            enabled = self._window_context_enabled()
        else:
            enabled = self.config.clipboard_context
        return (token is not None and expected == token and self._active() and enabled
                and not snapshot.protected
                and self._candidate is not None and self._candidate.key == snapshot.key
                and self._window_key(self._candidate) == self._window_key(snapshot)
                and self._allowed(snapshot.app_id, domain))

    def _context_allowed(self, token, snapshot, kind):
        # Re-read consent/exclusions after asynchronous waits and on each native
        # read. A stale candidate is not proof that the current field is safe.
        with self._lock:
            if not self._context_current(token, snapshot, kind):
                return False
        matches = getattr(self.bridge, "context_matches", None)
        try:
            safe = matches is not None and matches(snapshot)
        except Exception:
            safe = False
        with self._lock:
            return bool(safe and self._context_current(token, snapshot, kind))

    def _queue_window_context(self, snapshot, *, fresh=False):
        if not self._window_context_enabled():
            if not self.config.clipboard_context:
                self._context_status = "off"
            return
        if not snapshot.prefix.strip():
            self._screen_cache = None
            self._context_status = "empty"
            return
        if (not self._active() or snapshot.protected or snapshot.selection_length
                or not snapshot.window_id or not snapshot.window_revision):
            self._context_status = "unavailable"
            return
        key = self._window_key(snapshot)
        if fresh:
            # AX window identity/geometry is not a content revision. An actual
            # prediction must not inherit earlier reference text in that window.
            self._screen_cache = None
            if self.config.screenshot_context and "inklow.native.macos_vision" in sys.modules:
                sys.modules["inklow.native.macos_vision"].clear_text_cache()
        if self._screen_cache is not None and self._screen_cache[0] == key:
            if time.monotonic() - self._screen_cache[1] <= _CONTEXT_TTL:
                self._context_status = self._screen_cache[4]
                return
        if (not fresh and self._screen_pending is not None and self._screen_pending[1].key == snapshot.key
                and self._window_key(self._screen_pending[1]) == key):
            return
        if (not fresh and self._screen_job is not None and self._screen_job[1] == snapshot.key
                and self._screen_job[2] == key):
            return
        if self._screen_reader is None:
            self._context_status = "unavailable"
            return
        token = (object(), snapshot.key, key)
        self._screen_job = token
        self._screen_pending = (token, snapshot)
        self._screen_ready.clear()
        self._context_status = "pending"
        if self._screen_thread is None:
            self._screen_thread = threading.Thread(
                target=self._screen_loop, daemon=True, name="inklow-focused-window-context")
            self._screen_thread.start()
        self._screen_wake.set()

    def _screen_loop(self):
        while True:
            self._screen_wake.wait()
            with self._lock:
                if self._stopped:
                    return
                request = self._screen_pending
                self._screen_pending = None
                self._screen_wake.clear()
            if request is not None:
                self._read_window_context(*request)

    def _read_window_context(self, token, snapshot):
        def blocked():
            with self._lock:
                if self._screen_job == token:
                    self._screen_job = None
                    self._context_status = "blocked"
                    self._screen_cache = (self._window_key(snapshot), time.monotonic(), "", "", "blocked")
                    self._screen_ready.set()
        guard = lambda: self._context_allowed(token, snapshot, "screen")
        if not guard():
            blocked()
            return
        with self._lock:
            accessibility = bool(self.config.screen_context)
            screenshots = bool(self.config.screenshot_context)
        try:
            result = self._screen_reader(
                snapshot, bridge=self.bridge, authorized=guard, max_chars=2400,
                accessibility=accessibility, screenshot_context=screenshots)
            if not isinstance(result, dict):
                raise ValueError("Invalid context adapter result")
            status = result.get("status", "empty")
            if status not in _CONTEXT_MESSAGES:
                status = "error"
            if (result.get("window_id") != snapshot.window_id
                    or result.get("revision") != snapshot.window_revision):
                status = "stale"
            text = result.get("text", "") if status == "ready" else ""
            provenance = result.get("provenance", "")
            if not isinstance(text, str) or provenance not in {"", "accessibility", "screenshot"}:
                status, text, provenance = "error", "", ""
        except Exception:
            # Fixed content-free status only: native exception bodies can quote
            # private writing, clipboard entries, app names or window titles.
            status, text, provenance = "error", "", ""
        if not guard():
            blocked()
            return
        with self._lock:
            if not self._context_current(token, snapshot, "screen"):
                return
            if not self._provenance_allowed(provenance):
                # The source consent changed (or the adapter used one it lacked).
                blocked()
                return
            self._screen_cache = (self._window_key(snapshot), time.monotonic(),
                                  provenance, text[:2400], status)
            self._context_status = status
            self._screen_job = None
            self._screen_ready.set()

    def _reference_options(self, snapshot, references, *, use_window=True):
        from .writing import reference_context
        references = list(references)
        cached = self._screen_cache
        if (use_window and self._window_context_enabled() and cached is not None
                and cached[0] == self._window_key(snapshot) and self._provenance_allowed(cached[2])
                and time.monotonic() - cached[1] <= _CONTEXT_TTL and cached[4] == "ready"):
            references.append((cached[2], cached[3]))
        return reference_context(snapshot.prefix, snapshot.suffix, references)

    def _length(self):
        length = self.config.completion_length
        return _SHORTER[length] if self._on_battery and self.config.battery_shorter else length

    def _options(self, snapshot, mode, token, anchor=""):
        domain = getattr(snapshot, "domain", "")
        app_rule, domain_rule = self._rules(snapshot.app_id, domain)
        instructions = "\n".join(value for value in (
            self.config.custom_instructions, app_rule.get("custom_instructions", ""), domain_rule.get("custom_instructions", "")) if value)
        warning = ""
        personalization = ""
        strength = self.config.personalization_strength
        if strength:
            try:
                if self._context_allowed(token, snapshot, "personalization"):
                    evidence = self._storage.personalization(
                        strength, snapshot.app_id, domain, text=snapshot.prefix,
                        source_allowed=self._record_allowed)
                    if evidence and self._context_allowed(token, snapshot, "personalization"):
                        personalization = evidence
            except (RuntimeError, OSError, sqlite3.Error):
                warning = "Encrypted history is unavailable for personalization. Unlock your OS credential store or clear damaged history."
        context = []
        guard = lambda: self._context_allowed(token, snapshot, "clipboard")
        if self._clipboard_reader is not None and guard():
            try:
                text = self._clipboard_reader(authorized=guard)
                if guard() and isinstance(text, str) and text:
                    with self._lock:
                        if self._context_current(token, snapshot, "clipboard"):
                            context.append(("clipboard", text[:4000]))
                            self._context_status = "ready"
            except Exception:
                # Native errors can echo clipboard/writing; never surface them.
                with self._lock:
                    if self._preparing == token:
                        self._context_status = "error"
        with self._lock:
            reference = self._reference_options(snapshot, context)
            length = self._length()
        options = {"instructions": instructions[:26000], "context": reference, "_references": context,
                   "personalization": personalization, "max_words": completion_limits(length)[1]}
        if warning:
            options["_warning"] = warning
        if mode == "alternatives":
            options.update(alternatives=True, anchor=anchor)
        elif mode == "synonyms":
            from .features import synonym_prompt
            selected = self._selected_word(snapshot)
            suffix = snapshot.text[snapshot.caret + snapshot.selection_length:]
            options["utility"] = synonym_prompt(snapshot.prefix, suffix, selected)
        return options

    def _effective_config(self, mode):
        config = replace(self.config, max_tokens=min(self.config.max_tokens, completion_limits(self._length())[0]))
        if mode != "completion":
            config.max_tokens = 128
        battery_model = self.config.battery_model
        if self._on_battery and battery_model != "same" and config.backend == "gguf":
            from .models import get_preset, is_installed, model_path
            try:
                preset = get_preset(battery_model)
            except ValueError:
                preset = None
            if preset is not None and is_installed(preset):
                config = replace(config, model_path=str(model_path(preset)), prompt_style=preset.prompt_style,
                                 model_id=preset.id)
            else:
                self._error = "The battery model is not installed; the selected model is still used."
        return config

    def _synonyms_available(self, config=None):
        config = self._effective_config("synonyms") if config is None else config
        return config.backend != "gguf" or config.prompt_style == "writing"

    def _can_replace(self, snapshot):
        return self._rewrites_supported and snapshot.can_replace

    def _submit(self, snapshot, mode="completion", anchor=""):
        if mode == "completion" and not snapshot.inline_safe:
            return False
        if mode in {"emoji", "synonyms"} and not self._can_replace(snapshot):
            return False
        if mode == "synonyms" and not self._synonyms_available():
            self._attempted = (snapshot.key, mode)
            return False
        anchored = bool(mode == "alternatives" and anchor and self._suggestion.snapshot is not None
                        and self._suggestion.snapshot.key == snapshot.key and self._suggestion.text)
        if anchored:
            # Alternatives for the visible completion keep that completion on screen.
            self._client.cancel()
            self._request = None
            self._preparing = None
            self._context_pending = None
            self._prepared.clear()
            self._clear_choices()
        else:
            self._clear(cancel_context=False)
            self._completion_counted = False
        self._error = ""
        self._mode = mode
        self._choices_anchor = snapshot.key if anchored else None
        self._attempted = (snapshot.key, mode)
        token = (snapshot.key, self._suggestion.generation, mode)
        self._preparing = token

        self._queue_window_context(snapshot, fresh=True)
        self._context_pending = (token, snapshot, mode, anchor if anchored else "")
        if self._context_thread is None:
            self._context_thread = threading.Thread(target=self._context_loop, daemon=True, name="inklow-optional-context")
            self._context_thread.start()
        self._context_wake.set()
        return True

    def _prepare_options(self, token, snapshot, mode, anchor=""):
        try:
            with self._lock:
                if self._preparing != token or not self._active():
                    return
                awaiting_window = (self._window_context_enabled() and self._screen_job is not None
                                   and self._screen_job[1] == snapshot.key)
            if awaiting_window:
                # Optional native context may not stall typing indefinitely.
                # The previous cache was invalidated before this request.
                self._screen_ready.wait(0.25)
            options = self._options(snapshot, mode, token, anchor)
        except Exception:
            options = None
        with self._lock:
            if self._preparing == token and not self._stopped:
                self._prepared.append((token, snapshot, options))
            else:
                return
        self._notify_ui()

    def _context_loop(self):
        while True:
            self._context_wake.wait()
            with self._lock:
                if self._stopped:
                    return
                request = self._context_pending
                self._context_pending = None
                self._context_wake.clear()
            if request is not None:
                self._prepare_options(*request)

    def _consume_prepared(self):
        while self._prepared:
            token, snapshot, options = self._prepared.popleft()
            if token != self._preparing:
                continue
            # Keep the preparation token valid until optional-context freshness
            # is checked at the actual model-submission boundary.
            current = self._capture()
            if options is None:
                self._error = "Could not prepare local writing preferences."
                self._preparing = None
                continue
            if (current is None or not current.has_anchor or current.key != snapshot.key
                    or self._window_key(current) != self._window_key(snapshot)
                    or (token[2] == "completion" and not current.inline_safe)):
                self._clear(hide_accessory=True)
                continue
            references = options.pop("_references", [])
            if not self._context_allowed(token, current, "clipboard"):
                references = []
            options["context"] = self._reference_options(
                current, references, use_window=self._context_allowed(token, current, "screen_cache"))
            self._preparing = None
            self._error = options.pop("_warning", "")
            mode = token[2]
            length = self._length()
            try:
                config = self._effective_config(mode)
                if mode == "synonyms" and not self._synonyms_available(config):
                    continue
                identifier = self._client.submit(config, snapshot.prefix, snapshot.suffix, **options)
            except (RuntimeError, ValueError) as error:
                self._error = str(error)
                continue
            self._request = (identifier, snapshot, token[1], mode, length)

    def _anchored_choices(self):
        snapshot = self._suggestion.snapshot
        return bool(self._choices and self._choices_anchor is not None and snapshot is not None
                    and self._suggestion.text and self._choices_anchor == snapshot.key)

    def _publish_completion(self):
        snapshot, text = self._suggestion.snapshot, self._suggestion.text
        # Any new or moved drawing must be acknowledged again before keys act on it.
        self._disarm("completion")
        geometry, advance = snapshot, self._advance
        if (snapshot is not None and not snapshot.inline_safe and advance is not None
                and advance[0] == snapshot.key):
            # Just after a word was inserted the host has not reported its new
            # caret yet: keep drawing the remainder from the verified run,
            # shifted by the accepted text, instead of flickering it away.
            geometry = advance[1]
        else:
            advance = self._advance = None
        if snapshot is None or not text or not geometry.inline_safe:
            self._clear_choices()
            self._emit({"type": "clear"})
            return
        if self._choices and not self._anchored_choices():
            self._clear_choices()
        choices = list(self._choices) if self._choices else []
        event = {"type": "suggestion", "text": text, "key": snapshot.key,
                 "rect": list(geometry.anchor_rect) if geometry.anchor_rect else None,
                 "native_anchor": geometry.native_anchor, "choices": choices,
                 "field_rect": list(geometry.field_rect) if geometry.field_rect else None,
                 "field_anchor": geometry.caret_rect is None,
                 "mode": "completion",
                 "inline": True,
                 "render": asdict(geometry.inline_render) if geometry.inline_render else None}
        if choices:
            event["choices_mode"] = "alternatives"
        if advance is not None:
            event["advance"] = advance[2]
        self._emit(event)

    def _publish_choices(self, snapshot, choices, mode, replacement):
        if ((mode in {"emoji", "synonyms"} or replacement != (snapshot.caret, 0))
                and not self._can_replace(snapshot)):
            return
        anchored = bool(mode == "alternatives" and self._choices_anchor is not None
                        and self._choices_anchor == snapshot.key
                        and self._suggestion.snapshot is not None
                        and self._suggestion.snapshot.key == snapshot.key and self._suggestion.text)
        if not anchored:
            self._suggestion.invalidate()
            self._choices_anchor = None
            self._advance = None
            self._disarm("completion")
        self._choices = choices[:5]
        self._choice_snapshot = snapshot
        self._replacement = replacement
        self._mode = mode
        self._disarm("choices")
        if anchored:
            self._publish_completion()
            return
        self._completion_counted = False
        if snapshot.has_anchor and self._choices:
            self._emit({"type": "suggestion", "text": self._choices[0], "key": snapshot.key,
                        "rect": list(snapshot.anchor_rect) if snapshot.anchor_rect else None,
                        "native_anchor": snapshot.native_anchor,
                        "field_rect": list(snapshot.field_rect) if snapshot.field_rect else None,
                        "field_anchor": snapshot.caret_rect is None,
                        "choices": list(self._choices), "mode": mode})

    def _typo_active(self, snapshot):
        return bool(self.config.typo_suppression
                    or (self.config.typo_fixes and self._can_replace(snapshot)))

    def _assess(self, snapshot):
        """Latest-only local spelling result for this exact snapshot, or None while pending."""
        service = self._spelling
        if service is None:
            from .spelling import SpellingService
            service = self._spelling = SpellingService(wake=self._notify_ui)
        try:
            result = service.assess(snapshot)
        except Exception:
            return None
        return result if result is not None and result.key == snapshot.key else None

    def _completion_blocked(self, snapshot):
        """A suspected typo in the current word withholds the ordinary completion."""
        if not self._typo_active(snapshot):
            return False
        if self._correction is not None and self._correction[0].key == snapshot.key:
            return True
        result = self._assess(snapshot)
        return result is None or (result.suspected and bool(self.config.typo_suppression))

    @staticmethod
    def _rect(value):
        try:
            rect = tuple(float(item) for item in value)
        except (TypeError, ValueError):
            return None
        return rect if len(rect) == 4 and all(math.isfinite(item) for item in rect) and rect[2] > 0 and rect[3] > 0 else None

    def _publish_correction(self, snapshot, result, rect=None):
        """Show a strike-through fix on the same line only with verified range geometry."""
        if not self._can_replace(snapshot):
            return False
        if rect is None:
            reader = getattr(self.bridge, "range_rect", None)
            try:
                rect = self._rect(reader(snapshot, result.start, result.length)) if reader is not None else None
            except Exception:
                rect = None
        if (rect is None or not snapshot.inline_safe or not snapshot.inline_render
                or result.key != snapshot.key):
            if self._correction is not None:
                self._correction = None
                self._disarm("correction")
                self._emit({"type": "clear"})
            return False
        self._correction = (snapshot, result)
        self._disarm("correction")
        self._emit({"type": "suggestion", "mode": "correction", "key": snapshot.key, "text": result.replacement,
                    "rect": list(snapshot.anchor_rect) if snapshot.anchor_rect else None,
                    "native_anchor": snapshot.native_anchor, "choices": [],
                    "field_rect": list(snapshot.field_rect) if snapshot.field_rect else None,
                    "field_anchor": False, "inline": True,
                    "render": asdict(snapshot.inline_render),
                    "correction": {"start": result.start, "length": result.length, "word": result.word,
                                   "replacement": result.replacement, "language": result.language,
                                   "rect": list(rect)}})
        return True

    def _show_correction(self, snapshot, result):
        if not (self.config.typo_fixes and result.replacement and self._can_replace(snapshot)):
            return False
        if self._correction is not None and self._correction[0].key == snapshot.key:
            return True
        if self._failed_correction_key == snapshot.key:
            return False
        reader = getattr(self.bridge, "range_rect", None)
        try:
            rect = self._rect(reader(snapshot, result.start, result.length)) if reader is not None else None
        except Exception:
            rect = None
        if (rect is None or not snapshot.inline_safe or not snapshot.inline_render
                or result.key != snapshot.key):
            self._failed_correction_key = snapshot.key
            return False
        self._clear()
        self._attempted = (snapshot.key, "correction")
        return self._publish_correction(snapshot, result, rect)

    def _consume_results(self):
        for event in self._client.poll():
            request = self._request
            if request is None or event.get("id") != request[0]:
                continue
            _, snapshot, generation, mode, length = request
            current = self._candidate
            if current is None or current.key != snapshot.key or not self._active():
                continue
            snapshot = current
            if event.get("type") == "error":
                self._error = event.get("message", "Local inference failed.")
                self._clear(cancel=False)
            elif event.get("type") == "result":
                text = event.get("text", "")
                if mode == "completion":
                    if length == "word":
                        text = current_word_only(snapshot.prefix, text)
                    if (not self._completion_blocked(snapshot)
                            and self._suggestion.offer(generation, snapshot, text)):
                        self._publish_completion()
                elif event.get("final"):
                    from .features import parse_choices
                    choices = parse_choices(text, max_choices=5)
                    if mode == "synonyms":
                        selected = self._selected_word(snapshot)
                        choices = [choice for choice in choices if choice.casefold() != selected.casefold()]
                        replacement = (snapshot.caret, snapshot.selection_length)
                    else:
                        replacement = (snapshot.caret, 0)
                    if choices:
                        self._publish_choices(snapshot, choices, mode, replacement)
                if event.get("final"):
                    self._request = None

    def _flush_records(self, *, raise_errors=False):
        failed = False
        while True:
            with self._lock:
                if not self._records:
                    break
                record = self._records.popleft()
                allowed = self.config.statistics_enabled if record[0] == "stats" else self._record_allowed(record[2], record[3])
            if not allowed:
                continue
            try:
                if record[0] == "stats":
                    _, app, text, completion = record
                    self._storage.record_acceptance(app, text, completion=completion)
                else:
                    _, identifier, app, domain, text, authored_spans = record
                    self._storage.record_history(identifier, app, text, domain=domain,
                                                 authored_spans=authored_spans)
            except (RuntimeError, OSError, sqlite3.Error) as error:
                failed = True
                with self._lock:
                    self._error = str(error) if not raise_errors else "Local history or statistics could not be saved."
        if failed and raise_errors:
            raise RuntimeError("Local history or statistics could not be saved.")

    def _field_paused(self, snapshot):
        pause = self._field_pause
        if pause is None:
            return False
        if time.monotonic() >= pause[1]:
            self._field_pause = None
            return False
        return pause[0] == (snapshot.app_id, snapshot.field_id, getattr(snapshot, "domain", ""))

    def _prepare_focused_model(self, snapshot):
        # Capture has already established current foreground/privacy authority.
        # Remember activity, not worker state: idle release must remain idle
        # until a new focus or real input arrives.
        if snapshot is None:
            return
        # An empty native control has no glyph run/caret to measure yet. Loading
        # weights needs only its writable field/window authority, not permission
        # to evaluate writing or draw; those still require verified inline layout.
        empty_native_field = (
            not snapshot.text and snapshot.caret == 0 and snapshot.window_pid > 0
            and bool(snapshot.window_id) and self._rect(snapshot.field_rect) is not None)
        if (not self._active() or snapshot.protected
                or not snapshot.multiline or not snapshot.can_replace
                or not (snapshot.inline_safe or empty_native_field) or not snapshot.has_anchor
                or snapshot.selection_length or snapshot.on_demand_only
                or self._field_paused(snapshot)
                or (self._on_battery and self.config.battery_on_demand)):
            # Temporary Escape suppression or unavailable caret geometry is not
            # an idle-timeout/privacy revocation. Keep the shared worker warm.
            return
        activity = (snapshot.app_id, snapshot.field_id, snapshot.domain,
                    self._capture_revision, hash(snapshot.text), snapshot.caret)
        if activity == self._model_activity:
            return
        self._model_activity = activity
        config = self._effective_config("completion")
        if config.backend == "gguf" and Path(config.model_path).expanduser().is_file():
            self._client.prepare(config)

    def _completion_eligible(self, snapshot):
        if not (snapshot.inline_safe and snapshot.eligible(self.config.min_chars)):
            return False
        same_line = re.split(r"[\r\n]", snapshot.suffix, maxsplit=1)[0]
        if same_line.strip() and not self.config.complete_inside_text:
            return False
        if self.config.completion_length == "word" and not mid_word(snapshot.prefix):
            return False
        return not self._field_paused(snapshot)

    def _expire_temporary(self):
        now = time.monotonic()
        expired = [scope for scope, (_, expires) in self._temporary_apps.items() if now >= expires]
        for scope in expired:
            del self._temporary_apps[scope]
        if expired:
            self._attempted = None
            self.bridge.configure(self._native_config())

    def tick(self):
        settings_callbacks = []
        if not self._started:
            self.start()
        with self._lock:
            if self._stopped:
                return
            self._run_deferred()
            commands = list(self._commands)
            self._commands.clear()
        for command in commands:
            action = command[0] if isinstance(command, tuple) else command
            if action == "toggle":
                self.toggle_enabled()
            elif action == "app_toggle":
                if isinstance(command, tuple):
                    self._toggle_app(command[1], command[2])
                else:
                    self.toggle_current_app()
            elif action == "settings":
                settings_callbacks.append(self.show_settings)
            elif action in {"force", "alternatives", "synonyms"}:
                with self._lock:
                    snapshot = self._capture()
                    if snapshot is not None:
                        if isinstance(command, tuple) and snapshot.key != command[1]:
                            continue
                        if isinstance(command, tuple) and len(command) > 2 and self._user_revision() != command[2]:
                            continue
                        if not snapshot.has_anchor:
                            continue
                        mode = "completion" if action == "force" else action
                        if ((mode == "completion" and snapshot.eligible(1))
                                or (mode == "alternatives" and snapshot.eligible(1))
                                or (mode == "synonyms" and self._selected_word(snapshot))):
                            anchor = self._suggestion.text if (
                                mode == "alternatives" and self._suggestion.snapshot is not None
                                and self._suggestion.snapshot.key == snapshot.key) else ""
                            self._observe(snapshot)
                            self._submit(snapshot, mode, anchor)
        now = time.monotonic()
        if self._battery_reader is not None and now - self._battery_checked >= 5:
            self._battery_checked = now
            try:
                self._on_battery = bool(self._battery_reader())
            except Exception:
                self._on_battery = False
        with self._lock:
            self._expire_temporary()
            if self._screen_cache is not None and now - self._screen_cache[1] > _CONTEXT_TTL:
                self._screen_cache = None
            revision_reader = getattr(self.bridge, "input_revision", None)
            try:
                revision = revision_reader() if revision_reader is not None else None
            except Exception:
                revision = None
            if type(revision) is int:
                if revision != self._input_revision:
                    self._capture_after = 0.0
                self._input_revision = revision
            foreground = self._foreground()
            self._docs_context(foreground)
            if not self._active() or foreground is None:
                snapshot = None
                self._clear_docs_help()
                self._observe(None)
            elif now >= self._capture_after:
                snapshot = self._capture()
                self._observe(snapshot)
                self._refresh_docs_help(snapshot, foreground)
                recent_input = (snapshot is not None and (
                    now - self._candidate_since < 2 or self._preparing is not None or self._request is not None))
                self._capture_after = now + (min(self._capture_interval, 0.05) if recent_input else self._capture_interval)
            else:
                snapshot = self._candidate
            self._prepare_focused_model(snapshot)
            self._consume_prepared()
            self._consume_results()
            if snapshot is not None and snapshot.has_anchor:
                self._schedule(snapshot, now)
            notifications = list(self._notifications)
            self._notifications.clear()
        self._flush_records()
        for event in notifications:
            callback = self.on_suggestion if event["type"] == "suggestion" else self.on_clear
            if callback is not None:
                callback(event) if event["type"] == "suggestion" else callback()
        for callback in settings_callbacks:
            if callback is not None:
                callback()
        drain = getattr(self.bridge, "drain_input_transaction", None)
        if drain is not None:
            drain()

    def _schedule(self, snapshot, now):
        """Automatic work for the current field: chooser, correction or completion."""
        if snapshot.on_demand_only or self._field_paused(snapshot):
            return
        selected = self._selected_word(snapshot)
        mode = "synonyms" if selected else "completion"
        delay = self.config.synonyms_delay_ms if selected else self.config.debounce_ms
        eligible = ((bool(selected) and self._can_replace(snapshot) and self.config.synonyms_enabled
                     and self.config.synonyms_mode == "selection")
                    or (not selected and self._completion_eligible(snapshot)))
        emoji_query = False
        if self._can_replace(snapshot) and not selected and snapshot.eligible(1):
            from .features import emoji_choices
            result = emoji_choices(snapshot.prefix, self.config)
            if result is not None:
                emoji_query = True
                suffix, choices = result
                if suffix and snapshot.prefix.endswith(suffix) and self._attempted != (snapshot.key, "emoji"):
                    self._clear()
                    self._attempted = (snapshot.key, "emoji")
                    self._publish_choices(snapshot, choices, "emoji", (snapshot.caret - len(suffix), len(suffix)))
                eligible = False
        if not emoji_query and not selected and self._typo_active(snapshot) and snapshot.eligible(1):
            assessment = self._assess(snapshot)
            if assessment is None:
                eligible = False  # Wait for the local spelling result for this exact text.
            elif assessment.suspected:
                if self._show_correction(snapshot, assessment):
                    eligible = False
                if self.config.typo_suppression:
                    eligible = False
                    if self._suggestion.text and self._suggestion.snapshot is not None:
                        self._clear()
                        self._attempted = (snapshot.key, "completion")
        if (not emoji_query and not selected and self.config.alternatives_enabled
                and self.config.alternatives_mode == "pause" and self._request is None
                and self._preparing is None and self._suggestion.text and self._correction is None
                and self._suggestion.snapshot is not None and self._suggestion.snapshot.key == snapshot.key
                and (not self._fast_keys or "completion" in self._presented)
                and self._attempted != (snapshot.key, "alternatives")
                and now - self._candidate_since >= self.config.alternatives_delay_ms / 1000):
            self._submit(snapshot, "alternatives", self._suggestion.text)
            return
        if self._on_battery and self.config.battery_on_demand:
            eligible = False
        if (eligible and now - self._candidate_since >= delay / 1000
                and self._attempted != (snapshot.key, mode)
                and (mode != "completion" or (not self._suggestion.text and self._correction is None))):
            self._submit(snapshot, mode)

    def _accepted(self, current, text, resulting_text, *, start, length=0):
        if self.config.statistics_enabled:
            self._records.append(("stats", current.app_id, text, not self._completion_counted))
        self._completion_counted = True
        if self._record_allowed(current.app_id, getattr(current, "domain", "")):
            self._history_suppressed = None
            self._observe_session(current)
            if self._session_key is None:
                return
            if len(resulting_text) > 32000:
                self._finish_session()
                return
            # Exact edit geometry avoids attributing repeated generated words
            # to a matching authored prefix/suffix.
            self._session_authored[start:start + length] = bytes(len(text))
            self._session_text = resulting_text
            self._session_accepted = True
            self._queue_session()

    def _drawn(self, kind, key, claimed):
        """What the overlay confirmed it drew for key: None = no acknowledgment protocol."""
        if claimed is not None:
            claimed_kind, entry = claimed
            return entry if claimed_kind == kind and entry[0] == key else ()
        if not self._fast_keys:
            return None
        entry = self._presented.get(kind)
        return entry if entry is not None and entry[0] == key else ()

    def _accept(self, full=False, choice=None, claimed=None):
        current = self._capture()
        if current is None or not current.has_anchor:
            self._clear(hide_accessory=True)
            return False
        if choice is not None:
            return self._accept_choice(current, choice, claimed)
        if self._correction is not None:
            return self._apply_correction(current, claimed)
        if self._choices and not self._anchored_choices():
            # Tab/AboveTab never choose an alternative; only its number does.
            return False if self._mode == "alternatives" else self._accept_choice(current, 0, claimed)
        return self._accept_completion(current, full, claimed)

    def _accept_choice(self, current, index, claimed):
        if (not self._choices or self._choice_snapshot is None or current.key != self._choice_snapshot.key
                or not 0 <= index < len(self._choices)):
            self._clear()
            return False
        drawn = self._drawn("choices", current.key, claimed)
        if drawn is not None and (not drawn or drawn[4] != self._mode or index >= drawn[3]):
            return False
        text = self._choices[index]
        start, length = self._replacement
        if ((self._mode in {"emoji", "synonyms"} or (start, length) != (current.caret, 0))
                and not self._can_replace(current)):
            self._clear()
            return False
        if self._mode == "synonyms":
            success = self.bridge.replace_selection(current, text)
        elif start == current.caret and length == 0:
            success = self.bridge.insert(current, text)
        else:
            success = self.bridge.replace_range(current, start, length, text)
        if not success:
            self._clear()
            return False
        mode = self._mode
        resulting = current.text[:start] + text + current.text[start + length:]
        self._accepted(current, text, resulting, start=start, length=length)
        self._clear()
        self._attempted = (current.key, mode)
        return True

    def _apply_correction(self, current, claimed):
        snapshot, result = self._correction
        if (not self._can_replace(current)
                or current.key != snapshot.key or result.key != current.key):
            self._clear()
            return False
        drawn = self._drawn("correction", current.key, claimed)
        if drawn is not None and (not drawn or drawn[1] != result.replacement):
            return False
        if not self.bridge.replace_range(current, result.start, result.length, result.replacement):
            self._clear()
            return False
        resulting = current.text[:result.start] + result.replacement + current.text[result.start + result.length:]
        self._accepted(current, result.replacement, resulting, start=result.start, length=result.length)
        self._clear()
        self._attempted = None
        self._capture_after = 0.0
        return True

    def _accept_completion(self, current, full, claimed):
        if (self._suggestion.snapshot is None or current.key != self._suggestion.snapshot.key
                or not current.eligible(1) or not current.inline_safe):
            self._clear()
            return False
        drawn = self._drawn("completion", current.key, claimed)
        if drawn is not None and not drawn:
            return False  # Nothing confirmed drawn: never accept hidden text.
        text = self._suggestion.accept(
            current, full=full, trailing_space=self.config.trailing_space,
            trailing_punctuation=include_punctuation(self.config.punctuation_mode, full),
            visible=drawn[1] if drawn else None)
        updated = self._suggestion.snapshot
        # Compute the pure state transition under the decision lock, but commit
        # no statistics/history until the native adapter confirms insertion.
        if not text or updated is None or not self.bridge.insert(current, text):
            self._clear()
            return False
        self._client.cancel()
        self._request = None
        self._preparing = None
        self._accepted(current, text, updated.text, start=current.caret)
        self._candidate = updated
        self._candidate_revision = self._capture_revision
        self._capture_after = 0.0
        self._attempted = (updated.key, "completion")
        self._disarm()
        if not self._suggestion.text:
            self._suggestion.invalidate()
            self._clear_choices()
            self._emit({"type": "clear"})
        else:
            self._advance = (updated.key, current, text) if self._fast_keys else None
            self._publish_completion()
        return True

    def _dismiss(self, key, action, kind):
        self._clear()
        self._attempted = (key, "correction" if kind == "correction" else "completion")
        if action == "bypass":
            return
        if self.config.escape_behavior == "pause":
            # Brief, field-scoped pause; the warm worker is only cancelled.
            self._field_pause = ((key[0], key[1], key[6]), time.monotonic() + self.config.escape_pause_seconds)
        elif self.config.escape_behavior == "app":
            self._escape_app = key[0]
            self.bridge.configure(self._native_config())

    def _drop_inflight(self):
        """Escape over nothing drawn passes through but cancels work for this field."""
        if self._request is not None or self._preparing is not None:
            self._clear()
            if self._candidate is not None:
                self._attempted = (self._candidate.key, "completion")

    def _visible_kind(self):
        if self._fast_keys:
            for kind, expected in (("correction", self._correction[0] if self._correction else None),
                                   ("completion", self._suggestion.snapshot if self._suggestion.text else None),
                                   ("choices", self._choice_snapshot if self._choices else None)):
                entry = self._presented.get(kind)
                if expected is not None and entry is not None and entry[0] == expected.key:
                    return kind, expected
            return None
        if self._correction is not None:
            return "correction", self._correction[0]
        snapshot = self._suggestion.snapshot
        if self._suggestion.text and snapshot is not None and snapshot.inline_safe:
            return "completion", snapshot
        if self._choices and self._choice_snapshot is not None:
            return "choices", self._choice_snapshot
        return None

    def presentation_result(self, key, visible_text, mode="completion"):
        """UI-thread acknowledgment of exactly what the overlay drew for key."""
        if not isinstance(visible_text, str) or not isinstance(mode, str):
            return
        kind = "choices" if mode in _CHOOSER_MODES else mode
        if kind not in {"completion", "choices", "correction"}:
            return
        with self._lock:
            if self._stopped:
                return
            entry = None
            if visible_text:
                if kind == "completion":
                    snapshot = self._suggestion.snapshot
                    valid = (snapshot is not None and snapshot.key == key
                             and self._suggestion.text.startswith(visible_text))
                elif kind == "correction":
                    valid = (self._correction is not None and self._correction[0].key == key
                             and visible_text == self._correction[1].replacement)
                else:
                    valid = (bool(self._choices) and self._choice_snapshot is not None
                             and self._choice_snapshot.key == key and self._mode == mode)
                if valid:
                    count = 0
                    if kind == "choices":
                        for shown, choice in zip(visible_text.split("\n"), self._choices):
                            if shown != choice:
                                break
                            count += 1
                    revision = (self._candidate_revision if self._candidate is not None
                                and self._candidate.key == key else None)
                    if kind != "choices" or count:
                        entry = (key, visible_text, revision, count, mode)
            presented = dict(self._presented)
            if entry is None:
                presented.pop(kind, None)
            else:
                presented[kind] = entry
            self._presented = presented
            with self._arm_lock:
                armed = dict(self._armed)
                if entry is None or entry[2] is None or not self._fast_keys:
                    armed.pop(kind, None)
                else:
                    armed[kind] = entry
                self._armed = armed
            acknowledge = getattr(self.bridge, "acknowledge_input_presentation", None)
            if acknowledge is not None and kind == "completion":
                acknowledge(key)

    def _claim(self, kinds, index=None, chooser_modes=_CHOOSER_MODES):
        revision = self._user_revision()
        if revision is None:
            return None
        with self._arm_lock:
            for kind in kinds:
                entry = self._armed.get(kind)
                if (entry is None or entry[2] != revision
                        or (kind == "choices" and (entry[4] not in chooser_modes
                                                   or (index is not None and index >= entry[3])))):
                    continue
                # One key consumes the armed presentation; nothing else can reuse it.
                self._armed = {}
                return kind, entry
        return None

    def _defer(self, action, claimed):
        transaction = getattr(self.bridge, "input_transaction", None)
        gate = transaction() if transaction else None
        if action in {"escape", "dismiss", "bypass"} and (
                claimed is None or (action != "bypass" and self.config.escape_behavior == "passthrough")):
            gate = None
        self._deferred.append((action, claimed, gate))
        self._notify_ui()

    def fast_invoke(self, action: str) -> bool:
        """Keyboard-tap decision: O(1), no Accessibility, never the controller lock.

        A key is consumed only when the overlay confirmed it drew the text the
        key acts on and no user input arrived since; tick() then performs the
        authoritative insertion and replays the key once if it became stale.
        """
        if self._stopped or not isinstance(action, str):
            return False
        if action == "bypass":
            claimed = self._claim(("correction", "completion", "choices"))
            if claimed is not None:
                self._defer(action, claimed)
            return True  # Option+Tab always reaches the host as a plain Tab.
        if action in {"word", "all"}:
            claimed = self._claim(("correction", "completion", "choices"), chooser_modes={"emoji", "synonyms"})
            if claimed is None:
                return False
            self._defer(action, claimed)
            return True
        if action in {"escape", "dismiss"}:
            claimed = self._claim(("correction", "completion", "choices"))
            if claimed is None:
                if self._request is not None or self._preparing is not None:
                    self._defer(action, None)
                return False
            self._defer(action, claimed)
            return self.config.escape_behavior != "passthrough"
        if action.startswith("choice:"):
            try:
                index = int(action.split(":", 1)[1]) - 1
            except ValueError:
                return False
            if not 0 <= index <= 8:
                return False
            claimed = self._claim(("choices",), index=index)
            if claimed is None:
                return False
            self._defer(action, claimed)
            return True
        if action in _COMMAND_ACTIONS:
            config = self.config
            if action in {"force", "alternatives", "synonyms"} and not self._active():
                return False
            if action == "alternatives" and not config.alternatives_enabled:
                return False
            if action == "synonyms" and (not config.synonyms_enabled or not self._rewrites_supported):
                return False
            origin = self._candidate
            if action == "synonyms" and (origin is None or not origin.can_replace):
                return False
            revision = self._user_revision()
            if action in {"force", "alternatives", "synonyms", "app_toggle"}:
                if origin is None or revision is None or revision != self._candidate_revision:
                    return False
                self._defer(action, ("origin", origin, revision))
            else:
                self._defer(action, None)
            return True
        return False

    def _replay(self, action):
        replay = getattr(self.bridge, "replay_action", None)
        if replay is None:
            return False
        try:
            return bool(replay(action))
        except Exception:
            return False

    def _run_deferred(self):
        """Authoritative handling of keys the tap consumed (controller lock held)."""
        while self._deferred:
            item = self._deferred.popleft()
            action, claimed = item[:2]
            gate = item[2] if len(item) > 2 else None
            begin = getattr(self.bridge, "begin_input_transaction", None)
            finish = getattr(self.bridge, "finish_input_transaction", None)
            if begin is not None and not begin(gate):
                continue
            success = True
            if action in {"word", "all"}:
                success = self._accept(full=action == "all", claimed=claimed)
                if not success and gate is None:
                    self._replay(action)
            elif action.startswith("choice:"):
                success = self._accept(choice=int(action.split(":", 1)[1]) - 1, claimed=claimed)
                if not success and gate is None:
                    self._replay(action)
            elif action in {"escape", "dismiss", "bypass"}:
                if claimed is None:
                    if action != "bypass":
                        self._drop_inflight()
                else:
                    self._dismiss(claimed[1][0], action, claimed[0])
            elif claimed is not None and claimed[0] == "origin":
                origin, revision = claimed[1:]
                current = self._capture()
                if (self._user_revision() != revision or current is None or current.key != origin.key):
                    success = False
                    if gate is None:
                        self._replay(action)
                elif action == "app_toggle":
                    self._commands.append((action, origin.app_id, origin.domain))
                else:
                    self._commands.append((action, origin.key, revision))
            elif not self.invoke(action):
                success = False
                if gate is None:
                    self._replay(action)
            if finish is not None:
                remainder = (self._suggestion.snapshot.key
                             if success and action in {"word", "all"} and self._suggestion.text
                             and self._suggestion.snapshot is not None else None)
                finish(gate, success, presentation_key=remainder)

    def invoke(self, action: str) -> bool:
        with self._lock:
            if self._stopped:
                return False
            if action == "toggle":
                self._commands.append(action)
                return True
            if action == "app_toggle":
                try:
                    info = self.bridge.foreground() or {}
                except Exception:
                    return False
                if not info.get("app_id"):
                    return False
                app = info["app_id"]
                if any(exclusion.casefold() in value.casefold() for exclusion in self._policy_exclusions()
                       for value in (app, str(info.get("name", "")))):
                    return False
                self._commands.append((action, app, str(info.get("domain", ""))))
                return True
            if action == "settings":
                self._commands.append(action)
                self._emit({"type": "settings", "pane": "general"})
                return True
            if action == "plain_tab":
                if self._visible_kind() is not None:
                    self._clear()
                return self._replay("plain_tab")
            if action in {"escape", "dismiss", "bypass"}:
                visible = self._visible_kind()
                if visible is None:
                    if action != "bypass":
                        self._drop_inflight()
                    return False
                kind, expected = visible
                current = self._capture()
                if current is None or not current.has_anchor or current.key != expected.key:
                    self._clear(hide_accessory=True)
                    return False
                self._dismiss(current.key, action, kind)
                return action == "bypass" or self.config.escape_behavior != "passthrough"
            if not self._active():
                return False
            if action in {"force", "alternatives", "synonyms"}:
                if action == "alternatives" and not self.config.alternatives_enabled:
                    return False
                if action == "synonyms" and (not self.config.synonyms_enabled
                        or not self._rewrites_supported or not self._synonyms_available()):
                    return False
                snapshot = self._capture()
                if snapshot is None or not snapshot.has_anchor or (action == "synonyms" and not self._selected_word(snapshot)):
                    return False
                if action != "synonyms" and not snapshot.eligible(1):
                    return False
                if action == "force" and (not snapshot.inline_safe or (
                        self.config.completion_length == "word" and not mid_word(snapshot.prefix))):
                    return False
                self._commands.append((action, snapshot.key))
                return True
            if action in {"word", "all"}:
                return self._accept(full=action == "all")
            if action.startswith("choice:"):
                try:
                    index = int(action.split(":", 1)[1]) - 1
                except ValueError:
                    return False
                return self._accept(choice=index) if 0 <= index <= 8 and self._choices else False
            return False

    def force(self):
        return self.invoke("force")

    @staticmethod
    def _privacy_revoked(old, config):
        if any(getattr(old, name) and not getattr(config, name) for name in (
                "enabled", "clipboard_context", "screen_context", "screenshot_context",
                "record_history", "terminal_completion")):
            return True
        if old.personalization_strength and not config.personalization_strength and Controller._recording_configured(old):
            return True
        if set(config.excluded_apps) - set(old.excluded_apps):
            return True
        for scope in old.app_rules.keys() | config.app_rules.keys():
            previous = old.app_rules.get(scope, {})
            current = config.app_rules.get(scope, {})
            for name in ("enabled", "record_history"):
                if previous.get(name, getattr(old, name)) and not current.get(name, getattr(config, name)):
                    return True
        return False

    def update_config(self, config: Config, persist=True):
        config.validate()
        if self._recording_configured(config):
            self._storage.enable_history()
        if persist:
            save_config(config)
        spelling = None
        with self._lock:
            old = self.config
            self._finish_session()
            self.config = config
            self._clear_docs_help()
            if sys.platform == "darwin" and old.screenshot_context and not config.screenshot_context:
                from .native.macos_vision import clear_text_cache
                clear_text_cache()
            self._screen_cache = None
            self._capture_after = 0.0
            self._error = ""
            self._clear()
            self._attempted = None
            self._failed_correction_key = None
            self._candidate = None
            if not (config.typo_suppression or config.typo_fixes):
                spelling, self._spelling = self._spelling, None
            self.bridge.configure(self._native_config())
            self.bridge.set_shortcuts_enabled(True)
            self._client.set_idle_timeout(config.idle_release_seconds)
            if (not self._active() or provider_identity(old) != provider_identity(config)
                    or self._privacy_revoked(old, config)):
                self._client.release()
            if not self._recording_configured(config):
                self._session_text = ""
                self._session_authored.clear()
                self._session_key = None
        if spelling is not None:
            spelling.close()
        if not self._recording_configured(config):
            self._storage.lock_history()

    def toggle_enabled(self):
        with self._lock:
            if self.config.global_enabled and time.monotonic() < self._paused_until:
                self._paused_until = 0.0
                self._attempted = None
                return True
            self._paused_until = 0.0
            updated = replace(self.config, global_enabled=not self.config.global_enabled)
        self.update_config(updated)
        return self.config.global_enabled

    def pause(self, minutes: int):
        if type(minutes) is not int or minutes < 0:
            raise ValueError("Pause duration must be a non-negative number of minutes.")
        with self._lock:
            self._finish_session()
            self._paused_until = time.monotonic() + minutes * 60
            self._screen_cache = None
            self._clear(hide_accessory=True)
            self._candidate = None
            self._client.release()

    def toggle_current_app(self, *, app=None, domain=""):
        if app is not None:
            return self._toggle_app(app, domain)
        with self._lock:
            try:
                info = self.bridge.foreground() or {}
            except Exception:
                return False
            app = info.get("app_id", "") or self._app
            domain = info.get("domain", "") if info.get("app_id") else self._domain
        return self._toggle_app(app, domain)

    def _toggle_app(self, app, domain):
        with self._lock:
            if not app or any(exclusion.casefold() in app.casefold() for exclusion in self._policy_exclusions()):
                return False
            domain = self._domain_key(domain)
            # A website is toggled on its own in a browser, never the whole browser.
            scope = "domain:" + domain if domain else "app:" + app
            desired = not self._allowed(app, domain)
            if self._escape_app == app:
                self._escape_app = ""
            self._temporary_apps.pop(scope, None)
            if self._allowed(app, domain) != desired:
                self._temporary_apps[scope] = (desired, time.monotonic() + _TEMPORARY_TOGGLE_SECONDS)
            self._finish_session()
            self._screen_cache = None
            self._clear(hide_accessory=True)
            self._candidate = None
            self._attempted = None
            self.bridge.configure(self._native_config())
            return True

    def _temporary_status(self, app, domain):
        now = time.monotonic()
        for scope in (("domain:" + self._domain_key(domain)) if domain else "", "app:" + app if app else ""):
            entry = self._temporary_apps.get(scope) if scope else None
            if entry is not None and now < entry[1]:
                return scope, entry[1] - now
        return "", None

    def status(self):
        with self._lock:
            status = self._client.status()
            active = self._active()
            if not active:
                status["state"] = "paused"
                status["message"] = "Typing suggestions are paused." if self.config.global_enabled else "Typing suggestions are disabled."
            elif self._error:
                status["state"], status["message"] = "error", self._error
            native_status = self.bridge.status()
            if not self._rewrites_supported:
                native_status += " Atomic text replacement is unavailable: emoji/emoticon conversion and selected-word synonyms are disabled; continuation alternatives remain available."
            tab_enabled = True
            for rule in self._rules(self._app, self._domain):
                tab_enabled = rule.get("tab_enabled", tab_enabled)
            secure_reader = getattr(self.bridge, "secure_input_active", None)
            try:
                secure_input = bool(secure_reader()) if secure_reader is not None else False
            except Exception:
                secure_input = False
            scope, remaining = self._temporary_status(self._app, self._domain)
            return {**status, "enabled": active,
                    "inline_available": bool(self._candidate and self._candidate.inline_safe),
                    "inline_message": ("Verified same-line completion is available." if self._candidate and self._candidate.inline_safe
                                       else "This field does not expose verified same-line rendering; ordinary completion is silent."),
                    "context_status": self._context_status,
                    "context_message": _CONTEXT_MESSAGES[self._context_status],
                    "current_app_enabled": self._allowed(self._app, self._domain),
                    "current_recording": bool(self._app) and bool(self._record_allowed(self._app, self._domain)),
                    "current_tab_enabled": tab_enabled,
                    "secure_input": secure_input,
                    "temporary_scope": scope, "temporary_remaining": remaining,
                    "escape_suppressed": bool(self._escape_app) and self._escape_app == self._app,
                    "native_status": native_status, "current_app": self._app,
                    "current_domain": self._domain, "paused_until": self._paused_until,
                    "on_battery": self._on_battery, "download": dict(self._download) if self._download else None}

    def set_model(self, path, prompt_style="continuation", model_id=None):
        if model_id is None:
            from .models import PRESETS, model_path
            selected = Path(path).expanduser()
            model_id = next((preset.id for preset in PRESETS if model_path(preset) == selected), "")
        self.update_config(replace(self.config, backend="gguf", model_path=str(path),
                                   prompt_style=prompt_style, model_id=model_id))

    def models(self):
        from .models import PRESETS, is_installed, model_availability, model_path
        return [{"id": preset.id, "label": preset.label, "description": preset.description,
                 "path": str(model_path(preset)), "size_bytes": preset.size_bytes,
                 "installed": is_installed(preset), "prompt_style": preset.prompt_style,
                 "license_name": preset.license_name, "license_url": preset.license_url,
                 "requires_license_acceptance": preset.requires_license_acceptance,
                 "availability": model_availability(preset)} for preset in PRESETS]

    def download_model(self, preset_id, *, accept_license=False):
        from .models import DownloadCancellation, download_model, get_preset
        preset = get_preset(preset_id)
        with self._lock:
            if self._download_thread is not None and self._download_thread.is_alive():
                raise RuntimeError("Cancel the current model download before starting another.")
            cancel = self._download_cancel = DownloadCancellation()
            self._download = {"id": preset_id, "received": 0, "total": preset.size_bytes, "state": "downloading", "error": ""}

            def progress(received, total):
                with self._lock:
                    if cancel is self._download_cancel and not self._stopped:
                        self._download.update(received=received, total=total)
                        self._emit({"type": "download", **self._download})

            def run():
                try:
                    path = download_model(preset_id, progress, cancel, accept_license=accept_license)
                    with self._lock:
                        if cancel.is_set() or self._stopped:
                            return
                        self._download.update(state="complete", path=str(path), prompt_style=preset.prompt_style)
                        self._emit({"type": "download", **self._download})
                except Exception:
                    with self._lock:
                        if self._stopped:
                            return
                        self._download.update(state="cancelled" if cancel.is_set() else "error", error="Download cancelled." if cancel.is_set() else "Model download failed; existing weights were preserved.")
                        self._emit({"type": "download", **self._download})

            self._download_thread = threading.Thread(target=run, daemon=True, name="inklow-model-download")
            self._download_thread.start()

    def cancel_download(self):
        with self._lock:
            if self._download_cancel is not None:
                self._download_cancel.set()

    def stats(self, period="week", group_by="day"):
        self._flush_records()
        return self._storage.stats(period, group_by)

    def history_info(self):
        try:
            return self._storage.history_info()
        except (OSError, sqlite3.Error):
            return {"count": 0, "bytes": 0, "available": False, "error": "Local history storage could not be opened. Check its permissions and available disk space."}

    def history_counts(self, *, unlock=False):
        """A user-requested one-operation unlock never enables writing history."""
        self._flush_records()
        try:
            return self._storage.history_counts(unlock=unlock)
        except (RuntimeError, OSError, sqlite3.Error):
            return {"available": False, "apps": {}, "domains": {}}

    def delete_history(self, app_id=None, domain=None):
        if app_id is not None and domain is not None:
            raise ValueError("Delete history for one app or one website at a time.")
        domain = self._domain_key(domain) if domain is not None else None

        def matches(app, host):
            return (app_id is None and domain is None) or (app_id is not None and app == app_id) or (
                domain is not None and self._domain_key(host) == domain)

        with self._lock:
            self._records = deque((record for record in self._records
                                   if record[0] != "history" or not matches(record[2], record[3])), maxlen=128)
            candidate = self._candidate
            if ((self._session_key and matches(self._session_key[0], self._session_domain))
                    or (candidate and matches(candidate.app_id, getattr(candidate, "domain", "")))):
                self._session_key = None
                self._session_text = ""
                self._session_authored.clear()
                self._session_accepted = False
                self._history_suppressed = (
                    ((candidate.app_id, candidate.field_id, getattr(candidate, "domain", "")), candidate.text)
                    if candidate else None)
            self._history_mutating = True
            # Cancel prepared/in-flight prompts and their model KV cache, not
            # merely the on-disk records and derived feature index.
            self._candidate = None
        try:
            try:
                with self._lock:
                    self._clear(hide_accessory=True)
                self._client.release()
            finally:
                # Model cleanup failure must not prevent the requested unlearning.
                self._storage.delete_history(app_id, domain=domain, unlock=app_id is not None or domain is not None)
        finally:
            with self._lock:
                self._history_mutating = False

    def set_history_enabled(self, enabled):
        if type(enabled) is not bool:
            raise ValueError("History preference must be a boolean.")
        self.update_config(replace(self.config, record_history=enabled))

    def set_clipboard_reader(self, reader):
        """Install reader(*, authorized) -> str; GUI readers own their marshal."""
        self._clipboard_reader = reader

    def set_screen_reader(self, reader):
        """Install reader(snapshot, *, bridge, authorized, max_chars, accessibility, screenshot_context)."""
        self._screen_reader = reader

    def set_battery_reader(self, reader):
        self._battery_reader = reader

    def stop(self):
        """Stop activity immediately; attempt every cleanup and retry failures."""
        failures = []

        def finish(name, action):
            if name in self._shutdown_done:
                return
            try:
                action()
            except Exception:
                # Native/provider errors may quote private writing or paths.
                failures.append(name)
            else:
                self._shutdown_done.add(name)

        with self._lock:
            self._stopped = True
            self._wake_ui = None
            finish("callbacks", lambda: self.set_wake_callback(None))
            finish("session", self._finish_session)
            self._deferred.clear()
            self._screen_cache = None
            self._context_wake.set()
            self._screen_wake.set()
            self._screen_ready.set()
            finish("download", self.cancel_download)
            finish("presentation", lambda: self._clear(hide_accessory=True))
            if self._spelling is not None:
                finish("spelling", self._spelling.close)
                if "spelling" in self._shutdown_done:
                    self._spelling = None
            if sys.platform == "darwin":
                def clear_vision():
                    from .native.macos_vision import clear_text_cache
                    clear_text_cache()
                finish("vision", clear_vision)
        finish("bridge", self.bridge.stop)
        finish("client", self._client.close)
        finish("records", lambda: self._flush_records(raise_errors=True))
        finish("history", self._storage.lock_history)
        pending = []
        for name, thread in (("download_thread", self._download_thread),
                             ("context_thread", self._context_thread),
                             ("screen_thread", self._screen_thread)):
            if name in self._shutdown_done:
                continue
            if thread is not None and thread is not threading.current_thread():
                try:
                    thread.join(timeout=0.2)
                except RuntimeError:
                    failures.append(name)
                else:
                    if thread.is_alive():
                        pending.append(name)
                    else:
                        self._shutdown_done.add(name)
        if failures:
            with self._lock:
                self._error = "Shutdown is incomplete. Try Quit again to finish releasing local resources."
            raise RuntimeError(self._error)
        if pending:
            raise ShutdownPending("Finishing cancellation of local background work.")
