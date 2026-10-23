"""Real AEAD storage protects content and counters never contain writing."""
from datetime import datetime, timedelta
import sqlite3

import pytest

from inklow.storage import LocalStorage

TEXT = ("We discussed observability and instrumentation during the planning meeting. "
        "The observability review needs more instrumentation examples before the next team meeting.")


def test_history_ciphertext_has_no_plaintext_and_vocabulary_strength_changes_guidance(tmp_path):
    path = tmp_path / "local.sqlite3"
    store = LocalStorage(path, lambda: b"a" * 32)
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
    store = LocalStorage(path, lambda: b"a" * 32)
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
        store.record_history("one", "writer", TEXT)
    assert not path.exists()


def test_per_app_delete_removes_actual_encrypted_records_and_clear_reclaims_all(tmp_path):
    store = LocalStorage(tmp_path / "local.sqlite3", lambda: b"a" * 32)
    store.record_history("one", "writer", TEXT)
    store.record_history("two", "mail", TEXT)
    store.delete_history("writer")
    assert store.history_info()["count"] == 1
    assert [entry[1]["app"] for entry in store._entries()] == ["mail"]
    store.delete_history()
    assert store.history_info()["count"] == 0


def test_short_or_secret_looking_text_is_never_written(tmp_path):
    path = tmp_path / "local.sqlite3"
    store = LocalStorage(path, lambda: b"a" * 32)
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
