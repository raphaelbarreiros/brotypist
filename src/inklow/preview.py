"""Ephemeral inference for the owned Welcome field, never another app's input.

Uses the same provider and suggestion state as normal typing. No history, screen,
clipboard, native hooks or settings writes belong to a preview session.
"""
from __future__ import annotations

from copy import deepcopy
from dataclasses import replace
import time

from .core import Snapshot, SuggestionState
from .features import completion_limits, current_word_only, include_punctuation, mid_word
from .process_client import ProcessClient


class PreviewSession:
    def __init__(self, config, owner_controller):
        self.config = deepcopy(config)
        self.config.validate()
        self._owner = owner_controller
        self._client = ProcessClient()
        self._suggestion = SuggestionState()
        self._snapshot = Snapshot("brotypist.preview", "welcome-try-it", "", 0)
        self._request = None
        self._attempted = None
        self._expected = None
        self._changed = time.monotonic()
        self._accepted = 0
        self._error = ""
        self._closed = False
        # The caller only opens a preview while its own field has focus. Cancel
        # the external request first, then wait for its worker to exit in poll().
        # This keeps two large model workers from staying resident together.
        with owner_controller._lock:
            owner_controller._clear(hide_accessory=True)
            owner_controller._client.release()

    def update(self, text: str, caret: int, *, selection_length: int = 0):
        if self._closed:
            return
        if not (0 <= caret <= len(text) and 0 <= selection_length <= len(text) - caret):
            raise ValueError("Preview cursor must describe the current owned document.")
        current = replace(self._snapshot, text=text, caret=caret,
                          selection_length=selection_length)
        if current.key == self._snapshot.key:
            return
        self._client.cancel()
        self._request = None
        confirmed = self._expected is not None and current.key == self._expected.key
        if confirmed:
            self._accepted += 1
        elif not self._suggestion.advance(current):
            self._suggestion.invalidate()
        self._expected = None
        self._snapshot = current
        self._attempted = current.key if self._suggestion.text else None
        self._changed = time.monotonic()
        self._error = ""

    def poll(self) -> dict:
        if self._closed:
            return {"state": "idle", "suggestion": "", "message": "Preview closed.",
                    "accepted": self._accepted}
        for event in self._client.poll():
            request = self._request
            if (request is None or event.get("id") != request[0]
                    or request[1] != self._snapshot.key
                    or request[2] != self._suggestion.generation):
                continue
            if event.get("type") == "error":
                self._error = str(event.get("message", "Local inference failed."))
                self._suggestion.invalidate()
                self._request = None
            elif event.get("type") == "result":
                text = event.get("text", "")
                if self.config.completion_length == "word":
                    text = current_word_only(self._snapshot.prefix, text)
                self._suggestion.offer(request[2], self._snapshot, text)
                if event.get("final"):
                    self._request = None
        eligible = self._snapshot.eligible(self.config.min_chars) and (
            self.config.complete_inside_text or not self._snapshot.suffix) and (
            self.config.completion_length != "word" or mid_word(self._snapshot.prefix))
        pending_insert = self._expected is not None
        releasing = self._owner._client.status().get("worker_pid") is not None
        if (eligible and not pending_insert and not releasing and not self._error
                and not self._suggestion.text and self._request is None
                and self._attempted != self._snapshot.key
                and time.monotonic() - self._changed >= self.config.debounce_ms / 1000):
            limit, max_words = completion_limits(self.config.completion_length)
            config = replace(self.config, max_tokens=min(self.config.max_tokens, limit))
            try:
                identifier = self._client.submit(
                    config, self._snapshot.prefix[-config.context_chars:],
                    self._snapshot.suffix[:config.context_chars],
                    instructions=config.custom_instructions[:26000], context="",
                    max_words=max_words)
                self._request = (identifier, self._snapshot.key, self._suggestion.generation)
                self._attempted = self._snapshot.key
            except (RuntimeError, ValueError) as error:
                self._error = str(error)
        suggestion = (self._suggestion.text if not pending_insert and eligible
                      and self._suggestion.snapshot is not None
                      and self._suggestion.snapshot.key == self._snapshot.key else "")
        if self._error:
            state, message = "error", self._error
        elif suggestion:
            state, message = "ready", "Suggestion ready."
        elif self._request is not None:
            status = self._client.status()
            state = "loading"
            message = status.get("message") or (
                "Generating a suggestion…" if status.get("model_loaded")
                else "Warming up the selected model…")
        elif eligible and releasing:
            state, message = "waiting", "Releasing the typing model for this preview…"
        elif self._accepted:
            state, message = "idle", f"Accepted {self._accepted} suggestion{'s' if self._accepted != 1 else ''}."
        elif eligible:
            state, message = "waiting", "Waiting for a typing pause…"
        else:
            state, message = "idle", "Waiting for you to type…"
        return {"state": state, "suggestion": suggestion, "message": message,
                "accepted": self._accepted}

    def _accept(self, full: bool) -> str:
        if self._closed or self._expected is not None:
            return ""
        accepted = self._suggestion.accept(
            self._snapshot, full=full, trailing_space=self.config.trailing_space,
            trailing_punctuation=include_punctuation(self.config.punctuation_mode, full))
        if accepted:
            self._client.cancel()
            self._request = None
            # Only update() confirming the actual widget edit counts acceptance.
            self._expected = self._suggestion.snapshot
        return accepted

    def accept_word(self) -> str:
        return self._accept(False)

    def accept_all(self) -> str:
        return self._accept(True)

    def dismiss(self):
        if self._closed:
            return
        self._client.cancel()
        self._request = None
        self._expected = None
        self._suggestion.invalidate()
        self._attempted = self._snapshot.key
        self._error = ""

    def close(self):
        if self._closed:
            return
        self._closed = True
        self._request = None
        self._expected = None
        self._suggestion.invalidate()
        self._snapshot = replace(self._snapshot, text="", caret=0, selection_length=0)
        self._client.close()
        self._owner = None
