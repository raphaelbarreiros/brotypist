"""User settings only. Typed text and inference results are never persisted."""
from __future__ import annotations

from dataclasses import asdict, dataclass, fields
import ipaddress
import json
import os
from pathlib import Path
import tempfile
from urllib.parse import urlsplit

from platformdirs import user_config_path


@dataclass
class Config:
    backend: str = "gguf"
    model_path: str = ""
    endpoint: str = "http://127.0.0.1:11434"
    model: str = "qwen3:0.6b"
    debounce_ms: int = 400
    min_chars: int = 8
    max_tokens: int = 32
    context_chars: int = 2000
    enabled: bool = True
    global_enabled: bool = False
    excluded_apps: tuple[str, ...] = (
        "terminal", "iterm", "powershell", "cmd.exe", "windowsterminal",
        "konsole", "gnome-terminal", "kitty", "wezterm", "alacritty",
        "1password", "bitwarden", "keepass", "keychain", "lastpass",
    )

    def validate(self) -> None:
        if self.backend not in {"gguf", "ollama", "openai"}:
            raise ValueError("Backend must be gguf, ollama, or openai.")
        for name, low, high in (("debounce_ms", 100, 5000), ("min_chars", 1, 1000),
                                ("max_tokens", 1, 128), ("context_chars", 64, 8000)):
            value = getattr(self, name)
            if type(value) is not int or not low <= value <= high:
                raise ValueError(f"{name} must be an integer between {low} and {high}.")
        for name in ("enabled", "global_enabled"):
            if type(getattr(self, name)) is not bool:
                raise ValueError(f"{name} must be a boolean.")
        for name in ("model_path", "endpoint", "model"):
            if not isinstance(getattr(self, name), str):
                raise ValueError(f"{name} must be text.")
        if not isinstance(self.excluded_apps, (tuple, list)) or not all(
                isinstance(x, str) and x.strip() for x in self.excluded_apps):
            raise ValueError("Excluded apps must be a list of non-empty app identifiers.")
        if self.backend != "gguf":
            local_endpoint(self.endpoint)
            if not self.model.strip():
                raise ValueError("Set a model name for the local inference server.")


def local_endpoint(endpoint: str) -> str:
    """Only literal loopback addresses; no DNS, proxies, auth or redirects."""
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
    return user_config_path("inklow", appauthor=False) / "settings.json"


def load_config() -> Config:
    path = config_path()
    if not path.exists():
        return Config()
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise ValueError(f"Settings must be an object: {path}")
    allowed = {field.name for field in fields(Config)}
    config = Config(**{key: value for key, value in data.items() if key in allowed})
    config.validate()
    config.excluded_apps = tuple(config.excluded_apps)
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
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
