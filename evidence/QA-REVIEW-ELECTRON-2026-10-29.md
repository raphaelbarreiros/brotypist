# QA review: latest macOS Electron integration

## Verdict and scope

**Do not describe Discord support as end-to-end verified yet.** The changed native contract file passes repeatedly, but disposable boundary scenarios expose correctness and safety gaps. No implementation, installation, release, CI, or user-app state was changed during this review.

Scope: latest Electron changes identified against `HEAD~1`, principally `src/inklow/native/macos.py` and the three new cases at `tests/test_native_contract.py:1733–1797`; supporting fixtures and the runtime acceptance consumers were inspected. This is a focused change review, not a whole-suite or Linux audit. Existing `evidence/VERIFICATION.md` was preserved, not rewritten.

The graph has zero indexed nodes/edges; its zero risk/gap counts are not evidence of safety. LSP reports no configured servers. No `.agents/qa-project-context.md` or mutation configuration was found by the scoped discovery. Pytest is the declared development framework; mutmut is not installed. No dependency was installed.

## Reviewed-file register

One row per file inspected, including supporting production/configuration sections. Severity refers to findings in the inspected sections, not the entire file.

| File / inspected sections | Severity | Outcome / action | Remediation size |
|---|---|---|---|
| `src/inklow/native/macos.py:442–468,535–751,797–1065,1235–1332` | High | F1–F4: correct UTF-16 mapping, bounded/protected descendant traversal, field-text identity, and replacement capability handling. | Medium; native boundary contract and regression matrix |
| `tests/test_native_contract.py:578–667,752–816,924–1009,1639–1797` | High | F5: add independent behavioral coverage for actual chat bundle activation, multi-leaf geometry, Unicode, secure/cyclic trees, and no-op edit results. | Medium; deterministic fake-host cases plus separate app smoke |
| `src/inklow/runtime.py:1450–1532` | High | F4: replacement callers consume adapter success and record an expected result; they need an honest adapter capability/outcome contract. | Small to medium, depending on adapter outcome design |
| `pyproject.toml:1–37` | Low | Pytest convention identified; no finding requiring a configuration change in this focused review. | None |

## Findings

### F1 — High / P1: descendant offsets are not UTF-16

**Location:** `src/inklow/native/macos.py:701–705,713`.

`_range_bounds` accepts native UTF-16 offsets but increments descendant offsets using `len(text)`. Non-BMP characters consume two UTF-16 units and one Python character. It also concatenates static text without accounting for paragraph separators or proving that the resulting stream matches the field value.

**Executed evidence:** the disposable AX oracle returns valid native bounds for a leaf containing `😀a`, requested at UTF-16 caret 3. Production `_range_bounds` returns `None` without querying that leaf. An ASCII control at caret 3 succeeds. A field `a\nb` represented by leaves `a`, `b` also fails at caret 3.

**Impact:** otherwise supported emoji-containing or multi-paragraph drafts can lose caret geometry, preventing inline suggestions or mapping them to the wrong descendant. These are synthetic native-boundary observations, not a claim that every real Discord tree uses this shape.

**Fix:** build a bounded field-local mapping in UTF-16 units; validate the mapping against the captured field text, including explicit separators/attachments. Reject ambiguous mappings. Parameterize non-BMP, combining, multi-leaf, paragraph, boundary, and cross-node requests. Do not simply replace `len` and assume paragraph identity is solved.

### F2 — High / P1: descendant traversal can be unbounded under the capture lock

**Location:** `src/inklow/native/macos.py:696–716`; caller holds `_lock` at `1214`.

The fallback has no visited set, node/depth budget, or traversal deadline. AX messaging timeouts cannot bound an unlimited number of successful calls.

**Executed evidence:** a synthetic `AXChildren` cycle containing the field itself reaches **1,001 attribute reads**, where the harness—not production—raises its safety stop. The method does not reject or finish before that stop.

**Impact:** a cyclic or very large provider tree can stall capture and other work sharing its lock. This is a demonstrated algorithmic failure on a controlled graph, not measured latency in the installed Discord application.

**Fix:** cycle-safe identity tracking, hard node/depth limits, and an elapsed-time budget; return unavailable on exhaustion. Test cycles and exact limits with a deterministic clock. Existing `focused_context` demonstrates bounded/protected traversal conventions elsewhere in this module, but its context-collection semantics must not be copied blindly into geometry mapping.

### F3 — High / P1: fallback trusts unrelated/protected descendants

**Location:** `src/inklow/native/macos.py:698–715`.

It reads `AXValue` before classifying the node, checks no protected/secure/hidden subtree attributes, and accepts static-text coordinates without proving they describe the captured editable text. Ancestor validation of the focused field does not validate every nested subtree.

**Executed evidence:** a field value `hello` with a descendant value `other` returns the descendant rectangle at caret 5 rather than rejecting the mismatch. A synthetic static-text descendant marked `AXProtectedContent=True` has its value read and its rectangle returned; no protection attribute is queried. No real private content was read by this review.

**Impact:** incorrect anchors and unnecessary reads of nested sensitive content are possible when providers expose mixed or protected descendants. No real-world leak is asserted.

**Fix:** classify and enforce security/visibility/field ownership before reading values; stop at nested editable or secure subtrees; prove exact text/range correspondence before accepting bounds. Add forbidden-read assertions alongside returned-geometry assertions.

### F4 — High / P1: replacement features retain the no-op setter path

**Location:** `src/inklow/native/macos.py:1256–1265,1289–1330`; `src/inklow/runtime.py:1493–1504,1518–1522`.

The Electron append path avoids `AXSelectedText`, but `replace_range` still treats setter return code 0 as a successful replacement. Captured snapshots retain the default replacement capability. Runtime choice/correction consumers then construct and record the expected result without host text confirmation in these paths.

**Executed evidence:** using the existing recording AX fixture with `ChromeAXNodeId` and writable `AXSelectedText`, `replace_range(snapshot, 11, 0, ' works')` returns **True** while field value stays **`hello world`**. The setter records a call and intentionally performs no host mutation. This demonstrates behavior under the same no-op setter condition the new append path is meant to address; it is not live proof of every Discord replacement mode.

**Impact:** replacement-based features can appear accepted without changing the host draft. This is a pre-existing outcome weakness left unresolved by the newly claimed Electron support, not a newly introduced replacement implementation.

**Fix:** advertise replacement unavailable for unsupported controls, or implement a safely revalidated replacement mechanism with an honest outcome contract. Do not retry with synthetic input after an uncertain setter result: that can duplicate edits. Add no-op, rejected, stale-selection, and confirmed-result cases through the adapter and runtime consumer.

### F5 — Medium / P2: the new tests do not constrain the chat integration sufficiently

**Location:** `tests/test_native_contract.py:1733–1797`; related activation cases `790–809`.

The geometry test has one ASCII leaf. The font case replaces `_range_bounds` and uses fake AppKit metrics. The insertion case retains the fixture's app ID `editor`, adds `ChromeAXNodeId`, and asserts a mocked `_post_key` call; it does not model a host applying the event or exercise a Discord app-ID route. Its name says the selected-text setter is a no-op, but routing bypasses that setter entirely. This is useful dispatch coverage, not proof of inserted text or drawn ghost text.

**Mutation evidence:** removing chat-specific accessibility activation, chat app-ID insertion detection, descendant offset advancement, or the descendant finite-coordinate guard leaves all 245 tests green. Five of nine selected faults were caught: **55.6%**. This is a hand-selected changed-path sample, **not** a mutmut/full-project score.

**Fix:** independently controlled fake-host scenarios with expected text, native offsets, forbidden reads, and output geometry; cover exact real chat bundle IDs as well as Chromium-node capability. Keep mocks at native API boundaries. Retain a separate real-app smoke requiring model output → same-line pixels → Tab acceptance → actual field readback. Do not claim this smoke from unit tests.

## Readability

The added geometry setup is small and understandable; no mystery fixture or duplicate-assertion issue was established. Existing helpers explicitly document synthetic native behavior. The insertion test name overstates the host behavior it verifies; rename or strengthen its scenario (F5). Long lambda expressions in the new cases make API behavior harder to scan, but this is secondary to missing behavioral boundaries.

## Reliability

Three executions of the changed test file are green with no observed flakiness. The scoped new tests use no sleep-based synchronization or external services. F2 is an application reliability defect that the suite does not exercise. No claim about whole-suite order independence or installed-app reliability is made.

## Diagnostic

The geometry equality assertion reports concrete expected/actual coordinates. Selected killed mutants produce useful assertion failures. The insertion test reports dispatch calls rather than host state, so a green result cannot distinguish a queued key from an actual edit. F4 requires explicit before/after draft and outcome assertions.

## Design

Boundary-level AX fakes are appropriate here; replacing real system APIs is not itself over-mocking. However, separate tests that stub geometry and key posting leave no test of the combined capture/geometry/insertion contract. Existing `MacBridge.__new__` fixtures bypass initialization, so a future boundary refactor should expose a narrow AX interface rather than add production conditions to accommodate incomplete test doubles. No refactor was applied.

## AI-generated

The implementation and new tests were produced in the same preceding coding session, creating a closed-loop risk. Selected surviving faults demonstrate concrete blind spots rather than relying on authorship alone. No fabricated import or hallucinated DOM locator was established. The font test is explicitly synthetic, not a real-rendering check. Previous completion claims exceeded the available end-to-end proof; this review does not carry those claims forward.

## Coverage

Missing changed-path scenarios: chat-specific enhanced accessibility activation; exact chat app-ID insertion without `ChromeAXNodeId`; multi-leaf UTF-16 mapping; paragraph separators; protected/hidden/nested-editable children; graph cycles and limits; invalid descendant coordinates; uncertain/rejected/no-op host mutations; real prediction timing, ghost pixels, and Tab readback in Discord. Existing unrelated Unicode tests do not cover the new descendant resolver.

## Verification evidence

Command, run three separate times from repository root:

```sh
PYTHONPATH=src /Users/renanrocha/tmp/inklow-release-0.3.0/.venv-release/bin/python \
  -m pytest tests/test_native_contract.py -q --durations=10 \
  --junitxml=/tmp/brotypist-qa-native-runN.xml
```

| Run | Passed | Failed / skipped | JUnit suite seconds | Slowest recorded test seconds |
|---|---:|---:|---:|---:|
| 1 | 245 | 0 / 0 | 0.141 | 0.003 |
| 2 | 245 | 0 / 0 | 0.133 | 0.003 |
| 3 | 245 | 0 / 0 | 0.120 | 0.002 |

JUnit files record per-test timing; pytest's summary rounded these runs to 0.14 / 0.13 / 0.12 seconds. These very short mock-heavy runs do not measure prediction or AX latency.

Disposable harness: `/tmp/brotypist-electron-qa.py`; results: `/tmp/brotypist-electron-qa-results.json`. Invocation:

```sh
/Users/renanrocha/tmp/inklow-release-0.3.0/.venv-release/bin/python /tmp/brotypist-electron-qa.py
```

Each mutant compiles one modified method in memory in a fresh subprocess, then executes all 245 native contract tests. Repository source is never modified. Exit code 1 was counted as killed, 0 as survived; no runner errors occurred. The inspected killed-mutant log contains an assertion failure at `test_native_contract.py:1755`, not a setup/import error.

| Selected mutation | Result |
|---|---|
| Set descendant length to zero | Killed |
| Set descendant relative offset to zero | Killed |
| Stop advancing offset between leaves | Survived |
| Skip descendant traversal | Killed |
| Remove descendant finite-coordinate validation | Survived |
| Remove chat-specific enhanced-accessibility activation | Survived |
| Remove chat app-ID insertion detection | Survived |
| Remove Chromium node-ID insertion detection | Killed |
| Remove fallback for nameless font descriptor | Killed |

**Selected score: 5/9 = 55.6%; 4 survivors; 0 runner errors.** Manual mutation selection limits representativeness. A standard exhaustive mutation-runner score remains unmeasured; mutmut was unavailable and installing third-party tooling was outside this review.

No live Discord/Slack interaction, model generation, screenshot/rendering check, or acceptance smoke was performed in this review. The disposable scenarios exercise the real Python methods against controlled native boundaries only. No change to normal user settings, data, drafts, permissions, or installed applications was made.

## Gates and disposition

No permanent lint gate is warranted by this narrow sample: the demonstrated issues are semantic and native-integration failures, not a reliably lintable syntax pattern. No CI gate or source fix was committed in review-only mode. The hand-selected nine-mutant score is not representative enough to use as a permanent percentage threshold. Before shipping a fix, require the deterministic boundary matrix above and a separate recorded real Discord prediction/render/accept/readback smoke; adopt a stable mutation baseline before enforcing a percentage gate.

Priority: correct/bound the descendant resolver and report replacement capabilities honestly; then strengthen the integration tests and collect real Discord smoke evidence. Passing the existing tests alone is insufficient release evidence.
