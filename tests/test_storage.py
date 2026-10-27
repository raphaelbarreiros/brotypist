"""Real AEAD storage protects content and counters never contain writing."""
from datetime import datetime, timedelta
import json
import secrets
import sqlite3

import pytest

from inklow.storage import HistoryLocked, LocalStorage

TEXT = ("We discussed observability and instrumentation during the planning meeting. "
        "The observability review needs more instrumentation examples before the next team meeting.")


def unlocked(path):
    store = LocalStorage(path, lambda: b"a" * 32)
    store.enable_history()
    return store


def learn(store, strength=90, app="writer", domain="", text=TEXT, allowed=lambda app, domain: True):
    return store.personalization(strength, app, domain, text=text, source_allowed=allowed)


def test_history_ciphertext_has_no_plaintext_and_strength_scales_evidence(tmp_path):
    path = tmp_path / "local.sqlite3"
    store = unlocked(path)
    assert store.record_history("one", "writer", TEXT, authored_spans=[(0, len(TEXT))])
    assert TEXT.encode() not in path.read_bytes()
    assert b"observability" not in path.read_bytes()
    assert store.history_info()["count"] == 1
    assert learn(store, 0) == ""
    weak = json.loads(learn(store, 20))
    strong = json.loads(learn(store, 90))
    assert "observability" in weak["vocabulary"]
    assert "instrumentation" in strong["vocabulary"]
    assert weak["strength"] == 20 and strong["strength"] == 90


def test_tampered_ciphertext_cannot_be_used_for_personalization(tmp_path):
    path = tmp_path / "local.sqlite3"
    store = unlocked(path)
    store.record_history("one", "writer", TEXT)
    with sqlite3.connect(path) as connection:
        sealed = connection.execute("SELECT sealed FROM history").fetchone()[0]
        connection.execute("UPDATE history SET sealed=?", (sealed[:-1] + bytes([sealed[-1] ^ 1]),))
    with pytest.raises(RuntimeError, match="authenticated"):
        learn(store)
    assert not store.history_info()["available"]
    store.delete_history()
    assert store.history_info()["count"] == 0


def test_unavailable_secure_store_never_creates_plaintext_history(tmp_path):
    def locked():
        raise RuntimeError("Synthetic locked store")
    path = tmp_path / "local.sqlite3"
    store = LocalStorage(path, locked)
    with pytest.raises(RuntimeError, match="plaintext"):
        store.enable_history()
    assert not store.record_history("one", "writer", TEXT)
    assert not path.exists()


def test_per_app_delete_removes_actual_encrypted_records_and_clear_reclaims_all(tmp_path):
    store = unlocked(tmp_path / "local.sqlite3")
    store.record_history("one", "writer", TEXT)
    store.record_history("two", "mail", TEXT)
    store.delete_history("writer")
    assert store.history_info()["count"] == 1
    assert [entry[1]["app"] for entry in store._entries()] == ["mail"]
    store.delete_history()
    assert store.history_info()["count"] == 0


def test_short_or_secret_looking_text_is_never_written(tmp_path):
    path = tmp_path / "local.sqlite3"
    store = unlocked(path)
    assert not store.record_history("short", "writer", "Small message")
    assert not store.record_history("secret", "writer", TEXT + " api_key: synthetic-sensitive-value")
    assert not path.exists()


def test_statistics_count_only_explicit_acceptance_and_group_real_periods(tmp_path):
    path = tmp_path / "local.sqlite3"
    store = LocalStorage(path)
    now = datetime.now().astimezone()
    store.record_acceptance("writer", " first ", now=now)
    store.record_acceptance("writer", "second", completion=False, now=now)
    store.record_acceptance("mail", "sent yesterday", now=now - timedelta(days=1))
    store.record_acceptance("archive", "old", now=now - timedelta(days=60))
    today = store.stats("today", "app")
    assert today["totals"] == {"completions": 1, "words": 2, "characters": 13}
    week = store.stats("week", "app")
    assert {row["label"] for row in week["rows"]} == {"writer", "mail"}
    assert store.stats("all")["totals"]["completions"] == 3
    assert b"sent yesterday" not in path.read_bytes()
    assert b" first " not in path.read_bytes()
    average = store.stats("week", "day", now=now)
    assert average["active_days"] == 2
    assert average["daily_average"]["characters"] == round((13 + 14) / 2, 1)
    assert [row["estimated_seconds"] for row in average["rows"]] == [3, 3]
    assert store.stats("today", now=now - timedelta(days=30))["active_days"] == 0


def test_consent_off_never_reunlocks_or_serves_cached_history(tmp_path):
    loads = []

    def key():
        loads.append(1)
        return b"a" * 32
    store = LocalStorage(tmp_path / "local.sqlite3", key)
    store.enable_history()
    store.record_history("one", "writer", TEXT, authored_spans=[(0, len(TEXT))])
    assert "observability" in learn(store)
    store.lock_history()
    assert learn(store) == ""
    assert not store.record_history("two", "writer", TEXT)
    assert store.history_counts() == {"available": False, "apps": {}, "domains": {}}
    with pytest.raises(HistoryLocked):
        store._entries()
    with pytest.raises(HistoryLocked):
        store.delete_history("writer")
    assert loads == [1]
    assert store.history_info()["count"] == 1


def test_domain_delete_is_exact_and_explicit_unlock_relocks(tmp_path):
    path = tmp_path / "local.sqlite3"
    store = unlocked(path)
    for identifier, domain in (("a", "example.com"), ("b", "docs.example.com"), ("c", "notexample.com"), ("d", "")):
        assert store.record_history(identifier, "browser", TEXT, domain=domain)
    # A record sealed before domain scoping existed stays attributed to its app.
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    nonce = secrets.token_bytes(12)
    legacy = json.dumps({"app": "browser", "text": TEXT, "time": "2026-01-01T00:00:00+00:00"}).encode()
    with store._connect() as connection:
        connection.execute("INSERT INTO history VALUES (?,?)", ("legacy", nonce + AESGCM(b"a" * 32).encrypt(nonce, legacy, b"inklow-history-v1legacy")))
    assert store.history_counts()["domains"] == {"docs.example.com": 1, "example.com": 1, "notexample.com": 1}
    store.lock_history()
    store.delete_history(domain="Example.COM.", unlock=True)
    info = store.history_info()
    assert info["count"] == 4 and not info["available"]
    counts = store.history_counts(unlock=True)
    assert counts == {"available": True, "apps": {"browser": 4}, "domains": {"docs.example.com": 1, "notexample.com": 1}}
    assert not store.history_info()["available"]


def test_learning_follows_writes_deletion_and_current_source_policy(tmp_path):
    store = unlocked(tmp_path / "local.sqlite3")
    store.record_history("one", "writer", TEXT, authored_spans=[(0, len(TEXT))])
    before = learn(store)
    assert before
    assert learn(store, allowed=lambda app, domain: app != "writer") == ""
    assert learn(store) == before
    store.delete_history("writer")
    assert learn(store) == ""
    store.record_history("one", "writer", TEXT, authored_spans=[(0, len(TEXT))])
    assert learn(store) == before
    store.delete_history()
    assert learn(store) == ""


def test_learning_uses_authored_same_language_and_context_only(tmp_path):
    store = unlocked(tmp_path / "local.sqlite3")
    informal = ("we don't wait! we don't stop! we don't worry! the observability review is ready! "
                "the instrumentation review is ready! we don't need another observability review!")
    formal = ("The observability review, which includes the instrumentation results, is ready for the "
              "team to evaluate carefully before the next meeting, and the observability report, "
              "which covers the instrumentation review, will support the entire planning process.")
    portuguese = ("Nós discutimos observabilidade e instrumentação durante a reunião de planejamento. "
                  "A observabilidade precisa de mais instrumentação para a próxima reunião da equipe.")
    for identifier, text, app, domain in (
            ("a", informal, "chat", ""), ("b", formal, "writer", ""),
            ("c", portuguese, "writer", ""), ("d", informal, "browser", "chat.example")):
        store.record_history(identifier, app, text, domain, authored_spans=[(0, len(text))])
    chat = json.loads(learn(store, app="chat"))
    work = json.loads(learn(store))
    assert chat["style"]["mean_sentence_words"] < work["style"]["mean_sentence_words"]
    assert chat["style"]["exclamations_per_100_words"] > work["style"]["exclamations_per_100_words"]
    assert chat["style"]["contractions_per_100_words"] > work["style"]["contractions_per_100_words"]
    assert chat["style"]["lowercase_sentence_start_ratio"] > work["style"]["lowercase_sentence_start_ratio"]
    assert json.loads(learn(store, text=portuguese))["language"] == "pt"
    assert "observabilidade" not in learn(store)
    assert learn(store, app="unrelated") == ""
    assert learn(store, app="browser", domain="other.example") == ""
    assert json.loads(learn(store, app="browser", domain="chat.example"))["style"] == chat["style"]
    assert learn(store, text="x y z") == ""


def test_generated_and_legacy_history_is_not_authored_learning(tmp_path):
    store = unlocked(tmp_path / "local.sqlite3")
    store.record_history("legacy", "writer", TEXT)
    assert learn(store) == ""
    generated = " Generatedphantom generatedphantom generatedphantom generatedphantom."
    store.record_history("mixed", "writer", TEXT + generated, authored_spans=[(0, len(TEXT))])
    assert "observability" in learn(store)
    assert "generatedphantom" not in learn(store)
    store.record_history("mixed", "writer", TEXT + generated, authored_spans=[])
    assert learn(store) == ""


def test_learning_requires_unlock_without_implicit_key_access(tmp_path):
    loads = []
    store = LocalStorage(tmp_path / "local.sqlite3", lambda: loads.append(True) or b"a" * 32)
    assert learn(store) == ""
    assert loads == []
    assert not store.path.exists()


def test_learning_evidence_is_bounded_and_contains_no_raw_sentences(tmp_path):
    store = unlocked(tmp_path / "local.sqlite3")
    tokens = ["term" + chr(97 + first) + chr(97 + second)
              for first in range(8) for second in range(8)]
    text = TEXT + " " + " ".join(tokens + tokens) + "."
    store.record_history("bounded", "writer", text, authored_spans=[(0, len(text))])
    evidence = learn(store)
    assert len(evidence) <= 2048
    assert len(json.loads(evidence)["vocabulary"]) <= 24
    assert TEXT not in evidence


def test_authored_chat_without_terminal_punctuation_still_teaches_vocabulary(tmp_path):
    store = unlocked(tmp_path / "local.sqlite3")
    text = ("we check the telemetry with our team and we review the telemetry before "
            "sharing the report with our team")
    store.record_history("chat", "writer", text, authored_spans=[(0, len(text))])
    evidence = json.loads(learn(store))
    assert "telemetry" in evidence["vocabulary"]
    assert "mean_sentence_words" not in evidence["style"]
