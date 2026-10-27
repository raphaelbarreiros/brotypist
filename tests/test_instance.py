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


def test_login_entry_distinguishes_compositor_owned_startup_from_user_launch(monkeypatch, tmp_path):
    monkeypatch.delenv("INKLOW_HOME", raising=False)
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))
    monkeypatch.setattr(system.sys, "platform", "linux")
    system.set_launch_at_login(True)
    entry = system._autostart_path().read_text(encoding="utf-8")
    assert "--login-startup" in entry
    assert "--background" in entry
    assert "X-Inklow-Owned=true" in entry
    assert "--login-startup" not in system._launch_arguments()


def test_isolated_profile_does_not_inspect_live_kwin_selection(monkeypatch, tmp_path):
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    monkeypatch.setenv("XDG_SESSION_TYPE", "wayland")
    monkeypatch.setattr(system.sys, "platform", "linux")
    monkeypatch.setattr(system.shutil, "which", lambda *_: pytest.fail("Isolated profile queried real KDE configuration"))
    assert not system.wayland_input_method_selected()


@pytest.mark.parametrize("arguments", [
    ["--background"], ["--background", "--no-tray"], ["--background", "--settings"],
])
def test_explicit_background_launch_is_not_silently_suppressed_by_kde(monkeypatch, tmp_path, arguments):
    from inklow import __main__ as cli, desktop
    from inklow.config import Config
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path))
    monkeypatch.setattr(system, "wayland_input_method_selected",
                        lambda: pytest.fail("A user launch must not depend on KDE's selected input method"))
    monkeypatch.setattr(cli, "load_config", lambda: Config())
    launches = []
    monkeypatch.setattr(desktop, "launch", lambda config, **options: launches.append(options) or 0)
    assert cli.main(arguments) == 0
    assert len(launches) == 1


def test_login_startup_defers_only_to_explicitly_selected_input_method(monkeypatch, tmp_path):
    from inklow import __main__ as cli, desktop
    from inklow.config import Config
    monkeypatch.setattr(cli, "load_config", lambda: Config())
    launches = []
    monkeypatch.setattr(desktop, "launch", lambda config, **options: launches.append(options) or 0)
    monkeypatch.setattr(system, "wayland_input_method_selected", lambda: True)
    assert cli.main(["--background", "--login-startup"]) == 0
    assert not launches
    monkeypatch.setattr(system, "wayland_input_method_selected", lambda: False)
    assert cli.main(["--background", "--login-startup"]) == 0
    assert len(launches) == 1
