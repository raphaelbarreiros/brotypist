> Historical record: Named third-party research references were removed during repository-history cleanup. This edited historical record is not an original authenticated receipt. Recorded build identifiers and measurements describe their original observations.

# BroTypist

**Predictive typing that stays on your computer.**

BroTypist suggests the next words directly beside your cursor, in the apps you already use. Accept what fits, dismiss what doesn't, and keep writing—without moving your draft into a chatbot.

[Download](https://github.com/renan-brasilio/brotypist/releases) · [Changelog](CHANGELOG.md) · [Report an issue](https://github.com/renan-brasilio/brotypist/issues)

This branch is the Rust/native migration, with Rust-only first-party runtime and build entrypoints. The immutable Python baseline is retained externally and in Git history, not as a shipped fallback. Published release assets and the normal installation are unchanged. See the [implementation plan and current evidence](docs/rust-migration/README.md) and [feature-preservation register](docs/rust-migration/parity.md); source cutover and historical receipts do not establish completed Rust parity.

## Why BroTypist?

Writing assistance should fit your workflow, not become another editor. BroTypist lives in the menu bar or system tray, runs local models, and lets you decide where it helps and what context it can see.

- **Inline, not a popup.** Grey suggestions on the same line, with word-by-word or full acceptance and configurable shortcuts.
- **Your languages.** Writing prompts support Brazilian Portuguese, English and mixed-language drafts. Suggestions still need your judgment.
- **Your model.** Choose or import local GGUF models, including Gemma and Qwen. Onboarding includes a real prediction preview.
- **Your writing style.** Optional encrypted history helps reuse your vocabulary and style locally; it does not retrain model weights.
- **Context you control.** Separate opt-ins for window text, clipboard and screenshot context, plus per-app rules and writing instructions.
- **A small resident app.** Native settings, quick pause, battery-aware inference, model unloading and publisher-signed in-app updates.

This describes the current source. Check each release's notes for its packaged features.

## Platform support

| Platform | Status |
| --- | --- |
| macOS 15+, Apple Silicon | Primary target; native AppKit interface. |
| Linux x86-64 | Experimental test builds. Packaging, local inference and UI are tested; native typing compatibility remains limited. |
| Windows | Port and new releases are on hold. |

**Not every text field is supported.** BroTypist needs reliable focus, text, caret and insertion information. Password fields, active text composition and unsafe or unobservable targets are excluded. It stays silent rather than substituting a popup. Linux Wayland inline support is not yet ready for general use; see the [compatibility evidence](evidence/VERIFICATION.md).

On macOS, Electron/Chromium controls with unreliable replacement setters support ordinary completion only when safe inline geometry is available. Emoji replacement, synonyms and typo replacement are disabled for those controls; they must not report edits the host did not apply.

## Install

### macOS

1. Download the DMG from [Releases](https://github.com/renan-brasilio/brotypist/releases). Verify it against the included `SHA256SUMS`.
2. Quit any running copy, then drag **BroTypist.app** into **Applications** and open it there.
3. Grant **Accessibility / Device Control and Data Access** and **Input Monitoring** in System Settings → Privacy & Security. Setup shows permission status.
4. Choose a model in Welcome and try the preview. Gemma 4 E2B needs about **2.84 GB** of disk space for weights; smaller options are available. Models download separately.
5. Start typing in a supported field. **Tab** accepts the next word; **Escape** dismisses the suggestion. Configure full acceptance and other shortcuts in Settings.

Current Mac packages are Apple Development signed, **not notarized**. If macOS blocks a verified download, use the app-specific **Open Anyway** action in Privacy & Security. Do not disable Gatekeeper globally. Screen Recording is needed only for optional screenshot features.

### Linux preview

Use the Debian package on Ubuntu 24.04+/Debian 13+:

```sh
sudo apt install ./BroTypist-<version>-linux-amd64.deb
brotypist
```

On Arch/CachyOS, extract the portable archive and run `./launch-brotypist` from its directory. Keep the whole bundle together and run as your desktop user, **not root**. Packaged inference requires AVX2/FMA/F16C CPU support. KDE input-method selection is explicit; installation alone does not enable Wayland typing.

Closing Settings leaves BroTypist running. **Quit BroTypist** in the resident menu closes the app and its worker. **Check for Updates…** verifies publisher-signed metadata; downloading and installing remain your choice.

## Privacy

The default GGUF backend runs locally, without an account, automatic telemetry or cloud sync. Model downloads and update checks use the network. Extra context and writing history are off by default.

Opt-in history uses encryption backed by the OS credential store, with no plaintext fallback. History and acceptance statistics have separate deletion controls. Models and user data live outside the app installation. Optional loopback-server backends are only as private as the server you configure; custom instructions are ordinary settings text, not secret storage.

## Develop

Use **Rust 1.99.1** and a native C/C++ toolchain. The application, worker, updater and build tooling are Rust; inference uses the pinned native llama.cpp submodule, not Python bindings.

```sh
git clone --recurse-submodules https://github.com/renan-brasilio/brotypist.git
cd brotypist
cargo build --locked --workspace --bins
cargo run --locked -p brotypist --bin BroTypist-cli -- doctor
cargo run --locked -p brotypist --bin BroTypist -- --settings
cargo test --locked --workspace
```

Build on the target OS and architecture. macOS packaging requires full Xcode 27.1. Windows requires native MSVC Developer PowerShell and the Windows SDK, including the selected installation's `VCINSTALLDIR` and `VCToolsRedistDir`; native llama/ggml DLLs share the dynamic CRT and bundles carry authenticated app-local Microsoft redistributables. Linux uses GTK 3, AT-SPI, Pango/Cairo, session D-Bus, X11 and the explicitly selected KDE input-method transport. The [native CI workflow](.github/workflows/ci.yml) records the dependencies and separates development artifacts from releasable installers.

Use an absolute, disposable `INKLOW_HOME` for experiments. It isolates settings/history, not the shared model cache. Existing profile paths, the internal `inklow` identity and OS credential-store names are retained.

```sh
cargo run --locked -p xtask -- version
# Development bundle only; never eligible for OTA or release sealing.
INKLOW_CODESIGN_IDENTITY=- cargo run --locked -p xtask -- build --development
cargo run --locked -p xtask -- verify --development --bundle dist/BroTypist.app
```

The signing variable and `.app` path above are macOS-specific; Linux/Windows bundles use `dist/BroTypist`. Production signing, release assembly and installation over an existing application are separate operations.

- `crates/brotypist/src/runtime.rs`, `core.rs`: prediction, authorization and acceptance lifecycle.
- `providers.rs`, `writing.rs`, `worker.rs`, `process_client.rs`: native inference, prompts and owned worker processes.
- `ui/`, `platform/`: AppKit, Win32 and GTK shells and platform integration.
- `config.rs`, `storage.rs`, `models.rs`, `system.rs`: compatible preferences, encrypted data and system services.
- `crates/brotypist-updater/`, `crates/xtask/`: independent updater and native build/distribution tooling.
- `crates/brotypist/examples/`: real worker and owned-window smoke tools; desktop checks require the explicit `native-smoke` build feature and a fresh private profile.
- `cargo run --locked -p xtask --bin BroTypist-review-predictions -- --review-input report.json --reviews labels.json --output reviewed.json`: attach explicit human/assistant labels offline. This independent binary loads no app settings, models or inference engine; it preserves raw outputs and refuses to overwrite reports. Unix output uses mode `0600`; Windows uses inherited ACLs, not a POSIX-permission guarantee.


## Contribute

Found a problem? Open an issue with the app version, OS, target application and reproducible steps. Use synthetic text; never attach private drafts, profiles, keys or model weights.

Keep pull requests focused. Document public contracts and non-obvious lifecycle decisions, add behavioral regressions where needed, and report what you exercised on the actual platform. Preserve saved settings, explicit consent and safe handling of unsupported inputs. Discuss larger changes first.

[Design direction](DESIGN.md) · [Verification](evidence/VERIFICATION.md) · [Reference-app research](evidence/[redacted reference].md)

BroTypist is an independent implementation, not a redistribution of [redacted reference] code, models or artwork. Third-party components and model weights retain their respective licenses.
