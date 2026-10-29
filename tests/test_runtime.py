"""Headless controller contracts for privacy, focus, presentation and insertion.

Native fields and inference are controlled doubles; these tests exercise the
controller and temporary storage, not OS input delivery or real model output.
"""
from dataclasses import replace
import json
import threading
import time

import pytest

from inklow.config import Config
from inklow.core import InlineRender, Snapshot
from inklow.runtime import Controller
from inklow.storage import LocalStorage


RENDER = InlineRender("Fixture Sans", 14, 34, (0, 0, 2000, 500), 14, 4, source="test-native")


def field_snapshot(text, caret=None, **changes):
    """Build an editable field with synthetic host metrics and stable identity."""
    return Snapshot("writer", "body", text, len(text) if caret is None else caret,
                    caret_rect=(10, 20, 1, 18), inline_render=RENDER,
                    window_id="writer:window", window_revision="revision-1", **changes)


class Bridge:
    """Mutable native-field double that records only accepted text mutations."""

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
    """Queue caller-supplied results, including deliberately uncancelled stale ones."""

    def __init__(self):
        self.identifier = 0
        self.requests = []
        self.events = []
        self.released = False
        self.preparations = []

    def prepare(self, config):
        self.preparations.append(config)

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
    """Own a started controller and temporary encrypted store until teardown."""
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    bridge = Bridge()
    controller = Controller(Config(prompt_style="writing"), bridge)
    controller._capture_interval = 0
    controller._client = Inference()
    controller._storage = LocalStorage(tmp_path / "data.sqlite3", lambda: b"k" * 32)
    controller.start()
    yield controller, bridge, controller._client
    controller.stop()


def submit_command(controller, action):
    """Pump ticks until asynchronous preparation submits, or fail after two seconds."""
    previous = controller._client.identifier
    assert controller.invoke(action)
    deadline = time.monotonic() + 2
    while controller._client.identifier == previous and time.monotonic() < deadline:
        controller.tick()
        time.sleep(0.005)
    assert controller._client.identifier > previous
    return controller._client.identifier


def offer(controller, bridge, text):
    """Deliver a final result without acknowledging that an overlay has drawn it."""
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
    controller.set_screen_reader(lambda snapshot, *, bridge, authorized, max_chars, **sources: calls.append("screen") or "sensitive screen")
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
    def read_screen(snapshot, *, bridge, authorized, max_chars, accessibility, screenshot_context):
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


def test_authored_style_reaches_prompt_only_under_current_recording_consent(runtime):
    from inklow.providers import _prompt
    controller, bridge, client = runtime
    controller.update_config(replace(controller.config, record_history=True, custom_instructions="Use Portuguese.",
        personalization_strength=90, app_rules={
            "app:writer": {"custom_instructions": "Keep a professional tone."},
            "domain:work.example": {"custom_instructions": "Prefer short sentences."}}), persist=False)
    assert controller._storage.record_history("seed", "writer", LONG_SESSION, domain="work.example",
                                              authored_spans=[(0, len(LONG_SESSION))])
    bridge.info = {"app_id": "writer", "name": "Writer", "domain": "work.example", "browser": True, "domain_observable": True}
    bridge.snapshot = replace(bridge.snapshot, domain="work.example", text=LONG_SESSION, caret=len(LONG_SESSION))
    submit_command(controller, "force")
    config, prefix, suffix, options = client.requests[-1]
    evidence = json.loads(options["personalization"])
    assert evidence["language"] == "en" and evidence["style"]["mean_sentence_words"] > 0
    assert "observability" in evidence["vocabulary"]
    assert "observability" not in options["instructions"]
    prompt = _prompt(config, prefix, suffix, options)
    assert all(value in prompt for value in ("Use Portuguese.", "professional tone", "short sentences", "observability"))
    controller.update_config(replace(controller.config, personalization_strength=0), persist=False)
    submit_command(controller, "force")
    assert client.requests[-1][3]["personalization"] == ""
    assert "professional tone" in client.requests[-1][3]["instructions"]
    controller.update_config(replace(controller.config, record_history=False, personalization_strength=90), persist=False)
    submit_command(controller, "force")
    assert client.requests[-1][3]["personalization"] == ""


def type_text(controller, bridge, text):
    """Expose incremental authored edits to history tracking, not generated inserts."""
    for start in range(0, len(text), 8):
        current = bridge.snapshot
        addition = text[start:start + 8]
        bridge.snapshot = replace(current, text=current.prefix + addition + current.suffix,
                                  caret=current.caret + len(addition))
        controller.tick()


def leave_writer(controller, bridge):
    """Trigger the focus transition that flushes the observed writing session."""
    bridge.info = {"app_id": "other", "name": "Other", "domain": ""}
    controller.tick()


def learned_style(controller, text=LONG_SESSION):
    """Read learned evidence without source filtering to isolate authorship rules."""
    return controller._storage.personalization(90, "writer", text=text,
                                              source_allowed=lambda app, domain: True)


def test_generated_acceptance_never_trains_itself_but_later_typing_and_corrections_learn(runtime):
    controller, bridge, _ = runtime
    controller.update_config(replace(controller.config, record_history=True,
                                    typo_suppression=False, typo_fixes=False), persist=False)
    generated = (" The generatedphantom report is ready for the generatedphantom review. "
                 "The generatedphantom review is ready for the team.")
    offer(controller, bridge, generated)
    assert controller.invoke("all")
    controller.tick()
    assert controller.history_info()["count"] == 1
    assert learned_style(controller) == ""
    # The acceptance admits this session under record_unaccepted=False, but only
    # subsequent observed edits provide authorship evidence.
    type_text(controller, bridge, " " + LONG_SESSION)
    for _ in range(2):
        corrected = bridge.snapshot.text.replace("observability", "traceability", 1)
        bridge.snapshot = replace(bridge.snapshot, text=corrected, caret=len(corrected))
        controller.tick()
    leave_writer(controller, bridge)
    evidence = json.loads(learned_style(controller))
    assert "instrumentation" in evidence["vocabulary"]
    assert "traceability" in evidence["vocabulary"]
    assert "generatedphantom" not in evidence["vocabulary"]


def test_unaccepted_authored_typing_requires_the_existing_extra_consent(runtime):
    controller, bridge, _ = runtime
    controller.update_config(replace(controller.config, record_history=True,
                                    typo_suppression=False, typo_fixes=False), persist=False)
    controller.tick()
    type_text(controller, bridge, " " + LONG_SESSION)
    leave_writer(controller, bridge)
    assert controller.history_info()["count"] == 0
    controller.update_config(replace(controller.config, record_unaccepted=True), persist=False)
    bridge.info = {"app_id": "writer", "name": "Writer", "domain": ""}
    bridge.snapshot = field_snapshot("")
    controller.tick()
    type_text(controller, bridge, LONG_SESSION)
    leave_writer(controller, bridge)
    assert "instrumentation" in json.loads(learned_style(controller))["vocabulary"]
    controller.delete_history("writer")
    assert learned_style(controller) == ""


def test_current_source_exclusions_remove_old_history_from_predictions(runtime):
    controller, bridge, client = runtime
    controller.update_config(replace(controller.config, record_history=True), persist=False)
    assert controller._storage.record_history("old", "archived-writer", LONG_SESSION, "work.example",
                                              authored_spans=[(0, len(LONG_SESSION))])
    bridge.info = {"app_id": "writer", "name": "Writer", "domain": "work.example"}
    bridge.snapshot = replace(field_snapshot(LONG_SESSION), domain="work.example")
    submit_command(controller, "force")
    assert "observability" in client.requests[-1][3]["personalization"]
    controller.update_config(replace(controller.config,
        app_rules={"app:archived-writer": {"record_history": False}}), persist=False)
    submit_command(controller, "force")
    assert client.requests[-1][3]["personalization"] == ""
    assert controller.history_info()["count"] == 1
    controller.update_config(replace(controller.config, app_rules={}), persist=False)
    submit_command(controller, "force")
    assert client.requests[-1][3]["personalization"]
    controller.update_config(replace(controller.config, excluded_apps=("archived-writer",)), persist=False)
    submit_command(controller, "force")
    assert client.requests[-1][3]["personalization"] == ""


def test_paused_or_locked_history_never_reaches_prediction_options(runtime):
    controller, bridge, client = runtime
    controller.update_config(replace(controller.config, record_history=True), persist=False)
    controller._storage.record_history("seed", "writer", LONG_SESSION,
                                      authored_spans=[(0, len(LONG_SESSION))])
    bridge.snapshot = field_snapshot(LONG_SESSION)
    controller._storage.lock_history()
    submit_command(controller, "force")
    assert client.requests[-1][3]["personalization"] == ""
    controller.pause(1)
    requests = len(client.requests)
    assert not controller.invoke("force")
    controller.tick()
    assert len(client.requests) == requests


def test_deleting_history_discards_already_prepared_suggestions_and_model_context(runtime):
    controller, bridge, client = runtime
    controller.update_config(replace(controller.config, record_history=True), persist=False)
    controller._storage.record_history("seed", "writer", LONG_SESSION,
                                      authored_spans=[(0, len(LONG_SESSION))])
    bridge.snapshot = field_snapshot(LONG_SESSION)
    offer(controller, bridge, " The review can proceed.")
    before = bridge.snapshot.text
    controller.delete_history()
    assert client.released
    assert not controller.invoke("all")
    assert bridge.snapshot.text == before
    submit_command(controller, "force")
    assert client.requests[-1][3]["personalization"] == ""


def test_stop_attempts_all_resources_after_failures_and_retries_only_incomplete_steps(runtime, monkeypatch):
    controller, bridge, client = runtime
    calls = []

    def bridge_stop():
        calls.append("bridge")
        if calls.count("bridge") == 1:
            raise RuntimeError("Synthetic private native details")

    class Spelling:
        def close(self):
            calls.append("spelling")
            if calls.count("spelling") == 1:
                raise RuntimeError("Synthetic private spelling details")

    controller._spelling = Spelling()
    monkeypatch.setattr(bridge, "stop", bridge_stop)
    monkeypatch.setattr(client, "close", lambda: calls.append("client"))
    original_lock = controller._storage.lock_history
    monkeypatch.setattr(controller._storage, "lock_history",
                        lambda: (calls.append("history"), original_lock()))
    with pytest.raises(RuntimeError, match="Shutdown is incomplete") as failure:
        controller.stop()
    assert "private" not in str(failure.value)
    assert {"bridge", "spelling", "client", "history"} <= set(calls)
    controller.stop()
    controller.stop()
    assert calls.count("bridge") == calls.count("spelling") == 2
    assert calls.count("client") == calls.count("history") == 1


def test_quit_allows_pending_native_key_callback_to_finish_before_join(runtime, monkeypatch):
    controller, bridge, client = runtime
    completed = threading.Event()
    results, threads = [], []

    def dispatch():
        results.append(controller.invoke("word"))
        completed.set()

    def stop_native_hook():
        thread = threading.Thread(target=dispatch)
        threads.append(thread)
        thread.start()
        if not completed.wait(2):
            raise RuntimeError("Pending native callback could not finish")
        thread.join(timeout=2)

    monkeypatch.setattr(bridge, "stop", stop_native_hook)
    try:
        controller.stop()
    finally:
        for thread in threads:
            thread.join(timeout=2)
    assert results == [False]
    assert bridge.insertions == [] and client.requests == []


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


def test_prediction_refreshes_changed_window_reference_before_model_submission(runtime):
    controller, bridge, client = runtime
    controller.config = replace(controller.config, screen_context=True, personalization_strength=0)
    previous = field_snapshot("Thank you for helping with the launch review")
    bridge.snapshot = field_snapshot("The launch review is scheduled for")
    controller._screen_cache = (
        controller._window_key(previous), time.monotonic(), "accessibility",
        "The launch review is scheduled for Thursday at 14:30 in Room Cedar.", "ready")
    started, release = threading.Event(), threading.Event()

    def read_current(snapshot, **options):
        started.set()
        assert release.wait(2)
        return {"text": "The launch review is scheduled for Friday at 09:45 in Room Maple.",
                "provenance": "accessibility", "status": "ready",
                "window_id": snapshot.window_id, "revision": snapshot.window_revision}

    controller.set_screen_reader(read_current)
    try:
        assert controller.force()
        controller.tick()
        assert started.wait(1), "A new prediction reused the older window reference"
        release.set()
        deadline = time.monotonic() + 2
        while not client.requests and time.monotonic() < deadline:
            controller.tick()
            time.sleep(0.005)
        context = client.requests[-1][3]["context"]
        assert "Friday at 09:45 in Room Maple" in context
        assert "Thursday" not in context
    finally:
        release.set()


def test_superseded_optional_context_wait_does_not_hold_new_prediction(runtime, monkeypatch):
    controller, bridge, client = runtime
    controller.config = replace(controller.config, screen_context=True, personalization_strength=0)
    waiting, release_reader, ready = threading.Event(), threading.Event(), threading.Event()
    original_wait = threading.Condition.wait

    def hold_context_budget(condition, timeout=None):
        if threading.current_thread() is controller._context_thread and timeout is not None:
            waiting.set()
            # Expose cancellation rather than let the normal finite budget mask it.
            return original_wait(condition, 5)
        return original_wait(condition, timeout)

    def read_stalled(snapshot, **options):
        release_reader.wait(10)
        return {"text": "Obsolete reference.", "provenance": "accessibility", "status": "ready",
                "window_id": snapshot.window_id, "revision": snapshot.window_revision}

    monkeypatch.setattr(threading.Condition, "wait", hold_context_budget)
    controller.set_screen_reader(read_stalled)
    controller.set_wake_callback(ready.set)
    try:
        assert controller.force()
        controller.tick()
        assert waiting.wait(1)
        controller.update_config(replace(controller.config, screen_context=False), persist=False)
        bridge.snapshot = field_snapshot("A different current draft")
        ready.clear()
        assert controller.force()
        controller.tick()
        assert ready.wait(2), "The obsolete context budget blocked the current prediction"
        controller.tick()
        assert len(client.requests) == 1
        assert client.requests[0][1] == bridge.snapshot.text
        assert "Obsolete reference" not in client.requests[0][3]["context"]
    finally:
        release_reader.set()


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
    controller._request = (1, snapshot, controller._suggestion.generation, "completion", "medium")
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


@pytest.mark.parametrize("prefix,continuation", [("Então", " vamos"), ("猫", "が")])
def test_first_short_input_can_offer_and_accept_without_more_typing(runtime, monkeypatch, prefix, continuation):
    controller, bridge, client = runtime
    clock = [100.0]
    monkeypatch.setattr("inklow.runtime.time.monotonic", lambda: clock[0])
    ready = threading.Event()
    controller.set_wake_callback(ready.set)
    bridge.snapshot = field_snapshot(prefix)
    controller.tick()
    clock[0] += controller.config.debounce_ms / 1000 + 0.001
    controller.tick()
    assert ready.wait(1), "A completed pause after the short first input must schedule prediction"
    controller.tick()
    client.events.append({"type": "result", "id": client.identifier,
                          "text": continuation, "final": True})
    controller.tick()
    assert any(event["type"] == "suggestion" for event in controller.drain_events())
    assert bridge.snapshot.text == prefix
    assert controller.invoke("all")
    assert bridge.snapshot.text == prefix + continuation


def test_explicit_longer_input_threshold_still_defers_short_first_word(runtime, monkeypatch):
    controller, bridge, client = runtime
    controller.config = replace(controller.config, min_chars=12)
    clock = [100.0]
    monkeypatch.setattr("inklow.runtime.time.monotonic", lambda: clock[0])
    ready = threading.Event()
    controller.set_wake_callback(ready.set)
    bridge.snapshot = field_snapshot("Hello")
    controller.tick()
    clock[0] += 1
    controller.tick()
    assert not ready.is_set()
    assert not client.requests
    assert not any(event["type"] == "suggestion" for event in controller.drain_events())


def wait_request(controller, previous):
    """Pump ticks for up to two seconds; callers assert whether submission advanced."""
    deadline = time.monotonic() + 2
    while controller._client.identifier == previous and time.monotonic() < deadline:
        controller.tick()
        time.sleep(0.005)
    return controller._client.identifier


def test_escape_pause_is_brief_field_scoped_and_keeps_the_worker_warm(runtime):
    controller, bridge, client = runtime
    controller.update_config(replace(controller.config, escape_behavior="pause", escape_pause_seconds=30), persist=False)
    offer(controller, bridge, " arrives tomorrow")
    assert controller.invoke("escape")
    assert not client.released and controller.status()["enabled"]
    assert not controller._suggestion.text
    assert not controller.invoke("escape")  # Nothing is shown: the host receives it.
    requests = len(client.requests)
    bridge.snapshot = field_snapshot(bridge.snapshot.text + " and")
    controller.tick()
    controller._candidate_since -= 5
    controller.tick()
    assert len(client.requests) == requests
    bridge.snapshot = replace(bridge.snapshot, field_id="other")
    previous = client.identifier
    controller.tick()
    controller._candidate_since -= 5
    assert wait_request(controller, previous) > previous
    assert not client.released


def test_escape_over_inflight_prediction_passes_through_and_drops_its_late_result(runtime):
    controller, bridge, client = runtime
    controller.tick()
    identifier = submit_command(controller, "force")
    assert not controller.invoke("escape")
    client.events.append({"type": "result", "id": identifier, "text": " arrives tomorrow", "final": True})
    controller.tick()
    assert not controller._suggestion.text
    assert not any(event["type"] == "suggestion" for event in controller.drain_events())


def test_escape_app_mode_lasts_until_an_app_switch_without_saving_a_rule(runtime):
    controller, bridge, client = runtime
    controller.update_config(replace(controller.config, escape_behavior="app"), persist=False)
    offer(controller, bridge, " arrives tomorrow")
    assert controller.invoke("escape")
    status = controller.status()
    assert status["escape_suppressed"] and not status["current_app_enabled"]
    captures = bridge.captures
    controller.tick()
    assert bridge.captures == captures and not client.released
    bridge.info = {"app_id": "other", "name": "Other", "domain": ""}
    controller.tick()
    bridge.info = {"app_id": "writer", "name": "Writer", "domain": ""}
    controller.tick()
    assert controller.status()["current_app_enabled"]
    assert controller.config.app_rules == {}


def test_app_toggle_expires_after_a_few_minutes_and_scopes_websites(runtime, monkeypatch):
    controller, bridge, client = runtime
    clock = [100.0]
    monkeypatch.setattr("inklow.runtime.time.monotonic", lambda: clock[0])
    controller.tick()
    assert controller.toggle_current_app()
    status = controller.status()
    assert not status["current_app_enabled"]
    assert status["temporary_scope"] == "app:writer" and status["temporary_remaining"] == 300
    assert not client.released and controller.config.app_rules == {}
    clock[0] += 301
    controller.tick()
    assert controller.status()["current_app_enabled"]
    assert controller.toggle_current_app(app="browser", domain="Work.Example.")
    assert not controller._allowed("browser", "work.example")
    assert controller._allowed("browser", "other.example")


class FastBridge(Bridge):
    """Native key path with drawn-presentation acknowledgment (macOS contract)."""

    def __init__(self, text="Dear team, the update"):
        super().__init__(text)
        self.revision = 0
        self.replayed = []

    def user_input_revision(self):
        return self.revision

    def set_input_callback(self, callback):
        self.input_callback = callback

    def replay_action(self, action):
        self.replayed.append(action)
        return True


@pytest.fixture
def fast_runtime(tmp_path, monkeypatch):
    """Own the deferred-key variant with isolated storage and teardown."""
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    bridge = FastBridge()
    controller = Controller(Config(prompt_style="writing"), bridge)
    controller._capture_interval = 0
    controller._client = Inference()
    controller._storage = LocalStorage(tmp_path / "data.sqlite3", lambda: b"k" * 32)
    controller.start()
    yield controller, bridge, controller._client
    controller.stop()


def draw(controller, visible=None):
    """Acknowledge the latest suggestion, optionally limiting its visible prefix."""
    event = [event for event in controller.drain_events() if event["type"] == "suggestion"][-1]
    controller.presentation_result(event["key"], event["text"] if visible is None else visible, event["mode"])
    return event


def test_fast_keys_insert_only_text_the_overlay_confirmed_it_drew(fast_runtime):
    controller, bridge, _ = fast_runtime
    offer(controller, bridge, " arrives tomorrow morning")
    assert not controller.fast_invoke("word")  # Computed, not yet drawn.
    assert not controller.fast_invoke("escape")
    draw(controller, "")  # The draw failed (e.g. off-screen).
    assert not controller.fast_invoke("all")
    controller.tick()
    assert bridge.insertions == [] and bridge.replayed == []
    controller.presentation_result(controller._suggestion.snapshot.key, " arrives tomorrow", "completion")
    assert controller.fast_invoke("all")
    controller.tick()
    assert [text.strip() for text in bridge.insertions] == ["arrives tomorrow"]
    assert "morning" not in bridge.snapshot.text


def test_fast_word_with_only_an_interior_prefix_never_inserts_unseen_tail(fast_runtime):
    controller, bridge, _ = fast_runtime
    offer(controller, bridge, " arrive tomorrow")
    draw(controller, " arr")
    original = bridge.snapshot

    claimed = controller.fast_invoke("word")
    controller.tick()
    controller.tick()  # A claimed but rejected key must not be replayed twice.

    assert bridge.insertions == []
    assert bridge.snapshot == original
    assert bridge.replayed == (["word"] if claimed else [])


@pytest.mark.parametrize("action,visible,expected", [
    ("word", " arrive", " arrive "),
    ("all", " arrive", " arrive "),
    ("all", " arrive tom", " arrive tom"),
])
def test_fast_acceptance_inserts_only_visible_text_plus_allowed_separator(
        fast_runtime, action, visible, expected):
    controller, bridge, _ = fast_runtime
    offer(controller, bridge, " arrive tomorrow")
    draw(controller, visible)
    original = bridge.snapshot

    assert controller.fast_invoke(action)
    controller.tick()

    assert bridge.insertions == [expected]
    assert bridge.snapshot.text == original.text + expected
    assert bridge.replayed == []


def test_fast_key_after_user_input_passes_and_stale_acceptance_replays_once(fast_runtime):
    controller, bridge, _ = fast_runtime
    offer(controller, bridge, " arrives tomorrow")
    draw(controller)
    bridge.revision += 1  # A click in the field: no text change.
    assert not controller.fast_invoke("word")
    controller.tick()
    draw(controller)
    original = bridge.snapshot
    bridge.snapshot = replace(original, text=original.text + "!", caret=original.caret + 1)  # Host autocorrect.
    assert controller.fast_invoke("word")
    controller.tick()
    assert bridge.insertions == []
    assert bridge.replayed == ["word"]


def test_fast_escape_dismisses_drawn_text_keeps_worker_and_second_escape_passes(fast_runtime):
    controller, bridge, client = fast_runtime
    offer(controller, bridge, " arrives tomorrow")
    draw(controller)
    assert controller.fast_invoke("escape")
    controller.tick()
    assert not controller._suggestion.text and not client.released
    assert not controller.fast_invoke("escape")
    assert bridge.replayed == []


@pytest.mark.parametrize("action", ["escape", "dismiss"])
def test_fast_passthrough_escape_dismisses_even_after_tap_discards_gate(fast_runtime, action):
    controller, bridge, client = fast_runtime
    controller.update_config(replace(controller.config, escape_behavior="passthrough"), persist=False)
    offer(controller, bridge, " arrives tomorrow")
    draw(controller)
    gate = object()
    bridge.input_transaction = lambda: gate
    begun = []
    bridge.begin_input_transaction = lambda token: begun.append(token) or token is None
    assert not controller.fast_invoke(action)  # Original Escape reaches the host.
    controller.tick()
    assert begun == [None]
    assert not controller._suggestion.text and not client.released
    assert bridge.insertions == [] and bridge.replayed == []


TYPO_TEXT = "Dear team, we will recieve"


class Spelling:
    """Deterministic typo assessment independent of native spelling services."""

    def __init__(self):
        self.suspected = True
        self.closed = False

    def assess(self, snapshot):
        from types import SimpleNamespace
        return SimpleNamespace(key=snapshot.key, start=snapshot.text.rfind("recieve"), length=7, word="recieve",
                               replacement="receive", suspected=self.suspected, language="en",
                               reason="typo" if self.suspected else "ok", finished=False)

    def close(self):
        self.closed = True


def test_suspected_typo_withholds_completion_until_the_word_is_correct(runtime):
    controller, bridge, client = runtime
    controller.update_config(replace(controller.config, typo_suppression=True), persist=False)
    controller._spelling = spelling = Spelling()
    bridge.snapshot = field_snapshot(TYPO_TEXT)
    controller.tick()
    controller._candidate_since -= 5
    controller.tick()
    assert not client.requests
    spelling.suspected = False
    assert wait_request(controller, 0) == 1
    controller.update_config(replace(controller.config, typo_suppression=False), persist=False)
    assert spelling.closed


def test_suggested_fix_is_drawn_on_the_same_line_and_tab_applies_it_atomically(runtime):
    controller, bridge, client = runtime
    controller.update_config(replace(controller.config, typo_fixes=True), persist=False)
    controller._spelling = Spelling()
    bridge.range_rect = lambda snapshot, start, length: (60, 20, 40, 18)
    bridge.snapshot = field_snapshot(TYPO_TEXT)
    controller.tick()
    event = next(event for event in controller.drain_events() if event["type"] == "suggestion")
    assert event["mode"] == "correction" and event["text"] == "receive"
    assert event["correction"]["word"] == "recieve" and event["correction"]["rect"] == [60, 20, 40, 18]
    assert controller.invoke("word")
    assert bridge.snapshot.text == "Dear team, we will receive"
    assert not client.requests


@pytest.mark.parametrize("can_replace", [True, False])
def test_delayed_alternatives_keep_the_completion_and_tab_never_chooses_them(runtime, can_replace):
    controller, bridge, client = runtime
    controller.update_config(replace(controller.config, alternatives_enabled=True, alternatives_mode="pause"), persist=False)
    bridge.snapshot = replace(bridge.snapshot, can_replace=can_replace)
    offer(controller, bridge, " arrives tomorrow")
    previous = client.identifier
    controller._candidate_since -= 5
    assert wait_request(controller, previous) > previous
    client.events.append({"type": "result", "id": client.identifier,
                          "text": '[" is ready", " ships today"]', "final": True})
    controller.tick()
    event = [event for event in controller.drain_events() if event["type"] == "suggestion"][-1]
    assert event["mode"] == "completion" and event["text"] == " arrives tomorrow"
    assert event["choices"] == [" is ready", " ships today"]
    assert controller.invoke("word")
    assert bridge.insertions == [" arrives "]
    assert not controller._choices


def test_current_word_only_finishes_the_word_and_never_starts_the_next(runtime):
    controller, bridge, client = runtime
    controller.update_config(replace(controller.config, completion_length="word"), persist=False)
    bridge.snapshot = field_snapshot("Dear team, the upd")
    offer(controller, bridge, "ate arrives tomorrow")
    assert controller._suggestion.text == "ate"
    bridge.snapshot = field_snapshot("Dear team, the update ")
    assert not controller.force()


def test_battery_is_one_step_shorter_and_uses_only_an_installed_battery_model(runtime, monkeypatch):
    from inklow.features import completion_limits
    from inklow.models import get_preset, model_path
    controller, bridge, client = runtime
    controller.update_config(replace(controller.config, completion_length="long", battery_shorter=True,
                                     battery_model="tiny"), persist=False)
    controller.set_battery_reader(lambda: True)
    monkeypatch.setattr("inklow.models.is_installed", lambda preset: False)
    controller.tick()
    submit_command(controller, "force")
    config, _, _, options = client.requests[-1]
    assert options["max_words"] == completion_limits("medium")[1]
    assert config.model_path == controller.config.model_path
    assert "battery model" in controller.status()["message"]
    monkeypatch.setattr("inklow.models.is_installed", lambda preset: True)
    submit_command(controller, "force")
    assert client.requests[-1][0].model_path == str(model_path(get_preset("tiny")))


def test_screenshot_measured_appearance_needs_its_own_opt_in(runtime):
    controller, bridge, _ = runtime
    bridge.snapshot = replace(bridge.snapshot, inline_render=replace(RENDER, source="macos-ax-geometry+vision"))
    controller.tick()
    assert not controller.force()
    controller.update_config(replace(controller.config, screenshot_appearance=True), persist=False)
    controller.tick()
    assert controller.force()


def test_window_context_sources_follow_their_separate_consents(runtime):
    controller, bridge, _ = runtime
    snapshot = bridge.snapshot
    controller._candidate = snapshot
    seen = []

    def reader(snapshot, *, accessibility, screenshot_context, **kwargs):
        seen.append((accessibility, screenshot_context))
        return {"text": "The update arrives Friday.", "provenance": "screenshot", "status": "ready",
                "window_id": snapshot.window_id, "revision": snapshot.window_revision}

    controller.set_screen_reader(reader)
    for screen, screenshots in ((True, False), (False, True)):
        controller.config = replace(controller.config, screen_context=screen, screenshot_context=screenshots)
        token = (object(), snapshot.key, controller._window_key(snapshot))
        controller._screen_job = token
        controller._read_window_context(token, snapshot)
        assert ("Friday" in controller._reference_options(snapshot, [])) is screenshots
    assert seen == [(True, False), (False, True)]


def test_website_history_is_recorded_counted_and_deleted_per_domain(runtime):
    controller, bridge, _ = runtime
    controller.update_config(replace(controller.config, record_history=True, record_unaccepted=True), persist=False)
    bridge.info = {"app_id": "browser", "name": "Browser", "domain": "work.example", "browser": True, "domain_observable": True}
    bridge.snapshot = replace(bridge.snapshot, app_id="browser", domain="work.example",
                              text=LONG_SESSION, caret=len(LONG_SESSION))
    controller.tick()
    bridge.info = {"app_id": "other", "name": "Other", "domain": ""}
    controller.tick()
    counts = controller.history_counts()
    assert counts["available"] and counts["domains"] == {"work.example": 1}
    controller.delete_history(domain="work.example")
    assert controller.history_counts()["domains"] == {}


@pytest.mark.parametrize("tool", ["synonyms", "emoji", "correction"])
def test_escape_field_pause_suppresses_every_automatic_tool(runtime, tool):
    controller, bridge, client = runtime
    controller.update_config(replace(
        controller.config, escape_behavior="pause", synonyms_enabled=True, synonyms_mode="selection",
        emoji_enabled=True, typo_fixes=True, typo_suppression=False), persist=False)
    controller._spelling = Spelling()
    bridge.range_rect = lambda snapshot, start, length: (60, 20, 40, 18)
    if tool == "synonyms":
        bridge.snapshot = field_snapshot("kind", caret=0, selection_length=4)
        controller.tick()
        controller._publish_choices(bridge.snapshot, ["friendly"], "synonyms", (0, 4))
    elif tool == "emoji":
        bridge.snapshot = field_snapshot("Hello :-)")
        controller.tick()
        controller._publish_choices(bridge.snapshot, ["🙂"], "emoji", (6, 3))
    else:
        bridge.snapshot = field_snapshot(TYPO_TEXT)
        controller.tick()
    assert controller.invoke("escape")
    controller.drain_events()
    controller._candidate_since -= 5
    for _ in range(3):
        controller.tick()
    assert not client.requests
    assert not [event for event in controller.drain_events() if event["type"] == "suggestion"]
    assert bridge.insertions == []


def test_failed_correction_geometry_does_not_cancel_or_starve_completion(runtime):
    controller, bridge, client = runtime
    controller.update_config(replace(controller.config, typo_fixes=True, typo_suppression=False), persist=False)
    controller._spelling = Spelling()
    geometry_reads = []
    bridge.range_rect = lambda *args: geometry_reads.append(args) or None
    bridge.snapshot = field_snapshot(TYPO_TEXT)
    controller.tick()
    request = wait_request(controller, 0)
    for _ in range(3):
        controller.tick()
    client.events.append({"type": "result", "id": request, "text": " the update", "final": True})
    controller.tick()
    assert len(geometry_reads) == 1
    assert controller._suggestion.text == " the update"
    assert controller.invoke("word")
    assert bridge.insertions == [" the "]


@pytest.mark.parametrize("action", ["force", "synonyms", "app_toggle"])
@pytest.mark.parametrize("change", ["field", "app", "revision"])
def test_fast_focus_sensitive_commands_cannot_cross_the_origin(fast_runtime, action, change):
    controller, bridge, client = fast_runtime
    controller.update_config(replace(controller.config, synonyms_enabled=True), persist=False)
    controller.tick()
    assert controller.fast_invoke(action)
    if change == "field":
        bridge.snapshot = replace(bridge.snapshot, field_id="another-field")
    elif change == "app":
        bridge.info = {"app_id": "other", "name": "Other", "domain": ""}
        bridge.snapshot = replace(bridge.snapshot, app_id="other")
    else:
        bridge.revision += 1
    controller.tick()
    assert not client.requests and controller._temporary_apps == {}
    assert bridge.replayed == [action]


def test_chooser_acknowledgment_never_arms_a_hidden_long_tail(fast_runtime):
    controller, bridge, _ = fast_runtime
    bridge.snapshot = field_snapshot("kind", caret=0, selection_length=4)
    controller.tick()
    controller._publish_choices(bridge.snapshot, ["short", "a very long invisible tail"], "synonyms", (0, 4))
    controller.presentation_result(bridge.snapshot.key, "short\na very long…", "synonyms")
    assert not controller.fast_invoke("choice:2")
    assert controller.fast_invoke("choice:1")
    controller.tick()
    assert bridge.insertions == ["short"]


@pytest.mark.parametrize("acknowledgment", ["unseen", "empty", "revoked", "truncated"])
def test_chooser_with_no_fully_displayed_row_does_not_steal_tab_or_escape(
        fast_runtime, acknowledgment):
    controller, bridge, _ = fast_runtime
    bridge.snapshot = field_snapshot("kind", caret=0, selection_length=4)
    controller.tick()
    choice = "a very long invisible tail"
    controller._publish_choices(bridge.snapshot, [choice], "synonyms", (0, 4))
    original = bridge.snapshot
    if acknowledgment == "revoked":
        controller.presentation_result(original.key, choice, "synonyms")
    if acknowledgment in {"empty", "revoked"}:
        controller.presentation_result(original.key, "", "synonyms")
    elif acknowledgment == "truncated":
        controller.presentation_result(original.key, "a very long…", "synonyms")

    assert not controller.fast_invoke("word")
    assert not controller.fast_invoke("all")
    assert not controller.fast_invoke("choice:1")
    assert not controller.fast_invoke("escape")
    controller.tick()
    assert bridge.insertions == [] and bridge.replayed == []
    assert bridge.snapshot == original

    controller.presentation_result(original.key, choice, "synonyms")
    assert controller.fast_invoke("word")
    controller.tick()
    assert bridge.insertions == [choice]
    assert bridge.snapshot.text == choice
    assert bridge.replayed == []


@pytest.fixture
def docs_field(fast_runtime):
    """Expose Docs metadata without editable text; fast_runtime owns teardown."""
    controller, bridge, client = fast_runtime
    bridge.snapshot = None
    bridge.info = {"app_id": "browser", "name": "Browser", "domain": "docs.google.com",
                   "browser": True, "domain_observable": True, "pid": 42,
                   "window_id": "opaque-window", "document_id": "opaque-document"}
    hint = {"token": "document-capability", "app_id": "browser", "pid": 42,
            "window_id": "opaque-window", "document_id": "opaque-document",
            "anchor": (100, 200, 600, 400), "support": "unknown",
            "help_url": "https://support.google.com/docs/answer/6282736"}
    reads, enables = [], []
    def read_hint():
        reads.append("hint")
        if bridge.info.get("app_id") == hint["app_id"] and bridge.info.get("domain") == "docs.google.com":
            return dict(hint)
        return None
    bridge.docs_hint = read_hint
    bridge.input_revision = lambda: bridge.revision
    bridge.secure_input_active = lambda: False
    bridge.enable_docs_accessibility = lambda token: enables.append(token) or {
        "status": "unknown", "diagnostic": "unconfirmed", "help_url": hint["help_url"]}
    return controller, bridge, client, hint, reads, enables


def test_docs_help_is_metadata_only_deduplicated_and_never_arms_tab(docs_field):
    controller, bridge, client, hint, _, enables = docs_field
    controller.tick()
    controller.tick()
    events = controller.drain_events()
    assert [event for event in events if event["type"] == "docs_help"] == [
        {"type": "docs_help", "hint": hint}]
    assert not any(event["type"] in {"caret", "suggestion"} for event in events)
    controller.presentation_result(hint["token"], "Enable accessibility", "docs_help")
    assert not controller.fast_invoke("word")
    assert not controller.fast_invoke("all")
    assert not controller.invoke("word")
    assert not client.requests and not bridge.insertions and not enables
    assert controller.invoke("plain_tab")
    assert bridge.replayed == ["plain_tab"]


@pytest.mark.parametrize("loss", ["focus", "domain", "policy", "pause", "disabled", "native", "security"])
def test_docs_capability_is_revoked_on_focus_privacy_pause_or_native_loss(docs_field, loss):
    controller, bridge, _, hint, reads, enables = docs_field
    controller.tick()
    controller.drain_events()
    if loss == "focus":
        bridge.info = {"app_id": "other", "name": "Other", "domain": ""}
    elif loss == "domain":
        bridge.info = {**bridge.info, "domain": "private.example"}
    elif loss == "policy":
        controller.update_config(replace(controller.config, app_rules={
            "domain:docs.google.com": {"enabled": False}}), persist=False)
    elif loss == "pause":
        controller.pause(10)
    elif loss == "disabled":
        controller.update_config(replace(controller.config, global_enabled=False), persist=False)
    else:
        bridge.revision += 1  # Native focus/availability changes invalidate capabilities.
        bridge.docs_hint = lambda: None
    if loss == "security":
        bridge.secure_input_active = lambda: True
    controller.tick()
    assert {"type": "docs_help", "hint": None} in controller.drain_events()
    assert controller.enable_docs_accessibility(hint["token"])["status"] == "unknown"
    assert not enables
    if loss in {"policy", "pause", "disabled"}:
        assert reads == ["hint"]


@pytest.mark.parametrize("loss", ["pause", "focus", "domain", "disabled"])
def test_docs_enable_rechecks_policy_even_before_next_capture(docs_field, loss):
    controller, bridge, _, hint, _, enables = docs_field
    controller.tick()
    if loss == "pause":
        controller.pause(10)
    elif loss == "focus":
        bridge.info = {"app_id": "other"}
    elif loss == "domain":
        bridge.info = {**bridge.info, "domain": "private.example"}
    else:
        controller.update_config(replace(controller.config, global_enabled=False), persist=False)
    assert controller.enable_docs_accessibility(hint["token"])["status"] == "unknown"
    assert enables == []


def test_docs_enable_is_explicit_single_use_and_unknown_is_not_success(docs_field, monkeypatch):
    controller, bridge, _, hint, _, enables = docs_field
    controller.tick()
    assert enables == []
    assert controller.enable_docs_accessibility("stale-token")["status"] == "unknown"
    assert enables == []
    now = controller._docs_probe_after + 0.01
    monkeypatch.setattr("inklow.runtime.time.monotonic", lambda: now)
    controller.tick()
    result = controller.enable_docs_accessibility(hint["token"])
    assert result["status"] == "unknown"
    assert enables == [hint["token"]]
    assert not controller.fast_invoke("word")
    assert not bridge.insertions
    assert controller.enable_docs_accessibility(hint["token"])["status"] == "unknown"
    assert enables == [hint["token"]]


def test_docs_hint_disappears_when_real_capture_returns_without_arming_computed_text(docs_field):
    controller, bridge, client, _, _, _ = docs_field
    controller.tick()
    controller.drain_events()
    bridge.snapshot = replace(field_snapshot("Dear team, the update"), app_id="browser",
                              domain="docs.google.com")
    controller.tick()
    assert {"type": "docs_help", "hint": None} in controller.drain_events()
    offer(controller, bridge, " arrives tomorrow")
    assert not controller.fast_invoke("word")
    draw(controller)
    assert controller.fast_invoke("word")
    controller.tick()
    assert bridge.insertions == [" arrives "]


@pytest.mark.parametrize("positive", [False, True])
def test_docs_probes_are_bounded_and_typing_does_not_throttle_capture(docs_field, monkeypatch, positive):
    controller, bridge, _, _, reads, _ = docs_field
    now = [100.0]
    monkeypatch.setattr("inklow.runtime.time.monotonic", lambda: now[0])
    if not positive:
        bridge.docs_hint = lambda: reads.append("hint")
    controller.tick()
    captures = bridge.captures
    for index in range(1, 20):
        now[0] = 100.0 + index / 10
        bridge.revision += 1
        controller.tick()
    assert reads == ["hint"]
    assert bridge.captures >= captures + 19
    assert controller._docs_help is None  # First input revoked the shown capability.
    now[0] = 102.11
    controller.tick()
    assert reads == ["hint", "hint"]


@pytest.mark.parametrize("change", ["revision", "window_id", "document_id", "pid"])
def test_docs_identity_loss_revokes_action_before_next_capture(docs_field, change):
    controller, bridge, _, hint, reads, enables = docs_field
    controller.tick()
    if change == "revision":
        bridge.revision += 1
    else:
        bridge.info = {**bridge.info, change: "different"}
    assert controller.enable_docs_accessibility(hint["token"])["status"] == "unknown"
    assert enables == [] and reads == ["hint"]
    assert controller._docs_help is None


def test_docs_input_during_discovery_does_not_publish_stale_capability(docs_field):
    controller, bridge, _, hint, _, enables = docs_field
    def discover_after_focus_change():
        bridge.revision += 1
        return dict(hint)
    bridge.docs_hint = discover_after_focus_change
    controller.tick()
    assert controller._docs_help is None
    assert not any(event.get("hint") for event in controller.drain_events()
                   if event["type"] == "docs_help")
    assert controller.enable_docs_accessibility(hint["token"])["status"] == "unknown"
    assert not enables


@pytest.mark.parametrize("backend,style,on_battery,installed,expected", [
    ("gguf", "continuation", False, False, False),
    ("gguf", "writing", True, True, False),
    ("gguf", "writing", True, False, True),
    ("gguf", "writing", False, True, True),
    ("ollama", "continuation", True, True, True),
    ("openai", "continuation", False, False, True),
])
def test_synonyms_use_effective_model_capability_without_retrying_selection(
        runtime, monkeypatch, backend, style, on_battery, installed, expected):
    controller, bridge, client = runtime
    controller.update_config(replace(controller.config, backend=backend, prompt_style=style,
                                     battery_model="tiny", synonyms_mode="selection",
                                     synonyms_delay_ms=100), persist=False)
    controller._on_battery = on_battery
    monkeypatch.setattr("inklow.models.is_installed", lambda preset: installed)
    bridge.snapshot = field_snapshot("A useful update", caret=2, selection_length=6)
    controller.tick()
    controller._candidate_since -= 5
    for _ in range(3):
        controller.tick()
    if expected:
        if controller._preparing is not None:
            wait_request(controller, 0)
        assert client.requests and client.requests[0][3].get("utility")
    else:
        assert not controller.invoke("synonyms")
        assert not controller._submit(bridge.snapshot, "synonyms")
        assert not controller._preparing and not client.requests
        assert controller.config.prompt_style == style


def test_synonyms_recheck_battery_model_at_actual_dispatch(runtime, monkeypatch):
    controller, bridge, client = runtime
    controller.update_config(replace(controller.config, battery_model="tiny"), persist=False)
    monkeypatch.setattr("inklow.models.is_installed", lambda preset: True)
    bridge.snapshot = field_snapshot("A useful update", caret=2, selection_length=6)
    controller.tick()
    token = (bridge.snapshot.key, controller._suggestion.generation, "synonyms")
    controller._preparing = token
    controller._prepared.append((token, bridge.snapshot, {"utility": "guarded selected-word request"}))
    controller._on_battery = True  # Changed after optional context was prepared.
    controller._consume_prepared()
    assert not client.requests and controller._preparing is None


@pytest.mark.parametrize("tool", ["completion", "synonyms", "emoji", "correction"])
def test_on_demand_fields_never_schedule_automatic_tools(runtime, tool):
    controller, bridge, client = runtime
    controller.update_config(replace(
        controller.config, synonyms_mode="selection", typo_fixes=True,
        alternatives_enabled=True, alternatives_mode="pause"), persist=False)
    controller._spelling = Spelling()
    range_reads = []
    bridge.range_rect = lambda *args: range_reads.append(args) or (60, 20, 40, 18)
    if tool == "synonyms":
        snapshot = field_snapshot("A useful update", caret=2, selection_length=6)
    elif tool == "emoji":
        snapshot = field_snapshot("Hello :smile")
    elif tool == "correction":
        snapshot = field_snapshot(TYPO_TEXT)
    else:
        snapshot = bridge.snapshot
    bridge.snapshot = replace(snapshot, on_demand_only=True)
    controller.tick()
    controller._candidate_since -= 10
    controller.tick()
    assert not client.requests
    assert not range_reads
    assert not [event for event in controller.drain_events() if event["type"] == "suggestion"]
    assert not bridge.insertions


def test_on_demand_append_only_force_requires_drawn_text_and_does_not_schedule_alternatives(fast_runtime):
    controller, bridge, client = fast_runtime
    controller.update_config(replace(
        controller.config, alternatives_enabled=True, alternatives_mode="pause"), persist=False)
    bridge.snapshot = replace(bridge.snapshot, on_demand_only=True, can_replace=False)
    offer(controller, bridge, " arrives tomorrow")
    assert not controller.fast_invoke("word")
    controller._candidate_since -= 10
    controller.tick()
    assert len(client.requests) == 1
    draw(controller)
    assert controller.fast_invoke("word")
    controller.tick()
    assert bridge.insertions == [" arrives "]
    assert len(client.requests) == 1
    assert not bridge.replayed


@pytest.mark.parametrize("changes", [
    {"text": "", "caret": 0},
    {"caret": -1},
    {"selection_length": 1},
    {"protected": True},
    {"inline_render": None},
    {"caret_rect": None, "field_rect": None},
])
def test_on_demand_force_retains_input_privacy_and_geometry_guards(runtime, changes):
    controller, bridge, client = runtime
    bridge.snapshot = replace(bridge.snapshot, on_demand_only=True, can_replace=False, **changes)
    assert not controller.force()
    controller.tick()
    assert not client.requests
    assert not bridge.insertions


@pytest.mark.parametrize("capability", ["on_demand_only", "can_replace"])
@pytest.mark.parametrize("enabled", [False, True])
def test_capability_transition_drops_inflight_results_but_allows_fresh_force(runtime, capability, enabled):
    controller, bridge, client = runtime
    controller.update_config(replace(controller.config, min_chars=1000), persist=False)
    bridge.snapshot = replace(bridge.snapshot, **{capability: enabled})
    identifier = submit_command(controller, "force")
    bridge.snapshot = replace(bridge.snapshot, **{capability: not enabled})
    client.events.append({"type": "result", "id": identifier, "text": " stale", "final": True})
    controller.tick()
    assert not [event for event in controller.drain_events() if event["type"] == "suggestion"]
    assert not controller.invoke("all")
    assert not bridge.insertions
    offer(controller, bridge, " fresh")
    assert controller.invoke("all")
    assert bridge.insertions == [" fresh"]


@pytest.mark.parametrize("changes", [{"on_demand_only": True}, {"can_replace": False}])
@pytest.mark.parametrize("deferred", [False, True])
def test_capability_transition_revokes_drawn_and_deferred_acceptance(fast_runtime, changes, deferred):
    controller, bridge, _ = fast_runtime
    controller.update_config(replace(controller.config, min_chars=1000), persist=False)
    offer(controller, bridge, " arrives tomorrow")
    event = draw(controller)
    bridge.snapshot = replace(bridge.snapshot, **changes)
    if deferred:
        assert controller.fast_invoke("word")
    controller.tick()
    controller.presentation_result(event["key"], event["text"], event["mode"])
    assert not controller.fast_invoke("word")
    assert not bridge.insertions
    assert bridge.replayed == (["word"] if deferred else [])


@pytest.mark.parametrize("tool", ["synonyms", "emoji", "correction"])
def test_append_only_fields_never_generate_or_draw_replacement_tools(runtime, tool):
    controller, bridge, client = runtime
    controller.update_config(replace(
        controller.config, min_chars=1000, synonyms_mode="selection",
        typo_fixes=True, typo_suppression=False), persist=False)
    controller._spelling = Spelling()
    spelling_reads, range_reads = [], []
    controller._spelling.assess = lambda snapshot: spelling_reads.append(snapshot)
    bridge.range_rect = lambda *args: range_reads.append(args) or (60, 20, 40, 18)
    if tool == "synonyms":
        bridge.snapshot = field_snapshot("A useful update", caret=2, selection_length=6, can_replace=False)
    elif tool == "emoji":
        bridge.snapshot = field_snapshot("Hello :smile", can_replace=False)
    else:
        bridge.snapshot = field_snapshot(TYPO_TEXT, can_replace=False)
    controller.tick()
    controller._candidate_since -= 10
    controller.tick()
    assert not controller.invoke("synonyms")
    assert not client.requests
    assert not spelling_reads and not range_reads
    assert not [event for event in controller.drain_events() if event["type"] == "suggestion"]
    assert not controller.invoke("word")
    assert not bridge.insertions


@pytest.mark.parametrize("on_demand", [False, True])
def test_append_only_numbered_alternatives_append_without_rewriting(runtime, on_demand):
    controller, bridge, client = runtime
    controller.update_config(replace(
        controller.config, min_chars=1000, alternatives_enabled=True), persist=False)
    bridge.snapshot = replace(bridge.snapshot, can_replace=False, on_demand_only=on_demand)
    original = bridge.snapshot.text
    submit_command(controller, "alternatives")
    client.events.append({"type": "result", "id": client.identifier,
                          "text": '[" arrives", " is ready"]', "final": True})
    controller.tick()
    assert not controller.invoke("word")
    assert controller.invoke("choice:2")
    assert bridge.snapshot.text == original + " is ready"
    assert bridge.insertions == [" is ready"]


def test_append_only_typo_suppression_withholds_completion_without_offering_a_fix(runtime):
    controller, bridge, client = runtime
    controller.update_config(replace(
        controller.config, typo_fixes=True, typo_suppression=True), persist=False)
    controller._spelling = spelling = Spelling()
    bridge.snapshot = field_snapshot(TYPO_TEXT, can_replace=False)
    controller.tick()
    controller._candidate_since -= 10
    controller.tick()
    assert not client.requests
    assert not [event for event in controller.drain_events() if event["type"] == "suggestion"]
    spelling.suspected = False
    offer(controller, bridge, " the update")
    assert controller.invoke("all")
    assert bridge.insertions == [" the update"]


@pytest.mark.parametrize("tool", ["synonyms", "emoji", "correction"])
def test_losing_replacement_capability_before_acceptance_never_rewrites(runtime, tool):
    controller, bridge, client = runtime
    controller.update_config(replace(controller.config, min_chars=1000), persist=False)
    if tool == "synonyms":
        bridge.snapshot = field_snapshot("A useful update", caret=2, selection_length=6)
        identifier = submit_command(controller, "synonyms")
        client.events.append({"type": "result", "id": identifier, "text": '["helpful"]', "final": True})
        controller.tick()
    elif tool == "emoji":
        bridge.snapshot = field_snapshot("Hello :smile")
        controller.tick()
    else:
        controller.update_config(replace(controller.config, typo_fixes=True), persist=False)
        controller._spelling = Spelling()
        bridge.range_rect = lambda *args: (60, 20, 40, 18)
        bridge.snapshot = field_snapshot(TYPO_TEXT)
        controller.tick()
    assert any(event["type"] == "suggestion" for event in controller.drain_events())
    original = bridge.snapshot.text
    bridge.snapshot = replace(bridge.snapshot, can_replace=False)
    assert not controller.invoke("word")
    assert bridge.snapshot.text == original
    assert not bridge.insertions


def test_fast_synonym_shortcut_passes_through_in_append_only_selection(fast_runtime):
    controller, bridge, client = fast_runtime
    bridge.snapshot = field_snapshot("A useful update", caret=2, selection_length=6, can_replace=False)
    controller.tick()
    assert not controller.fast_invoke("synonyms")
    controller.tick()
    assert not client.requests and not bridge.replayed


@pytest.mark.parametrize("changes", [{"on_demand_only": True}, {"can_replace": False}])
def test_queued_force_cannot_cross_a_capability_transition(runtime, changes):
    controller, bridge, client = runtime
    controller.update_config(replace(controller.config, min_chars=1000), persist=False)
    assert controller.force()
    bridge.snapshot = replace(bridge.snapshot, **changes)
    controller.tick()
    assert not client.requests
    assert not bridge.insertions


def test_append_only_fields_still_schedule_and_accept_ordinary_completion(runtime):
    controller, bridge, client = runtime
    bridge.snapshot = replace(bridge.snapshot, can_replace=False)
    controller.tick()
    controller._candidate_since -= 10
    identifier = wait_request(controller, 0)
    client.events.append({"type": "result", "id": identifier, "text": " arrives", "final": True})
    controller.tick()
    assert controller.invoke("all")
    assert bridge.insertions == [" arrives"]


@pytest.mark.parametrize("before,after,released", [
    ({}, {"prompt_style": "continuation", "max_tokens": 64, "context_chars": 4000}, False),
    ({}, {"endpoint": "http://127.0.0.1:12345", "model": "inactive-server-model"}, False),
    ({}, {"personalization_strength": 0}, False),
    ({}, {"model_path": "/models/other-synthetic.gguf"}, True),
    ({}, {"acceleration": "cpu"}, True),
    ({}, {"global_enabled": False}, True),
    ({}, {"enabled": False}, True),
    ({"clipboard_context": True}, {"clipboard_context": False}, True),
    ({"screen_context": True}, {"screen_context": False}, True),
    ({"screenshot_context": True}, {"screenshot_context": False}, True),
    ({"terminal_completion": True}, {"terminal_completion": False}, True),
    ({"record_history": True}, {"record_history": False}, True),
    ({"record_history": True}, {"personalization_strength": 0}, True),
    ({}, {"excluded_apps": ("writer",)}, True),
    ({"app_rules": {"app:writer": {"enabled": True}}},
     {"app_rules": {"app:writer": {"enabled": False}}}, True),
    ({"app_rules": {"app:writer": {"record_history": True}}}, {"app_rules": {}}, True),
])
def test_settings_preserve_reusable_models_except_on_lifecycle_or_privacy_revocation(runtime, before, after, released):
    controller, _bridge, client = runtime
    controller.update_config(replace(controller.config, **before), persist=False)
    client.released = False
    identifier = submit_command(controller, "force")
    client.events.append({"type": "result", "id": identifier, "text": " obsolete private continuation", "final": True})
    controller.update_config(replace(controller.config, **after), persist=False)
    assert client.released is released
    controller.tick()
    assert not controller._suggestion.text
    assert not any(event.get("type") == "suggestion" for event in controller.drain_events())


def test_focused_preparation_overlaps_typing_without_idle_reload(runtime, tmp_path):
    controller, bridge, client = runtime
    model = tmp_path / "local.gguf"
    model.touch()
    controller.config = replace(controller.config, model_path=str(model), min_chars=20)
    bridge.snapshot = field_snapshot("a")
    controller.tick()
    assert len(client.preparations) == 1
    assert not client.requests
    for _ in range(3):
        controller.tick()
    assert len(client.preparations) == 1
    # A worker's idle release does not itself create new focus/input activity.
    client.released = True
    controller.tick()
    assert len(client.preparations) == 1
    bridge.snapshot = field_snapshot("ab")
    controller.tick()
    assert len(client.preparations) == 2
    assert not client.requests
    bridge.snapshot = None
    client.released = False
    controller.tick()
    assert not client.released
    # A transient missing native capture is not new user activity, either.
    bridge.snapshot = field_snapshot("ab")
    controller.tick()
    assert len(client.preparations) == 2


@pytest.mark.parametrize("changes,prepares", [
    ({}, True),
    ({"window_pid": 0}, False),
    ({"window_id": ""}, False),
    ({"field_rect": None}, False),
    ({"field_rect": (0, 0, 0, 200)}, False),
    ({"field_rect": (0, float("nan"), 400, 200)}, False),
    ({"can_replace": False}, True),
])
def test_empty_native_field_prepares_without_evaluating_or_accepting_writing(runtime, tmp_path, changes, prepares):
    controller, bridge, client = runtime
    model = tmp_path / "local.gguf"
    model.touch()
    controller.config = replace(controller.config, model_path=str(model))
    bridge.snapshot = replace(
        field_snapshot("", window_pid=42, field_rect=(10, 20, 400, 200)),
        caret_rect=None, inline_render=None, **changes)

    controller.tick()

    assert bool(client.preparations) is prepares
    assert not client.requests
    assert not controller.invoke("word")
    assert not bridge.insertions
    assert not any(event.get("type") == "suggestion" for event in controller.drain_events())


@pytest.mark.parametrize("changes", [
    {"multiline": False}, {"protected": True}, {"selection_length": 1},
    {"on_demand_only": True}, {"inline_render": None},
])
def test_nonordinary_fields_never_prepare_model(runtime, tmp_path, changes):
    controller, bridge, client = runtime
    model = tmp_path / "local.gguf"
    model.touch()
    controller.config = replace(controller.config, model_path=str(model), min_chars=20)
    bridge.snapshot = replace(field_snapshot("a"), **changes)
    controller.tick()
    assert not client.preparations
    assert not client.requests


def test_battery_force_only_and_pause_revoke_preparation(runtime, tmp_path):
    controller, bridge, client = runtime
    model = tmp_path / "local.gguf"
    model.touch()
    controller.config = replace(controller.config, model_path=str(model), min_chars=20,
                                battery_on_demand=True)
    bridge.snapshot = field_snapshot("a")
    controller._on_battery = True
    controller.tick()
    assert not client.preparations
    controller._on_battery = False
    controller.tick()
    assert len(client.preparations) == 1
    controller.pause(5)
    controller.tick()
    assert client.released
    assert len(client.preparations) == 1


def test_resident_model_prepares_without_focus_or_writing_and_only_once(runtime, tmp_path):
    controller, bridge, client = runtime
    model = tmp_path / "resident.gguf"
    model.touch()
    controller.config = replace(controller.config, model_path=str(model), idle_release_seconds=0)
    bridge.info = bridge.snapshot = None

    for _ in range(4):
        controller.tick()

    assert len(client.preparations) == 1
    assert bridge.captures == 0
    assert not client.requests and not bridge.insertions
    assert not any(event.get("type") == "suggestion" for event in controller.drain_events())


@pytest.mark.parametrize("changes,on_battery", [
    ({"idle_release_seconds": 300}, False),
    ({"global_enabled": False}, False),
    ({"battery_on_demand": True}, True),
    ({"backend": "ollama"}, False),
])
def test_resident_policy_never_preloads_when_inactive_or_server_managed(runtime, tmp_path, changes, on_battery):
    controller, bridge, client = runtime
    model = tmp_path / "resident.gguf"
    model.touch()
    controller.config = replace(
        replace(controller.config, model_path=str(model), idle_release_seconds=0), **changes)
    controller._on_battery = on_battery
    bridge.info = bridge.snapshot = None
    controller.tick()
    assert not client.preparations and not client.requests


def test_resident_model_reprepares_after_pause_expires_and_explicit_reenable(runtime, tmp_path, monkeypatch):
    controller, bridge, client = runtime
    clock = [100.0]
    monkeypatch.setattr("inklow.runtime.time.monotonic", lambda: clock[0])
    model = tmp_path / "resident.gguf"
    model.touch()
    bridge.info = bridge.snapshot = None
    controller.update_config(replace(controller.config, model_path=str(model), idle_release_seconds=0), persist=False)
    controller.tick()
    assert len(client.preparations) == 1

    controller.pause(5)
    controller.tick()
    assert client.released and len(client.preparations) == 1
    clock[0] += 301
    controller.tick()
    assert len(client.preparations) == 2

    controller.update_config(replace(controller.config, global_enabled=False), persist=False)
    controller.tick()
    assert len(client.preparations) == 2
    controller.update_config(replace(controller.config, global_enabled=True), persist=False)
    controller.tick()
    assert len(client.preparations) == 3
    assert not client.requests and not bridge.insertions
