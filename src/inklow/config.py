"""Preferences only; transient writing and model output never belong in settings."""
from __future__ import annotations

from dataclasses import asdict, dataclass, field, fields
import ipaddress
import json
import os
from pathlib import Path
import tempfile
from urllib.parse import urlsplit

from platformdirs import user_config_path


DEFAULT_SHORTCUTS = {
    "word": "Tab", "all": "AboveTab", "force": "Ctrl+AboveTab",
    "app_toggle": "Ctrl+Alt+Super+AboveTab", "toggle": "",
    "alternatives": "", "synonyms": "",
}


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
    battery_smaller_model: bool = False
    battery_shorter: bool = True
    app_rules: dict[str, dict] = field(default_factory=dict)

    def validate(self) -> None:
        enums = {
            "backend": {"gguf", "ollama", "openai"}, "acceleration": {"auto", "cpu"},
            "prompt_style": {"continuation", "writing"},
            "completion_length": {"short", "medium", "long"},
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
        for name in ("model_path", "endpoint", "model", "custom_instructions"):
            if not isinstance(getattr(self, name), str):
                raise ValueError(f"{name} must be text.")
        if len(self.custom_instructions) > 8000:
            raise ValueError("Custom instructions cannot exceed 8000 characters.")
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
            if not isinstance(rule, dict) or set(rule) - {"enabled", "tab_enabled", "record_history", "custom_instructions"}:
                raise ValueError("Invalid per-app rule fields.")
            for name, value in rule.items():
                if name == "custom_instructions":
                    if not isinstance(value, str) or len(value) > 8000:
                        raise ValueError("Per-app instructions must be text up to 8000 characters.")
                elif type(value) is not bool:
                    raise ValueError(f"Rule {name} must be a boolean.")
            if key.startswith("domain:"):
                host = key[7:].strip().rstrip(".").casefold()
                try:
                    host = host.encode("idna").decode("ascii")
                except UnicodeError as error:
                    raise ValueError("Invalid domain rule.") from error
                if not host or any(not (char.isalnum() or char in ".-") for char in host):
                    raise ValueError("Domain rules must contain a hostname, not a URL or path.")
                key = "domain:" + host
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


def load_config() -> Config:
    path = config_path()
    if not path.exists():
        return Config()
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise ValueError(f"Settings must be an object: {path}")
    allowed = {item.name for item in fields(Config)}
    # Legacy editor keep_running is deliberately discarded. Existing explicit
    # global_enabled is retained; only new/missing values use the new default.
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
