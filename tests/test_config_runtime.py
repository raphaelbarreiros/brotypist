"""Strict menu preferences and isolated settings migration, without draft persistence."""
from dataclasses import asdict, replace
import json

import pytest

from inklow.config import Config, config_path, load_config, save_config


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
