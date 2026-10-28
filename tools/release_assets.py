"""Version checks, native asset provenance, and draft-first GitHub publication."""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import runpy
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
TARGET_FILES = {
    'macos-arm64': ('macos-arm64.dmg', 'macos-arm64.zip'),
    'windows-x86_64': ('windows-x86_64.msi', 'windows-x86_64.zip'),
    'linux-x86_64': ('linux-amd64.deb', 'linux-amd64.tar.gz'),
}


def version() -> str:
    """Read the source version and reject versions unsuitable for installers."""
    value = runpy.run_path(str(ROOT / 'src/inklow/__init__.py'))['__version__']
    if not re.fullmatch(r'\d+\.\d+\.\d+', value):
        raise ValueError('Installer releases require a three-part application version.')
    return value


def sha256(path: Path) -> str:
    """Return the file digest used to bind manifests to exact artifact bytes."""
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def commit(value: str) -> str:
    """Require a full lowercase SHA-1 commit spelling without resolving it in Git."""
    if not re.fullmatch(r'[0-9a-f]{40}', value):
        raise ValueError('A full source commit SHA is required.')
    return value


def manifest(args) -> None:
    """Hash the target's assets and embed a caller-supplied passed smoke receipt.

    This records receipt data rather than executing or independently attesting
    the native smoke test. Refuse an existing build-manifest destination.
    """
    app_version = version()
    receipt = json.loads(args.receipt.read_text(encoding='utf-8'))
    if not isinstance(receipt, dict):
        raise ValueError('The native installer smoke receipt must be an object.')
    if receipt.get('status', receipt.get('result')) != 'passed':
        raise ValueError('Only passed native installer receipts can seal release assets.')
    rows = []
    for ending in TARGET_FILES[args.target]:
        path = args.assets / f'BroTypist-{app_version}-{ending}'
        if path.is_symlink() or not path.is_file() or path.stat().st_size == 0:
            raise ValueError(f'Missing, empty, or unsafe native asset: {path}')
        rows.append({'name': path.name, 'size': path.stat().st_size, 'sha256': sha256(path)})
    value = {'version': app_version, 'source_commit': commit(args.commit), 'target': args.target,
             'artifacts': rows, 'installer_smoke': receipt}
    output = args.assets / 'build-manifest.json'
    if output.exists():
        raise FileExistsError(output)
    output.write_text(json.dumps(value, indent=2) + '\n', encoding='utf-8')
    print(json.dumps({'target': args.target, 'artifacts': [row['name'] for row in rows]}))


def gh(*arguments: str, allow_missing=False) -> subprocess.CompletedProcess:
    """Run GitHub CLI, optionally returning HTTP 404 failures for missing objects."""
    result = subprocess.run(['gh', *arguments], text=True, capture_output=True)
    if result.returncode:
        if allow_missing and 'HTTP 404' in result.stderr:
            return result
        raise RuntimeError(result.stderr.strip() or 'GitHub command failed.')
    return result


def signed_update_files(native: list[dict], output: Path, source: str) -> list[Path]:
    """Sign prevalidated package identities with the existing macOS Keychain key.

    The secret is read into process memory for signing, never written to output.
    Require the pinned public identity and verify the signature with the client
    verifier before writing the manifest/signature pair. The caller owns asset
    integrity checks; this helper does not rehash the native packages.
    """
    sys.path.insert(0, str(ROOT / "src"))
    from inklow.update_keys import APP_ID, PUBLISHER_PUBLIC_KEY
    import base64
    import keyring
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

    assets = []
    for target in native:
        for artifact in target["artifacts"]:
            name = artifact["name"]
            suffix = next((ending for ending in ("tar.gz", "zip", "deb", "dmg")
                           if name.endswith("." + ending)), None)
            if suffix is None:
                raise ValueError("Held or unsupported OTA package format.")
            assets.append({**artifact, "target": target["target"], "format": suffix})
    data = (json.dumps({"schema": 1, "application": APP_ID, "version": version(),
                        "source_commit": source, "assets": assets},
                       sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")
    try:
        backend = keyring.get_keyring()
        if type(backend).__module__ != "keyring.backends.macOS" or backend.priority <= 0:
            raise ValueError("Publisher signing requires the existing macOS Keychain key.")
        encoded = backend.get_password("BroTypist Release Signing", "ed25519-v1")
        if not encoded:
            raise ValueError("The publisher signing key is unavailable.")
        key = Ed25519PrivateKey.from_private_bytes(base64.b64decode(encoded, validate=True))
        public = base64.b64encode(key.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)).decode("ascii")
        if public != PUBLISHER_PUBLIC_KEY:
            raise ValueError("The publisher key does not match the application's pinned identity.")
        signature = base64.b64encode(key.sign(data)) + b"\n"
    except Exception:
        raise RuntimeError("The existing publisher Keychain key is required; no unsigned metadata was produced.") from None
    from inklow.updates import verify_manifest
    verify_manifest(data, signature)
    manifest_path = output / "update-manifest.json"
    signature_path = output / "update-manifest.sig"
    manifest_path.write_bytes(data)
    signature_path.write_bytes(signature)
    return [manifest_path, signature_path]


def prepare_release(args) -> list[Path]:
    """Validate transferred assets and write signed metadata without publishing.

    Require exactly the requested native manifests at one version and commit.
    Return artifact and metadata paths; a failure after creating the output
    directory can leave partial local metadata, but makes no GitHub mutation.
    """
    app_version = version()
    if args.tag != 'v' + app_version:
        raise ValueError('Release tag does not match the application version.')
    source = commit(args.commit)
    requested = set(args.targets)
    if len(requested) != len(args.targets):
        raise ValueError('Release targets must be unique.')
    if args.repo != "renan-brasilio/brotypist":
        raise ValueError("Use the canonical BroTypist repository for the pinned OTA identity.")
    manifests = sorted(args.inputs.glob('*/build-manifest.json'))
    if len(manifests) != len(requested):
        raise ValueError('A successful native manifest for every explicitly requested target is required.')
    targets = set()
    native = []
    paths = []
    for path in manifests:
        value = json.loads(path.read_text(encoding='utf-8'))
        target = value.get('target')
        if target not in requested or target in targets:
            raise ValueError('Duplicate or unexpected release target.')
        targets.add(target)
        if value.get('source_commit') != source or value.get('version') != app_version:
            raise ValueError('Native packages were built from different versions or commits.')
        smoke = value.get("installer_smoke", {})
        if smoke.get("status", smoke.get("result")) != "passed":
            raise ValueError("Only passed native installer receipts may be signed for OTA.")
        expected = {f'BroTypist-{app_version}-{ending}' for ending in TARGET_FILES[target]}
        if {row['name'] for row in value['artifacts']} != expected:
            raise ValueError('Native manifest has an unexpected asset set.')
        for row in value['artifacts']:
            artifact = path.parent / row['name']
            if artifact.is_symlink() or not artifact.is_file():
                raise ValueError('Native asset is absent or a symlink.')
            if artifact.stat().st_size != row['size'] or sha256(artifact) != row['sha256']:
                raise ValueError('Native asset failed post-transfer integrity verification.')
            paths.append(artifact)
        native.append(value)
    args.output.mkdir(parents=True, exist_ok=False)
    release_manifest = args.output / 'release-manifest.json'
    release_manifest.write_text(json.dumps({
        'tag': args.tag, 'source_commit': source, 'repository': args.repo,
        'created_at': datetime.now(timezone.utc).isoformat(),
        'signing': 'Recorded in each native installer receipt; no distribution-signing or notarization claim.',
        'targets': native,
        'unpublished_targets': sorted(set(TARGET_FILES) - requested),
    }, indent=2) + '\n', encoding='utf-8')
    paths.append(release_manifest)
    paths.extend(signed_update_files(native, args.output, source))
    sums = args.output / 'SHA256SUMS'
    sums.write_text(''.join(f'{sha256(path)}  {path.name}\n' for path in sorted(paths, key=lambda p: p.name)), encoding='utf-8')
    paths.append(sums)
    return paths


def publish(args) -> None:
    """Create a verified tag if absent, upload a draft, then publish it as latest.

    Existing tags must resolve to the requested commit. GitHub failures
    propagate; completed remote steps are not rolled back automatically.
    """
    app_version = version()
    source = commit(args.commit)
    requested = set(args.targets)
    repo = json.loads(gh('api', f'repos/{args.repo}').stdout)
    if repo.get("private") or repo.get("full_name") != "renan-brasilio/brotypist":
        raise ValueError("OTA publication requires the authorized public BroTypist repository.")
    paths = prepare_release(args)

    # Resolve an existing lightweight or annotated tag without rewriting it.
    ref = gh('api', f'repos/{args.repo}/git/ref/tags/{args.tag}', allow_missing=True)
    if ref.returncode:
        gh('api', '--method', 'POST', f'repos/{args.repo}/git/refs',
           '-f', f'ref=refs/tags/{args.tag}', '-f', f'sha={source}')
    else:
        obj = json.loads(ref.stdout)['object']
        for _ in range(8):
            if obj['type'] != 'tag':
                break
            obj = json.loads(gh('api', f'repos/{args.repo}/git/tags/{obj["sha"]}').stdout)['object']
        if obj['type'] != 'commit' or obj['sha'] != source:
            raise ValueError('The existing release tag points to a different source commit.')

    platform_rows = {
        'macos-arm64': '| macOS 15+, Apple Silicon | `.dmg` | `.zip` |\n',
        'windows-x86_64': '| Windows 10/11, x86-64 | `.msi` (app + CLI EXEs) | `.zip` |\n',
        'linux-x86_64': '| Ubuntu 24.04+/Debian 13+, x86-64/X11 | `.deb` | `.tar.gz` |\n',
    }
    unpublished = set(TARGET_FILES) - requested
    unavailable_note = ('**Not published in this release:** ' + ', '.join(sorted(unpublished))
                        + '; these platform ports/builds are currently held.\n\n') if unpublished else ''
    notes = (
        f'Native installers for BroTypist {app_version}.\n\n'
        '| Platform | Installer | Portable |\n| --- | --- | --- |\n'
        + ''.join(platform_rows[target] for target in TARGET_FILES if target in requested) + '\n'
        + unavailable_note
        + 'Download an installer under **Assets**. '
        'Models are separate explicit downloads; no model weights, settings, or writing history are bundled. '
        'Upgrades and removal preserve external per-user data.\n\n'
        '**Signing:** consult the native receipt for the actual package signature. '
        'An identity-signed local build is not a Developer ID or notarization claim. '
        'Do not disable OS security protections globally. Verify downloads against `SHA256SUMS`.\n\n'
        '**Verification:** consult `release-manifest.json` for the exact native installer, launch, '
        'shutdown and data-preservation receipts. Additional regression, inference and update/rollback '
        'evidence is linked from the repository; none certifies typing in every third-party application.\n\n'
        f'Source: `{source}`. '
        f'See the [versioned changelog](https://github.com/{args.repo}/blob/{source}/CHANGELOG.md) '
        'and README for model, compatibility and platform limitations.\n'
    )
    gh('release', 'create', args.tag, '--repo', args.repo, '--target', source,
       '--verify-tag', '--draft', '--title', f'BroTypist {app_version}', '--notes', notes,
       *[str(path) for path in paths])
    # Never replace an existing published release or claim success after an upload failure.
    gh('release', 'edit', args.tag, '--repo', args.repo, '--draft=false', '--latest')
    result = json.loads(gh('release', 'view', args.tag, '--repo', args.repo,
                           '--json', 'url,isDraft,assets,tagName').stdout)
    if result['isDraft'] or {asset['name'] for asset in result['assets']} != {p.name for p in paths}:
        raise RuntimeError('The published release asset set does not match the verified files.')
    print(json.dumps(result, indent=2))


def main() -> None:
    """Dispatch version, native manifest, local preparation or remote publication."""
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    check = commands.add_parser('version')
    check.add_argument('--tag', default='')
    build = commands.add_parser('manifest')
    build.add_argument('--assets', type=Path, required=True)
    build.add_argument('--target', choices=TARGET_FILES, required=True)
    build.add_argument('--commit', required=True)
    build.add_argument('--receipt', type=Path, required=True)
    for name in ("prepare", "publish"):
        release = commands.add_parser(name)
        release.add_argument('--inputs', type=Path, required=True)
        release.add_argument('--output', type=Path, required=True)
        release.add_argument('--tag', required=True)
        release.add_argument('--commit', required=True)
        release.add_argument('--repo', required=True)
        release.add_argument('--targets', nargs='+', choices=TARGET_FILES,
                             default=['macos-arm64', 'linux-x86_64'],
                             help='Explicit verified targets; Windows publication remains held.')
    args = parser.parse_args()
    if args.command == 'version':
        current = version()
        if args.tag and args.tag != 'v' + current:
            parser.error('Requested release tag must equal v' + current)
        if os.environ.get('GITHUB_OUTPUT'):
            with Path(os.environ['GITHUB_OUTPUT']).open('a', encoding='utf-8') as output:
                output.write(f'version={current}\ntag={args.tag}\n')
        print(current)
    elif args.command == 'manifest':
        manifest(args)
    elif args.command == 'prepare':
        print(json.dumps({'assets': [str(path) for path in prepare_release(args)]}, indent=2))
    else:
        publish(args)


if __name__ == '__main__':
    main()
