"""Strict menu preferences and isolated settings migration, without draft persistence."""
from dataclasses import asdict, replace
import json

import pytest

from inklow.config import Config, config_path, load_config, provider_identity, save_config
from inklow.models import PRESETS


@pytest.mark.parametrize("changes", [
    {"idle_release_seconds": -1}, {"idle_release_seconds": True},
    {"personalization_strength": 101}, {"screen_context": 1},
    {"completion_length": "unbounded"}, {"emoji_gender": "unknown"},
    {"emoji_skin_tone": 6}, {"record_unaccepted": "yes"},
    {"alternatives_delay_ms": 0}, {"synonyms_mode": "always"},
    {"punctuation_mode": "sometimes"}, {"punctuation_mode": False},
    {"custom_instructions": "x" * 8001},
    {"app_rules": {"domain:https://private.example/path": {"enabled": False}}},
    {"app_rules": {"app:writer": {"record_history": 1}}},
    {"app_rules": {"app:writer": {"text": "draft must not be a preference"}}},
    {"shortcuts": {"word": "Tab", "all": "Tab"}},
    {"battery_model": "not-a-preset"}, {"battery_model": True}, {"model_id": "not-a-preset"},
    {"typo_fixes": 1}, {"screenshot_context": "yes"},
    {"app_rules": {"app:code": {"complete_in_code_editors": "yes"}}},
])
def test_invalid_menu_preferences_are_rejected(changes):
    with pytest.raises(ValueError):
        replace(Config(), **changes).validate()


def test_legacy_editor_flag_is_discarded_and_explicit_global_disable_survives(tmp_path, monkeypatch):
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    config_path().write_text(json.dumps({"keep_running": False, "global_enabled": False, "unknown_draft": "never persist this"}))
    config = load_config()
    assert config.global_enabled is False
    assert "keep_running" not in asdict(config)
    save_config(config)
    content = json.loads(config_path().read_text())
    assert "keep_running" not in content and "unknown_draft" not in content
    assert "never persist this" not in config_path().read_text()


def test_new_configs_enable_global_typing_and_home_override_is_fully_isolated(tmp_path, monkeypatch):
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path / "isolated"))
    config = load_config()
    assert config.global_enabled
    config.app_rules = {"domain:EXAMPLE.COM.": {"enabled": False}}
    save_config(config)
    assert config_path() == tmp_path / "isolated" / "settings.json"
    assert load_config().app_rules == {"domain:example.com": {"enabled": False}}
    assert not list(config_path().parent.glob(".settings-*"))


def test_latency_defaults_never_override_saved_scheduling_or_context_consent(tmp_path, monkeypatch):
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    explicit = {"debounce_ms": 400, "idle_release_seconds": 60,
                "screen_context": False, "clipboard_context": False}
    config_path().write_text(json.dumps(explicit))
    config = load_config()
    save_config(config)
    saved = json.loads(config_path().read_text())
    assert {key: saved[key] for key in explicit} == explicit


def test_legacy_settings_keep_their_model_and_text_while_new_fields_migrate(tmp_path, monkeypatch):
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    preset = PRESETS[0]
    legacy = {"model_path": f"/models/{preset.filename}", "battery_smaller_model": True, "custom_instructions": ""}
    config_path().write_text(json.dumps(legacy))
    config = load_config()
    assert (config.model_path, config.model_id, config.custom_instructions) == (legacy["model_path"], preset.id, "")
    assert config.battery_model == ("tiny" if any(p.id == "tiny" for p in PRESETS) else "same")
    custom = replace(config, completion_length="word", typo_suppression=True, typo_fixes=True,
                     screenshot_context=True, screenshot_appearance=True, battery_model="same",
                     app_rules={"app:code": {"improve_compatibility": True, "complete_in_code_editors": False}})
    save_config(custom)
    assert "battery_smaller_model" not in json.loads(config_path().read_text())
    assert load_config() == custom


def test_custom_model_and_instructions_survive_loading_existing_settings(tmp_path, monkeypatch):
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    settings = {"model_path": "/custom/own.gguf", "custom_instructions": "Mine."}
    config_path().write_text(json.dumps(settings))
    saved = load_config()
    assert saved.model_path == settings["model_path"]
    assert saved.model_id == ""
    assert saved.custom_instructions == settings["custom_instructions"]


@pytest.mark.parametrize("backend,changes,reallocated", [
    ("gguf", {"prompt_style": "writing", "max_tokens": 64, "context_chars": 4000}, False),
    ("gguf", {"endpoint": "http://127.0.0.1:12345", "model": "unused-server-model"}, False),
    ("gguf", {"model_path": "/models/other.gguf"}, True),
    ("gguf", {"acceleration": "cpu"}, True),
    ("ollama", {"prompt_style": "writing", "model_path": "/unused.gguf", "acceleration": "cpu"}, False),
    ("openai", {"model": "other-model"}, True),
    ("ollama", {"endpoint": "http://127.0.0.1:12345"}, True),
    ("gguf", {"backend": "ollama"}, True),
])
def test_model_allocation_identity_ignores_request_only_and_inactive_backend_settings(backend, changes, reallocated):
    config = replace(Config(), backend=backend)
    assert (provider_identity(config) != provider_identity(replace(config, **changes))) is reallocated
