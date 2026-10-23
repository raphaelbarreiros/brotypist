import os
from pathlib import Path
import subprocess
import sys

import pytest

from inklow.instance import instance_lock
from inklow import system


def test_another_process_cannot_own_the_profile_until_current_instance_releases(monkeypatch, tmp_path):
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    command = [sys.executable, "-c", "from inklow.instance import instance_lock\nwith instance_lock() as owned: print(owned)"]
    with instance_lock() as acquired:
        assert acquired
        assert subprocess.check_output(command, text=True, timeout=10).strip() == "False"
    assert subprocess.check_output(command, text=True, timeout=10).strip() == "True"


def test_startup_does_not_overwrite_an_unowned_entry(monkeypatch, tmp_path):
    monkeypatch.delenv("INKLOW_HOME", raising=False)
    monkeypatch.setattr(system.sys, "platform", "linux")
    target = tmp_path / "Inklow.desktop"
    original = b"[Desktop Entry]\nExec=another-application\n"
    target.write_bytes(original)
    monkeypatch.setattr(system, "_autostart_path", lambda: target)
    with pytest.raises(RuntimeError):
        system.set_launch_at_login(True)
    assert target.read_bytes() == original


def test_isolated_profiles_cannot_install_real_login_items(monkeypatch, tmp_path):
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    with pytest.raises(RuntimeError):
        system.set_launch_at_login(True)
    assert list(tmp_path.iterdir()) == []
