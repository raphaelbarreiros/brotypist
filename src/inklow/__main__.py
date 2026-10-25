"""Menu utility launcher, local continuation CLI, and verified model management."""
from __future__ import annotations

import argparse
from dataclasses import replace
import json
import os
from pathlib import Path
import platform
import sys

from .config import config_path, load_config, save_config
from .providers import create_provider
from . import __version__
from .models import DEFAULT_PRESET, PRESETS, download_model, get_preset, is_installed


_PANES = ("welcome", "general", "models", "autocomplete", "languages", "privacy", "about")
def main(argv: list[str] | None = None) -> int:
    arguments = list(sys.argv[1:] if argv is None else argv)
    if arguments == ["_worker"]:
        from .process_worker import main as worker_main
        return worker_main()
    parser = argparse.ArgumentParser(prog="brotypist", description="Local-first predictive typing")
    parser.add_argument("--model-path", help="Use a GGUF model for this launch without changing settings")
    parser.add_argument("--version", action="version", version=f"BroTypist {__version__}")
    parser.add_argument("--background", action="store_true", help="Start in the menu bar/system tray when available")
    parser.add_argument("--no-tray", action="store_true", help="Open recoverable settings without a tray icon")
    parser.add_argument("--wayland-input-method", action="store_true",
                        help="Run as the KDE-selected input method using KWin's private socket")
    parser.add_argument("--paused", action="store_true", help="Start with global suggestions paused for this launch")
    parser.add_argument("--settings", action="store_true", help="Open Settings instead of remaining in the menu bar")
    parser.add_argument("--pane", choices=_PANES, help="Open a specific settings pane")
    parser.add_argument("--prompt-style", choices=["continuation", "writing"], help="Override the GGUF prompt profile")
    parser.add_argument("--acceleration", choices=["auto", "cpu"], help="Override local model acceleration for this launch")
    commands = parser.add_subparsers(dest="command")
    commands.add_parser("doctor", help="Show native integration and local model readiness")
    commands.add_parser("complete", help="Read text from stdin and print a local continuation")
    wayland = commands.add_parser("wayland", help="Explicit per-user KDE Wayland input-method setup")
    wayland.add_argument("action", choices=["install"], help="Install the selector entry without changing the selected input method")
    model = commands.add_parser("model", help="List or explicitly download verified writing models")
    model.add_argument("action", choices=["list", "download"])
    model.add_argument("--preset", choices=[preset.id for preset in PRESETS], default=DEFAULT_PRESET)
    model.add_argument("--no-select", action="store_true", help="Download without changing the selected model")
    args = parser.parse_args(arguments)
    try:
        if args.wayland_input_method:
            if not sys.platform.startswith("linux") or args.command:
                raise ValueError("The Wayland input-method mode is only for KDE application launches on Linux.")
            descriptor = os.environ.get("WAYLAND_SOCKET")
            if descriptor is None:
                raise ValueError("Select BroTypist in KDE Virtual Keyboard settings; KWin must supply the private input-method socket.")
            from .native.wayland import prepare_input_method
            prepare_input_method(int(descriptor))
        elif args.background and not args.command and not args.settings and not args.pane:
            from .system import wayland_input_method_selected
            if wayland_input_method_selected():
                return 0
        config = load_config()
        if args.model_path:
            known = next((p for p in PRESETS if p.filename == Path(args.model_path).name), None)
            config = replace(config, backend="gguf", model_path=args.model_path,
                             prompt_style=known.prompt_style if known else "continuation")
        if args.prompt_style:
            config = replace(config, prompt_style=args.prompt_style)
        if args.paused:
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
            print("Model downloaded; selection unchanged." if args.no_select else "Model selected. Restart BroTypist or select it in AI Models to reload.")
            return 0
        if args.command == "wayland":
            from .system import install_wayland_input_method
            path = install_wayland_input_method()
            print(f"Installed {path}\nQuit BroTypist, then select BroTypist in KDE System Settings → Keyboard → Virtual Keyboard.\nKDE starts the input-method instance; no desktop setting was changed automatically.")
            return 0
        if args.command == "doctor":
            from .native import create_bridge
            bridge = create_bridge()
            bridge.configure(config)
            try:
                print(json.dumps({"platform": platform.system(), "python": platform.python_version(),
                                  "settings": str(config_path()), "backend": config.backend,
                                  "version": __version__, "acceleration": config.acceleration,
                                  "shell": "AppKit" if sys.platform == "darwin" else "Qt menu utility",
                                  "idle_release_seconds": config.idle_release_seconds,
                                  "model_file_exists": Path(config.model_path).expanduser().is_file() if config.model_path else False,
                                  "native": bridge.status(), "global_enabled": config.global_enabled}, indent=2))
            finally:
                bridge.stop()
            return 0
        if args.command == "complete":
            prefix = sys.stdin.read()
            if not prefix.strip():
                raise ValueError("Provide text on stdin, e.g. printf 'The capital of France is' | brotypist complete")
            provider = create_provider(config)
            try:
                print(provider.complete(prefix, ""), end="", flush=True)
            finally:
                provider.close()
            return 0
        from .desktop import launch
        return launch(config, background=args.background, no_tray=args.no_tray,
                      pane=args.pane or ("general" if args.settings else None))
    except (ValueError, RuntimeError, OSError) as error:
        if sys.stderr is not None:
            print(f"BroTypist: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
