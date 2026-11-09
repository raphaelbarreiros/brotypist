<h1 align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/brand/brotypist-lockup-dark.svg">
    <source media="(prefers-color-scheme: light)" srcset="docs/brand/brotypist-lockup-light.svg">
    <img src="docs/brand/brotypist-lockup-light.svg" alt="BroTypist" width="500">
  </picture>
</h1>

<p align="center"><strong>Predictive typing that stays on your computer.</strong></p>
<p align="center">
  <a href="https://github.com/renan-brasilio/brotypist/releases">Downloads &amp; release notes</a> ·
  <a href="#install">Install</a> ·
  <a href="#use-brotypist">Usage</a> ·
  <a href="#privacy">Privacy</a> ·
  <a href="#develop">Develop</a>
</p>

BroTypist suggests the next words beside your cursor, in the apps you already
use. Accept what fits, dismiss what doesn't, and keep writing—without moving
your draft into a chatbot. It lives in the menu bar or system tray, with
OS-native Settings and local model inference.

**Native Rust, with macOS first.** **[0.5.4 is the Latest macOS-only stable
release](https://github.com/renan-brasilio/brotypist/releases/tag/v0.5.4);
Windows and Linux remain ALPHA.** It contains Apple Silicon DMG/ZIP assets only,
bound to original signed-build source `cdf10e5b442e6ea1ac33043d504e2a3d1e4c72c2`,
not the rewritten tag's source identity or this unreleased working candidate.
This patch improves prediction responsiveness and TextEdit inline placement,
and documents safe macOS unknown-developer launch troubleshooting.
Model relevance and text-field compatibility still vary: stable does not mean
every app is supported or establish three-platform parity. The
[migration and acceptance record](docs/rust-migration/README.md) separates
source-bound observations from final release proof and remaining platform gates.

## Writing, with you in control

- **Inline completion.** Grey text on the same line, with next-word or full
  acceptance and configurable shortcuts. No popup substitute for unsupported fields.
- **Your languages and model.** Brazilian Portuguese, English and mixed-language
  prompts; choose a local GGUF model or import a compatible one.
- **Emoji without inference.** Explicit `:name` and emoticon suggestions from a
  local catalogue, where the target safely supports replacement.
- **Context by choice.** Separate opt-ins for window text, clipboard and
  screenshot context, plus per-app rules and writing instructions.
- **Optional local personalization.** Encrypted writing history can help reuse
  vocabulary and style; it does not retrain model weights.
- **A resident utility.** Quick pause, battery-aware inference, model unloading
  and native controls rather than another writing window.

<p align="center">
  <img src="evidence/rust-migration/macos-0.5.3-menu.png" alt="BroTypist 0.5.3 native macOS General Settings with aligned sidebar icons and grouped controls" width="760">
</p>
<p align="center"><sub>Historical 0.5.3 native macOS Settings capture, rasterized from its original tagged source with synthetic preferences and no runtime services—not a mockup, personal conversation or current-worktree acceptance. <a href="evidence/rust-migration/macos-0.5.3-release-verification.json">0.5.3 capture and release verification record</a>.</sub></p>

## Platform support

| Platform | Current native status | Installation |
| --- | --- | --- |
| **macOS 15+, Apple Silicon** | **0.5.4 stable channel.** Prediction responsiveness and TextEdit inline placement improvements; polished AppKit Settings retained. Compatibility varies by text field. | Mac-only DMG/ZIP in Releases; Apple Development signed, **not notarized**. |
| **Linux x86-64, X11** | **ALPHA.** Packaging, inference and native UI have evidence; desktop typing compatibility is limited. Emoji/synonym/typo rewrites are unavailable. | Alpha DEB or portable test bundle when supplied; not part of the macOS stable release. |
| **Linux x86-64, Wayland** | **ALPHA. Ordinary inline completion is unavailable.** Manual preview only; experimental KDE input-method controls require explicit selection and are not general inline support. | Alpha test bundles; installation does not enable an input method. |
| **Windows x86-64** | **ALPHA.** Unsigned portable testing; inference and owned-window GUI checks do not establish full physical typing, upgrade or rollback acceptance. **MSI distribution is held.** | Authorized portable test bundle only; not part of the macOS stable release. |

**Not every text field is supported.** BroTypist needs reliable focus, text,
caret geometry and insertion information. Password fields, active text
composition and unsafe or unobservable targets are excluded. It stays silent
rather than placing a completion in the wrong place. See
[compatibility evidence](evidence/VERIFICATION.md) and the
[native acceptance register](docs/rust-migration/parity.md).

On macOS, Electron/Chromium controls with unreliable replacement setters support
ordinary completion only when safe inline geometry is available. Emoji,
synonym and typo replacement are disabled in those append-only controls.

The **unreleased working candidate** adds a separate, guarded emoji-only path
for eligible native WhatsApp fields on macOS. It selects only the shortcode or
emoticon range, sends Unicode to the verified target process, and requires exact
text/caret readback. It does not enable synonyms, typo correction or general
replacement, and does not change the Electron/Chromium limits above. Actual
WhatsApp observations were nonmutating accessibility metadata; controlled native
component proof is not acceptance of editing a real WhatsApp conversation.

For the macOS VS Code compatibility path in 0.5.1, code-editor completions remain
an explicit per-app opt-in. VS Code 1.144's native EditContext backend does not
expose reliable caret/glyph geometry. The verified native geometry path requires
`"editor.accessibilitySupport": "on"` and `"editor.editContext": false` in VS Code.
These change its accessibility/input mode; BroTypist does not silently apply them.
Its textarea relay is clipped to the verified native editor viewport, not its
one-point input element. Direct Cursor verification is still pending.

Copilot and Cursor Tab also draw grey inline text and accept with Tab. Use one
automatic inline provider in a given editor rather than assuming their overlays
and shortcuts coordinate. BroTypist does not disable either assistant.
WordCraft 0.13.2's advertised accessibility setter did not change the owned test
document; BroTypist keeps that unverified editing target excluded.

## Install

Use only assets actually listed for your platform in
[Releases](https://github.com/renan-brasilio/brotypist/releases), or an explicitly
supplied preview bundle. Check the package's release notes and included
`SHA256SUMS`; a CI artifact is not automatically a released installer.

### macOS

1. Quit any running copy. Open the DMG, drag **BroTypist.app** into
   **Applications**, then open it there.
2. Grant **Accessibility / Device Control and Data Access** and **Input
   Monitoring** in System Settings → Privacy & Security. Setup shows their status.
3. Choose a model in Welcome and try the prediction preview. Gemma 4 E2B needs
   about **2.84 GB** of disk space for weights; smaller options are available.
   Models download separately.
4. Enable predictions for the apps you want to use, then type in a supported field.

#### macOS says “Move to Trash” or cannot verify the developer

Current packages are **Apple Development-signed, not Developer ID-notarized**.
An initial warning that Apple “cannot check it for malicious software” can offer
**Move to Trash** and **Done**. Only for a download from the official
[BroTypist releases](https://github.com/renan-brasilio/brotypist/releases) whose
SHA-256 matches that release's `SHA256SUMS`:

Download `SHA256SUMS` from the same release page. In Terminal, run
`shasum -a 256` followed by the path to your downloaded DMG or ZIP (you can drag
the file into Terminal to insert its path). Compare the resulting hash with the
entry for that exact filename. A matching checksum confirms download integrity;
it does not mean Apple notarized the app.

1. Choose **Done**, not Move to Trash.
2. Open **System Settings → Privacy & Security → Security**.
3. Find the blocked BroTypist launch and choose **Open Anyway**, then confirm
   **Open** and authenticate if requested. This approves this app, not all apps.
4. If you already moved it to Trash, download a fresh official release, verify
   its checksum, and copy **BroTypist.app** into **Applications** again.

Do **not** treat “damaged,” modified-app/signature failures, or “will damage your
computer”/known-malware warnings as merely an unknown developer. Do not bypass
those warnings: obtain a fresh official download, check its integrity, and report
the exact warning if it persists. A managed Mac may require your IT administrator.
Do not disable Gatekeeper, remove quarantine attributes, or bypass permission
controls. See Apple's
[safely opening apps guidance](https://support.apple.com/en-us/102445) and
[modified or damaged app guidance](https://support.apple.com/en-ge/guide/mac-help/mh40619/mac).

Launch approval is separate from normal app permissions. This troubleshooting
requires no new permissions: Accessibility and Input Monitoring remain the setup
permissions described above; Screen Recording is only for optional screenshot
features.

Downloaded models are retained outside the app bundle, so an app update does
not require downloading them again. Startup verifies selected primary and battery
catalog models from the old default cache before retaining missing weights in
durable app data. Original weights and custom model paths are preserved.

### Linux ALPHA

For a supplied DEB on Ubuntu 24.04+/Debian 13+:

```sh
sudo apt install ./BroTypist-<version>-linux-amd64.deb
brotypist
```

On Arch/CachyOS, extract the portable archive and run `./launch-brotypist` from
its directory. Keep the entire bundle together and run as your desktop user,
**not root**. Packaged inference requires AVX2/FMA/F16C CPU support. On Wayland,
use the manual preview; do not change session environment variables to pretend
the session is X11.

### Windows ALPHA — portable testing

Extract the supplied ZIP into a user-writable folder, keep all bundled DLLs
together, and launch `BroTypist.exe`. The preview is **unsigned**; do not disable
Windows security controls to run it. MSI installation remains held pending the
installer acceptance gates.

## Use BroTypist

- **Complete:** type normally in a supported field. **Tab** accepts the next
  word of eligible visible grey text; default **Escape** dismisses. In the
  unreleased candidate, verified completion acceptance in **native WhatsApp**
  reserves the accept-word key for that exact writing field. Extra Tabs without
  visible text do nothing, even after a pause or a genuine no-offer. Typing,
  caret/selection or focus changes, Escape, policy/safety revocation and
  unconfirmed edits release the reservation. Other apps retain the maximum
  two-second renewal/repaint guard; ordinary Tab outside its scope is unchanged.
  Neither policy accepts unseen text or retries a no-offer. Exhausted verified
  suggestions renew with normal debounce. Configure bindings in **Settings → Shortcuts**.
- **Find emoji:** in an eligible emoji-replacement field, type `:sweat_smile` or
  `:sorriso` at the start of a line or after a space. A closing `:` is optional.
  With emoticons enabled, `:-)` or `<3` also offers a choice. These are supported
  examples, not a promise that every arbitrary shortcode is recognized.
  Accept explicitly with a numbered choice (**1–9**) or a click; plain words do
  not trigger emoji, and text is never silently rewritten. Linux native rewrites
  are unavailable; the native WhatsApp exception above is unreleased.
- **Limit where it helps:** use **App Settings** for per-app rules and **Context**
  for optional sources. Pause from the resident menu whenever you want.
- **Close or quit:** closing Settings leaves BroTypist running. **Quit
  BroTypist** in the resident menu closes the app and its worker.
- **Updates:** **Check for Updates…** verifies publisher-signed metadata.
  Downloading and installing remain your choice.

### Settings and save results (Unreleased)

**Writing** separates the default writing language (`writing_language`) from
regional spelling/conventions (`spelling_language`). `auto` follows detected
writing; explicit language tags guide the model, not a guarantee of language
quality. Regional spelling also selects an exact available macOS dictionary;
another region is not silently substituted. Native dictionary correction remains
unavailable on Windows/Linux. Authored instructions remain intact, with global,
app and exact-website instructions applied in that order.

App rules use canonical application identities and website rules use normalized
**exact hosts**, not wildcard/subdomain matching. Optional clipboard, focused-window
text, screenshot context and screenshot appearance each have their own global
consent ceiling: a scoped rule cannot turn on a globally denied source.
Appearance sampling is separate from model context; enabling appearance does not
authorize sending screenshot content to the model. A required but unavailable
browser domain fails closed.

Save results belong to the specific operation: **Saved** means persisted;
**Applied** means session-only, not saved. Failure, cancellation and supersession
are not success, even when displayed values happen to match. GGUF model changes
are checked through the existing inference worker, including cold preparation,
prompt-template compatibility and catalogue integrity where applicable, before
persistence. Supported loopback-server settings can be saved without connecting:
**server/model availability is not checked** by that receipt.

General keeps everyday enable/pause and startup controls; Setup owns platform
readiness and permission guidance; About retains version, notices and update
information. These changes do not replace the resident app, its identities or
its independent updater.

Model details use canonical catalog download size, minimum physical memory and
license name/link. A zero or missing catalog minimum means **unspecified**, not
zero memory consumption: macOS omits that line; Linux/Windows show unavailable.
RAM labels and insufficient-memory warnings share a binary GiB formatter:
8,589,934,592 bytes formats as **8.00 GiB**, without truncating fractional
thresholds. Download-size formatting and byte-exact eligibility are unchanged.
A catalog minimum is not measured consumption or a hardware-fit guarantee.
Physical-memory detection remains separate and reports unknown when unavailable.
[RAM unit evidence](evidence/rust-migration/ram-units-current/receipt.json)
distinguishes native Mac rendering from the real Linux Models dialog; Windows
rendering remains source-reviewed, not executed. Required license acceptance
is explicit for a download. Advanced Models exposes the ordinary completion cap
(1–128 tokens), additionally bounded by the selected completion-length profile;
utility requests retain their separate 128-token cap.

On macOS, App Settings can add/remove hard exclusions: case-insensitive
substrings of app identifiers/names, not exact app rules or website hosts.
Built-in terminal exclusions retain the explicit Terminal AI-prompt exception;
custom exclusions remain hard blocks and shell prompts remain excluded.
Windows preserves shortcut collisions rather than silently reassigning another
action; OCR, terminal completion, native dictionary correction and Mac-specific
compatibility controls remain unavailable, with saved preferences preserved.
Instructions append language defaults, global, app, then exact-host text; an
empty scoped instruction does not erase inherited text. Learning strength zero
suppresses learned guidance without deleting history or changing recording/use
consent.

Statistics include a year period and month grouping. “Estimated time saved”
means accepted characters divided by five per second, not measured productivity.
Linux diagnostics distinguish actual X11 shortcut conflicts, stopped interception
and disabled interception from prediction enable/pause state.

### Model quality and limits

Local does not mean infallible. Suggestions can be awkward, incorrect, in the
wrong language, or absent. Review them before accepting. Model size,
quantization, context, hardware and the target field all affect the result;
larger models are not a promise of better completion in every language.
Use the built-in preview with representative, non-sensitive text before choosing
a model. Gemma and Qwen options have different resource requirements; imported
GGUF files must be compatible with the bundled inference engine and prompt format.

## Privacy

The default GGUF backend runs locally, without an account, automatic telemetry
or cloud sync. Model downloads and update checks use the network. Extra context
and writing history are off by default. Diagnostic exports are explicit and
local; BroTypist does not automatically upload them.

Opt-in history uses encryption backed by the OS credential store, with no
plaintext fallback. History and acceptance statistics have separate deletion
controls. Models and user data live outside the app installation.
Personalization does not train the model. Optional loopback-server backends are
only as private as the server you configure; custom instructions are ordinary
settings text, not secret storage.

Recording (`record_history`) and use (`use_history`) are independent permissions.
Both default off for new profiles. Disabling either does not delete history or
grant the other. Legacy profiles without the new use field inherit their
historical recording choice once, including existing scoped recording choices;
after migration, missing scoped values mean inheritance. Recording/use rules
may explicitly opt in an app or exact website independently of their global
defaults; the global context ceilings described above are a separate policy.

Personalization uses authenticated authored vocabulary and aggregate style,
not raw-sentence replay or weight training. Strength is **not a training
percentage**: zero disables the contribution; positive values bound the number
of recurring vocabulary items (up to 24) and accompany the style guidance.
Evidence must match the writing language and eligible app/exact-host scope.
Incremental refresh reuses only unchanged authenticated records, not merely
matching record IDs.

Retention is `history_retention_days`: **0 keeps history indefinitely**; a positive
value deletes authenticated dated records older than the UTC cutoff while secure
history is unlocked. Unknown legacy dates are preserved, not guessed. Explicit
deletion and expiry also forget derived learning and revoke affected in-flight
state so unchanged deleted sessions cannot immediately recreate the evidence.
Aggregate acceptance statistics remain separate.

Explicit local diagnostics contain bounded, content-free accounting, including
standalone model preparations and actual native draw callbacks where observed.
Unknown/unreported work is not zero. A callback does not prove compositor
presentation, physical latency, energy use or compatibility with every text field.

## Develop

Use **Rust 1.99.1** and a native C/C++ toolchain. The application, worker, updater and build tooling are Rust; inference uses the pinned native llama.cpp submodule, not Python bindings.

```sh
git clone --recurse-submodules https://github.com/renan-brasilio/brotypist.git
cd brotypist
cargo build --locked --workspace --bins
cargo test --locked --workspace
```

Build on the target OS and architecture. macOS packaging requires full Xcode 27.1 or 27.2; CI remains pinned to 27.1 and each bundle records its actual compiler version. Windows requires native MSVC Developer PowerShell and the Windows SDK, including the selected installation's `VCINSTALLDIR` and `VCToolsRedistDir`; native llama/ggml DLLs share the dynamic CRT and bundles carry authenticated app-local Microsoft redistributables. Linux uses GTK 3, AT-SPI, Pango/Cairo, session D-Bus, X11 and the explicitly selected KDE input-method transport. The [native CI workflow](.github/workflows/ci.yml) records the dependencies and separates development artifacts from releasable installers.

Windows compiler scratch uses a separate private temporary directory, not the requested bundle destination. Keep `TMP`/`TEMP` short for legacy MSBuild FileTracker limits. Failed builds report and retain both staging paths; successful builds remove compiler scratch without changing OS long-path policy.

Use an absolute, disposable `INKLOW_HOME` for experiments. Before any manual app
or CLI launch, validate that its value is nonempty and resolves to the intended
fresh private directory, not the normal profile. Stop if allocation or validation
fails; never launch with an empty/unset value or silently fall back. Confirm the
running instance uses that private profile before interaction. Do not copy normal
settings, history or credentials into it. This isolates settings, history, updates
and models (`INKLOW_HOME/models`); it never migrates the normal profile's weights.
Existing profile and credential-store identities are retained.

Downloaded models are retained app data, not disposable cache: `~/Library/Application Support/inklow/models` on macOS, `$XDG_DATA_HOME/inklow/models` (normally `~/.local/share/inklow/models`) on Linux, and `%LOCALAPPDATA%/inklow/models` on Windows. App startup verifies and retains selected primary and configured battery catalog weights from the old default cache without deleting originals. Existing durable weights and custom model paths are unchanged.

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
- `cache_reuse_smoke` exercises real fresh/reused logits, cancellation and retained-memory guards. Exact matched schedules remain strict; cross-schedule float drift is compared with an independent fresh/fresh floor. [Current-primary calibration](evidence/rust-migration/cache-schedule-calibration-current.json) passes 55 prompts in all three gate classes, and [real-grammar shortlist parity](evidence/rust-migration/grammar-shortlist-parity-current.json) passes 114 units/456 trajectories. These bounded receipts are not semantic, saved-checkpoint or release approval.
- `cargo run --locked -p xtask --bin BroTypist-review-predictions -- --review-input report.json --reviews labels.json --output reviewed.json`: attach explicit human/assistant labels offline. This independent binary loads no app settings, models or inference engine; it preserves raw outputs and refuses to overwrite reports. Unix output uses mode `0600`; Windows uses inherited ACLs, not a POSIX-permission guarantee.

Implementation and acceptance: [BroTypist migration and verification register](docs/rust-migration/README.md#current-unreleased-evidence-not-release-approval) — behavior, safety contracts, evidence and remaining gates.

The [clean 0.5.4-based integration](evidence/rust-migration/pr-integration-current.json)
passes **800 reported Mac test invocations** across 26 summaries (one ignored),
plus binary/example builds and formatting. Its isolated paused CLI reports 0.5.4
without starting shortcut hooks; 80 deterministic word/emoji public-seam smoke
checks pass. This is source/CLI verification, not a new model-quality, native
Windows/Linux, installer or release acceptance run.

The [remaining-Tab follow-up](evidence/rust-migration/tab-followup-current/receipt.json)
records **789 pre-integration Mac test passes** (784 parent-suite plus five child
invocations; 26 summaries, one ignored), rebuilt binaries/examples and formatting.
The user confirms both the reported **Tab and emoji bugs are fixed**. The final
WhatsApp test retained seven exact completion edits, two genuine no-offers,
zero hook timeouts and no validated Tab-gap failure. This is bounded manual
acceptance, not the full host/security/IME/release matrix.

Earlier failed candidates remain negative evidence. Known successful Unicode
posts can await exact readback through a freshly proved transient AX race for
at most two seconds; uncertain edits are never replayed. A real multirow
whitespace gap remains correctly refused rather than drawn with guessed geometry.
Historical owned geometry checks cover Tab stops, trailing whitespace and NFD
text, but remain partial for one unavailable non-BMP font-coverage prerequisite.
The eight completion/emoji component cases predate the final reservation policy;
the separate user test above verifies that policy.

Earlier [Tab/emoji macOS source gates and owned-native evidence](evidence/rust-migration/tab-emoji-current/receipt.json)
record **743 reported test passes** (738 parent-suite passes plus five successful
child invocations; 26 result summaries, one ignored), locked/offline workspace
all-targets/native-smoke tests, binaries/examples and formatting. The earlier
[737-pass corrected-observation-clock checkpoint](evidence/rust-migration/canonical-cancellation-current.json)
retains its historical scope.
The [emulated Linux AMD64 locked/offline gate](evidence/rust-migration/emulated-linux-current/receipt.json)
passed 699 reported passes (693 parent-suite plus six child invocations;
27 summaries, two ignored) with `--test-threads=1`, plus binaries/examples.
That Linux gate was rerun successfully after the Wayland proxy-teardown correction.
Its first concurrent run retained three UnixStream fixture read timeouts;
no assertions or deadlines changed for the serial comparison. The earlier
[735-pass macOS/Linux ARM64 checkpoint](evidence/rust-migration/ram-units-current/receipt.json)
remains historical. These are local uncommitted-source checks, not native Windows,
release or physical-input acceptance. Durable scoped evidence includes [277 Python/Rust storage checks](evidence/rust-migration/storage-interoperability-current.json),
[18 learning-store scenarios](evidence/rust-migration/learning-store-current.json),
and [22 native dictionary checks](evidence/rust-migration/native-dictionary-current.json)
(en-GB/pt-BR exercised; en-US unavailable, not substituted).

The Tab/emoji receipt separates three kinds of evidence: regression tests for
renewal after verified exhaustion; actual owned-native component runs; and
nonmutating live WhatsApp metadata. The controlled-provider `--owned-choices`
run passed eight acceptance cases, including `:sorriso` and `<3>` through actual
painted native choices with exact readback and duplicate refusal. The separate
`--owned-emoji` run exercised the real Mac bridge against an authenticated owned
AppKit child with read-only selected text and writable selection range/value:
two range replacements preserved astral/NFD surrounding text, with one
PID-targeted key pair and one native text change each. These are not actual
WhatsApp edits, physical typing, undo or full host-matrix acceptance. No new
native Windows/Linux run, model-quality, latency or power claim follows;
**178/192**, the 14 no-offer failures and all 15 held gates remain unchanged.

The [headless configuration receipt](evidence/rust-migration/config-apply-evaluation.json)
records seven acknowledged validation admissions and five matching prepared/done
pairs, including real selected-model preparation and retention, merge,
persistence recovery, cancellation and shutdown. It does not exercise native UI,
physical power transitions, in-flight GPU/hash races, immutable-file attestation
or a successful distinct battery model.

The [owned AX reuse run](evidence/rust-migration/ax-read-reuse-current.json)
reduced actual frame acquisitions from 3 to 2 for the same three requests and
identical capture, with independent widget readback. Reuse ends before fresh
appearance/focus/anchor barriers; this is not latency, physical-input or full
OS write-confinement proof.
[Generated notices](evidence/rust-migration/emoji-notice-current.json) actually
include Unicode emoji 17.0 data and the full Unicode License V3; this was a
notices-generator run, not packaging or legal certification.
[Word-tools smoke](evidence/rust-migration/word-tools-current.json) passed 80
deterministic public-API checks for boundaries, emoji, decoded choices/ordinals
and pure acceptance, not model semantics or native insertion. No trigger
customization was adopted.

Windows generic UIA advertises replacement only for an existing selection,
not arbitrary range movement; collapsed carets still permit ordinary insertion.
The exact selected-readback transition permits loss of selection-only replacement
capability after the selection collapses, while requiring matching authority,
surrounding text and resulting caret. Earlier Mac/Linux gates exercised generic
runtime selected-replacement/readback but excluded the pure Windows decision
module. Now [all 12 pure Windows decision tests](evidence/rust-migration/portable-windows-decisions-current.json)
compile and pass on both hosts through one canonical
`platform::windows_decisions` module under `cfg(any(target_os = "windows", test))`;
native Windows UIA, hooks and lifecycle remain unverified.

Full native acceptance remains pending. The current [Mac render-only evidence](evidence/rust-migration/ram-units-current/receipt.json)
records 86 captures and 117 geometry observations, retaining the existing pane
and Unicode resize scenarios. Catalog availability is an explicit render fixture,
not model-filesystem inspection or loading; no Controller/services or actions run.
These native NSView bitmaps do not prove full shell persistence, physical typing,
Windows acceptance or WindowServer/compositor presentation. The earlier
[78-capture/107-geometry receipt](evidence/rust-migration/native-settings-current/receipt.json)
and its six reviewed images remain historical. A controlled GTK owned-widget
interaction run passed separately.

[Source-bound Linux CLI navigation](evidence/rust-migration/linux-cli-models-current/receipt.json)
and its reviewed image prove actual first-open AI Models and native Quit (exit 0,
no lingering children), with byte-identical settings and a memory-only paused
override; this CLI run loaded no model. The [system D-Bus fix](evidence/rust-migration/linux-dbus-linkage-current.json)
removes duplicate vendored exports without a package upgrade; GTK children now
show before selecting Models.

The current [Linux full native-shell receipt](evidence/rust-migration/ram-units-current/receipt.json)
passes 99 assertions through stage 45 and clean canonical Quit on current source:
actual English word/Portuguese full preview acceptance with GTK readback,
six Welcome steps, independent consent dialogs, real model-selection
success/failure/cancellation, exact dirty Unicode instruction retention,
external/live rebase and concurrent updates across panes/resizing, guarded
close/Quit Cancel, and native persistence failure/recovery. Three added assertions
exercise the real Models dialog: unspecified memory is not numeric zero, positive
memory remains visible, and browsing changes neither runtime/saved configuration
nor the Settings draft. This uses actual runtime catalog inventory, with no
installation fixture, download request, license consent or saved model selection.
The earlier [96-assertion receipt](evidence/rust-migration/linux-native-shell-latest/receipt.json)
remains historical, with four parent-viewed images:
[English preview](evidence/rust-migration/linux-native-shell-latest/preview-english-visible.png),
[Portuguese acceptance](evidence/rust-migration/linux-native-shell-latest/preview-portuguese-accepted.png),
[independent consents](evidence/rust-migration/linux-native-shell-latest/privacy-independent-consents.png),
and [compacted native choices](evidence/rust-migration/linux-native-shell-latest/choices-eligible-subsequence.png).
The [pre-wave receipt](evidence/rust-migration/linux-native-shell-current/receipt.json)
remains historical. Native UI delivery is not universal platform or release
acceptance: these are ARM64 Xvfb/owned-widget observations, not physical input,
real IME, KWin, Windows, external-host editing or installer acceptance.

[Current target-owned GTK3/Qt6 clients](evidence/rust-migration/target-owned-client-current.json)
separately pass compact choice ordinal 2, GTK pre-paint refusal and Qt exact
selected/collapsed Unicode replacement/readback/undo with synthetic dispatch.
[Stock AT-SPI](evidence/rust-migration/stock-atspi-current.json) instead proves
four captures and eight mutation refusals with unchanged text/carets, with zero
insertion attempts or confirmations. Missing inactive-composition/inline/range
evidence prevents treating stock capture success as positive insertion support.

The [Mac ordinary-model matrix](evidence/rust-migration/native-owned-observations.json)
still has an **overall false gate** (two English abstentions, two Portuguese
positive draws/full-prefix acceptances). Canonical Mac owned Welcome aborted on
input interruption before inference requests/edits and cleaned up; its positive
bilingual gate remains unexercised, not a permission/model failure. Linux success
does not close these Mac, signed-host/TCC or full disposable Mac-shell gates.

Production keeps the original v19/v22 full-prefill **layout**, full-SWA and flash off.
The NFC model-replay correction below changes replay bytes, not that layout.
[All six isolated layout/cache candidates plus the fixed reminder are rejected](evidence/rust-migration/prompt-cache-ablation-observations.json).
Reference freezing regresses grounding/work; learned freezing preserves output
and decoded work while reducing API calls, without measured CPU-time benefit.
Decoded-token savings alone are not latency or quality acceptance.
Historical [saved-state/flash](evidence/rust-migration/native-state-policy-evaluation.json)
and [learning-bias](evidence/rust-migration/learning-bias-evaluation.json) negatives
remain intact. [Calibrated saved-policy evidence](evidence/rust-migration/cache-policy-calibrated-current.json)
still fails 23 of 80 admitted checkpoint cases: all four arms fail exact/numeric
gates but pass decisions; three independent live controls are bit-exact. This is
not an ordinary live-KV defect or grounds to change defaults.
The [pre-NFC publication frontier](evidence/rust-migration/publication-frontier-current.json)
preserved all 192 generated texts/mechanical outcomes and 269 native-call
outputs/seeds/non-timing work records at its **175/192** absolute baseline.
The current [NFC healing correction](evidence/rust-migration/nfc-healing-current.json)
fixes the actionable missing-`o` replay bug: only model replay is normalized;
the exact original NFD authored bytes, reference and suffix remain unchanged.
There is no whitespace prohibition, language special case, layout rearrangement
or new preset. Full64 now passes **178/192**: exactly three target improvements,
189 unchanged texts/mechanical outcomes and zero regressions, not 192 passes.
All 52 named long controls preserve required outputs, non-timing work and seeds;
four publication callback-count changes are confined to that long comparison,
not changes in final outputs. Current full64 records 269 native calls, 117
known-terminal emissions before return and 211 exact nonempty finish-offer
agreements with zero mismatches; this is not compositor or latency proof.
The [14 remaining no-offers](evidence/rust-migration/writing-no-offer-analysis-current.json)
are measured model abstentions (ten replay-only stops, four replay-plus-comma
stops), still absolute-quality failures—not an external blocker or proof they
cannot be improved. No additional production bug was justified by investigation.
[Post-NFC cache calibration](evidence/rust-migration/nfc-cache-calibration-current.json)
passes all 55 prompts and three gate classes. Direct healing passes 3/3;
historical [production-worker healing](evidence/rust-migration/nfc-worker-healing-current.json)
also passed its 3/3 targets, but exited 1 because its short after-result probes
found already-completed requests. That receipt remains inconclusive, not rewritten.
The [current canonical cancellation probe](evidence/rust-migration/canonical-cancellation-current.json)
exits 0: both probes pass in all three sessions, with matching terminal accounting,
retirement/quiescence and acknowledged release/shutdown. The after-result-only
128-token/32-word workload observes nonempty nonfinal IPC output while the matching
request is active; ordinary cases remain cap 24/medium. Diagnostic observation
timestamps now follow lock waits; operational grace/idle policy is unchanged.
This proves the observed supervisor contract, **not native interruption or native
cancellation acknowledgement** (both unknown), physical freshness or GPU reclamation.
[Current live reuse](evidence/rust-migration/live-cache-reuse-calibrated-current.json)
passes its updated original smoke. [Native utility-grammar admission](evidence/rust-migration/utility-grammar-admission-current.json)
passes ten fixed cases, including `[]`, without inference or semantic scoring.
The [pre-NFC full64 matched comparison](evidence/rust-migration/writing-full-corpus-parity-current.json)
retains its historical 192-text relative preservation and **175/192** absolute
result, including the three then-unfixed NFD joins and 14 no-offers.
[Boundary/replay characterization](evidence/rust-migration/no-offer-boundary-replay-current.json)
rejects no-healing: both arms have 19/54 no-offers, but the candidate adds 18
trailing-space abstentions and splits `feedback` into `feed back` in all three
partial-word controls. Removing replay metadata, grammar, stripping and reservation
together is not a grammar-only experiment; its cold-KV/selected-reference schedule
is not the retained full64 trajectory. No later full64/long/per-keystroke adoption
gate is claimed. Production prompt layout and cache policy remain unchanged.
[Completed assistant semantic review](evidence/rust-migration/semantic-review-current.json)
covers 89 unique input/output IDs across all 192 occurrences. A selected blind
second assistant review agrees on 143/156 dimension comparisons and disagrees on
13; both label sets are retained, not merged. Agreed defects include Portuguese
`sala` leaking into English, unsupported `next week`, and Portuguese `ágiles`.
These are attributed assistant judgments, not human validation or population
calibration; **178/192 and all 14 mechanical failures remain unchanged**.
The [emulated Linux AMD64 native shell](evidence/rust-migration/emulated-linux-current/receipt.json)
passed 99 assertions, stage 45, with clean exit under Xvfb/Openbox/Picom before
the later Wayland-only correction; X11/UI source was unchanged by that correction.
Linux emulation is not Windows, physical typing or release acceptance.
The same receipt records an actual nested genuine KWin 5.27.11 probe, **partial
(exit 2), 10/13 gates** after fixing version-1 keyboard-proxy teardown: destroy
the local proxy without sending unsupported `keyboard.release`, retaining owning
context teardown. The real grab/deactivate/context-destroy path then has zero
protocol errors, versus the prior pre-cleanup `invalid object 11` error.
GTK Wayland mapped with exact Unicode readback; input-method/panel bind requests
and activation remain observed. Missing KWin D-Bus ownership and emulation-masked
executable identity leave identity-qualified binding false; raw protocol traffic
is not native acceptance. Rendering/module limitations remain, no model was loaded,
and forced cleanup is not canonical Quit. Full KWin/manual commit, physical IME
and normal KDE-selection gates remain unmet.
The 15 held acceptance gates retain their scope; completed implementation and
these follow-ups do not complete the roadmap or approve quality.
GPUI's isolated
[owned-Linux receipt and images](evidence/rust-migration/gpui-owned-linux/)
do not establish adoption or add a product dependency. Windows/Linux remain alpha.
Wayland source is manual-only: no ordinary inline/preedit or popup fallback;
explicit KDE input-method selection does not change that contract. Independent
target-owned GTK3/Qt6 clients and their scoped evidence remain separate.

CLI model selection respects the resident inference owner: while BroTypist is
running, select the downloaded model in **AI Models** rather than creating a
second validation owner. Launch overrides do not silently persist preferences.


## Contribute

Found a problem? Open an issue with the app version, OS, target application and reproducible steps. Use synthetic text; never attach private drafts, profiles, keys or model weights.

Keep pull requests focused. Document public contracts and non-obvious lifecycle decisions, add behavioral regressions where needed, and report what you exercised on the actual platform. Preserve saved settings, explicit consent and safe handling of unsupported inputs. Discuss larger changes first.

[Design direction](DESIGN.md) · [Verification](evidence/VERIFICATION.md)

Third-party components and model weights retain their respective licenses.
