"""Signed update boundaries; no public network or real installation is exercised."""
from __future__ import annotations

import base64
import hashlib
import io
import json
from pathlib import Path
import socket
import threading
from types import SimpleNamespace
import urllib.request

import pytest
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from inklow import updates


@pytest.fixture
def publisher(monkeypatch):
    key = Ed25519PrivateKey.generate()
    public = key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
    monkeypatch.setattr(updates, "PUBLISHER_PUBLIC_KEY", base64.b64encode(public).decode("ascii"))
    # Transport/security regressions remain independent of host OS quarantine.
    # The native API contract is exercised explicitly below.
    monkeypatch.setattr(updates, "_mark_web_download", lambda *args: None)
    return lambda data: base64.b64encode(key.sign(data))


def manifest(payload=b"verified archive", version="99.2.0", **changes):
    value = {"schema": 1, "application": updates.APP_ID, "version": version,
             "source_commit": "a" * 40,
             "assets": [{"name": f"BroTypist-{version}-linux-amd64.tar.gz",
                         "target": "linux-x86_64", "format": "tar.gz", "size": len(payload),
                         "sha256": hashlib.sha256(payload).hexdigest()}]}
    value.update(changes)
    return json.dumps(value, separators=(",", ":")).encode()


class Response(io.BytesIO):
    status = 200

    def __init__(self, data, headers=None):
        super().__init__(data)
        self.headers = {} if headers is None else headers

    def read1(self, count):
        return self.read(count)


def serve(monkeypatch, data, headers=None):
    seen = []

    def open_request(request, timeout):
        seen.append(request)
        return Response(data, headers)

    monkeypatch.setattr(updates.urllib.request, "build_opener",
                        lambda *args: SimpleNamespace(open=open_request))
    return seen


def test_manifest_pins_signature_to_exact_bytes(publisher):
    data = manifest()
    release = updates.verify_manifest(data, publisher(data))
    assert release.version == "99.2.0"
    assert release.release_url == f"https://github.com/{updates.REPOSITORY}/releases/tag/v99.2.0"
    with pytest.raises(updates.UpdateError):
        updates.verify_manifest(data + b" ", publisher(data))
    with pytest.raises(updates.UpdateError):
        updates.verify_manifest(b"not JSON", publisher(data))


@pytest.mark.parametrize("changes", [
    {"schema": True}, {"schema": 2}, {"application": "other.app"},
    {"version": "1.2"}, {"version": "01.2.3"}, {"version": "1.2.3-beta"},
    {"version": "9" * 100 + ".1.0"}, {"source_commit": "a" * 39},
    {"command": "arbitrary"}, {"assets": []},
])
def test_signed_manifest_rejects_invalid_schema(publisher, changes):
    data = manifest(**changes)
    with pytest.raises(updates.UpdateError):
        updates.verify_manifest(data, publisher(data))


@pytest.mark.parametrize("changes", [
    {"name": "../archive.tar.gz"}, {"name": "BroTypist-99.2.0-linux-amd64.zip"},
    {"target": "windows-x86_64"}, {"format": "zip"}, {"size": True},
    {"size": 0}, {"size": updates.MAX_PACKAGE_BYTES + 1},
    {"sha256": "A" * 64}, {"url": "https://evil.example/archive"},
])
def test_signed_asset_metadata_is_strict(publisher, changes):
    value = json.loads(manifest())
    value["assets"][0].update(changes)
    data = json.dumps(value).encode()
    with pytest.raises(updates.UpdateError):
        updates.verify_manifest(data, publisher(data))


def test_duplicate_keys_assets_and_oversize_are_rejected(publisher):
    data = manifest().replace(b'"schema":1', b'"schema":1,"schema":1')
    with pytest.raises(updates.UpdateError):
        updates.verify_manifest(data, publisher(data))
    value = json.loads(manifest())
    value["assets"] *= 2
    data = json.dumps(value).encode()
    with pytest.raises(updates.UpdateError):
        updates.verify_manifest(data, publisher(data))
    data = b" " * (updates.MAX_MANIFEST_BYTES + 1)
    with pytest.raises(updates.UpdateError):
        updates.verify_manifest(data, publisher(data))
    with pytest.raises(updates.UpdateError):
        updates.verify_manifest(manifest(), b"x" * 129)


@pytest.mark.parametrize("url", ["http://github.com/file", "https://evil.example/file",
                                  "https://github.com.evil.example/file",
                                  "https://user@github.com/file", "https://github.com:444/file"])
def test_redirects_cannot_escape_github_https(url):
    request = urllib.request.Request("https://github.com/release")
    with pytest.raises(updates.UpdateError):
        updates._GitHubRedirects().redirect_request(request, None, 302, "Found", {}, url)


def test_download_verifies_and_atomically_replaces(publisher, monkeypatch, tmp_path):
    payload = b"new archive"
    data = manifest(payload)
    release = updates.verify_manifest(data, publisher(data))
    target = tmp_path / release.assets[0].name
    target.write_bytes(b"old archive")
    seen = serve(monkeypatch, payload, {"Content-Length": str(len(payload))})
    progress = []
    assert updates.download_asset(release, release.assets[0], target,
                                  progress=lambda *value: progress.append(value)) == target
    assert target.read_bytes() == payload
    assert progress[-1] == (len(payload), len(payload))
    assert seen[0].full_url == f"https://github.com/{updates.REPOSITORY}/releases/download/v99.2.0/{target.name}"
    assert not seen[0].has_header("Authorization")
    assert list(tmp_path.iterdir()) == [target]


@pytest.mark.parametrize("payload", [b"invalid archive", b"short", b"too long for the declared archive"])
def test_bad_download_preserves_existing_destination(publisher, monkeypatch, tmp_path, payload):
    data = manifest(b"correct archive")
    release = updates.verify_manifest(data, publisher(data))
    target = tmp_path / "previous"
    target.write_bytes(b"previous")
    serve(monkeypatch, payload)
    with pytest.raises(updates.UpdateError):
        updates.download_asset(release, release.assets[0], target)
    assert target.read_bytes() == b"previous"
    assert list(tmp_path.iterdir()) == [target]


def test_cancel_after_final_chunk_never_promotes(publisher, monkeypatch, tmp_path):
    payload = b"archive"
    data = manifest(payload)
    release = updates.verify_manifest(data, publisher(data))
    target = tmp_path / "previous"
    target.write_bytes(b"previous")
    serve(monkeypatch, payload)
    cancel = updates.DownloadCancellation()
    with pytest.raises(updates.UpdateCancelled):
        updates.download_asset(release, release.assets[0], target, cancel=cancel,
                              progress=lambda received, total: cancel.set() if received else None)
    assert target.read_bytes() == b"previous"
    assert list(tmp_path.iterdir()) == [target]


def test_socket_cancellation_interrupts_owned_stalled_read():
    local, remote = socket.socketpair()
    cancel = updates.DownloadCancellation()
    response = SimpleNamespace(fp=SimpleNamespace(raw=SimpleNamespace(_sock=local)))
    reading = threading.Event()
    finished = threading.Event()

    def read_body():
        with cancel.response(response):
            reading.set()
            local.recv(1)
        finished.set()

    worker = threading.Thread(target=read_body)
    try:
        worker.start()
        assert reading.wait(2)
        cancel.set()
        assert finished.wait(2)
    finally:
        remote.close()
        local.close()
        worker.join(2)


def test_fetch_bounds_untrusted_response_before_parsing(monkeypatch):
    serve(monkeypatch, b"x" * (updates.MAX_MANIFEST_BYTES + 1))
    with pytest.raises(updates.UpdateError):
        updates.fetch_release()


def test_fetch_returns_exact_authenticated_feed_bytes(publisher, monkeypatch):
    data = manifest()
    signature = publisher(data)
    seen = []

    def open_request(request, timeout):
        seen.append(request.full_url)
        return Response(signature if request.full_url.endswith(".sig") else data)

    monkeypatch.setattr(updates.urllib.request, "build_opener",
                        lambda *args: SimpleNamespace(open=open_request))
    release, actual_data, actual_signature = updates.fetch_release()
    assert actual_data == data and actual_signature == signature
    assert release.version == "99.2.0"
    assert seen == [f"https://github.com/{updates.REPOSITORY}/releases/latest/download/update-manifest.json",
                    f"https://github.com/{updates.REPOSITORY}/releases/latest/download/update-manifest.sig"]


@pytest.mark.parametrize("headers", [{"Content-Length": "9999999999"},
                                    {"Content-Length": "invalid"},
                                    {"Content-Encoding": "gzip"}])
def test_response_headers_cannot_bypass_integrity_bounds(publisher, monkeypatch, tmp_path, headers):
    payload = b"archive"
    data = manifest(payload)
    release = updates.verify_manifest(data, publisher(data))
    serve(monkeypatch, payload, headers)
    target = tmp_path / "archive"
    with pytest.raises(updates.UpdateError):
        updates.download_asset(release, release.assets[0], target)
    assert not target.exists()
    assert not list(tmp_path.iterdir())


def test_symlink_download_destination_is_never_replaced(publisher, monkeypatch, tmp_path):
    data = manifest()
    release = updates.verify_manifest(data, publisher(data))
    previous = tmp_path / "previous"
    previous.write_bytes(b"unchanged")
    target = tmp_path / "link"
    target.symlink_to(previous)
    with pytest.raises(updates.UpdateError):
        updates.download_asset(release, release.assets[0], target)
    assert target.is_symlink() and previous.read_bytes() == b"unchanged"


@pytest.fixture
def service_env(monkeypatch, tmp_path):
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path / "isolated"))
    from inklow import update_install
    installation = update_install.Installation(tmp_path / "app", "linux_portable", True, "")
    monkeypatch.setattr(update_install, "detect_installation", lambda: installation)
    monkeypatch.setattr(updates.platform, "machine", lambda: "x86_64")
    monkeypatch.setattr(updates.sys, "platform", "linux")
    return installation


def settle(service):
    worker = service._worker
    if worker is not None:
        worker.join(2)
        assert not worker.is_alive()


def test_service_single_flight_and_cancel_hides_transport_errors(monkeypatch, service_env):
    entered, leave = threading.Event(), threading.Event()
    calls = []

    def fetch(*, cancel):
        calls.append(True)
        entered.set()
        leave.wait(2)
        raise RuntimeError("secret URL transport details")

    monkeypatch.setattr(updates, "fetch_release", fetch)
    service = updates.UpdateService()
    service.start_check()
    assert entered.wait(2)
    service.start_check()
    assert len(calls) == 1
    service.cancel()
    leave.set()
    settle(service)
    assert service.snapshot()["state"] == "cancelled"
    assert "secret" not in str(service.snapshot())
    service.close()


@pytest.mark.parametrize("version", ["99.9.0", "99.10.0"])
def test_versions_are_numeric_and_downgrades_never_offer_download(monkeypatch, publisher, service_env, version):
    monkeypatch.setattr(updates, "_current_version", lambda: "99.10.0")
    data = manifest(version=version)
    monkeypatch.setattr(updates, "fetch_release", lambda **kwargs:
                        (updates.verify_manifest(data, publisher(data)), data, publisher(data)))
    service = updates.UpdateService()
    service.start_check()
    settle(service)
    assert service.snapshot()["state"] == "current"
    with pytest.raises(updates.UpdateError):
        service.start_download()
    service.close()


def test_automatic_cache_is_profile_isolated_but_manual_always_checks(monkeypatch, publisher, service_env, tmp_path):
    data = manifest()
    calls = []

    def fetch(**kwargs):
        calls.append(True)
        return updates.verify_manifest(data, publisher(data)), data, publisher(data)

    monkeypatch.setattr(updates, "fetch_release", fetch)
    first = updates.UpdateService()
    first.start_check(automatic=True)
    settle(first)
    first.close()
    second = updates.UpdateService()
    second.start_check(automatic=True)
    settle(second)
    assert len(calls) == 1
    second.start_check()
    settle(second)
    assert len(calls) == 2
    second.close()
    monkeypatch.setenv("INKLOW_HOME", str(tmp_path / "other-profile"))
    third = updates.UpdateService()
    third.start_check(automatic=True)
    settle(third)
    assert len(calls) == 3
    third.close()


def test_source_install_can_check_but_cannot_download(monkeypatch, publisher, service_env):
    from inklow import update_install
    monkeypatch.setattr(update_install, "detect_installation", lambda:
                        update_install.Installation(None, "source", False, "Run the packaged app to install updates."))
    data = manifest()
    monkeypatch.setattr(updates, "fetch_release", lambda **kwargs:
                        (updates.verify_manifest(data, publisher(data)), data, publisher(data)))
    service = updates.UpdateService()
    service.start_check()
    settle(service)
    status = service.snapshot()
    assert status["state"] == "available"
    assert status["install_kind"] == "source" and status["installable"] is False
    status["state"] = "forged"
    assert service.snapshot()["state"] == "available"
    with pytest.raises(updates.UpdateError):
        service.start_download()
    service.close()


def test_ready_handoff_can_abort_retry_and_close_does_not_abort_commit(monkeypatch, publisher, service_env):
    from inklow import update_install
    payload = b"archive"
    data = manifest(payload)
    monkeypatch.setattr(updates, "fetch_release", lambda **kwargs:
                        (updates.verify_manifest(data, publisher(data)), data, publisher(data)))
    serve(monkeypatch, payload)
    handoffs = []

    class Handoff:
        committed = False
        aborted = False

        def commit(self):
            self.committed = True

        def abort(self):
            if not self.committed:
                self.aborted = True

    def spawn(manifest_path, signature_path, archive_path, installation):
        assert manifest_path.read_bytes() == data
        assert updates.verify_manifest(manifest_path.read_bytes(), signature_path.read_bytes())
        assert archive_path.read_bytes() == payload
        assert installation == service_env
        result = Handoff()
        handoffs.append(result)
        return result

    monkeypatch.setattr(update_install, "spawn_installer", spawn)
    service = updates.UpdateService()
    service.start_check()
    settle(service)
    service.start_download()
    settle(service)
    assert service.snapshot()["state"] == "ready"
    first = service.prepare_install()
    first.abort()
    service.cancel()
    assert service.snapshot()["state"] == "ready"
    second = service.prepare_install()
    second.commit()
    service.cancel()
    assert service.snapshot()["state"] == "installing"
    service.close()
    assert not second.aborted
    assert len(handoffs) == 2


def test_debian_adapter_rechecks_verified_bytes_and_never_claims_installed(monkeypatch, publisher, service_env):
    from inklow import update_install
    monkeypatch.setattr(update_install, "detect_installation", lambda:
                        update_install.Installation(Path("/opt/brotypist"), "linux_debian", False,
                                                    "Use your system package installer."))
    payload = b"deb archive"
    value = json.loads(manifest(payload))
    value["assets"][0].update(name="BroTypist-99.2.0-linux-amd64.deb", format="deb")
    data = json.dumps(value).encode()
    monkeypatch.setattr(updates, "fetch_release", lambda **kwargs:
                        (updates.verify_manifest(data, publisher(data)), data, publisher(data)))
    serve(monkeypatch, payload)
    opened = []
    monkeypatch.setattr(updates.subprocess, "Popen", lambda args, **kwargs: opened.append(args))
    service = updates.UpdateService()
    service.start_check()
    settle(service)
    service.start_download()
    settle(service)
    service.open_package_installer()
    assert opened[0][0] == "xdg-open" and len(opened[0]) == 2
    assert service.snapshot()["state"] == "ready"
    assert "restart" in service.snapshot()["message"].lower()
    service._archive_path.write_bytes(b"tampered")
    with pytest.raises(updates.UpdateError):
        service.open_package_installer()
    assert len(opened) == 1
    service.close()


def test_constructor_and_close_never_start_network_or_create_profile(monkeypatch, service_env):
    def unexpected_fetch(**kwargs):
        raise AssertionError("Network started outside an explicit check")

    monkeypatch.setattr(updates, "fetch_release", unexpected_fetch)
    service = updates.UpdateService()
    assert service.snapshot()["state"] == "idle"
    assert not service._root.exists()
    service.close()
    assert not service._root.exists()
    with pytest.raises(updates.UpdateError):
        service.start_check()


def test_failed_check_reports_safe_error_without_exception_details(monkeypatch, service_env):
    def failing_fetch(**kwargs):
        raise RuntimeError("https://private.invalid/token?secret=secret")

    monkeypatch.setattr(updates, "fetch_release", failing_fetch)
    service = updates.UpdateService()
    service.start_check()
    settle(service)
    assert service.snapshot()["state"] == "error"
    assert "private" not in str(service.snapshot())
    assert "secret" not in str(service.snapshot())
    service.close()


def test_untrusted_staging_directory_blocks_download_state(monkeypatch, service_env, tmp_path):
    root = tmp_path / "other"
    root.mkdir()
    service = updates.UpdateService()
    service._root.parent.mkdir(parents=True)
    service._root.symlink_to(root, target_is_directory=True)
    service.start_check()
    settle(service)
    assert service.snapshot()["state"] == "error"
    assert not list(root.iterdir())
    service.close()


def test_package_not_listed_in_release_cannot_trigger_network(publisher, monkeypatch, tmp_path):
    data = manifest()
    release = updates.verify_manifest(data, publisher(data))
    forged = updates.UpdateAsset("../escape", "linux-x86_64", "tar.gz", 1, "a" * 64)
    with pytest.raises(updates.UpdateError):
        updates.download_asset(release, forged, tmp_path / "package")
    assert not list(tmp_path.iterdir())


def test_close_keeps_shutdown_pending_until_worker_really_exits(monkeypatch, service_env):
    from inklow.runtime import ShutdownPending
    entered, leave = threading.Event(), threading.Event()

    def fetch(*, cancel):
        entered.set()
        leave.wait(3)
        raise updates.UpdateCancelled("Cancelled")

    monkeypatch.setattr(updates, "fetch_release", fetch)
    service = updates.UpdateService()
    service.start_check()
    try:
        assert entered.wait(2)
        with pytest.raises(ShutdownPending):
            service.close()
        assert service._cancel.is_set()
        assert service._worker.is_alive()
    finally:
        leave.set()
        settle(service)
        service.close()


@pytest.mark.parametrize("read_success,write_success", [(True, True), (False, True), (True, False)])
def test_mac_quarantine_uses_exported_constants_and_preserves_existing_metadata(
        monkeypatch, tmp_path, read_success, write_success):
    monkeypatch.setattr(updates.sys, "platform", "darwin")
    constants = {name: object() for name in
                 ("kLSQuarantineTypeKey", "kLSQuarantineTypeWebDownload",
                  "kLSQuarantineDataURLKey", "kLSQuarantineOriginURLKey")}
    resource_key = object()
    existing = {"native-agent": "preserved", "native-event": "preserved"}
    written = []

    class FileURL:
        def getResourceValue_forKey_error_(self, value, key, error):
            assert key is resource_key
            return read_success, existing, None

        def setResourceValue_forKey_error_(self, value, key, error):
            assert key is resource_key
            written.append(value)
            return write_success, None

    def load_constants(bundle, destination, variable_info, *, skip_undefined):
        assert skip_undefined is False
        assert all(encoding == b"@" for name, encoding in variable_info)
        destination.update(constants)

    monkeypatch.setitem(updates.sys.modules, "Foundation", SimpleNamespace(
        NSBundle=SimpleNamespace(bundleWithPath_=lambda path: SimpleNamespace(load=lambda: True)),
        NSURL=SimpleNamespace(fileURLWithPath_=lambda path: FileURL()),
        NSURLQuarantinePropertiesKey=resource_key))
    monkeypatch.setitem(updates.sys.modules, "objc", SimpleNamespace(loadBundleVariables=load_constants))
    if read_success and write_success:
        updates._mark_web_download(tmp_path / "package", "https://github.com/package", "https://github.com/release")
        assert written[0]["native-agent"] == "preserved"
        assert written[0]["native-event"] == "preserved"
        assert written[0][constants["kLSQuarantineTypeKey"]] is constants["kLSQuarantineTypeWebDownload"]
        assert written[0][constants["kLSQuarantineDataURLKey"]] == "https://github.com/package"
        assert written[0][constants["kLSQuarantineOriginURLKey"]] == "https://github.com/release"
        assert existing == {"native-agent": "preserved", "native-event": "preserved"}
    else:
        with pytest.raises(updates.UpdateError):
            updates._mark_web_download(tmp_path / "package", "https://github.com/package", "https://github.com/release")
        if not read_success:
            assert not written


def test_native_security_metadata_failure_preserves_previous_package(publisher, monkeypatch, tmp_path):
    payload = b"archive"
    data = manifest(payload)
    release = updates.verify_manifest(data, publisher(data))
    serve(monkeypatch, payload)
    target = tmp_path / "package"
    target.write_bytes(b"previous")

    def fail_native_metadata(*args):
        raise updates.UpdateError("Native security metadata is unavailable.")

    monkeypatch.setattr(updates, "_mark_web_download", fail_native_metadata)
    with pytest.raises(updates.UpdateError):
        updates.download_asset(release, release.assets[0], target)
    assert target.read_bytes() == b"previous"
    assert list(tmp_path.iterdir()) == [target]
