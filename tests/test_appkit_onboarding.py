"""Readiness gates for native onboarding; never start inference or downloads."""
from dataclasses import replace
from pathlib import Path
from types import SimpleNamespace

import pytest

A = pytest.importorskip("AppKit")
from inklow import appkit_ui, appkit_welcome
from inklow.config import Config


@pytest.fixture
def onboarding(monkeypatch, tmp_path):
    A.NSApplication.sharedApplication()
    monkeypatch.setattr(appkit_welcome, "importlib", SimpleNamespace(import_module=lambda name: object()))
    current = tmp_path / "current.gguf"
    current.write_bytes(b"GGUF")
    candidate = tmp_path / "candidate.gguf"
    inventory = [
        {"id": "current", "label": "Current model", "path": str(current), "installed": True,
         "size_bytes": 4, "description": "Installed model", "availability": ""},
        {"id": "candidate", "label": "Another model", "path": str(candidate), "installed": False,
         "size_bytes": 4, "description": "Another local model", "availability": ""},
    ]
    download = {}
    shell = appkit_ui.BroTypistApplication.alloc().init()
    shell.model_selection = None
    shell.controller = SimpleNamespace(
        config=Config(model_path=str(current), model_id="current"),
        models=lambda: inventory,
        status=lambda: {"download": download},
    )
    welcome = appkit_welcome.WelcomeWindow.alloc().init().configure(shell)
    welcome.step = 3
    welcome.render()
    try:
        yield welcome, shell, inventory, download
    finally:
        welcome.window.setDelegate_(None)
        welcome.window.close()


def test_pending_uninstalled_choice_cannot_continue_using_previous_model(onboarding):
    welcome, shell, _, _ = onboarding
    assert welcome.primary.isEnabled()
    shell.model_selection = "candidate"
    welcome.render()
    assert not welcome.primary.isEnabled()
    welcome.advance()
    assert welcome.step == 3


def test_verified_download_does_not_continue_until_that_model_is_current(onboarding):
    welcome, shell, inventory, download = onboarding
    shell.model_selection = "candidate"
    welcome.render()
    candidate = inventory[1]
    Path(candidate["path"]).write_bytes(b"GGUF")
    candidate["installed"] = True
    download.update(id="candidate", state="complete", path=candidate["path"], received=4, total=4)
    welcome.refresh_model()
    assert not welcome.primary.isEnabled()
    shell.model_selection = None
    shell.controller.config = replace(shell.controller.config, model_path=candidate["path"], model_id="candidate")
    welcome.refresh_model()
    assert welcome.primary.isEnabled()


def test_continue_rechecks_file_even_before_next_status_tick(onboarding):
    welcome, shell, _, _ = onboarding
    assert welcome.primary.isEnabled()
    Path(shell.controller.config.model_path).unlink()
    welcome.advance()
    assert welcome.step == 3
    assert not welcome.primary.isEnabled()


def test_existing_model_file_cannot_bypass_missing_local_inference_support(onboarding, monkeypatch):
    welcome, _, _, _ = onboarding

    def unavailable(name):
        raise ImportError("Native inference library is missing")

    monkeypatch.setattr(appkit_welcome, "importlib", SimpleNamespace(import_module=unavailable))
    welcome.render()
    assert not welcome.primary.isEnabled()
    welcome.advance()
    assert welcome.step == 3
