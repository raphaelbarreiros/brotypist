"""Menu-agent controller. Native operations belong to tick/the shortcut decision.

No widgets, sockets, model imports, or persistent drafts live in this process.
"""
from __future__ import annotations

from collections import deque
from dataclasses import asdict, replace
import re
import sqlite3
import sys
import threading
import time
import uuid
import unicodedata

from .config import Config, save_config
from .core import SuggestionState
from .process_client import ProcessClient
from .storage import LocalStorage

_CONTEXT_TTL = 2.0
_CONTEXT_MESSAGES = {
    "off": "Optional context is off.",
    "pending": "Reading eligible text from the focused window.",
    "ready": "Optional reference text is available.",
    "empty": "No relevant optional reference text is available.",
    "unavailable": "This field does not expose safe focused-window Accessibility context.",
    "blocked": "Optional context was blocked by current focus or privacy policy.",
    "stale": "Focused-window context changed and was discarded.",
    "error": "Optional context could not be read. Check Accessibility permissions or turn it off.",
}


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
        self._client = ProcessClient()
        self._storage = LocalStorage()
        self._lock = threading.RLock()
        self._events = deque(maxlen=128)
        self._notifications = deque(maxlen=128)
        self._records = deque(maxlen=128)
        self._suggestion = SuggestionState()
        self._candidate = None
        self._candidate_since = 0.0
        self._attempted = None
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
        self._screen_thread = None
        self._screen_cache = None
        self._context_status = "off"
        self._choices = []
        self._choice_snapshot = None
        self._replacement = None
        self._mode = "completion"
        self._completion_counted = False
        self._started = False
        self._stopped = False
        self._browser_apps = set()
        self._temporary_apps = {}
        self._paused_until = 0.0
        self._app = ""
        self._domain = ""
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
        self._session_domain = ""
        self._session_accepted = False
        self._history_suppressed = None
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
            self.bridge.start_shortcuts(self.invoke)
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

    def _rules(self, app, domain):
        app_rule = self.config.app_rules.get("app:" + app, {})
        domain_rule = self.config.app_rules.get("domain:" + domain.casefold().rstrip("."), {}) if domain else {}
        return app_rule, domain_rule

    def _native_config(self):
        if not self._temporary_apps:
            return self.config
        rules = {key: dict(value) for key, value in self.config.app_rules.items()}
        for app, enabled in self._temporary_apps.items():
            rules.setdefault("app:" + app, {})["enabled"] = enabled
        return replace(self.config, app_rules=rules)

    def _allowed(self, app, domain, name=""):
        if not app:
            return False
        identifiers = (app.casefold(), name.casefold())
        if any(exclusion.casefold() in identifier for exclusion in self.config.excluded_apps for identifier in identifiers):
            return False
        if self._temporary_apps.get(app) is False:
            return False
        app_rule, domain_rule = self._rules(app, domain)
        enabled = self._temporary_apps.get(app, app_rule.get("enabled", self.config.enabled))
        return domain_rule.get("enabled", enabled)

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
            return None
        app = str(info.get("app_id", ""))
        domain = str(info.get("domain", "")).casefold().rstrip(".")
        if app:
            self._app, self._domain = app, domain
            if info.get("browser"):
                self._browser_apps.add(app)
        if (info.get("browser") and (not domain or not info.get("domain_observable", bool(domain))) and any(
                key.startswith("domain:") and rule.get("enabled") is False
                for key, rule in self.config.app_rules.items())):
            return None
        return info if self._allowed(app, domain, str(info.get("name", ""))) else None

    @staticmethod
    def _selected_word(snapshot):
        if (snapshot is None or snapshot.protected or not 0 < snapshot.selection_length <= 80
                or snapshot.caret < 0 or snapshot.caret + snapshot.selection_length > len(snapshot.text)):
            return ""
        text = snapshot.text[snapshot.caret:snapshot.caret + snapshot.selection_length]
        normalized = unicodedata.normalize("NFC", text)
        return text if re.fullmatch(
            r"[^\W\d_]+(?:[’'-][^\W\d_]+)*(?: [^\W\d_]+(?:[’'-][^\W\d_]+)*){0,4}", normalized, re.UNICODE) else ""

    def _capture(self):
        if self._foreground() is None or not self._active():
            return None
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
        return snapshot

    def _clear(self, *, hide_accessory=False, cancel=True, cancel_context=True):
        if cancel:
            self._client.cancel()
        self._request = None
        self._preparing = None
        self._context_pending = None
        self._prepared.clear()
        if cancel_context:
            self._screen_pending = None
            self._screen_job = None
            self._context_status = "blocked" if self.config.screen_context or self.config.clipboard_context else "off"
        self._suggestion.invalidate()
        self._choices = []
        self._choice_snapshot = None
        self._replacement = None
        self._emit({"type": "clear", "hide_accessory": hide_accessory})

    def _finish_session(self):
        if self._session_key and (self._session_accepted or self.config.record_unaccepted):
            app = self._session_key[0]
            if self._record_allowed(app, self._session_domain):
                self._records.append(("history", self._session_id, app, self._session_domain, self._session_text))
        self._session_key = None
        self._session_text = ""
        self._session_accepted = False

    def _observe_session(self, snapshot):
        key = (snapshot.app_id, snapshot.field_id, getattr(snapshot, "domain", ""))
        if self._history_suppressed == (key, snapshot.text):
            return
        self._history_suppressed = None
        if not self._record_allowed(snapshot.app_id, getattr(snapshot, "domain", "")):
            self._finish_session()
            return
        if key != self._session_key:
            self._finish_session()
            self._session_key = key
            self._session_id = uuid.uuid4().hex
            self._session_domain = getattr(snapshot, "domain", "")
        self._session_text = snapshot.text

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
            if (previous.caret_rect != snapshot.caret_rect or previous.field_rect != snapshot.field_rect
                    or previous.native_anchor != snapshot.native_anchor
                    or previous.inline_render != snapshot.inline_render
                    or previous.native_inline != snapshot.native_inline):
                if ((snapshot.has_anchor and not previous.has_anchor)
                        or (snapshot.inline_safe and not previous.inline_safe)):
                    self._attempted = None
                if self._suggestion.snapshot is not None:
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
        if self._suggestion.advance(snapshot):
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

    def _context_current(self, token, snapshot, kind):
        expected = self._screen_job if kind == "screen" else self._preparing
        return (token is not None and expected == token and self._active()
                and bool(self.config.personalization_strength if kind == "personalization"
                         else getattr(self.config, ("screen" if kind.startswith("screen") else "clipboard") + "_context"))
                and not snapshot.protected
                and self._candidate is not None and self._candidate.key == snapshot.key
                and self._window_key(self._candidate) == self._window_key(snapshot)
                and self._allowed(snapshot.app_id, getattr(snapshot, "domain", "")))

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

    def _queue_window_context(self, snapshot):
        if not self.config.screen_context:
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
        if self._screen_cache is not None and self._screen_cache[0] == key:
            if time.monotonic() - self._screen_cache[1] <= _CONTEXT_TTL:
                self._context_status = self._screen_cache[4]
                return
        if (self._screen_pending is not None and self._screen_pending[1].key == snapshot.key
                and self._window_key(self._screen_pending[1]) == key):
            return
        if (self._screen_job is not None and self._screen_job[1] == snapshot.key
                and self._screen_job[2] == key):
            return
        if self._screen_reader is None:
            self._context_status = "unavailable"
            return
        token = (object(), snapshot.key, key)
        self._screen_job = token
        self._screen_pending = (token, snapshot)
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
        guard = lambda: self._context_allowed(token, snapshot, "screen")
        if not guard():
            blocked()
            return
        try:
            result = self._screen_reader(
                snapshot, bridge=self.bridge, authorized=guard, max_chars=2400)
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
            if not isinstance(text, str) or provenance not in {"", "accessibility"}:
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
            self._screen_cache = (self._window_key(snapshot), time.monotonic(),
                                  provenance, text[:2400], status)
            self._context_status = status
            self._screen_job = None

    def _reference_options(self, snapshot, references, *, use_window=True):
        from .writing import reference_context
        references = list(references)
        cached = self._screen_cache
        if (use_window and self.config.screen_context and cached is not None
                and cached[0] == self._window_key(snapshot)
                and time.monotonic() - cached[1] <= _CONTEXT_TTL and cached[4] == "ready"):
            references.append((cached[2], cached[3]))
        return reference_context(snapshot.prefix, snapshot.suffix, references)

    def _options(self, snapshot, mode, token):
        app_rule, domain_rule = self._rules(snapshot.app_id, getattr(snapshot, "domain", ""))
        instructions = "\n".join(value for value in (
            self.config.custom_instructions, app_rule.get("custom_instructions", ""), domain_rule.get("custom_instructions", "")) if value)
        warning = ""
        if self.config.personalization_strength:
            try:
                if self._storage.history_info()["count"] and self._context_allowed(token, snapshot, "personalization"):
                    vocabulary = self._storage.vocabulary(self.config.personalization_strength, snapshot.app_id)
                    if vocabulary and self._context_allowed(token, snapshot, "personalization"):
                        instructions += "\n" + vocabulary
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
        length = "short" if self._on_battery and self.config.battery_shorter else self.config.completion_length
        options = {"instructions": instructions[:26000], "context": reference, "_references": context,
                   "max_words": {"short": 4, "medium": 8, "long": 16}[length]}
        if warning:
            options["_warning"] = warning
        if mode != "completion":
            from .features import utility_prompt
            selected = self._selected_word(snapshot) if mode == "synonyms" else ""
            suffix = snapshot.text[snapshot.caret + snapshot.selection_length:] if selected else snapshot.suffix
            options["utility"] = utility_prompt(mode, snapshot.prefix, suffix, selected)
        return options

    def _effective_config(self, mode):
        length = "short" if self._on_battery and self.config.battery_shorter else self.config.completion_length
        limit = {"short": 16, "medium": 32, "long": 64}[length]
        config = replace(self.config, max_tokens=min(self.config.max_tokens, limit))
        if mode != "completion":
            config.max_tokens = 128
        if self._on_battery and self.config.battery_smaller_model and config.backend == "gguf":
            from .models import get_preset, is_installed, model_path
            tiny = get_preset("tiny")
            if is_installed(tiny):
                config = replace(config, model_path=str(model_path(tiny)), prompt_style=tiny.prompt_style)
            else:
                self._error = "Battery smaller-model mode needs the Tiny model installed in Setup; the selected model is still used."
        return config

    def _submit(self, snapshot, mode="completion"):
        if mode == "completion" and not snapshot.inline_safe:
            return False
        if mode in {"emoji", "synonyms"} and not self._rewrites_supported:
            return False
        self._clear(cancel_context=False)
        self._error = ""
        self._mode = mode
        self._completion_counted = False
        self._attempted = (snapshot.key, mode)
        token = (snapshot.key, self._suggestion.generation, mode)
        self._preparing = token

        self._queue_window_context(snapshot)
        self._context_pending = (token, snapshot, mode)
        if self._context_thread is None:
            self._context_thread = threading.Thread(target=self._context_loop, daemon=True, name="inklow-optional-context")
            self._context_thread.start()
        self._context_wake.set()
        return True

    def _prepare_options(self, token, snapshot, mode):
        try:
            with self._lock:
                if self._preparing != token or not self._active():
                    return
            options = self._options(snapshot, mode, token)
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
            try:
                identifier = self._client.submit(self._effective_config(mode), snapshot.prefix,
                                                 snapshot.suffix, **options)
            except (RuntimeError, ValueError) as error:
                self._error = str(error)
                continue
            self._request = (identifier, snapshot, token[1], mode)

    def _publish_completion(self):
        snapshot, text = self._suggestion.snapshot, self._suggestion.text
        if snapshot is None or not text or not snapshot.inline_safe:
            self._emit({"type": "clear"})
            return
        self._emit({"type": "suggestion", "text": text,
                    "rect": list(snapshot.anchor_rect) if snapshot.anchor_rect else None,
                    "native_anchor": snapshot.native_anchor, "choices": [],
                    "field_rect": list(snapshot.field_rect) if snapshot.field_rect else None,
                    "field_anchor": snapshot.caret_rect is None,
                    "mode": "completion",
                    "inline": True,
                    "render": asdict(snapshot.inline_render) if snapshot.inline_render else None})

    def _publish_choices(self, snapshot, choices, mode, replacement):
        if mode in {"emoji", "synonyms"} and not self._rewrites_supported:
            return
        self._suggestion.invalidate()
        self._choices = choices[:5]
        self._choice_snapshot = snapshot
        self._replacement = replacement
        self._mode = mode
        self._completion_counted = False
        if snapshot.has_anchor and self._choices:
            self._emit({"type": "suggestion", "text": self._choices[0],
                        "rect": list(snapshot.anchor_rect) if snapshot.anchor_rect else None,
                        "native_anchor": snapshot.native_anchor,
                        "field_rect": list(snapshot.field_rect) if snapshot.field_rect else None,
                        "field_anchor": snapshot.caret_rect is None,
                        "choices": list(self._choices), "mode": mode})

    def _consume_results(self):
        for event in self._client.poll():
            request = self._request
            if request is None or event.get("id") != request[0]:
                continue
            _, snapshot, generation, mode = request
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
                    if self._suggestion.offer(generation, snapshot, text):
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

    def _flush_records(self):
        while True:
            with self._lock:
                if not self._records:
                    return
                record = self._records.popleft()
                allowed = self.config.statistics_enabled if record[0] == "stats" else self._record_allowed(record[2], record[3])
            if not allowed:
                continue
            try:
                if record[0] == "stats":
                    _, app, text, completion = record
                    self._storage.record_acceptance(app, text, completion=completion)
                else:
                    _, identifier, app, domain, text = record
                    self._storage.record_history(identifier, app, text)
            except (RuntimeError, OSError, sqlite3.Error) as error:
                with self._lock:
                    self._error = str(error)

    def tick(self):
        settings_callbacks = []
        if not self._started:
            self.start()
        with self._lock:
            if self._stopped:
                return
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
                        if not snapshot.has_anchor:
                            continue
                        mode = "completion" if action == "force" else action
                        if ((mode == "completion" and snapshot.eligible(1))
                                or (mode == "alternatives" and snapshot.eligible(1))
                                or (mode == "synonyms" and self._selected_word(snapshot))):
                            self._observe(snapshot)
                            self._submit(snapshot, mode)
        now = time.monotonic()
        if self._battery_reader is not None and now - self._battery_checked >= 5:
            self._battery_checked = now
            try:
                self._on_battery = bool(self._battery_reader())
            except Exception:
                self._on_battery = False
        with self._lock:
            revision_reader = getattr(self.bridge, "input_revision", None)
            try:
                revision = revision_reader() if revision_reader is not None else None
            except Exception:
                revision = None
            if type(revision) is int:
                if revision != self._input_revision:
                    self._capture_after = 0.0
                self._input_revision = revision
            if not self._active() or self._foreground() is None:
                snapshot = None
                self._observe(None)
            elif now >= self._capture_after:
                snapshot = self._capture()
                self._observe(snapshot)
                recent_input = (snapshot is not None and (
                    now - self._candidate_since < 2 or self._preparing is not None or self._request is not None))
                self._capture_after = now + (min(self._capture_interval, 0.05) if recent_input else self._capture_interval)
            else:
                snapshot = self._candidate
            self._consume_prepared()
            self._consume_results()
            if snapshot is not None and snapshot.has_anchor:
                selected = self._selected_word(snapshot)
                mode = "synonyms" if selected else "completion"
                delay = self.config.synonyms_delay_ms if selected else self.config.debounce_ms
                eligible = (bool(selected) and self._rewrites_supported and self.config.synonyms_enabled and self.config.synonyms_mode == "selection") or (
                    snapshot.inline_safe and snapshot.eligible(self.config.min_chars) and (self.config.complete_inside_text or not snapshot.suffix))
                emoji_query = False
                if self._rewrites_supported and not selected and snapshot.eligible(1):
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
                if not emoji_query and not selected and self.config.alternatives_enabled and self.config.alternatives_mode == "pause" and snapshot.eligible(1):
                    if now - self._candidate_since >= self.config.alternatives_delay_ms / 1000:
                        mode, delay, eligible = "alternatives", self.config.alternatives_delay_ms, True
                if self._on_battery and self.config.battery_on_demand:
                    eligible = False
                if (eligible and now - self._candidate_since >= delay / 1000
                        and self._attempted != (snapshot.key, mode)
                        and (mode != "completion" or not self._suggestion.text)):
                    self._submit(snapshot, mode)
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

    def _accepted(self, current, text, resulting_text):
        if self.config.statistics_enabled:
            self._records.append(("stats", current.app_id, text, not self._completion_counted))
        self._completion_counted = True
        if self._record_allowed(current.app_id, getattr(current, "domain", "")):
            self._history_suppressed = None
            self._observe_session(current)
            self._session_text = resulting_text
            self._session_accepted = True
            self._records.append(("history", self._session_id, current.app_id, self._session_domain, resulting_text))

    def _accept(self, full=False, choice=None):
        current = self._capture()
        if current is None or not current.has_anchor:
            self._clear(hide_accessory=True)
            return False
        if self._choices:
            index = 0 if choice is None else choice
            if (self._choice_snapshot is None or current.key != self._choice_snapshot.key
                    or not 0 <= index < len(self._choices)):
                self._clear()
                return False
            text = self._choices[index]
            start, length = self._replacement
            if self._mode == "synonyms":
                success = self.bridge.replace_selection(current, text)
            elif start == current.caret and length == 0:
                success = self.bridge.insert(current, text)
            else:
                success = self.bridge.replace_range(current, start, length, text)
            if not success:
                self._clear()
                return False
            resulting = current.text[:start] + text + current.text[start + length:]
            self._accepted(current, text, resulting)
            self._clear()
            self._attempted = (current.key, self._mode)
            return True
        if (choice is not None or self._suggestion.snapshot is None or current.key != self._suggestion.snapshot.key
                or not current.eligible(1) or not current.inline_safe):
            self._clear()
            return False
        include_punctuation = self.config.punctuation_mode == "always" or (self.config.punctuation_mode == "full" and full)
        text = self._suggestion.accept(
            current, full=full, trailing_space=self.config.trailing_space,
            trailing_punctuation=include_punctuation)
        updated = self._suggestion.snapshot
        # Compute the pure state transition under the decision lock, but commit
        # no statistics/history until the native adapter confirms insertion.
        if not text or updated is None or not self.bridge.insert(current, text):
            self._clear()
            return False
        self._client.cancel()
        self._request = None
        self._accepted(current, text, updated.text)
        self._candidate = updated
        self._capture_after = 0.0
        self._attempted = (updated.key, "completion")
        if not self._suggestion.text:
            self._suggestion.invalidate()
            self._emit({"type": "clear"})
        else:
            self._publish_completion()
        return True

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
                if any(exclusion.casefold() in value.casefold() for exclusion in self.config.excluded_apps
                       for value in (app, str(info.get("name", "")))):
                    return False
                self._commands.append((action, app, str(info.get("domain", ""))))
                return True
            if action == "settings":
                self._commands.append(action)
                self._emit({"type": "settings", "pane": "general"})
                return True
            if action in {"escape", "dismiss", "bypass"}:
                had_suggestion = bool(self._suggestion.text or self._choices or self._request or self._preparing)
                if not had_suggestion:
                    return False
                current = self._capture()
                expected = self._choice_snapshot or self._suggestion.snapshot or (self._request[1] if self._request else self._candidate)
                if current is None or not current.has_anchor or expected is None or current.key != expected.key:
                    self._clear(hide_accessory=True)
                    return False
                self._clear()
                self._attempted = (self._candidate.key, "completion") if self._candidate else None
                if action == "bypass":
                    return True
                if self.config.escape_behavior == "pause":
                    self._paused_until = time.monotonic() + self.config.escape_pause_seconds
                    self._client.release()
                elif self.config.escape_behavior == "app":
                    self._commands.append(("app_toggle", current.app_id, getattr(current, "domain", "")))
                return self.config.escape_behavior != "passthrough"
            if not self._active():
                return False
            if action in {"force", "alternatives", "synonyms"}:
                if action == "alternatives" and not self.config.alternatives_enabled:
                    return False
                if action == "synonyms" and (not self.config.synonyms_enabled or not self._rewrites_supported):
                    return False
                snapshot = self._capture()
                if snapshot is None or not snapshot.has_anchor or (action == "synonyms" and not self._selected_word(snapshot)):
                    return False
                if action != "synonyms" and not snapshot.eligible(1):
                    return False
                if action == "force" and not snapshot.inline_safe:
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

    def update_config(self, config: Config, persist=True):
        config.validate()
        if self._recording_configured(config):
            self._storage.enable_history()
        if persist:
            save_config(config)
        with self._lock:
            old = self.config
            self._finish_session()
            self.config = config
            self._screen_cache = None
            self._capture_after = 0.0
            self._error = ""
            self._clear()
            self._attempted = None
            self._candidate = None
            self.bridge.configure(self._native_config())
            self.bridge.set_shortcuts_enabled(True)
            self._client.set_idle_timeout(config.idle_release_seconds)
            if not self._active() or any(getattr(old, key) != getattr(config, key) for key in ("backend", "model_path", "endpoint", "model", "acceleration", "prompt_style")):
                self._client.release()
            if not self._recording_configured(config):
                self._session_text = ""
                self._session_key = None
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
            if not app or any(exclusion.casefold() in app.casefold() for exclusion in self.config.excluded_apps):
                return False
            self._finish_session()
            self._temporary_apps[app] = not self._allowed(app, domain)
            self._screen_cache = None
            self._clear(hide_accessory=True)
            self._candidate = None
            self._attempted = None
            self.bridge.configure(self._native_config())
            self._client.release()
            return True

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
            return {**status, "enabled": active,
                    "inline_available": bool(self._candidate and self._candidate.inline_safe),
                    "inline_message": ("Verified same-line completion is available." if self._candidate and self._candidate.inline_safe
                                       else "This field does not expose verified same-line rendering; ordinary completion is silent."),
                    "context_status": self._context_status,
                    "context_message": _CONTEXT_MESSAGES[self._context_status],
                    "current_app_enabled": self._allowed(self._app, self._domain),
                    "native_status": native_status, "current_app": self._app,
                    "current_domain": self._domain, "paused_until": self._paused_until,
                    "on_battery": self._on_battery, "download": dict(self._download) if self._download else None}

    def set_model(self, path, prompt_style="continuation"):
        self.update_config(replace(self.config, backend="gguf", model_path=str(path), prompt_style=prompt_style))

    def models(self):
        from .models import PRESETS, is_installed, model_path
        return [{"id": preset.id, "label": preset.label, "description": preset.description,
                 "path": str(model_path(preset)), "size_bytes": preset.size_bytes,
                 "installed": is_installed(preset), "prompt_style": preset.prompt_style} for preset in PRESETS]

    def download_model(self, preset_id):
        from .models import download_model, get_preset
        preset = get_preset(preset_id)
        with self._lock:
            if self._download_thread is not None and self._download_thread.is_alive():
                raise RuntimeError("Cancel the current model download before starting another.")
            cancel = self._download_cancel = threading.Event()
            self._download = {"id": preset_id, "received": 0, "total": preset.size_bytes, "state": "downloading", "error": ""}

            def progress(received, total):
                with self._lock:
                    if cancel is self._download_cancel and not self._stopped:
                        self._download.update(received=received, total=total)
                        self._emit({"type": "download", **self._download})

            def run():
                try:
                    path = download_model(preset_id, progress, cancel)
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

    def delete_history(self, app_id=None):
        with self._lock:
            self._records = deque((record for record in self._records if record[0] != "history" or (app_id is not None and record[2] != app_id)), maxlen=128)
            if app_id is None or (self._session_key and self._session_key[0] == app_id) or (self._candidate and self._candidate.app_id == app_id):
                self._session_key = None
                self._session_text = ""
                self._session_accepted = False
                self._history_suppressed = (
                    ((self._candidate.app_id, self._candidate.field_id, getattr(self._candidate, "domain", "")), self._candidate.text)
                    if self._candidate else None)
        self._storage.delete_history(app_id)

    def set_history_enabled(self, enabled):
        if type(enabled) is not bool:
            raise ValueError("History preference must be a boolean.")
        self.update_config(replace(self.config, record_history=enabled))

    def set_clipboard_reader(self, reader):
        """Install reader(*, authorized) -> str; GUI readers own their marshal."""
        self._clipboard_reader = reader

    def set_screen_reader(self, reader):
        self._screen_reader = reader

    def set_battery_reader(self, reader):
        self._battery_reader = reader

    def stop(self):
        with self._lock:
            if self._stopped:
                return
            self.set_wake_callback(None)
            self._finish_session()
            self._stopped = True
            self._screen_cache = None
            self._context_wake.set()
            self._screen_wake.set()
            self.cancel_download()
            self._clear(hide_accessory=True)
            self.bridge.stop()
        self._client.close()
        self._flush_records()
        self._storage.lock_history()
        for thread in (self._download_thread, self._context_thread, self._screen_thread):
            if thread is not None and thread is not threading.current_thread():
                thread.join(timeout=0.2)
