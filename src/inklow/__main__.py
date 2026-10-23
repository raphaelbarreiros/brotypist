"""Desktop launcher, local completion CLI, and verified starter-model download."""
from __future__ import annotations

import argparse
from dataclasses import replace
import hashlib
import json
import os
from pathlib import Path
import platform
import sys
import tempfile
import urllib.request

from platformdirs import user_cache_path

from .config import config_path, load_config, save_config
from .providers import create_provider

MODEL_NAME = "SmolLM2-135M-Q4_K_M.gguf"
MODEL_REVISION = "1076b85d135017971845b19b8d07df1479155988"
MODEL_URL = ("https://huggingface.co/ggml-org/SmolLM2-135M-GGUF/resolve/"
             + MODEL_REVISION + "/" + MODEL_NAME)
MODEL_SHA256 = "e3131339bf4e8065265593d4fd8f7bb7ff2d3abff1edb5618aa1197b89cad9f5"
MODEL_BYTES = 101016128


def model_digest(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def download_model() -> Path:
    """Explicit download only. Immutable revision, size and SHA-256 checked."""
    target = user_cache_path("inklow", appauthor=False) / "models" / MODEL_NAME
    target.parent.mkdir(parents=True, exist_ok=True)
    if target.is_file() and model_digest(target) == MODEL_SHA256:
        return target
    fd, temporary = tempfile.mkstemp(prefix=".download-", dir=target.parent)
    try:
        count = 0
        digest = hashlib.sha256()
        with os.fdopen(fd, "wb") as destination:
            request = urllib.request.Request(MODEL_URL, headers={"User-Agent": "inklow/0.1"})
            with urllib.request.urlopen(request, timeout=60) as response:
                if urllib.parse.urlsplit(response.url).scheme != "https":
                    raise RuntimeError("Refusing a non-HTTPS model download.")
                while chunk := response.read(1024 * 1024):
                    count += len(chunk)
                    if count > MODEL_BYTES:
                        raise RuntimeError("Model download exceeds its expected size.")
                    digest.update(chunk)
                    destination.write(chunk)
        if count != MODEL_BYTES or digest.hexdigest() != MODEL_SHA256:
            raise RuntimeError("Model integrity verification failed; existing model was not replaced.")
        os.replace(temporary, target)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    return target


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="inklow", description="Local-first predictive typing")
    parser.add_argument("--model-path", help="Use a GGUF model for this launch without changing settings")
    commands = parser.add_subparsers(dest="command")
    commands.add_parser("doctor", help="Show native integration and local model readiness")
    commands.add_parser("complete", help="Read text from stdin and print a local continuation")
    model = commands.add_parser("model", help="Manage the starter model (Apache-2.0)")
    model.add_argument("action", choices=["download"])
    args = parser.parse_args(argv)
    try:
        config = load_config()
        if args.model_path:
            config = replace(config, backend="gguf", model_path=args.model_path)
        if args.command == "model":
            path = download_model()
            save_config(replace(config, backend="gguf", model_path=str(path)))
            print(f"Verified starter model: {path}")
            print("Settings updated. Run inklow to start writing. This small English model favors low memory over quality.")
            return 0
        if args.command == "doctor":
            from .native import create_bridge
            bridge = create_bridge()
            try:
                print(json.dumps({"platform": platform.system(), "python": platform.python_version(),
                                  "settings": str(config_path()), "backend": config.backend,
                                  "model_file_exists": Path(config.model_path).expanduser().is_file() if config.model_path else False,
                                  "native": bridge.status(), "global_enabled": config.global_enabled}, indent=2))
            finally:
                bridge.stop()
            return 0
        if args.command == "complete":
            prefix = sys.stdin.read()
            if not prefix.strip():
                raise ValueError("Provide text on stdin, e.g. printf 'The capital of France is' | inklow complete")
            provider = create_provider(config)
            try:
                print(provider.complete(prefix, ""), end="", flush=True)
            finally:
                provider.close()
            return 0
        from .desktop import launch
        return launch(config)
    except (ValueError, RuntimeError, OSError) as error:
        print(f"Inklow: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
