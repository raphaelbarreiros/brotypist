"""Exercise real local inference inside the frozen CLI and its isolated worker."""
from __future__ import annotations

import argparse
from dataclasses import asdict
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import threading
import time
import unicodedata

ROOT = Path(__file__).resolve().parents[1]


def worker_completion(command: list[str], environment: dict) -> dict:
    from inklow.config import load_config
    from dataclasses import replace
    config = replace(load_config(), acceleration="cpu", global_enabled=False)
    process = subprocess.Popen(
        command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, text=True, encoding="utf-8", env=environment,
        creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0,
    )
    print(f"Started actual worker pid={process.pid}", flush=True)
    responses = queue.Queue()
    def read_responses():
        try:
            while line := process.stdout.readline(256 * 1024 + 1):
                if len(line.encode("utf-8")) > 256 * 1024:
                    responses.put(RuntimeError("Worker emitted an oversized frame"))
                    return
                responses.put(json.loads(line))
        except Exception as error:
            responses.put(error)
        finally:
            responses.put(None)
    threading.Thread(target=read_responses, daemon=True).start()
    final_text, updates, last_phase = "", 0, "startup"
    started = time.monotonic()
    try:
        process.stdin.write(json.dumps({
            "type": "generate", "id": 1, "config": asdict(config),
            "prefix": "Thank you for your", "suffix": "",
        }) + "\n")
        process.stdin.flush()
        while True:
            try:
                event = responses.get(timeout=max(.1, 120 - (time.monotonic() - started)))
            except queue.Empty as error:
                raise RuntimeError(f"Worker protocol timed out in {last_phase}; exit={process.poll()}") from error
            if event is None:
                raise RuntimeError("Frozen worker exited before completing the request")
            if isinstance(event, Exception):
                raise event
            if event.get("id") != 1:
                raise RuntimeError("Frozen worker returned a mismatched request")
            phase = event.get("type", "unknown")
            if phase != last_phase:
                print(f"Worker phase: {phase}", flush=True)
                last_phase = phase
            if phase == "ready":
                process.stdin.write(json.dumps({"type": "proceed", "id": 1}) + "\n")
                process.stdin.flush()
            if event["type"] == "error":
                raise RuntimeError(event.get("message", "Frozen inference failed"))
            if event["type"] == "result":
                text = event["text"]
                if any(unicodedata.category(c) == "Cc" for c in text):
                    raise RuntimeError("Worker produced text that could navigate a form")
                final_text = text
                updates += 1
            if event["type"] == "done":
                break
        if not any(c.isalnum() for c in final_text):
            raise RuntimeError("The actual frozen model did not produce an insertable continuation")
        process.stdin.write('{"type":"stop"}\n')
        process.stdin.flush()
        process.stdin.close()
        process.wait(timeout=15)
        if process.returncode != 0:
            raise RuntimeError(f"Frozen worker exited with {process.returncode}")
        return {"worker_exit_code": process.returncode, "stream_updates": updates,
                "suggestion": final_text, "worker_elapsed_ms": round((time.monotonic() - started) * 1000, 2)}
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=10)
        process.stdout.close()
        process.stderr.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-worker", action="store_true", help="Exercise the actual source worker before freezing")
    args = parser.parse_args()
    environment = dict(os.environ, QT_QPA_PLATFORM="offscreen")
    if args.source_worker:
        receipt = worker_completion([sys.executable, "-m", "inklow", "_worker"], environment)
        print(json.dumps(receipt, ensure_ascii=False, indent=2))
        return 0
    if sys.platform == "darwin":
        executable = ROOT / "dist/Inklow.app/Contents/MacOS/Inklow"
    elif sys.platform == "win32":
        executable = ROOT / "dist/Inklow/Inklow-cli.exe"
    else:
        executable = ROOT / "dist/Inklow/Inklow"
    if not executable.is_file():
        raise RuntimeError(f"Frozen executable missing: {executable}")
    environment = dict(os.environ, QT_QPA_PLATFORM="offscreen")
    result = subprocess.run(
        [str(executable), "--acceleration", "cpu", "complete"],
        input="Thank you for your", text=True, encoding="utf-8", capture_output=True,
        env=environment, timeout=120, check=True,
    )
    if not any(c.isalnum() for c in result.stdout):
        raise RuntimeError("Frozen CLI did not produce a continuation")
    receipt = {"executable": str(executable), "cli_exit_code": result.returncode,
               "cli_suggestion": result.stdout, **worker_completion([str(executable), "_worker"], environment)}
    (ROOT / "worker-smoke.json").write_text(json.dumps(receipt, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(receipt, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
