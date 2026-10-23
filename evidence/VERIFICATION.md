> Historical record: Named third-party research references were removed during repository-history cleanup. This edited historical record is not an original authenticated receipt. Recorded build identifiers and measurements describe their original observations.

# Observed verification — 0.3.0 — 2026-10-24

Environment: macOS 27.2 arm64, Apple M1 Pro, CPython 3.12.14, PyObjC 12.2.2, llama-cpp-python 0.3.36. Independent Qwen2.5-1.5B-Instruct Q4_K_M weights, not [redacted reference]'s model. All writing in screenshots/tests is synthetic. No personal document or actual password was read.

## Actual native settings, not an editor mockup

Source AppKit and the **built PyInstaller application** were launched with disposable `INKLOW_HOME` profiles. The source smoke also verified that neither `llama_cpp` nor PySide6 was imported while native preferences were idle. Qt is absent from the Mac dependency/bundle path.

Real process-scoped accessibility interactions exercised all eleven panes, persisted a General switch, saved/reverted custom instructions, recorded a native Control+F6 shortcut and restored Tab, created/deleted a synthetic app rule, expanded model management and selected an installed model, closed preferences while the agent remained alive, and quit through the native menu with exit code 0.

Receipts: [source preferences](menu-first-native-ui.json), [frozen preferences](menu-first-frozen-ui.json), [first-install model selection](menu-first-new-install.json). The empty-cache scenario preserved an uninstalled preset across opening model management and exposed an enabled Download action without activating missing weights or downloading anything.

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

These are one observation each, different prefixes, not a controlled benchmark or a speed comparison with [redacted reference]. The warm measurement includes the 200 ms debounce and native polling/UI delivery. Worker release trades lower idle allocations for another cold load.

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

Local final suite: **220 passed, 1 skipped, 10.16 seconds**. The skip is the Qt-only module on macOS, where Qt is intentionally absent. Regression checks include selected short phrases and decomposed Unicode. The Mac application built successfully, rejected private/model payloads, and passed final resource-signature verification.

The actual frozen CLI generated ` message.`. The actual frozen `_worker` protocol streamed ` cooperation.`, processed stop, and exited 0. These outputs prove real inference and IPC, not writing quality. Receipt: [frozen CLI/worker](menu-first-frozen-worker.json). Direct source/settings and frozen settings checks above are separate from unit tests.

The GitHub Actions matrix defines Windows/macOS/Ubuntu installation, tests, actual source settings launch, verified Tiny download, CPU inference, wheel/native packaging, and frozen CLI/worker smoke. Current-run results are recorded separately when observed; earlier green runs in [upgrade-ci-run.json](upgrade-ci-run.json) apply to 0.2, not this revision.

Interactive Windows/Linux accessibility and keyboard compatibility have not been exercised here. Linux has no atomic rewrite operation, so selected-text synonyms and emoji/emoticon rewriting are disabled rather than risking destructive partial edits. Wayland global typing is unavailable. Windows/Linux URL/screen-context limitations are explicit in the README. Passing headless CI is not proof of interactive compatibility.

## Reference evidence and limits

[REA [redacted reference] report]([redacted reference].md), [sanitized native observations]([redacted reference]-menu-reference.json). [redacted reference]'s settings were observed directly, but no live accepted [redacted reference] prediction, exact prompt, performance comparison, or network-privacy audit was established. The implementation is independent and does not claim proprietary feature/model equivalence.

Historical 0.2 evidence remains in [UPGRADE-0.2.md](UPGRADE-0.2.md); older CI and image artifacts remain historical, not descriptions of the removed editor's current availability.
