"""Constrain loopback inference, safe insertions and consent-bound context reads.

Native context and clipboard readers are synthetic; no user clipboard is read.
"""

from dataclasses import replace

import pytest

from inklow.config import Config, local_endpoint
from inklow.providers import clean_completion


@pytest.mark.parametrize("endpoint", [
    "https://example.com", "http://localhost:11434", "http://192.168.1.2:8080",
    "http://127.0.0.1.evil.example", "http://user:pass@127.0.0.1:8080",
    "file:///tmp/model", "http://127.0.0.1:8080?forward=remote",
])
def test_nonlocal_or_ambiguous_inference_endpoints_are_refused(endpoint):
    with pytest.raises(ValueError):
        local_endpoint(endpoint)


def test_literal_loopback_including_ipv6_is_accepted():
    assert local_endpoint("http://127.0.0.1:11434/") == "http://127.0.0.1:11434"
    assert local_endpoint("http://[::1]:8080/v1") == "http://[::1]:8080/v1"


@pytest.mark.parametrize("changes", [
    {"debounce_ms": True}, {"max_tokens": 0}, {"max_tokens": 100000},
    {"global_enabled": "false"}, {"excluded_apps": "terminal"},
    {"backend": "cloud"},
])
def test_invalid_settings_cannot_enable_unbounded_or_unsafe_operation(changes):
    with pytest.raises(ValueError):
        replace(Config(), **changes).validate()


def test_prediction_sanitization_preserves_leading_space_and_existing_suffix():
    assert clean_completion(" tomorrow.\x00<|im_end|>ignored", ".") == " tomorrow"
    assert clean_completion(" café\nnew paragraph", "") == " café"


def test_generated_tabs_cannot_navigate_to_another_field():
    assert clean_completion(" café\tamanhã", "") == " café amanhã"


def test_window_context_is_accessibility_only_and_rejects_wrong_window_identity():
    from types import SimpleNamespace
    from inklow.core import Snapshot
    from inklow.system import screen_text
    snapshot = Snapshot("writer", "body", "The update", 10,
                        window_id="writer:window", window_revision="revision-1")
    calls = []
    def read(expected, *, authorized, max_chars):
        assert authorized()
        calls.append(expected.window_id)
        return {"text": "The update arrives Friday", "provenance": "accessibility",
                "window_id": "unrelated-window", "revision": expected.window_revision}
    bridge = SimpleNamespace(focused_context=read)
    result = screen_text(snapshot, bridge=bridge, authorized=lambda: True)
    assert result["status"] == "stale" and result["text"] == ""
    assert calls == ["writer:window"]


def test_window_context_revoked_during_accessibility_read_drops_content():
    from types import SimpleNamespace
    from inklow.core import Snapshot
    from inklow.system import screen_text
    snapshot = Snapshot("writer", "body", "The update", 10,
                        window_id="writer:window", window_revision="revision-1")
    consent = [True]
    def read(expected, **kwargs):
        # Revoke after admission but before return to exercise the second consent check.
        consent[0] = False
        return {"text": "update private content", "window_id": expected.window_id,
                "revision": expected.window_revision}
    result = screen_text(snapshot, bridge=SimpleNamespace(focused_context=read), authorized=lambda: consent[0])
    assert result["status"] == "blocked"
    assert result["text"] == ""


def test_unverified_window_context_never_calls_native_reader():
    from types import SimpleNamespace
    from inklow.core import Snapshot
    from inklow.system import screen_text
    calls = []
    bridge = SimpleNamespace(focused_context=lambda *args, **kwargs: calls.append("private read"))
    snapshot = Snapshot("writer", "body", "The update", 10)
    result = screen_text(snapshot, bridge=bridge, authorized=lambda: True)
    assert calls == []
    assert result["status"] == "blocked" and not result["text"]


@pytest.mark.parametrize("revoke_during_read", [False, True])
def test_native_mac_clipboard_rechecks_consent_without_qt_or_user_clipboard(monkeypatch, revoke_during_read):
    import sys
    from types import SimpleNamespace
    from inklow import system
    consent = [True]
    reads = []

    class PasteboardBoundary:
        """Record native read attempts and optionally revoke consent during the read."""
        def stringForType_(self, kind):
            reads.append(kind)
            if revoke_during_read:
                consent[0] = False
            return "Fixture clipboard reference"

    boundary = PasteboardBoundary()
    monkeypatch.setattr(system.sys, "platform", "darwin")
    monkeypatch.setitem(sys.modules, "AppKit", SimpleNamespace(
        NSPasteboard=SimpleNamespace(generalPasteboard=lambda: boundary),
        NSPasteboardTypeString="fixture-type"))
    result = system.clipboard_text(authorized=lambda: consent[0])
    assert result == ("" if revoke_during_read else "Fixture clipboard reference")
    assert reads == ["fixture-type"]
    consent[0] = False
    assert system.clipboard_text(authorized=lambda: consent[0]) == ""
    assert reads == ["fixture-type"]
