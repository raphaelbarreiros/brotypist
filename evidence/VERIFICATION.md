> Historical record: Named third-party research references were removed during repository-history cleanup. This edited historical record is not an original authenticated receipt. Recorded build identifiers and measurements describe their original observations.

# Observed verification — 0.3.0 — 2026-10-24

Environment: macOS 27.2 arm64, Apple M1 Pro, CPython 3.12.14, PyObjC 12.2.2, llama-cpp-python 0.3.36. Independent Qwen2.5-1.5B-Instruct Q4_K_M weights, not [redacted reference]'s model. All writing in screenshots/tests is synthetic. No personal document or actual password was read.

## Actual native settings, not an editor mockup

Source AppKit and the **built PyInstaller application** were launched with disposable `INKLOW_HOME` profiles. The source smoke also verified that neither `llama_cpp` nor PySide6 was imported while native preferences were idle. Qt is absent from the Mac dependency/bundle path.

Real process-scoped accessibility interactions exercised all eleven panes, persisted a General switch, saved/reverted custom instructions, recorded a native Control+F6 shortcut and restored Tab, created/deleted a synthetic app rule, expanded model management and selected an installed model, closed preferences while the agent remained alive, and quit through the native menu with exit code 0.

Receipts: [source preferences](menu-first-native-ui.json), [frozen preferences](menu-first-frozen-ui.json), [first-install model selection](menu-first-new-install.json). The empty-cache scenario preserved an uninstalled preset across opening model management and exposed an enabled Download action without activating missing weights or downloading anything.

The installed status-menu button was also opened through its native accessibility action. Its real five-minute Pause submenu action changed the visible entry to Resume Inklow; native Quit exited 0. [Menu screenshot](menu-first-menu.png) and [interaction receipt](menu-first-menu.json). macOS returned `kAXErrorCannotComplete` for the opening AXPress even though the menu actually opened, so the smoke verified the resulting menu state rather than treating that return code as proof of failure.

Visual proof: [General](menu-first-general.png), [Context](menu-first-context.png), [Personalization](menu-first-personalization.png), [Emoji](menu-first-emoji.png), [Shortcuts](menu-first-shortcuts.png), [Battery](menu-first-battery.png), [App Settings](menu-first-appSettings.png), [Labs](menu-first-labs.png), [Statistics](menu-first-statistics.png), [Setup](menu-first-setup.png), [About](menu-first-about.png), [expanded model management](menu-first-model-management.png). These are real native controls, not an HTML design. The preferences screenshots were refreshed from the frozen app.

## Complete real global-typing path

A separate real AppKit `NSTextView`/`NSSecureTextField` process supplied disposable targets. The production controller, AX capture, Quartz interception, isolated inference worker, and native overlay ran together. A test-only PID guard restricted capture to that synthetic process; native insertion and the actual model were not mocked. No other application was eligible.

Observed:

1. Model absent at initial agent status; real inference started only for the eligible target.
2. English continuation ` your feedback.` appeared as native inline ghost text while the target retained focus and its document remained unchanged.
3. A real Tab inserted exactly ` your `; the physical ISO above-Tab key accepted the remaining `feedback.`. Final text: `Thank you for taking the time to review the proposal. I look forward to your feedback.`
4. A new Portuguese prefix reused the same inference process and produced ` visita.`.
5. Focusing the secure field hid the overlay; inactivity terminated the model child. The test used a three-second timeout; the shipped default is sixty seconds.
6. Offline `That is :smile` offered a numbered emoji chooser; actual number-key acceptance produced `That is 😄` without loading a model for emoji lookup.
7. Selecting `excellent` produced actual local-model choices `superior`, `outstanding`, `great`, `remarkable`, `distinguished`. Number-key acceptance changed only the selection: `The result is superior.`
8. Pause released the inference worker and hid the overlay. Confirmed-insertion statistics were three completions, three words, twenty-four characters; generated-but-unaccepted Portuguese output was not counted.
9. Agent quit exited 0. The disposable target was then closed.

Receipt: [native typing, timing, memory, statistics](menu-first-typing-smoke.json). Actual surfaces: [inline completion](menu-first-inline-typing.png), [emoji chooser](menu-first-emoji-typing.png), [synonyms](menu-first-synonyms-typing.png).

### Geometry and quality defects found by the smoke

The first inline capture revealed a real macOS 27 `NSTextView` accessibility defect: `AXBoundsForRange(location, 0)` reported a row one line above the native insertion point. A nonempty adjacent glyph range gave the correct row. The adapter now preserves exact zero-range x while using compatible observed glyph-row metrics; it does not apply a guessed constant offset. Empty lines without a reliable glyph row are skipped. A native `firstRectForCharacterRange` comparison and corrected screenshot verified alignment. Supplementary UTF-16 glyph ranges have regression coverage.

The original utility prompt returned `[]` for an ordinary adjective and sometimes suggested changes to the sentence prefix rather than the selection. A separate real-model diagnostic reproduced this. Explicit selection-replacement framing and a generic example corrected the sampled cases. [Raw bounded quality samples](menu-first-quality-samples.json). These samples establish the observed improvement, not semantic correctness for arbitrary writing. No canned/dictionary synonym fallback is shipped.

### Bounded latency and memory samples

| Observation | Seconds |
| --- | ---: |
| Cold first English suggestion, including source-app startup/focus harness | 2.050 |
| First suggestion for a new Portuguese prefix with the same model worker | 0.537 |

These are one observation each from the earlier 0.3 native-typing run, different prefixes, not a controlled benchmark or a speed comparison with [redacted reference]. The warm measurement includes the 200 ms debounce and native polling/UI delivery. Worker release trades lower idle allocations for another cold load. After the worker-protocol rebuild, a separate installed CPU cold-cancellation smoke reached its latest final result at 6.83 seconds; that functional run is also not a benchmark. Do not interpret the earlier two samples as a latency guarantee.

Memory came from the operating system's `proc_pid_rusage(..., RUSAGE_INFO_V4, ...)`, using a small helper compiled against the installed macOS SDK. RSS was independently sampled with `ps`.

| State | Agent physical footprint, MiB | Worker physical footprint, MiB | Worker RSS, MiB |
| --- | ---: | ---: | ---: |
| Active model after acceptance | 62.2 | 246.9 | 1331.4 |
| Idle after worker exit | 62.7 | No process | No process |

RSS includes resident mapped/shared/file-backed pages and is **not** physical footprint. These are source-agent measurements; they are not an apples-to-apples memory comparison against the older editor or [redacted reference]. Worker-process disappearance is the direct proof of releasing its allocations. System/GPU accounting and file caches remain OS-managed.

## Context and privacy checks

A separate source smoke exercised ScreenCaptureKit/Vision OCR against only the disposable target window. The returned text included the synthetic proposal draft. No image was persisted, and screen context remains opt-in. The smoke identified a PyObjC/macOS Vision initializer issue with a bridged Python options dictionary; using the documented nullable options argument succeeded. This is one native target/OS result, not universal OCR coverage.

AES-GCM storage tests use deterministic test keys and exercise encryption, unavailable secure-store failure, deletion, retention policy, vocabulary, and aggregate counts without touching personal Keychain entries. They do not prove that a user's OS secret store is unlocked/configured. Native secure/read-only/composition/domain checks and stale-insertion boundaries have deterministic regression coverage. Independent code review found and prompted fixes for Windows UIA final focus revalidation, emoji/alternatives precedence, Tab policy precedence, portable shortcut recording, and inaccurate history/emoji disclosures.

## Build, frozen inference, and automated checks

Original menu-first suite: **223 passed, 1 skipped, 11.36 seconds**. The input-integration follow-up below extends this to **246 passed, 1 skipped, 11.24 seconds**. The skip is the Qt-only module on macOS, where Qt is intentionally absent. Regression checks include selected short phrases, decomposed Unicode, cancellation at model readiness, initialization/input ordering, and failed-warmup recovery. The Mac application built successfully, rejected private/model payloads, and passed final resource-signature verification.

The actual frozen CLI generated ` message.`. The actual frozen `_worker` protocol streamed ` cooperation.`, processed stop, and exited 0. These outputs prove real inference and IPC, not writing quality. Receipt: [frozen CLI/worker](menu-first-frozen-worker.json). Direct source/settings and frozen settings checks above are separate from unit tests.

GitHub Actions run [40045550783](https://github.com/renan-brasilio/inklow/actions/runs/40045550783) passed **Windows, macOS, and Ubuntu** at source revision `08d878b68030bb9ff841688d1d9715fbf02e1e72`. Each completed regressions, actual source-worker inference, actual settings event-loop launch, wheel/native packaging, frozen CLI/worker inference, and artifact upload. CI uses the checksum-verified Tiny SmolLM2 GGUF; the separate Mac typing/lifecycle checks use Qwen. Windows/Linux GUI smoke is offscreen, not interactive accessibility proof. [Run/stage/native-artifact metadata](menu-first-ci-run.json). Uploaded artifact metadata was observed through GitHub's API; the archives were not independently downloaded/rehashed locally. Earlier green runs in [upgrade-ci-run.json](upgrade-ci-run.json) apply to 0.2, not this revision.

Interactive Windows/Linux accessibility and keyboard compatibility have not been exercised here. Linux has no atomic rewrite operation, so selected-text synonyms and emoji/emoticon rewriting are disabled rather than risking destructive partial edits. Wayland global typing is unavailable. Windows/Linux URL/screen-context limitations are explicit in the README. Passing headless CI is not proof of interactive compatibility.

## Reference evidence and limits

[REA [redacted reference] report]([redacted reference].md), [sanitized native observations]([redacted reference]-menu-reference.json). [redacted reference]'s settings were observed directly, but no live accepted [redacted reference] prediction, exact prompt, performance comparison, or network-privacy audit was established. The implementation is independent and does not claim proprietary feature/model equivalence.

Historical 0.2 evidence remains in [UPGRADE-0.2.md](UPGRADE-0.2.md); older CI and image artifacts remain historical, not descriptions of the removed editor's current availability.

## Original side-by-side installation and document protection (historical)

The current verified bundle was copied to `/Applications/Inklow 0.3.app` and passed `codesign --verify --deep --strict`. Its executable SHA-256 matches the verified `dist` executable: `f678c0111ebac6f02da53fcfd1591d0c78b933d5513e10f61f004690e0b484f5`. An earlier installed 0.3 build exercised all eleven native preference-pane/control/close/quit interactions using a disposable paused profile and exited 0: [installed UI receipt](menu-first-installed-ui.json). After the worker-protocol rebuild, the current installed copy displayed its real General window and exited 0 through normal native application termination: [current installed screenshot](menu-first-installed-current.png). This latest visual smoke used only a disposable paused profile and captured only its own window.

Normal Quit of the original `/Applications/Inklow.app` exposed its unsaved-writing protection. Quit was cancelled; document values were never read, and that running app/bundle was not replaced. Save and quit the original editor before normal use of the new build; the two versions share their application identity and default settings. [Safe installation receipt](menu-first-installed-app.json).

## Portable settings correction

The first hosted run passed the complete Mac pipeline but found a real Windows/Linux launch failure: a shortcut-controls dictionary shadowed the dynamically dispatched `_shortcuts` pane builder. The field was renamed and its consumers migrated. A separate actual Qt offscreen launch then constructed all eleven panes and saved a real preference using an isolated profile: [portable smoke](menu-first-portable-smoke.json). This local Qt check ran in a temporary dependency overlay on macOS, not an interactive Linux/Windows desktop, and did not add Qt back to the Mac app.

The next hosted run passed both portable regression suites and direct inference but waited in the no-tray quit-confirmation dialog: the smoke used `QApplication.quit()` instead of the application's guarded Quit action. The smoke now checks that untouched settings are clean and calls the real menu-agent Quit path. That actual Qt event loop exited 0 in 2.70 seconds in the local dependency overlay; the close-confirmation safeguard was not disabled. The hosted GUI smoke step is bounded to one minute.

## Native worker initialization and cancellation

Hosted run [40041926809](https://github.com/renan-brasilio/inklow/actions/runs/40041926809) completed the Mac and Ubuntu pipelines, including source/frozen inference and actual preferences launch. Windows instead remained in `loading` until the 120-second source-worker smoke deadline; the child had not exited. This localized the failure before prompt evaluation, not solely to freezing. Linux's earlier illegal-instruction failure was addressed by source-building the dependency with the documented explicit AVX2/FMA/F16C baseline rather than reusing a host-native wheel.

The worker now prevents the next blocking stdin read during native GGUF preparation/warmup. It reports ready, then requires the parent's explicit per-request `proceed` decision before evaluating a prompt. The parent sends cancellation once, suppresses stale/cancelled readiness, and preserves the warm process for the newest same-model request. A failed warmup closes its partial model and a retry re-enters initialization. A CRT/standard-handle initialization interaction on Windows is **[INFERENCE]**, not a captured native stack diagnosis.

The current installed executable ran the real Qwen model with the production source supervisor: cold cancellation retained the same worker, the newest request returned ` from you soon.`, idle release removed the child, release during a new initialization removed that child, and the parent did not import the model module. Only the executable locator was redirected to the installed bundle; no model/IPC behavior was mocked. [Lifecycle receipt](menu-first-worker-lifecycle.json).

The subsequent complete three-platform run linked above passed Windows source and frozen inference with this protocol. That establishes resolution of the observed Windows loading stall for the CI configuration; it does not independently prove the inferred CRT mechanism or every production-model/desktop combination.

## Input integration and canonical installation follow-up

The current installation is `/Applications/Inklow.app`; the old editor bundle was replaced only after it was no longer running, and the owned `/Applications/Inklow 0.3.app` preview was removed. The current executable SHA-256 is `40320412a6948a9981a193bcf8c423578bddbb980b122fd2f9f5f47fbb9af537`; deep/strict signature verification passed. Saved settings bytes were unchanged by installation; models/history were not removed.

Two implementation gates hid the field accessory: it required nonempty text and a usable caret rectangle. The badge now uses native field geometry independently of prediction eligibility and stays at the field's left edge. Native caret geometry still permits inline text. Where only field bounds are reliable, a real field-attached suggestion card permits explicit acceptance with the same focus/protection/exact-snapshot guards. No caret coordinates are invented. Tern's observed character-range bounds had zero height, while its focused-element bounds were nondegenerate. That diagnosis read metadata/geometry only, not personal input values.

The **actual installed frozen GUI**, not a source controller, ran with an isolated profile allowing only a disposable native input application. A real empty field showed the muted wordmark. Actual Qwen inference produced an inline completion; real Tab changed `I look forward to hearing` to `I look forward to hearing from `. A native text-field subclass with unavailable character-range bounds showed a field-attached completion card; real Tab changed `Thank you for your` to `Thank you for your time`. Focusing the secure/read-only fixture controls hid overlays. The foreground PID was checked before key injection and each scoped screenshot. [Empty field](input-empty-badge.png), [native inline completion](input-native-suggestion.png), [field-only completion card](input-field-card.png).

**This direct-executable GUI smoke inherited its terminal host's permission. It is not normal-launch authorization proof.** A separate `open -W -n -a /Applications/Inklow.app … --args doctor` run under LaunchServices reported Accessibility denied despite global enablement and valid model weights. Earlier terminal-launched results did not establish that Finder-launched Inklow was authorized. No TCC grant was automated. Normal-launch typing verification remains blocked until the user grants the current Inklow app Accessibility (and Input Monitoring for shortcuts) and restarts it. The native menu now explicitly identifies missing required permissions instead of only displaying idle.

[Observed installation/input/permission receipt](input-integration-repair.json). Source revision: `b39cd0a46dda37c30479d616219fa02323df9249`. [Follow-up three-platform CI](https://github.com/renan-brasilio/inklow/actions/runs/40071249762) was running when this section was written; do not substitute the earlier green run for this revision's result.
