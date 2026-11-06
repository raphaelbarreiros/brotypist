# BroTypist: feature-preserving Rust conversion

Status: **[0.5.3 is the published Latest macOS-only stable release](https://github.com/renan-brasilio/brotypist/releases/tag/v0.5.3) (release `433791567`, tag source `f7e80cfddaa69ca03e237cd7689cf2dd0f7d34c7`); Windows and Linux remain ALPHA.** This Settings-polish patch passed 542 local test results, formatting, 43 native captures, 67 geometry observations, four retained Unicode-draft resize observations, and source-bound signed DMG/ZIP installer/launch/Quit and real Tiny-worker checks. All six public assets matched sealed local bytes and SHA256SUMS; the update signature verified against the pinned publisher. [Publication and capture evidence](../../evidence/rust-migration/macos-0.5.3-release-verification.json). Tagged-source Actions were still pending during the GitHub incident at publication and are not counted as passing. No new full prior-version upgrade matrix or physical typing acceptance is claimed.

Historical 0.5.2 release `431641010`, source `730cb78cec9b9bb7032bf6634a3b215342c4aafa`, separately verified retained primary/battery models, visible owned-field predictions, genuine 0.4.13/0.5.0 upgrades, delayed-start rollback and held-lock refusal. Section 24 retains earlier 0.5.1 observations; Section 23 preserves the 0.5.0 prerelease history. macOS packages remain Apple Development-signed, **not notarized**; model-quality, application-geometry/security and Windows/Linux acceptance limits remain open. This is not full three-platform parity.
Baseline: `6eaeed5fe56e1ea8dcf878b1b7498f922c26fd59`, version 0.4.13.

Requested outcome: convert the application to Rust for macOS, Windows and Linux while retaining all existing functionality. The current explicit release priority is **macOS-only 0.5.3 stable**, then continued Windows/Linux **ALPHA** work. Missing Windows/Linux evidence is not a pass and those platforms are not included in the Mac stable publication. This does not authorize reduced functionality, cloud features, security-policy changes or consent changes. Local replacements require explicit host authorization and preserve rollback copies and external user data.

Read only the section/artifact needed:

- [Review and observed baseline](review.md): current findings, baseline tests/runtime checks, known CI failures.
- [Feature acceptance register](parity.md): P01-P32, platform limitations and exact data/identity contracts.
- This file: proposed boundaries, dependency-ordered packages, bounded subagent protocol and release gates.

## 1. Decision and non-negotiables

**Approved architecture:** shared Rust behavior/services; OS-native adapters and interfaces. macOS uses Rust Objective-C bindings to AppKit/AX/CoreText/Quartz/Security/Vision; Windows uses UIA/Win32; Linux uses GTK 3.24, AT-SPI/Pango/X11/KWin behind explicit capability contracts. Linux GTK/Pango/Cairo Rust bindings are unified at 0.18 to avoid duplicate native `links` dependencies. No Tauri/webview or Swift shell.

This removes Python from the **final shipped application**, not every non-Rust dependency: llama.cpp remains C/C++, and OS frameworks/toolkits remain native dependencies. Replacing it with Candle/MLX or translating every upstream C/C++ dependency is not required and would add model-compatibility risk.

The user selected Rust-owned native UI over the presented Swift-shell and webview-settings alternatives. The candidate checkout now selects Rust exclusively: obsolete Python runtime, tests, packaging launchers, build/evaluation scripts and dependency manifests were removed after the Rust candidate ran. The complete unchanged Python baseline remains in an external detached worktree and Git history for remaining comparisons, never a shipped fallback. Language-neutral corpus/assets and historical evidence are preserved. This branch source cutover is not production rollout or closure of the remaining native gates. Rust 1.99.1 is installed and pinned in `rust-toolchain.toml`.

Performance is measured, not assumed from language: avoid tick-by-tick full-draft copies, repeated scalar indexing, synchronous keychain/AX/model/power work in hooks, or a universal mutex across OS/UI/inference. Reuse immutable text buffers and checked offset maps when text changes; bound caches and queues according to actual lifecycle safety. Do not drop user input to satisfy a memory limit during an issued uncertain mutation.

## 2. Workspace ownership and interfaces

Production uses the `crates/brotypist` application crate with the logical module boundaries below, an independently linked `crates/brotypist-updater` helper, and Rust `xtask` distribution tooling. Do not create empty boundary crates.

| Owner | Boundary | Must not own |
|---|---|---|
| `core`, `features`, `spelling`, `runtime` | Exact scalar-indexed observations, policy/rules, suggestion/choice/spelling decisions, presentation/input leases, scheduling transitions with injected logical time. | OS handles in pure decisions; duplicate prediction lifecycle. |
| `config`, `paths`, `storage`, `models`, `updates` | Compatible preferences, encrypted SQLite/keychain history, statistics/learning, model acquisition, signed feed/download. | Native edit authority. |
| `inference_native`, `providers`, `writing`, `worker`, `process_client` | Prompts/token budgets/healing/output fences, three providers, pinned llama.cpp ownership and process supervision. | Foreground reads, OS hooks or profile mutation. |
| `platform`, `system`, `power`, `instance` | Per-OS capture/edit/input/context/permissions, special Mac adapters, system services and lifecycle ownership. | Cross-platform policy duplication or inferred acceptance from dispatch. |
| `cli`, `ui`, `preview` | Native main loop, all controls, resident lifecycle, visible presentation acknowledgments and ephemeral preview. | Model evaluation on the UI thread. |
| `brotypist-updater` | Independently linked offline binary using the same canonical trust/transaction source without the app/llama dependency. | Capturing typing, loading models or owning user data. |

The integration owner controls shared manifests/lockfile, interfaces, release identity and CI. A dedicated inference owner controls `build.rs`; packaging consumes its fresh native-library manifest, not guessed target-cache files. Agents submit dependency requirements rather than competing lockfile edits. Each owner implements its nonoverlapping production files and meaningful behavioral tests together.

Freeze these cross-slice contracts before dependent packages start:

- **Text indices:** separate `ScalarIndex`, `Utf16Offset`, `Utf8Offset`; checked conversions and rejection of surrogate/interior/out-of-bounds ranges. Preserve original text; normalization/casefold only for derived matching. One cached offset map per immutable text revision avoids repeated rescans.
- **Snapshot versus authority:** text/policy identity is distinct from window/document identity, permission epoch, native-input revision and layout generation. Ordinary geometry refresh can reposition; authority change invalidates context, output, display and edit claims.
- **Capabilities:** append, existing-selection replacement, arbitrary atomic replacement, safe inline geometry, explicit chooser anchor, native manual controls; never infer one from another. Unknown permission/composition/security state is not “allowed.”
- **Decision/event boundary:** observe field, command, prepared context, model chunk/final, spelling verdict, presentation acknowledgment, native edit outcome, configuration/power change. Effects request work/render/edit/record; the decision core does not execute native effects.
- **Presentation:** acknowledgment contains authority/revision, exactly visible completion prefix or complete choice rows and visibility generation. Hiding/reordering invalidates acknowledgment. Ordinary suggestion is grey same-line text or absent, never a popup substitute.
- **Edit outcome:** distinguish rejected-with-no-mutation, dispatched-awaiting-observation, applied-with-verified-result and indeterminate/possibly-mutated. Record acceptance/history against verified outcomes, not input queue delivery. Never retry uncertain edits automatically; keep replay ownership until the native transaction resolves.
- **Inference:** configuration-only prepare; cumulative text replaces prior stream text, not append-deltas; immutable request ID/generation/authority, bounded messages, proceed barrier, cancellation epoch and explicit release watermark. One native context owner, outside UI/hook threads.
- **Privacy revocation:** cancel prepared/in-flight work, release model context as required, clear references/OCR/derived learning/display leases, and prevent stale events from reauthorizing reads or writes.
- **Persistence:** exact formats/paths/credential identity in parity.md. Never point an unfinished candidate at the normal profile. Synthetic crypto compatibility uses injected disposable keys, not production key reads.
- **UI actions:** captured app/domain and displayed updater intent survive deferred execution. Dirty editors and three-way settings rebasing are functional contracts, not styling details.

Candidate binding research: [objc2](https://github.com/madsmtm/objc2) supports main-thread markers and retained native delegates; [llama-cpp-rs](https://github.com/utilityai/llama-cpp-rs) exposes explicit decode/sampler/token/KV APIs. Neither proves BroTypist parity. Initially pin llama.cpp to the native revision used by `llama-cpp-python==0.3.36`: `0c1e57098bba43ac29e6e3b677cdceebdd22334f`. Verify upstream pin, wrapper defaults/sampler order and actual Gemma4 load before adopting a Rust binding revision. If a binding cannot target it safely, use a small audited C ABI wrapper, not an unverified inference-engine substitution. Pin reviewed versions/licenses for all dependencies in Cargo.lock.

## 3. Implementation waves

These are complete, testable packages, not permission to stop at a scaffold. A package is integrated only after its focused behavior works. The old app remains the reference until every required parity row has evidence; missing gates remain open rather than being redefined as out of scope.

| Wave / packet | Owned implementation | Dependencies | Required parity / evidence |
|---|---|---|---|
| 0A: baseline contracts | Synthetic canonical settings/history/Unicode/worker/event fixtures and explicit source provenance; no personal writing or production keys. | Frontend strategy and toolchain available. | P01-P32 inventory, baseline CI issue disposition. Capture intended semantics, not incidental wording/defaults or known bugs. |
| 0B: runnable vertical probe | Disposable Rust CLI + actual pinned Tiny and cached Gemma4 generation; native owned Mac text field + ghost panel + explicit acceptance readback. | 0A text/authority interfaces. | Prove binding/model compatibility, native ownership, same-line rendering and event-loop integration before broad port. This is a disposable feasibility probe, not the deliverable. |
| 1A: core/features/spelling policy | `brotypist-core` observations/indices/acceptance/features/chords and pure spelling planning. | 0A; shared contract frozen. | P01, P04-P06; adversarial Unicode and decoded choice safety R3/R11. |
| 1B: config/data services | Paths/settings/rules/SQLite/AES-GCM/keychain adapters/statistics/learning. | 0A persistence fixtures. | P07-P09, P28; bidirectional ciphertext/profile compatibility and corruption/lost-key refusal. |
| 1C: models/providers | Catalogue/acquisition plus prompt/token/healing/stream engine and GGUF/HTTP providers. | 0B native model feasibility; shared generation types. | P13-P16; all existing families/quantizations accounted for, actual cached models; R2/R3/R11. |
| 1D: update services | Signed feed, download, state machine and independent transaction data model. | 0A trust/identity fixtures. | P29; tamper/redirect/size/hash/cancel/daily policy. No live release mutation. |
| 2A: controller + worker | Owned scheduling/effect integration, supervisor/process ABI, preparation/cancel/release, spelling and presentation leases, history commitment. | 1A-1C. | P02-P03, P11-P12, P17; deterministic races and actual child pipes/exit; R5-R8. |
| 2B: Mac observation/geometry | AX/framework ownership, browser/document identity, ranges/typography/CoreText and native overlay primitives. | 1A authority/capability contracts. | P19-P20; Secure Input unknown refusal R1; no protected reads; actual owned controls. |
| 2C: Mac mutation/special adapters | Input gate/hooks/replay, native edits, terminal, Docs help, language/spelling, ScreenCaptureKit/Vision and consent. | 2B stable capture contract; 1A. | P05-P06, P10, P21-P22; honest mutation outcomes, fixed diagnostics R13; special unsupported cases retained. |
| 3A: Mac product shell | Complete native settings/menu/onboarding/preview/power/startup/URL/CLI integration. | 1B, 1D, 2A-2C. | P11-P12, P18, P26, P28, P31; actual pane/control/draft/accessibility/focus/close/Quit flows. |
| 3B: Windows native owner | Native capacity/credential/power/startup, UIA capture/edit/geometry, hook lifecycle and complete chosen settings/resident/preview UI. | 1A-1D, 2A. | P06-P18, P23, P27-P28, P31; native Windows CI and owned host fields, R2/R8/R9. |
| 3C: Linux X11/product owner | AT-SPI/Pango/X11, credentials/power/startup, complete chosen settings/tray/no-tray/preview UI. | 1A-1D, 2A. | P06-P18, P24, P27-P28, P31; real session and insertion-only limitations. |
| 3D: KDE transport owner | Private socket/bootstrap, input-method protocol/XKB/SHM/manual surfaces; C ABI retained only until complete Rust equivalent is verified. | 3C toolkit-init contract and 1A. | P25; real KWin manual controls/refusal/input forwarding/cleanup; obsolete harness R10 replaced. Do not enable ordinary inline. |
| 4A: updater + Mac distribution | Offline Rust updater, signed native bundle, Icon Composer, DMG/ZIP, startup ACK/rollback and developer promotion safety. | All Mac behavior plus 1D. | P28-P30; actual healthy/failed/delayed startup transaction, quarantine/identity/data preservation; R4 and delayed-child investigation. Fix icon toolchain prerequisite without changing OS safety policy. |
| 4B: Windows/Linux distribution | MSI/ZIP, DEB/portable, desktop/input-method ownership, uninstall/data preservation and platform update/held semantics. | 3B-3D and updater contract. | P28-P30; native install/upgrade/rollback/remove evidence. No publication until authorized. |
| 5: full parity and clean cutover | Complete three-OS behavioral/host/installer matrix, remove obsolete runtime/callers/dependencies/build paths, update canonical docs/release tooling. | All prior packets. | P01-P32; no Python runtime/shipped shim, no dropped setting/control/provider, no false platform-support claim; one candidate replaces one old runtime only after proof. |

Parallelism follows true dependencies: wave 1 packages can run together; Windows/Linux owners need not wait for Mac UI completion once shared interfaces are stable. Narrow Mac mutation work waits for AX identity/ownership, not for unrelated storage. Large UI packages are subdivided by complete pane groups only after the chosen frontend supplies real shared controls/navigation; no duplicate UI state models.

## 4. Low-context subagent execution protocol

Use one integration owner and normally four to six active substantial owners. Increase only when interfaces/file ownership are genuinely independent; more agents reading the same whole repo increases context and integration churn.

Each dispatched task receives a short packet, not the conversation or every review:

1. Goal and packet ID, baseline/current integration revision, owned files/tests, non-goals.
2. Only relevant parity IDs and exact shared API definitions; source/test anchors for that slice.
3. Observable acceptance scenarios, platform authority and known issue disposition.
4. Restrictions: no production-profile use, install/publication, dependency/manifest/lockfile edits, mid-flight build/test/lint/format; request missing interface changes from integration owner.
5. Return contract: modified files, behavior implemented, contract changes requested, exact verification commands **not run**, blockers, and brief handoff. Never claim checks passed without execution.

Before dispatch, persist packet decisions and verified evidence in the repository's tracker; `bd` was unavailable in this environment and no `.beads` state was found, so no parallel tracking database or fake issue IDs were created. The implementation plan is not a second issue database. Use actual tracker IDs once the approved tool is available.

Keep common interface ownership narrow. Workers use nonoverlapping files or verified worktrees outside live app/model/profile paths. The parent owns integration and checks once a wave is complete. A worker can ask another owner a concrete API question, but cannot edit another owner's shared state. Prefer compact local handoff artifacts to repeatedly restating long results. Start a fresh bounded worker for the next packet rather than accumulating every subsystem in one agent.

For each integrated packet, one independent reviewer checks semantic/data/security boundaries; avoid redundant whole-repository reviews after every edit. Parent runs relevant tests and actual smoke, then records precise evidence and advances the tracker. At final integration, run the complete behavioral suite once plus native platform/installer gates. A failed gate returns to its owning packet with evidence, not a new competing architecture.

## 5. Verification and cutover rules

- **Behavioral oracle:** frozen synthetic fixtures derived from existing tests and authored corpus; compare intended behavior, not source text, UI wording, mock wiring or incidental implementation. Known bugs in review.md receive corrected expectations with explicit disposition.
- **Inference:** exact tokens/framing/grammar/sampler configuration where deterministic; safe mechanical outputs and actual model load/stream/cancel elsewhere. Do not demand identical probabilistic sentences or claim naturalness/latency parity from one generation. Large catalogue entries without available execution environments remain named open evidence gates, not silently deleted presets.
- **Mac native:** sign and launch the candidate normally via LaunchServices using existing identity; own-window controls first, then explicitly authorized host scenarios. Require model output -> actual same-line pixels -> Tab/full/repeated acceptance -> host readback, plus IME/secure/focus/Spaces/append-only/refusal cases. CLI-host authorization does not prove bundled-app TCC retention.
- **Windows/Linux native:** actual target runners/desktops with native compiler/dependencies. UIA rejection/IME/UIPI, X11 replay/conflicts and KWin manual transport are distinct gates; cross-compilation/source mocks are insufficient. Establish available target environments before claiming completion.
- **Data:** first synthetic profiles/keys; then only authorized backed-up profile migration. Compare settings/model/history identities and bytes where intended unchanged. Do not move the profile, regenerate an inaccessible key, reset consent/TCC, replace weights or delete user data to obtain a pass.
- **Updates/installers:** signed package provenance, actual external-data-preserving install/remove, healthy GUI ACK, failed/delayed-start rollback and clean worker/hook exit. Preserve quarantine and managed package boundaries. macOS notarization and Windows signing are independent release decisions.
- **Cutover:** keep the legacy baseline immutable as an external reference while developing the complete candidate. Final source/build/runtime entrypoints select Rust exclusively; migrate all callers and remove obsolete Python/PySide/PyObjC/PyInstaller production paths. Keep useful language-neutral fixtures/evidence, not permanent runtime adapters to old code.
- **Delivery:** exact source revision/artifact identity, features exercised, pass/skip/failure counts and real native evidence per platform. No normal-app replacement, public release, upload, signing-policy changes or held-platform publication follows implicitly from local conversion.

## 6. Current verification evidence and gates

The table retains source-bound historical observations. Sections 23–24 record the 0.5.0 deployment and earlier local 0.5.1 correction; earlier passes do not automatically apply to the final 0.5.2 stable candidate. The current channel boundary is macOS stable, Windows/Linux ALPHA.

| Boundary | Observed evidence | Limits / next gate |
| --- | --- | --- |
| Integrated Rust workspace | 451 passing invocations, including two child probes; native bins/examples and actual Tiny/Gemma worker/public CLI passed. Portuguese auto continuation, ready/proceed, stale-document refusal, release/reload and clean exit exercised. | Not a coverage comparison with Python. |
| macOS native CI | Complete tests/build/CLI/worker/twelve-pane settings/Welcome/EN-PT owned-preview acceptance/clean Quit/development bundle passed again in [41119387237](https://github.com/renan-brasilio/brotypist/actions/runs/41119387237), head `2f4f1f0` (14m40s). | Owned NSView captures are not external-screen, signed-host or live TCC proof. Authorized installation is recorded separately below. |
| Linux native CI | The same run passed the full Linux job (17m37s), including actual DEB install/replacement, owned X11/AT-SPI Quit, installed Tiny worker/public CLI, data-preserving purge and relocated portable diagnostics. The earlier inspected installer receipt explicitly records successful cleanup and `full_installer_matrix_passed=false`. | Previous-release upgrade and KWin manual transport remain unverified. |
| Windows native CI | Native [41236356500](https://github.com/renan-brasilio/brotypist/actions/runs/41236356500), package merge `1e105adc`: compile/checks, 278 tests plus two child invocations, all 14 owned-UI checks, PE/DLL closure and exact ZIP payload passed. Relocated canonical ZIP now passes real Tiny worker/public inference and visible Settings/Quit with unchanged synthetic data/autostart. [Current proof](../../evidence/rust-migration/windows-build-completion.json); delivery details in section 18. | MSI still reports exclusive per-user context 2/State 5/AssignmentType 0 but creates machine ARP64. Strict gate and overall installer failure remain; repair/installed runtime were not reached. External-app physical typing, upgrade, rollback, signing/publication are not certified. |
| Signed Mac distribution | Candidate `a278b6b` matched the installed designated requirement. Actual DMG/ZIP extraction/copy, bundled CLI/Tiny inference, nonactivating launch, OS Quit/exit 0, removal/data-fixture preservation and ZIP traversal refusal passed. Receipt: `/private/tmp/brotypist-rust-source-a278b6b/installer-smoke/macos-installer-smoke.json`. | Receipt is explicitly `full_matrix=false`: no previous-version/OTA, notarization or physical-host typing claim. |
| Data/tool contracts | Synthetic AES-GCM and startup-ACK interoperability; relocated independent offline review CLI, source-hash/raw-output/private-file/no-overwrite behavior; authentic locked-source dependency notices passed. | Notices do not grant a first-party license. No app/model/profile access was needed by offline review. |
| Unicode | Shared allocation-free casefold matches all 1,112,064 valid scalars against immutable Python Unicode 15, correcting 129 dependency-table differences. Rules/history/choices/utilities/archive collision/MSI identity regressions passed. [Mapping proof](../../evidence/rust-migration/unicode15-casefold.json). | No normalization/category/regex equivalence claim. |

The full 51-case Gemma corpus matched every insertion/decoded-choice value and prompt-token count between Rust and immutable Python. Forty-eight raw strings were byte-identical; three differed only in alternatives JSON whitespace. Both retained the same three safe-empty availability failures (`language-en-locale-preference`, `grammar-fr-agreement`, `suffix-ja-unspaced`): **48/51 each**, not a full pass or semantic-quality approval. Reports: `/private/tmp/brotypist-rust-full-corpus-z95szwqa/report.json` and `/private/tmp/brotypist-oracle-full-corpus-iy2tsw_d/report.json`.

Six selected direct/worker benchmark cases passed mechanical checks. Separate direct EN/PT diagnostics exercised the 256-token cap, sampler/BOS/token budgets and process-lifetime RSS. Direct cancellation passed; one after-result worker cancellation was explicitly inconclusive because the producer had already finished. These are not physical-typing latency or universal stochastic-output claims.

Remaining acceptance boundaries: current Windows MSI ownership/installation/repair/installed runtime; signed external-host editing/readback and live TCC beyond the bounded observations below; actual KWin controls/refusal/input forwarding/cleanup and personal PC typing; the full cross-platform previous-release upgrade/rollback/OTA matrix; unavailable large-model scenarios and semantic model quality. No provider/model, ownership or paint-safety gate is relaxed to obtain a pass. Section 23 records historical Mac source-bound package verification, genuine healthy updater transactions and bounded negative component fixtures. Final 0.5.2 Mac stable candidate proof and publication remain separate gates; Windows/Linux remain ALPHA, Windows MSI distribution is held, and ordinary Wayland inline completion is unavailable.

## 7. Authorized local macOS installation

At the user's explicit request, signed Rust/AppKit candidate `22fb3fe2bd1472d1f5726897988fcbf0b998880f` was installed at `/Applications/BroTypist.app` using an atomic directory exchange. A private original-bundle and opaque-profile backup is retained. The exact designated requirement matches the former app; normal settings and history database remained byte-identical after completed nonactivating launch, and the selected model exists without replacing weights. Xcode 27.2 compiled the actual Icon Composer source and the canonical signed bundle; CI remains pinned to 27.1. The targeted tooling suites passed 57 invocations.

[Redacted installation evidence](../../evidence/rust-migration/macos-local-install.json) records the actual source, signature and process-health limits. CLI Accessibility diagnostics and the external AX observer were denied; those results do not establish the GUI process's live authority. No visual/inline-typing, notarization or OTA claim is made, and no permission grant/TCC reset was attempted. Main and published releases remain unchanged.

## 8. macOS menu and Settings regression correction

The user's installed-app screenshots exposed visual regressions not caught by prior owned-control assertions. The resident menu now contains only the six baseline action groups, with no battery view/percentage/charging cue or raw diagnostic/model header. Battery Settings, sampling and power policy remain. Settings restores the immutable AppKit baseline's grouped row geometry, full-width wrapped details and viewport-derived document sizing. Sidebar navigation clears the traffic lights, has an appearance-aware native selection background and scrolls independently of its bounded status footer. Retained editors and all destinations remain.

The [native layout receipt and images](../../evidence/rust-migration/macos-ui-layout/receipt.json) exercise the actual Kit/Settings, twelve panes, General at 900×640 / 760×560 / 1000×740, light/dark appearance, and scrolling through the final General row and detail. The real native menu has eight rows including separators and no custom dashboard views. The integration owner inspected the rendered General, minimum/bottom, light-mode, Personalization and Battery images. Workspace tests passed **396 tests** (plus two nested child invocations). Foreground was unchanged and no profile directory was created by this capture.

Reproduce this bounded layout check with a new absolute output path:

```sh
cargo +1.99.1 run --locked -p brotypist --features native-smoke \
  --example native_desktop_smoke -- --layout-only /private/tmp/brotypist-new-layout
```

This mode never constructs a Controller, starts hooks/services, loads a profile, requests permissions or activates the app. Its native theme-frame bitmaps are not WindowServer vibrancy/compositor, signed-host typing or live TCC proof; the full activating desktop smoke remains CI-only.

The corrected signed app, source `61c4e36284b0c3da3c34b6a4c8b5576879413a75`, is now installed and running at `/Applications/BroTypist.app`. [Installation evidence](../../evidence/rust-migration/macos-ui-local-install.json) records strict signature/inventory verification, the unchanged designated requirement, graceful old-process exit, atomic exchange, a fresh private app/profile rollback backup, unchanged settings/history bytes and the existing selected model. LaunchServices completed background launch without taking foreground or exiting during the eight-second event observation. No model replacement, TCC reset, main merge or release publication occurred.

## 9. Bounded Windows installer diagnosis

`workflow_dispatch` accepts optional `installer_artifact_run`; an empty value preserves the complete native matrix. A nonempty same-repository run ID selects only the hosted Windows artifact-reuse job:

```sh
gh workflow run ci.yml --ref migration/rust-native \
  -f installer_artifact_run=41127270547
```

The probe authenticates the original Actions artifact digest and immutable run-head/PR-merge-parent binding, preserves its raw package receipt, verifies MSI/ZIP bytes and exact portable payload, then changes only artifact paths in a probe-only InputFile for the existing native lifecycle script. It records original package source separately from probe checkout/core/adapter identities; an associated PR's current head is not immutable run identity. Model acquisition stays checksum-pinned and inside private work, never the normal cache.

This is a diagnostic lane, not a completed installer matrix: the archive lacks `worker_smoke.exe`, so installed-worker execution is explicitly unexercised rather than invented. Normal canonical worker/upgrade/rollback/publication gates are unchanged. The current CLI credential cannot dispatch Actions (403); an explicitly scoped temporary Git trigger exercised the lane and was removed afterward. [Actual replay evidence](../../evidence/rust-migration/windows-installer-scope/windows-installer-reuse.json) records verified artifact/package/model hashes and the still-failed native ownership gate, with successful install/uninstall/data preservation. It matches the external MSI/ARP behavior reported in [WiX #9323](https://github.com/wixtoolset/issues/issues/9323); no Windows acceptance criterion is relaxed. Broad registry-debug scaffolding was removed after preserving the bounded explicit-view evidence.

## 10. Shared Settings alignment follow-up

The user's follow-up exposed remaining alignment drift: form headings/notes/rows/custom blocks used different leading offsets, header controls were forced to 27 points, and switches were still allocated 42 points while current AppKit reports an intrinsic 54×24. The shared form now uses one 38-point text inset, native content-alignment rectangles for control edges and centered header placement. Switches use the OS-reported size rather than a replacement constant. Compound status/action rows and slider endpoints resize within their actual allocation; the power card fits its actual parent.

The actual native switch-overflow assertion [failed before the factory correction](../../evidence/rust-migration/macos-settings-alignment/switch-intrinsic-before.json) and [passed afterward](../../evidence/rust-migration/macos-settings-alignment/receipt.json). The latter retains 25 images, native geometry for all twelve panes, minimum/expanded General/Setup/Personalization, reachable final General content, and four resize observations preserving the same Unicode instruction editor, text and UTF-16 selection. Shared leading/trailing edges, header centers, description separation and compound-child containment are checked from Cocoa-converted rectangles, not merely control presence. Workspace regressions passed 396 tests plus two nested child invocations.

Verification remained layout-only with no Controller, profile, hooks, downloads or permission prompts. The final capture left foreground unchanged. Native NSView/theme-frame images still do not prove WindowServer compositing or external-host typing. The full activating Mac shell smoke remains CI-only.

Signed source `b8d1bc11f5b2295571e391171708541578624331` was the alignment-only local installation, now superseded by the spacing follow-up below. Its [historical installation evidence](../../evidence/rust-migration/macos-settings-alignment-install.json) and private app/profile rollback remain retained.

## 11. Settings reading measure and spacing

The user's next screenshot rejected the remaining density, despite aligned edges. Shared rows now keep titles and descriptions in one bounded reading column, reserve separate control space and center controls against the complete text block. Vertical row padding, title/description spacing and heading/section gaps have a consistent rhythm. Narrow forms stack controls beneath the text; full-width editors, tables and action bars keep their allocation and identity.

Visual inspection also exposed clipped wrapping: an actual AppKit text-cell probe required three lines where attributed-string bounds and intrinsic sizing reported two. `text_height` now uses `NSCell.cellSize(forBounds:)`, the native minimum display-size API, rather than attributed-string bounds. The render probe checks actual text-cell height against retained row-label frames, plus text/control separation, text-block centering, row padding and section spacing. Heading-only sections still introduce custom content without an extra empty-group gap. Existing placeholder-width normalization is preserved, avoiding accidental group-wide stacking.

[Spacing evidence](../../evidence/rust-migration/macos-settings-spacing/receipt.json) passed with 41 native images and 63 geometry observations: twelve baseline panes; General/Setup/Models/Context/Personalization at minimum and expanded sizes; each representative pane's final content/control reachable at all three sizes; and the same Unicode editor/text/UTF-16 selection retained through four resize observations. Native General/Models/Context/narrow General, expanded Setup, final General controls and retained editor renders were inspected. All 396 workspace tests plus two nested child invocations passed. Final capture left foreground unchanged and never created its isolated profile directory. These remain native-view images, not WindowServer/TCC/external-host typing proof.

Signed spacing source `315d125c2a9bf36143af63208afd6c1ab585f864` is installed and running at `/Applications/BroTypist.app`. [Installation evidence](../../evidence/rust-migration/macos-settings-spacing-install.json) records exact candidate/installed inventory, preserved original signing requirement, graceful prior-app exit, a fresh locked opaque profile backup, atomic exchange, normal-profile Rust/AppKit doctor with selected model present and completed inactive startup. Settings and history bytes remained unchanged; foreground stayed unchanged. All previous rollback copies remain retained. Main and published releases are unchanged.

## 12. Sidebar selection craft and pending icon handoff

The selected sidebar row must contain its icon with visible inset padding, and the category icons need more visual weight. Rows retain a full native button target while placing a larger tile inside a transparent padded image canvas. One opaque rounded `NSBox` supplies selection; native button-cell alternate/highlight fills are disabled without removing logical state or actions. A dynamic semantic fill is retained directly so switching from dark to light does not freeze the dark selection color. Actual selection movement, rather than only a native push-on/push-off button's already-changed state, drives emphasis and reveal; ordinary status refreshes do not force sidebar scrolling. Rule details keep App Settings selected.

[Sidebar evidence](../../evidence/rust-migration/macos-sidebar-craft/receipt.json) contains 43 native images and 67 geometry observations, including selected About in dark/light and narrow-height cases. The probe measures the actual image bitmap ink and native cell drawing rectangles: larger tiles, inset padding, title separation, one selection background and visible selected rows all passed. Images are retained through refreshes; all thirteen native destinations remain present. Native view images were visually inspected, workspace tests passed 396 plus two child invocations, foreground stayed unchanged and the isolated profile directory was never created. The probe now installs the production source-launch classic icon fallback, so About does not display a generic example-process icon. These are not compositor/external-host/TCC claims.

After the fourth rejected visual attempt, the user approved task-scoped reference use of [macos-design-guidelines](https://github.com/ehmo/platform-design-skills/tree/dc2be825d8b439caea78e9eaa8fb3ac23b0ff3e9/skills/macos), pinned at `dc2be825d8b439caea78e9eaa8fb3ac23b0ff3e9` (SKILL.md SHA-256 `9fa44a38eee0138d6aaf41b7f34223bcd9b25099e4706846b8773124fc5b6ecc`). Static review found MIT-licensed text/metadata with no bundled executable hooks. Only relevant sidebar/visual guidance is used; broad pack mandates do not expand the utility's scope. No persistent skill installation or behavior-quality guarantee is claimed.

The handoff was initially blocked by macOS Downloads permissions. The user subsequently supplied the accessible copy at `/Users/renanrocha/tmp/brotypist-handoff.zip` (SHA-256 `b0b7ffcad8f363024fa9347e306c484d0eef7d5730ee1eba5b4c11bd6d678211`). No permission bypass was used.

Icon cutover must cover both `design/icons/BroTypist.icon` (modern macOS `Assets.car`) and committed `crates/brotypist/assets/app.icns` (classic/source-launch Mac), `app.ico` (Windows embedded and sidecar), `app-24.png`/`app-24@2x.png` (Linux runtime/Wayland), and all nine `assets/icons/app-{size}.png` desktop exports. Existing tooling compiles Composer and copies these exports; it does not regenerate the committed ICNS/ICO/PNG files. About/Welcome use the actual application icon. Category tiles, native monochrome Mac menu/accessory marks and text wordmark are separate, not arbitrary app-icon replacement targets.

## 13. Supplied Typing Buddy artwork

The supplied default/dark/foreground/16/32 SVGs replace the old app artwork. Composer uses separate full-bleed background, eyes and smile layers with the system mask and default/dark/tinted variants. Native `sips` exports the supplied simplified small artwork through 64px and full artwork at larger sizes; `iconutil` creates the classic multi-resolution ICNS. The Windows ICO contains seven PNG-backed entries through 256px; all nine Linux desktop sizes and both runtime PNGs are replaced. No inference, permission, identity, wordmark or menu behavior was imported from the handoff documentation.

[Combined native proof](../../evidence/rust-migration/macos-sidebar-branding/receipt.json) passed 43 captures and 67 geometry observations, with actual supplied classic artwork visible in About and the corrected sidebar. Workspace tests passed 396 plus two child invocations. The capture never activated its application and did not create its isolated profile; the external before/after foreground PID differed, so unchanged foreground is not claimed for that run. Adaptive packaged artwork and signed installation are verified separately below.

Signed combined source `ebb1ba0e1437d376f15e77157c141bcb2f22b9b5` is installed at `/Applications/BroTypist.app`. [Installation proof](../../evidence/rust-migration/macos-sidebar-branding-install.json) records identical candidate/installed inventory, preserved original signing requirement, graceful quit, locked opaque backup, atomic replacement and successful inactive startup with unchanged foreground and settings/history bytes. The selected model remains present. The packaged adaptive icon was rendered through native Icon Services and visually inspected; classic artwork was inspected in About. All older rollback copies remain retained. This supersedes the earlier sidebar/icon installation blocker; no release was published.

## 14. Prediction failure diagnosis and menu face

The user reported the main macOS prediction feature not working and the menu glyph remaining old. Native render/startup proof was not prediction proof. Supplied menu face variants now replace the independent caret/pill path, with seven states and explicit template 1x/2x representations; actual ink was visually inspected against a contrasting background after correcting a bitmap-coordinate transform and typed CGContext return encoding.

Python parity review found a concrete marked-text compatibility defect: a typed zero-length AXMarkedTextRange with kCFNotFound location is inactive composition, not a valid editable offset. Rust now accepts that state while keeping selection/mutation ranges strict, with a native AXValue regression. Content-free capture and geometry rejection stages surface through the existing GUI Setup nativeStatus label. This defect has not been proven to explain the user's host failure; no admission/render safety gate is broadly relaxed.

Signed source `b42bab21695b4e9b7a81235d9330b1b1f1025f43` is installed with preserved original requirement, exact candidate/installed inventory, locked opaque rollback and unchanged settings/history bytes. [Installation boundaries](../../evidence/rust-migration/macos-prediction-diagnostic-install.json); [native glyph proof](../../evidence/rust-migration/macos-prediction-menu/receipt.json). Integrated workspace tests passed 397 plus two child invocations.

The user authorized a new unsaved TextEdit document with synthetic text only. Observer Accessibility is unavailable; the attempt to create only that synthetic document through TextEdit scripting failed `-1743` Automation denied. No permission grant, TCC reset, personal content, clipboard access or full activating CI smoke was attempted. External-host prediction/acceptance is therefore still unverified. Next discriminating evidence: after typing `The weather today is ` in a new TextEdit document, open BroTypist Setup and read `Native input`, `Keyboard` and `Last ordinary field capture`. That is the actual GUI's status, unlike the CLI doctor.

## 15. Inline accessory versus WhatsApp prediction admission

The user's WhatsApp image identifies a third artwork path: the floating caret accessory, independent of the application and status-item icons. Its retained 1x/2x PNGs now use the supplied face. Placement prefers the actual caret instead of distant field-left geometry, with a two-point above-left gap; passive actions and screen-bound refusal remain intact. Actual raster ink was inspected and the integrated suite passed 399 workspace tests plus two nested child invocations, including caret-gap, four screen-edge boundaries and negative-origin screen cases.

WhatsApp predictions remain unresolved, not fixed by icon work. Source comparison confirms that accessory caret events require only `has_anchor` (caret or field rectangle), while ordinary scheduling/submission independently require `inline_safe` font/baseline/clipping/suffix evidence. Therefore accessory visibility cannot establish prediction readiness. Comparison with the immutable Python ordinary geometry/typography path found no additional proven WhatsApp bug warranting a gate relaxation. The exact next discriminator remains the actual signed GUI's content-free Setup line after typing in the failing composer: Native input, Keyboard, Last ordinary field capture. No private chat image/content is needed, and no new export or permission bypass was added.

Signed accessory source `f240739b7c774fcde7970fe0072a6b757bc6c62d` is installed. [Installation evidence](../../evidence/rust-migration/macos-caret-accessory-install.json) records exact candidate/installed inventory, original identity, locked opaque backup, atomic replacement, eight-second healthy startup and unchanged settings/history bytes. This delivers the requested new inline face and closer caret anchor, not a claim that WhatsApp predictions work. The content-free actual GUI capture stage remains required.

## 16. Working Discord narrows WhatsApp failure; diagnostic UI correction

The user confirms Discord predictions work while WhatsApp does not. Safe settings inspection found both prediction switches enabled, no WhatsApp exclusion and only recording consent in its app rule. No global input/model rewrite is justified. Native application window/PID ownership checks differ from Python but are not proven to reject WhatsApp without the actual capture stage.

A concrete diagnostic UI defect was found: runtime `native_status` is an object, but Setup's generic string helper returned empty. Setup now reads the object's message (retaining existing string payload handling). [Actual owned-label rendering proof](../../evidence/rust-migration/macos-gui-status/receipt.json) checks native readback and raster ink after prepare/reflow at all three sizes. The fixture's `attributed_text_unverified` is synthetic, not a WhatsApp observation. All 399 workspace tests plus two child invocations passed. This corrects the missing discriminator display, not the unproven WhatsApp prediction cause.

Signed source `387d2e687d1b6cc3d3762dc95def0bf9b4ff0a24` is installed with the corrected visible Setup diagnostic. [Installation proof](../../evidence/rust-migration/macos-gui-status-install.json) records original signing requirement, matching candidate/installed inventory, locked opaque rollback, atomic replacement, healthy startup and unchanged settings/history. The user-confirmed Discord success narrows investigation; the actual WhatsApp stage is still required. No prediction gate or permission was changed by this diagnostic-display correction.

## 17. Observed WhatsApp focused-window discovery failure

The user supplied actual GUI evidence: `Native input: native_available. Keyboard: Quartz shortcuts enabled. Last ordinary field capture: focused_window_unavailable.` This identifies the first failing gate before text capture/inference. Rust previously attempted compatibility accessibility only after constructing a session, but session construction already required AXFocusedWindow. Preparation now occurs first for foreground/security-authorized and policy-allowed applications; domain-dependent cases retain the later preparation call. Compatibility caching prevents repeated attribute work. Window/process identity, field ownership, protected-field and rendering checks remain unchanged. Integrated tests passed 399 plus two child invocations. Real WhatsApp success still requires post-install user observation; the ordering defect is corrected, not a claimed end-to-end acceptance pass.

Signed repair source `6e44d96710ea653152b11549b16eb942ce01f281` is installed. [Receipt](../../evidence/rust-migration/macos-window-order-install.json) records actual user-observed rejection, unchanged original identity/settings/history, locked opaque rollback, atomic replacement and healthy startup. The user can now retry WhatsApp and report success or the next actual Setup capture stage; external-host prediction is not marked passed from compilation/startup alone.

## 18. Windows build and independently verified portable runtime

At the user's request to complete Windows while WhatsApp testing is paused, native [run 41236356500](https://github.com/renan-brasilio/brotypist/actions/runs/41236356500) rebuilt the x86_64 MSI and ZIP. Package source is the authenticated PR merge `1e105adcba63b79637a67bc5175e1061a536e4e7`, with baseline `6eaeed5f` and branch head `1fbe9ac9` as exact parents. Native compilation/checks, 278 top-level tests plus two child invocations, real build-tree worker inference, and all 14 owned desktop checks passed.

The canonical ZIP was relocated to a Unicode directory with its old location absent and build-tree DLL paths removed. Its own CLI/doctor, real checksum-verified Tiny worker/public completion, and visible Settings/real Quit passed. Synthetic data and autostart remained unchanged. [Build/package proof](../../evidence/rust-migration/windows-build-completion.json), [canonical portable worker proof](../../evidence/rust-migration/windows-portable-worker.json), [native desktop proof](../../evidence/rust-migration/windows-native-desktop.json), and [full portable/installer observations](../../evidence/rust-migration/windows-portable-installer-observations.json) preserve the distinction between portable success and installed-runtime failure. Both Actions archive digests, artifact hashes and all 389 ZIP payload members were independently verified after download.

The unsigned portable candidate is delivered locally under `~/Downloads/BroTypist-Windows-Rust-0.4.13/`, with SHA256SUMS and build-verification.json. Extract the entire ZIP and retain its DLL/resource files beside BroTypist.exe. ZIP SHA-256: `616e45175f8291810f341d98bcfe80c6563aabe40649016bc5b7c151446bf684`.

**The Windows installer is not complete or approved for installation.** MSI still returns successful fresh install and exclusive per-user unmanaged context 2/State 5/AssignmentType 0, but creates a machine ARP64 registration. The gate remains enforced; repair and installed-runtime checks are not reached. Uninstall and synthetic-data continuity pass during failure cleanup. This resembles upstream [WiX issue 9323](https://github.com/wixtoolset/issues/issues/9323), not a proven upstream fix. ARPSYSTEMCOMPONENT merely changes display and is not a demonstrated way to prevent the machine key; suppressing native registration actions would remove repair/uninstall contracts ([Microsoft](https://learn.microsoft.com/en-us/windows/win32/msi/configuring-add-remove-programs-with-windows-installer)). Neither workaround was adopted. External-app physical typing, prior-version upgrade, transaction rollback, signing and publication remain unverified/held. Portable execution proof does not close those gates.

## 19. macOS prediction correctness, context relevance and emoji

The next user report supersedes the earlier inference that Discord/Telegram prediction was working well: suggestions appeared, but `Então` repeatedly produced generic meeting wording; WhatsApp remained silent and emoji seemed unavailable. The approved GUI-owned diagnostic recorded **205 WhatsApp `inline_ready` captures**, rather than a continuing focused-window failure. Its later background AXGroup is not a capture-failure diagnosis.

Integrated corrections:

- Retire and debounce revoked in-flight work after unchanged-draft AX notifications or transient geometry loss. Previously `attempted` could remain latched after cancellation, suppressing subsequent automatic prediction. Dismissal and model-error attempts stay suppressed. This is a demonstrated lifecycle defect, not yet proof of the exact live WhatsApp rejection.
- Recheck selected range, caret bounds and field/window frames at the end of capture, including selection after the geometry read. Preserve strict identity, protected-field and same-line glyph gates.
- The lexical reference selector previously discarded focused-window facts for a draft like `Então` without a shared word. Single-word drafts can now use already-consented AX/OCR facts only when no positive matches exist. Clipboard still requires positive relevance; longer topic-bearing drafts do not acquire arbitrary zero-score screen facts. Normal consent settings, collection scope and selected model remain unchanged.
- Native emoji row 1 incorrectly emitted choice index 1; sole emoticon choices were therefore rejected. Row clicks now use the same zero-based indices as keyboard actions. Hidden/rebuilt targets revoke pointer pairs by generation; unknown metadata, other host input and native mutation gates still invalidate normally. CGColor native ABI types are corrected on overlay drawing paths.
- Runtime exports are explicit atomic mode-0600 writes, not continuous logs. `runtime-whatsapp.json` retains only typed stage counters/eligibility/worker/lease/presentation flags for WhatsApp across foreground switches, including whether references actually entered the submitted request. No text, clipboard, domains, snapshot keys or arbitrary error messages are exported. Native shape diagnostics no longer retrieve AXValue.

[Verification](../../evidence/rust-migration/macos-prediction-repair/verification.json): **416 workspace tests plus two child invocations passed**. New short-draft context and revoked-chooser generation regressions were observed failing before their fixes. Actual nonactivating AppKit [emoji proof](../../evidence/rust-migration/macos-prediction-repair/native-emoji.json) covers alias, sole emoticon and actual last row: full-row acknowledgment, real NSButton target/action, exact owned NSTextView replacement/readback, preserved key window and hidden-row inactivity. This is not physical global-click or external-host insertion proof.

Emoji triggers remain explicit shortcodes/emoticons, for example `:sorriso`, `:obrigado:`, `:wave` and `<3`. Plain words do not imply semantic emoji prediction. Atomic replacement must be verified; append-only Electron/Chromium controls cannot safely replace a typed shortcode. This retained limitation is not represented as a repaired feature.

[Actual synthetic model observations](../../evidence/rust-migration/macos-prediction-repair/model-quality-synthetic.json) show genuine model generation, not a canned phrase. Corrected reference assembly reached the selected Gemma model: car facts produced `, devo ir buscar o carregador agora.`, while cake/power facts produced `, precisamos verificar o termostato.` The latter adds an unjustified diagnosis; **context attachment is fixed in this scenario, semantic quality is not certified**. A stronger grounding prompt failed controlled cold-worker checks and encouraged unrelated-topic leakage, so it was rejected. An already-cached Qwen3 comparison was also imperfect; the normal model was not switched. No guarantee of useful contextual quality, live WhatsApp success, physical chooser clicks or general external-host emoji acceptance follows from these tests.

Signed source `59f9621880d17964517acf40d0e580e16426ff5a` is now installed with matching original designated requirement, verified inventory/signature, locked opaque rollback, atomic replacement, unchanged settings/history and healthy startup. [Installation proof](../../evidence/rust-migration/macos-prediction-repair/installation.json) includes actual one-shot export from the GUI to mode-0600 `runtime-whatsapp.json`. Its initial `observed=false` means WhatsApp has not been exercised since this replacement—not that permissions or consent are disabled. Exact external behavior remains a user-owned acceptance gate.

An additional [already-cached Qwen2.5 7B comparison](../../evidence/rust-migration/macos-prediction-repair/cached-qwen7b-comparison.json) produced on-topic car/cake continuations on three short synthetic inputs but invented a boss recipient in the illness case. It is a possible heavier-model tradeoff, not a validated replacement or automatic selection change. No new weights were downloaded.

## 20. Actual WhatsApp empty-render acknowledgments and fractional ghost allocation

The user's next failure report supplied a decisive [GUI-owned runtime observation](../../evidence/rust-migration/macos-ghost-placement/runtime-before.json) on signed `59f9621`: 263 captured/inline-safe fields, six submitted requests, four proceeded requests and 19 model results/offers. All 19 native presentation acknowledgments were current but empty; none rendered. Worker/submission errors were zero. This places the current failure at native presentation rather than capture or model execution. Context was not attached to the last request despite tickets/consents; its exact collector outcome was not recorded in that source.

The new smoke exercises the actual public OverlayOwner and GhostSurface, not the separate Welcome preview. It reproduced an inline-ready refusal at caret `[100.125,133.875,1,15.310546875]`: AppKit changed requested window `[100,967.5,174,16]` to `[100,967,174,16]`. Origin compensation then required 16.125 points of content in a 16-point frame and correctly refused to acknowledge clipped writing. The frame now rounds outward in **logical points**, while drawing offsets preserve the exact original glyph origin and baseline. CGContext state explicitly clips to the original verified footprint so extra transparent window allocation cannot overpaint. Font, host clip, screen and actual-frame readback checks remain enforced. Actual GhostSurface drawing also exposed a debug Objective-C CGContext encoding mismatch; its typed boundary is corrected.

[Native proof](../../evidence/rust-migration/macos-ghost-placement/native-ghost-proof.json): the fractional case failed before the allocation correction; all 32 system/named-font and fractional-coordinate cases now have actual visible acknowledgments and rasterized glyph ink with exact global origin/baseline. All 32 deliberately too-narrow inline-ready cases still reject. Independent alpha checks found no painted pixels wholly outside the original footprint in any of the 32 PNGs. Workspace tests passed **421 plus two child invocations**. This is first-screen/current-scale owned-window proof, not a mixed-display or post-install WhatsApp claim.

Explicit one-shot `renderer-whatsapp.json` adds 26 fixed rejection counters and bounded technical font/geometry metadata, without writing, suffix contents, raw events, keys, window titles or domains. Unobserved visibility/geometry remains optional. Submission-time runtime fields now distinguish collector timeout, token mismatch, fixed read statuses, absent/filtered references and expiry; later UI readiness cannot relabel the submitted request. No new waits, permission changes or context budgets were introduced. These discriminators remain necessary if the real WhatsApp refusal has another cause.

Signed source `d1dc04518e657d505926b52019fc151549626080` is installed. [Installation proof](../../evidence/rust-migration/macos-ghost-placement/installation.json) records original identity, matching inventory/signatures, locked opaque rollback, atomic replacement, unchanged settings/history and healthy startup. The explicit notification produced both renderer/runtime files from the actual GUI at mode 0600, including the new context outcomes. Both initial exports were unobserved because WhatsApp had not yet been exercised after replacement. The user subsequently confirmed **“Works now”** before requesting installation on gamer-pc and release preparation. This closes the user-owned WhatsApp prediction-visibility gate for that installed candidate; it is not a new automated external-host capture, semantic-quality certification or a diagnosis of the prior request's exact context-collection failure.

## 21. gamer-pc native preview installation

The requested PC is CachyOS x86_64 with KDE Wayland, not Windows. [Installation evidence](../../evidence/rust-migration/gamer-pc-native-install.json) binds its native 0.4.13 portable to CI run `41247499588`, authenticated merge source `3a47450006b9ec09bffab38ca23465441c38e502` and exact baseline/branch parents. Both Actions archives, both package hashes and all 535 portable runtime files were independently verified; only the three source-documented desktop registrations are omitted from the portable archive. Native Linux tests, real inference, desktop and core DEB/portable installer checks passed. The full installer matrix remains open.

The previous app's actual PID-owned tray Quit completed without termination signals. Its newer saved startup/setup preferences and KDE input-method choice differed from the initial snapshot; the guarded installer preserved the latest state rather than restoring the older snapshot. The original payload, opaque profile/launcher snapshots and exact portable remain in a private per-user rollback directory. The installed executable's hash, latest settings, login launcher and cached Gemma bytes were verified after startup. An actual PID-owned, responsive `BroTypist Settings` GTK frame was observed at 960×700 on the desktop; compositor pixels were not captured. The staged production CLI generated `a festa.` from synthetic cake text using the existing Gemma model in an isolated profile. No model, consent or input-method selection was changed by installation.

This historical 0.4.13 installation is superseded by the actual 0.5.0 deployment in section 23. **Ordinary Wayland inline predictions remain unavailable**; model preview and explicitly selected experimental input-method controls are not general inline support. PC acceptance is pending, not inferred from process or inference success.

## 22. Branded README and conditional release preparation

The main README now uses the supplied Typing Buddy identity with theme-aware wordmark assets, an actual owned native Settings capture, installation/usage guidance and explicit platform/privacy/model-quality limits. Relative links, self-contained SVGs and GitHub's actual GFM rendering were checked.

The user subsequently explicitly approved a **macOS-only 0.5.0 prerelease**, rather than waiting to publish for PC acceptance. Rust tooling supports clean native Mac/Linux package sealing and externally signed assembly, but holds Windows and has no per-target preview OTA channel. Linux remains a separately tested private preview, not a published prerelease asset or Latest OTA target. Final Mac source-bound rebuild, package verification and pinned-publisher metadata signing are now complete as recorded below; publication remains pending. No signing secret was exported or new permission granted. Stable `v0.4.13` is unchanged.

## 23. Actual 0.5.0 deployment and approved Mac-only prerelease boundary

### gamer-pc: canonical Linux package, native lifecycle and real worker

The [sanitized 0.5.0 deployment receipt](../../evidence/rust-migration/gamer-pc-0.5.0-deployment.json) preserves the content-free verified receipt's substantive observations and all canonical source/artifact/package identities. Its original receipt SHA-256 is `6d9943cbd188490b99960746294a428606ba7e9c5f443a4e5da387ca7143c7b8`; absolute user/recovery paths are replaced with `$HOME`-relative paths and private recovery placeholders. That original digest identifies the private input receipt, not the sanitized copy.

CI [run 41288799403](https://github.com/renan-brasilio/brotypist/actions/runs/41288799403), artifact `13077480393`, supplied the installed canonical Linux x86_64 0.5.0 portable. Its source is **PR synthetic merge `a7aec67228d1204ddaf1dce208eac47f837906ee`**, with parents `c307260084039e6a0c1d6ae4f96aaeb57bf72eee` and PR head `2f6e992f3571a9d028753c756dad18a1f2a8712e`; it is **not** the Mac head/build source. Archive SHA-256 is `6b2479fb89d5c341354f049b3131212e2cbee714a33f26e1b4270b58d4c118ac`, portable SHA-256 is `e85bcb144a065d4577b418a1f3af8a638a34fbabfad45a41739ac0e9b9e9ed32`, and all 535 payload files are bound by the retained manifest and binary hashes.

Actual GTK/AT-SPI automation observed the owned Settings frame, clicked its real Close button, verified that the same PID stayed resident with zero visible owned frames, reopened Settings through the tray, and clicked the real Quit BroTypist button with PID exit confirmed. The canonical installed `_worker`, using existing Gemma weights without download and an isolated inference profile, emitted loading/ready/streamed-result/done events, produced 55 result bytes in 3.948 seconds, and stopped with exit 0 and no surviving worker. The separate successful public CLI completion uses the in-process provider; it is not a substitute for that child-worker proof. No personal writing was read or used for these observations.

The final normal service is `brotypist-native-private-050-normal.service`, PID `3107248`, generation `6871792`, with the original `--settings --pane general` arguments restored. A temporary paused smoke process quit cleanly; **no runtime pause override or saved pause change remains**. The final launch was observed through its owned executable, unit MainPID and arguments, not a fresh final-frame/typing smoke. Normal profile bytes, launchers, symlink, model/cache, consent/login settings and `kwinrc` were preserved; rollback and earlier recovery remain private. The receipt's earlier doctor snapshot (`global_enabled=false`) is not the final saved-settings state: the last preservation observation was enabled, not reread or altered during the final launch correction.

KWin reports virtual keyboard **available=true, active=false**. This user-service invocation has no compositor private socket and correctly refuses input-method transport. No socket was forged; input-method selection, compositor/session environment and policy were unchanged, with no reselect or compositor restart and no new autostart enabled. **Ordinary KDE inline/preedit remains disabled under baseline P25.** Manual completion/typing and personal desktop acceptance were not exercised. An unsigned private Linux deployment plus GTK/worker success is neither a full installer/upgrade/rollback matrix nor Linux publication or user-typing acceptance.

### Mac source-bound rendering, context and model-quality limits

An actual owned synthetic-window check on Mac source `2f6e992f3571a9d028753c756dad18a1f2a8712e` observed `Então, qual o próximo` (generic) and `Vou buscar o carregador agora.` with a visible car/charger reference. The [C0 native receipt and pixels](../../evidence/rust-migration/macos-prediction-repair/native-context-render.json) remain bound to that earlier source. Rendering code is unchanged in final `52604fc`, but **no final-source pixel claim is made**. This is supplied-context native rendering proof, not universal semantic quality, personal-message acceptance or a reconstructed WhatsApp request. The runtime context-prefetch cancellation defect has been fixed; the exact historical WhatsApp request cannot be reconstructed. The earlier user-confirmed visibility success remains source/scenario-bound, not a blanket acceptance of later model output.

Gemma remains selected. Actual quality comparisons rejected the prompt rewrite and larger Qwen replacement rather than silently changing model selection. Generic or unjustified completions remain possible. C0 on `2f6e992` is superseded by the final updater deadline-fix build below; Linux's synthetic-merge identity must not be reused as that Mac identity.

### Final Mac 52604fc: signed packages, actual updater transactions and local promotion

The [sanitized final Mac proof receipt](../../evidence/rust-migration/macos-0.5.0-release-verification.json) binds source **`52604fc3ff1a82cf36a04223cb80ee1f12997893`** to the sealed native build and release manifests, final DMG/ZIP identities and four actual updater scenario receipts. Private-input SHA-256 values identify the exact underlying technical receipts, not the sanitized summary. It omits personal content, normal-profile contents/database hashes, absolute personal/recovery paths, signing-email identity and ephemeral process IDs.

The final Mac integration source passed **429 workspace tests** and final native worker/GUI smoke. Its actual DMG/ZIP package smoke is **`passed=true`, `full_matrix=false`**: read-only DMG mount/layout and canonical payload, disposable drag-copy, bundled CLI/doctor, real Tiny worker, nonactivating LaunchServices background lifetime, normal OS Quit with exit 0, ZIP extraction/identity/traversal refusal and fixture-data-preserving removal passed. That package smoke did not access the normal profile, assert physical menu/keyboard/typing or visible-window acceptance, or itself exercise a genuine prior-version upgrade. The application is **Apple Development-signed, not notarized**; no OS protection bypass is authorized.

| Final Mac asset | SHA-256 | Bytes |
| --- | --- | --- |
| `BroTypist-0.5.0-macos-arm64.dmg` | `23d9ea56e261ee202818ed848d40a08ee3adee42683fe8915909ec97b4e9b47c` | 18,800,132 |
| `BroTypist-0.5.0-macos-arm64.zip` | `86d0df8d4b3eed2102fda27003cf3d9a4c45b87abd366ff6021bcefaced88811` | 18,012,876 |

Separate actual owned GUI updater transactions verified the existing pinned-publisher signature and these final signed bytes. Both healthy paths used **genuine previous helpers**, actual retiring-parent exit and real first-GUI-loop acknowledgment: published Python `v0.4.13` helper/source `6eaeed5fe56e1ea8dcf878b1b7498f922c26fd59` (including its exact legacy three-field ACK), and the previously installed native helper/source `d1dc04518e657d505926b52019fc151549626080`. Both ended `healthy` with helper exit 0, complete previous signed build retained and synthetic data preserved.

The two negative scenarios used the **current `52604fc` helper component fixture with a genuine old destination and parent**, not the old installed helper. A real enrolled replacement suspended before GUI ACK exercised delayed cancellation, owned-process resume/exit, restored complete previous build and confirmed `rolled_back` (helper exit 1). A held ownership lock exercised bounded **`rollback_pending`**, failed closed without swapping while ownership was unresolved, retained both complete signed builds, and the resumed cancelled process exited without a late ACK (helper exit 1). Those expected negative helper exits are successful scenario observations, not successful startup. No fake ACK, changed timeout, normal-profile use or permission/TCC grant was involved. They do **not** close an old-installed-helper negative full matrix or the cross-platform installer matrix.

Final `52604fc` is now locally installed and its normal application was launched. Integration-owner preservation observations confirm unchanged settings and `local.sqlite3` hashes, with the exact prior C0 whole signed application and opaque-profile rollback retained privately. Only preservation booleans and rollback presence are published here; no normal-profile content, raw database hash or private recovery path is exposed. This local promotion is not publication, final-source pixel proof or expanded personal-message acceptance.

### Windows and publication remain bounded

Native CI [41297308730](https://github.com/renan-brasilio/brotypist/actions/runs/41297308730) passed Mac (17m40s) and Linux (27m29s); Windows failed after 23m9s only at the guarded installer gate. This CI observation is separate from the final Mac package's exact `52604fc` manifest binding. Windows artifact-reuse [41297558244](https://github.com/renan-brasilio/brotypist/actions/runs/41297558244) resolved the disposable fixture's **1625** and passed actual policy restoration/quiescence, but **the strict real HKLM64 ARP ownership gate still failed**. The approved fixture sets `DisableMSI=0` only when absent and transactionally restores it; existing policy and production settings remain unchanged. The direct-no-shell child-token fix is read-only-review-cleared but uncommitted at this observation, with new native verification pending—not passed. Earlier portable/runtime checks do not establish current installed-runtime, repair, upgrade or rollback acceptance.

The subsequent [direct-client probe `41301762511`](https://github.com/renan-brasilio/brotypist/actions/runs/41301762511), committed source `2f189c3`, is recorded in the [sanitized ownership receipt](../../evidence/rust-migration/windows-direct-client-ownership.json). It authenticated the newer canonical 0.5.0 package from run `41297308730` (synthetic package source `db3c5df3bf10d50a03b0e1df57365abc6c2f2605`). The actual directly launched msiexec primary token independently passed medium integrity, same user, non-System, Administrator-disabled and restricted-rights assertions, while retaining elevation metadata 1/default type 1. Native user context 2/assignment 0 passed, but a matching **HKLM64 Uninstall entry still existed**, so the strict ownership gate correctly failed. Normal uninstall removed the installed product's ARP entry; both family products were absent in all views after cleanup. Synthetic settings/model/history fixtures remained unchanged, owned processes/MSI transactions were quiescent, and the absent-only policy fixture restored its initial state. Shell brokerage is therefore not a sufficient explanation for this runner. A genuine standard-user or UAC-filtered logon is the next discriminator; creating a test account or using alternate credentials is outside the current fixture approval and requires explicit authorization. No ownership gate was weakened and no genuine standard-user/upgrade/rollback pass is claimed.

**Explicit user disposition:** keep Windows held. No new CI account, alternate credentials, additional security changes or Windows MSI publication are authorized. The genuine-user ownership/upgrade/rollback gate remains intentionally unaccepted; the Mac prerelease delivery is complete and this decision does not turn the failed Windows check into a pass.

The approved **[Mac-only prerelease is published](https://github.com/renan-brasilio/brotypist/releases/tag/v0.5.0)** as release `430525839`, with `draft=false`, `prerelease=true` and the immutable `v0.5.0` tag resolving to Mac source `52604fc`. After CLI creation was denied with 403, the existing authenticated browser workflow uploaded exactly six authorized Mac assets without changing credentials or permissions. The integration owner independently downloaded all six server responses both before and after publication, verified every size/SHA-256 and the exact SHA256SUMS membership, and verified the downloaded update manifest/signature against the existing publisher pin. Post-publication API inspection confirmed the same uploaded asset identities and stable **`v0.4.13` still Latest** (`427862094`). Windows/Linux publication and acceptance remain held; Mac model-quality, notarization and external personal-message limits stay explicit. These bounded passes do not complete the full migration matrix.

## 24. macOS 0.5.1 responsiveness, Monaco support and clean Setup

Signed **0.5.1**, payload source **`751ee74e2275d81827412d8508cdb09d5f039f1f`**, was installed locally with the existing designated requirement. The [sanitized correction receipt](../../evidence/rust-migration/macos-0.5.1-responsiveness.json) binds that regression run, actual editor observations, measured desktop response and preservation checks. These are **earlier local correction observations, not final stable-release proof**; existing `v0.5.0` assets and tag were not changed. The intended 0.5.2 stable release additionally retains selected primary and battery catalog models in durable app data, verifies missing old-default weights before retention and preserves originals/custom paths. Its final source, package identities and publication evidence must be supplied by the release owner.

### Runtime and native corrections

- Preserve the exact unchanged-draft spelling verdict across automatic submission instead of discarding it and scheduling another assessment.
- Start the next routine capture interval after the previous native capture completes. Slow AX reads no longer make that deadline immediately overdue; genuine input and forced captures remain fresh.
- Drop the encrypted learning-store lock before native post-authorization. A blocked native check must not retain the mutex needed by history/statistics work. The regression uses a real temporary encrypted store and preserves revoked/latest-only learning refusal.
- A benign native notification can disarm a completed visible ghost. Reauthorize an unchanged draft only after fresh capture and a new version-bound presentation acknowledgment, without another model submission. Presentation versions remain distinct from actual user-input revisions; an old queued acknowledgment cannot authorize a newer same-text offer.
- Negotiate settable false editor accessibility capabilities once per PID. For the classic Monaco textarea relay, verify its native paint viewport, ancestry, owner/window, glyphs and containment; do not invent geometry or relax protected-field/draft/authority checks.
- Remove the raw native-input/Quartz/capture sentence from normal Setup. Structured permission rows, actions and explicit diagnostics remain. The obsolete wording/raster test was removed rather than repinned. The [native Setup render](../../evidence/rust-migration/macos-0.5.1-setup.png) covers the former row area; baseline, compact and resized AppKit renders passed without starting the controller, profile or hooks.

`cargo +1.99.1 test --locked --workspace` passed **441 workspace tests plus two additional child-process invocations**, with no failures. Formatting passed. These checks precede the source freeze; later repository additions are documentation and sanitized evidence, not a different installed payload.

### Actual VS Code prediction, Tab and Save

Installed VS Code **1.144.0 / Electron 43.8.3** exposes unusable zero caret geometry through its default EditContext backend. `editor.accessibilitySupport: "on"` alone does not repair that geometry. Together with `editor.editContext: false`, the classic textarea exposes verifiable native caret/glyph bounds.

Two consecutive fresh disposable-profile runs observed the exact BroTypist-owned [prediction pixels](../../evidence/rust-migration/macos-0.5.1-code-prediction.png), posted session-tap Tab, read back insertion of `the `, and invoked the real File → Save action. The saved synthetic file matched the native readback. The [owned editor after Tab](../../evidence/rust-migration/macos-0.5.1-code-after-tab.png) was captured before Save. The final probe observed both original events at session head and the service's tagged Unicode emission; this is not physical-keyboard proof.

An initial final-source attempt produced no edit and remains unattributed. Its tail-only missing-down observation did not identify a consuming process; no speculative event-routing workaround, native-guard relaxation or new permission grant was added.

The isolated profiles disabled AI/inline suggestions and extensions, so the visible text was not Copilot. Normal user-level Copilot automatic completion was already disabled; workspace overrides and live Copilot/Cursor-Tab coexistence were not exercised. After end-to-end success, the user's conditional approval was applied: only the two Code compatibility preferences and BroTypist's Code-specific enabled/code-editor rule changed. All other Code preferences, including Copilot choices, were semantically unchanged. Code projects and normal extension state were not opened or modified.

### Final desktop timing

Owned AppKit text edits exercised real native capture, the existing cached Gemma 4 E2B model and the service's visible prediction panel on an M1 Pro / 16 GiB machine. Screen and screenshot context were enabled, clipboard/history/personalization disabled only in the private fixture, with 150 ms debounce. Normal model, context and learning preferences were not reduced.

| Phase | Last local edit → prediction panel |
| --- | ---: |
| Cold idle | 783 ms |
| Warm typing cadence 1 | 556 ms |
| Warm idle | 524 ms |
| Warm typing cadence 2 | 505 ms |

The observer sampled WindowServer at 16 ms; actual [warm prediction glyphs](../../evidence/rust-migration/macos-0.5.1-warm-prediction.png) and the separate owned host window were inspected. These are four controlled local-edit observations, **not physical typing, a general latency SLA or a statistically established speedup ratio**. Earlier cold measurements varied materially.

### Installation and remaining application boundaries

The signed app was atomically replaced, its real GUI loop and eight-second startup liveness observed, and the exact previous signed 0.5.0 app plus opaque profile retained privately. The original Code settings also have a verified private rollback copy. History database bytes remained unchanged; normal BroTypist settings differ only by the approved Code rule. This is preservation/retained rollback evidence, not a new updater downgrade matrix. The app remains Apple Development-signed, **not notarized**.

No usable Cursor bundle was present; the user chose installed Code instead of a Cursor download. The shared Monaco path is exercised, **direct Cursor support is unverified**. Code-editor completions remain a per-app opt-in; the VS Code settings above are explicit prerequisites, not silent preference changes. BroTypist does not disable Copilot or Cursor Tab; use one automatic inline provider per editor. WordCraft 0.13.2's owned document reported writable AXValue but its setter returned success without changing text; the user chose BroTypist-only changes, so no upstream WordCraft edit or unsafe document-role fallback was made. Reliable field identity, caret/glyph geometry and insertion authority remain mandatory; protected fields, active composition and unsafe/unobservable targets stay excluded. **Windows and Linux remain ALPHA; Windows MSI distribution remains held and ordinary Wayland inline completion remains unavailable.**

## 25. Native prediction latency investigation

**Source-only, not installed or published; the requested performance target is not achieved.** The [sanitized receipt](../../evidence/rust-migration/macos-prediction-latency-unreleased.json) records the changed source files, executables, engine libraries, model bytes and individual observations. Public receipts omit third-party research and are not the unchanged original evidence bundles. The available validation host is **M1 Pro, 10 logical CPUs, 16 GiB**, not the user's M5 Pro.

### Changes and preserved contracts

- Request the vocabulary output only for the final prefill chunk. Intermediate chunks still populate the KV cache.
- Keep the existing 64-token cancellation boundary, match its microbatch size, and use the pinned engine's automatic Flash Attention selection/fallback. No larger production batch was adopted: the separate 1,024-token batch experiment did not establish a reliable gain beyond observed run-to-run drift.
- Track cached-logit validity separately from cached tokens. An exact-prefix successor after cancelled intermediate prefill must replay its final token before sampling; a native decode error clears both ledgers.
- Remove repeated native authorization from the queued clipboard wait while it owns the result mutex. Cheap cancellation, shutdown and deadline checks remain in the wait; fresh authorization still surrounds the real read and result acceptance.
- Expose opt-in native benchmark counters and synchronized wall-time stages. Ordinary prediction does not enable those synchronization points. Sampling time is not called CPU time.

The actual Gemma 4 E2B Q4_0 weights, 4,096-token context, 2,000-character draft limit, medium four-word/32-token cap, sampler order/parameters/seed, prompt semantics, privacy and native geometry/freshness guards remain unchanged.

### Uninstrumented worker comparison

Three serial runs, baseline → candidate → baseline, each contain three rounds. These are request-proceed → first displayable worker text, **excluding** debounce, native context preparation, desktop supervision and rendering. Non-offers are not zero-latency successes.

| Scenario | Offers per run | Baseline A median | Candidate median | Baseline A2 median |
| --- | ---: | ---: | ---: | ---: |
| 2,000-character draft / edits | 31 / 33 | 844 ms | 685 ms | 833 ms |
| Draft plus changed reference | 6 / 6 | 2,513 ms | 2,056 ms | 2,439 ms |
| Identical reference repeat | 3 / 3 | 133 ms | 126 ms | 137 ms |
| Draft edit, stable reference | 3 / 3 | 915 ms | 789 ms | 899 ms |

The separate short fixture returned no offer in all six attempts in each run. Instrumented attribution, not used for the speedup ratio, found **1,334 ms of prefill across 14 decode calls for 880 evaluated reference-prompt tokens**, versus 63 ms of generation decode and 141 ms of sampling. Re-evaluating changed long prompts remains the main measured cost.

Six timed real-worker supersessions per build produced no stale result after cancellation. Baseline/candidate old-request drain medians were **24/32 ms**, maxima **41/52 ms**; successor first-displayable medians were **391/414 ms**. This exercises real worker cancellation, not a proven Metal abort boundary or the desktop supervisor's escalation path.

### Actual native desktop observations

User-authorized disposable signed applications and an external, owned AppKit text view exercised normal capture, context preparation, inference and suggestion panels. Only the fixture's app ID was enabled. AX screen context and screenshot appearance were enabled; clipboard, OCR context, history, personalization and statistics were disabled in the private profiles. Normal installation/profile and permissions were not changed.

Forty successful observations across baseline → candidate → baseline → candidate runs used local synthetic edits with 150 ms debounce. WindowServer was sampled at roughly 16 ms. The metric below is **last local edit → first owned suggestion panel**, not physical-key latency or an exact first-painted-pixel timestamp. Actual [baseline](../../evidence/rust-migration/macos-prediction-latency-unreleased-baseline.png) and [candidate](../../evidence/rust-migration/macos-prediction-latency-unreleased-candidate.png) suggestion glyphs were separately inspected. All owned drafts remained unchanged; final focus restoration and process cleanup were observed.

| Warm scenario | Samples per build | Baseline median | Candidate median |
| --- | ---: | ---: | ---: |
| Short English draft | 6 | 450 ms | 426 ms |
| 1,992-character English draft | 6 | 1,105 ms | 963 ms |
| Short Portuguese draft | 6 | 428 ms | 452 ms |

The first-request observations after app launch were **1,013/465 ms baseline** and **5,539/492 ms candidate**. OS/model/shader caches were not evicted, so these are not a controlled cold-start comparison. The large candidate outlier is retained, not averaged away. Neither consistently faster short-input behavior nor a cold-start improvement is established.

Initial fixture calibration was excluded: a stale observer run loop, an unpadded first text row rejected by existing geometry checks, and the observer's incorrect assumption that a floating NSPanel retains level 101. The actual AppKit level is 3. No production guard was relaxed to obtain a passing observation.

### Quality, correctness and remaining boundary

The 64-case, three-round direct corpus preserved **191/192 exact suggestion strings**, with **183/192 mechanical passes on both builds**. The same nine no-offer failures remain: three each for `language-en-locale-preference`, `grammar-fr-agreement`, and `suffix-ja-unspaced`. The one changed Portuguese fragment stays in Portuguese and does not import the unrelated reference's battery facts; assistant inspection is not calibrated human semantic evaluation or proof of contextual utility.

`cargo test --locked --workspace --all-targets --features native-smoke` passed **548 tests**, with one real-model test ignored by default and no failures. That regression was then explicitly run with the actual Gemma model in **auto and CPU modes**, both passing. It covers cancelled intermediate prefill, final-token replay, shortened/exact cache reuse, sampling and native-error reset. The release CLI/benchmark build passed. Two independent code reviews reported no actionable findings.

The measured improvements are modest: long desktop drafts improve, while changed-reference worker requests still take about two seconds and short desktop cases are mixed. These changes do **not** establish M5 Pro performance, competitor parity, general application compatibility, physical typing/acceptance latency or a release-ready performance SLA. Further work needs source-bound measurements on the M5 and a calibrated competitor workflow, with particular attention to long-prompt cache reuse and startup variance rather than reducing model quality.

## 26. Quality-preserving grammar sampling

**Local source only; not installed, pushed or released.** The [source-bound receipt](../../evidence/rust-migration/macos-quality-speed-unreleased.json) records individual outputs, native stages, worker requests, desktop insertions, binary/model/library fingerprints, rejected experiments and limits. The [implementation plan](prediction-performance-plan.md) sets the remaining quality, cache-reuse, startup and comparator gates. Measurements use the available **M1 Pro / 16 GiB**, not the user's M5 Pro.

The baseline here already includes section 25's prefill/logit/clipboard changes. These are incremental shortlist results, not a comparison against the installed 0.5.2 or published 0.5.3 application.

### Exact constrained sampling

The pinned grammar sampler checks the full vocabulary before top-k selection. The new wrapper checks a fixed 128-candidate high-logit shortlist only when it can establish the same top-k survivors. Boundary/internal ties, too few valid candidates, non-finite exceptional scores, small inputs and unsupported top-k sizes retain the original path with the original candidate buffer/order. Grammar state advances only on acceptance; clone/reset/free preserve ownership.

The wrapper uses fixed storage and in-place heap sorting, with no per-token wrapper allocation. This does not claim that the underlying native grammar or whole inference path allocates nothing. The prompt, model, context, seed policy and downstream sampling order/parameters are unchanged.

The complete 64-case, three-round direct corpus preserved **192/192 displayed outputs and all 270 native-call raw outputs, seed sequences, prompt-token counts and resolved settings**. Both builds offered 176/192 samples and passed 183/192 mechanical checks. The same nine no-offer failures remain in English locale preference, French agreement and Japanese unspaced suffix cases. Output identity establishes no measured regression, not an improvement in semantic quality.

Instrumented median per-call sampling cost per token fell from **22.89 ms to 5.08 ms**. Median first displayable text fell from **274 ms to 233 ms**; total stream median fell from **351 ms to 264 ms**. These direct diagnostics use medium four-word/**24-token** limits and synchronized stage timing; they are not desktop latency or a production SLA.

### Canonical worker and actual desktop

Three serial worker runs—baseline A, shortlist, baseline B—used ten selected cases, three rounds each, with four-word/**32-token** limits and no native diagnostic synchronization. All **90 requests offered text, passed their mechanical checks and retained identical paired outputs**. Worker timing starts at proceed and excludes debounce, native context capture and rendering.

| Worker case | Baseline A median | Shortlist median | Baseline B median |
| --- | ---: | ---: | ---: |
| English email | 289 ms | 250 ms | 290 ms |
| Portuguese email | 272 ms | 193 ms | 275 ms |
| Stable focused-window facts | 369 ms | 321 ms | 353 ms |
| Long optional reference | 1,497 ms | 1,442 ms | 1,476 ms |

The desktop run used an owned native text view and four alternating baseline/shortlist/shortlist/baseline launches. Each case set a synthetic prefix, then injected its final two characters as actual CG key events. All **64 attempts produced an offer; all 64 Tab and 64 Ctrl+Tab insertions passed readback and append-integrity checks**. These checks verify nonempty insertion, preserved authored prefix and absence of spurious tabs/newlines, not OCR equality for every overlay.

| Warm desktop case | Samples per build | Baseline median | Shortlist median |
| --- | ---: | ---: | ---: |
| Short English | 6 | 541 ms | 467 ms |
| Short Portuguese | 8 | 616 ms | 537 ms |
| Partial English word | 8 | 555 ms | 509 ms |
| 1,992-character English draft | 8 | 882 ms | 905 ms |

The desktop metric is **last native edit → first owned on-screen panel**, not physical-device latency or exact first painted pixels. Separately inspected glyphs show [English](../../evidence/rust-migration/macos-quality-speed-unreleased-english.png), [Portuguese](../../evidence/rust-migration/macos-quality-speed-unreleased-portuguese.png), and [partial long-draft](../../evidence/rust-migration/macos-quality-speed-unreleased-partial-long-draft.png) completions corresponding to accepted text. An immediate after-word PNG was blank; it is not evidence of painted punctuation, although subsequent readback verified insertion of the remaining period.

First-after-launch requests were **1,173/627 ms baseline** and **5,214/564 ms shortlist**. Caches were not evicted. The large candidate outlier remains unexplained; neither controlled cold-start improvement nor an adequate startup experience is established. The long-draft desktop median did not improve.

The final long fixture starts its last clause on a new paragraph. An earlier single-paragraph fixture produced no offer near a wrap edge; it was not counted as successful inference timing. Initial immediate acceptance readback also raced asynchronous insertion; bounded waiting corrected the harness. No production geometry, freshness or insertion-authorization guard was relaxed.

### Verification, rejected work and remaining prerequisite

The workspace/native-smoke suite passed **549 tests**, with one ignored by default and no failures. The release CLI/benchmark builds passed; the benchmark was rebuilt after adding source fingerprints for the new header/build script. The ignored actual-model cache/cancelled-prefill recovery test then passed separately in **auto and CPU modes**.

Native regression coverage compares complete candidate IDs, logits, probabilities, selected indices and evolving state, including clone/reset, sparse/low-ranked constraints, exceptional logits, top-k boundaries and untouched fallback order. A disposable mutation weakening the omitted-boundary tie guard made the new test fail at its buffer/order assertion; the unchanged production guard passed. Two independent reviews identified the libc `qsort` allocation risk and missing isolated boundary/fallback coverage; both were resolved.

All nine before-proceed worker cancellations passed without later output. All nine after-first-result cancellation requests arrived after generation had already completed and are **inconclusive**, which explains the worker benchmarks' exit code 1. These are not active-generation cancellation successes. The direct corpus exits 1 for its nine unchanged no-offer failures.

A separate prompt-cache ablation removed changing healing metadata before the draft. Mechanical checks improved, but blinded model review found new malformed Chinese, Japanese suffix corruption and language switches. It was rejected and the original prompt bytes restored. Utility retries also shifted later seeds in that experiment; its aggregate review counts are not a fully seed-controlled causal comparison. No smaller model or shortened context was adopted.

Owned desktop processes were stopped and original Tern foreground ownership was restored and checked. Normal installation, permission grants, publication state and remote gaming hosts were not changed. The measured short-input gain is useful, but long-context recomputation, first-use variance and existing writing-quality defects still prevent calling this the target experience.

## 27. TextEdit geometry, stage attribution and 128-token prefill

Source-only verification on an **M1 Pro, 16 GiB**, not the reported M5 Pro. The [source-bound receipt](../../evidence/rust-migration/macos-textedit-prefill-unreleased.json) identifies source/binaries/model, all synthetic attempts, configuration mistakes and remaining gates. Nothing was installed, pushed or released.

### Verified geometry and desktop behavior

TextEdit's Menlo 11 row exposes a 13-point native line fragment and a 10-point baseline offset. Its nominal 10.21045-point ascent previously put the computed top 0.21045 points above the first-row clip and correctly failed the global render validator. Native and typographic range models now jointly corroborate the row before using its actual line-fragment envelope. Baseline, font, clipping, security and insertion checks remain intact; browser geometry is unchanged. Negative coverage retains refusal for conflicting rows, clipping, privacy and invalid metrics.

Historical desktop observations used a private diagnostic harness, not a shipped or in-tree tool. It guarded synthetic TextEdit documents, text, selection and focus; missing/ambiguous captures were not eligible offers. Acceptance required independent OCR and exact prefix/suffix/caret readback. These historical checks do not eliminate the non-atomic OS check-to-post race or establish current desktop acceptance.

An initial private profile accidentally omitted `prompt_style`, selecting raw `continuation` rather than the intended `writing` mode for the instruction-tuned model. That pilot had 11/17 observed offer phases and no acceptance attempts; it is retained, not pooled with writing-mode results. Production settings/defaults were not changed.

With explicit writing mode, the 128-token candidate completed **15/15 scenarios, 17/17 offer phases and both independent acceptance checks**. Tab inserted `time` while preserving ` tomorrow.`; Ctrl+Tab inserted `confirmação.`. [Word-acceptance ghost pixels](../../evidence/rust-migration/macos-textedit-word-acceptance.png) and [full-acceptance ghost pixels](../../evidence/rust-migration/macos-textedit-full-acceptance.png) are bound to exact readback in the receipt. Ordinary short-case panel observations were roughly 479–651 ms, long English/Portuguese 1,073/1,186 ms; the suffix-bearing word case took 1,667 ms. These are one corpus run, not a p95/SLA or a matched 64/128 desktop speedup. A focus switch retained old pixels briefly; the first absent observation was 305 ms after observed switch, within a sample cadence that does not establish a strict 300 ms dismissal bound.

### Model and canonical worker comparison

The only model-path change in this experiment is the shared prefill capacity from 64 to 128; the model, prompt, context, sampler and seed policy remain unchanged. Across the full 64-case corpus, three rounds per build, **all 192 outputs, raw generations, sampler seeds and resolved arguments were identical**. Both builds retained **183/192 mechanical passes and 176 offers**. Nine no-offer failures remain in English locale preference, French agreement and Japanese suffix joining. Equality is not a new semantic-quality pass.

Instrumented direct first-displayable median was **215.50 → 200.19 ms**, with long-reference prefill reduced from 14 to 7 batches. Because synchronized diagnostic batch boundaries affect those measurements, a separate canonical IPC baseline/candidate/baseline run used four explicitly reported cases, three rounds each:

| Canonical worker case | 64-token A median | 128-token median | 64-token B median |
| --- | ---: | ---: | ---: |
| English email | 233 ms | 218 ms | 239 ms |
| Portuguese email | 183 ms | 172 ms | 176 ms |
| Focused-window facts | 291 ms | 271 ms | 287 ms |
| Long optional reference | 1,205 ms | 1,114 ms | 1,206 ms |

All 36 worker samples passed mechanical checks and produced identical matched text/errors across builds. Nine before-proceed cancellations passed with no later output; nine after-first-result requests were already completed and remain **inconclusive**, causing benchmark exit 1. All nine release/shutdown checks passed. Actual-model interrupted-prefill/cache/logit recovery passed separately in **auto and CPU modes** at the 128-token cancellation boundary.

The final workspace/all-target native-smoke suite passed **554 tests**, one ignored by default. Release native-smoke and normal-feature CLI builds passed. A normal-feature real-worker smoke passed all four selected cases, before-proceed cancellation and release/shutdown; its after-result cancellation was likewise inconclusive. This is not an active-generation cancellation-latency measurement.

### Stage attribution

`native-smoke` plus `INKLOW_NATIVE_STAGE_TRACE=disposable-profile` enables a preallocated, bounded 4,096-event content-free ring. `BroTypistExportNativeStages` exports only into a canonical private owned `INKLOW_HOME` that cannot overlap the normal profile or escape through symlinks. Capture/context/scheduling, supervisor loading/prepared/ready, first nonempty result, first usable runtime completion and presentation acknowledgements have distinct boundaries. Spans overlap; observations are not additive and acknowledgements are not paint.

The retained writing trace showed a 165 ms median eligibility/debounce interval, 14 ms native context preparation, 33 ms supervisor-ready → runtime-ready and 33 ms first-nonempty → usable-completion intervals. Its ring dropped 1,454 earlier events; only retained intervals are reported. A final all-apps-disabled, nonactivating startup smoke specifically observed loading and prepared lifecycle events, no writing request, no foreground takeover and no dropped events. Loading → prepared was **6.813 seconds** in that run. Earlier fresh-process model preparations varied around 0.9–7.0 seconds without cache eviction. Child model-open versus first-prefill work is not separated by the parent trace.

## 28. Owner-thread wake and child preparation

**Local source only; not installed, pushed or released.** The [scoped receipt](../../evidence/rust-migration/macos-owner-wake-preparation-unreleased.json) records the M1 Pro observations, exact artifacts, rejected shader experiment and verification limits. It does not establish M5 performance or the useful-visible-offer targets.

### Event-driven handoff and actual AppKit correction

Worker/preparer publications now coalesce through an `OwnerWake` state and a default-mode Core Foundation source. A private AppKit event interrupts the existing event wait; ready-only processing drains published results and bounded renderer acknowledgements without shortening the global polling interval. Native capture still supplies fresh authorization. Routine capture deadlines are measured from capture completion, preventing already-expired recapture loops; menu tracking and shutdown retain their exclusion boundaries.

The first nonactivating production-loop probe exposed a real integration failure: AppKit copies posted NSEvents. Pointer-identity matching recorded owner notifications through ordinary polling but **zero native wake deliveries**. A separate nonactivating native check reproduced the changed object identity with preserved event type/subtype/data. Matching now requires an outstanding queued event, application-defined type, private subtype and per-instance cookie.

The corrected signed disposable app observed **two actual private-event deliveries**, three scheduled/consumed owner notifications, successful resident preparation and zero dropped trace records. No writing request was made; every app was disabled in the private profile. Tern retained foreground ownership, and graceful Quit returned zero. Over roughly 12 seconds, the GUI process used 1.07 CPU seconds before the cookie correction and 1.09 afterward, approximately 8.9% and 9.1% of one core. This is a short lifecycle/no-runaway check, not a low-idle-CPU claim or a measured typing-to-glyph improvement.

The existing CI-owned native shell scenario now consumes private events and requires two successive wake cycles. Its full interactive UI suite was not run on this occupied desktop; no `CI` override or permission bypass was used.

### Preparation attribution and rejected offline-shader promotion

The separate child recorder requires `native-smoke`, `INKLOW_NATIVE_STAGE_TRACE=disposable-profile` and `INKLOW_NATIVE_PREPARE_TRACE=disposable-profile`, with the same canonical private-profile export boundaries. It records no text, tokens, app identities or paths. Child timestamps are monotonic and separate from the parent clock. Initial evaluation includes explicit backend synchronization only during opted-in preparation; export happens after measured preparation and before the prepared/error protocol event.

One fresh-process auto run measured backend initialization/GPU discovery at **5,418 ms**, model load at **1,798 ms**, context creation at **38 ms**, framing at **2 ms** and initial evaluation at **277 ms**. Total provider preparation was **7,533 ms**. Subsequent auto preparations were **978/976 ms**. Three CPU preparations were **4,195/3,360/3,077 ms**. These are serial fresh-process observations without OS/Metal cache eviction, not controlled cold-cache percentiles.

The pinned engine embeds shader source and compiles it during Metal initialization. A disposable experiment used an already-downloaded, read-only-mounted Apple Metal toolchain to compile default and tensor libraries, retaining explicit BF16 support; no compiler was installed and no vendor/build/package change was adopted. Runtime loader observations confirmed the experimental libraries were selected.

In the later embedded → precompiled → embedded sequence, provider preparations were **2,457/1,008/1,017 ms**, **2,187/974/967 ms**, and **999/982/987 ms**. Precompiled initial evaluation itself took **1,403 ms** on its first run, then **227/223 ms**. Cache/order differences prevent attributing the earlier 7.5-second outlier as a proven startup speedup. Offline compilation remains unpromoted: warm generation did not improve, and controlled cold behavior, packaging, supported-OS compatibility and physical M5 tensor execution remain unverified.

### Canonical worker and verification boundaries

Four cases, three serial rounds per condition, retained **12/12 mechanical passes and identical matched output** in the previous build, current implementation, offline shader experiment and trailing previous build. These canonical-worker timings exclude desktop capture, debounce and rendering:

| Case | Previous A median | Current median | Offline shader median | Previous B median |
| --- | ---: | ---: | ---: | ---: |
| English email | 221 ms | 209 ms | 209 ms | 209 ms |
| Portuguese email | 182 ms | 173 ms | 171 ms | 169 ms |
| Focused-window facts | 289 ms | 270 ms | 270 ms | 268 ms |
| Long optional reference | 1,201 ms | 1,129 ms | 1,126 ms | 1,129 ms |

The trailing baseline tracks the current build: this is **not evidence of a new inference speedup**. All 12 before-proceed cancellations passed without later output; all 12 after-result requests were already complete and remain inconclusive, explaining benchmark exit 1. Release/shutdown checks passed. The final workspace/all-target native-smoke suite passed **566 tests**, one ignored by default, with zero failures. Actual-model interrupted-prefill/cache recovery passed in auto and CPU modes before the final AppKit-only cookie correction; inference code did not change afterward.

The final normal-feature release build also passed. Its real worker passed all four selected cases, before-proceed cancellation, model release and shutdown; after-result cancellation was again already complete/inconclusive. Even with both diagnostic opt-ins supplied, it produced no child diagnostic export. Its first preparation took **8,119 ms**: startup variance is still unresolved, not hidden by the faster warm measurements. The desktop harness's pure parser/corpus/key/text-boundary self-check passed.

## 29. M1 independent seeds and incremental prefill

**Local source only; not installed, pushed or released.** The M1 Pro/16 GiB is the acceptance target. The [scoped receipt](../../evidence/rust-migration/macos-m1-incremental-prefill-unreleased.json) retains BroTypist outputs, rejected experiments, binary/source identities and remaining gates; third-party research is omitted.

### Desktop verification boundary

The final implementation below has **no new on-screen latency or acceptance measurement**. Historical desktop observations do not establish useful-offer quality or an alternating-order statistical benchmark. A foreground guard refused a later reset before input was sent; no further GUI test or preference/permission change was performed. The private diagnostic harness is not distributed in this repository.


### Retained implementation and controlled rejection

- Direct benchmark cases use `fnv1a64-base-round-case-v1`: base seed, zero-based round and complete UTF-8 case ID determine the initial RNG state. Prior retries cannot shift another case's seed. Reset leaves KV, framing and model ownership intact; production sampling is unchanged.
- Incremental inline prompts keep at least 256 recent Unicode scalars in model prefill. At a safe whitespace/CJK-punctuation boundary, complete earlier text becomes untrusted JSON before changing healing metadata. Both portions remain present; short/unbroken drafts retain full prefill.
- Batch alternatives retain full-draft prefill. A universal split produced unsupported Thursday schedules against authored Tuesday/10 a.m. facts. Preserving the alternatives flag through internal direct sampling removes that new regression without a language-specific exception or new public mode parameter.
- If whole-clause language evidence is uncertain, completed words from that same clause may supply evidence through the unchanged native confidence gate. `The results were extraordinar` now completes as `extraordinary.` in all three matched cases, rather than malformed Portuguese continuations or an empty offer.

The full-draft/empty-prefill variant fell to 163/192 mechanical passes and was rejected. Replacing replay metadata rose to 186/192 but corrupted Chinese; a separate global hidden-replay clarification also reached 186/192 but introduced an ungrounded “usual spot” and did not close the quality gaps. Neither wording change was retained.

### Sustained inference results and first-request cost

The same 43 synthetic steps ran in three serial rounds per build: long English/PT drafts, partial words, Japanese, Chinese, suffix changes and refreshed references. Continued-request figures exclude each trace's first request. Timings end at the first nonempty canonical callback after healing replay—not a proven useful word, worker transport or painted overlay. No-offer requests remain in the denominators and raw results.

| Continued long draft | Baseline nonempty/requests | Retained nonempty/requests | Baseline median / p95 | Retained median / p95 |
| --- | ---: | ---: | ---: | ---: |
| English | 18/21 | 20/21 | 531 / 606 ms | 162 / 235 ms |
| Portuguese | 18/21 | 18/21 | 634 / 730 ms | 207 / 266 ms |

Median evaluated prefill tokens fell from 321 to 54 in English and 401 to 70 in Portuguese. The first long request became slower: median English 582→649 ms and Portuguese 644→725 ms, three observations each. Preparation, initial-context cost and changed references must not be hidden inside a sustained-cache headline.

Across all steps, nonempty results increased from 114/129 to 116/129. The two extra English endpoint offers were dangling additions, not established usefulness gains. The complete 64-case/three-round corpus retained 184/192 mechanical passes versus 183/192 before the language fix. Eight no-offer failures remain: two English locale-preference, three French agreement and three Japanese suffix cases. Literal checks observed no contradiction in their 12 covered samples; that narrow result does not clear broader grounding.

### Quality and verification boundaries

Independent machine-assisted blinded review covered all 333 paired samples from the corpus, traces and 12 long-context worker cases. It was not calibrated human review. Final corpus/trace outputs are byte-identical to the reviewed candidate; final worker outputs were already represented on one of the reviewed sides. The review found shared fabricated dates/rooms, weak contextual use, suffix problems and Portuguese synonym agreement errors. The preserved English alternatives still include unsupported fiscal-quarter/month-end deadlines. Baseline equivalence is not quality acceptance.

The actual canonical-worker probe exercised 12 fresh-worker cases, withholding `proceed` for 100 ms without output, then requiring final/done and acknowledged shutdown. All 12 completed without protocol error. All four final batch-alternative outputs matched their baseline outputs, including removal of the newly invented Japanese schedules; this does not excuse the baseline's own factual errors.

The final all-target native-smoke run reported 570 passes and two opt-in tests ignored by default. Both real-model tests—independent seed isolation and interrupted-prefill/cache-logit recovery—then passed serially in **auto and CPU** modes. All three corpus rounds also passed pre-cancel and first-native-chunk cancellation with no post-cancel output. These are specific native boundaries, not a desktop active-generation cancellation claim. Normal-feature GUI/CLI binaries compiled, and the desktop harness's pure self-check passed without importing or exercising UI.

The normal-feature CLI then passed the same 12 fresh-worker long-context cases with outputs identical to native-smoke and all shutdowns acknowledged. Supplying both diagnostic opt-ins still produced no diagnostic export. The existing `worker_smoke` also passed with the digest-verified cached Gemma model on CPU: explicit proceed barriers, ordinary writing, partial-word/suffix acceptance and stale-document refusal, ready-before-proceed cancellation, release, reload, shutdown and the public `complete` CLI. Its acceptance checks use the canonical suggestion state, not a live application; they do not replace GUI proof.

Promotion remains blocked on semantic-quality gates and a fresh M1 foreground interval for the retained candidate: realistic typing, first useful readable offer, exact acceptance, focus/stale-result behavior, startup/idle reload and resident-resource observations. The 300 ms median / 600 ms p95 useful-visible-offer target is **not yet proven**. Installed applications, normal saved preferences, publication state and remote gaming hosts are unchanged.

