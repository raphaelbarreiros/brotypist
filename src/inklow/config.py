"""Preferences only; transient writing and model output never belong in settings."""
from __future__ import annotations

from dataclasses import asdict, dataclass, field, fields, replace
from functools import lru_cache
import ipaddress
import json
import locale
import os
from pathlib import Path
import sys
import tempfile
from urllib.parse import urlsplit

from platformdirs import user_config_path


DEFAULT_SHORTCUTS = {
    "word": "Tab", "all": "AboveTab", "force": "Ctrl+AboveTab",
    "app_toggle": "Ctrl+Alt+Super+AboveTab", "toggle": "",
    "alternatives": "", "synonyms": "",
}
RULE_FLAGS = frozenset({"enabled", "tab_enabled", "record_history", "improve_compatibility", "complete_in_code_editors"})
# English names keep the instruction readable by every local model when the
# platform cannot name a locale (non-macOS hosts or a missing Foundation bridge).
_LANGUAGES = {
    "ar": "Arabic", "ca": "Catalan", "cs": "Czech", "da": "Danish", "de": "German", "el": "Greek",
    "en": "English", "es": "Spanish", "fi": "Finnish", "fr": "French", "he": "Hebrew", "hi": "Hindi",
    "hu": "Hungarian", "id": "Indonesian", "it": "Italian", "ja": "Japanese", "ko": "Korean",
    "nb": "Norwegian Bokmål", "nl": "Dutch", "pl": "Polish", "pt": "Portuguese", "ro": "Romanian",
    "ru": "Russian", "sk": "Slovak", "sv": "Swedish", "th": "Thai", "tr": "Turkish",
    "uk": "Ukrainian", "vi": "Vietnamese", "zh": "Chinese",
}


@lru_cache(maxsize=1)
def _system_locale() -> str:
    """The user's language and region; never the C/POSIX process default."""
    if sys.platform == "darwin":
        try:
            from Foundation import NSLocale
            preferred = NSLocale.preferredLanguages()
            language = str(preferred[0]) if preferred is not None and len(preferred) else ""
            region = NSLocale.currentLocale().countryCode()
            language = language.replace("-", "_").split("_")[0]
            if language:
                return f"{language}_{region}" if region else language
        except Exception:
            pass
    for value in (os.environ.get("LC_ALL"), os.environ.get("LC_MESSAGES"), os.environ.get("LANG"), locale.getlocale()[0]):
        if value and value.split(".")[0] not in {"C", "POSIX"}:
            return value.split(".")[0].split("@")[0]
    return "en_US"


def _english_name(kind: str, code: str) -> str:
    if sys.platform == "darwin":
        try:
            from Foundation import NSLocale
            english = NSLocale.alloc().initWithLocaleIdentifier_("en")
            name = (english.localizedStringForLanguageCode_(code) if kind == "language"
                    else english.localizedStringForCountryCode_(code))
            if name:
                return str(name)
        except Exception:
            pass
    return _LANGUAGES.get(code, code) if kind == "language" else code


def default_instructions(locale_identifier: str | None = None) -> str:
    """Locale-seeded writing preferences shown in, and editable from, Settings."""
    parts = [part for part in (locale_identifier or _system_locale()).replace("-", "_").split("_") if part]
    language = parts[0].casefold() if parts and parts[0].isalpha() else "en"
    region = next((part.upper() for part in reversed(parts[1:])
                   if len(part) == 2 and part.isalpha() or len(part) == 3 and part.isdigit()), "")
    language_name = _english_name("language", language)
    region_name = _english_name("region", region) if region else ""
    if not region_name or region_name == region:
        # An unnamed region code ("BR", "419") would read as noise to a model.
        return (f"Write in {language_name} by default. "
                "If the surrounding text is in another language, continue in that language.")
    return (f"Write in {language_name} by default, using {region_name} spelling, vocabulary and punctuation, "
            f"and {region_name} conventions for dates, numbers and currency. "
            "If the surrounding text is in another language, continue in that language.")


def normalize_domain(host: str) -> str:
    """Canonical website host used by domain rules and domain-scoped history."""
    if not isinstance(host, str):
        raise ValueError("A domain must be text.")
    value = host.strip().rstrip(".").casefold()
    try:
        value = value.encode("idna").decode("ascii")
    except UnicodeError as error:
        raise ValueError("Invalid domain.") from error
    if not value or len(value) > 253 or any(not (char.isalnum() or char in ".-") for char in value):
        raise ValueError("Domains must contain a hostname, not a URL or path.")
    return value


def _preset_ids() -> frozenset[str]:
    from .models import PRESETS
    return frozenset(preset.id for preset in PRESETS)


def provider_identity(config) -> tuple:
    """Settings that actually select a model allocation, not request framing."""
    if config.backend == "gguf":
        return config.backend, config.model_path, config.acceleration
    return config.backend, config.endpoint, config.model


@dataclass
class Config:
    backend: str = "gguf"
    model_path: str = ""
    endpoint: str = "http://127.0.0.1:11434"
    model: str = "qwen3:0.6b"
    debounce_ms: int = 150
    min_chars: int = 8
    max_tokens: int = 24
    context_chars: int = 2000
    enabled: bool = True
    global_enabled: bool = True
    acceleration: str = "auto"
    prompt_style: str = "continuation"
    excluded_apps: tuple[str, ...] = (
        "terminal", "iterm", "powershell", "cmd.exe", "windowsterminal",
        "konsole", "gnome-terminal", "kitty", "wezterm", "alacritty",
        "1password", "bitwarden", "keepass", "keychain", "lastpass",
    )
    setup_completed: bool = False
    launch_at_login: bool = False
    show_accessory: bool = True
    complete_inside_text: bool = True
    completion_length: str = "medium"
    idle_release_seconds: int = 300
    trailing_space: bool = True
    punctuation_mode: str = "full"
    escape_behavior: str = "pause"
    escape_pause_seconds: int = 3
    shortcuts: dict[str, str] = field(default_factory=lambda: dict(DEFAULT_SHORTCUTS))
    clipboard_context: bool = False
    screen_context: bool = False
    screenshot_context: bool = False
    screenshot_appearance: bool = False
    terminal_completion: bool = False
    typo_suppression: bool = False
    typo_fixes: bool = False
    custom_instructions: str = ""
    record_history: bool = False
    record_unaccepted: bool = False
    personalization_strength: int = 50
    statistics_enabled: bool = True
    emoji_enabled: bool = True
    emoticons_enabled: bool = True
    emoji_skin_tone: int = 0
    emoji_include_neutral: bool = True
    emoji_gender: str = "neutral"
    alternatives_enabled: bool = False
    alternatives_mode: str = "shortcut"
    alternatives_delay_ms: int = 1500
    synonyms_enabled: bool = True
    synonyms_mode: str = "selection"
    synonyms_delay_ms: int = 1500
    battery_on_demand: bool = False
    battery_model: str = "same"
    battery_shorter: bool = True
    model_id: str = ""
    app_rules: dict[str, dict] = field(default_factory=dict)

    def validate(self) -> None:
        enums = {
            "backend": {"gguf", "ollama", "openai"}, "acceleration": {"auto", "cpu"},
            "prompt_style": {"continuation", "writing"},
            "completion_length": {"word", "short", "medium", "long"},
            "escape_behavior": {"pause", "app", "passthrough"},
            "emoji_gender": {"neutral", "female", "male"},
            "alternatives_mode": {"shortcut", "pause"},
            "punctuation_mode": {"full", "always", "never"},
            "synonyms_mode": {"selection", "shortcut"},
        }
        for name, choices in enums.items():
            if not isinstance(getattr(self, name), str) or getattr(self, name) not in choices:
                raise ValueError(f"{name} must be one of {', '.join(sorted(choices))}.")
        bounds = {
            "debounce_ms": (100, 5000), "min_chars": (1, 1000), "max_tokens": (1, 128),
            "context_chars": (64, 8000), "idle_release_seconds": (0, 3600),
            "escape_pause_seconds": (1, 3600), "personalization_strength": (0, 100),
            "emoji_skin_tone": (0, 5), "alternatives_delay_ms": (100, 10000),
            "synonyms_delay_ms": (100, 10000),
        }
        for name, (low, high) in bounds.items():
            if type(getattr(self, name)) is not int or not low <= getattr(self, name) <= high:
                raise ValueError(f"{name} must be an integer between {low} and {high}.")
        for item in fields(self):
            if item.type in (bool, "bool") and type(getattr(self, item.name)) is not bool:
                raise ValueError(f"{item.name} must be a boolean.")
        for name in ("model_path", "endpoint", "model", "custom_instructions", "battery_model", "model_id"):
            if not isinstance(getattr(self, name), str):
                raise ValueError(f"{name} must be text.")
        if len(self.custom_instructions) > 8000:
            raise ValueError("Custom instructions cannot exceed 8000 characters.")
        presets = _preset_ids()
        if self.battery_model != "same" and self.battery_model not in presets:
            raise ValueError("battery_model must be 'same' or an available model preset.")
        if self.model_id and self.model_id not in presets:
            raise ValueError("model_id must be empty or an available model preset.")
        if not isinstance(self.excluded_apps, (tuple, list)) or not all(
                isinstance(x, str) and x.strip() and len(x) <= 512 for x in self.excluded_apps):
            raise ValueError("Excluded apps must be a list of non-empty app identifiers.")
        self.excluded_apps = tuple(self.excluded_apps)
        if not isinstance(self.shortcuts, dict) or set(self.shortcuts) - set(DEFAULT_SHORTCUTS):
            raise ValueError("Shortcuts contain an unknown action.")
        from .shortcuts import normalize_shortcut
        normalized = dict(DEFAULT_SHORTCUTS)
        for action, shortcut in self.shortcuts.items():
            if not isinstance(shortcut, str):
                raise ValueError("Shortcuts must be text.")
            normalized[action] = normalize_shortcut(shortcut)
        active = [value for value in normalized.values() if value]
        if len(active) != len(set(active)):
            raise ValueError("Assign each active shortcut to only one action.")
        self.shortcuts = normalized
        if not isinstance(self.app_rules, dict) or len(self.app_rules) > 1000:
            raise ValueError("App rules must be an object with at most 1000 entries.")
        rules = {}
        for key, rule in self.app_rules.items():
            if not isinstance(key, str) or not key.startswith(("app:", "domain:")) or not key.split(":", 1)[1].strip():
                raise ValueError("Rules require app:EXACT_APP_ID or domain:host keys.")
            if not isinstance(rule, dict) or set(rule) - RULE_FLAGS - {"custom_instructions"}:
                raise ValueError("Invalid per-app rule fields.")
            for name, value in rule.items():
                if name == "custom_instructions":
                    if not isinstance(value, str) or len(value) > 8000:
                        raise ValueError("Per-app instructions must be text up to 8000 characters.")
                elif type(value) is not bool:
                    raise ValueError(f"Rule {name} must be a boolean.")
            if key.startswith("domain:"):
                try:
                    key = "domain:" + normalize_domain(key[7:])
                except ValueError as error:
                    raise ValueError("Domain rules must contain a hostname, not a URL or path.") from error
            rules[key] = dict(rule)
        self.app_rules = rules
        if self.backend != "gguf":
            local_endpoint(self.endpoint)
            if not self.model.strip():
                raise ValueError("Set a model name for the local inference server.")


def local_endpoint(endpoint: str) -> str:
    """Literal loopback only: no DNS, proxies, auth or redirects."""
    parsed = urlsplit(endpoint)
    try:
        loopback = ipaddress.ip_address(parsed.hostname or "").is_loopback
        port = parsed.port
    except ValueError:
        loopback, port = False, None
    if (parsed.scheme not in {"http", "https"} or not loopback
            or parsed.username is not None or parsed.password is not None
            or parsed.query or parsed.fragment or port == 0):
        raise ValueError("Use a literal loopback server URL, e.g. http://127.0.0.1:11434.")
    return endpoint.rstrip("/")


def config_path() -> Path:
    home = os.environ.get("INKLOW_HOME")
    return (Path(home).expanduser() if home else user_config_path("inklow", appauthor=False)) / "settings.json"


def reset_instructions(config: Config) -> Config:
    """Validated copy whose global instructions return to the locale default."""
    updated = replace(config, custom_instructions=default_instructions())
    updated.validate()
    return updated


def load_config() -> Config:
    path = config_path()
    if not path.exists():
        # Only a genuinely new install is seeded; saved preferences stay as-is.
        from .models import recommended_preset
        return Config(model_id=recommended_preset().id, custom_instructions=default_instructions())
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise ValueError(f"Settings must be an object: {path}")
    allowed = {item.name for item in fields(Config)}
    presets = _preset_ids()
    if "battery_model" not in data:
        # 0.3 battery_smaller_model=True meant the installed Tiny preset.
        data["battery_model"] = "tiny" if data.get("battery_smaller_model") is True and "tiny" in presets else "same"
    elif isinstance(data["battery_model"], str) and data["battery_model"] not in presets | {"same"}:
        data["battery_model"] = "same"  # A preset removed from the catalogue falls back to the selected model.
    if "model_id" not in data or (isinstance(data["model_id"], str) and data["model_id"]
                                  and data["model_id"] not in presets):
        # Derive from the existing file only; never redirect a saved model selection.
        from .models import PRESETS
        name = Path(str(data.get("model_path") or "")).name
        data["model_id"] = next((preset.id for preset in PRESETS
                                 if data.get("backend", "gguf") == "gguf" and name and preset.filename == name), "")
    # Legacy editor keep_running and battery_smaller_model are deliberately
    # discarded. Existing explicit global_enabled is retained; only new/missing
    # values use the new default.
    config = Config(**{key: value for key, value in data.items() if key in allowed})
    config.validate()
    return config


def save_config(config: Config) -> None:
    config.validate()
    path = config_path()
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd, temporary = tempfile.mkstemp(prefix=".settings-", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            json.dump(asdict(config), stream, indent=2)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
