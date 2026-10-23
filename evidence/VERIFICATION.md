> Historical record: Named third-party research references were removed during repository-history cleanup. This edited historical record is not an original authenticated receipt. Recorded build identifiers and measurements describe their original observations.

# Observed verification — 2026-10-23

Environment: macOS arm64, CPython 3.12.14, PySide6 installed by uv, llama-cpp-python 0.3.36. Real SmolLM2-135M Q4_K_M weights, pinned revision and SHA-256 verified by `inklow model download`. No cloud API, mocked LLM, or proprietary [redacted reference] model was used.

## Local model execution

`printf 'The capital of France is' | .venv/bin/inklow complete` executed real inference and produced ` the capital of France.` This repetitive answer is evidence of execution, not evidence of factual quality.

A warm provider produced:

| Typed prefix | Actual continuation | Elapsed seconds |
| --- | --- | --- |
| Thank you for your | ` interest in the` | 0.368 |
| I look forward to hearing | ` from you."` | 0.020 |
| The capital of France is | ` the capital of France.` | 0.069 |

These are three local samples, including one cold model initialization—not a benchmark or quality guarantee.

## Actual desktop surface

A disposable Qt driver launched `InklowWindow` on the real macOS Qt backend, typed a synthetic prompt, waited for real worker inference, and sent acceptance/dismissal/input-method events.

Observed passing outcomes:

- Model suggestion ` interest in the` after `Thank you for your`.
- Document unchanged while ghost text was visible.
- Tab inserted exactly ` interest `; Shift+Tab accepted the remainder.
- Undo reverted each accepted insertion independently.
- Escape cleared the suggestion and started the three-second pause.
- An IME preedit event suppressed predictions; committed Unicode text remained correct.
- Settings rejected a remote inference URL without saving it.
- Worker/model resources shut down cleanly.

Actual window capture: [inklow-editor.png](inklow-editor.png). Screenshots contain only synthetic test text.

The initial driver starved Python worker execution by repeatedly using `QTest.qWait`; stack diagnostics showed imports waiting in the inference thread. Replacing that test-driver wait with a short GIL-releasing sleep while pumping Qt events resolved the harness issue; no production workaround was added.

## Native API and keyboard smoke

A separate disposable Qt process supplied real editable and password fields. The production macOS bridge performed cross-process accessibility capture and native insertion.

Observed passing outcomes:

- Supplementary Unicode caret converted correctly: 25 Python codepoints versus 26 UTF-16 units.
- Accepted text inserted into the original field without replacing its existing content.
- Reusing the old snapshot refused insertion.
- Password field refused, including Qt's secure-role-description variant with no AXSubrole.
- Noncollapsed selection was not overwritten.
- Real Quartz Tab interception invoked accepted insertion.

## Complete system-wide path

A separate disposable **AppKit NSTextView** process was used to verify the entire production controller path, not merely the adapter:

1. Inklow ran with global mode enabled in memory only.
2. The native target became the foreground app with `Thank you for your` in its field.
3. Production polling, debounce, and real model inference produced ` interest in the`.
4. The nonfocusable overlay became visible while the target retained foreground focus.
5. A real Quartz Tab event inserted exactly ` interest ` into that target field.
6. Native status reported `macOS AX and Quartz ready`.

Actual overlay capture: [inklow-global-overlay.png](inklow-global-overlay.png).

Integration testing found and corrected: system-wide AXFocusedApplication failure despite per-app AX access; tuple-based PyObjC AXValue results; Qt's missing AXSelectedText setter; password state supplied only via localized AXRoleDescription; NSTextView's optional AXEnabled attribute; and delayed tap-disable notifications overriding newer enable state. Fields lacking real caret bounds remain unsupported for global predictions rather than being assigned invented geometry.

The desktop and global scenarios must run serially because they both need foreground focus. A parallel attempt caused focus interference and was discarded, not counted as passing evidence.

## Automated and cross-platform limits

The deterministic suite exercises stale generations, wrong-field acceptance, protected/selected text, Unicode acceptance, endpoint privacy, invalid settings, and prediction sanitization. Final local run: **29 passed in 0.04 seconds**, module compilation succeeded, and `uv build` produced both `inklow-0.1.0.tar.gz` and `inklow-0.1.0-py3-none-any.whl`. `inklow doctor` confirmed the local model exists and persisted global mode remains off. Workflow YAML parsed successfully. A generated-tab regression failed before correction and passed after tabs were normalized to spaces, preventing accepted text from navigating to another form field.

Windows and Linux implementations were reviewed as source, not exercised on interactive desktops here. The GitHub Actions matrix checks installation, safety tests, module syntax, diagnostics, checksum-verified model acquisition, real local inference, and wheel packaging across all three OSes; it does not establish keyboard/accessibility compatibility. Wayland system-wide integration is intentionally unavailable and documented, not represented as complete.

Linux insertion length was checked against the [canonical AT-SPI contract](https://docs.gtk.org/atspi2/method.EditableText.insert_text.html): the insertion position is a character offset, but the length is a UTF-8 byte count. The adapter uses `len(text.encode("utf-8"))` to avoid truncating non-ASCII suggestions. This is documentation-backed API verification, not a live Linux desktop result.

Hosted run [40012515758](https://github.com/renan-brasilio/inklow/actions/runs/40012515758) passed macOS and Windows installation, all safety regressions, real local inference, and wheel packaging. Ubuntu stopped before tests because Qt import required missing `libEGL.so.1`. CI now installs the documented native Qt runtime packages and launches the real desktop offscreen on all OSes. The same offscreen launch command completed locally with exit code 0. The unused pytest-qt plugin was removed; runtime checks remain in place rather than suppressing Qt import errors.

Disposable smoke drivers were removed after verification. No user document, actual password, or personal typing history was used. These observations are bounded by the stated fields/providers and do not establish universal app compatibility.
