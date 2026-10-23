"""Desktop launcher, local continuation CLI, and verified model management."""
from __future__ import annotations

import argparse
from dataclasses import replace
import json
from pathlib import Path
import platform
import sys

from .config import config_path, load_config, save_config
from .providers import create_provider
from . import __version__
from .models import DEFAULT_PRESET, PRESETS, download_model, get_preset, is_installed



def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="inklow", description="Local-first predictive typing")
    parser.add_argument("--model-path", help="Use a GGUF model for this launch without changing settings")
    parser.add_argument("--version", action="version", version=f"Inklow {__version__}")
    parser.add_argument("--background", action="store_true", help="Start in the menu bar/system tray when available")
    parser.add_argument("--no-tray", action="store_true", help="Run as a foreground application without a tray icon")
    parser.add_argument("--editor-only", action="store_true", help="Disable system-wide typing for this launch only")
    parser.add_argument("--acceleration", choices=["auto", "cpu"], help="Override local model acceleration for this launch")
    commands = parser.add_subparsers(dest="command")
    commands.add_parser("doctor", help="Show native integration and local model readiness")
    commands.add_parser("complete", help="Read text from stdin and print a local continuation")
    model = commands.add_parser("model", help="List or explicitly download verified writing models")
    model.add_argument("action", choices=["list", "download"])
    model.add_argument("--preset", choices=[preset.id for preset in PRESETS], default=DEFAULT_PRESET)
    model.add_argument("--no-select", action="store_true", help="Download without changing the selected model")
    args = parser.parse_args(argv)
    try:
        config = load_config()
        if args.model_path:
            config = replace(config, backend="gguf", model_path=args.model_path)
        if args.editor_only:
            config = replace(config, global_enabled=False)
        if args.acceleration:
            config = replace(config, acceleration=args.acceleration)
        if args.command == "model":
            if args.action == "list":
                for preset in PRESETS:
                    state = "installed" if is_installed(preset) else "not downloaded"
                    print(f"{preset.id}: {preset.label} — {preset.size_bytes / 1_000_000:.0f} MB, {state}")
                    print(f"  {preset.description}")
                return 0
            preset = get_preset(args.preset)
            path = download_model(preset.id)
            if not args.no_select:
                save_config(replace(config, backend="gguf", model_path=str(path), prompt_style=preset.prompt_style))
            print(f"Verified model: {preset.label}\n{path}")
            print("Model downloaded; selection unchanged." if args.no_select else "Model selected. Restart Inklow or select it in Models to reload.")
            return 0
        if args.command == "doctor":
            from .native import create_bridge
            bridge = create_bridge()
            try:
                print(json.dumps({"platform": platform.system(), "python": platform.python_version(),
                                  "settings": str(config_path()), "backend": config.backend,
                                  "version": __version__, "acceleration": config.acceleration,
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
        return launch(config, background=args.background, no_tray=args.no_tray)
    except (ValueError, RuntimeError, OSError) as error:
        print(f"Inklow: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
