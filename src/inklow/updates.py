"""Credential-free signed releases and an explicitly owned background updater.

Importing this module does not access the network, settings, models or UI. The
standalone installer also imports verify_manifest, so application dependencies
belong inside the service methods rather than at module scope.
"""
from __future__ import annotations

import base64
import binascii
from contextlib import contextmanager, nullcontext
from dataclasses import dataclass
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import socket
import stat
import subprocess
import sys
import tempfile
from threading import Event, Lock, RLock, Thread
import time
from urllib.parse import urlsplit
import urllib.request

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

from .update_keys import APP_ID, PUBLISHER_PUBLIC_KEY, REPOSITORY

MAX_MANIFEST_BYTES = 256 * 1024
MAX_SIGNATURE_BYTES = 128
MAX_PACKAGE_BYTES = 512 * 1024 * 1024
MAX_UNPACKED_BYTES = 2 * 1024 * 1024 * 1024
MAX_ARCHIVE_ENTRIES = 20000
_FEED = f"https://github.com/{REPOSITORY}/releases/latest/download/update-manifest.json"
_CHECK_INTERVAL = 24 * 60 * 60
_VERSION = re.compile(r"(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\Z")
_ALLOWED_HOSTS = frozenset({"github.com", "release-assets.githubusercontent.com",
                            "objects.githubusercontent.com", "github-releases.githubusercontent.com"})
_ASSET_FORMATS = {("macos-arm64", "zip"): "macos-arm64.zip",
                  ("macos-arm64", "dmg"): "macos-arm64.dmg",
                  ("linux-x86_64", "tar.gz"): "linux-amd64.tar.gz",
                  ("linux-x86_64", "deb"): "linux-amd64.deb"}


class UpdateError(RuntimeError):
    """A bounded, user-safe update operation failure."""


class UpdateCancelled(UpdateError):
    """Cancellation never promotes an unverified or incomplete package."""


@dataclass(frozen=True)
class UpdateAsset:
    name: str
    target: str
    format: str
    size: int
    sha256: str


@dataclass(frozen=True)
class UpdateRelease:
    version: str
    source_commit: str
    assets: tuple[UpdateAsset, ...]

    @property
    def release_url(self) -> str:
        _version(self.version)
        return f"https://github.com/{REPOSITORY}/releases/tag/v{self.version}"


def _version(value):
    if not isinstance(value, str) or not _VERSION.fullmatch(value):
        raise UpdateError("The update version is not a supported stable version.")
    return tuple(int(part) for part in value.split("."))


def _object(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise UpdateError("The update metadata contains duplicate fields.")
        value[key] = item
    return value


def _fields(value, expected):
    if type(value) is not dict or set(value) != expected:
        raise UpdateError("The update metadata has an unsupported schema.")


def _asset(value, version):
    _fields(value, {"name", "target", "format", "size", "sha256"})
    if any(type(value[field]) is not str for field in ("name", "target", "format", "sha256")):
        raise UpdateError("The update asset metadata is invalid.")
    suffix = _ASSET_FORMATS.get((value["target"], value["format"]))
    if suffix is None or value["name"] != f"BroTypist-{version}-{suffix}":
        raise UpdateError("The update asset does not match its platform and version.")
    if type(value["size"]) is not int or not 0 < value["size"] <= MAX_PACKAGE_BYTES:
        raise UpdateError("The update package size is outside the allowed bounds.")
    if not re.fullmatch(r"[a-f0-9]{64}", value["sha256"]):
        raise UpdateError("The update package digest is invalid.")
    return UpdateAsset(**value)


def verify_manifest(data: bytes, signature: bytes) -> UpdateRelease:
    """Authenticate exact bytes with the built-in publisher key before JSON parsing."""
    if type(data) is not bytes or not 0 < len(data) <= MAX_MANIFEST_BYTES:
        raise UpdateError("The update metadata exceeds its allowed bounds.")
    if type(signature) is not bytes or not 0 < len(signature) <= MAX_SIGNATURE_BYTES:
        raise UpdateError("The update signature exceeds its allowed bounds.")
    try:
        # A single trailing newline accommodates the publisher's text asset;
        # whitespace inside a base64 signature is never accepted.
        encoded = signature.strip(b"\r\n")
        encoded.decode("ascii")
        raw_signature = base64.b64decode(encoded, validate=True)
        raw_key = base64.b64decode(PUBLISHER_PUBLIC_KEY, validate=True)
        if len(raw_signature) != 64 or len(raw_key) != 32:
            raise ValueError("Invalid Ed25519 lengths")
        Ed25519PublicKey.from_public_bytes(raw_key).verify(raw_signature, data)
    except (ValueError, TypeError, UnicodeError, binascii.Error, InvalidSignature) as error:
        raise UpdateError("The update publisher signature could not be verified.") from error
    try:
        value = json.loads(data.decode("utf-8"), object_pairs_hook=_object)
    except (ValueError, UnicodeError, RecursionError) as error:
        raise UpdateError("The signed update metadata is invalid.") from error
    _fields(value, {"schema", "application", "version", "source_commit", "assets"})
    if type(value["schema"]) is not int or value["schema"] != 1 or value["application"] != APP_ID:
        raise UpdateError("The update is not for this application or schema.")
    _version(value["version"])
    if type(value["source_commit"]) is not str or not re.fullmatch(r"[a-f0-9]{40}", value["source_commit"]):
        raise UpdateError("The update source identity is invalid.")
    if type(value["assets"]) is not list or not 1 <= len(value["assets"]) <= len(_ASSET_FORMATS):
        raise UpdateError("The signed update asset inventory is invalid.")
    assets = tuple(_asset(item, value["version"]) for item in value["assets"])
    if len({asset.name for asset in assets}) != len(assets):
        raise UpdateError("The signed update contains duplicate packages.")
    return UpdateRelease(value["version"], value["source_commit"], assets)


class DownloadCancellation(Event):
    """Interrupt a urllib socket read without waiting for its buffered IO lock.

    The same ownership pattern is used by model acquisition, without importing
    model inventory or model execution into the updater/helper process.
    """

    def __init__(self):
        super().__init__()
        self._interrupt_lock = Lock()
        self._socket = None

    def _interrupt(self):
        if self.is_set() and self._socket is not None:
            try:
                self._socket.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass

    def set(self):
        super().set()
        with self._interrupt_lock:
            self._interrupt()

    @contextmanager
    def response(self, response):
        raw = getattr(getattr(response, "fp", None), "raw", None)
        connection = getattr(raw, "_sock", None)
        with self._interrupt_lock:
            self._socket = connection if isinstance(connection, socket.socket) else None
            self._interrupt()
        try:
            yield
        finally:
            with self._interrupt_lock:
                self._socket = None


def _check_cancel(cancel):
    if cancel is not None and cancel.is_set():
        raise UpdateCancelled("The update operation was cancelled.")


def _trusted_url(url):
    try:
        parsed = urlsplit(url)
        valid = (parsed.scheme == "https" and parsed.hostname in _ALLOWED_HOSTS
                 and parsed.port in (None, 443) and parsed.username is None and parsed.password is None)
    except ValueError:
        valid = False
    if not valid:
        raise UpdateError("The update transport refused an untrusted redirect.")


class _GitHubRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        _trusted_url(newurl)
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def _stream(url, limit, *, cancel=None, expected=None):
    _trusted_url(url)
    _check_cancel(cancel)
    request = urllib.request.Request(url, headers={"User-Agent": "BroTypist-updater/1",
                                                  "Accept-Encoding": "identity"})
    opener = urllib.request.build_opener(_GitHubRedirects())
    try:
        with opener.open(request, timeout=20) as response, (
                cancel.response(response) if isinstance(cancel, DownloadCancellation) else nullcontext()):
            if getattr(response, "status", 200) != 200:
                raise UpdateError("The update server returned an unexpected response.")
            headers = response.headers
            encoding = headers.get("Content-Encoding", "identity")
            if encoding.lower() != "identity":
                raise UpdateError("The update server returned an unsupported encoding.")
            length = headers.get("Content-Length")
            if length is not None:
                try:
                    count = int(length)
                except (ValueError, TypeError) as error:
                    raise UpdateError("The update response length is invalid.") from error
                if count < 0 or count > limit or (expected is not None and count != expected):
                    raise UpdateError("The update response size does not match its allowed bounds.")
            received = 0
            read = getattr(response, "read1", response.read)
            while True:
                _check_cancel(cancel)
                try:
                    chunk = read(min(64 * 1024, limit - received + 1))
                except OSError:
                    _check_cancel(cancel)
                    raise
                _check_cancel(cancel)
                if not chunk:
                    break
                received += len(chunk)
                if received > limit:
                    raise UpdateError("The update response exceeds its allowed size.")
                yield chunk
            _check_cancel(cancel)
    except UpdateError:
        raise
    except Exception as error:
        _check_cancel(cancel)
        raise UpdateError("The update server could not be reached securely.") from error


def fetch_release(*, cancel=None) -> tuple[UpdateRelease, bytes, bytes]:
    data = b"".join(_stream(_FEED, MAX_MANIFEST_BYTES, cancel=cancel))
    signature = b"".join(_stream(_FEED.removesuffix(".json") + ".sig", MAX_SIGNATURE_BYTES, cancel=cancel))
    _check_cancel(cancel)
    return verify_manifest(data, signature), data, signature


def _checked_asset(release, asset):
    _version(release.version)
    if not isinstance(asset, UpdateAsset) or asset not in release.assets:
        raise UpdateError("The selected package is not part of the signed release.")
    return _asset({"name": asset.name, "target": asset.target, "format": asset.format,
                   "size": asset.size, "sha256": asset.sha256}, release.version)


def _mark_web_download(path, source_url, origin_url):
    """Use native quarantine resources; never synthesize or remove xattr flags."""
    if sys.platform != "darwin":
        return
    try:
        from Foundation import NSBundle, NSURL, NSURLQuarantinePropertiesKey
        import objc

        # Use exported system constants without requiring another framework
        # wrapper dependency in either the application or standalone helper.
        bundle = NSBundle.bundleWithPath_("/System/Library/Frameworks/CoreServices.framework")
        if bundle is None or not bundle.load():
            raise UpdateError("The native download security framework is unavailable.")
        constants = {}
        names = ("kLSQuarantineTypeKey", "kLSQuarantineTypeWebDownload",
                 "kLSQuarantineDataURLKey", "kLSQuarantineOriginURLKey")
        objc.loadBundleVariables(bundle, constants, [(name, b"@") for name in names],
                                 skip_undefined=False)
        file_url = NSURL.fileURLWithPath_(str(Path(path).absolute()))
        success, existing, error = file_url.getResourceValue_forKey_error_(
            None, NSURLQuarantinePropertiesKey, None)
        if not success:
            raise UpdateError("The native download security properties could not be read.")
        properties = dict(existing) if existing is not None else {}
        # Preserve native agent, timestamp, event and other existing properties.
        properties[constants["kLSQuarantineTypeKey"]] = constants["kLSQuarantineTypeWebDownload"]
        properties.setdefault(constants["kLSQuarantineDataURLKey"], source_url)
        properties.setdefault(constants["kLSQuarantineOriginURLKey"], origin_url)
        success, error = file_url.setResourceValue_forKey_error_(
            properties, NSURLQuarantinePropertiesKey, None)
        if not success:
            raise UpdateError("The downloaded package could not retain native security metadata.")
    except UpdateError:
        raise
    except Exception as error:
        raise UpdateError("The downloaded package could not retain native security metadata.") from error


def download_asset(release, asset, destination: Path, *, cancel=None, progress=None) -> Path:
    """Stream/hash to a private sibling, then promote only verified complete bytes."""
    _checked_asset(release, asset)
    _check_cancel(cancel)
    destination = Path(destination)
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.is_symlink():
        raise UpdateError("The update destination must not be a symbolic link.")
    fd, temporary = tempfile.mkstemp(prefix=".update-download-", dir=destination.parent)
    try:
        digest = hashlib.sha256()
        received = 0
        url = f"https://github.com/{REPOSITORY}/releases/download/v{release.version}/{asset.name}"
        with os.fdopen(fd, "wb") as output:
            if progress is not None:
                progress(0, asset.size)
            for chunk in _stream(url, asset.size, cancel=cancel, expected=asset.size):
                output.write(chunk)
                digest.update(chunk)
                received += len(chunk)
                if progress is not None:
                    progress(received, asset.size)
            _check_cancel(cancel)
            if received != asset.size or digest.hexdigest() != asset.sha256:
                raise UpdateError("The update package failed integrity verification; the previous file was preserved.")
            output.flush()
            os.fsync(output.fileno())
        _mark_web_download(temporary, url, release.release_url)
        _check_cancel(cancel)
        os.replace(temporary, destination)
        return destination
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def _current_version():
    from . import __version__
    return __version__


def _cache_root():
    home = os.environ.get("INKLOW_HOME")
    if home:
        return Path(home).expanduser() / "updates"
    from platformdirs import user_cache_path
    return user_cache_path("inklow", appauthor=False) / "updates"


def _private_directory(path):
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or (hasattr(os, "getuid") and info.st_uid != os.getuid()):
        raise UpdateError("The update staging directory is not privately owned.")
    if stat.S_IMODE(info.st_mode) & 0o077:
        raise UpdateError("The update staging directory must be accessible only to its owner.")
    return path


def _write_private(path, data):
    fd, temporary = tempfile.mkstemp(prefix=".update-record-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def _recent_check(root, now):
    path = root / "check.json"
    try:
        if path.is_symlink() or path.stat().st_size > 1024:
            return False
        record = json.loads(path.read_bytes())
        checked = record["checked_at"]
        return (type(checked) in (int, float) and math.isfinite(checked)
                and 0 <= now - checked < _CHECK_INTERVAL)
    except (OSError, ValueError, KeyError, TypeError):
        return False


def _target():
    machine = platform.machine().lower()
    if sys.platform == "darwin" and machine in {"arm64", "aarch64"}:
        return "macos-arm64"
    if sys.platform.startswith("linux") and machine in {"x86_64", "amd64"}:
        return "linux-x86_64"
    return None


class UpdateService:
    """One worker, one cancellation owner, and copied scalar-only snapshots."""

    def __init__(self):
        from .update_install import detect_installation
        self._lock = RLock()
        self._worker = None
        self._cancel = None
        self._closed = False
        self._next_automatic_check = 0.0
        self._release = None
        self._manifest = None
        self._signature = None
        self._asset = None
        self._archive_path = None
        self._manifest_path = None
        self._signature_path = None
        self._handoff = None
        self._installation = detect_installation()
        self._root = _cache_root()
        self._status = {"state": "idle", "current_version": _current_version(), "version": "",
                        "message": "Updates have not been checked.", "received": 0, "total": 0,
                        "install_kind": self._installation.kind,
                        "installable": self._installation.installable,
                        "install_reason": self._installation.reason, "release_url": ""}

    def snapshot(self) -> dict:
        with self._lock:
            return dict(self._status)

    def _busy(self):
        return self._worker is not None and self._worker.is_alive()

    def _ensure_open(self):
        if self._closed:
            raise UpdateError("The update service has closed.")

    def _launch(self, operation):
        cancel = self._cancel = DownloadCancellation()

        def run():
            try:
                operation(cancel)
            except Exception:
                with self._lock:
                    if not self._closed:
                        cancelled = cancel.is_set()
                        self._status.update(state="cancelled" if cancelled else "error",
                                            message="The update operation was cancelled." if cancelled else
                                            "The update could not be completed securely. Your installed app was not changed.")

        self._worker = Thread(target=run, daemon=True, name="brotypist-update")
        self._worker.start()

    def start_check(self, *, automatic=False) -> None:
        with self._lock:
            if automatic and (self._closed or time.monotonic() < self._next_automatic_check
                              or self._status["state"] == "ready"):
                return
            self._ensure_open()
            if self._busy() or self._status["state"] == "installing":
                return
            if automatic:
                self._next_automatic_check = time.monotonic() + _CHECK_INTERVAL
            self._status.update(state="checking", message="Checking for signed updates.", received=0, total=0)

            def check(cancel):
                _check_cancel(cancel)
                root = _private_directory(self._root)
                now = time.time()
                if automatic and _recent_check(root, now):
                    with self._lock:
                        _check_cancel(cancel)
                        if not self._closed:
                            state = "available" if self._release is not None and _version(self._release.version) > _version(self._status["current_version"]) else ("current" if self._release is not None else "idle")
                            self._status.update(state=state, message="Automatic updates are checked once per day. Use Check for Updates to check now.")
                    return
                _check_cancel(cancel)
                _write_private(root / "check.json", json.dumps({"checked_at": now}).encode("ascii"))
                release, data, signature = fetch_release(cancel=cancel)
                newer = _version(release.version) > _version(self._status["current_version"])
                with self._lock:
                    _check_cancel(cancel)
                    if self._closed:
                        return
                    self._release, self._manifest, self._signature = release, data, signature
                    self._asset = self._archive_path = None
                    self._status.update(state="available" if newer else "current", version=release.version,
                                        release_url=release.release_url,
                                        message="A signed update is available." if newer else "You are running the latest available version.")

            self._launch(check)

    def _select_asset(self):
        installation = self._installation
        if not installation.installable and installation.kind != "linux_debian":
            raise UpdateError("This installation cannot be updated in place.")
        desired = {"macos_bundle": "zip", "linux_portable": "tar.gz", "linux_debian": "deb"}.get(installation.kind)
        target = _target()
        if desired is None or target is None:
            raise UpdateError("Updates are not supported for this platform.")
        asset = next((asset for asset in self._release.assets if asset.target == target and asset.format == desired), None)
        if asset is None:
            raise UpdateError("This signed release does not contain a compatible update package.")
        return asset

    def start_download(self) -> None:
        with self._lock:
            self._ensure_open()
            if self._busy():
                return
            if (self._status["state"] not in {"available", "error", "cancelled"} or self._release is None
                    or _version(self._release.version) <= _version(self._status["current_version"])):
                raise UpdateError("Check for a newer signed release before downloading.")
            asset = self._select_asset()
            release, data, signature = self._release, self._manifest, self._signature
            # The only source of install metadata remains the authenticated bytes.
            if verify_manifest(data, signature) != release:
                raise UpdateError("The signed release no longer matches the selected update.")
            self._status.update(state="downloading", message="Downloading and verifying the update.", received=0, total=asset.size)

            def download(cancel):
                root = _private_directory(self._root)
                stage = Path(tempfile.mkdtemp(prefix="release-", dir=root))
                manifest_path = stage / "update-manifest.json"
                signature_path = stage / "update-manifest.sig"
                archive_path = stage / asset.name
                try:
                    _write_private(manifest_path, data)
                    _write_private(signature_path, signature)

                    def progress(received, total):
                        with self._lock:
                            if not self._closed and not cancel.is_set():
                                self._status.update(received=received, total=total)

                    download_asset(release, asset, archive_path, cancel=cancel, progress=progress)
                    with self._lock:
                        _check_cancel(cancel)
                        if self._closed:
                            raise UpdateCancelled("The update operation was cancelled.")
                        self._asset, self._archive_path = asset, archive_path
                        self._manifest_path, self._signature_path = manifest_path, signature_path
                        self._status.update(state="ready", message="The verified package is ready. Open the system installer to upgrade and restart BroTypist." if asset.format == "deb" else "The verified update is ready to install and relaunch.")
                except BaseException:
                    for path in (archive_path, signature_path, manifest_path):
                        path.unlink(missing_ok=True)
                    stage.rmdir()
                    raise

            self._launch(download)

    def cancel(self) -> None:
        with self._lock:
            if self._handoff is not None and self._handoff.committed:
                return
            if self._cancel is not None:
                self._cancel.set()
            if self._handoff is not None and self._status["state"] == "installing":
                self._handoff.abort()
                self._handoff = None
                self._status.update(state="ready", message="Installation was cancelled. The verified update is still ready.")

    def _settle_worker(self):
        with self._lock:
            worker = self._worker
        if worker is not None and worker.is_alive():
            worker.join(0.25)
            if worker.is_alive():
                from .runtime import ShutdownPending
                raise ShutdownPending("Finishing cancellation of the update operation.")

    def prepare_install(self):
        # A successful handoff must not leave a worker that could make the
        # post-commit close fail. Never join while holding its progress lock.
        self._settle_worker()
        with self._lock:
            self._ensure_open()
            if self._status["state"] != "ready" or self._asset is None or self._asset.format == "deb":
                raise UpdateError("A verified self-installable update must be ready first.")
            from .update_install import detect_installation, spawn_installer
            current = detect_installation()
            if current != self._installation or not current.installable:
                raise UpdateError("The installation identity or update policy changed. Check for updates again.")
            try:
                handoff = spawn_installer(self._manifest_path, self._signature_path, self._archive_path, current)
            except Exception as error:
                self._status.update(message="The installer could not be prepared. The existing app was not changed.")
                raise UpdateError("The installer could not be prepared safely.") from error
            self._handoff = handoff
            self._status.update(state="installing", message="Waiting for approved app shutdown before installing.")
            return handoff

    def open_package_installer(self) -> None:
        with self._lock:
            self._ensure_open()
            if (self._status["state"] != "ready" or self._installation.kind != "linux_debian"
                    or self._asset is None or self._asset.format != "deb"):
                raise UpdateError("A verified Debian package must be ready first.")
            # No shell, elevation or installation claim: the package manager
            # owns consent, privileges, replacement and the user's restart.
            try:
                release = verify_manifest(self._manifest_path.read_bytes(), self._signature_path.read_bytes())
                if release != self._release or self._asset not in release.assets:
                    raise UpdateError("The update identity changed.")
                path = self._archive_path
                info = path.lstat()
                if not stat.S_ISREG(info.st_mode) or info.st_size != self._asset.size:
                    raise UpdateError("The package changed after verification.")
                digest = hashlib.sha256()
                with path.open("rb") as source:
                    for chunk in iter(lambda: source.read(1024 * 1024), b""):
                        digest.update(chunk)
                if digest.hexdigest() != self._asset.sha256:
                    raise UpdateError("The package changed after verification.")
                subprocess.Popen(["xdg-open", str(path.resolve())], stdin=subprocess.DEVNULL,
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, close_fds=True)
            except Exception as error:
                raise UpdateError("The verified package could not be opened in the system installer.") from error
            self._status.update(message="Opening the verified package was requested. Complete the upgrade in your system installer, then restart BroTypist.")

    def close(self) -> None:
        with self._lock:
            self._closed = True
            if self._cancel is not None:
                self._cancel.set()
            if self._handoff is not None:
                # The installer's abort is explicitly a no-op after commit;
                # its writer then remains alive until actual process exit.
                self._handoff.abort()
        self._settle_worker()
