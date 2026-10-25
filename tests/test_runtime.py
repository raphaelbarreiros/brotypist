"""Consumer-visible headless typing, privacy and confirmed insertion behavior."""
from dataclasses import replace
import threading
import time

import pytest

from inklow.config import Config
from inklow.core import InlineRender, Snapshot
from inklow.runtime import Controller
from inklow.storage import LocalStorage


RENDER = InlineRender("Fixture Sans", 14, 34, (0, 0, 2000, 500), 14, 4, source="test-native")


def field_snapshot(text, caret=None, **changes):
    return Snapshot("writer", "body", text, len(text) if caret is None else caret,
                    caret_rect=(10, 20, 1, 18), inline_render=RENDER,
                    window_id="writer:window", window_revision="revision-1", **changes)


class Bridge:
    supports_atomic_replacement = True

    def __init__(self, text="Dear team, the update"):
        self.snapshot = field_snapshot(text)
        self.info = {"app_id": "writer", "name": "Writer", "domain": ""}
        self.captures = 0
        self.insertions = []
        self.accepts = True

    def configure(self, config):
        self.config = config

    def foreground(self):
        return self.info

    def capture(self):
        self.captures += 1
        return self.snapshot

    def context_matches(self, snapshot):
        current = self.snapshot
        return (current is not None and not current.protected and current.key == snapshot.key
                and current.window_id == snapshot.window_id
                and current.window_revision == snapshot.window_revision
                and self.info.get("app_id") == snapshot.app_id
                and self.info.get("domain", "") == snapshot.domain)

    def insert(self, snapshot, text):
        if not self.accepts or snapshot.key != self.snapshot.key or not snapshot.eligible(1):
            return False
        self.insertions.append(text)
        self.snapshot = replace(snapshot, text=snapshot.prefix + text + snapshot.suffix, caret=snapshot.caret + len(text))
        return True

    def replace_range(self, snapshot, start, length, text):
        if not self.accepts or snapshot.key != self.snapshot.key:
            return False
        self.insertions.append(text)
        self.snapshot = replace(snapshot, text=snapshot.text[:start] + text + snapshot.text[start + length:], caret=start + len(text), selection_length=0)
        return True

    def replace_selection(self, snapshot, text):
        if not snapshot.selection_length:
            return False
        return self.replace_range(snapshot, snapshot.caret, snapshot.selection_length, text)

    def start_shortcuts(self, callback):
        self.callback = callback

    def set_shortcuts_enabled(self, enabled):
        pass

    def status(self):
        return "Native test field"

    def stop(self):
        pass


class Inference:
    def __init__(self):
        self.identifier = 0
        self.requests = []
        self.events = []
        self.released = False

    def submit(self, config, prefix, suffix, **options):
        self.identifier += 1
        self.requests.append((config, prefix, suffix, options))
        return self.identifier

    def cancel(self):
        pass  # Intentionally deliver stale results to exercise Controller rejection.

    def release(self):
        self.released = True

    def close(self):
        pass

    def set_idle_timeout(self, seconds):
        pass

    def poll(self):
        result, self.events = self.events, []
        return result

    def status(self):
        return {"state": "idle", "message": "", "worker_pid": None, "model_loaded": False}


@pytest.fixture
def runtime(tmp_path, monkeypatch):
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    bridge = Bridge()
    controller = Controller(Config(), bridge)
    controller._capture_interval = 0
    controller._client = Inference()
    controller._storage = LocalStorage(tmp_path / "data.sqlite3", lambda: b"k" * 32)
    controller.start()
    yield controller, bridge, controller._client
    controller.stop()


def submit_command(controller, action):
    previous = controller._client.identifier
    assert controller.invoke(action)
    deadline = time.monotonic() + 2
    while controller._client.identifier == previous and time.monotonic() < deadline:
        controller.tick()
        time.sleep(0.005)
    assert controller._client.identifier > previous
    return controller._client.identifier


def offer(controller, bridge, text):
    controller.tick()
    identifier = submit_command(controller, "force")
    controller._client.events.append({"type": "result", "id": identifier, "text": text, "final": True})
    controller.tick()
    return identifier


def test_partial_acceptance_counts_only_confirmed_insertions_and_keeps_remainder(runtime):
    controller, bridge, client = runtime
    offer(controller, bridge, " arrives tomorrow morning")
    assert controller.invoke("word")
    assert bridge.insertions == [" arrives "]
    assert controller._suggestion.text == "tomorrow morning"
    assert controller.invoke("all")
    totals = controller.stats("today")["totals"]
    assert totals == {"completions": 1, "words": 3, "characters": len(" arrives tomorrow morning")}
    assert bridge.snapshot.text.endswith(" arrives tomorrow morning")


def test_rejected_native_insertion_does_not_count_or_mutate(runtime):
    controller, bridge, _ = runtime
    offer(controller, bridge, " arrives tomorrow")
    original = bridge.snapshot
    bridge.accepts = False
    assert not controller.invoke("word")
    assert bridge.snapshot == original
    assert controller.stats()["totals"]["characters"] == 0


def test_stale_stream_cannot_reappear_after_typing_or_field_change(runtime):
    controller, bridge, client = runtime
    identifier = offer(controller, bridge, " arrives tomorrow morning")
    snapshot = bridge.snapshot
    bridge.snapshot = replace(snapshot, text=snapshot.prefix + " arrives" + snapshot.suffix, caret=snapshot.caret + len(" arrives"))
    controller.tick()
    assert controller._suggestion.text == " tomorrow morning"
    client.events.append({"type": "result", "id": identifier, "text": " stale replacement", "final": True})
    controller.tick()
    assert controller._suggestion.text == " tomorrow morning"
    bridge.snapshot = replace(bridge.snapshot, field_id="another")
    assert not controller.invoke("all")
    assert not bridge.insertions


def test_domain_exclusion_is_checked_before_capture_and_optional_readers(runtime):
    controller, bridge, client = runtime
    controller.update_config(replace(controller.config, clipboard_context=True, screen_context=True,
        app_rules={"domain:private.example": {"enabled": False}}), persist=False)
    calls = []
    controller.set_clipboard_reader(lambda *, authorized: calls.append("clipboard") or "sensitive clipboard")
    controller.set_screen_reader(lambda snapshot, *, bridge, authorized, max_chars: calls.append("screen") or "sensitive screen")
    bridge.info = {"app_id": "browser", "name": "Browser", "domain": "private.example", "browser": True, "domain_observable": True}
    bridge.snapshot = replace(bridge.snapshot, app_id="browser", domain="private.example")
    controller.tick()
    assert bridge.captures == 0
    assert calls == [] and client.requests == []


def test_unobservable_browser_domain_fails_closed_without_treating_rule_as_unmatched(runtime):
    controller, bridge, client = runtime
    controller.update_config(replace(controller.config, app_rules={"domain:private.example": {"enabled": False}}), persist=False)
    bridge.info = {"app_id": "browser", "name": "Browser", "domain": "", "browser": True, "domain_observable": False}
    controller.tick()
    assert bridge.captures == 0
    assert not client.requests


def test_blocked_old_context_never_delays_fresh_prediction_or_reappears(runtime):
    controller, bridge, client = runtime
    controller.update_config(replace(controller.config, screen_context=True), persist=False)
    started, release, finished = threading.Event(), threading.Event(), threading.Event()
    calls = []
    def read_screen(snapshot, *, bridge, authorized, max_chars):
        calls.append(snapshot.field_id)
        if snapshot.field_id == "body":
            started.set()
            assert release.wait(2)
        try:
            return {"text": "update obsolete confidential reference", "provenance": "accessibility",
                    "window_id": snapshot.window_id, "revision": snapshot.window_revision, "status": "ready"}
        finally:
            finished.set()
    controller.set_screen_reader(read_screen)
    try:
        controller.tick()
        assert started.wait(1)
        bridge.snapshot = replace(bridge.snapshot, field_id="fresh", window_revision="revision-2")
        # The first reader remains blocked until AFTER the new generation is
        # submitted. This cannot pass by merely dropping a result afterward.
        submit_command(controller, "force")
        assert not release.is_set()
        assert client.requests[-1][1] == bridge.snapshot.prefix
        assert "obsolete" not in client.requests[-1][3]["context"]
        controller.pause(1)
        release.set()
        assert finished.wait(1)
        assert controller._screen_cache is None
        assert calls == ["body"]
    finally:
        release.set()


def test_synonyms_need_explicit_guarded_word_selection_and_numbered_confirmation(runtime):
    controller, bridge, client = runtime
    bridge.snapshot = Snapshot("writer", "body", "A useful update", 2, 6, (10, 20, 1, 18))
    assert not bridge.snapshot.eligible(1)
    assert not controller.force()
    assert not controller.invoke("word")
    submit_command(controller, "synonyms")
    client.events.append({"type": "result", "id": client.identifier, "text": '["helpful", "valuable"]', "final": True})
    controller.tick()
    assert not bridge.insertions
    assert not controller.invoke("choice:0")
    assert controller.invoke("choice:2")
    assert bridge.snapshot.text == "A valuable update"
    assert controller.stats()["totals"]["completions"] == 1


def test_alternatives_insert_after_caret_without_replacing_existing_word(runtime):
    controller, bridge, client = runtime
    controller.update_config(replace(controller.config, alternatives_enabled=True), persist=False)
    submit_command(controller, "alternatives")
    client.events.append({"type": "result", "id": client.identifier, "text": '[" arrives", " is ready"]', "final": True})
    controller.tick()
    original = bridge.snapshot.text
    assert controller.invoke("choice:1")
    assert bridge.snapshot.text == original + " arrives"


def test_temporary_pause_resumes_and_app_toggle_never_persists_rule(runtime, tmp_path):
    controller, bridge, client = runtime
    controller.tick()
    controller.pause(5)
    assert not controller.status()["enabled"] and client.released
    assert controller.toggle_enabled()
    assert controller.status()["enabled"]
    controller.toggle_current_app()
    assert not controller.status()["current_app_enabled"]
    assert controller.config.app_rules == {}
    assert not (tmp_path / "settings.json").exists()
    controller.toggle_current_app()
    assert controller.status()["current_app_enabled"]


def test_field_menu_toggle_keeps_captured_target_after_focus_moves(runtime, tmp_path):
    controller, bridge, _ = runtime
    bridge.info = {"app_id": "settings-host", "name": "Settings", "domain": ""}
    assert controller.toggle_current_app(app="writer", domain="")
    assert not controller._allowed("writer", "")
    assert controller._allowed("settings-host", "")
    assert controller.config.app_rules == {}
    assert not (tmp_path / "settings.json").exists()
    assert controller.toggle_current_app(app="writer", domain="")
    assert controller._allowed("writer", "")


def test_default_disabled_can_be_explicitly_enabled_for_one_app(runtime):
    controller, bridge, _ = runtime
    controller.update_config(replace(controller.config, enabled=False, app_rules={"app:writer": {"enabled": True}}), persist=False)
    controller.tick()
    assert bridge.captures > 0
    assert controller.force()


def test_no_history_for_short_sensitive_or_excluded_sessions(runtime):
    controller, bridge, _ = runtime
    controller.update_config(replace(controller.config, record_history=True), persist=False)
    offer(controller, bridge, " soon")
    controller.invoke("all")
    controller.tick()
    assert controller.history_info()["count"] == 0
    sensitive = "This is a sufficiently long synthetic message with many ordinary words and password: do-not-store-this-secret in the middle of the sentence."
    bridge.snapshot = replace(bridge.snapshot, text=sensitive, caret=len(sensitive))
    offer(controller, bridge, " safely")
    controller.invoke("all")
    controller.tick()
    assert controller.history_info()["count"] == 0
    captures = bridge.captures
    bridge.info = {"app_id": "1password", "name": "1Password", "domain": ""}
    bridge.snapshot = replace(bridge.snapshot, app_id="1password")
    controller.tick()
    assert bridge.captures == captures
    assert controller.history_info()["count"] == 0


def test_failed_secure_store_optin_never_saves_true(runtime, tmp_path):
    controller, _, _ = runtime
    def unavailable():
        raise RuntimeError("Locked synthetic keychain")
    controller._storage = LocalStorage(tmp_path / "locked.sqlite3", unavailable)
    with pytest.raises(RuntimeError, match="plaintext"):
        controller.set_history_enabled(True)
    assert controller.config.record_history is False
    assert not (tmp_path / "settings.json").exists()


LONG_SESSION = ("The planning meeting covered observability and instrumentation with the entire team. "
                "We agreed to review the instrumentation examples before the next observability meeting.")


def test_default_history_records_only_accepted_long_sessions_and_delete_stays_deleted(runtime):
    controller, bridge, _ = runtime
    controller.update_config(replace(controller.config, record_history=True), persist=False)
    bridge.snapshot = replace(bridge.snapshot, text=LONG_SESSION, caret=len(LONG_SESSION))
    controller.tick()
    bridge.info = {"app_id": "other", "name": "Other", "domain": ""}
    controller.tick()
    assert controller.history_info()["count"] == 0
    bridge.info = {"app_id": "writer", "name": "Writer", "domain": ""}
    offer(controller, bridge, " Thank you.")
    assert controller.invoke("all")
    controller.tick()
    assert controller.history_info()["count"] == 1
    controller.update_config(replace(controller.config, record_unaccepted=True), persist=False)
    controller.tick()
    controller.delete_history()
    bridge.snapshot = replace(bridge.snapshot, caret=bridge.snapshot.caret - 1)
    controller.tick()
    controller.stop()
    assert controller.history_info()["count"] == 0


def test_all_monitored_history_and_per_app_record_exclusion_are_real(runtime):
    controller, bridge, _ = runtime
    controller.update_config(replace(controller.config, record_history=True, record_unaccepted=True,
        app_rules={"app:excluded-writer": {"record_history": False}}), persist=False)
    bridge.snapshot = replace(bridge.snapshot, text=LONG_SESSION, caret=len(LONG_SESSION))
    controller.tick()
    bridge.info = {"app_id": "excluded-writer", "name": "Other", "domain": ""}
    bridge.snapshot = replace(bridge.snapshot, app_id="excluded-writer", field_id="private")
    controller.tick()
    assert controller.history_info()["count"] == 1
    bridge.info = {"app_id": "other", "name": "Other", "domain": ""}
    controller.tick()
    assert controller.history_info()["count"] == 1


def test_unobservable_domain_record_exclusion_stops_browser_history(runtime):
    controller, bridge, _ = runtime
    controller.update_config(replace(controller.config, record_history=True, record_unaccepted=True,
        app_rules={"domain:private.example": {"record_history": False}}), persist=False)
    bridge.info = {"app_id": "browser", "name": "Browser", "domain": "", "browser": True, "domain_observable": False}
    bridge.snapshot = replace(bridge.snapshot, app_id="browser", text=LONG_SESSION, caret=len(LONG_SESSION))
    controller.tick()
    bridge.info = {"app_id": "other", "name": "Other", "domain": ""}
    controller.tick()
    assert controller.history_info()["count"] == 0


def test_slow_statistics_write_never_holds_the_keyboard_decision_lock(runtime):
    controller, bridge, _ = runtime
    offer(controller, bridge, " arrives tomorrow morning")
    assert controller.invoke("word")
    started, release, finished = threading.Event(), threading.Event(), threading.Event()
    original = controller._storage.record_acceptance
    def slow_write(*args, **kwargs):
        started.set()
        release.wait(2)
        return original(*args, **kwargs)
    controller._storage.record_acceptance = slow_write
    flush = threading.Thread(target=controller._flush_records)
    flush.start()
    try:
        assert started.wait(1)
        def accept():
            assert controller.invoke("word")
            finished.set()
        decision = threading.Thread(target=accept)
        decision.start()
        assert finished.wait(0.5), "Keyboard decision waited on disk I/O"
        decision.join(1)
    finally:
        release.set()
        flush.join(3)


def test_global_app_domain_and_recorded_vocabulary_reach_real_prompt_at_selected_strength(runtime):
    from inklow.providers import _prompt
    controller, bridge, client = runtime
    controller._storage.record_history("seed", "writer", LONG_SESSION)
    controller.update_config(replace(controller.config, custom_instructions="Use Portuguese.",
        personalization_strength=90, app_rules={
            "app:writer": {"custom_instructions": "Keep a professional tone."},
            "domain:work.example": {"custom_instructions": "Prefer short sentences."}}), persist=False)
    bridge.info = {"app_id": "writer", "name": "Writer", "domain": "work.example", "browser": True, "domain_observable": True}
    bridge.snapshot = replace(bridge.snapshot, domain="work.example")
    submit_command(controller, "force")
    config, prefix, suffix, options = client.requests[-1]
    prompt = _prompt(config, prefix, suffix, options)
    assert all(value in prompt for value in ("Use Portuguese.", "professional tone", "short sentences", "observability", "strongly"))
    controller.update_config(replace(controller.config, personalization_strength=0), persist=False)
    submit_command(controller, "force")
    config, prefix, suffix, options = client.requests[-1]
    prompt = _prompt(config, prefix, suffix, options)
    assert "recurring vocabulary" not in prompt
    assert "professional tone" in prompt


def test_trailing_material_can_be_accepted_later_without_counting_punctuation_as_word(runtime):
    controller, bridge, _ = runtime
    controller.update_config(replace(controller.config, trailing_space=False, punctuation_mode="never"), persist=False)
    offer(controller, bridge, " arrives. ")
    assert controller.invoke("word")
    assert bridge.insertions == [" arrives"]
    assert controller.invoke("all")
    assert bridge.insertions == [" arrives", "."]
    assert controller.stats()["totals"]["words"] == 1


def test_stale_escape_does_not_swallow_key_or_pause_a_different_field(runtime):
    controller, bridge, _ = runtime
    offer(controller, bridge, " arrives tomorrow")
    bridge.snapshot = replace(bridge.snapshot, field_id="other")
    assert not controller.invoke("escape")
    assert controller.status()["enabled"]
    assert not bridge.insertions


def test_queued_force_is_bound_to_exact_field_and_explicit_force_can_use_short_prefix(runtime):
    controller, bridge, client = runtime
    assert controller.force()
    bridge.snapshot = replace(bridge.snapshot, field_id="other")
    controller.tick()
    assert not client.requests
    bridge.snapshot = field_snapshot("A")
    submit_command(controller, "force")
    assert client.requests[-1][1] == "A"


def test_queued_app_toggle_targets_original_app_not_new_focus(runtime):
    controller, bridge, _ = runtime
    controller.tick()
    assert controller.invoke("app_toggle")
    bridge.info = {"app_id": "other", "name": "Other", "domain": ""}
    bridge.snapshot = replace(bridge.snapshot, app_id="other")
    controller.tick()
    assert controller.status()["current_app_enabled"]
    captures = bridge.captures
    bridge.info = {"app_id": "writer", "name": "Writer", "domain": ""}
    bridge.snapshot = replace(bridge.snapshot, app_id="writer")
    controller.tick()
    assert not controller.status()["current_app_enabled"]
    assert bridge.captures == captures


@pytest.mark.parametrize("mode,action,expected", [
    ("full", "word", " arrives"), ("full", "all", " arrives."),
    ("always", "word", " arrives."), ("always", "all", " arrives."),
    ("never", "word", " arrives"), ("never", "all", " arrives"),
])
def test_three_punctuation_modes_apply_to_word_and_full_acceptance(runtime, mode, action, expected):
    controller, bridge, _ = runtime
    controller.update_config(replace(controller.config, trailing_space=False, punctuation_mode=mode), persist=False)
    offer(controller, bridge, " arrives. ")
    assert controller.invoke(action)
    assert bridge.insertions == [expected]


def test_missing_caret_bounds_never_accept_an_invisible_completion(runtime):
    controller, bridge, _ = runtime
    offer(controller, bridge, " arrives tomorrow")
    bridge.snapshot = replace(bridge.snapshot, caret_rect=None)
    assert not controller.invoke("word")
    assert not controller.force()
    assert not bridge.insertions


def test_caret_geometry_changes_reposition_existing_ghost_without_regenerating(runtime):
    controller, bridge, client = runtime
    offer(controller, bridge, " arrives tomorrow")
    requests = len(client.requests)
    controller.drain_events()
    bridge.snapshot = replace(bridge.snapshot, caret_rect=(150, 220, 1, 18),
                              inline_render=replace(RENDER, baseline=234))
    controller.tick()
    events = controller.drain_events()
    suggestion = next(event for event in events if event["type"] == "suggestion")
    assert suggestion["rect"] == [150, 220, 1, 18]
    assert suggestion["inline"] is True
    assert len(client.requests) == requests


@pytest.mark.parametrize("action,expected", [("word", " arrives "), ("all", " arrives")])
def test_exact_final_word_uses_core_snapshot_including_requested_space(runtime, action, expected):
    controller, bridge, _ = runtime
    offer(controller, bridge, " arrives")
    assert controller.invoke(action)
    assert bridge.insertions == [expected]
    assert controller._candidate.key == bridge.snapshot.key
    assert controller.stats()["totals"]["characters"] == len(expected)


def test_accepting_first_streamed_word_adds_separator_and_rejects_later_old_stream(runtime):
    controller, bridge, client = runtime
    identifier = submit_command(controller, "force")
    client.events.append({"type": "result", "id": identifier, "text": " arrives", "final": False})
    controller.tick()
    assert controller.invoke("word")
    assert bridge.snapshot.text.endswith(" arrives ")
    assert controller._candidate.key == bridge.snapshot.key
    client.events.append({"type": "result", "id": identifier, "text": " arrives tomorrow", "final": True})
    controller.tick()
    assert not controller._suggestion.text
    assert bridge.insertions == [" arrives "]


def test_existing_same_line_suffix_never_intercepts_or_displays_a_completion(runtime):
    controller, bridge, client = runtime
    original = bridge.snapshot.text
    bridge.snapshot = replace(bridge.snapshot, text=original + " tomorrow")
    assert not controller.invoke("force")
    controller.tick()
    assert not controller.invoke("word")
    assert not bridge.insertions
    assert not client.requests
    assert not any(event["type"] == "suggestion" for event in controller.drain_events())


def test_rejected_space_adding_acceptance_has_no_counter_or_snapshot_commit(runtime):
    controller, bridge, _ = runtime
    offer(controller, bridge, " arrives")
    original = bridge.snapshot
    bridge.accepts = False
    assert not controller.invoke("word")
    assert bridge.snapshot == original
    assert not controller._suggestion.text
    assert controller.stats()["totals"]["characters"] == 0


def test_linux_rewrite_gate_preserves_preferences_without_work_or_unusable_choices(tmp_path, monkeypatch):
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    monkeypatch.setattr("inklow.runtime.sys.platform", "linux")
    monkeypatch.delattr(Bridge, "supports_atomic_replacement")
    bridge = Bridge()
    controller = Controller(replace(Config(), min_chars=1000, alternatives_enabled=True), bridge)
    controller._client = Inference()
    controller.start()
    try:
        bridge.snapshot = Snapshot("writer", "body", "A useful update", 2, 6, (10, 20, 1, 18))
        controller.tick()
        controller._candidate_since -= 10
        controller.tick()
        assert not controller.invoke("synonyms")
        assert controller.config.synonyms_enabled
        assert not controller._client.requests
        bridge.snapshot = Snapshot("writer", "body", "Hello :smile", len("Hello :smile"), caret_rect=(10, 20, 1, 18))
        controller.tick()
        controller._candidate_since -= 10
        controller.tick()
        assert not controller._choices
        assert not controller._client.requests
        assert controller.config.emoji_enabled and controller.config.emoticons_enabled
        assert "atomic text replacement" in controller.status()["native_status"].lower()
        submit_command(controller, "alternatives")
        client = controller._client
        client.events.append({"type": "result", "id": client.identifier, "text": '[" next"]', "final": True})
        controller.tick()
        before = bridge.snapshot.text
        assert controller.invoke("choice:1")
        assert bridge.snapshot.text == before + " next"
    finally:
        controller.stop()


def test_emoji_chooser_takes_precedence_over_pause_alternatives(runtime):
    controller, bridge, client = runtime
    controller.update_config(replace(controller.config, alternatives_enabled=True, alternatives_mode="pause"), persist=False)
    bridge.snapshot = Snapshot("writer", "body", "Hello :smile", 12, caret_rect=(10, 20, 1, 18))
    controller.tick()
    controller._candidate_since -= 5
    controller.tick()
    assert not client.requests
    assert controller.invoke("choice:1")
    assert bridge.snapshot.text == "Hello 😄"


def test_non_tab_word_shortcut_remains_available_when_app_disables_tab(runtime):
    controller, bridge, _ = runtime
    controller.update_config(replace(controller.config, shortcuts={**controller.config.shortcuts, "word": "F8"},
                                     app_rules={"app:writer": {"tab_enabled": False}}), persist=False)
    offer(controller, bridge, " arrives tomorrow")
    assert controller.invoke("word")
    assert bridge.snapshot.text == "Dear team, the update arrives "


def test_domain_can_reenable_tab_disabled_by_browser_rule(runtime):
    from inklow.native.base import BridgeBase
    controller, bridge, _ = runtime
    config = replace(controller.config, app_rules={"app:writer": {"tab_enabled": False},
                                                  "domain:example.com": {"tab_enabled": True}})
    controller.update_config(config, persist=False)
    bridge.info["domain"] = "example.com"
    bridge.snapshot = replace(bridge.snapshot, domain="example.com")
    offer(controller, bridge, " arrives tomorrow")
    router = BridgeBase()
    router.configure(config)
    router.foreground = bridge.foreground
    router.start_shortcuts(controller.invoke)
    router.set_shortcuts_enabled(True)
    assert router._dispatch(router._key_action("Tab"))
    assert bridge.snapshot.text == "Dear team, the update arrives "


@pytest.mark.parametrize("selected,replacement", [("very useful", "helpful"), ("cafe\u0301", "bistro")])
def test_synonyms_replace_short_phrases_and_decomposed_unicode_without_touching_context(runtime, selected, replacement):
    controller, bridge, client = runtime
    bridge.snapshot = Snapshot("writer", "body", "A " + selected + " update", 2, len(selected), (10, 20, 1, 18))
    submit_command(controller, "synonyms")
    client.events.append({"type": "result", "id": client.identifier,
                          "text": '["' + replacement + '"]', "final": True})
    controller.tick()
    assert controller.invoke("choice:1")
    assert bridge.snapshot.text == "A " + replacement + " update"


def test_empty_field_shows_accessory_without_prediction_and_hides_after_blur(runtime):
    controller, bridge, client = runtime
    bridge.snapshot = Snapshot("writer", "empty", "", 0, field_rect=(100, 200, 400, 50))
    controller.tick()
    events = controller.drain_events()
    anchor = next(event for event in events if event["type"] == "caret")
    assert anchor["field_rect"] == [100, 200, 400, 50]
    assert not client.requests
    bridge.snapshot = None
    controller.tick()
    assert any(event["type"] == "clear" and event.get("hide_accessory") for event in controller.drain_events())


def test_field_accessory_moves_without_regenerating_text(runtime):
    controller, bridge, client = runtime
    bridge.snapshot = replace(bridge.snapshot, field_rect=(100, 200, 400, 50))
    controller.tick()
    controller.drain_events()
    bridge.snapshot = replace(bridge.snapshot, field_rect=(150, 250, 400, 50))
    controller.tick()
    anchor = next(event for event in controller.drain_events() if event["type"] == "caret")
    assert anchor["field_rect"] == [150, 250, 400, 50]
    assert not client.requests


def test_protected_empty_field_never_shows_brand_accessory(runtime):
    controller, bridge, client = runtime
    bridge.snapshot = Snapshot("writer", "password", "", 0, protected=True, field_rect=(100, 200, 400, 50))
    controller.tick()
    assert not any(event["type"] == "caret" for event in controller.drain_events())
    assert not client.requests


def test_field_bounds_without_host_metrics_cannot_offer_or_accept_completion(runtime):
    controller, bridge, client = runtime
    bridge.snapshot = replace(bridge.snapshot, caret_rect=None, field_rect=(100, 200, 400, 50))
    controller.tick()
    assert not controller.force()
    controller._candidate_since -= 1
    controller.tick()
    assert not client.requests
    assert not controller.status()["inline_available"]
    assert not controller.invoke("word")
    assert bridge.insertions == []
    assert not any(event["type"] == "suggestion" for event in controller.drain_events())


def test_native_partial_acceptance_survives_new_revision_without_global_geometry(runtime):
    # R2: same native context, fresh revision/serial, exact remaining acceptance.
    controller, bridge, _ = runtime
    bridge.snapshot = replace(bridge.snapshot, field_id="body:wl:101", caret_rect=None,
                              native_anchor="7:19", native_inline=True)
    offer(controller, bridge, " arrives tomorrow morning")
    assert controller.invoke("word")
    assert bridge.snapshot.text == "Dear team, the update arrives "
    bridge.snapshot = replace(bridge.snapshot, native_anchor="8:20")
    controller.drain_events()
    controller.tick()
    event = next(event for event in controller.drain_events() if event["type"] == "suggestion")
    assert event["text"] == "tomorrow morning"
    assert event["native_anchor"] == "8:20" and event["rect"] is None
    assert controller.invoke("all")
    assert bridge.snapshot.text == "Dear team, the update arrives tomorrow morning"
    assert bridge.insertions == [" arrives ", "tomorrow morning"]
    assert controller.stats()["totals"] == {
        "completions": 1, "words": 3, "characters": len(" arrives tomorrow morning")}


def test_native_typing_through_candidate_keeps_suffix_and_rejects_old_stream(runtime):
    # R2: typed-through text is not counted/inserted again after a native revision.
    controller, bridge, client = runtime
    prefix, suffix = "The next update is", ""
    bridge.snapshot = Snapshot("writer", "body:wl:101", prefix + suffix, len(prefix),
                               native_anchor="7:19", native_inline=True)
    identifier = offer(controller, bridge, " ready for review")
    bridge.snapshot = replace(bridge.snapshot, text=prefix + " ready" + suffix,
                              caret=len(prefix + " ready"), native_anchor="8:20")
    controller.tick()
    assert controller._suggestion.text == " for review"
    client.events.append({"type": "result", "id": identifier, "text": " obsolete text", "final": True})
    controller.tick()
    assert controller._suggestion.text == " for review"
    assert controller.invoke("all")
    assert bridge.snapshot.text == "The next update is ready for review"
    assert bridge.insertions == [" for review"]
    assert controller.stats()["totals"]["characters"] == len(" for review")


def test_native_reactivated_context_cannot_accept_identical_old_offer(runtime):
    # R1/R2: unchanged visible text does not make another native lifetime safe.
    controller, bridge, _ = runtime
    bridge.snapshot = replace(bridge.snapshot, field_id="body:wl:101", caret_rect=None,
                              native_anchor="7:19", native_inline=True)
    offer(controller, bridge, " arrives tomorrow")
    bridge.snapshot = replace(bridge.snapshot, field_id="body:wl:102", native_anchor="8:20")
    original = bridge.snapshot
    assert not controller.invoke("all")
    assert bridge.snapshot == original and bridge.insertions == []
    assert controller._suggestion.text == ""
    assert controller.stats()["totals"]["characters"] == 0


@pytest.mark.parametrize("change", ["pause", "disable", "consent", "exclusion", "protected", "window"])
def test_inflight_context_rechecks_current_policy_before_sensitive_read(runtime, change):
    # Context safety: revocation between native phases must prevent the read,
    # not just hide an already-captured private result.
    controller, bridge, _ = runtime
    controller.config = replace(controller.config, screen_context=True)
    snapshot = bridge.snapshot
    controller._candidate = snapshot
    token = (object(), snapshot.key, controller._window_key(snapshot))
    controller._screen_job = token
    sensitive_reads = []

    def reader(snapshot, *, authorized, **kwargs):
        if change == "pause":
            controller.pause(1)
        elif change == "disable":
            controller.update_config(replace(controller.config, global_enabled=False), persist=False)
        elif change == "consent":
            controller.update_config(replace(controller.config, screen_context=False), persist=False)
        elif change == "exclusion":
            controller.update_config(replace(controller.config, excluded_apps=("writer",)), persist=False)
        elif change == "protected":
            bridge.snapshot = replace(snapshot, protected=True)
        else:
            bridge.snapshot = replace(snapshot, window_id="other-window")
        if authorized():
            sensitive_reads.append("private window read")
        return {"text": "update private reference", "provenance": "accessibility",
                "status": "ready", "window_id": snapshot.window_id, "revision": snapshot.window_revision}

    controller.set_screen_reader(reader)
    controller._read_window_context(token, snapshot)
    assert sensitive_reads == []
    assert controller._screen_cache is None or controller._screen_cache[3] == ""


def test_clipboard_consent_is_checked_again_after_focus_validation(runtime):
    controller, bridge, _ = runtime
    controller.config = replace(controller.config, clipboard_context=True, personalization_strength=0)
    snapshot = bridge.snapshot
    controller._candidate = snapshot
    token = (snapshot.key, controller._suggestion.generation, "completion")
    controller._preparing = token
    reads = []
    def matches(expected):
        controller.config = replace(controller.config, clipboard_context=False)
        return True
    bridge.context_matches = matches
    controller.set_clipboard_reader(lambda *, authorized: reads.append("clipboard") or "update private clipboard")
    options = controller._options(snapshot, "completion", token)
    assert reads == []
    assert options["context"] == ""


@pytest.mark.parametrize("change", ["pause", "disable", "consent", "exclusion", "protected", "window"])
def test_clipboard_result_cannot_survive_inflight_authorization_change(runtime, change):
    controller, bridge, client = runtime
    controller.config = replace(controller.config, clipboard_context=True, personalization_strength=0)
    snapshot = bridge.snapshot
    controller._candidate = snapshot
    token = (snapshot.key, controller._suggestion.generation, "completion")
    controller._preparing = token
    checks = []

    def reader(*, authorized):
        assert authorized()
        if change == "pause":
            controller.pause(1)
        elif change == "disable":
            controller.update_config(replace(controller.config, global_enabled=False), persist=False)
        elif change == "consent":
            controller.update_config(replace(controller.config, clipboard_context=False), persist=False)
        elif change == "exclusion":
            controller.update_config(replace(controller.config, excluded_apps=("writer",)), persist=False)
        elif change == "protected":
            bridge.snapshot = replace(snapshot, protected=True)
        else:
            bridge.snapshot = replace(snapshot, window_id="other-window")
        checks.append(authorized())
        return "The update includes private clipboard reference."

    controller.set_clipboard_reader(reader)
    options = controller._options(snapshot, "completion", token)
    assert checks == [False]
    assert options["context"] == "" and options["_references"] == []
    assert controller.status()["context_status"] != "ready"
    assert client.requests == []


def test_clipboard_errors_expose_only_fixed_non_content_diagnostic(runtime):
    controller, bridge, _ = runtime
    controller.config = replace(controller.config, clipboard_context=True, personalization_strength=0)
    snapshot = bridge.snapshot
    controller._candidate = snapshot
    token = (snapshot.key, controller._suggestion.generation, "completion")
    controller._preparing = token

    def reader(*, authorized):
        assert authorized()
        raise RuntimeError("Fixture private clipboard content")

    controller.set_clipboard_reader(reader)
    options = controller._options(snapshot, "completion", token)
    status = controller.status()
    assert options["context"] == "" and options["_references"] == []
    assert status["context_status"] == "error"
    assert status["context_message"] == "Optional context could not be read. Check Accessibility permissions or turn it off."
    assert "Fixture private" not in str(status)


def test_focused_context_reaches_real_prompt_with_provenance_and_no_unrelated_lines(runtime):
    from inklow.providers import _prompt
    controller, bridge, client = runtime
    controller.config = replace(controller.config, screen_context=True, prompt_style="writing", personalization_strength=0)
    snapshot = bridge.snapshot
    controller._candidate = snapshot
    capture_token = (object(), snapshot.key, controller._window_key(snapshot))
    controller._screen_job = capture_token
    controller.set_screen_reader(lambda snapshot, **kwargs: {
        "text": snapshot.text + "\nThe update arrives Friday after review.\nLunch reservation confirmed.",
        "provenance": "accessibility", "status": "ready",
        "window_id": snapshot.window_id, "revision": snapshot.window_revision})
    controller._read_window_context(capture_token, snapshot)
    token = (snapshot.key, controller._suggestion.generation, "completion")
    controller._preparing = token
    controller._prepare_options(token, snapshot, "completion")
    controller._consume_prepared()
    config, prefix, suffix, options = client.requests[-1]
    prompt = _prompt(config, prefix, suffix, options)
    assert "Focused-window Accessibility (untrusted reference data)" in prompt
    assert "The update arrives Friday after review." in prompt
    assert "Lunch reservation" not in prompt
    assert prompt.count(snapshot.text) == 1
    assert not any(key.startswith("_") for key in options)


def test_context_cache_rejects_other_window_revision_and_expired_text(runtime, monkeypatch):
    controller, bridge, _ = runtime
    controller.config = replace(controller.config, screen_context=True)
    snapshot = bridge.snapshot
    clock = [100.0]
    monkeypatch.setattr("inklow.runtime.time.monotonic", lambda: clock[0])
    controller._screen_cache = (controller._window_key(snapshot), clock[0],
                                "accessibility", "The update arrives Friday.", "ready")
    assert "Friday" in controller._reference_options(snapshot, [])
    assert controller._reference_options(replace(snapshot, window_id="another"), []) == ""
    assert controller._reference_options(replace(snapshot, window_revision="revision-2"), []) == ""
    clock[0] += 2.01
    assert controller._reference_options(snapshot, []) == ""


def test_context_errors_expose_only_fixed_non_content_diagnostic(runtime):
    controller, bridge, _ = runtime
    controller.config = replace(controller.config, screen_context=True)
    snapshot = bridge.snapshot
    controller._candidate = snapshot
    token = (object(), snapshot.key, controller._window_key(snapshot))
    controller._screen_job = token
    def reader(*args, **kwargs):
        raise RuntimeError("synthetic private draft and clipboard must not appear")
    controller.set_screen_reader(reader)
    controller._read_window_context(token, snapshot)
    status = controller.status()
    assert status["context_status"] == "error"
    assert status["context_message"] == "Optional context could not be read. Check Accessibility permissions or turn it off."
    assert "synthetic private" not in str(status)


def test_fast_event_drain_does_not_multiply_native_capture(runtime, monkeypatch):
    controller, bridge, client = runtime
    controller._capture_interval = 0.1
    clock = [100.0]
    monkeypatch.setattr("inklow.runtime.time.monotonic", lambda: clock[0])
    controller.tick()
    captures = bridge.captures
    snapshot = bridge.snapshot
    controller._request = (1, snapshot, controller._suggestion.generation, "completion")
    client.events.append({"type": "result", "id": 1, "text": " arrives tomorrow", "final": True})
    for step in (0.025, 0.04, 0.049):
        clock[0] = 100.0 + step
        controller.tick()
    assert bridge.captures == captures
    event = next(event for event in controller.drain_events() if event["type"] == "suggestion")
    assert event["text"] == " arrives tomorrow"
    assert event["inline"] is True
    assert event["render"]["font_name"] == "Fixture Sans"
    clock[0] = 100.051
    controller.tick()
    assert bridge.captures == captures + 1


def test_midline_and_changed_host_metrics_cannot_leave_acceptance_enabled(runtime):
    controller, bridge, _ = runtime
    offer(controller, bridge, " arrives tomorrow")
    bridge.snapshot = replace(bridge.snapshot, inline_render=None)
    controller.tick()
    assert not controller.status()["inline_available"]
    assert not controller.invoke("word")
    assert bridge.insertions == []
    bridge.snapshot = field_snapshot("The update on Friday", caret=len("The update"))
    controller.tick()
    assert not controller.force()
    assert not controller.invoke("all")


def test_empty_or_excluded_focus_drops_cached_reference_without_new_capture(runtime):
    controller, bridge, _ = runtime
    controller.config = replace(controller.config, screen_context=True)
    snapshot = bridge.snapshot
    controller._screen_cache = (controller._window_key(snapshot), time.monotonic(),
                                "accessibility", "The update arrives Friday.", "ready")
    reads = []
    controller.set_screen_reader(lambda *args, **kwargs: reads.append("window"))
    bridge.snapshot = replace(snapshot, text="", caret=0)
    controller.tick()
    assert controller._screen_cache is None
    assert reads == []
    controller._screen_cache = (controller._window_key(snapshot), time.monotonic(),
                                "accessibility", "The update arrives Friday.", "ready")
    bridge.info = {"app_id": "1password", "name": "1Password", "domain": ""}
    controller.tick()
    assert controller._screen_cache is None
    assert reads == []


def test_capture_polling_returns_to_idle_and_reaccelerates_on_text_change(runtime, monkeypatch):
    controller, bridge, _ = runtime
    controller.config = replace(controller.config, min_chars=1000)
    controller._capture_interval = 0.1
    clock = [100.0]
    monkeypatch.setattr("inklow.runtime.time.monotonic", lambda: clock[0])
    controller.tick()
    clock[0] = 102.1
    controller.tick()
    captures = bridge.captures
    clock[0] = 102.15
    controller.tick()
    assert bridge.captures == captures
    bridge.snapshot = replace(bridge.snapshot, text=bridge.snapshot.text + "!", caret=bridge.snapshot.caret + 1)
    clock[0] = 102.201
    controller.tick()
    assert bridge.captures == captures + 1
    clock[0] = 102.252
    controller.tick()
    assert bridge.captures == captures + 2


def test_timestamp_only_native_input_hint_triggers_next_tick_capture(runtime, monkeypatch):
    controller, bridge, _ = runtime
    controller.config = replace(controller.config, min_chars=1000)
    controller._capture_interval = 0.1
    revision = [0]
    bridge.input_revision = lambda: revision[0]
    clock = [100.0]
    monkeypatch.setattr("inklow.runtime.time.monotonic", lambda: clock[0])
    controller.tick()
    clock[0] = 102.1
    controller.tick()
    captures = bridge.captures
    bridge.snapshot = replace(bridge.snapshot, text=bridge.snapshot.text + "!", caret=bridge.snapshot.caret + 1)
    revision[0] += 1
    clock[0] = 102.125
    controller.tick()
    assert bridge.captures == captures + 1
    assert controller._candidate.text == bridge.snapshot.text
    clock[0] = 102.15
    controller.tick()
    assert bridge.captures == captures + 1


def test_partial_acceptance_waits_for_observed_new_caret_before_redrawing_suffix(runtime):
    controller, bridge, _ = runtime
    offer(controller, bridge, " arrives tomorrow morning")
    controller.drain_events()
    assert controller.invoke("word")
    assert controller._suggestion.text == "tomorrow morning"
    assert not any(event["type"] == "suggestion" for event in controller.drain_events())
    assert controller._capture_after == 0
    bridge.snapshot = replace(bridge.snapshot, caret_rect=(70, 20, 1, 18))
    controller.tick()
    event = next(event for event in controller.drain_events() if event["type"] == "suggestion")
    assert event["text"] == "tomorrow morning"
    assert event["rect"] == [70, 20, 1, 18]
    assert event["render"]["font_name"] == "Fixture Sans"


@pytest.mark.parametrize("same_field", [True, False])
def test_typing_pause_uses_native_input_time_only_for_the_same_field(runtime, monkeypatch, same_field):
    import threading
    controller, bridge, client = runtime
    controller.config = replace(controller.config, debounce_ms=150)
    clock = [100.0]
    monkeypatch.setattr("inklow.runtime.time.monotonic", lambda: clock[0])
    ready = threading.Event()
    controller.set_wake_callback(ready.set)
    controller.tick()
    bridge.input_timestamp = lambda: 100.04
    text = bridge.snapshot.text + "!"
    bridge.snapshot = replace(
        bridge.snapshot, text=text, caret=len(text),
        field_id=bridge.snapshot.field_id if same_field else "another-field")
    clock[0] = 100.12  # AX publishes the edit after the native input arrived.
    controller.tick()
    assert not client.requests
    clock[0] = 100.191
    controller.tick()
    if same_field:
        assert ready.wait(1), "The completed typing pause did not schedule preparation"
        controller.tick()
        assert client.requests[-1][1] == text
    else:
        assert not ready.is_set()
        assert not client.requests
