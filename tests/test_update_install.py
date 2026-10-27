"""Security boundaries and observable OTA transactions (no live app/profile)."""
import base64
import io
import json
import os
from pathlib import Path, PurePosixPath
import subprocess
import sys
import tarfile
from types import SimpleNamespace
import zipfile

import pytest
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from inklow import update_install as installer
from inklow import updates

OLD_COMMIT = "1" * 40
NEW_COMMIT = "2" * 40
NATIVE = sys.platform
pytestmark = pytest.mark.skipif(NATIVE not in {"darwin", "linux"}, reason="Native POSIX OTA only")


def make_bundle(path, version="0.4.4", source=OLD_COMMIT, native=NATIVE):
    path.mkdir(parents=True)
    marker = path / "Contents/Resources" / installer.MARKER if native == "darwin" else path / installer.MARKER
    marker.parent.mkdir(parents=True, exist_ok=True)
    identity = {"application": installer.APP_ID, "platform": native, "version": version,
                "source_commit": source, "architecture": installer._architecture()}
    marker.write_text(json.dumps(identity))
    executable = installer._executable(path, native)
    executable.parent.mkdir(parents=True, exist_ok=True)
    header = bytearray(64)
    if native == "darwin":
        header[:4] = b"\xcf\xfa\xed\xfe"
        header[4:8] = (0x100000C if installer._architecture() == "arm64" else 0x1000007).to_bytes(4, "little")
    else:
        header[:6] = b"\x7fELF\x02\x01"
        header[18:20] = (183 if installer._architecture() == "arm64" else 62).to_bytes(2, "little")
    executable.write_bytes(header)
    executable.chmod(0o755)
    (path / "payload-version").write_text(version)
    return identity


@pytest.fixture
def frozen(monkeypatch):
    monkeypatch.setattr(sys, "frozen", True, raising=False)
    monkeypatch.delenv("INKLOW_HOME", raising=False)
    monkeypatch.setattr(installer, "_managed_debian", lambda _path: False)


@pytest.fixture
def signer(monkeypatch):
    key = Ed25519PrivateKey.generate()
    public = key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
    monkeypatch.setattr(updates, "PUBLISHER_PUBLIC_KEY", base64.b64encode(public).decode())
    return key


def signed_package(root, bundle, signer, version="0.4.5", source=NEW_COMMIT):
    archive = root / "package"
    if NATIVE == "darwin":
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as stream:
            for item in [bundle, *bundle.rglob("*")]:
                stream.write(item, str(Path("BroTypist.app") / item.relative_to(bundle)))
        target, format_name, ending = "macos-" + installer._architecture(), "zip", "macos-" + installer._architecture() + ".zip"
    else:
        with tarfile.open(archive, "w:gz") as stream:
            stream.add(bundle, arcname="BroTypist")
        target, format_name, ending = "linux-x86_64", "tar.gz", "linux-amd64.tar.gz"
    import hashlib
    data = json.dumps({"schema": 1, "application": installer.APP_ID, "version": version,
                       "source_commit": source, "assets": [{"name": f"BroTypist-{version}-{ending}",
                       "target": target, "format": format_name, "size": archive.stat().st_size,
                       "sha256": hashlib.sha256(archive.read_bytes()).hexdigest()}]}).encode()
    installer._write_private(root / "manifest.json", data)
    installer._write_private(root / "manifest.sig", base64.b64encode(signer.sign(data)))
    archive.chmod(0o600)


@pytest.fixture
def transaction(tmp_path, monkeypatch, signer, frozen):
    if NATIVE == "linux" and installer._architecture() != "x86_64":
        pytest.skip("The signed Linux release target is x86_64")
    destination = tmp_path / ("BroTypist.app" if NATIVE == "darwin" else "BroTypist")
    old = make_bundle(destination)
    monkeypatch.setattr(sys, "executable", str(installer._executable(destination, NATIVE)))
    root = tmp_path / ".brotypist-update-transaction"
    root.mkdir(mode=0o700)
    newer = tmp_path / "package-source" / destination.name
    make_bundle(newer, "0.4.5", NEW_COMMIT)
    signed_package(root, newer, signer)
    info = destination.stat()
    request = {"schema": 1, "destination": str(destination), "platform": NATIVE, "identity": old,
               "device": info.st_dev, "inode": info.st_ino, "uid": os.geteuid(),
               "parent_pid": os.getpid(), "isolated_home": None, "nonce": "a" * 64, "version": "0.4.5"}
    installer._write_private(root / "request.json", request)
    # The transaction tests use real archive parsing, signature/hash verification,
    # extraction and the OS atomic exchange, but never execute a synthetic binary.
    monkeypatch.setattr(installer, "_wait_for_exit", lambda *_args: None)
    monkeypatch.setattr(installer, "_health_version", lambda *_args: None)
    monkeypatch.setattr(installer, "_signing_identity", lambda _path: (installer.APP_ID, "TEAM", ("Apple Development: Test",)))
    monkeypatch.setattr(installer, "_launch", lambda *_args: SimpleNamespace(poll=lambda: None))
    return root, destination, request


def run_transaction(root):
    reader, writer = os.pipe()
    os.close(writer)
    return installer.run_installer(root / "request.json", reader)


def receipt(root):
    return json.loads((root / "transaction.json").read_text())


def test_success_atomically_installs_signed_build_and_retains_rollback(transaction, monkeypatch):
    root, destination, request = transaction
    monkeypatch.setattr(installer, "_wait_startup", lambda *_args: None)
    assert run_transaction(root) == 0
    assert (destination / "payload-version").read_text() == "0.4.5"
    result = receipt(root)
    assert result["status"] == "healthy"
    old = Path(result["rollback"])
    assert (old / "payload-version").read_text() == "0.4.4"
    assert old.stat().st_ino == request["inode"]
    assert not (root / "package").exists()
    assert (root / "transaction.json").stat().st_mode & 0o077 == 0


def test_native_startup_failure_restores_original_inode(transaction, monkeypatch):
    root, destination, request = transaction
    def failed(*_args):
        raise installer.InstallError("Native startup was not acknowledged.")
    monkeypatch.setattr(installer, "_wait_startup", failed)
    assert run_transaction(root) == 1
    assert receipt(root)["status"] == "rolled_back"
    assert destination.stat().st_ino == request["inode"]
    assert (destination / "payload-version").read_text() == "0.4.4"


def test_unsupported_atomic_exchange_never_moves_old_app(transaction, monkeypatch):
    root, destination, request = transaction
    def unsupported(*_args):
        raise OSError("unsupported exchange")
    monkeypatch.setattr(installer, "_atomic_exchange", unsupported)
    assert run_transaction(root) == 1
    assert destination.stat().st_ino == request["inode"]
    assert receipt(root)["status"] == "not_installed"
    assert not (root / "payload").exists()


def test_failed_rollback_preserves_both_complete_builds(transaction, monkeypatch):
    root, destination, _ = transaction
    exchange = installer._atomic_exchange
    calls = []
    def fail_second(left, right):
        calls.append((left, right))
        if len(calls) == 2:
            raise OSError("rollback failure")
        exchange(left, right)
    monkeypatch.setattr(installer, "_atomic_exchange", fail_second)
    monkeypatch.setattr(installer, "_wait_startup", lambda *_args: (_ for _ in ()).throw(installer.InstallError("startup failed")))
    with pytest.raises(installer.InstallError, match="previous build is retained"):
        run_transaction(root)
    assert (destination / "payload-version").read_text() == "0.4.5"
    result = receipt(root)
    assert result["status"] == "rollback_failed"
    assert (Path(result["rollback"]) / "payload-version").read_text() == "0.4.4"


@pytest.mark.parametrize("input_name", ["manifest.json", "manifest.sig", "package"])
def test_modified_signed_input_cannot_replace_application(transaction, input_name):
    root, destination, request = transaction
    with (root / input_name).open("ab") as stream:
        stream.write(b"tampered")
    assert run_transaction(root) == 1
    assert destination.stat().st_ino == request["inode"]
    assert (destination / "payload-version").read_text() == "0.4.4"


def test_destination_replacement_during_parent_exit_wait_is_rejected(transaction, monkeypatch):
    root, destination, _ = transaction
    def replace(*_args):
        destination.rename(destination.with_name("original"))
        make_bundle(destination)
    monkeypatch.setattr(installer, "_wait_for_exit", replace)
    assert run_transaction(root) == 1
    assert receipt(root)["status"] == "not_installed"
    assert (destination / "payload-version").read_text() == "0.4.4"


def test_archive_build_source_must_match_signed_release(transaction):
    root, destination, _ = transaction
    # The signed release's claimed commit differs from the authentic package's marker.
    manifest = json.loads((root / "manifest.json").read_text())
    manifest["source_commit"] = "3" * 40
    # Without resigning this is rejected at the earliest signature boundary.
    (root / "manifest.json").write_text(json.dumps(manifest))
    assert run_transaction(root) == 1
    assert (destination / "payload-version").read_text() == "0.4.4"


def test_wrong_signed_build_provenance_rejected(transaction, signer):
    root, destination, _ = transaction
    manifest = json.loads((root / "manifest.json").read_text())
    manifest["source_commit"] = "3" * 40
    data = json.dumps(manifest).encode()
    (root / "manifest.json").write_bytes(data)
    (root / "manifest.sig").write_bytes(base64.b64encode(signer.sign(data)))
    assert run_transaction(root) == 1
    assert receipt(root)["status"] == "not_installed"
    assert (destination / "payload-version").read_text() == "0.4.4"


def test_source_and_protected_normal_installations_cannot_self_update(tmp_path, monkeypatch, frozen):
    destination = tmp_path / "normal" / ("BroTypist.app" if NATIVE == "darwin" else "BroTypist")
    make_bundle(destination)
    monkeypatch.setattr(sys, "executable", str(installer._executable(destination, NATIVE)))
    assert installer.detect_installation().installable
    profile = tmp_path / "profile"
    profile.mkdir()
    monkeypatch.setenv("INKLOW_HOME", str(profile))
    assert not installer.detect_installation().installable
    monkeypatch.delenv("INKLOW_HOME")
    (destination.parent / ".git").mkdir()
    assert not installer.detect_installation().installable
    monkeypatch.setattr(sys, "frozen", False)
    assert installer.detect_installation().kind == "source"


def test_isolated_QA_copy_requires_external_profile(tmp_path, monkeypatch, frozen):
    profile = tmp_path / "profile"
    destination = profile / "qa" / ("BroTypist.app" if NATIVE == "darwin" else "BroTypist")
    make_bundle(destination)
    settings = profile / "settings.json"
    settings.write_text("sentinel")
    monkeypatch.setattr(sys, "executable", str(installer._executable(destination, NATIVE)))
    monkeypatch.setenv("INKLOW_HOME", str(profile))
    assert installer.detect_installation().installable
    assert settings.read_text() == "sentinel"
    monkeypatch.setenv("INKLOW_HOME", str(destination))
    assert not installer.detect_installation().installable


def test_symlink_destination_and_wrong_binary_architecture_rejected(tmp_path, monkeypatch, frozen):
    destination = tmp_path / "real" / ("BroTypist.app" if NATIVE == "darwin" else "BroTypist")
    make_bundle(destination)
    link = tmp_path / destination.name
    link.symlink_to(destination, target_is_directory=True)
    monkeypatch.setattr(sys, "executable", str(installer._executable(link, NATIVE)))
    assert not installer.detect_installation().installable
    executable = installer._executable(destination, NATIVE)
    monkeypatch.setattr(sys, "executable", str(executable))
    executable.write_bytes(b"not a native executable")
    assert not installer.detect_installation().installable


def test_debian_installation_never_self_swaps(tmp_path, monkeypatch, frozen):
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setattr(sys, "executable", str(tmp_path / "BroTypist"))
    monkeypatch.setattr(installer, "_managed_debian", lambda _path: True)
    result = installer.detect_installation()
    assert result.kind == "linux_debian" and not result.installable
    with pytest.raises(installer.InstallError, match="does not support"):
        installer.spawn_installer(tmp_path / "manifest", tmp_path / "sig", tmp_path / "archive", result)


def test_unapproved_handoff_closes_pipe_and_approved_handoff_survives_abort(tmp_path):
    reader, writer = os.pipe()
    handoff = installer.Handoff(writer, SimpleNamespace(poll=lambda: None), tmp_path)
    handoff.abort()
    assert os.read(reader, 1) == b""
    os.close(reader)
    reader, writer = os.pipe()
    handoff = installer.Handoff(writer, SimpleNamespace(poll=lambda: None), tmp_path)
    try:
        handoff.commit()
        assert os.read(reader, 1) == b"1"
        handoff.abort()
        assert handoff.committed
        os.fstat(writer)  # ordinary service.close must not signal actual process exit
        handoff.commit()  # idempotent; no second approval byte
    finally:
        installer._COMMITTED_WRITERS.discard(writer)
        os.close(writer)
        os.close(reader)


def test_approval_byte_alone_cannot_promote_while_parent_alive():
    reader, writer = os.pipe()
    os.write(writer, b"1")
    os.close(writer)
    try:
        with pytest.raises(installer.InstallError, match="did not exit"):
            installer._wait_for_exit(reader, os.getpid(), timeout=0.02)
    finally:
        os.close(reader)


def test_open_parent_pipe_cannot_be_treated_as_exit():
    reader, writer = os.pipe()
    os.write(writer, b"1")
    try:
        with pytest.raises(installer.InstallError, match="timed out"):
            installer._wait_for_exit(reader, os.getpid(), timeout=0.02)
    finally:
        os.close(reader)
        os.close(writer)


@pytest.mark.parametrize("approval", [b"", b"0", b"11"])
def test_unapproved_or_repeated_approval_is_rejected(approval):
    reader, writer = os.pipe()
    os.write(writer, approval)
    os.close(writer)
    try:
        with pytest.raises(installer.InstallError, match="cancelled|not approved"):
            installer._wait_for_exit(reader, os.getpid(), timeout=0.1)
    finally:
        os.close(reader)


@pytest.mark.parametrize("name,link,kind", [
    ("../escape", None, "file"), ("/absolute", None, "file"),
    ("BroTypist/../../escape", None, "file"), ("BroTypist/a", "/tmp/escape", "link"),
    ("BroTypist/a", "../../escape", "link"), ("BroTypist/settings.json", None, "file"),
    ("BroTypist/model.gguf", None, "file"), ("BroTypist/fifo", None, "fifo"),
])
def test_tar_security_boundaries(tmp_path, name, link, kind):
    archive = tmp_path / "package.tar.gz"
    with tarfile.open(archive, "w:gz") as stream:
        entry = tarfile.TarInfo(name)
        if kind == "link":
            entry.type, entry.linkname = tarfile.SYMTYPE, link
        elif kind == "fifo":
            entry.type = tarfile.FIFOTYPE
        else:
            entry.size = 1
        stream.addfile(entry, io.BytesIO(b"x") if kind == "file" else None)
    stage = tmp_path / "stage"
    stage.mkdir()
    with pytest.raises(installer.InstallError):
        installer._extract_package(archive, stage, "linux")
    assert not (tmp_path / "escape").exists()


def test_archive_duplicate_case_and_link_parent_are_rejected():
    for rows in [
        [(PurePosixPath("BroTypist/a"), None, False), (PurePosixPath("BroTypist/A"), None, False)],
        [(PurePosixPath("BroTypist/a"), "internal", False), (PurePosixPath("BroTypist/A/file"), None, False)],
    ]:
        with pytest.raises(installer.InstallError):
            installer._validate_members(rows)


def test_tar_limits_are_checked_before_extraction(tmp_path, monkeypatch):
    archive = tmp_path / "package.tar.gz"
    with tarfile.open(archive, "w:gz") as stream:
        for name in ("BroTypist/a", "BroTypist/b"):
            entry = tarfile.TarInfo(name)
            entry.size = 2
            stream.addfile(entry, io.BytesIO(b"xx"))
    stage = tmp_path / "stage"
    stage.mkdir()
    monkeypatch.setattr(installer, "MAX_ENTRIES", 1)
    with pytest.raises(installer.InstallError, match="too many"):
        installer._extract_package(archive, stage, "linux")
    assert not list(stage.iterdir())
    monkeypatch.setattr(installer, "MAX_ENTRIES", 10)
    monkeypatch.setattr(installer, "MAX_UNPACKED", 3)
    with pytest.raises(installer.InstallError, match="unpacked limit"):
        installer._extract_package(archive, stage, "linux")
    assert not list(stage.iterdir())


def test_zip_traversal_rejected_before_native_ditto(tmp_path, monkeypatch):
    archive = tmp_path / "bad.zip"
    with zipfile.ZipFile(archive, "w") as stream:
        stream.writestr("BroTypist.app/../../escape", "bad")
    monkeypatch.setattr(installer.subprocess, "run", lambda *_args, **_kwargs: pytest.fail("ditto ran before validation"))
    with pytest.raises(installer.InstallError, match="unsafe path"):
        installer._extract_package(archive, tmp_path, "darwin")


def test_safe_internal_framework_links_survive_tar_extraction(tmp_path):
    archive = tmp_path / "links.tar.gz"
    with tarfile.open(archive, "w:gz") as stream:
        for name in ("BroTypist", "BroTypist/F.framework", "BroTypist/F.framework/Versions", "BroTypist/F.framework/Versions/A"):
            item = tarfile.TarInfo(name)
            item.type = tarfile.DIRTYPE
            stream.addfile(item)
        item = tarfile.TarInfo("BroTypist/F.framework/Versions/A/F")
        item.size = 1
        stream.addfile(item, io.BytesIO(b"x"))
        item = tarfile.TarInfo("BroTypist/F.framework/Versions/Current")
        item.type, item.linkname = tarfile.SYMTYPE, "A"
        stream.addfile(item)
        item = tarfile.TarInfo("BroTypist/F.framework/F")
        item.type, item.linkname = tarfile.SYMTYPE, "Versions/Current/F"
        stream.addfile(item)
    stage = tmp_path / "stage"
    stage.mkdir()
    bundle = installer._extract_package(archive, stage, "linux")
    assert (bundle / "F.framework/F").is_symlink()
    assert (bundle / "F.framework/F").read_bytes() == b"x"


def test_health_environment_cannot_write_arbitrary_file(tmp_path, monkeypatch, frozen):
    victim = tmp_path / "victim"
    monkeypatch.setenv(installer.HEALTH_PATH, str(victim))
    monkeypatch.setenv(installer.HEALTH_NONCE, "a" * 64)
    assert not installer.acknowledge_startup()
    assert not victim.exists()
    assert installer.HEALTH_PATH not in os.environ
    assert installer.HEALTH_NONCE not in os.environ


def test_native_ack_requires_signed_current_build_and_is_exclusive(transaction, monkeypatch):
    root, destination, request = transaction
    make_path = destination / "Contents/Resources" / installer.MARKER if NATIVE == "darwin" else destination / installer.MARKER
    identity = {**request["identity"], "version": "0.4.5", "source_commit": NEW_COMMIT}
    make_path.write_text(json.dumps(identity))
    installer._receipt(root, request, "promoted")
    for expected in (True, False):
        monkeypatch.setenv(installer.HEALTH_PATH, str(root / "startup.json"))
        monkeypatch.setenv(installer.HEALTH_NONCE, request["nonce"])
        assert installer.acknowledge_startup() is expected
    ack = json.loads((root / "startup.json").read_text())
    assert ack == {"nonce": request["nonce"], "version": "0.4.5", "pid": os.getpid()}
    assert (root / "startup.json").stat().st_mode & 0o077 == 0


def test_native_ack_is_not_visible_before_its_complete_payload(transaction, monkeypatch):
    root, destination, request = transaction
    marker = destination / "Contents/Resources" / installer.MARKER if NATIVE == "darwin" else destination / installer.MARKER
    marker.write_text(json.dumps({**request["identity"], "version": "0.4.5", "source_commit": NEW_COMMIT}))
    installer._receipt(root, request, "promoted")
    startup = root / "startup.json"
    monkeypatch.setenv(installer.HEALTH_PATH, str(startup))
    monkeypatch.setenv(installer.HEALTH_NONCE, request["nonce"])
    native_open = os.open

    def observe_creation(path, flags, *args, **kwargs):
        descriptor = native_open(path, flags, *args, **kwargs)
        if flags & os.O_CREAT and Path(path).parent == root and "startup" in Path(path).name:
            try:
                if startup.exists():
                    # This is the installer's concurrent consumer boundary:
                    # a published receipt must already contain complete JSON.
                    installer._json(installer._read_file(startup, 1024, private=True))
            except BaseException:
                os.close(descriptor)
                raise
        return descriptor

    monkeypatch.setattr(os, "open", observe_creation)
    assert installer.acknowledge_startup()
    published = json.loads(startup.read_bytes())
    assert published == {"nonce": request["nonce"], "version": "0.4.5", "pid": os.getpid()}


def test_open_success_is_not_native_health(tmp_path):
    process = SimpleNamespace(poll=lambda: 0)
    request = {"platform": "darwin", "nonce": "a" * 64, "version": "0.4.5"}
    with pytest.raises(installer.InstallError, match="not acknowledged"):
        installer._wait_startup(tmp_path, request, process, timeout=0.01)


def test_atomic_exchange_keeps_both_paths_present(tmp_path):
    left, right = tmp_path / "left", tmp_path / "right"
    left.mkdir()
    right.mkdir()
    (left / "version").write_text("old")
    (right / "version").write_text("new")
    original = left.stat().st_ino
    installer._atomic_exchange(left, right)
    assert (left / "version").read_text() == "new"
    assert (right / "version").read_text() == "old"
    assert right.stat().st_ino == original
    installer._atomic_exchange(left, right)
    assert left.stat().st_ino == original




@pytest.mark.skipif(NATIVE != "darwin", reason="macOS code signature identity")
def test_changed_development_identity_cannot_promote(transaction, monkeypatch):
    root, destination, request = transaction
    def signing(path):
        return (installer.APP_ID, "OLD" if path == destination else "NEW", ("Apple Development: Test",))
    monkeypatch.setattr(installer, "_signing_identity", signing)
    assert run_transaction(root) == 1
    assert destination.stat().st_ino == request["inode"]
    assert "signing identity changed" in receipt(root)["message"]


def _native_write_attribute(path, name, value):
    subprocess.run(["/usr/bin/xattr", "-w", "-x", name, value.hex(), str(path)], check=True)


def _native_read_attribute(path, name):
    result = subprocess.run(["/usr/bin/xattr", "-p", "-x", name, str(path)],
                            check=True, capture_output=True, text=True)
    return bytes.fromhex(result.stdout)


@pytest.mark.skipif(NATIVE != "darwin", reason="macOS quarantine provenance")
def test_private_copy_preserves_quarantine_without_overwriting_existing_provenance(tmp_path):
    source, copied = tmp_path / "source", tmp_path / "copied"
    source.write_bytes(b"archive")
    quarantine = b"0083;00000000;Browser;"
    _native_write_attribute(source, "com.apple.quarantine", quarantine)
    installer._copy_private(source, copied, 1024)
    assert _native_read_attribute(copied, "com.apple.quarantine") == quarantine
    other = b"0083;11111111;OtherBrowser;"
    _native_write_attribute(source, "com.apple.quarantine", other)
    installer._preserve_quarantine(source, copied)
    assert _native_read_attribute(copied, "com.apple.quarantine") == quarantine


def test_invalid_health_signature_does_not_raise_or_acknowledge(transaction, monkeypatch):
    root, destination, request = transaction
    marker = destination / "Contents/Resources" / installer.MARKER if NATIVE == "darwin" else destination / installer.MARKER
    marker.write_text(json.dumps({**request["identity"], "version": "0.4.5", "source_commit": NEW_COMMIT}))
    installer._receipt(root, request, "promoted")
    (root / "manifest.sig").write_bytes(b"invalid")
    monkeypatch.setenv(installer.HEALTH_PATH, str(root / "startup.json"))
    monkeypatch.setenv(installer.HEALTH_NONCE, request["nonce"])
    assert not installer.acknowledge_startup()
    assert not (root / "startup.json").exists()
    assert installer.HEALTH_NONCE not in os.environ


def test_approved_pipe_releases_only_after_real_parent_exit():
    reader, writer = os.pipe()
    child = subprocess.Popen([sys.executable, "-c",
                              "import os,sys; os.write(int(sys.argv[1]), b'1')", str(writer)],
                             pass_fds=(writer,))
    os.close(writer)
    try:
        child.wait(timeout=10)
        installer._wait_for_exit(reader, child.pid, timeout=1)
    finally:
        os.close(reader)


@pytest.mark.skipif(NATIVE != "darwin", reason="Native ditto framework-link extraction")
def test_zip_preserves_safe_framework_links_and_appledouble_metadata(tmp_path):
    archive = tmp_path / "framework.zip"
    source = tmp_path / "source/BroTypist.app"
    framework = source / "Contents/Frameworks/F.framework"
    binary = framework / "Versions/A/F"
    binary.parent.mkdir(parents=True)
    binary.write_bytes(b"framework binary")
    _native_write_attribute(binary, "com.example.brotypist-test", b"resource-metadata")
    (framework / "Versions/Current").symlink_to("A")
    (framework / "F").symlink_to("Versions/Current/F")
    subprocess.run(["/usr/bin/ditto", "-c", "-k", "--sequesterRsrc", "--keepParent",
                    str(source), str(archive)], check=True)
    stage = tmp_path / "stage"
    stage.mkdir()
    bundle = installer._extract_package(archive, stage, "darwin")
    link = bundle / "Contents/Frameworks/F.framework/F"
    assert link.is_symlink()
    assert link.read_bytes() == b"framework binary"
    assert _native_read_attribute(link.resolve(), "com.example.brotypist-test") == b"resource-metadata"


def test_zip_metadata_cannot_evade_unpack_bound(tmp_path, monkeypatch):
    archive = tmp_path / "large-metadata.zip"
    with zipfile.ZipFile(archive, "w") as stream:
        stream.writestr("__MACOSX/", b"1234")
    monkeypatch.setattr(installer, "MAX_UNPACKED", 3)
    with pytest.raises(installer.InstallError, match="unpacked limit"):
        installer._extract_package(archive, tmp_path, "darwin")


@pytest.mark.skipif(NATIVE != "darwin", reason="Darwin libc extended-attribute ABI")
def test_native_darwin_metadata_handles_binary_values_and_exclusive_creation(tmp_path):
    import errno
    path = tmp_path / "metadata"
    path.write_bytes(b"data")
    name = "com.example.brotypist-test"
    original = b"\x00\xffquarantine\x00metadata"
    installer._darwin_setxattr(path, name, original, create=True)
    assert _native_read_attribute(path, name) == original
    assert installer._darwin_getxattr(path, name) == original
    with pytest.raises(OSError) as error:
        installer._darwin_setxattr(path, name, b"replacement", create=True)
    assert error.value.errno == errno.EEXIST
    assert _native_read_attribute(path, name) == original
