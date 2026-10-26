"""Aggregate counters and opt-in AEAD history. No plaintext history fallback."""
from __future__ import annotations

from collections import Counter, OrderedDict
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

from .config import config_path, normalize_domain

_AAD = b"inklow-history-v1"
_MIN_HISTORY_CHARS = 80
_VOCABULARY_ENTRIES = 200
_VOCABULARY_CACHE = 32
_WORD = re.compile(r"[^\W\d_][\w’'-]{3,31}", re.UNICODE)
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


class HistoryLocked(RuntimeError):
    """Reading encrypted history requires an explicit, consented unlock."""


def _scope_domain(domain: str) -> str:
    return normalize_domain(domain) if domain else ""


class LocalStorage:
    """Only enable_history() loads the key. Reads never unlock implicitly."""

    def __init__(self, path: Path | None = None, key_loader: Callable[[], bytes] | None = None):
        self.path = path or config_path().parent / "local.sqlite3"
        self._key_loader = key_loader or os_history_key
        self._cipher = None
        self._broken = False
        self._error = ""
        self._lock = RLock()
        # Derived plaintext exists only in memory while unlocked and is dropped
        # on every history mutation or lock.
        self._word_index = None
        self._vocabulary_cache: OrderedDict[tuple, str] = OrderedDict()
        self._counts = None

    def _invalidate(self) -> None:
        self._word_index = None
        self._vocabulary_cache.clear()
        self._counts = None

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
        """The explicit unlock; call only for current recording consent or a user action."""
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
            self._invalidate()

    @contextmanager
    def _unlocked(self, unlock: bool):
        """Use the current unlock, or an explicit one-operation unlock."""
        if self._cipher is not None:
            yield
            return
        if not unlock:
            raise HistoryLocked("Encrypted history is locked. Confirm this action to unlock it from OS secure storage.")
        self.enable_history()
        try:
            yield
        finally:
            self.lock_history()

    def record_history(self, identifier: str, app_id: str, text: str, domain: str = "") -> bool:
        """False when unsafe, locked, or scoped to an unusable domain; never unlocks."""
        if not history_safe(text):
            return False
        try:
            domain = _scope_domain(domain)
        except ValueError:
            return False
        with self._lock:
            if self._cipher is None:
                return False
            payload = json.dumps({"app": app_id[:512], "domain": domain, "text": text,
                                  "time": datetime.now(timezone.utc).isoformat()}, ensure_ascii=False).encode()
            nonce = secrets.token_bytes(12)
            sealed = nonce + self._cipher.encrypt(nonce, payload, _AAD + identifier.encode())
            with self._connect() as connection:
                connection.execute("INSERT OR REPLACE INTO history VALUES (?,?)", (identifier, sealed))
            self._invalidate()
            return True

    def _entries(self, limit: int | None = None):
        """Decrypted (identifier, record) pairs, newest first. Records written
        before domain scoping carry no domain and stay attributed to their app."""
        if self._cipher is None:
            raise HistoryLocked("Encrypted history is locked.")
        if not self.path.exists():
            return []
        with self._connect() as connection:
            rows = connection.execute("SELECT id,sealed FROM history ORDER BY rowid DESC LIMIT ?", (-1 if limit is None else limit,)).fetchall()
        entries = []
        try:
            for identifier, sealed in rows:
                raw = self._cipher.decrypt(sealed[:12], sealed[12:], _AAD + identifier.encode())
                value = json.loads(raw)
                if (not isinstance(value, dict) or not isinstance(value.get("text"), str)
                        or not isinstance(value.get("app", ""), str) or not isinstance(value.get("domain", ""), str)):
                    raise ValueError("Invalid history record")
                value.setdefault("app", "")
                value.setdefault("domain", "")
                entries.append((identifier, value))
        except Exception as error:
            self._error = "Encrypted history could not be authenticated. Recording/personalization stopped; clear history to remove damaged records."
            self._cipher = None
            self._broken = True
            self._invalidate()
            raise RuntimeError(self._error) from error
        return entries

    def vocabulary(self, strength: int, app_id: str = "", domain: str = "") -> str:
        """Recurring words from recent history; empty while locked."""
        if strength <= 0:
            return ""
        try:
            domain = _scope_domain(domain)
        except ValueError:
            domain = ""
        with self._lock:
            if self._cipher is None:
                return ""
            key = (strength, app_id, domain)
            cached = self._vocabulary_cache.get(key)
            if cached is not None:
                self._vocabulary_cache.move_to_end(key)
                return cached
            if self._word_index is None:
                self._word_index = [
                    (value["app"], value["domain"], Counter(word.casefold() for word in _WORD.findall(value["text"])))
                    for _, value in self._entries(limit=_VOCABULARY_ENTRIES)]
            words = Counter()
            for entry_app, entry_domain, counts in self._word_index:
                weight = 1 + (entry_app == app_id) + bool(domain and entry_domain == domain)
                for word, frequency in counts.items():
                    words[word] += frequency * weight
            count = max(1, round(strength * 0.4))
            ranked = sorted(words.items(), key=lambda item: (-item[1], item[0]))
            selected = [word for word, frequency in ranked[:count] if frequency >= 2]
            result = ""
            if selected:
                degree = "lightly" if strength < 34 else "moderately" if strength < 67 else "strongly"
                result = f"When natural, {degree} prefer this user's recurring vocabulary ({strength}/100): " + ", ".join(selected)
            self._vocabulary_cache[key] = result
            if len(self._vocabulary_cache) > _VOCABULARY_CACHE:
                self._vocabulary_cache.popitem(last=False)
            return result

    def delete_history(self, app_id: str | None = None, *, domain: str | None = None, unlock: bool = False) -> None:
        """Delete all history, one app's records, or one exact website host's records.

        Scoped deletion must decrypt records to find them; ``unlock=True`` marks an
        explicit user action that may unlock for this operation only."""
        if app_id is not None and domain is not None:
            raise ValueError("Delete history for one app or one domain, not both.")
        target = normalize_domain(domain) if domain is not None else None
        with self._lock:
            self._invalidate()
            if not self.path.exists():
                return
            if app_id is None and target is None:
                with self._connect() as connection:
                    connection.execute("DELETE FROM history")
                self._error = ""
                self._broken = False
            else:
                with self._unlocked(unlock):
                    identifiers = [key for key, value in self._entries()
                                   if (value["domain"] == target if target is not None else value["app"] == app_id)]
                with self._connect() as connection:
                    connection.executemany("DELETE FROM history WHERE id=?", ((key,) for key in identifiers))
            # secure_delete also overwrites deleted ciphertext; VACUUM does not
            # need or expose plaintext and reclaims actual disk space.
            with self._connect() as connection:
                connection.execute("VACUUM")
            self._invalidate()

    def history_counts(self, *, unlock: bool = False) -> dict:
        """Record counts per app and per website host; empty and unavailable while locked."""
        with self._lock:
            if self._counts is not None and self._cipher is not None:
                return {"available": True, "apps": dict(self._counts[0]), "domains": dict(self._counts[1])}
            if not self.path.exists():
                return {"available": True, "apps": {}, "domains": {}}  # Known zero; no key needed.
            if self._cipher is None and not unlock:
                return {"available": False, "apps": {}, "domains": {}}
            with self._unlocked(unlock):
                entries = self._entries()
                apps = Counter(value["app"] for _, value in entries)
                domains = Counter(value["domain"] for _, value in entries if value["domain"])
                counts = (dict(sorted(apps.items())), dict(sorted(domains.items())))
                if self._cipher is not None:
                    self._counts = counts
            return {"available": True, "apps": dict(counts[0]), "domains": dict(counts[1])}

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

    def stats(self, period: str = "week", group_by: str = "day", *, now: datetime | None = None) -> dict:
        """Totals, grouped rows, and averages over active days (days with any acceptance)."""
        period = "today" if period == "day" else period
        if period not in {"today", "week", "month", "year", "all"}:
            raise ValueError("Statistics period must be today, week, month, year, or all.")
        if group_by not in {"day", "app", "month"}:
            raise ValueError("Statistics grouping must be day, app, or month.")
        today = (now or datetime.now().astimezone()).date()
        days = {"today": 0, "week": 6, "month": 29, "year": 364}
        start = (today - timedelta(days=days[period])).isoformat() if period != "all" else "0000"
        labels = ("completions", "words", "characters")
        rows, current, active_days = [], (0, 0, 0), 0
        if self.path.exists():
            expression = {"day": "day", "app": "app", "month": "substr(day,1,7)"}[group_by]
            with self._lock, self._connect() as connection:
                rows = connection.execute(f"SELECT {expression},sum(completions),sum(words),sum(characters) FROM statistics WHERE day>=? AND day<=? GROUP BY {expression} ORDER BY {expression}", (start, today.isoformat())).fetchall()
                current = connection.execute("SELECT coalesce(sum(completions),0),coalesce(sum(words),0),coalesce(sum(characters),0) FROM statistics WHERE day=?", (today.isoformat(),)).fetchone()
                active_days = connection.execute("SELECT count(DISTINCT day) FROM statistics WHERE day>=? AND day<=?", (start, today.isoformat())).fetchone()[0]
        values = [dict(label=row[0], **dict(zip(labels, row[1:])), estimated_seconds=round(row[3] / 5)) for row in rows]
        totals = {name: sum(row[name] for row in values) for name in labels}
        estimated = round(totals["characters"] / 5)
        average = {name: round(value / active_days, 1) if active_days else 0.0
                   for name, value in {**totals, "estimated_seconds": estimated}.items()}
        return {"totals": totals, "rows": values, "today": dict(zip(labels, current)),
                "estimated_seconds": estimated, "active_days": active_days, "daily_average": average}
