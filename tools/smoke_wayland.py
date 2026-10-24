#!/usr/bin/env python3
"""Run an owned, virtual KWin only; never connect to the user's desktop.

Requires an already-built native library and the application's Linux Python
venv. No compilation, downloads, installs, desktop selection or host settings.
Exit 0 means all required observations passed; 1 is failure; 2 is incomplete
proof (for example a compositor denying the fixture-only fake-input protocol).
The JSON report explicitly separates client-generated images from scene proof.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import time


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model-path", required=True, type=Path,
                        help="Existing cached GGUF; used only as a launch override")
    parser.add_argument("--output", required=True, type=Path,
                        help="New evidence directory (must not already exist)")
    parser.add_argument("--timeout", type=int, default=240,
                        help="Whole process-group deadline, 30..600 seconds")
    args = parser.parse_args()
    if sys.platform != "linux":
        parser.error("Run on the Linux test host with its application venv")
    if not 30 <= args.timeout <= 600:
        parser.error("--timeout must be 30..600 seconds")
    model = args.model_path.expanduser().resolve(strict=True)
    if not model.is_file() or model.suffix.lower() != ".gguf":
        parser.error("--model-path must identify an existing GGUF file")
    commands = {name: shutil.which(name) for name in ("dbus-run-session", "kwin_wayland")}
    if not all(commands.values()):
        parser.error("dbus-run-session and kwin_wayland must already be installed")
    repo = Path(__file__).resolve().parents[1]
    if not (repo / "src/inklow/native/libinklow_wayland.so").is_file():
        parser.error("Parent must compile src/inklow/native/libinklow_wayland.so first")
    output = args.output.expanduser().absolute()
    output.mkdir(parents=True, exist_ok=False, mode=0o700)
    output.chmod(0o700)
    report = {"schema": 1, "result": "failed", "checks": [], "limits": [],
              "isolation": {"owned_virtual_kwin": True, "private_bus": True,
                            "host_settings_changed": False},
              "image_semantics": "panel and widget PNGs are client-generated pixels, not a compositor screenshot"}
    process = None
    started = time.monotonic()
    def interrupted(number, _frame):
        raise InterruptedError(f"Smoke interrupted by signal {number}")

    signals = (signal.SIGINT, signal.SIGTERM)
    handlers = {number: signal.signal(number, interrupted) for number in signals}
    try:
        with tempfile.TemporaryDirectory(prefix="inklow-wayland-smoke-") as temporary:
            root = Path(temporary)
            root.chmod(0o700)
            # An allowlist, not an inherited session with a few known variables
            # removed: no DISPLAY, session bus, AT-SPI, WAYLAND_SOCKET or proxies.
            env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                   "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8",
                   "PYTHONPATH": str(repo / "src"), "PYTHONUNBUFFERED": "1",
                   "XDG_SESSION_TYPE": "wayland", "XDG_CURRENT_DESKTOP": "KDE",
                   "QT_QPA_PLATFORM": "wayland", "QT_IM_MODULE": "wayland",
                   "QT_WAYLAND_TEXT_INPUT_PROTOCOL": "zwp_text_input_v3",
                   "QT_QUICK_BACKEND": "software", "QT_OPENGL": "software",
                   "LIBGL_ALWAYS_SOFTWARE": "1", "GALLIUM_DRIVER": "llvmpipe",
                   "KWIN_COMPOSE": "O2", "KWIN_DRM_DEVICES": str(root / "no-hardware-gpu"),
                   "DBUS_SYSTEM_BUS_ADDRESS": "unix:path=" + str(root / "no-system-bus"),
                   "QT_LINUX_ACCESSIBILITY_ALWAYS_ON": "1",
                   "QT_ACCESSIBILITY": "1", "NO_AT_BRIDGE": "0",
                   "XDG_DATA_DIRS": "/usr/local/share:/usr/share",
                   "INKLOW_SMOKE_ROOT": str(root), "INKLOW_SMOKE_OUTPUT": str(output),
                   "INKLOW_SMOKE_TOKEN": secrets.token_hex(24),
                   "INKLOW_SMOKE_MODEL": str(model), "INKLOW_SMOKE_TIMEOUT": str(args.timeout),
                   "INKLOW_SMOKE_KWIN": commands["kwin_wayland"]}
            for key, name in (("HOME", "home"), ("XDG_RUNTIME_DIR", "runtime"),
                              ("XDG_CONFIG_HOME", "config"), ("XDG_CONFIG_DIRS", "config-dirs"),
                              ("XDG_DATA_HOME", "data"), ("XDG_CACHE_HOME", "cache"),
                              ("INKLOW_HOME", "inklow")):
                directory = root / name
                directory.mkdir(mode=0o700)
                env[key] = str(directory)
            (root / "owner.json").write_text(json.dumps({"token": env["INKLOW_SMOKE_TOKEN"],
                                                         "uid": os.getuid()}))
            # Only this disposable profile is changed; model selection remains
            # unset here too. The actual entrypoint receives --model-path.
            settings = {"setup_completed": True, "acceleration": "cpu", "min_chars": 8,
                        "max_tokens": 48, "debounce_ms": 500, "idle_release_seconds": 0,
                        "punctuation_mode": "always", "trailing_space": True,
                        "clipboard_context": False, "screen_context": False,
                        "record_history": False, "statistics_enabled": False,
                        "battery_on_demand": False, "emoji_enabled": False,
                        "synonyms_enabled": False}
            (root / "inklow/settings.json").write_text(json.dumps(settings))
            fixture = Path(__file__).with_name("wayland_smoke_fixture.py")
            command = [commands["dbus-run-session"], "--", sys.executable, str(fixture), "session"]
            report["command"] = ["dbus-run-session", "--", "<venv-python>", "wayland_smoke_fixture.py", "session"]
            with (output / "session.log").open("wb") as log:
                process = subprocess.Popen(command, env=env, cwd=repo, stdout=log,
                                           stderr=subprocess.STDOUT, start_new_session=True)
                try:
                    report["session_returncode"] = process.wait(timeout=args.timeout)
                except subprocess.TimeoutExpired:
                    report["limits"].append("Whole isolated session exceeded its finite deadline")
                finally:
                    # KWin, its input method, target, worker and activated bus
                    # services all inherit this new group. Clean even if the
                    # group leader has already exited with descendants alive.
                    terminate_group(process.pid)
            result_path = output / "target-report.json"
            if result_path.is_file():
                target = json.loads(result_path.read_text())
                report.update(target)
            if report.get("session_returncode") != 0 and report["result"] == "passed":
                report["result"] = "failed"
                report["limits"].append("Owned compositor/session did not exit successfully")
    except (OSError, ValueError, RuntimeError) as error:
        report["limits"].append(f"{type(error).__name__}: {error}")
    finally:
        for number in signals:
            signal.signal(number, signal.SIG_IGN)
        if process is not None:
            terminate_group(process.pid)
        report["elapsed_seconds"] = round(time.monotonic() - started, 3)
        (output / "report.json").write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n")
        for number, handler in handlers.items():
            signal.signal(number, handler)
    print(json.dumps({"result": report["result"], "report": str(output / "report.json")}))
    return {"passed": 0, "incomplete": 2}.get(report["result"], 1)


def terminate_group(pid: int) -> None:
    try:
        os.killpg(pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    deadline = time.monotonic() + 2
    while time.monotonic() < deadline:
        try:
            os.killpg(pid, 0)
        except ProcessLookupError:
            return
        time.sleep(0.05)
    try:
        os.killpg(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


if __name__ == "__main__":
    raise SystemExit(main())
