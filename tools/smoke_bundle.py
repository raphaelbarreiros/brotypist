"""Exercise real local inference through the frozen CLI, not source imports."""
from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]


def main() -> int:
    if sys.platform == "darwin":
        executable = ROOT / "dist" / "Inklow.app" / "Contents" / "MacOS" / "Inklow"
    elif sys.platform == "win32":
        executable = ROOT / "dist" / "Inklow" / "Inklow-cli.exe"
    else:
        executable = ROOT / "dist" / "Inklow" / "Inklow"
    if not executable.is_file():
        raise RuntimeError(f"Frozen executable missing: {executable}")
    environment = dict(os.environ, QT_QPA_PLATFORM="offscreen")
    result = subprocess.run(
        [str(executable), "--editor-only", "--no-tray", "--acceleration", "cpu", "complete"],
        input="Thank you for your", text=True, capture_output=True,
        env=environment, timeout=120, check=True,
    )
    if not result.stdout.strip():
        raise RuntimeError(f"Frozen inference did not produce a continuation: {result.stderr}")
    print(f"Frozen local inference exited {result.returncode}: {executable}")
    print(result.stdout)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
