> Historical record: Named third-party research references were removed during repository-history cleanup. This edited historical record is not an original authenticated receipt. Recorded build identifiers and measurements describe their original observations.

# BroTypist

Local-first predictive typing for macOS, Windows, and Linux. BroTypist lives in the menu bar or system tray and offers suggestions in supported text inputs. Settings are not a separate writing editor.

[Releases](https://github.com/renan-brasilio/brotypist/releases) · [Changelog](CHANGELOG.md) · [Design specification](DESIGN.md) · [Verification](evidence/VERIFICATION.md)

![Native BroTypist settings](evidence/brotypist/macos-settings-general.png)

## Install

This repository is **private**. Sign in to a GitHub account with access before downloading release assets or cloning it.

The latest published installers are **0.3.0**, released under the previous **Inklow** name. Those immutable assets keep their original filenames. The BroTypist redesign, strict inline/window-context changes, and **0.3.3 minimal field-menu/Retina artwork corrections are in source**. An older installer does not contain these changes; use a verified current build until the next release is published.

| Platform | Published 0.3.0 assets | Current native build outputs |
| --- | --- | --- |
| macOS, Apple Silicon | `Inklow-0.3.0-macos-arm64.dmg` / `.zip` | `BroTypist.app`, versioned `.dmg` / `.zip` |
| Windows, x86-64 | Not published; native installer verification remains pending | `BroTypist.exe`, `BroTypist-cli.exe`, `.msi` / portable `.zip` |
| Linux, x86-64 | `Inklow-0.3.0-linux-amd64.deb` / `.tar.gz` | `brotypist` launcher, versioned `.deb` / `.tar.gz` |

Packaged applications include Python and their UI runtime. Model weights are separate and download only when requested. Installers never include settings, writing history, credentials, or model weights.

### macOS

Build on Apple Silicon with macOS 15 or newer. Drag the built **BroTypist.app** to Applications; the installed app name has no version suffix. Quit the running copy before replacing it. Open **Welcome** or **Settings → Privacy** to review actual permission state.

GitHub-built macOS distributions are ad-hoc signed and **not notarized**, unless the release explicitly says otherwise. A local Apple Development signature is not Developer ID distribution or notarization. Do not disable Gatekeeper globally.

### Windows

The MSI installs per user and adds a **BroTypist** Start Menu shortcut. Extract the complete portable ZIP before running `BroTypist.exe`; use `BroTypist-cli.exe` for terminal commands. The EXEs are **not standalone**: keep `_internal` and the rest of the payload together.

Windows 10/11 and an AVX2, FMA, and F16C-capable x86-64 CPU are required for the portable inference build. Releases are unsigned unless explicitly stated otherwise. Do not bypass Windows security protections globally.

### Linux

Install the specific Debian package you built or downloaded with `sudo apt install ./<package>.deb`, then run **`brotypist` as your desktop user, not with sudo**. The current package replaces the old `inklow` package while leaving external user data alone. Portable builds run through `launch-brotypist`; keep their directory intact.

The published 0.3.0 package supports **X11 only**, including its restriction on XWayland applications. Current source also has the opt-in KDE Wayland path below. Native Debian releases target Ubuntu 24.04+/Debian 13+ amd64, with D-Bus and AVX2/FMA/F16C. Other distributions, architectures, and CPU baselines require a target-system build.

#### KDE Wayland: current source

BroTypist uses KWin's private **input-method-v1** connection for its opt-in brand/manual-menu controls, not raw input devices, global injection, or a screen-reader identity. **Ordinary inline completion is currently unavailable on this Wayland path.** A real isolated KWin/Qt test showed that delayed preedit could replace a newly selected range and did not expose inactive styling. The unsafe preedit path was removed; missing native font metrics are not replaced with a suggestion popup.

```sh
.venv/bin/python tools/build_wayland.py
.venv/bin/brotypist wayland install
```

The install command only creates the per-user selector entry. Explicitly choose **BroTypist** in **KDE System Settings → Keyboard → Virtual Keyboard**; Welcome and Privacy can open that selector. KDE supplies the private socket and owns the resulting launch. Do not invoke `--wayland-input-method` directly from a terminal.

Selecting an input method lets it receive keyboard events and surrounding text for supported fields and occupies KDE's input-method slot. Consider an existing Fcitx/IBus setup before replacing it. BroTypist never selects itself or changes the desktop session automatically. After quitting or updating a selected source build, select **None**, then **BroTypist**, to start it again. A terminal `brotypist doctor` cannot inspect the KDE-owned private connection; use the running app's Privacy status.

The exercised Qt target uses **text-input-v3**. A target can request it at its own launch with `QT_WAYLAND_TEXT_INPUT_PROTOCOL=zwp_text_input_v3 QT_IM_MODULE=wayland <application>`. The tested default text-input-v2 path lacked complete context. Do not set those variables globally just to make one application work.

KWin owns manual-panel placement. Protected fields, selections, stale contexts, and unmatched metadata are rejected. The current Wayland path neither submits ordinary predictions nor consumes their acceptance keys. Background window context, atomic cross-app replacement, selected-text synonyms, and emoji/emoticon rewriting remain unavailable. Restoring Wayland completion requires a proven host/compositor path with observable inactive styling and atomic stale-state fencing; the historical native-panel receipts are not proof of that capability.

### Verify downloads

Releases include `SHA256SUMS`. On Linux, use `sha256sum --check SHA256SUMS --ignore-missing`; on macOS, use `shasum -a 256 <file>`; on Windows, use `Get-FileHash <file> -Algorithm SHA256`. Compare against the matching manifest entry. Integrity checks are not a signing or notarization certificate.

## First run and daily use

Welcome covers privacy, system access, explicit model selection, a real local prediction preview, and the resident menu. Reopen it from **About → Show Welcome**. Six settings destinations retain the existing supported preferences:

- **General:** startup, predictions, field accessory, and aggregate activity.
- **AI Models:** GGUF files, verified downloads, loopback backends, inference and battery controls.
- **Autocomplete:** completion behavior, configurable shortcuts, emoji, alternatives and synonyms where supported.
- **Languages:** truthful model-language information and access to existing custom instructions; no invented language-quality rankings.
- **Privacy:** actual permission status, optional context/history, exclusions, and app/website rules.
- **About:** version, source, Welcome and Quit.

The writing preset is **Qwen2.5 1.5B**, approximately 1.12 GB. The approximately 101 MB Tiny preset is an English starter/testing model, not a comparable writing-quality replacement. Existing complete models are reused. Suggestions are not a grammar or factuality guarantee.

| Action | Existing default |
| --- | --- |
| Accept next word | Tab |
| Accept full suggestion | Physical key above Tab |
| Request a suggestion | Control + physical key above Tab |
| Dismiss and briefly pause | Escape |
| Open field controls | Click the small caret icon above/beside the field |
| Stop the app and worker | **Quit BroTypist** in the menu-bar/tray menu |

Closing Settings leaves BroTypist running. Normal keys pass through when no eligible suggestion is active. Shortcuts and existing model choices are **not reset by the redesign**. The design document's proposed new defaults do not override saved preferences.

Models load lazily in a separate process and unload after five minutes idle by default, on pause, model change or Quit. A later request can require a cold load. The new typing-delay default is 150 ms; existing saved values are preserved unless you explicitly change them. UI-thread readiness notifications deliver prepared requests and streamed results without waiting for the fallback polling timer. Welcome's preview uses the same provider, does not record its text, and closes its worker when the field loses focus or the preview closes. Separately managed loopback servers have their own model lifetimes.

### OS access and suggestion placement

- **macOS:** grant **Accessibility / Device Control and Data Access** and **Input Monitoring** to the installed app in System Settings → Privacy & Security. Source Python/terminal permission is not proof of installed-app permission. Focused-window context uses Accessibility text and does not require Screen Recording. An ad-hoc signature replacement may require manual approval again.
- **Windows:** target controls must expose supported UI Automation/native Edit behavior. Elevated apps can require matching integrity privileges; BroTypist does not bypass isolation.
- **Linux:** run in your user's D-Bus/accessibility session. X11 uses AT-SPI; KDE Wayland additionally needs explicit input-method selection. Some Qt targets need `QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1` before starting.

Ordinary completions are transparent, mouse-through grey text at the host caret baseline, in a verified host font. They have no card, border, shadow or keycap. Secure, excluded, selected, composing, clipped or unobservable inputs remain silent and do not intercept acceptance keys. **Native-only strict alignment is intentional:** Chromium on macOS currently omits font names, so affected Chrome/Edge/Electron fields are unsupported rather than guessed. Custom line heights, RTL runs and Linux providers without reliable composition/font metadata are also unsupported. Explicit alternative-word tools retain their separate chooser. The menu/settings expose native-inline and context availability; a working owned Welcome preview is not a universal compatibility claim.

## Privacy and data preservation

- No built-in cloud inference, account requirement, automatic telemetry or cloud sync. Alternative inference endpoints are restricted to literal loopback addresses; their operators control their logging/network behavior.
- Clipboard and focused-window Accessibility context are off by default. Opt-in window context is bounded, relevant visible reference text from the verified focused window/document; protected/hidden content and unrelated editable fields are excluded. No screenshots are taken. Pending reads recheck consent, focus and policy, expire, and cannot supply stale context after revocation. Qt clipboard access is queued on the GUI thread with a bounded timeout and authorization checks before and after the read.
- Writing history is opt-in, AES-GCM encrypted and requires the OS credential store, with no plaintext fallback. Aggregate statistics contain acceptance counts, not writing. History and statistics have separate deletion controls.
- Custom instructions are ordinary settings text, not a place to store credentials.
- Models, preferences and history live outside the installation and are not uninstalled with it.

**Stable internal identities are intentional.** The Python package `inklow`, bundle/desktop ID `com.inklow.Inklow`, existing startup/credential keys and `inklow` data/cache directories remain established technical namespaces. Changing the visible app/repository name does not migrate encrypted data or deliberately reset OS permission identity. Public commands and new installer/bundle names are BroTypist / `brotypist`.

`INKLOW_HOME` remains the isolated settings/data profile switch for development. It does not relocate the shared model cache. Isolated profiles cannot change startup or desktop integration. OS swap/crash collection and independently managed servers have privacy behavior outside the app's control.

## Develop

Use Python 3.12 and [uv](https://docs.astral.sh/uv/). Build on the intended OS and architecture; a Mac build cannot produce a Windows runtime.

```sh
git clone git@github.com:renan-brasilio/brotypist.git
cd brotypist
uv sync --python 3.12 --extra dev --extra local --extra build
uv run brotypist doctor
uv run brotypist --settings
uv run brotypist --pane welcome
uv run pytest -q
```

GGUF builds need Xcode Command Line Tools, Visual Studio C++ Build Tools, or GCC/Clang/CMake. Linux additionally needs distribution AT-SPI/GI and desktop libraries; use its Python with system bindings when appropriate:

```sh
/usr/bin/python3 -m venv --system-site-packages .venv
.venv/bin/python -m pip install -e '.[dev,local,build]'
```

`brotypist model list` reports the pinned catalog. `brotypist model download --preset balanced --no-select` explicitly downloads without selecting. Avoid changing your normal profile just to run a smoke test: use `INKLOW_HOME=<disposable-directory>` for the entire test sequence.

### Build applications, icons and installers

```sh
uv run python tools/build_app.py
uv run python tools/smoke_bundle.py
# Run the matching native packager from a committed build:
uv run python tools/package_macos.py --bundle dist/BroTypist.app --output release --version 0.3.1
uv run python tools/package_windows.py --bundle dist/BroTypist --output release --version 0.3.1
uv run python tools/package_linux.py --bundle dist/BroTypist --output release --version 0.3.1
```

The inference smoke needs an explicitly configured existing model. Installers consume complete native builds; they do not download models or cross-compile. Windows packaging needs .NET SDK 8 and WiX **5.0.2** (`dotnet tool install --global wix --version 5.0.2`). Linux needs `dpkg-dev`, GI/AT-SPI/GTK and Qt libraries; the Wayland client additionally needs a C compiler, `pkg-config`, Wayland/xkbcommon development libraries, protocol XMLs and `wayland-scanner`. Use CPython 3.12 on Ubuntu 24.04 for its distro GI ABI. The [CI workflow](.github/workflows/ci.yml) lists exact build prerequisites.

Canonical artwork is in `design/icons/`. The normal, dark, and small SVGs come directly from the supplied handoff; keep their geometry intact. Rebuild with `python tools/generate_icons.py`: macOS rasterizes the vectors with CoreGraphics and packages ICNS with `iconutil`; Windows/Linux use their existing Qt toolkit. Neither path adds enclosure lighting or a bevel. Runtime apps load committed raster assets and do not require an icon authoring tool.

For stable local Mac permission identity, sign with the **same existing Keychain identity**:

```sh
INKLOW_CODESIGN_IDENTITY='<existing identity name or fingerprint>' uv run python tools/build_app.py
```

Never export or commit signing keys. Reusing an identity is not a notarization claim.

### Release workflow

GitHub Actions exercises deterministic regressions, native settings, source/frozen inference and actual installer installation/mount/removal. All required native targets must pass before publication. Windows native runtime/installer verification remains a release gate; source-level checks are not a substitute.

Update both `pyproject.toml` and `src/inklow/__init__.py`, commit, then run **Actions → Cross-platform checks** with matching `release_tag=v<version>`. Empty input checks only. A matching version tag or `release/<version>` branch also requests publication. Never overwrite a published version or its historical assets.

Manual releases use the same native packagers and smoke tools on disposable target desktops. `tools/release_assets.py manifest` seals the two files per target with their native receipt; `publish --inputs <directory> --output <new-directory> --tag v<version> --commit <source-sha> --repo renan-brasilio/brotypist` verifies them before draft-first publication. Reduced target sets require explicit `--targets` and list omissions in the manifest/release notes.

### Source layout

- `src/inklow/runtime.py`, `core.py`: controller, exact snapshots, acceptance guards.
- `process_client.py`, `process_worker.py`, `providers.py`, `writing.py`, `models.py`: isolated local inference, prompts, verified downloads.
- `appkit_ui.py`, `appkit_welcome.py`, `qt_ui.py`, `appkit_overlay.py`: resident native/portable interfaces.
- `preview.py`: owned-field ephemeral Welcome inference; no global capture or persistent history.
- `native/`, `system.py`: OS integration, supported accessibility/text-input paths and explicit OS actions.
- `config.py`, `storage.py`: preferences, encryption and statistics.
- `tools/`, `packaging/`, `.github/workflows/`: verification, native packaging and releases.

The supplied design is adapted to existing capabilities: no fake MLX/Sparkle controls, unmeasured performance counters, unsupported language controls or changed key defaults. Backend/model upgrades are a separate product change.

## Research and attribution

[Implementation verification](evidence/VERIFICATION.md) · [Reference-app research](evidence/[redacted reference].md)

BroTypist contains no [redacted reference] proprietary code, models or artwork. Unicode emoji data, third-party runtimes and model weights retain their own licenses. No model weights are committed or embedded in installers.
