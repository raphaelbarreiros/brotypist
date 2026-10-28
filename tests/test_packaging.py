"""Packaging safety scenarios: local://inklow-packaging-test-design.json."""
import json
from pathlib import Path
import runpy

import pytest

from inklow import resources

BUILD = runpy.run_path(str(Path(__file__).resolve().parents[1] / "tools" / "build_app.py"))


def mark(bundle, platform="linux"):
    path = BUILD["marker_path"](bundle, platform)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps({"application": resources.APP_ID, "platform": platform}))



def test_asset_lookup_uses_relocated_package_and_refuses_escape(tmp_path, monkeypatch):
    # R1/S2: frozen layout must not depend on cwd or source checkout.
    package = tmp_path / "frozen" / "_internal" / "inklow"
    assets = package / "assets"
    assets.mkdir(parents=True)
    (assets / "app.png").write_bytes(resources.asset_path("app.png").read_bytes())
    outside = package / "private.txt"
    outside.write_text("not an asset")
    monkeypatch.setattr(resources, "__file__", str(package / "resources.py"))
    monkeypatch.chdir(tmp_path)
    assert resources.asset_path("app.png") == assets / "app.png"
    for name in ("../private.txt", str(outside), "missing.png"):
        with pytest.raises(FileNotFoundError):
            resources.asset_path(name)


@pytest.fixture
def macos_relocated_assets(tmp_path, monkeypatch):
    contents = tmp_path / "BroTypist.app" / "Contents"
    package = contents / "Frameworks" / "inklow"
    assets = contents / "Resources" / "inklow" / "assets"
    package.mkdir(parents=True)
    assets.mkdir(parents=True)
    (assets / "app.png").write_bytes(resources.asset_path("app.png").read_bytes())
    outside = assets.parent / "private.txt"
    outside.write_bytes(b"not an application asset")
    try:
        (package / "assets").symlink_to(assets, target_is_directory=True)
        (assets / "escape.txt").symlink_to(outside)
    except OSError:
        pytest.skip("Creating symlinks is not permitted on this runner")
    monkeypatch.setattr(resources, "__file__", str(package / "resources.py"))
    monkeypatch.chdir(tmp_path)
    return assets, outside


def test_asset_lookup_accepts_macos_symlinked_resource_root(macos_relocated_assets):
    assets, _ = macos_relocated_assets
    assert resources.asset_path("app.png") == assets / "app.png"


@pytest.mark.parametrize("escape", ["parent", "absolute", "symlink", "missing"])
def test_symlinked_asset_root_still_refuses_escape(macos_relocated_assets, escape):
    _, outside = macos_relocated_assets
    name = {"parent": "../private.txt", "absolute": str(outside),
            "symlink": "escape.txt", "missing": "missing.png"}[escape]
    with pytest.raises(FileNotFoundError):
        resources.asset_path(name)


@pytest.mark.parametrize("platform", ["linux", "win32", "darwin"])
def test_replacement_refuses_unmarked_application_without_mutating_it(tmp_path, platform):
    # R3/S3: --replace is not permission to delete an unrelated application.
    destination = tmp_path / "Other.app"
    destination.mkdir()
    sentinel = destination / "user-document.txt"
    sentinel.write_bytes(b"keep this exact content")
    with pytest.raises(ValueError):
        BUILD["ensure_destination"](destination, replace=True, platform=platform)
    assert sentinel.read_bytes() == b"keep this exact content"
    assert list(destination.iterdir()) == [sentinel]


def test_replacement_refuses_symlink_even_if_target_is_owned(tmp_path):
    # R3/S3: ownership markers must not turn a symlink into deletion authority.
    target = tmp_path / "owned"
    mark(target)
    (target / "data").write_bytes(b"preserve")
    link = tmp_path / "link"
    try:
        link.symlink_to(target, target_is_directory=True)
    except OSError:
        pytest.skip("Creating symlinks is not permitted on this runner")
    with pytest.raises(ValueError):
        BUILD["ensure_destination"](link, replace=True, platform="linux")
    assert link.is_symlink()
    assert (target / "data").read_bytes() == b"preserve"


def test_owned_build_promotion_leaves_neighbors_untouched(tmp_path):
    # R3/S4: publication swaps only the marked destination.
    destination = tmp_path / "Inklow"
    mark(destination)
    (destination / "old").write_bytes(b"previous")
    neighbor = tmp_path / "Other.app"
    neighbor.mkdir()
    (neighbor / "document").write_bytes(b"private")
    staging = tmp_path / "staging"
    bundle = staging / "dist" / "Inklow"
    mark(bundle)
    (bundle / "new").write_bytes(b"new application")
    BUILD["publish"](bundle, destination, replace=True, platform="linux")
    assert (destination / "new").read_bytes() == b"new application"
    assert not (destination / "old").exists()
    assert not bundle.exists()
    assert (neighbor / "document").read_bytes() == b"private"


def test_failed_promotion_restores_previous_build(tmp_path, monkeypatch):
    # R3/S4: an OS rename failure must leave the prior app fully recoverable.
    destination = tmp_path / "Inklow"
    mark(destination)
    (destination / "document").write_bytes(b"previous content")
    bundle = tmp_path / "staging" / "dist" / "Inklow"
    mark(bundle)
    (bundle / "replacement").write_bytes(b"new content")
    rename = Path.rename

    def fail_new_promotion(path, target):
        if path == bundle:
            raise OSError("simulated destination rename failure")
        return rename(path, target)

    monkeypatch.setattr(Path, "rename", fail_new_promotion)
    with pytest.raises(OSError):
        BUILD["publish"](bundle, destination, replace=True, platform="linux")
    assert (destination / "document").read_bytes() == b"previous content"
    assert (bundle / "replacement").read_bytes() == b"new content"
    assert BUILD["marker_path"](destination, "linux").is_file()
    assert not (bundle.parent / "previous-build").exists()


@pytest.mark.parametrize("private_name", ["private.gguf", "weights.safetensors", "settings.json", ".env",
                                          "history.sqlite3", "statistics.db", "history.enc"])
def test_private_payload_is_rejected_but_native_libraries_are_allowed(tmp_path, private_name):
    # Native code may ship, but weights, settings and personal stores may not.
    library = tmp_path / "llama_cpp" / "lib" / "libggml-metal.dylib"
    library.parent.mkdir(parents=True)
    library.write_bytes(b"native library payload")
    BUILD["reject_private_payload"](tmp_path)
    private = tmp_path / "unexpected" / private_name
    private.parent.mkdir()
    private.write_bytes(b"must not ship")
    with pytest.raises(RuntimeError):
        BUILD["reject_private_payload"](tmp_path)
    assert library.read_bytes() == b"native library payload"
    assert private.read_bytes() == b"must not ship"
