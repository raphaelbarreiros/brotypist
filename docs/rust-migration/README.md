# BroTypist: feature-preserving Rust conversion

Status: **implementation and native integration in progress; native Rust UI, Rust 1.99.1/Cargo, a dedicated `migration/rust-native` branch, draft PR and native CI artifacts approved on 2026-10-30**. No main merge, normal-app replacement or release publication is authorized.
Baseline: `6eaeed5fe56e1ea8dcf878b1b7498f922c26fd59`, version 0.4.13.

Requested outcome: convert the application to Rust for macOS, Windows and Linux while retaining all existing functionality. macOS remains the first parity gate; implemented Windows/Linux paths must also be migrated. This is not authorization to ship reduced-functionality builds, add cloud features, install over the normal application, or publish releases.

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

## 6. Immediate next action

Current candidate: the integrated macOS workspace passed **451 test invocations** (including two child-probe invocations), followed by successful native application/updater/tool/example builds. This count is not a coverage comparison with the Python suite. The real Tiny and Gemma worker/CLI paths, Portuguese auto-mode continuation, ready/proceed barrier, stale-document rejection, release/reload and clean exit were exercised in disposable profiles. The independently linked offline review executable passed relocated CLI/source-hash/raw-output/private-file/no-overwrite checks without creating an application profile.

The native benchmark exercised six selected writing/utility cases through direct inference and the canonical worker, with separate timing paths. All selected case mechanical checks passed. Synchronous direct cancellation passed; one after-result worker cancellation was explicitly inconclusive because the producer had already finished. A separate direct EN/PT run exercised the 256-token diagnostic cap, observed actual sampler arguments/BOS/token budgets and measured process-lifetime RSS; it made no semantic-quality or physical-typing latency claim.

Offline dependency-notice collection succeeded against the actual locked macOS source cache. It preserves exact declarations/terms and records legal limits rather than granting a first-party license. A clean-source, identity-signed Rust Mac bundle at `a278b6b45d105a17e234e6b65c3e8d781fce6c2a` built and verified; its designated requirement exactly matched `/Applications/BroTypist.app`. Real DMG/ZIP smoke passed read-only extraction/copy, bundled CLI/doctor, Tiny inference, nonactivating LaunchServices background launch, OS Quit with exit 0, removal/data-fixture preservation and ZIP traversal refusal. Receipt: `/private/tmp/brotypist-rust-source-a278b6b/installer-smoke/macos-installer-smoke.json`. This limited receipt explicitly records `full_matrix=false`: no physical host typing, live TCC grant retention, prior-version upgrade, notarization or OTA acceptance is claimed.

The complete 51-case corpus was exercised with cached Gemma 4 through both the Rust direct diagnostic and the immutable Python oracle. All 51 insertion/decoded-choice values and prompt token-count lists matched; 48 raw output strings were byte-identical, with three differing only in JSON whitespace. Both reports failed the same three availability checks because the model returned safe empty completions (`language-en-locale-preference`, `grammar-fr-agreement`, `suffix-ja-unspaced`): **48/51 each**, not a full pass. Safety, token/BOS and cleanup checks passed. Reports: `/private/tmp/brotypist-rust-full-corpus-z95szwqa/report.json` and `/private/tmp/brotypist-oracle-full-corpus-iy2tsw_d/report.json`. This is one corpus/seed comparison, not universal model-output or semantic-quality equivalence.

[Hosted run 41075418249](https://github.com/renan-brasilio/brotypist/actions/runs/41075418249), PR head `6567bca`, passed complete macOS and Linux jobs. Mac covered tests/build/CLI/real worker/canonical twelve-pane settings/Welcome/English-Portuguese owned-preview acceptance/clean Quit/development bundle. Linux additionally passed actual DEB install, same-version replacement, owned X11/AT-SPI visible Quit, installed Tiny worker and public CLI, exact external settings/history/model preservation on purge, and relocated portable diagnostics. Its inspected receipt records merge-source `1e783a57dfc8a068bb97eadc091c31e6d27259ac`, successful cleanup and `full_installer_matrix_passed=false` because previous-release upgrade was not exercised. Native captures/readback were inspected; Mac captures remain owned NSView bitmaps, not external-screen or signed-host proof.

Windows real Tiny-worker/public CLI passed in [run 41080048721](https://github.com/renan-brasilio/brotypist/actions/runs/41080048721), PR head `287b6bf`: model loading, ready/proceed barrier, inference, cancellation, release/reload and clean shutdown all passed. Native UI then reached actual six-pane rendering, but its multiline-setting probe used `WM_SETTEXT`, which does not emit `EN_CHANGE`; no changed draft reached the canonical handler. It now uses the owned Edit control's `EM_REPLACESEL` transaction and separately retains dirty-state and unsaved-runtime guards. Windows UI/MSI/ZIP re-execution remains required.

An exhaustive compiled Rust/immutable-Python comparison found 129 default-full casefold differences: the pinned dependency uses Unicode 9 while the baseline uses Unicode 15. A single canonical supplement now restores all **1,112,064 Unicode scalars** (zero remaining differences), shared across app/history/policy, offline updater and tools. Actual rules/history/choice/utility/collision/MSI-identity regressions passed. [Recorded mapping proof](../../evidence/rust-migration/unicode15-casefold.json) deliberately does not claim normalization/category/regex equivalence. KWin, signed external-host editing, live TCC, previous-version/OTA and unavailable large-model scenarios remain named gates. Main, normal application and releases are unchanged.
