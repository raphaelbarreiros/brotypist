"""Launch the real platform settings shell using an isolated, paused profile."""
from __future__ import annotations

import os
from pathlib import Path
import sys
import tempfile


def run_qt_smoke(config) -> int:
    """Observe Qt settings after two seconds, then use the shell's Quit path."""
    from PySide6.QtCore import QTimer
    from PySide6.QtWidgets import QApplication
    from inklow.qt_ui import launch
    app = QApplication([])

    def finish():
        """Fail the process on missing/dirty settings rather than a timer exception."""
        shell = getattr(app, "_inklow_menu_agent", None)
        if shell is None or shell.settings is None or not shell.settings.isVisible():
            print("Portable settings did not become visible", file=sys.stderr, flush=True)
            os._exit(2)
        if shell.settings._changes():
            print("Untouched portable settings unexpectedly became dirty", file=sys.stderr, flush=True)
            os._exit(3)
        print("Portable settings visible; quitting through the application action", flush=True)
        # QApplication.quit() closes the no-tray window through its intentional
        # confirmation guard. Use the same clean Quit path as the actual menu.
        shell.quit()

    QTimer.singleShot(2000, finish)
    return launch(config, no_tray=True, pane="general")


def main() -> int:
    """Run the platform event loop with a temporary profile and predictions paused."""
    with tempfile.TemporaryDirectory(prefix="inklow-settings-smoke-") as root:
        os.environ["INKLOW_HOME"] = root
        from inklow.config import Config
        from inklow.desktop import launch
        config = Config(global_enabled=False, setup_completed=True)
        if sys.platform == "darwin":
            import AppKit
            from Foundation import NSObject, NSTimer
            class FinishSmoke(NSObject):
                def finish_(self, timer):
                    """Check idle AppKit visibility/import boundaries before normal quit."""
                    app = AppKit.NSApplication.sharedApplication()
                    delegate = app.delegate()
                    if delegate is None or delegate.window is None or not delegate.window.isVisible():
                        print("Native settings did not become visible", file=sys.stderr, flush=True)
                        os._exit(2)
                    if "llama_cpp" in sys.modules or any(name.startswith("PySide6") for name in sys.modules):
                        print("Idle native settings loaded the model or Qt", file=sys.stderr, flush=True)
                        os._exit(3)
                    print("Native AppKit settings visible; no model/Qt loaded", flush=True)
                    app.terminate_(None)
            stopper = FinishSmoke.alloc().init()
            timer = NSTimer.scheduledTimerWithTimeInterval_target_selector_userInfo_repeats_(2.0, stopper, "finish:", None, False)
            return launch(config, no_tray=True, pane="general")
        return run_qt_smoke(config)


if __name__ == "__main__":
    raise SystemExit(main())
