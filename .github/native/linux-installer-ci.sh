#!/usr/bin/env bash
# Only checks.ps1's explicitly opted-in GitHub-hosted root lane may invoke this.
# The Rust probe owns dpkg cleanup, private Xvfb/D-Bus, native Quit and receipts.
set -euo pipefail
umask 077

if [[ $# != 6 || ${EUID} != 0 || ${CI:-} != true || ${GITHUB_ACTIONS:-} != true || ${RUNNER_ENVIRONMENT:-} != github-hosted || ${BROTYPIST_INSTALLER_SMOKE_ALLOW_SYSTEM_PACKAGE:-} != isolated-ci-root ]]; then
    printf '%s\n' 'Refusing installer mutation outside the explicit disposable hosted root lane.' >&2
    exit 1
fi
xtask=$1
probe=$2
model=$3
bundle=$4
assets=$5
output=$6
for path in "$xtask" "$probe" "$model" "$bundle" "$assets" "$output"; do
    [[ "$path" == /* ]] || { printf '%s\n' 'Installer inputs/output must be absolute.' >&2; exit 1; }
done
[[ -f "$xtask" && ! -L "$xtask" && -f "$model" && ! -L "$model" ]]
[[ -d "$probe" && ! -L "$probe" && -d "$bundle" && ! -L "$bundle" && -d "$assets" && ! -L "$assets" ]]
[[ ! -e "$output" && ! -L "$output" ]]
# Keep the evidence directory separate from package payloads. Root-owned private
# staging never reuses the hosted runner's HOME, session bus, display or loaders.
work=$(mktemp -d /var/tmp/brotypist-installer-ci.XXXXXXXX)
mkdir -- "$output"
chmod 0755 -- "$output"
finish() {
    status=$?
    trap - EXIT
    # Do not expose the private session/model/configuration trees. The Rust
    # module writes this bounded receipt on both success and failure.
    if [[ -f "$work/evidence/installer-smoke-linux.json" && ! -L "$work/evidence/installer-smoke-linux.json" ]]; then
        if ! install -m 0644 -- "$work/evidence/installer-smoke-linux.json" "$output/installer-smoke-linux.json"; then status=1; fi
    elif [[ $status == 0 ]]; then
        printf '%s\n' 'Installer probe returned success without an actual receipt.' >&2
        status=1
    fi
    if ! rm -rf -- "$work"; then status=1; fi
    exit "$status"
}
trap finish EXIT
mkdir -- "$work/bin" "$work/probe" "$work/home" "$work/tmp" "$work/evidence"
install -m 0755 -- "$xtask" "$work/bin/xtask"
cp -R --no-preserve=ownership -- "$probe/." "$work/probe/"
cp -R --no-preserve=ownership -- "$bundle" "$work/BroTypist"
cp -R --no-preserve=ownership -- "$assets" "$work/assets"
install -m 0600 -- "$model" "$work/tiny.gguf"
# Relocate only the QA worker's copied dynamic closure, never canonical bundle
# bytes or receipts. The root session clears LD_LIBRARY_PATH by design.
for binary in "$work/probe"/*; do
    [[ -f "$binary" && ! -L "$binary" ]]
    patchelf --set-rpath '$ORIGIN' "$binary"
done
chmod 0755 -- "$work/probe/worker_smoke"
chown -R root:root -- "$work"
chmod -R go-w -- "$work"
# The guarded Rust module creates its own root-owned session/runtime/profile
# directories and invokes actual dpkg --unpack/--configure/--purge transactions.
# No previous-version or rollback fixtures are synthesized by this lane.
env -i \
    PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    HOME="$work/home" TMPDIR="$work/tmp" LC_ALL=C.UTF-8 \
    CI=true GITHUB_ACTIONS=true RUNNER_ENVIRONMENT=github-hosted \
    BROTYPIST_INSTALLER_SMOKE_ALLOW_SYSTEM_PACKAGE=isolated-ci-root \
    BROTYPIST_INSTALLER_SMOKE_WORKER_EXE="$work/probe/worker_smoke" \
    BROTYPIST_INSTALLER_SMOKE_MODEL_PATH="$work/tiny.gguf" \
    "$work/bin/xtask" smoke-installer \
        --bundle "$work/BroTypist" --assets "$work/assets" --output "$work/evidence"
