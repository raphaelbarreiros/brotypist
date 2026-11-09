# Rust migration: source review and baseline

Reviewed 2026-10-30. Repository: `renan-brasilio/brotypist`.
Baseline: `6eaeed5fe56e1ea8dcf878b1b7498f922c26fd59` (`main`, version 0.4.13).

## Scope and evidence

The baseline phase used five bounded read-only reviews covering runtime/features/spelling, inference/models/workers, macOS native integration, Windows/Linux/Wayland, and UI/data/updater/packaging. The integration owner independently exercised the old application and selected findings before implementation. This is a broad engineering review, not exhaustive vulnerability certification. Subsequent Rust implementation and independent review are recorded separately below. The user later authorized a backed-up local Mac installation; profile bytes and permission policy were preserved, and releases remain unchanged.

The baseline implementation contained 24,614 lines across `src/inklow` Python/C/header files. All Python paths in this baseline review refer to commit `6eaeed5fe56e1ea8dcf878b1b7498f922c26fd59` or the immutable external oracle, not the Rust-only candidate checkout. The old indexed graph contained 2,975 nodes and 32,023 edges across 99 parsed files. Its controller trace misresolved an explicit anchor and its file-level test queries returned no matches despite existing tests; graph output was not treated as proof of coverage.

Observed locally:

- `uv sync --python 3.12 --extra dev`, then `uv run pytest -q --junitxml=/tmp/brotypist-review-baseline-20261030.xml`: **1,524 passed, 22 skipped in 27.54 seconds**.
- Isolated-profile `brotypist --version`, `doctor`, and `model list`: exit 0. Doctor reported Accessibility available and Input Monitoring required for the source host. No global input hook was started.
- Existing cached Tiny/SmolLM2 GGUF, CPU, actual `tools/smoke_bundle.py --source-worker`: `loading -> ready -> result -> done`, four cumulative updates, ` interest in the`, worker exit 0. The 6,850.43-ms harness interval is not desktop typing latency.
- Existing cached Gemma 4 E2B, auto/Metal, actual CLI writing completion: `Amanhã vamos nos encontrar para` -> ` discutir os próximos passos.`, exit 0. One sample is not a quality score or cross-runtime equivalence proof.
- Disposable probes reproduced R1, R2, R3, R4, and R12 below. They changed only synthetic process state/temporary files.

The model cache is shared even with `INKLOW_HOME`; these checks read existing weights and wrote only a disposable profile. No model download or normal-profile/keychain change was required.

## Findings

“Static” means the source path was inspected, not that the defect was observed in a real host application. Preserve intended functionality, not these defects. Fixes require explicit behavioral cases in their owning migration package; do not silently change unrelated policy.

| ID | Priority | Finding and source | Evidence / disposition |
|---|---|---|---|
| R1 | P1 | Secure Input detector fails open when Carbon is unavailable or raises: `src/inklow/native/macos.py:83-89`; field capture gates on it at `488-494`. | Disposable actual-function probe returned `false` for both unavailable and throwing detector. Other field protections still exist; no private-content exposure was observed. Model permission state as allowed/blocked/unknown; unknown must block. The appearance detector already uses conservative behavior (`macos_vision.py:113-125`). |
| R2 | P1 | Windows memory discovery calls `os.sysconf` without handling its absence: `models.py:156-178`; catalog consumed by `runtime.py:2045-2054`. | Simulated Windows API availability in an isolated process: actual function raised `AttributeError`. Not a live Windows run. Use the native Windows capacity API; unknown capacity must remain supported. |
| R3 | P2 | JSON-decoded choices bypass output safety: raw utility screening precedes decoding (`providers.py:323-332`), while `features.py:179-215` rejects decoded Cc/Cf but not structural markers or Zl/Zp separators. | Actual pipeline accepted wire escapes as `good\u2028next` and `<think>reason`. Choice consumers pass decoded strings directly to native insertion (`runtime.py:1479-1504`). Validate decoded insertion strings, not only wire bytes. |
| R4 | P2 | Interrupted developer `--replace` promotion can lose the previous build: `tools/build_app.py:107-122,149-198`. | Injected KeyboardInterrupt on second rename inside real TemporaryDirectory lifecycle: destination absent, zero retained old payloads after cleanup. This affects the developer builder, not proof of an OTA rollback defect. Retain previous build outside disposable cleanup until promotion commits. |
| R5 | P2 | Window-only authority changes do not invalidate in-flight output: `core.py:163-167`; `runtime.py:568-591,959-961,1135-1169`. | Static. Submission checks window identity but result consumption checks only the text/policy key. Carry a separate window/document authority lease through results/presentation; geometry movement alone must not regenerate text. |
| R6 | P2 | Passthrough dismissal records completion/correction attempt regardless of actual tool: `runtime.py:1564-1573`, contrasted with emoji/synonym/correction scheduling at `1394-1432`. | Static. Unchanged emoji/synonym/correction may reappear on the next cycle. Store dismissed tool plus exact field authority; cover bypass separately. |
| R7 | P2 | Explicit completion can be discarded while spelling is pending and never retried: `runtime.py:1068-1075,1154-1169,1429-1432`. | Static. Final output clears the request while the existing attempt blocks rescheduling. Specify pending-verdict/result ownership before porting scheduling. |
| R8 | P2 | Windows native message/input dispatch is treated as accepted text without host readback: `native/windows.py:493-511,532-582`; accounting at `runtime.py:1541-1549`. macOS has an analogous documented dispatch/readback distinction. | Static; no Windows host rejection reproduced. EM_REPLACESEL has no success return, and queued SendInput events do not prove applied text. Use explicit dispatched/applied/rejected/indeterminate outcomes; never retry an uncertain edit automatically. |
| R9 | P2 | Windows suppresses consumed shortcut presses but forwards their releases: `native/windows.py:595-614`. | Static. Target applications can observe unmatched edges. Preserve injected-event/AltGr refusal and pair consumed key events. Native Windows acceptance remains required. |
| R10 | P2 | Current Wayland smoke harness targets removed inline state: `tools/wayland_smoke_fixture.py:274-291,569-584`; current bridge `native/wayland.py:130-139,552-566`. | Static mismatch: harness reads nonexistent `_panel_text` and expects suggestion-bearing panels, while production deliberately refuses nonempty inline presentation. Replace obsolete smoke expectations with current manual/refusal scenarios; do not restore unsafe inline behavior to satisfy the harness. |
| R11 | P2 | Native synonym grammar requires 3-5 choices while prompt permits `[]`: `providers.py:514-517`; `features.py:168-173`. | Direct source contradiction. Permit safe abstention; never fabricate choices to meet a count. |
| R12 | P3 | macOS CLI rejects `--pane models` despite an AI Models settings pane: `__main__.py:19-21,44`. | Actual CLI exited 2 with invalid-choice error. Preserve the Models destination in the replacement CLI. |
| R13 | P3 | Terminal diagnostics can contain arbitrary conversion error strings: `native/macos_terminal.py:532,628,723-725`, published at `458-460`. | Static. `int()` ValueError text can include native-provided strings. Use fixed diagnostic codes, not arbitrary exception messages. No personal text was collected. |

Additional investigation gate: updater timeout rollback swaps back and relaunches the previous app without explicitly stopping a still-starting replacement (`update_install.py:796-816,872-890`). Delayed-child/process ownership needs a controlled runtime scenario before claiming healthy rollback. This is not a demonstrated exploit or established data loss.

No tracked LICENSE/COPYING/NOTICE file was found by scoped repository discovery, although DESIGN.md describes the app as MIT licensed. Clarify first-party licensing before redistribution; do not invent a license grant. Model and dependency licenses remain independent.

## Existing CI failures and evidence gaps

Latest inspected run: [40875674239](https://github.com/renan-brasilio/brotypist/actions/runs/40875674239), same baseline revision; both platform jobs failed.

- Linux: actual source worker emitted `loading`, then EOF (`tools/smoke_bundle.py:67`). Root cause is **not established** by the available log. Source tests passing does not close it.
- macOS: actool reported only macOS 15.5 compilation support while native icons request deployment 26.0; build raised `actool did not produce the native app icon and its bundle metadata` (`tools/generate_icons.py:380-406`). Pin/select a compatible icon toolchain; runtime minimum and asset compiler version are separate constraints.
- CI matrix has macOS and Ubuntu only (`.github/workflows/ci.yml:35-43`), no Windows lane. Linux GUI smoke uses Xvfb, not KWin. Neither cross-compilation nor synthetic protocol peers certifies live OS behavior.
- Latest GitHub stable was freshly observed as [v0.4.13](https://github.com/renan-brasilio/brotypist/releases/tag/v0.4.13). The checked-in changelog/verification record has historical candidate language; do not attribute earlier receipts to new binaries.

## What must not be reported as missing functionality

Already implemented protections include bounded/cycle-safe/protected descendant geometry, UTF-16 field-local mapping, Electron append-only capability, visible-presentation acknowledgments, input replay gates, cancellation epochs, model preparation privacy barriers, signed update manifests, and transactional offline installation. Historical Electron review findings were remediated in this baseline; do not reopen their old source locations as current bugs.

Current limitations are part of the safety contract: ordinary KDE Wayland inline is disabled; Linux X11 arbitrary range replacement is refused; Windows publication is held; Docs accessibility setup is not Docs typing support; terminal writing is narrowly pinned to one verified signed Codex renderer. Rust alone does not change those OS capabilities.

## Migration prerequisites

Use [the parity inventory](parity.md) as the acceptance register and [the implementation plan](README.md) as the work/dependency contract. Rust/Cargo were absent during the baseline phase; approved Rust 1.99.1 is now installed/pinned, with native AppKit/Win32/GTK selected and an isolated migration branch/draft PR. No completed cross-platform parity is inferred from source conversion.

## Independent Rust review and exercised integration

Additional bounded reviews covered text/provider parity, persistence, updater/distribution boundaries, and native/runtime authority. Findings below concern the Rust candidate, not new claims against the historical baseline.

| Finding | Correction / evidence |
|---|---|
| HTTP total request deadline incorrectly replaced streaming inactivity timeout. | Separate streaming read timeout and longer preparation timeout. Owned-loopback regressions exercised active NDJSON/SSE streams outliving the timeout, silent-read failure and preparation timing. |
| Linux history credentials did not unlock/create the native default collection; sensitive-text boundaries used Rust rather than Python word semantics. | Direct Secret Service collection handling and explicit compatible boundary predicate. Shared sensitive-history regressions passed; actual locked/unlocked Linux desktop credential-store behavior remains a native gate. |
| Windows/X11 input notifications revoked cached claims before initiating shortcut dispatch. | Transaction-first shortcut claiming; only forwarded/cancelled input invalidates. Target-native transaction/replay tests are included; their execution requires the hosted native lanes. |
| Non-browser Mac snapshots had empty document authority, preventing consented window context. | Stable process/window/document identity, detached-ticket revalidation and focused-window ownership. Mac identity regressions passed; live permission/host context is not claimed. |
| Anchored alternatives bumped the epoch while retaining old completion claims. | Disarm and republish the retained completion for fresh visible acknowledgment. Regression failed before the fix, then passed for pending alternatives and safe abstention, including actual engine acceptance/remainder. |
| Terminal authority included mutable row/caret evidence, making successful dispatched appends fail readback confirmation. | Separate stable signed session/window authority from exact one-use append evidence. Mac lease/readback regressions passed; live signed Codex host typing remains unverified. |
| Native installer probes emitted boolean success and nested identity metadata rejected/ignored by their release consumer. | Canonical boolean receipt and nested version/source support; explicit conflicting failures remain failures. Regressions failed before correction and passed afterward. Limited matrix declarations remain intact. |
| Version changes omitted workspace package identities in Cargo.lock. | Offline workspace-only Cargo update, rejection of unrelated dependency changes, restoration on failure. A real disposable Cargo workspace failed locked resolution before correction and passed afterward. The obsolete source-string assertion was removed. |
| Crate archives omitted complete standard license text; source `copying.rs` falsely counted as legal material. | Exact checksum/source-bound upstream declarations plus authentic vendored standard terms; source-code false positives rejected. Actual offline collection passed. No first-party, Apple SDK, model or toolchain license certification is implied. |
| Portable launchers misrouted Models/Welcome or hid explicit settings behind background mode; doctor discarded all output if native initialization failed. | Public pane mapping/visibility corrected and unavailable native integration represented as diagnostic data, never as a simulated bridge. Hosted native launch checks and headless Linux doctor exercise remain target gates. |
| Offline saved-output review-label attachment was absent from the initial native tools. | Independent Rust review executable; twelve behavior cases and actual relocated CLI/source-hash/raw-output/mode-0600/no-overwrite smoke passed. Windows retains inherited-ACL semantics without claiming POSIX permissions. |
| Hosted UI smoke used a view-only AppKit action and saved blank GTK surfaces before paint, then stalled in modal handling/self AT-SPI queries. | Real advertised exposed AX actions; frame-settled GDK pixels; self-PID filtering before properties; correct owned modal response and independent watchdog. Complete canonical Mac and GTK UI re-execution passed in run 41067489417; captures/readback were inspected. |
| Installed Linux diagnostic gate still expected the old native-status string. | Canonical doctor emits an object. Check platform/readiness/accessibility, paused state and absence of a doctor-created hook, not incidental status wording. Run 41075418249 passed actual install/replacement, native visible Quit, installed inference/CLI, data-preserving purge and relocated portable checks. |
| Windows shared llama/ggml DLLs used isolated `/MT` CRTs while passing `FILE*` streams across DLL boundaries. | Use consistent `/MD` for those DLLs, retaining the opaque-only Rust/bridge static CRT. Authenticate/stage selected Visual Studio redistributables using an isolated system PowerShell host, preserve source/hash provenance, and require app-local dependency closure. Actual Windows Tiny-worker/public CLI passed in run 41080048721. The earlier generic failure retained no native exception; the repaired path now loads and evaluates successfully. [Microsoft CRT boundary guidance](https://learn.microsoft.com/en-us/cpp/c-runtime-library/potential-errors-passing-crt-objects-across-dll-boundaries?view=msvc-170). |
| Windows preview language selector reset writing on focus notifications. | Seed the preview only for `CBN_SELCHANGE`, not `CBN_SETFOCUS`/`CBN_KILLFOCUS`. A native focus-transition/document-readback regression is included; Windows execution remains required. Paused predictions intentionally retain global shortcuts while independently blocking external field-text capture. |
| Unicode 9 casefold tables differed from the immutable Python baseline's Unicode 15 tables. | Actual exhaustive comparison found 129 differing scalars. A shared allocation-free supplement yields zero differences across all 1,112,064 Unicode scalars. Rules/history/choice/utility, NFC-aware archive/payload rejection and exact baseline MSI identity regressions passed. Authored text and caller normalization boundaries remain unchanged. [Mapping proof](../../evidence/rust-migration/unicode15-casefold.json); no normalization/category/regex equivalence claim. |
| Windows multiline draft smoke used `WM_SETTEXT`, which omits `EN_CHANGE` for `ES_MULTILINE`. | Use the actual owned control's replacement transaction, retaining exact field readback, dirty draft and unchanged runtime checks. Run 41085218652 passed edit/pane/Revert/Save/dirty-close and independent consent scenarios. The original failure did not establish an unsaved configuration mutation. [Microsoft notification contract](https://learn.microsoft.com/en-us/windows/win32/controls/en-change). |
| Windows model-picker smoke targeted a missing legacy control, then a ComboBoxEx container rather than its actual filename Edit. | Runs 41093435844/41096104885 isolated the absent `edt1` and ineffective container write. Resolve the owned embedded Edit through `CBEM_GETEDITCONTROL`, enter via its real replacement transaction and require exact readback before Open. Run 41099292517 passed import identity/readiness, then reached a separate preview timeout. [Microsoft embedded-Edit contract](https://learn.microsoft.com/en-us/windows/win32/controls/cbem-geteditcontrol). |
| Windows preview seed used selection removal instead of an explicit document-end caret. | `EM_SETSEL(-1,-1)` deselects; eligibility uses text before the caret. Set both bounds to the borrowed seed's UTF-16 length. Run 41105229573 passed bilingual end-caret, visible inline/word/full readback, language-focus preservation and complete Welcome/Quit checks. [Receipt](../../evidence/rust-migration/windows-owned-ui.json); [Microsoft selection contract](https://learn.microsoft.com/en-us/windows/win32/controls/em-setsel). |
| Canonical Windows build paths exceeded native tool constraints. | Run 41105229573 failed an MSBuild command under verbatim `\\?\D:\…` paths. DOS spelling cleared that error; 41108610393 then exposed a 273-character FileTracker filename. Separate short private compiler staging advanced 41112026118 through compilation to notice collection. Canonical fresh-output containment, owner-only permissions and failure retention remain enforced; native command-directory and Unix private-stage regressions passed. No registry/policy change or skipped compiler check. |
| Notice collection compared Cargo manifest paths against a differently spelled canonical checkout path. | Derive relative manifest paths using Cargo's own workspace spelling only after verifying that workspace's canonical identity. Preserve component-wise link refusal and outside-root rejection. The equivalent `/var` versus `/private/var` case failed locally before the one-line correction; all 15 notice tests then passed, including the outside-workspace fail-before-output boundary. This also addresses ordinary versus verbatim Windows drive spelling without persisting absolute source paths. |
| Native installer probe counted transaction completion as verified removal. | The final uninstall transaction and post-removal observation both used `uninstall`, producing a count of two where one verified removal was required. Rename the transaction observation to `uninstall_transaction`; retain all actual registration/payload/shortcut/sentinel assertions and the strict final count. Source-evidenced review finding; actual lifecycle execution remains required. |
| Canonical Windows artifact/log paths crossed directly into MSI consumers. | Reuse the shared namespace-only tool-path adapter for WiX inputs/output/payload roots, MSI artifact serialization and PowerShell work/input paths. Rust keeps canonical hash/provenance/containment checks; the native script rechecks artifact hashes before COM/install/repair. No MSI execution is claimed from this source correction. |
| PowerShell product-registration queries passed an empty SID string instead of native NULL. | Run 41116419958 passed actual clean bundle/MSI/ZIP assembly and PE/portable audits, then stopped at product query before installation. Use documented `NullString.Value` in both `MsiGetProductInfoExW` phases and retain strict return-code checks with bounded code/context/property diagnostics. Native lifecycle confirmation remains required. [PowerShell null-string contract](https://learn.microsoft.com/en-us/dotnet/api/system.management.automation.language.nullstring); [MSI current-user SID contract](https://learn.microsoft.com/en-us/windows/win32/api/msi/nf-msi-msigetproductinfoexw). |
| Installer helper `Cli` collided with PowerShell's built-in `cli` alias. | Run 41119387237 passed MSI family/isolation preflight after the native-NULL correction, then failed in relocated portable CLI invocation. Aliases precede functions, so `Clear-Item` received the helper's argument array instead. Rename the helper and every caller to `Invoke-NativeCli`; do not mutate the system alias table. Native lifecycle confirmation remains required. [Command precedence](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_command_precedence). |
| Exclusively published MSI retained an owner-only staging ACL. | In 41123495273, relocated ZIP/CLI/doctor passed but the MSI service returned 110 and source-file access denied (1309/5). `publish_file` hard-links the same file, preserving its private DACL. Grant only SYSTEM read access to the verified non-secret MSI before publication; do not change user data/private stages or grant broad write access. Actual install/repair/uninstall confirmation remains required. |
| Rust Mac shell visually regressed despite passing owned-control assertions. | User screenshots exposed a dashboard-style menu with battery/model/raw diagnostics, overlapping sidebar branding, missing selection and bad form geometry. Source `61c4e3` restores compact battery-free menu/status presentation and baseline grouped layout/viewport sizing. 396 workspace tests plus two child invocations passed; twelve native panes rendered, selected light/dark/minimum/bottom images inspected, signed app reinstalled with unchanged data/identity. [Layout evidence](../../evidence/rust-migration/macos-ui-layout/receipt.json); [installation](../../evidence/rust-migration/macos-ui-local-install.json). Not WindowServer/TCC/host-typing proof. |
| Settings still felt misaligned after the first visual correction. | Shared form blocks used 24/28/38-point leading offsets, controls had a fixed 27-point header height, and switches retained an obsolete 42-point width. Actual AppKit now reports a 54×24-point switch. Use one content inset, native intrinsic sizes and alignment rectangles, centered title/control headers, and responsive compound rows. The actual native intrinsic-size check failed before the switch factory correction and passed afterward; [geometry, native renders and four retained-draft resize observations](../../evidence/rust-migration/macos-settings-alignment/receipt.json) passed. No hard-coded replacement switch width or per-pane positional patch. |
| Edge alignment passed but Settings still felt squeezed. | The shared layout retained long full-width descriptions, header-only control centering and tight row/section rhythm. Bound each title/description pair to a shared reading column, reserve separate control space, center against the complete text block and apply consistent padding/stacking. Visual inspection caught wrapped copy clipped by attributed-string measurement; native text-cell sizing now preserves all lines. [41 images and 63 native geometry observations](../../evidence/rust-migration/macos-settings-spacing/receipt.json) passed, including bottom reachability and retained Unicode editor state. 396 workspace tests plus two child invocations passed. |
| Sidebar selection still looked careless and category icons undersized. | Preserve native row actions but inset actual larger image ink inside a padded canvas and one rounded selection background. Disable duplicate native button fill; keep the semantic fill dynamic across appearance switches and account for native button state changing before navigation dispatch. [43 native images / 67 geometry observations](../../evidence/rust-migration/macos-sidebar-craft/receipt.json) and 396 workspace tests plus two child invocations passed. The new supplied app-icon ZIP is unreadable under macOS file permissions; combined icon integration and signed installation remain blocked, not claimed complete. |
| Windows MSI reports per-user installation but creates a machine-hive Apps/Uninstall entry. | Build-free [41136859854](https://github.com/renan-brasilio/brotypist/actions/runs/41136859854) authenticated the unchanged `6b0a711` package and completed its diagnostic replay in 51s. Native context is exclusively user-unmanaged (2), State=5, AssignmentType=0; explicit 64-bit HKLM ARP and current-SID InstallProperties both exist, HKCU ARP does not. Fresh install/uninstall/synthetic continuity passed; the ownership gate remains failed. [Native evidence](../../evidence/rust-migration/windows-installer-scope/windows-core-installer-smoke.json) matches the pattern in upstream [WiX/MSI #9323](https://github.com/wixtoolset/issues/issues/9323), not proof of its internal cause. No scope/permission workaround or acceptance relaxation. Temporary Git trigger and broad registry-dump scaffolding removed after retaining evidence. |

Earlier integrated checks passed 451 invocations plus later notice/staging regressions; the Mac spacing correction passed 396 workspace tests plus two child invocations and actual native renders. Signed Mac source `315d125` is installed with unchanged identity/data and a retained rollback backup; 41 native captures cover spacing, wrapping, final-content reachability and editor retention. Current Windows evidence isolates the still-failed MSI/ARP gate above without rebuilding or weakening it. The 51-case Gemma comparison preserves the same three safe-empty availability failures (48/51 each). Remaining signed-host/TCC, KWin, installer upgrade/rollback/OTA and unavailable-model boundaries stay explicit in [the current plan](README.md). Main and published releases remain unchanged.

## Current R1–R13 disposition

The findings table above records the immutable Python baseline; its “current”
wording describes that historical review, not a newly observed Rust defect.
The following is the current Rust reconciliation. Paths are relative to
`crates/brotypist/src/` unless explicitly prefixed. Exact symbols/test seams
identify source corrections; they are **not substitutes for native acceptance**.
Earlier [Tab/emoji locked/offline Mac source gates](../../evidence/rust-migration/tab-emoji-current/receipt.json)
passed **743 reported passes: 738 parent-suite plus five child-invocation passes,
26 result summaries, one ignored**, plus binaries/examples and formatting.
These workspace/all-targets `native-smoke` totals are not distinct-scenario
counts; there was no new native Windows/Linux run in this wave.
Historical [locked/offline RAM-unit source gates](../../evidence/rust-migration/ram-units-current/receipt.json)
passed **735 reported macOS test passes** (730 parent-suite passes plus five child-
invocation passes; 26 result summaries, one ignored) and binaries/examples.
Linux ARM64 workspace/all-targets tests (569 library passes, one ignored,
plus other suites) and binaries/examples passed. Earlier 733-pass and 721-test Mac checkpoints remain historical.
These are local uncommitted-source checks, not release attestations.
Source reviews of AX reuse, publication frontier, Windows capability,
emoji notices and selected readback found no findings; this is not exhaustive
certification. Earlier Mac/Linux gates exercised generic runtime selected-
replacement/readback but excluded the nested Windows-only pure decision module.
All 12 pure Windows tests now compile and pass on both hosts through one canonical
`platform::windows_decisions` module under `cfg(any(target_os = "windows", test))`.
This does not execute native Windows UIA/hooks or certify hook lifecycle,
MSVC/SDK, installers or physical typing.

The [parity evidence lanes](parity.md#current-per-identifier-reconciliation)
separate **C** (controlled Controller/overlay), **A** (genuine hookless owned-child
MacBridge AX), and **M** (ordinary real-model owned widget), now preserved in
[durable scoped observations](../../evidence/rust-migration/native-owned-observations.json).
Their older runs lack frozen whole-tree/executable attestation. M's
overall gate remains **false**: two English abstentions and two Portuguese
positive draw/full-prefix acceptances. It is not canonical Welcome. Canonical
owned Welcome aborted on input interruption before any model request/edit,
with empty cases and clean Controller shutdown. Its positive gate remains
failed/unexercised, not a permission/model failure. Historical Windows bilingual
preview evidence is not a new bilingual pass.

### Tab/emoji correction and R8 boundary (2026-11-08)

The [earlier receipt](../../evidence/rust-migration/tab-emoji-current/receipt.json)
records permanent Word/All renewal regressions: only verified remainder
exhaustion clears the attempt and restarts ordinary debounce; retained remainder
stays receipt-bound and no-offer stays quiet. The final user-approved native
WhatsApp reservation persists across idle/no-offer until input, caret/focus,
default Escape or policy/safety revocation. Other apps retain the two-second
renewal/repaint guard. Neither accepts unseen text or retries generation.
Corrected red was 110 runtime passes/two new failures; green was 112 passes,
including six word accepts across two generations with fresh paint and single
use. This does not reproduce or certify physical repeated typing.

The [remaining-Tab follow-up](../../evidence/rust-migration/tab-followup-current/receipt.json)
preserves each failed candidate and the final user-confirmed success. Raw AX
measurement corrected the zero-range/glyph-row Y convention; owned row coverage
remains partial for one unavailable non-BMP font case. A later native insertion
was proved posted but lacked immediate readback during a final-anchor race.
Fresh safe-target proof now permits bounded deferred observation, not mutation
retry or guessed success. Both pending confirmation and native lease storage
expire at their original two-second bounds.

Actual WhatsApp geometry then showed a genuinely multirow whitespace gap; that
refusal remains correct. The user chose field-session Tab reservation rather
than relaxing geometry. The final manual test reports success with seven exact
edits, two genuine no-offers, zero hook timeouts and no validated gap failure.
Emoji is separately user-confirmed fixed. These bounded results do not close
the full physical host/security/IME/release matrix.

The dedicated default-false `Snapshot.can_replace_emoji` and
`NativeBridge::replace_emoji_range` preserve ordinary replacement, synonyms and
typo capability. Native WhatsApp-only fallback checks safe API capabilities,
preflights before selection, independently recaptures exact checked UTF-16
selection/unchanged text/authority and rechecks foreground/field/window/IME/
revision before one PID-targeted event pair. No whole-message rewrite,
Backspace, clipboard or session-global posting. Any uncertainty after selection
may have changed is `Indeterminate`, never no-mutation rejection/replay. Only
already dispatched dedicated emoji may retain the exact selected-only
intermediate under existing deadline/revision/privacy/policy checks; strict
final glyph/text/caret confirms once. Permanent runtime/native/core regressions
cover stale/unrelated/foreign/revoked/protected/expired/pause states, no revival
and no synonym/correction expansion. This is not relaxed generic reconciliation.

Actual expanded `--owned-choices` passed eight controlled acceptance cases,
including ZWJ/alias/sole emoticon, using real WorkerClient/Controller/OverlayOwner,
native draw/typed receipt before NSButton ordinal 1 → choice 0, independent exact
readback, one edit, duplicate refusal and no extra provider submission. Singleton
images were inspected. New explicit `--owned-emoji` passed two real MacBridge
replacements in a separate authenticated AppKit child with genuinely read-only
AX selected text and writable range/value. Astral/NFD prefix/suffix and exact
UTF-8/UTF-16 text/caret survived synthetic `:coder:` → `👩‍💻` and `:-)` → `🙂`;
each had one marked PID-targeted event pair and one native text change.
`:coder:` is a backend span, not a public-shortcode claim. Generic/stale replay,
second-field capability, read-only/foreign-window and secure-field boundaries
were exercised; close/stop was clean. Original `--owned-ax` remains no-keyboard-
posting; the new explicit mode alone permits owned-child events and refuses
physical/foreign input before Unicode inspection, without model/global hooks or
permission requests.

Retained negatives: initial choices stopped on physical input with zero edits/
requests; initial dedicated emoji failed on a fixture typed-CGEvent ABI mismatch
before completion. Fresh runs passed after using the correct typed API (no
unchecked message send) and correcting the secondary-field assertion to
capability refusal, without weakened guards/deadlines. Live WhatsApp evidence
remains approved **nonmutating metadata only**. Neither controlled mode proves
WhatsApp mutation, physical typing/clicks, undo, full host/platform acceptance or
release approval. Quality remains 178/192 with 14 no-offers and 15 held gates.

| ID | Current source correction / exact regression seam | Evidence level and remaining acceptance |
|---|---|---|
| R1 | `platform/macos.rs`: optional Secure Input detector result; `runtime.rs`: `Engine::capture` refuses anything except known false; `unknown_security_never_reads_text_and_history_unlock_failure_does_not_save_consent`. | **Source correction + deterministic gate.** A observed Secure Input false but did not toggle secure fields or remove the detector. Actual unavailable-detector/Secure Input and signed-host/TCC acceptance remain open; the AX pass is not that proof. |
| R2 | `models.rs`: Windows `measure_physical_memory` uses `GlobalMemoryStatusEx` and returns unknown on failure/zero; `model_availability` preserves unknown; `capacity_policy_distinguishes_unknown_and_low_memory`, `ram_labels_use_binary_gibibytes_and_preserve_fractions`, `fractional_memory_requirements_keep_byte_exact_eligibility`. | **Source correction + policy tests.** [RAM-unit evidence](../../evidence/rust-migration/ram-units-current/receipt.json) records common allocation-free two-decimal binary GiB formatting for RAM labels and insufficient-memory availability warnings. Positive fractions are not integer-truncated (8.50 GiB, 0.50 GiB; 1,000,000,000 bytes → 0.93 GiB). Zero/missing catalogue minimum remains unspecified: Mac omits it, portable surfaces show unavailable. Physical RAM remains separate and unknown hardware stays unknown; byte-exact eligibility, capacity detection and download/transfer formatting remain unchanged, with no hardware-fit promise. Mac render-only 86 captures/117 geometry observations and Linux's same 99 native assertions do not exercise Windows memory discovery or rendering. The [historical catalogue-memory receipt](../../evidence/rust-migration/catalog-memory-current/receipt.json) preserves its actual pre-unit-fix 8.6 GB Mac observation; earlier raw-byte labels are historical. [Historical Windows build](../../evidence/rust-migration/windows-build-completion.json) is genuine native execution at its recorded revision, without a failing-memory-API scenario. Current Windows/MSVC/desktop unavailable; Mac/Linux tests do not exercise this Windows API. |
| R3 | `features.rs`: `safe_choice`/`parse_choices` validate decoded strings for Cc/Cf/Zl/Zp and structural markers; `decoded_utility_rejects_lines_formats_protocols_and_allows_abstention`. | **Source correction + explicit hostile vectors.** Escaped separators, bidi controls and role/think markers are covered by the regression seam. C proves benign exact choices, not hostile native insertion. No remaining source defect established; no dedicated hostile-choice native receipt. |
| R4 | `crates/xtask/src/common.rs`: promotion keeps old build in destination sibling, outside disposable staging; `replacement_failure_restores_previous`, `failed_restore_survives_stage_cleanup`, `panic_between_renames_retains_old_sibling`. | **Source correction + owned filesystem regression seams.** Aggregate workspace gate supports bounded developer-promotion correction. [Historical Mac installation](../../evidence/rust-migration/macos-0.5.0-release-verification.json) has a different updater/process scope; neither proves current developer hard-kill/power-loss durability. Do not conflate this baseline builder defect with OTA rollback. |
| R5 | `core.rs`: snapshot authority separate from text/policy key; runtime lease checks include both; `window_only_change_rejects_output_and_old_presentation_ack`. | **Source correction + bounded native observations.** C invalidates on owned field/focus/selection/suffix/scroll/destruction with zero edit/acceptance deltas; A rejects stale/foreign native authority. Neither proves a real external browser document-change/in-flight-result race. Physical external-host presentation invalidation remains open. |
| R6 | `runtime/native_client.rs`: `finish_dismissal` records actual tool mode and authority even for bypass; `runtime.rs`: `correction_dismissal_and_replacement_require_current_authority` and emoji passthrough regression. | **Source correction + emoji/correction seams.** No dedicated synonym-dismissal/bypass/no-resurrection execution matrix was located. C completion dismissal is not interchangeable with that coverage. This is a targeted remaining acceptance gap, not a claim that the corrected code still resurrects tools. |
| R7 | `runtime.rs`: `publish_pending_completion` restores owned request while spelling is pending; `final_completion_waits_for_pending_spelling_instead_of_losing_explicit_force`. | **Source correction + late-verdict regression.** [Actual NSSpellChecker evidence](../../evidence/rust-migration/native-dictionary-current.json) passes 22 structural checks (en-GB/pt-BR; exact en-US unavailable), but does not exercise asynchronous service/final-result handoff. C/M disabled typo checks. Native pending-dictionary integration remains open. |
| R8 | `platform/mod.rs`: `EditOutcome::{Applied,Dispatched,Rejected,Indeterminate}`, `replace_emoji_range`; `core.rs`: `matches_pending_emoji_selection`; Mac classifier and permanent runtime/native partial-mutation/readback regressions; canonical Windows decisions unchanged. | **Source correction + actual controlled Mac component proof + portable decision tests.** [New owned emoji receipt](../../evidence/rust-migration/tab-emoji-current/receipt.json) proves two PID-targeted, one-edit replacements against actual read-only-selected-text AX capability and exact independent text/caret. Post-selection uncertainty is Indeterminate/no replay; dispatched exact selected-only state is pending, never Applied before final readback. Original A stays no-post, C adds actual chooser acceptance; retained negative attempts are scoped above. [Twelve portable Windows tests](../../evidence/rust-migration/portable-windows-decisions-current.json) remain distinct from native UIA/SendInput execution. Actual WhatsApp mutation, physical/undo/permission-race and original native Windows rejection/readback/SendInput acceptance remain open. |
| R9 | `platform/windows_hook.rs`: injected-edge refusal and consumed-release suppression survive policy disable; canonical `platform/windows_decisions.rs`: `actual_hook_decision_preserves_altgr_injection_os_switching_and_exact_modifiers`, `consumed_press_repeat_and_release_remain_paired_after_disable`. | **Source correction + executed portable decision tests.** [All 12 pure Windows tests](../../evidence/rust-migration/portable-windows-decisions-current.json) compile/pass on Mac and Linux under `cfg(any(target_os = "windows", test))`; earlier host gates excluded this module. One canonical implementation serves native Windows and tests, without duplicated policy. [Target-owned clients](../../evidence/rust-migration/target-owned-client-acceptance.json) demonstrate analogous Qt release ownership, not Windows hooks. Current native Windows hook lifecycle, physical press/repeat/release/disable/AltGr/injection and held-key cleanup acceptance remain open. |
| R10 | `platform/wayland.rs`: `capture_native`/`confirm` force `native_inline=false`; ordinary preedit send/claim route removed. `core.rs`: `manual_native_anchor_does_not_grant_target_owned_inline_capability`; `runtime.rs`: `manual_transport_without_inline_capability_refuses_ordinary_completion`, `manual_native_choices_without_inline_keep_dense_ordinals_and_confirmed_insertion`, `target_owned_receipt_cannot_authorize_manual_transport_acceptance`. | **Scout discrepancy corrected, not outstanding.** Ordinary inline/preedit is retired without popup fallback; manual panels/confirmed insertion and target-owned clients remain separate. Full Mac/Linux source gates passed; read-only safety review found no findings. [Actual KWin emulation](../../evidence/rust-migration/emulated-linux-current/receipt.json) observes compositor/private-socket/protocol activity, but strict exit 2 leaves identity-qualified product binding false; see reconciliation boundary below. Full KWin manual/physical acceptance remains unmet. [Historical KDE receipt](../../evidence/rust-migration/gamer-pc-0.5.0-deployment.json) lacked the private socket; old Python smoke remains historical. |
| R11 | `providers.rs`: `UTILITY_GRAMMAR` permits `[]` or 3–5 strings and `stream_one` installs it; `features.rs`: `decoded_utility_rejects_lines_formats_protocols_and_allows_abstention`. | **Source contradiction removed + actual native grammar admission.** [Ten fixed native-vocabulary/token-mask cases](../../evidence/rust-migration/utility-grammar-admission-current.json) pass, including `[]` and 3–5 strings, rejecting invalid counts/incomplete/trailing data. No inference/context was created: this proves grammar admission, not model-generated abstention or semantic quality. [Real-grammar shortlist parity](../../evidence/rust-migration/grammar-shortlist-parity-current.json) passes 114 units/456 trajectories; [post-NFC calibration](../../evidence/rust-migration/nfc-cache-calibration-current.json) passes 55 prompts/all three gate classes. [Pre-NFC full64 preservation](../../evidence/rust-migration/writing-full-corpus-parity-current.json) retains its 175/192 baseline; current [NFC healing](../../evidence/rust-migration/nfc-healing-current.json) reaches 178/192 with 14 unchanged no-offers. No fabricated choices or absolute quality pass inferred. |
| R12 | `cli.rs`: `valid_pane("models")` admitted on Mac and portable branches; `ui/macos_settings.rs`: Models pane; `ui/linux_settings.rs`: show children before first GtkStack selection. | **Parser correction plus actual Linux CLI acceptance.** [Source/binary-bound receipt and reviewed PNG](../../evidence/rust-migration/linux-cli-models-current/receipt.json) prove `--settings --pane models --paused --no-tray` opens AI Models, native AT-SPI Quit exits 0 without lingering children, settings remain byte-identical and pause is memory-only. No model inference. The separately fixed [duplicate D-Bus loader hazard](../../evidence/rust-migration/linux-dbus-linkage-current.json) changes exports/system bindings 536/126 → 0/0 using system libdbus, no package upgrades (CI development dependency already present). Current Mac/Windows launch, full shell and release gates remain open. |
| R13 | `platform/macos_terminal.rs`: `DIAGNOSTICS` allowlist, fixed `native-input-unavailable` fallback and `&'static str` diagnostic field; `diagnostics_never_publish_native_text`. | **Source correction + bounded membership test.** Arbitrary conversion errors are not exported by inspected branches. The regression excludes a private sentinel but is not malformed-native-error injection through actual inspection/read paths. Live signed-terminal failure diagnostics remain unexercised. |

### Reconciliation boundary

All P01–P32 and R1–R13 now have individual current dispositions, with the bounded
P05/P08/P09 receipts retained once in the parity register. This closes inventory
reconciliation only. The [current AppKit RAM-unit render evidence](../../evidence/rust-migration/ram-units-current/receipt.json)
records 86 captures and 117 geometry observations, retaining existing panes and
Unicode editor/selection resize scenarios. Catalog availability is an explicit
render fixture, not model-filesystem availability or loading; no Controller,
services, saved-profile inspection or actions run. Native NSView bitmaps do not
establish full shell persistence, runtime controls, physical acceptance or
WindowServer compositing. The earlier [78-capture/107-geometry receipt](../../evidence/rust-migration/native-settings-current/receipt.json)
and its six reviewed images remain historical.

The separate [current Linux full native-shell receipt](../../evidence/rust-migration/catalog-memory-current/receipt.json)
passes 99 assertions through stage 45 and clean canonical Quit on current source:
six Settings panes/six Welcome steps, actual English word/Portuguese full
preview acceptance with GTK readback, independent consent dialogs, real
model-selection success/failure/cancellation, exact dirty Unicode instructions,
external/live configuration rebase and concurrent updates across panes/resizing,
guarded close/Quit Cancel, and native persistence failure/recovery. Three added
assertions use the actual Models dialog/runtime catalog: unspecified memory is
not numeric zero, positive memory remains visible, and browsing preserves
runtime/saved configuration and Settings draft. No installation fixture, download,
license consent or saved model selection is introduced by these checks.
First-open readiness awaits the mapped window, complete populated catalog and
active row within the existing five-second deadline, without a fixed sleep or
deadline reset. The [earlier 96-assertion receipt](../../evidence/rust-migration/linux-native-shell-latest/receipt.json)
remains historical, with four parent-viewed images:
[EN preview](../../evidence/rust-migration/linux-native-shell-latest/preview-english-visible.png),
[PT acceptance](../../evidence/rust-migration/linux-native-shell-latest/preview-portuguese-accepted.png),
[independent consents](../../evidence/rust-migration/linux-native-shell-latest/privacy-independent-consents.png),
[compacted native choices](../../evidence/rust-migration/linux-native-shell-latest/choices-eligible-subsequence.png).
The [pre-wave shell receipt](../../evidence/rust-migration/linux-native-shell-current/receipt.json)
remains intact. Native UI delivery closes only recorded ARM64 Xvfb/owned-widget
scenarios, not universal platform, physical input, real IME, external hosts,
full Mac shell/Welcome, KWin, Windows, energy or installer/release acceptance.

Additional current evidence retains explicit boundaries:

- [Actual AX reuse](../../evidence/rust-migration/ax-read-reuse-current.json):
  three requests use 3 → 2 native frame acquisitions with identical capture and
  independent widget readback. Reuse ends before fresh appearance/focus/anchor
  barriers. No latency, physical-input or full OS write-confinement claim.
- [Actual notices generation](../../evidence/rust-migration/emoji-notice-current.json):
  Unicode emoji 17.0 and full Unicode License V3 appear alongside preserved
  Cargo/native notices; not packaging or legal certification.
- [Word-tools public-API smoke](../../evidence/rust-migration/word-tools-current.json):
  80 deterministic checks (21 boundaries, 23 emoji, 18 decoded choices/ordinals,
  18 pure acceptance), not model semantics or native insertion. No trigger
  customization adopted.
- [Current target-owned GTK3/Qt6 clients](../../evidence/rust-migration/target-owned-client-current.json):
  compact ordinal 2, GTK pre-paint refusal and Qt selected/collapsed Unicode
  replacement/readback/undo pass with synthetic dispatch, not physical IME.
  [Stock AT-SPI](../../evidence/rust-migration/stock-atspi-current.json) is a
  separate capability: four captures, eight mutation refusals, unchanged
  text/carets and zero insertion attempts/confirmations. Absent inactive-
  composition/inline/range evidence prevents positive insertion authority.
- Windows generic UIA replacement capability is selected-only, not arbitrary
  range movement; collapsed-caret insertion and checked native Edit ranges
  remain supported. Exact selected-readback permits selection-only capability
  loss after collapse only with matching authority, surrounding text and caret.
  [All 12 pure Windows decision tests](../../evidence/rust-migration/portable-windows-decisions-current.json)
  now compile/run on both Mac/Linux through the canonical test-enabled module;
  prior generic runtime readback coverage did not include that module.
  Current native Windows UIA, hooks and lifecycle remain unverified.
- [Catalog memory labels](../../evidence/rust-migration/catalog-memory-current/receipt.json):
  zero/missing means unspecified, not zero consumption. Mac omits the minimum
  line; Linux/Windows use unavailable. Positive values retain their existing
  units (observed E2B: Mac 8.6 GB, Linux 8.00 GiB); physical RAM remains separate,
  backend eligibility is unchanged, and neither labels nor render fixtures
  promise model availability or hardware fit. Windows surface is source-reviewed,
  not executed.
- [Pre-NFC publication frontier](../../evidence/rust-migration/publication-frontier-current.json):
  all 192 texts/mechanical outcomes and 269 native-call outputs/seeds/non-timing
  work records matched at its historical 175/192 absolute baseline. The current
  NFC full64 still records 269 calls, 117 known-terminal emissions before return
  and 211 exact nonempty finish-offer agreements with zero mismatches.
  This is synchronous ordering/byte evidence, not worker/compositor/GPU or
  input-to-paint latency. Historical cancellation/close results retain their scope.

GPUI evaluation is closed without adoption; candidate-only IME, assistive,
resource and all-platform gates remain unmet. Ordinary Wayland inline remains
disabled; independent target-owned clients do not change that restriction.

Current Windows/MSVC/desktop remains unavailable. Genuine KWin 5.27.11 did run
under nested X11/software Mesa in [actual Linux AMD64 emulation](../../evidence/rust-migration/emulated-linux-current/receipt.json).
The native GTK Wayland editor mapped/read back exact NFD/non-BMP text and focus
property true, with `window_active=false`; an owned screenshot was captured.
The compositor-inherited connected AF_UNIX peer was validated and product logs
record input-method/input-panel bind requests and activation. Strict probe
**exit 2 / partial** remains after the fix, with 10/13 gates passing. D-Bus
`ListNames` returned no `org.kde.KWin*` names; executable observations identified
the Rosetta interpreter despite matching process start ticks. Exact D-Bus identity,
product executable identity and identity-qualified binding remain false.
Raw requests cannot be promoted to qualified acceptance. Logs record
missing EGLImage pixmap support/compositing disabled and KDE decoration/QML
modules. No model loaded/inference ran; forced process cleanup is not canonical
Quit/release proof. Corrected UP-interface/no-IPv4/IPv6-route guards passed;
the initial down-tunnel helper refusal remains historical. Full manual commit,
physical IME and normal KDE selection remain unmet; all 15 gates stay held.
Neither ARM64 Xvfb nor actual AMD64 emulation proves an x86-64 installer.
The real pre-cleanup probe found a production bug, not merely an environment
limitation: grab → deactivate → `keyboard.release` caused invalid-object-11
`wl_display.error`. `wayland_wire.rs::drop_keyboard` now destroys the local
version-1 proxy only, while retaining owning-context teardown. Actual post-fix
execution observes grab → deactivate → context destroy, no release, no protocol
errors. Both product and KWin logs change from one pre-cleanup/zero during-cleanup
errors to zero/zero; the complete product log remains. The new
no-pre-cleanup-protocol-error gate passes without suppressing an exception.
Post-fix Linux locked/offline serial all-target tests and bins/examples again
passed (693 parent + six child = 699 reported, 27 summaries, two ignored);
formatting passed in the parent run. The X11 99-assertion native-shell pass
preceded this Wayland-only fix; X11/UI source is unchanged, not rerun evidence.
Signed/clean-commit candidate, prior-version upgrade,
current Tiny, physical host/input, TCC/grant continuity and disposable full Mac
shell gates remain missing. [Windows installer ownership](../../evidence/rust-migration/windows-direct-client-ownership.json)
remains failed/held. Historical Mac installation/release and Windows receipts
remain historical, including [published 0.5.4](../../evidence/rust-migration/macos-0.5.4-release-verification.json);
none authorizes a new install or publication. Original v19/v22 production prompt
layout remains; NFC replay bytes change. [Six rejected layout/cache candidates plus the fixed reminder](../../evidence/rust-migration/prompt-cache-ablation-observations.json)
retain distinct work/quality reasons. Learned API-call savings do not reduce
decoded work or establish CPU-time benefit; reference freezing regresses grounding.
The [pre-NFC calibration](../../evidence/rust-migration/cache-schedule-calibration-current.json)
passed all three gate classes over 55 prompts, and the
[updated original live-reuse smoke](../../evidence/rust-migration/live-cache-reuse-calibrated-current.json)
passes. [Calibrated saved-policy evidence](../../evidence/rust-migration/cache-policy-calibrated-current.json)
still fails 23/80 admitted checkpoint cases; all four arms fail exact/numeric
but pass decisions, with three independent bit-exact live controls. Physical
layout as the cause is [INFERENCE], not proved; no ordinary KV defect, default
change or performance claim follows.
[Pre-NFC full64 matched evidence](../../evidence/rust-migration/writing-full-corpus-parity-current.json)
retains 192 matching texts/mechanical outcomes and native-call/seed counts, with
both arms at **175/192 absolute cases**, including three missing-`o` NFD joins.
Its cancellation results are historical, not current worker acceptance.

The accepted [NFC model-replay correction](../../evidence/rust-migration/nfc-healing-current.json)
fixes that actionable bug without changing exact original NFD authored bytes,
reference or suffix. Only model replay is normalized, consistently across grammar,
tokenizer-derived replay allowance and stripping. It adds no whitespace prohibition,
language special case, prompt-layout rearrangement or preset.
Current full64 is **178/192**: exactly three target improvements, 189 unchanged
texts/mechanical outcomes, zero regressions. All 52 long required outputs,
non-timing work and seeds match; four publication callback-count changes are
scoped to that long comparison, not final-output changes. The older long baseline
has different source hashes; it is not an NFC-only source comparison.

[Fourteen unchanged no-offers](../../evidence/rust-migration/writing-no-offer-analysis-current.json)
remain measured model abstentions: ten replay-only stops and four replay-plus-comma
stops, not execution errors or useful insertions lost to cleanup. They remain
absolute-quality failures, not an external blocker or a claim of unfixability.
No further production bug was justified; cases and required outputs were not weakened.
[Post-NFC cache calibration](../../evidence/rust-migration/nfc-cache-calibration-current.json)
passes all 55 prompts and three gate classes. Direct healing passes 3/3, and
[historical short-probe worker healing targets](../../evidence/rust-migration/nfc-worker-healing-current.json)
pass 3/3 with protocol completion and acknowledged shutdown. That old command
retains exit 1 and three already-completed, inconclusive after-result probes.
Before-proceed passed 3/3; no later outputs were observed and raw child exits
remain unobserved. Current corrected-clock evidence below supersedes this
cancellation observation without rewriting the historical failure.

Current Linux ARM64 all-target tests/build and the current canonical native shell
(99 assertions, stage 45, clean exit) passed on corrected source; earlier
96-assertion receipts retain their historical scopes. The catalog-label and
portable-module changes leave the accepted NFC inference implementation and
measured results unchanged. Completed implementation and this fixed bug do not
close the 15 held acceptance gates, calibrate the now-attached assistant semantic
labels, remove platform release holds or establish overall roadmap acceptance.

### Current follow-up review register (2026-11-07)

- [Canonical cancellation](../../evidence/rust-migration/canonical-cancellation-current.json):
  the corrected-clock report records parent exit 0, both probes passing in three
  sessions, three NFD target passes and acknowledged release/shutdown. A fixed
  ordinary garden-handover after-result probe alone uses 128 tokens/32 words;
  ordinary cases retain 24 tokens/medium length. Each observes a real nonempty
  nonfinal IPC result while the matching supervisor request is active. This closes
  only the observed supervisor active-window/accounting/quiescence/retirement
  contract. Native acknowledgement/interruption remain null; consumer silence is
  not producer stopping. Diagnostic timestamps now follow actual observation/
  retention after lock waits instead of step-entry time; operational grace/idle,
  ownership and accounting are unchanged. Both clock regressions passed within
  the Mac source gate: 732 parent + five child = 737 reported passes, 26 summaries,
  one ignored, with binaries/examples built. The old short-probe exit 1 stays historical.
- [Boundary/replay experiment](../../evidence/rust-migration/no-offer-boundary-replay-current.json):
  reject no-healing adoption. Both arms completed 54 requests with zero stream/
  projection errors and 19 no-offers. Original target boundaries improve
  14/15 → 1/15, but trailing-space cases regress 0/21 → 18/21; all three protective
  partial-word outputs split `feedback` into `feed back`. Full authored prefill
  removes replay metadata/grammar/stripping/reservation together, not grammar
  alone. The synthetic-context 8,000-byte selector and cold KV-cleared schedule
  differ from retained full64; Japanese ASCII space is diagnostic only.
  Production remains unchanged; no full64/long/per-keystroke adoption gate is claimed.
- [Assistant semantic review](../../evidence/rust-migration/semantic-review-current.json):
  canonical offline attachment covers 89 unique input/output IDs/all 192
  occurrences, with 337 labels (277 acceptable, 25 unacceptable, 11 uncertain,
  24 not applicable). Generated/mechanical/native/timing fields and original
  **178/192 with 14 no-offer failures remain unchanged**. The blind second review
  selects 29 flagged IDs plus six controls: 143 agreements and 13 unadjudicated
  disagreements across 156 dimensions. Preserve both review sets, not majority
  voting. Same-model, selection-biased assistant review is not human validation,
  population calibration or an automatic overall score. Agreed defects include
  Portuguese `sala` in English, unsupported `next week`, and `ágiles` rather than
  Portuguese `ágeis`; lexical-join/grammar attribution and utility remain disputed.
- [Actual Linux AMD64 emulation](../../evidence/rust-migration/emulated-linux-current/receipt.json):
  real x86-64 native full-shell execution under Xvfb/Openbox/Picom passed 99
  assertions, stage 45, clean exit, no error. Serial locked/offline all-target
  tests plus bins/examples passed: 693 parent + six child = 699 reported passes,
  27 summaries, two ignored. The initial concurrent library run retains 568
  passes, three EAGAIN/eight-second UnixStream fixture read timeouts and one ignored;
  deadlines/assertions were unchanged and cgroup PID/memory/OOM events were zero.
  Do not describe these as spawn/PID/OOM failures. Source tests and emulated UI
  are distinct; neither closes native Windows, physical typing, installer or release
  holds. **All 15 acceptance gates remain held**; ordinary Wayland inline/preedit
  remains disabled.
