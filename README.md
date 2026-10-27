> Historical record: Named third-party research references were removed during repository-history cleanup. This edited historical record is not an original authenticated receipt. Recorded build identifiers and measurements describe their original observations.

# BroTypist

Local-first predictive typing. **Current development and release verification focus on macOS; Linux and Windows ports/builds are paused until the macOS experience is accepted.** BroTypist lives in the menu bar and offers grey same-line suggestions in supported inputs. Settings are not a writing editor.

[Releases](https://github.com/renan-brasilio/brotypist/releases) · [Changelog](CHANGELOG.md) · [Design specification](DESIGN.md) · [Verification](evidence/VERIFICATION.md)


## Install

This repository is **private**. Sign in to a GitHub account with access before downloading release assets or cloning it.

**[macOS 0.4.1](https://github.com/renan-brasilio/brotypist/releases/tag/v0.4.1)** is the real-messenger/crash-fix update, with DMG/ZIP, checksums and native receipts. Linux/Windows product builds are held; historical Inklow 0.3.0 assets are preserved and do not contain the macOS changes.

| Platform | Download | Status |
| --- | --- | --- |
| macOS, Apple Silicon | `BroTypist-0.4.1-macos-arm64.dmg` / `.zip` | Native/frozen and installer checks exercised; see compatibility limits below |
| Windows, x86-64 | No published native installer | Port/build held; packaging definitions retained |
| Linux, x86-64 | Historical `Inklow-0.3.0-linux-amd64.deb` / `.tar.gz` only | Port/build held; not macOS-parity builds |

Packaged applications include Python and their UI runtime. Model weights are separate and download only when requested. Installers never include settings, writing history, credentials, or model weights.

### macOS

Build on Apple Silicon with macOS 15 or newer. Drag the built **BroTypist.app** to Applications; the installed app name has no version suffix. Quit the running copy before replacing it. Open **Welcome** or **Settings → Setup** to review live permission state.

The Mac distribution is **Apple Development identity-signed, not Developer ID-signed or notarized**. Historical/CI-built packages may be ad-hoc signed; read the specific release receipt. Do not disable Gatekeeper globally.

### Windows — held

The MSI installs per user and adds a **BroTypist** Start Menu shortcut. Extract the complete portable ZIP before running `BroTypist.exe`; use `BroTypist-cli.exe` for terminal commands. The EXEs are **not standalone**: keep `_internal` and the rest of the payload together.

Windows 10/11 and an AVX2, FMA, and F16C-capable x86-64 CPU are required for the portable inference build. Releases are unsigned unless explicitly stated otherwise. Do not bypass Windows security protections globally.

### Linux — held

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

Welcome covers privacy, system access, explicit model selection, a real local prediction preview and the resident menu. Reopen it from **About → Show Welcome**. macOS settings contain:

- **Setup:** live required/optional permission and model-readiness states.
- **General:** startup, predictions, accessory icon, model management, completion length, inside-text behavior and typo controls.
- **Context:** separate opt-ins for focused-window Accessibility text, clipboard, screenshot OCR and screenshot-based appearance.
- **Personalization:** encrypted history, recording controls, derived word preferences, custom instructions and scoped deletion/counts.
- **Emoji / Shortcuts:** emoji preferences, configurable physical keys, word/full acceptance, punctuation and Escape behavior.
- **Battery:** on-demand behavior, installed smaller-model selection and shorter completions.
- **App Settings:** searchable app/domain rules, compatibility controls, Tab/recording overrides and instructions.
- **Labs:** explicit alternatives and selected-text synonyms. These numbered tools are separate from ordinary inline suggestions.
- **Statistics / About:** actual accepted-writing aggregates, source/release links, Welcome and Quit.

New installations select **Gemma 4 E2B**, a pinned 2.84 GB public GGUF, with a conservative RAM recommendation. **Gemma 3 1B** is an independently sourced 806 MB option requiring explicit acceptance of [Gemma Terms](https://ai.google.dev/gemma/terms). The catalogue also contains larger Gemma 3/4 and Qwen 3 variants with memory guards; catalogue metadata is not a claim that every size has been exercised on this Mac. Qwen2.5 1.5B and the English Tiny preset remain available. Existing saved model choices are not silently replaced. Suggestions are not a grammar or factuality guarantee.

| Action | Existing default |
| --- | --- |
| Accept next word | Tab |
| Accept full suggestion | Physical key above Tab |
| Request a suggestion | Control + physical key above Tab |
| Dismiss and briefly pause | Escape |
| Open field controls | Click the small caret icon above/beside the field |
| Stop the app and worker | **Quit BroTypist** in the menu-bar/tray menu |

Closing Settings leaves BroTypist running. Normal keys pass through when no eligible suggestion is active. Shortcuts and existing model choices are **not reset by the redesign**. The design document's proposed new defaults do not override saved preferences.

In development source, a cached local GGUF model prepares when an authorized multiline writing field is focused, before the typing threshold/debounce needs a prediction. This includes an empty, writable native field with verified window ownership and valid field bounds: it has no glyph run to measure yet, but preparation sends configuration only, not a draft. Drawing and acceptance still require independently verified same-line layout. Protected/unsupported fields and battery force-only mode do not trigger it. The published **0.4.1** binary still starts loading only when an eligible prediction request needs inference; source changes are not an installed update.

Models use a separate process and unload after five minutes idle by default, on model replacement, privacy revocation, pause or Quit. Focus preparation does not eliminate loading cost or override your saved idle timeout. Temporary field-scoped Escape suppression keeps the worker warm. The typing-delay default is 150 ms; saved values are preserved. UI-thread readiness notifications deliver prepared requests and streamed results without waiting for the fallback polling timer. Welcome's private preview does not record text and closes its worker on focus loss/close. Separately managed loopback servers have their own lifetimes.

### OS access and suggestion placement

- **macOS:** grant **Accessibility / Device Control and Data Access** and **Input Monitoring** to the installed app in System Settings → Privacy & Security. Source Python/terminal permission is not proof of installed-app permission. Focused-window context uses Accessibility text and does not require Screen Recording. An ad-hoc signature replacement may require manual approval again.
- **Windows:** target controls must expose supported UI Automation/native Edit behavior. Elevated apps can require matching integrity privileges; BroTypist does not bypass isolation.
- **Linux:** run in your user's D-Bus/accessibility session. X11 uses AT-SPI; KDE Wayland additionally needs explicit input-method selection. Some Qt targets need `QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1` before starting.

Ordinary completions are transparent, mouse-through grey text at the host caret: no card, border, shadow or keycap. Secure, excluded, hidden, selected, composing, clipped or unobservable inputs stay silent and do not intercept acceptance keys. AX typography is checked against observed glyph advances when exposed. At a document end, providers such as Qt/Telegram that omit zero-length caret bounds can use the final observed LTR glyph edge. When they omit font attributes, BroTypist fits a system font to the observed row; that is not an exact host-font match, does not repaint existing words, and needs no screenshot consent. Soft-wrapped paragraphs are measured on the caret's visual row. Opt-in screenshot appearance can recover additional styling evidence; inside-text displacement still requires measured foreground/background colors and a fully visible same-line suffix. Unsupported RTL/custom-layout/provider cases remain silent, not a mirror/popup fallback. Stock VS Code chat fields have a narrowly scoped accessible-name exception; ordinary code fields remain off unless explicitly enabled. Other editor composers and terminal/browser compatibility require their own native evidence, not a generic “AI” label. A working Welcome preview is not a universal compatibility claim.

**Bounded compatibility:** Google Docs needs both screen-reader and braille support in **Tools → Accessibility**. The metadata-only setup offer is not proof that document typing works. Aside Browser's native preference actions failed persisted-state verification; BroTypist does not change Docs preferences through that provider and shows manual instructions instead. Terminal completion is an additional explicit opt-in with separate per-host Automation consent. Its current input adapter recognizes only the source-verified standalone Codex 0.162.1 single-line composer in Terminal.app; iTerm, other agents, multiplexers, shells, wrapped drafts and hidden paste/image elements remain unavailable. Terminal consent never authorizes scrollback as window context.

When focused-window context is enabled, a prediction refreshes its authorized reference text rather than treating unchanged window geometry as unchanged content. The background request waits up to 250 ms for that read. If it is too slow, the previous reference is not reused; optional context may be absent for that prediction. Local model output still needs review for grammar and factual accuracy.

## Privacy and data preservation

- No built-in cloud inference, account requirement, automatic telemetry or cloud sync. Alternative inference endpoints are restricted to literal loopback addresses; their operators control their logging/network behavior.
- Clipboard, focused-window Accessibility context and both screenshot options are off by default. AX context reads bounded visible reference text from the verified focused window/document, excluding protected/hidden content and unrelated editable fields. Screenshot OCR is AX-first; appearance capture takes the focused window and crops around the line in memory. Both require explicit opt-in and macOS Screen Recording permission. Images are never saved/uploaded by the product; OCR text is short-lived and cleared on revocation. Reads recheck consent, exact window/field/domain, security and policy before capture and publication. OS recording indicators can appear.
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

`brotypist model list` reports the pinned catalogue and licenses. `brotypist model download --preset gemma4-e2b --no-select` downloads without changing selection. Gemma 3 additionally requires `--accept-model-license` or explicit agreement in the native UI. Use `INKLOW_HOME=<disposable-directory>` for the entire smoke sequence instead of changing your normal profile. It isolates settings/data, not the shared model cache.

### Synthetic model checks

Use an existing model; the evaluator does not download weights or change your normal app profile:

```sh
uv run python tools/benchmark_writing.py \
  --model /absolute/path/model.gguf \
  --output /tmp/brotypist-writing.json \
  --repetitions 2 --max-tokens 32 --completion-length medium
```

The authored corpus covers multilingual writing, reference facts, Unicode partial words, fixed suffixes, alternatives and synonyms. Reports include actual native call settings, raw/assembled output, cancellation and close observations. Mechanical pass counts and literal fact checks are **not grammar/semantic accuracy**: inspect the per-dimension unreviewed labels and the actual continuations. Match repeated samples by case ID, not list position—the evaluator rotates case order. Provider token-stream times exclude desktop focus/capture/debounce/rendering; installed-app permission, same-line pixels and real acceptance need their own unlocked native smoke.


### Build applications, icons and installers

```sh
uv run python tools/build_app.py
uv run python tools/smoke_bundle.py
# Run the matching native packager from a committed build:
uv run python tools/package_macos.py --bundle dist/BroTypist.app --output release --version 0.4.2
```

The inference smoke needs an explicitly configured existing model. Installers consume complete native builds; they do not download models or cross-compile. Windows packaging needs .NET SDK 8 and WiX **5.0.2** (`dotnet tool install --global wix --version 5.0.2`). Linux needs `dpkg-dev`, GI/AT-SPI/GTK and Qt libraries; the Wayland client additionally needs a C compiler, `pkg-config`, Wayland/xkbcommon development libraries, protocol XMLs and `wayland-scanner`. Use CPython 3.12 on Ubuntu 24.04 for its distro GI ABI. The [CI workflow](.github/workflows/ci.yml) lists exact build prerequisites.

Canonical artwork is in `design/icons/`. The normal, dark, and small SVGs come directly from the supplied handoff; keep their geometry intact. Rebuild with `python tools/generate_icons.py`: macOS rasterizes the vectors with CoreGraphics and packages ICNS with `iconutil`; Windows/Linux use their existing Qt toolkit. Neither path adds enclosure lighting or a bevel. Runtime apps load committed raster assets and do not require an icon authoring tool.

For stable local Mac permission identity, sign with the **same existing Keychain identity**:

```sh
INKLOW_CODESIGN_IDENTITY='<existing identity name or fingerprint>' uv run python tools/build_app.py
```

Never export or commit signing keys. Reusing an identity is not a notarization claim.

### Release workflow

The [macOS checks workflow](.github/workflows/ci.yml) runs deterministic regressions, native settings, source/frozen inference and DMG mount/launch checks on Apple Silicon. Linux/Windows product jobs and automatic cross-platform release publication are paused.

Update both `pyproject.toml` and `src/inklow/__init__.py` and commit before a release build. **Actions → macOS checks** accepts a matching `release_tag=v<version>` to validate/build macOS artifacts; it does not publish a release automatically. Never overwrite published versions or historical assets.

Manual publication still uses native packagers, smoke receipts and `tools/release_assets.py`: `manifest` seals artifacts; `publish --inputs <directory> --output <new-directory> --tag v<version> --commit <source-sha> --repo renan-brasilio/brotypist` verifies them before draft-first publication. A macOS-only release must explicitly select its target and disclose the held platforms. Do not publish unverified Windows/Linux installers or label source-only changes as installed.

Publishing through `gh` requires a GitHub API credential with repository **Contents: write** access; SSH push authorization does not grant Release API access. The 0.4.0 release used the existing authenticated owner browser after the CLI token refused writes. No token, SSH credential or account permission was changed.

### Source layout

- `src/inklow/runtime.py`, `core.py`: controller, exact snapshots, acceptance guards.
- `process_client.py`, `process_worker.py`, `providers.py`, `writing.py`, `models.py`: isolated local inference, prompts, verified downloads.
- `appkit_ui.py`, `appkit_welcome.py`, `qt_ui.py`, `appkit_overlay.py`: resident native/portable interfaces.
- `preview.py`: owned-field ephemeral Welcome inference; no global capture or persistent history.
- `native/`, `system.py`: OS integration, supported accessibility/text-input paths and explicit OS actions.
- `config.py`, `storage.py`: preferences, encryption and statistics.
- `tools/`, `packaging/`, `.github/workflows/`: verification, native packaging and releases.

The app uses native AppKit on macOS and an isolated llama.cpp worker. No fake MLX/Sparkle controls, invented throughput/RAM counters, [redacted reference] account impersonation or automatic telemetry. iCloud synchronization is explicitly deferred. Framework/compiler tools used for REA are analysis dependencies, not application runtime requirements.

## Research and attribution

[Implementation verification](evidence/VERIFICATION.md) · [Reference-app research](evidence/[redacted reference].md)

BroTypist contains no [redacted reference] proprietary code, models or artwork. Unicode emoji data, third-party runtimes and model weights retain their own licenses. No model weights are committed or embedded in installers.
