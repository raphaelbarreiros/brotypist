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
