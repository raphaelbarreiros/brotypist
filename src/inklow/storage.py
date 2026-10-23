"""Aggregate counters and opt-in AEAD history. No plaintext history fallback."""
from __future__ import annotations

from collections import Counter
from datetime import datetime, timedelta, timezone
from contextlib import contextmanager
import base64
import json
import os
from pathlib import Path
import re
import secrets
import sqlite3
from threading import RLock
from typing import Callable

from .config import config_path

_AAD = b"inklow-history-v1"
_MIN_HISTORY_CHARS = 80
_SENSITIVE = re.compile(
    r"(?i)(?:password|passwd|api[_ -]?key|access[_ -]?token|secret|one[ -]time|verification[ -]code)\s*[:=]"
    r"|\b(?:\d[ -]?){13,19}\b|-----BEGIN [A-Z ]*PRIVATE KEY-----|\b(?:sk|ghp|github_pat)[_-][A-Za-z0-9_-]{16,}"
)


def history_safe(text: str) -> bool:
    """Only substantial, non-secret-looking writing from already eligible fields."""
    return (len(text.strip()) >= _MIN_HISTORY_CHARS and len(text.split()) >= 12
            and len(text) <= 32000 and not _SENSITIVE.search(text))


def os_history_key() -> bytes:
    """Accept only actual OS credential stores, never keyrings.alt/plaintext."""
    try:
        import keyring
        backend = keyring.get_keyring()
        identity = type(backend).__module__ + "." + type(backend).__name__
        allowed = {
            "keyring.backends.macOS.Keyring", "keyring.backends.Windows.WinVaultKeyring",
            "keyring.backends.SecretService.Keyring", "keyring.backends.kwallet.DBusKeyring",
            "keyring.backends.kwallet.DBusKeyringKWallet4",
            "keyring.backends.kwallet.DBusKeyringKWallet5",
        }
        if identity not in allowed or backend.priority <= 0:
            raise RuntimeError("No supported operating-system credential store is available.")
        # Isolated homes never share encryption keys with the live app.
        import hashlib
        account = "history-v1-" + hashlib.sha256(str(config_path().parent.resolve()).encode()).hexdigest()[:24]
        encoded = backend.get_password("Inklow", account)
        if encoded is None:
            database = config_path().parent / "local.sqlite3"
            if database.exists():
                connection = sqlite3.connect(database.resolve().as_uri() + "?mode=ro", uri=True)
                try:
                    if connection.execute("SELECT count(*) FROM history").fetchone()[0]:
                        raise RuntimeError("The history key is missing; clear unrecoverable encrypted history before creating a new key.")
                finally:
                    connection.close()
            key = secrets.token_bytes(32)
            encoded = base64.b64encode(key).decode("ascii")
            backend.set_password("Inklow", account, encoded)
            if backend.get_password("Inklow", account) != encoded:
                raise RuntimeError("The OS credential store did not retain the history key.")
        key = base64.b64decode(encoded, validate=True)
        if len(key) != 32:
            raise RuntimeError("The stored history key is invalid.")
        return key
    except Exception as error:
        # Backend exceptions may include secrets; never propagate their details.
        raise RuntimeError("Encrypted history requires an unlocked macOS Keychain, Windows Credential Manager, or Linux Secret Service/KWallet and keyring support. No history was written.") from error


class LocalStorage:
    def __init__(self, path: Path | None = None, key_loader: Callable[[], bytes] | None = None):
        self.path = path or config_path().parent / "local.sqlite3"
        self._key_loader = key_loader or os_history_key
        self._cipher = None
        self._broken = False
        self._error = ""
        self._lock = RLock()

    @contextmanager
    def _connect(self):
        self.path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        connection = sqlite3.connect(self.path, timeout=2)
        try:
            os.chmod(self.path, 0o600)
            connection.execute("PRAGMA secure_delete=ON")
            connection.execute("CREATE TABLE IF NOT EXISTS history (id TEXT PRIMARY KEY, sealed BLOB NOT NULL)")
            connection.execute("CREATE TABLE IF NOT EXISTS statistics (day TEXT, app TEXT, completions INTEGER, words INTEGER, characters INTEGER, PRIMARY KEY(day,app))")
            with connection:
                yield connection
        finally:
            connection.close()

    def enable_history(self) -> None:
        with self._lock:
            if self._broken:
                raise RuntimeError(self._error)
            if self._cipher is not None:
                return
            try:
                from cryptography.hazmat.primitives.ciphers.aead import AESGCM
                key = self._key_loader()
                if not isinstance(key, bytes) or len(key) != 32:
                    raise RuntimeError("Invalid secure-store key.")
                self._cipher = AESGCM(key)
                self._error = ""
            except Exception as error:
                self._cipher = None
                self._error = "Encrypted history is unavailable. Unlock your OS credential store and install keyring and cryptography; plaintext recording is never used."
                raise RuntimeError(self._error) from error

    def lock_history(self) -> None:
        with self._lock:
            self._cipher = None

    def record_history(self, identifier: str, app_id: str, text: str) -> bool:
        if not history_safe(text):
            return False
        with self._lock:
            self.enable_history()
            payload = json.dumps({"app": app_id[:512], "text": text, "time": datetime.now(timezone.utc).isoformat()}, ensure_ascii=False).encode()
            nonce = secrets.token_bytes(12)
            sealed = nonce + self._cipher.encrypt(nonce, payload, _AAD + identifier.encode())
            with self._connect() as connection:
                connection.execute("INSERT OR REPLACE INTO history VALUES (?,?)", (identifier, sealed))
            return True

    def _entries(self, limit: int | None = None):
        self.enable_history()
        if not self.path.exists():
            return []
        with self._connect() as connection:
            rows = connection.execute("SELECT id,sealed FROM history ORDER BY rowid DESC LIMIT ?", (-1 if limit is None else limit,)).fetchall()
        entries = []
        try:
            for identifier, sealed in rows:
                raw = self._cipher.decrypt(sealed[:12], sealed[12:], _AAD + identifier.encode())
                value = json.loads(raw)
                if not isinstance(value, dict) or not isinstance(value.get("text"), str):
                    raise ValueError("Invalid history record")
                entries.append((identifier, value))
        except Exception as error:
            self._error = "Encrypted history could not be authenticated. Recording/personalization stopped; clear history to remove damaged records."
            self._cipher = None
            self._broken = True
            raise RuntimeError(self._error) from error
        return entries

    def vocabulary(self, strength: int, app_id: str = "") -> str:
        if strength <= 0:
            return ""
        with self._lock:
            entries = self._entries(limit=200)
            words = Counter()
            for _, value in entries:
                weight = 2 if value.get("app") == app_id else 1
                for word in re.findall(r"[^\W\d_][\w’'-]{3,31}", value["text"], flags=re.UNICODE):
                    words[word.casefold()] += weight
            count = max(1, round(strength * 0.4))
            selected = [word for word, frequency in words.most_common(count) if frequency >= 2]
            if not selected:
                return ""
            degree = "lightly" if strength < 34 else "moderately" if strength < 67 else "strongly"
            return f"When natural, {degree} prefer this user's recurring vocabulary ({strength}/100): " + ", ".join(selected)

    def delete_history(self, app_id: str | None = None) -> None:
        with self._lock:
            if not self.path.exists():
                return
            if app_id is None:
                with self._connect() as connection:
                    connection.execute("DELETE FROM history")
                self._error = ""
                self._broken = False
            else:
                identifiers = [key for key, value in self._entries() if value.get("app") == app_id]
                with self._connect() as connection:
                    connection.executemany("DELETE FROM history WHERE id=?", ((key,) for key in identifiers))
            # secure_delete also overwrites deleted ciphertext; VACUUM does not
            # need or expose plaintext and reclaims actual disk space.
            with self._connect() as connection:
                connection.execute("VACUUM")

    def history_info(self) -> dict:
        with self._lock:
            count = size = 0
            if self.path.exists():
                with self._connect() as connection:
                    count, size = connection.execute("SELECT count(*),coalesce(sum(length(sealed)),0) FROM history").fetchone()
            return {"count": count, "bytes": size, "available": self._cipher is not None, "error": self._error}

    def record_acceptance(self, app_id: str, text: str, *, completion: bool = True, now: datetime | None = None) -> None:
        if not text:
            return
        day = (now or datetime.now().astimezone()).date().isoformat()
        with self._lock, self._connect() as connection:
            connection.execute(
                "INSERT INTO statistics VALUES (?,?,?,?,?) ON CONFLICT(day,app) DO UPDATE SET completions=completions+excluded.completions, words=words+excluded.words, characters=characters+excluded.characters",
                (day, app_id[:512], int(completion), len(re.findall(r"[^\W_]+(?:[’'-][^\W_]+)*", text, re.UNICODE)), len(text)))

    def stats(self, period: str = "week", group_by: str = "day") -> dict:
        period = "today" if period == "day" else period
        if period not in {"today", "week", "month", "year", "all"}:
            raise ValueError("Statistics period must be today, week, month, year, or all.")
        if group_by not in {"day", "app", "month"}:
            raise ValueError("Statistics grouping must be day, app, or month.")
        today = datetime.now().astimezone().date()
        days = {"today": 0, "week": 6, "month": 29, "year": 364}
        start = (today - timedelta(days=days[period])).isoformat() if period != "all" else "0000"
        empty = {"completions": 0, "words": 0, "characters": 0}
        if not self.path.exists():
            return {"totals": dict(empty), "rows": [], "today": dict(empty), "estimated_seconds": 0}
        expression = {"day": "day", "app": "app", "month": "substr(day,1,7)"}[group_by]
        with self._lock, self._connect() as connection:
            rows = connection.execute(f"SELECT {expression},sum(completions),sum(words),sum(characters) FROM statistics WHERE day>=? GROUP BY {expression} ORDER BY {expression}", (start,)).fetchall()
            current = connection.execute("SELECT coalesce(sum(completions),0),coalesce(sum(words),0),coalesce(sum(characters),0) FROM statistics WHERE day=?", (today.isoformat(),)).fetchone()
        labels = ("completions", "words", "characters")
        values = [dict(label=row[0], **dict(zip(labels, row[1:]))) for row in rows]
        totals = {name: sum(row[name] for row in values) for name in labels}
        return {"totals": totals, "rows": values, "today": dict(zip(labels, current)), "estimated_seconds": round(totals["characters"] / 5)}
