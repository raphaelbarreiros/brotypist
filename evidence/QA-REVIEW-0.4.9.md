# BroTypist 0.4.9 — QA review

Date: 2026-10-28. Audited source: `a97161f2153dbf1078b3c71b530200421366143f`.
Mode: **review, not remediation**. macOS and shared typing/learning code; Linux/Windows builds remain held. Only this report and its evidence JSON were added. No production/test code, settings, installed application, GitHub or CI changes.

**Conclusion:** the green suite is useful but does not establish a reliable same-line typing experience. Two current application defects were reproduced in isolated diagnostics: unseen chooser acknowledgement and Unicode-dependent Portuguese learning. The suite also accepted ten independently demonstrated behavior-changing faults.

Machine-readable results: [qa-review-0.4.9.json](brotypist/qa-review-0.4.9.json). All text/data in diagnostics was synthetic. No user chats, normal profile, OS keyring, model weights or physical input were used.

## Scope and conventions

- `ai-qa-review` batch audit plus application testability review. `.agents/qa-project-context.md`, dedicated pytest configuration files and a mutation configuration were absent; conventions came from `pyproject.toml:24,30–31` and the existing suites.
- Inventory: **34 test modules, 663 test definitions, 1,356 collected cases including 22 skips**. The suite mixes deterministic unit and local integration tests without unit/integration/E2E markers; an exact categorical split is not represented and is not invented here.
- Seven primary test modules (~21% of modules), prioritizing the recent native overlay and the two largest suites (`test_runtime.py`, 2,390 lines; `test_native_contract.py`, 1,654 lines). Related targeted cross-checks are recorded in the file ledger. A ledger row refers to reviewed sections, not a claim of exhaustive review of every line.
- Code-review-graph had zero nodes/files and no `Controller` test linkage. Its zero-risk/zero-gap output was discarded, not used as safety evidence. Review used source and execution instead.
- Parent ran verification; three independent read-only reviewers covered native/runtime, writing/learning, and power/updater slices. Their unverified claims were checked against the execution evidence below.

## Executed verification

Each full-suite run used a private `INKLOW_HOME`, a private pytest cache and JUnit output, with the existing Python 3.12.14 / pytest 9.1.1 environment on macOS 27.2 arm64. No new packages were installed.

| Run | Passed | Skipped | Failed/errors | Wall time |
|---|---:|---:|---:|---:|
| 1 | 1,334 | 22 | 0 | 24.666 s |
| 2 | 1,334 | 22 | 0 | 24.085 s |
| 3 | 1,334 | 22 | 0 | 24.131 s |

Per-test timings were captured for every case in JUnit. The slowest case, `test_keep_ready_retains_worker_until_explicit_release_and_model_switch_replaces_it`, took 1.898 / 1.901 / 1.887 s including fixture phases. No naturally occurring flaky failure was observed in these same-order runs; this does not prove order independence. Skips include unavailable PySide6/Qt paths and collection skips; skipped cases are not passes.

Additional execution:
- Actual installed executable `BroTypist --version`: exit 0, `BroTypist 0.4.9`. Version-only smoke; not proof of frozen/source equivalence or live input behavior.
- Real `GhostSurface.drawRect_` rendered synthetic Portuguese/English into offscreen native bitmaps. Suppressing `CTLineDraw` changed inked pixels from 301/296 to 0/0. No window, focus change or screen capture.
- Fake-native-panel boundary: a valid chooser row `tomorrow` was acknowledged while its panel was hidden, both after identical replay and refused native ordering. This is not a physical Space/sleep or keystroke reproduction.
- Real AEAD storage with a synthetic key/data: canonical-equivalent Portuguese query forms produced different learning results; details in C3.
- Existing statistics test passed under a fixed virtual clock and failed when that clock crossed local midnight. The OS clock was not changed.

## Mutation evidence and interpretation

`mutmut`/other mutation runners were not installed. This review used **19 explicitly selected, single-node AST faults** in disposable archived source copies, checked package import origin, preserved unrelated source bytes and ran an unmodified-copy control first. Each targeted survivor was then tested against the **whole suite**. No source working-tree file was mutated.

**8 killed; 10 behavior-changing survivors; 1 excluded redundant/indeterminate mutation; no invalid cases or timeouts. Adjusted sampled mutation score: 8/18 = 44.4%.** This is a deliberately risk-selected sample, not an exhaustive mutation score, code-coverage percentage or overall application grade. All ten counted survivors had separate semantic counterexamples; the underlying raw run recorded eleven survivors before R03 was excluded.

| ID | Single fault | Result |
|---|---|---|
| R01 | Hidden cached ghost is not re-presented | Killed |
| R02 | Inactive-Space ghost is not recovered | Killed |
| R03 | Remove redundant empty-cache predicate; other recovery checks remain | Excluded: redundant/indeterminate |
| R04 | Acknowledge failed native ordering | Killed |
| R05 | Remove actual CoreText glyph drawing | Survived |
| R06 | Move glyph baseline by 12 points | Survived |
| R07 | Remove whole-word fitting | Killed |
| R08 | Bypass renderer validity guard | Survived |
| W01 | Classify whole prefix instead of active clause | Survived |
| W02 | Include lexically unrelated reference context | Killed |
| S01 | Ignore vocabulary strength limit | Survived |
| S02 | Drop accented learned vocabulary | Survived |
| S03 | Mix learning across languages | Killed |
| N01 | Use code-point length for a native UTF-16 range | Survived |
| N02 | Accept an incompletely displayed word | Survived |
| N03 | Invert native transaction begin gate | Killed |
| P01 | Change low-battery threshold from <=20 to <20 | Survived |
| P02 | Make actual native help presenter fail | Survived |
| U01 | Bypass macOS ZIP path validation | Killed |

R03 is not counted as a coverage defect: removing only the empty-cache predicate leaves native visibility/Space checks that still force modeled recovery. Conversely, R08 was demonstrated to acknowledge a baseline 80 points outside the caret row, and N02 returned ` arrive ` when only ` arr` was declared displayed.

Important positive controls: recent hidden-ghost/Space/refused-ordering regressions killed R01/R02/R04; unrelated-context rejection killed W02; cross-language isolation killed S03; existing runtime tests killed N03; ZIP traversal rejection killed U01. These results contradict broad claims that those contracts are wholly untested.

## Readability

No actionable systemic readability defect in the primary sample. Tests generally name concrete risks and expected outcomes. Large state-machine suites and necessary synthetic domains are not, by themselves, smells. Real AEAD data/key injection and OS API fakes are appropriate boundary isolation. No fabricated imports were found in the exercised collection.

## Reliability

### R1 — Medium: statistics test depends on wall-clock midnight

**Evidence:** `tests/test_storage.py:84–103` captures `now` for writes but calls `stats("today")` and `stats("week")` without its existing `now` parameter at lines 92, 94 and 96. The same existing test passed with a fixed synthetic clock and failed when the next clock read crossed local midnight.

**Risk:** nondeterministic red tests unrelated to an app defect, particularly around the date boundary. No random flake was observed during the three baseline runs.

**Remediation:** pass the captured `now` to all period aggregations, or inject a fixed clock in the fixture. Use the existing API; no new production clock abstraction is required.

### R2 — Low: elapsed sleep does not establish the intended in-flight race

**Evidence:** `tests/test_spelling.py:157–169`, especially `time.sleep(0.05)` at line 163, assumes the first backend request is already running before another field supersedes it.

**Risk:** a delayed worker may leave the first request merely queued, so the test can miss its intended stale-in-flight path even while green. This review did not observe a natural flaky failure.

**Remediation:** expose a `threading.Event` from the synthetic backend at entry, wait for that explicit condition, then supersede/unblock it. Keep timeout only as a failure bound, not synchronization.

## Diagnostic

### D1 — Medium: learning tests constrain metadata more than learned behavior

**Evidence:** `tests/test_storage.py:25–37` checks echoed strength values and a few vocabulary members, but does not provide enough vocabulary to distinguish the low/high strength caps. `tests/test_storage.py:164–188` checks the Portuguese language tag, not preservation of accented vocabulary. `storage.py:330` and `storage.py:90–91` were independently faulted.

**Observed counterexamples, with the entire suite still green:** strength 20 returned 24 vocabulary items instead of 5; dropping every accented vocabulary item removed `instrumentação` while leaving the language tag `pt`.

**Remediation:** use sufficiently rich repeated authored vocabulary to cross both caps; assert the semantic limit and precedence rather than only echoed parameters. For Portuguese, require representative accented learned terms and exact scope/language isolation. Do not replace semantic assertions with a hard-coded copy of the implementation formula.

## Design

### D2 — Medium: policy fake leaves the native low-battery presenter unconstrained

**Evidence:** `tests/test_appkit_power.py:146–168` replaces `_show_help` at line 157. That is reasonable for the deduplication-policy test, but there is no companion oracle that detects a broken presenter: replacing the first actual presenter statement (`appkit_power.py:257`) with a fatal error survived the full suite. Separately, changing `<= 20` to `< 20` at `appkit_power.py:39` also survived; its real result at 20% changed from `low` to `battery`.

**Risk:** correct help-policy tests can coexist with an unusable charging-help surface or an incorrect boundary-state icon.

**Remediation:** retain the isolated policy test and add a native presentation contract with an owned synthetic anchor, nonactivating window, visible help and functional dismissal. Exercise 19/20/21% for presentation separately from help-policy rearm. Do not mutate the real battery or use user windows. Existing `LocalStorage(key_loader=...)` injection is good design and should be retained; no blanket DI rewrite is indicated.

The review rejected a proposed exact `0.12`-second fade assertion: both existing short opacity fades can satisfy the accessibility behavior. Re-pinning an incidental duration is not an appropriate quality fix.

## AI-generated

### A1 — Medium: active-clause language behavior can be removed without failing tests

**Evidence:** `writing.py:9–23` extracts the active clause; relevant `tests/test_writing.py:123–174` cases replace `_draft_language` itself. W01 classified the entire mixed-language prefix instead of `agora vamos conversar` and still passed every test. The diagnostic patched only the recognizer boundary and executed the real wrapper.

**Risk:** tests of prompt structure/data preservation do not constrain the user-visible English/Portuguese switching policy.

**Remediation:** test the real clause wrapper with a controlled recognizer boundary, including language switches, trailing punctuation and ambiguous short prefixes. Keep exact typed-data/token-healing tests. The evaluator's `not_evaluated`/unreviewed metadata guards are legitimate defensive contracts, not grammar-quality proof; do not reclassify them as model evaluations.

Independent review and mutation checks exposed concrete closed-loop risks, not proof that every AI-authored test is bad. No fresh real-Gemma naturalness/grammar evaluation was performed here. Missing Gemma 3 delimiters in display-only prompting were **not** promoted to a local-model injection vulnerability: the local GGUF path tokenizes exact data spans with `special=False`.

## Coverage

### C1 — High: chooser acknowledges text the user cannot see — current application defect

**Evidence:** `appkit_overlay.py:399–405` reuses the chooser acknowledgement without checking native visibility/active Space; lines 445–454 acknowledge after ordering without verifying success. The diagnostic reproduced nonempty `tomorrow` acknowledgement with `visible=False` after both native-boundary hiding and refused ordering.

**Impact trace:** `runtime.py:1563–1598` trusts matching presentation text to arm choices; `runtime.py:1642–1666` can claim numbered choices and, for synonyms/emoji, word/full acceptance. Thus **when a chooser mode is active and its origin/revision still matches**, unseen choices can consume keys and insert/replace text. This is a source/control-flow and fake-native-boundary reproduction, not a claim that physical Space switching or user text modification was observed. Ordinary ghost rendering is a separate surface; this is not evidence that its recent fix failed.

**Remediation:** apply the ghost's visibility/Space/no-ack-on-failure contract to chooser initial presentation and cache reuse; recover/re-present hidden panels and revoke acknowledgement when nothing is actually shown. Add a hidden/refused-ordering/Space chooser regression that also checks controller key acceptance is not armed. Preserve existing origin/revision and truncation fences.

### C2 — High: native painting/layout and undrawn-acceptance failures can remain green

**Evidence:** `tests/test_appkit_overlay.py:27–44,57–79` primarily checks fake-panel state, callback text and frame. Removing `CoreText.CTLineDraw` (`appkit_overlay.py:238`), shifting drawing by 12 points (`:237`), or bypassing `InlineRender.valid` (`:468`) each survived all 1,334 passing cases. Independent native bitmap/layout probes showed zero glyph ink, a 12-point centroid shift, and acknowledgement of an invalid caret-row baseline respectively.

Also, removing the hidden-word guard at `core.py:259–260` survived the suite and made `SuggestionState.accept(..., visible=" arr")` return ` arrive `.

**Remediation:** add offscreen native ink/baseline/clip invariants using synthetic inputs and real drawing, not only window bookkeeping. Require invalid caret-row metadata to yield no acknowledged text. Test partial-visible word acceptance directly through the real state/controller contract, including whitespace-only extensions that are intentionally allowed. Assert geometric/visibility behavior, not brittle exact pixel screenshots or implementation source strings.

### C3 — Medium: canonical-equivalent Portuguese loses learning — current application defect

**Evidence:** `_writing_language` uses unnormalized regex tokens (`storage.py:23–46`), and `_style_features` likewise uses raw fragments (`:75–91`). Real synthetic encrypted history produced Portuguese learning evidence for NFC `Nós na próxima reunião`, but the visually identical NFD query returned an empty language and no personalization. Derived NFD vocabulary included fragments such as `instrumentac`, `reunia` and `xima` instead of their complete accented words. Tests at `test_storage.py:164–188` use precomposed text.

**Remediation:** normalize **derived** language/vocabulary inputs to NFC after extracting original authored spans. Never normalize the stored draft before slicing its original authored offsets; that changes coordinate space and can misattribute generated text. Add NFC/NFD equivalence and original-text/span-preservation behavior cases. This is a learning/indexing defect, not observed corruption of encrypted raw history.

### C4 — Medium: native non-caret UTF-16 conversion lacks its consumer boundary case

**Evidence:** `macos.py:1239–1255`, specifically the range length at line 1244. Replacing UTF-16 units with Python code-point length survived the whole suite. The actual `MacBridge.replace_range` with only AX boundary fakes wrote `(4,2)` for the emoji in `say 😀 now`; the faulted version wrote `(4,1)`.

**Remediation:** exercise a non-caret replacement containing a surrogate pair through the real `MacBridge`, verify the native range conversion and read-back fence, and include a rejected mismatched selection. This is a demonstrated coverage gap; the current conversion is correct. No real Accessibility edit was made.

### Explicit limits, not passes

No physical WhatsApp/Telegram typing or Tab acceptance, sleep/wake/Space change, real 20% battery transition, or fresh real-model grammar/naturalness/physical-keystroke latency measurement was performed. Offscreen text drawing does not prove compositor visibility; CLI startup does not prove cross-app completion. This macOS review and its Qt skips do not validate Linux/Windows. Earlier receipts are not counted as fresh execution in this review.

## Automation decision and remediation order

**No new blanket lint rule or numerical mutation-threshold gate is warranted from this selected sample.** A threshold here would turn a deliberately adversarial sample into false precision; generic mock/sleep/loop bans would also flag valid boundary tests. The existing CI pytest gate (`.github/workflows/ci.yml:64–65`) is retained, not remotely rerun or changed.

Use that gate for focused behavior regressions during remediation: chooser no-unseen-key acceptance; real offscreen glyph/layout invariants; partial-visible word and non-caret surrogate ranges; canonical Unicode and semantic learning limits; actual help presentation and 20% boundaries. Then establish an approved full/changed-path mutation baseline before choosing a numeric gate. No third-party tool installation, CI edit or commit was authorized by this review.

Recommended order: **C1 → C2/C3 → D1/A1/D2/C4 → R1/R2**, followed by a separately scoped live bilingual first-word latency/quality evaluation. Current code defects and test gaps are distinct; no fix has been applied here.

## Reviewed-file ledger

One row per reviewed file (31). Low means no substantive defect found in the reviewed portions, not certification of the entire file. Metadata-only and narrow cross-check scopes are stated explicitly.

| File | Severity | Reviewed result/scope |
|---|---|---|
| `tests/test_appkit_overlay.py` | High | C1/C2: fake-panel acknowledgement and frame checks miss actual painting and invalid-layout use. |
| `tests/test_native_contract.py` | Medium | C2: non-caret, non-BMP range conversion fault N01 survives. |
| `tests/test_runtime.py` | Medium | C2: partial-visible acceptance fault survives; native begin-gate inversion is correctly killed. |
| `tests/test_writing.py` | Medium | A1: active-clause wrapper bypassed; structural eval metadata is not language-quality proof. |
| `tests/test_storage.py` | Medium | C3/D1/R1: Unicode equivalence, strength/accented vocabulary assertions and clock boundary. |
| `tests/test_appkit_power.py` | Medium | D2: native help presenter replaced; exactly-20% card boundary missing. |
| `tests/test_update_install.py` | Low | Narrow review found no demonstrated defect; ZIP path-check removal is killed. |
| `tests/test_spelling.py` | Low | R2: fixed sleep does not establish that the first check entered its backend. |
| `tests/test_token_healing.py` | Low | Related token/data-boundary cross-check; no actionable finding in reviewed portions. |
| `tests/test_macos_language.py` | Low | Related recognizer-boundary cross-check; does not replace wrapper/clause coverage. |
| `tests/test_power.py` | Low | Related low-battery policy cross-check; help policy and native card are separate contracts. |
| `tests/test_updates.py` | Low | Related manifest/update-service cross-check; no demonstrated defect in reviewed portions. |
| `tests/test_appkit_menu.py` | Low | Narrow lifecycle/menu cross-check only; not treated as live native Quit proof. |
| `src/inklow/appkit_overlay.py` | High | C1 actual chooser visibility-acknowledgement defect; C2 rendering contract coverage gaps. |
| `src/inklow/native/macos.py` | Medium | C2 correct UTF-16 implementation lacks the demonstrated non-caret surrogate-range oracle. |
| `src/inklow/core.py` | Medium | C2 correct unseen-word and geometry guards are removable without test failures. |
| `src/inklow/runtime.py` | Low | Reviewed acknowledgement/claim/deferred paths; rejected claim that all begin-gate behavior is untested. |
| `src/inklow/storage.py` | Medium | C3 actual Unicode-derived learning defect; D1 semantic assertion gaps. |
| `src/inklow/writing.py` | Medium | A1 real clause extraction unexercised by its wrapper tests; no local GGUF delimiter exploit established. |
| `src/inklow/providers.py` | Low | Narrow exact-data/token-healing boundary review; no actionable finding in reviewed portions. |
| `src/inklow/native/macos_language.py` | Low | Narrow recognizer/reset/confidence review; no actionable finding in reviewed portions. |
| `src/inklow/native/base.py` | Low | Narrow range/bridge-contract review; no actionable finding in reviewed portions. |
| `src/inklow/appkit_power.py` | Medium | D2 native presenter and 20% presentation boundary are not constrained by current tests. |
| `src/inklow/power.py` | Low | Policy separated from presentation; no actionable finding in reviewed portions. |
| `src/inklow/update_install.py` | Low | Archive/transaction boundary review; broad ZIP-validation bypass is rejected by tests. |
| `src/inklow/updates.py` | Low | Narrow signature/manifest/service review; no demonstrated current defect. |
| `src/inklow/appkit_ui.py` | Low | Relevant lifecycle/integration sections only; no live settings/menu review claimed. |
| `tools/evaluate_predictions.py` | Low | Narrow quality-status/reporting contract review; unreviewed status is correctly not a quality pass. |
| `.github/workflows/ci.yml` | Low | Existing pytest/native packaging checks reviewed as configuration, not remotely executed. |
| `pyproject.toml` | Low | Pytest/platform dependency conventions checked; no project QA context document present. |
| `README.md` | Low | Install/development/platform-limit sections checked for review scope only. |

## Reproduction records

Portable evidence above includes the original source fingerprint and AST before/after for every selected fault, pass counts, native offscreen/chooser results, Unicode/virtual-clock results and semantic counterexamples. Full JUnit/logs and disposable diagnostic programs remain local in the private QA workspace; they contain synthetic data only and were not added to the repository. Reproduction must use a disposable copy, its own `INKLOW_HOME`/pytest cache and `PYTHONPATH` pointing to that copy; never apply a fault to the normal app/profile.
