"""Unprivileged, offline OTA handoff and atomic native installation.

Handoff copies the inputs and helper outside the running app. The helper waits
for approval, pipe closure and parent exit; it never invokes a shell or changes
Gatekeeper. Models and user settings are not part of the replacement payload.
"""
from __future__ import annotations

import argparse
import ctypes
import errno
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import plistlib
import re
import secrets
import select
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
from dataclasses import dataclass
import zipfile

from .update_keys import APP_ID

MARKER = ".inklow-build.json"
MAX_PACKAGE = 512 * 1024 * 1024
MAX_UNPACKED = 2 * 1024 * 1024 * 1024
MAX_ENTRIES = 20000
APPROVAL_TIMEOUT = 300
STARTUP_TIMEOUT = 60
HEALTH_PATH = "INKLOW_UPDATE_HEALTH_RECEIPT"
HEALTH_NONCE = "INKLOW_UPDATE_HEALTH_NONCE"
# Deliberately retain approved writers even if the GUI/service drops its handle.
# Only actual process exit closes these descriptors; do not add an atexit closer.
_COMMITTED_WRITERS: set[int] = set()
_FORBIDDEN_SUFFIXES = {".gguf", ".ggml", ".safetensors", ".pt", ".pth", ".db", ".sqlite", ".sqlite3"}
_FORBIDDEN_NAMES = {"settings.json", "config.json", "secrets.yaml", ".env", "history.enc", ".git"}


class InstallError(RuntimeError):
    """A fixed, user-safe installer failure; never includes raw input paths."""


@dataclass(frozen=True)
class Installation:
    """Detected layout and replacement policy, not a promise that install will succeed."""
    path: Path | None
    kind: str
    installable: bool
    reason: str


def _architecture() -> str:
    machine = platform.machine().lower()
    return {"aarch64": "arm64", "amd64": "x86_64"}.get(machine, machine)


def _version(value: str) -> tuple[int, int, int]:
    if not isinstance(value, str) or not re.fullmatch(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", value):
        raise InstallError("Invalid application version.")
    return tuple(map(int, value.split(".")))


def _json(data: bytes) -> dict:
    """Decode object metadata without accepting duplicate keys at any nesting level."""
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:
                raise InstallError("Duplicate installer metadata field.")
            result[key] = value
        return result
    try:
        value = json.loads(data, object_pairs_hook=pairs)
    except (ValueError, UnicodeError) as error:
        raise InstallError("Invalid installer metadata.") from error
    if not isinstance(value, dict):
        raise InstallError("Invalid installer metadata.")
    return value


def _read_file(path: Path, maximum: int, *, private: bool = False) -> bytes:
    """Read at most maximum bytes from a regular file, refusing a final symlink.

    With ``private=True``, require the effective user to own an owner-only file.
    Parent-component checks are a separate responsibility of the caller.
    """
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_size > maximum:
            raise InstallError("Unsafe installer input.")
        if private and (info.st_uid != os.geteuid() or info.st_mode & 0o077):
            raise InstallError("Installer input must be private.")
        with os.fdopen(fd, "rb", closefd=False) as stream:
            data = stream.read(maximum + 1)
        if len(data) > maximum:
            raise InstallError("Installer input exceeds its limit.")
        return data
    finally:
        os.close(fd)


def _write_private(path: Path, value: dict | bytes) -> None:
    """Create an exclusive owner-only file and fsync its bytes; never overwrite."""
    data = json.dumps(value, separators=(",", ":")).encode() if isinstance(value, dict) else value
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "wb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())


def _marker(bundle: Path, native: str) -> dict:
    """Read and validate the native build marker without authenticating its publisher."""
    path = bundle / "Contents/Resources" / MARKER if native == "darwin" else bundle / MARKER
    _no_symlink_components(path)
    value = _json(_read_file(path, 8192))
    if (set(value) != {"application", "platform", "version", "source_commit", "architecture"}
            or value.get("application") != APP_ID or value.get("platform") != native
            or value.get("architecture") != _architecture()
            or not isinstance(value.get("source_commit"), str)
            or not re.fullmatch(r"[0-9a-f]{40}", value["source_commit"])):
        raise InstallError("Application build identity does not match this computer.")
    _version(value.get("version"))
    return value


def _no_symlink_components(path: Path) -> None:
    """Require an absolute path with no parent traversal or existing symlink component."""
    if not path.is_absolute() or ".." in path.parts:
        raise InstallError("Application path must be canonical.")
    for part in (path, *path.parents):
        if part.is_symlink():
            raise InstallError("Linked application paths cannot be updated.")


def _executable(bundle: Path, native: str) -> Path:
    return bundle / "Contents/MacOS/BroTypist" if native == "darwin" else bundle / "BroTypist"


def _managed_debian(path: Path) -> bool:
    """Keep the conventional Debian location and dpkg-owned binaries out of self-update."""
    if path == Path("/opt/brotypist") or Path("/opt/brotypist") in path.parents:
        return True
    tool = Path("/usr/bin/dpkg-query")
    if tool.is_file():
        result = subprocess.run([str(tool), "-S", str(path / "BroTypist")],
                                capture_output=True, timeout=10, check=False)
        return result.returncode == 0
    return False


def _target_policy(bundle: Path, native: str, isolated_home: str | None) -> dict:
    """Require a writable native app outside source trees and return its build identity."""
    _no_symlink_components(bundle)
    info = bundle.lstat()
    parent = bundle.parent.stat()
    if (not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid()
            or info.st_mode & 0o022 or parent.st_mode & 0o002
            or parent.st_mode & 0o020 and parent.st_gid not in os.getgroups()
            or parent.st_uid not in {0, os.geteuid()}
            or not os.access(bundle.parent, os.W_OK | os.X_OK)):
        raise InstallError("Application directory is not privately writable.")
    if any((part / ".git").exists() for part in (bundle, *bundle.parents)):
        raise InstallError("Source checkouts cannot replace themselves.")
    if native == "linux" and _managed_debian(bundle):
        raise InstallError("Use the system package installer for this installation.")
    if isolated_home:
        home = Path(isolated_home).expanduser().resolve(strict=True)
        # The root's settings.json must remain outside the executable payload.
        if bundle == home or not bundle.is_relative_to(home):
            raise InstallError("An isolated profile can update only its own QA application copy.")
    identity = _marker(bundle, native)
    executable = _executable(bundle, native)
    _no_symlink_components(executable)
    if not executable.is_file() or not os.access(executable, os.X_OK):
        raise InstallError("Native application executable is missing.")
    _binary_architecture(executable, native)
    return identity


def detect_installation() -> Installation:
    """Classify the running executable without mutating it or claiming install success."""
    if sys.platform not in {"darwin", "linux"}:
        return Installation(None, "held", False, "Updates are not available for this platform.")
    if not getattr(sys, "frozen", False):
        return Installation(None, "source", False, "Source checkouts must be updated from source.")
    executable = Path(sys.executable).absolute()
    if sys.platform == "darwin":
        if executable.name != "BroTypist" or executable.parent.name != "MacOS" or executable.parent.parent.name != "Contents":
            return Installation(None, "held", False, "Unrecognized native application layout.")
        bundle = executable.parents[2]
        kind = "macos_bundle"
    else:
        bundle = executable.parent
        kind = "linux_portable"
        try:
            if _managed_debian(bundle):
                return Installation(bundle, "linux_debian", False, "Open the downloaded Debian package in your system installer; restart afterwards.")
        except (OSError, subprocess.SubprocessError):
            return Installation(bundle, "read_only", False, "Package ownership could not be established safely.")
    try:
        _target_policy(bundle, sys.platform, os.environ.get("INKLOW_HOME"))
    except (OSError, ValueError, subprocess.SubprocessError, InstallError):
        return Installation(bundle, "read_only", False, "This app cannot replace itself here. Use a writable native copy; isolated profiles can update only their own QA copy.")
    return Installation(bundle, kind, True, "Install and relaunch this native application.")


class Handoff:
    """Own the parent's approval pipe until abort or irrevocable commit.

    Commit retains the writer until process exit: approval alone must not let
    the helper replace a still-running application.
    """
    def __init__(self, writer: int, process: subprocess.Popen, request_root: Path):
        """Take ownership of writer; the detached helper owns the pipe's read end."""
        self._writer: int | None = writer
        self.process = process
        self.request_root = request_root
        self.committed = False

    def commit(self) -> None:
        """Send approval once and retain the writer for the rest of this process."""
        if self.committed:
            return
        if self._writer is None or self.process.poll() is not None:
            raise InstallError("Update helper is no longer available.")
        try:
            if os.write(self._writer, b"1") != 1:
                raise InstallError("Update approval could not be delivered.")
        except OSError as error:
            raise InstallError("Update helper is no longer available.") from error
        self.committed = True
        _COMMITTED_WRITERS.add(self._writer)

    def abort(self) -> None:
        """Close an uncommitted pipe to cancel; never revoke a committed handoff."""
        if self.committed:
            return
        if self._writer is not None:
            os.close(self._writer)
            self._writer = None

    def __del__(self):
        self.abort()


def _copy_private(source: Path, destination: Path, maximum: int) -> None:
    """Copy bounded regular-file bytes privately and retain native quarantine metadata."""
    fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or not 0 < info.st_size <= maximum:
            raise InstallError("Invalid update input size.")
        out = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "rb", closefd=False) as reader, os.fdopen(out, "wb") as writer:
            remaining = maximum + 1
            while remaining:
                block = reader.read(min(1024 * 1024, remaining))
                if not block:
                    break
                writer.write(block)
                remaining -= len(block)
            if not remaining:
                raise InstallError("Update input grew beyond its limit.")
            writer.flush()
            os.fsync(writer.fileno())
        _preserve_quarantine(source, destination)
    finally:
        os.close(fd)


def _darwin_getxattr(path: Path, name: str) -> bytes:
    """Read a bounded native Darwin xattr without following the final symlink.

    Darwin's six-argument ABI differs from Linux's Python os.xattr API.
    sys/xattr.h defines XATTR_NOFOLLOW=0x0001; no security-bypass flags are used.
    """
    function = ctypes.CDLL(None, use_errno=True).getxattr
    function.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_void_p,
                         ctypes.c_size_t, ctypes.c_uint32, ctypes.c_int]
    function.restype = ctypes.c_ssize_t
    encoded_path, encoded_name = os.fsencode(path), name.encode("utf-8")
    size = function(encoded_path, encoded_name, None, 0, 0, 0x0001)
    if size < 0:
        raise OSError(ctypes.get_errno(), "Could not read native extended attribute")
    if size > 64 * 1024:
        raise InstallError("Native quarantine metadata exceeds its limit.")
    buffer = ctypes.create_string_buffer(size)
    received = function(encoded_path, encoded_name, buffer, size, 0, 0x0001)
    if received < 0:
        raise OSError(ctypes.get_errno(), "Could not read native extended attribute")
    if received > size:
        raise InstallError("Native extended attribute changed while reading.")
    return buffer.raw[:received]


def _darwin_setxattr(path: Path, name: str, value: bytes, *, create: bool = False) -> None:
    """Write using the documented Darwin ABI; optionally forbid replacement."""
    function = ctypes.CDLL(None, use_errno=True).setxattr
    function.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_void_p,
                         ctypes.c_size_t, ctypes.c_uint32, ctypes.c_int]
    function.restype = ctypes.c_int
    buffer = ctypes.create_string_buffer(value, len(value))
    # XATTR_NOFOLLOW | XATTR_CREATE (when retaining an existing provenance).
    options = 0x0001 | (0x0002 if create else 0)
    if function(os.fsencode(path), name.encode("utf-8"), buffer, len(value), 0, options):
        raise OSError(ctypes.get_errno(), "Could not preserve native extended attribute")


def _preserve_quarantine(source: Path, destination: Path) -> None:
    """Copy Darwin quarantine only when present and absent from the destination."""
    if sys.platform != "darwin":
        return
    missing = {errno.ENODATA, getattr(errno, "ENOATTR", 93)}
    try:
        value = _darwin_getxattr(source, "com.apple.quarantine")
    except OSError as error:
        if error.errno in missing:
            return
        raise
    try:
        _darwin_setxattr(destination, "com.apple.quarantine", value, create=True)
    except OSError as error:
        if error.errno != errno.EEXIST:
            raise


def spawn_installer(manifest_path: Path, signature_path: Path, archive_path: Path,
                    installation: Installation) -> Handoff:
    """Copy a private transaction and launch its detached, initially unapproved helper.

    Recheck the running installation before staging next to it. The caller owns
    the returned Handoff and must commit or abort it; no replacement occurs here.
    """
    if not installation.installable or installation.path is None:
        raise InstallError("This installation does not support self-updating.")
    actual = detect_installation()
    if actual != installation:
        raise InstallError("Application installation changed before updating.")
    bundle = installation.path
    native = sys.platform
    identity = _target_policy(bundle, native, os.environ.get("INKLOW_HOME"))
    helper = Path(getattr(sys, "_MEIPASS", "")) / "helpers/BroTypist-updater"
    if not getattr(sys, "frozen", False) or helper.is_symlink() or not helper.is_file():
        raise InstallError("Standalone update helper is missing.")
    root = Path(tempfile.mkdtemp(prefix=".brotypist-update-", dir=bundle.parent))
    root.chmod(0o700)
    read_fd = write_fd = None
    try:
        _copy_private(manifest_path, root / "manifest.json", 256 * 1024)
        _copy_private(signature_path, root / "manifest.sig", 128)
        _copy_private(archive_path, root / "package", MAX_PACKAGE)
        _copy_private(helper, root / "BroTypist-updater", 128 * 1024 * 1024)
        (root / "BroTypist-updater").chmod(0o700)
        from .updates import verify_manifest
        release = verify_manifest(_read_file(root / "manifest.json", 256 * 1024),
                                  _read_file(root / "manifest.sig", 128))
        if _version(release.version) <= _version(identity["version"]):
            raise InstallError("An update must be newer than the installed version.")
        info = bundle.stat()
        request = {"schema": 1, "destination": str(bundle), "platform": native,
                   "identity": identity, "device": info.st_dev, "inode": info.st_ino,
                   "uid": os.geteuid(), "parent_pid": os.getpid(),
                   "isolated_home": os.environ.get("INKLOW_HOME"),
                   "nonce": secrets.token_hex(32), "version": release.version}
        _write_private(root / "request.json", request)
        read_fd, write_fd = os.pipe()
        process = subprocess.Popen([str(root / "BroTypist-updater"), "--request", str(root / "request.json"),
                                    "--approval-fd", str(read_fd)], pass_fds=(read_fd,),
                                   stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL, start_new_session=True, cwd=root,
                                   env={**_clean_environment(), "TMPDIR": str(root)})
        os.close(read_fd)
        read_fd = None
        return Handoff(write_fd, process, root)
    except BaseException:
        if read_fd is not None:
            os.close(read_fd)
        if write_fd is not None:
            os.close(write_fd)
        shutil.rmtree(root)
        raise


def _clean_environment() -> dict[str, str]:
    # Never carry Python injection knobs, inherited health tokens or loader paths.
    environment = {key: value for key, value in os.environ.items()
                   if key not in {"TMPDIR", "TEMP", "TMP"}
                   and not key.startswith(("PYTHON", "DYLD_", "LD_", "_PYI_", "_MEI", "INKLOW_UPDATE_HEALTH_"))}
    environment["PYINSTALLER_RESET_ENVIRONMENT"] = "1"
    return environment


def _wait_for_exit(fd: int, parent_pid: int, timeout: float = APPROVAL_TIMEOUT) -> None:
    """Require one approval byte, pipe EOF and parent exit within timeout seconds."""
    import fcntl
    if (not stat.S_ISFIFO(os.fstat(fd).st_mode)
            or fcntl.fcntl(fd, fcntl.F_GETFL) & os.O_ACCMODE != os.O_RDONLY):
        raise InstallError("Update approval requires a private pipe.")
    deadline = time.monotonic() + timeout
    received = b""
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([fd], [], [], max(0, remaining))[0]:
            raise InstallError("Update approval timed out; the original app was not changed.")
        block = os.read(fd, 2)
        if not block:
            break
        received += block
        if received != b"1":
            raise InstallError("Update was not approved.")
    if received != b"1":
        raise InstallError("Update was cancelled.")
    # EOF is necessary, but a caller closing a descriptor early is not exit.
    while True:
        try:
            os.kill(parent_pid, 0)
        except ProcessLookupError:
            return
        except PermissionError as error:
            raise InstallError("Cannot establish that the previous app exited.") from error
        if time.monotonic() >= deadline:
            raise InstallError("The previous application did not exit.")
        time.sleep(0.05)


def _request(path: Path) -> dict:
    """Validate the private transaction location, schema and same-parent destination."""
    _no_symlink_components(path)
    root = path.parent
    info = root.stat()
    if (not root.name.startswith(".brotypist-update-") or info.st_uid != os.geteuid()
            or info.st_mode & 0o077 or not stat.S_ISDIR(info.st_mode)
            or path.name != "request.json"):
        raise InstallError("Invalid private update transaction.")
    value = _json(_read_file(path, 16384, private=True))
    keys = {"schema", "destination", "platform", "identity", "device", "inode", "uid",
            "parent_pid", "isolated_home", "nonce", "version"}
    if (set(value) != keys or type(value["schema"]) is not int or value["schema"] != 1 or value["uid"] != os.geteuid()
            or value["platform"] != sys.platform or not isinstance(value["destination"], str)
            or not isinstance(value["nonce"], str) or not re.fullmatch(r"[0-9a-f]{64}", value["nonce"])
            or any(type(value[key]) is not int or value[key] <= 0 for key in ("device", "inode", "parent_pid"))
            or value["isolated_home"] is not None and not isinstance(value["isolated_home"], str)):
        raise InstallError("Invalid update transaction identity.")
    destination = Path(value["destination"])
    if destination.parent != root.parent:
        raise InstallError("Update transaction is not beside its application.")
    _version(value["version"])
    return value


def _check_destination(request: dict) -> Path:
    """Recheck replacement policy and the original build's device/inode identity."""
    bundle = Path(request["destination"])
    identity = _target_policy(bundle, request["platform"], request["isolated_home"])
    info = bundle.stat()
    if (identity != request["identity"] or info.st_dev != request["device"]
            or info.st_ino != request["inode"]):
        raise InstallError("The installed application changed during the update.")
    return bundle


def _verified_asset(root: Path, request: dict):
    """Reauthenticate metadata and hash the private native package before extraction."""
    from .updates import verify_manifest
    release = verify_manifest(_read_file(root / "manifest.json", 256 * 1024, private=True),
                              _read_file(root / "manifest.sig", 128, private=True))
    if release.version != request["version"] or _version(release.version) <= _version(request["identity"]["version"]):
        raise InstallError("Update would not advance the installed version.")
    target = ("macos-" if sys.platform == "darwin" else "linux-") + _architecture()
    format_name = "zip" if sys.platform == "darwin" else "tar.gz"
    assets = [asset for asset in release.assets if asset.target == target and asset.format == format_name]
    if len(assets) != 1:
        raise InstallError("Release has no unique native update package.")
    asset = assets[0]
    fd = os.open(root / "package", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    digest = hashlib.sha256()
    with os.fdopen(fd, "rb") as stream:
        info = os.fstat(stream.fileno())
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid()
                or info.st_mode & 0o077 or info.st_size != asset.size or info.st_size > MAX_PACKAGE):
            raise InstallError("Update package size does not match its signed manifest.")
        received = 0
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            received += len(block)
            if received > asset.size:
                raise InstallError("Update package grew beyond its signed size.")
            digest.update(block)
    if received != asset.size or digest.hexdigest() != asset.sha256:
        raise InstallError("Update package hash does not match its signed manifest.")
    return release, asset


def _archive_path(name: str, expected_root: str) -> PurePosixPath:
    """Admit only application-root paths, excluding traversal and private-data names."""
    path = PurePosixPath(name)
    if (not name or "\x00" in name or "\\" in name or path.is_absolute()
            or any(part in {"", ".", ".."} for part in name.rstrip("/").split("/"))
            or path.parts[0] != expected_root):
        raise InstallError("Update archive contains an unsafe path.")
    if any(part.casefold() in _FORBIDDEN_NAMES for part in path.parts) or path.suffix.casefold() in _FORBIDDEN_SUFFIXES:
        raise InstallError("Update archives cannot contain models or private user data.")
    return path


def _link_target(path: PurePosixPath, link: str, root: str) -> PurePosixPath:
    """Resolve a relative archive link lexically without escaping its application root."""
    if not link or link.startswith("/") or "\\" in link or "\x00" in link:
        raise InstallError("Update archive contains an unsafe link.")
    parts = list(path.parent.parts)
    for part in link.split("/"):
        if part == "..":
            if len(parts) <= 1:
                raise InstallError("Update archive link escapes its application.")
            parts.pop()
        elif part not in {"", "."}:
            parts.append(part)
    if not parts or parts[0] != root:
        raise InstallError("Update archive link escapes its application.")
    return PurePosixPath(*parts)


def _validate_members(rows: list[tuple[PurePosixPath, str | None, bool]]) -> None:
    """Reject case-insensitive duplicates and any member nested beneath a link or file."""
    names = set()
    links = set()
    files = set()
    for path, link, directory in rows:
        folded = str(path).casefold()
        if folded in names:
            raise InstallError("Update archive has duplicate paths.")
        names.add(folded)
        if link is not None:
            links.add(folded)
        elif not directory:
            files.add(folded)
    for path, _, _ in rows:
        if any(str(parent).casefold() in links or str(parent).casefold() in files for parent in path.parents):
            raise InstallError("Update archive traverses a link or non-directory.")


def _extract_package(archive: Path, stage: Path, native: str) -> Path:
    """Preflight an archive, extract into private stage, then inspect resulting files.

    ZIP uses native ditto to preserve bundle metadata; tar uses the data filter.
    Both paths enforce entry/byte limits and application-root link containment.
    Returns the extracted bundle, not a publisher-authenticated installation.
    """
    root_name = "BroTypist.app" if native == "darwin" else "BroTypist"
    rows = []
    total = 0
    if native == "darwin":
        with zipfile.ZipFile(archive) as stream:
            entries = stream.infolist()
            if len(entries) > MAX_ENTRIES:
                raise InstallError("Update archive has too many entries.")
            for entry in entries:
                # ditto's AppleDouble metadata is confined to this application's tree.
                total += entry.file_size
                if total > MAX_UNPACKED:
                    raise InstallError("Update archive exceeds its unpacked limit.")
                name = entry.filename
                metadata = name.startswith("__MACOSX/")
                if metadata:
                    stripped = name.removeprefix("__MACOSX/")
                    if stripped in {"", "BroTypist.app/"}:
                        if not entry.is_dir() or entry.file_size:
                            raise InstallError("Invalid archive metadata directory.")
                        continue
                    name = stripped
                path = _archive_path(name, root_name)
                mode = entry.external_attr >> 16
                kind = stat.S_IFMT(mode)
                if entry.flag_bits & 1 or kind not in {0, stat.S_IFREG, stat.S_IFDIR, stat.S_IFLNK} or mode & 0o6000:
                    raise InstallError("Unsupported update archive entry.")
                link = None
                if kind == stat.S_IFLNK:
                    if metadata or entry.file_size > 4096:
                        raise InstallError("Invalid update archive link.")
                    try:
                        link = stream.read(entry).decode("utf-8")
                    except UnicodeError as error:
                        raise InstallError("Invalid update archive link.") from error
                    _link_target(path, link, root_name)
                else:
                    # Check actual decompression/CRC before native extraction,
                    # rather than trusting only the ZIP directory's sizes.
                    actual_size = 0
                    with stream.open(entry) as contents:
                        for block in iter(lambda: contents.read(1024 * 1024), b""):
                            actual_size += len(block)
                            if actual_size > entry.file_size:
                                raise InstallError("Invalid archive entry size.")
                    if actual_size != entry.file_size:
                        raise InstallError("Invalid archive entry size.")
                if not metadata:
                    rows.append((path, link, entry.is_dir()))
            _validate_members(rows)
        subprocess.run(["/usr/bin/ditto", "-x", "-k", str(archive), str(stage)],
                       check=True, capture_output=True, timeout=180)
    else:
        with tarfile.open(archive, "r:gz") as stream:
            entries = []
            for entry in stream:
                if len(entries) >= MAX_ENTRIES:
                    raise InstallError("Update archive has too many entries.")
                path = _archive_path(entry.name, root_name)
                if (not (entry.isdir() or entry.isfile() or entry.issym())
                        or entry.mode & 0o6000 or entry.sparse is not None):
                    raise InstallError("Unsupported update archive entry.")
                total += entry.size
                if total > MAX_UNPACKED:
                    raise InstallError("Update archive exceeds its unpacked limit.")
                link = entry.linkname if entry.issym() else None
                if link is not None:
                    _link_target(path, link, root_name)
                rows.append((path, link, entry.isdir()))
                entries.append(entry)
            _validate_members(rows)
            # data_filter additionally strips dangerous ownership/permission bits.
            stream.extractall(stage, members=entries, filter="data")
    bundle = stage / root_name
    if (not bundle.is_dir() or bundle.is_symlink()
            or bundle.stat().st_uid != os.geteuid() or bundle.stat().st_mode & 0o022):
        raise InstallError("Update archive has no native application.")
    # Validate what extraction actually produced, not only the archive inventory.
    count = total = 0
    for path in bundle.rglob("*"):
        count += 1
        info = path.lstat()
        if count > MAX_ENTRIES or not (stat.S_ISREG(info.st_mode) or stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode)):
            raise InstallError("Invalid extracted application payload.")
        if not path.is_symlink() and (info.st_uid != os.geteuid() or info.st_mode & 0o022):
            raise InstallError("Extracted application permissions are unsafe.")
        _archive_path(path.relative_to(stage).as_posix(), root_name)
        if path.is_symlink():
            _link_target(PurePosixPath(path.relative_to(stage).as_posix()), os.readlink(path), root_name)
            if not path.resolve().is_relative_to(bundle.resolve()):
                raise InstallError("Extracted link escapes the application.")
        elif path.is_file():
            total += info.st_size
            if total > MAX_UNPACKED:
                raise InstallError("Extracted application exceeds its size limit.")
    return bundle


def _signing_identity(bundle: Path) -> tuple[str, str, tuple[str, ...]]:
    """Verify macOS code seals and return identifier/team/authorities, not notarization."""
    subprocess.run(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(bundle)],
                   capture_output=True, check=True, timeout=60)
    result = subprocess.run(["/usr/bin/codesign", "--display", "--verbose=4", str(bundle)],
                            capture_output=True, check=True, timeout=30, text=True)
    lines = (result.stdout + result.stderr).splitlines()
    identifier = next((line.partition("=")[2] for line in lines if line.startswith("Identifier=")), "")
    team = next((line.partition("=")[2] for line in lines if line.startswith("TeamIdentifier=")), "")
    authorities = tuple(line.partition("=")[2] for line in lines if line.startswith("Authority="))
    if identifier != APP_ID or not team or team == "not set" or not authorities:
        raise InstallError("Updates require the existing identity-signed application.")
    with (bundle / "Contents/Info.plist").open("rb") as stream:
        info = plistlib.load(stream)
    if info.get("CFBundleIdentifier") != APP_ID or info.get("CFBundleExecutable") != "BroTypist":
        raise InstallError("Unexpected native bundle identity.")
    return identifier, team, authorities


def _binary_architecture(executable: Path, native: str) -> None:
    """Require the host architecture in a thin Mach-O or little-endian ELF64 header."""
    header = _read_header(executable)
    expected = _architecture()
    if native == "linux":
        actual = {62: "x86_64", 183: "arm64"}.get(int.from_bytes(header[18:20], "little"))
        valid = header[:6] == b"\x7fELF\x02\x01"
    else:
        actual = {0x1000007: "x86_64", 0x100000C: "arm64"}.get(int.from_bytes(header[4:8], "little"))
        valid = header[:4] == b"\xcf\xfa\xed\xfe"
    if not valid or actual != expected:
        raise InstallError("Native executable architecture does not match this computer.")


def _read_header(path: Path) -> bytes:
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd, "rb") as stream:
        return stream.read(64)


def _health_version(bundle: Path, version: str, native: str) -> None:
    """Check the signed release version via sealed macOS plist or Linux CLI execution."""
    if native == "darwin":
        # codesign has verified this sealed plist. Do not execute quarantined
        # code for a CLI probe before LaunchServices can request OS approval.
        info = plistlib.loads(_read_file(bundle / "Contents/Info.plist", 64 * 1024))
        if (not isinstance(info, dict) or info.get("CFBundleShortVersionString") != version
                or info.get("CFBundleVersion") != version):
            raise InstallError("Updated application's sealed bundle version does not match the signed release.")
        return
    result = subprocess.run([str(_executable(bundle, native)), "--version"],
                            capture_output=True, timeout=30, check=True, env=_clean_environment(), cwd=bundle.parent)
    if result.stdout.decode("utf-8").strip() != f"BroTypist {version}":
        raise InstallError("Updated executable did not report the signed version.")


def _atomic_exchange(left: Path, right: Path) -> None:
    """Swap two paths atomically or fail; no non-atomic replacement fallback is used."""
    library = ctypes.CDLL(None, use_errno=True)
    if sys.platform == "darwin":
        function = getattr(library, "renameatx_np", None)
        flags = 0x00000002  # RENAME_SWAP
    elif sys.platform == "linux":
        function = getattr(library, "renameat2", None)
        flags = 2  # RENAME_EXCHANGE
    else:
        function = None
        flags = 0
    if function is None:
        raise InstallError("This filesystem has no supported atomic application exchange.")
    function.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
    function.restype = ctypes.c_int
    at_fdcwd = -2 if sys.platform == "darwin" else -100
    if function(at_fdcwd, os.fsencode(left), at_fdcwd, os.fsencode(right), flags):
        raise OSError(ctypes.get_errno(), "Atomic application exchange failed")


def _launch(bundle: Path, native: str, root: Path | None = None, request: dict | None = None):
    """Launch a native app, optionally carrying a one-transaction startup receipt token."""
    environment = _clean_environment()
    if root is not None and request is not None:
        environment[HEALTH_PATH] = str(root / "startup.json")
        environment[HEALTH_NONCE] = request["nonce"]
    if native == "darwin":
        command = ["/usr/bin/open", "-n", "-a", str(bundle)]
        # LaunchServices does not inherit arbitrary caller environment variables.
        for key in ("INKLOW_HOME", HEALTH_PATH, HEALTH_NONCE):
            if key in environment:
                command += ["--env", f"{key}={environment[key]}"]
    else:
        command = [str(_executable(bundle, native))]
    return subprocess.Popen(command, env=environment, stdin=subprocess.DEVNULL,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                            cwd=bundle.parent, start_new_session=True)


def acknowledge_startup() -> bool:
    """Called only by a native GUI after its first main-loop iteration.

    Consume tokens before a model subprocess can inherit them. A hostile env var
    cannot select an arbitrary write: both the owned request and installed build
    must agree, and the receipt has one fixed basename in a private transaction.
    """
    receipt = os.environ.pop(HEALTH_PATH, None)
    nonce = os.environ.pop(HEALTH_NONCE, None)
    if not receipt or not nonce:
        return False
    try:
        path = Path(receipt)
        if path.name != "startup.json" or not getattr(sys, "frozen", False):
            return False
        request = _request(path.parent / "request.json")
        if not secrets.compare_digest(nonce, request["nonce"]):
            return False
        bundle = Path(request["destination"])
        if Path(sys.executable).absolute() != _executable(bundle, request["platform"]):
            return False
        identity = _marker(bundle, request["platform"])
        from .updates import verify_manifest
        release = verify_manifest(_read_file(path.parent / "manifest.json", 256 * 1024, private=True),
                                  _read_file(path.parent / "manifest.sig", 128, private=True))
        transaction = _json(_read_file(path.parent / "transaction.json", 4096, private=True))
        if (identity["version"] != request["version"] or identity["version"] != release.version
                or identity["source_commit"] != release.source_commit
                or transaction.get("status") != "promoted"
                or transaction.get("version") != release.version):
            return False
        temporary = path.with_name(".startup-" + secrets.token_hex(16))
        try:
            _write_private(temporary, {"nonce": nonce, "version": identity["version"], "pid": os.getpid()})
            # Publish complete bytes atomically without replacing an existing
            # acknowledgment. Readers never observe a partially written file.
            os.link(temporary, path, follow_symlinks=False)
        finally:
            temporary.unlink(missing_ok=True)
        return True
    except (OSError, ValueError, RuntimeError):
        return False


def _wait_startup(root: Path, request: dict, process, timeout: float = STARTUP_TIMEOUT) -> None:
    """Wait for a nonce-bound receipt and live PID, not merely launcher success."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        path = root / "startup.json"
        if path.exists():
            ack = _json(_read_file(path, 1024, private=True))
            if (set(ack) != {"nonce", "version", "pid"} or ack.get("nonce") != request["nonce"]
                    or ack.get("version") != request["version"] or type(ack.get("pid")) is not int
                    or ack["pid"] <= 0):
                raise InstallError("Invalid native startup acknowledgment.")
            try:
                os.kill(ack["pid"], 0)
            except OSError as error:
                raise InstallError("Updated application exited during startup.") from error
            return
        result = process.poll()
        if result is not None and (request["platform"] != "darwin" or result != 0):
            raise InstallError("The updated application could not start.")
        time.sleep(0.1)
    raise InstallError("Native startup was not acknowledged; macOS may require per-app approval. The previous application was restored.")


def _receipt(root: Path, request: dict, status: str, *, rollback: Path | None = None,
             message: str | None = None) -> None:
    """Atomically publish transaction state while retaining the rollback path, if any."""
    value = {"schema": 1, "application": APP_ID, "version": request["version"], "status": status,
             "previous_version": request["identity"]["version"], "notarization": "not claimed"}
    if message:
        value["message"] = message[:512]
    if rollback is not None:
        value["rollback"] = str(rollback)
    temporary = root / "receipt.new"
    _write_private(temporary, value)
    os.replace(temporary, root / "transaction.json")


def run_installer(request_path: Path, approval_fd: int) -> int:
    """Run an offline replacement and startup handshake, attempting rollback on failure.

    Returns 0 after acknowledged GUI startup or 1 for a handled failure.
    Invalid requests and failures during rollback/cleanup may propagate.
    Once inside the transaction try/finally, this function owns approval_fd.
    """
    request = _request(request_path)
    root = request_path.parent
    stage = root / "payload"
    exchanged = False
    candidate = None
    try:
        destination = _check_destination(request)
        release, _ = _verified_asset(root, request)
        signing = _signing_identity(destination) if sys.platform == "darwin" else None
        _wait_for_exit(approval_fd, request["parent_pid"])
        destination = _check_destination(request)
        # Reverify after the bounded wait, not just when the helper was spawned.
        release, _ = _verified_asset(root, request)
        stage.mkdir(mode=0o700)
        candidate = _extract_package(root / "package", stage, sys.platform)
        identity = _marker(candidate, sys.platform)
        if identity != {**request["identity"], "version": release.version, "source_commit": release.source_commit}:
            raise InstallError("Extracted build does not match the signed release.")
        _binary_architecture(_executable(candidate, sys.platform), sys.platform)
        if sys.platform == "darwin":
            if _signing_identity(candidate) != signing:
                raise InstallError("Application signing identity changed.")
            # Retain downloaded provenance first; never overwrite an existing
            # quarantine attribute and never strip any quarantine metadata.
            _preserve_quarantine(root / "package", candidate)
            _preserve_quarantine(destination, candidate)
        _health_version(candidate, release.version, sys.platform)
        _check_destination(request)
        if sys.platform == "darwin" and _signing_identity(destination) != signing:
            raise InstallError("The installed application's signing identity changed.")
        # After exchange, candidate names the previous full build. Keep it until
        # the replacement acknowledges GUI startup, and retain it for recovery.
        _atomic_exchange(candidate, destination)
        exchanged = True
        _receipt(root, request, "promoted", rollback=candidate)
        _health_version(destination, release.version, sys.platform)
        process = _launch(destination, sys.platform, root, request)
        _wait_startup(root, request, process)
        _receipt(root, request, "healthy", rollback=candidate)
        return 0
    except BaseException as error:
        message = str(error) if isinstance(error, InstallError) else "Update validation or native launch failed; no user data was changed."
        if exchanged:
            try:
                _atomic_exchange(candidate, Path(request["destination"]))
            except BaseException:
                _receipt(root, request, "rollback_failed", rollback=candidate)
                # Keep both full builds and the transaction receipt for recovery.
                raise InstallError("Automatic rollback failed; the previous build is retained beside the application.")
            _receipt(root, request, "rolled_back", message=message)
            _launch(Path(request["destination"]), sys.platform)
        else:
            _receipt(root, request, "not_installed", message=message)
        return 1
    finally:
        os.close(approval_fd)
        if not exchanged and stage.exists():
            shutil.rmtree(stage)
        for name in ("package", "manifest.json", "manifest.sig"):
            (root / name).unlink(missing_ok=True)


def helper_main(argv: list[str] | None = None) -> int:
    """Parse helper-only arguments and translate expected installer failures to exit 1."""
    parser = argparse.ArgumentParser(description="BroTypist offline update transaction")
    parser.add_argument("--request", required=True, type=Path)
    parser.add_argument("--approval-fd", required=True, type=int)
    arguments = parser.parse_args(argv)
    try:
        return run_installer(arguments.request, arguments.approval_fd)
    except (OSError, ValueError, InstallError, subprocess.SubprocessError, tarfile.TarError, zipfile.BadZipFile):
        return 1
