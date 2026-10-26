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


def test_history_ciphertext_has_no_plaintext_and_vocabulary_strength_changes_guidance(tmp_path):
    path = tmp_path / "local.sqlite3"
    store = unlocked(path)
    assert store.record_history("one", "writer", TEXT)
    assert TEXT.encode() not in path.read_bytes()
    assert b"observability" not in path.read_bytes()
    assert store.history_info()["count"] == 1
    assert store.vocabulary(0, "writer") == ""
    weak = store.vocabulary(20, "writer")
    strong = store.vocabulary(90, "writer")
    assert "observability" in weak and "instrumentation" in strong
    assert "lightly" in weak and "strongly" in strong
    assert weak != strong


def test_tampered_ciphertext_cannot_be_used_for_personalization(tmp_path):
    path = tmp_path / "local.sqlite3"
    store = unlocked(path)
    store.record_history("one", "writer", TEXT)
    with sqlite3.connect(path) as connection:
        sealed = connection.execute("SELECT sealed FROM history").fetchone()[0]
        connection.execute("UPDATE history SET sealed=?", (sealed[:-1] + bytes([sealed[-1] ^ 1]),))
    with pytest.raises(RuntimeError, match="authenticated"):
        store.vocabulary(80)
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
    store.record_history("one", "writer", TEXT)
    assert "observability" in store.vocabulary(90, "writer")
    store.lock_history()
    assert store.vocabulary(90, "writer") == ""
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


def test_vocabulary_cache_follows_every_history_mutation(tmp_path):
    store = unlocked(tmp_path / "local.sqlite3")
    store.record_history("one", "writer", TEXT)
    before = store.vocabulary(10, "writer")
    assert store.vocabulary(10, "writer") == before
    other = ("Kubernetes kubernetes clusters need kubernetes upgrades, kubernetes dashboards and kubernetes "
             "alerts before every kubernetes release reaches the production kubernetes environment.")
    store.record_history("two", "mail", other, domain="mail.example")
    after = store.vocabulary(10, "writer")
    assert "kubernetes" in after and after != before
    store.delete_history(domain="mail.example")
    assert store.vocabulary(10, "writer") == before
