> Historical record: Named third-party research references were removed during repository-history cleanup. This edited historical record is not an original authenticated receipt. Recorded build identifiers and measurements describe their original observations.

# Observed verification

## 0.4.1 signed, installed real-messenger verification — 2026-10-26

Runtime source **`0dd98b10e2cf809cf612d720878cd46b388b7ec7`** was frozen, Apple Development-signed and atomically installed at **`/Applications/BroTypist.app`** with the same designated requirement. [Installed receipt](brotypist/installed-0.4.1.json) records the executable hash, isolated acceptance and normal-profile observations.

| Actual application | Rendering observed | Normal saved profile | Installed binary, isolated recording-disabled profile |
| --- | --- | --- | --- |
| WhatsApp | Grey text at the attributed native caret row | Visible suggestion; owned draft cleared | Real hardware Tab appended ` appreciate `; no literal Tab |
| Telegram | Grey text at the final observed LTR glyph edge, fitted system font | Visible suggestion; owned draft cleared | Real hardware Tab appended ` appreciate `; no literal Tab |

The observed continuation was ` appreciate it.` after the synthetic `Thank you for your help with the report. I really`. [Actual normal-profile WhatsApp pixels](brotypist/whatsapp-installed-0.4.1.png) · [Actual normal-profile Telegram pixels](brotypist/telegram-installed-0.4.1.png). Both application drafts were cleared; no messages were sent. Normal-profile checks did not accept text or change saved preferences: settings bytes matched the private pre-install backup after normal Quit. Existing cached models and permission identity were retained; screenshot consent was not enabled.

The packaged private-profile run remained alive after actual HID-level typing and Tab acceptance in both messengers, then exited normally with code 0. The app's own menu-bar item was registered at 24×24 points. AXPress did not establish status-menu tracking/Quit; that path is not reported as passed. The final app was relaunched through LaunchServices using the user's normal profile.

The actual signed DMG passed read-only mount, drag-install layout, copied source/signature/version, visible own-PID settings, normal Quit, unchanged isolated settings, preserved data on removal and detach. Apple Development signing is not Developer ID distribution or notarization.

Cold first-offer observations remain approximately four seconds; warm observations were sub-second. These individual samples are not an SLA or a solved cold-start claim. No universal input, multilingual grammar-quality or RAM-parity claim is made. Linux/Windows product builds remain held.

**Published private release:** [`v0.4.1`](https://github.com/renan-brasilio/brotypist/releases/tag/v0.4.1) is published, not draft or pre-release. Exactly four assets—DMG, ZIP, `release-manifest.json`, `SHA256SUMS`—were independently downloaded. The packages/manifest passed the downloaded checksum file; all four files, including `SHA256SUMS`, matched GitHub's recorded digests. The remote annotated tag resolves to the exact runtime source above. Later `main` commits contain verification documentation only. [Publication and integrity receipt](brotypist/release-0.4.1.json).

## 0.4.1 source repair: real messenger keyboard path — 2026-10-26

The user-reported absence of ordinary predictions was not a model-download failure. A real macOS crash report recorded `EXC_BREAKPOINT` / `SIGTRAP`: the background Quartz callback entered `NSEvent.charactersIgnoringModifiers`, HIToolbox input-source lookup, then `_dispatch_assert_queue_fail`. Keyboard-layout translation now runs on the main queue, publishing a replacement map for the tap. A failing-before/passing-after regression exercises background typing, passthrough and a logical AZERTY→QWERTY shortcut transition.

Separate native evidence established WhatsApp's `(0,0)` visible-range sentinel, wrapped caret rows, padded glyph envelopes and independently rounded integer glyph edges. The observed last sub-span advanced 11 points versus CoreText's 10.1943 points: the old 0.75-point threshold refused it. The new bound is one point only for integer edges; incompatible sub-spans, fractional-coordinate mismatches, clipping, hidden fields and stale inputs remain rejected. Telegram's missing caret/font metadata uses the bounded end-of-line path described below, not a popup.

- **Actual source WhatsApp:** hardware-level key events, grey same-line ` appreciate it.`, then real Tab appended ` appreciate `; own draft cleared, no message sent. [Actual cropped pixels](brotypist/whatsapp-source-0.4.1.png).
- **Actual source Telegram:** the same keyboard/Tab sequence passed in its native Qt composer; own draft cleared, no message sent. [Actual cropped pixels](brotypist/telegram-source-0.4.1.png).
- **Local integrated suite:** 897 passed, 21 skipped in 16.72 seconds. Hosted CI remains unestablished because of the account-level runner-allocation block recorded below.
- **Limits:** these particular checks are source-runtime observations, not by themselves proof of a frozen/installed build. Screenshot consent was off; synthetic observations used a history/statistics-disabled profile. No universal input, multilingual grammar, latency SLA or RAM-parity claim follows.

## 0.4.0 signed Mac release and installation — 2026-10-26

Application source **`312edd696f60951fea74763e5b3b4f821e7239f7`** was frozen as arm64 0.4.0 and installed at **`/Applications/BroTypist.app`**. The previous signed app/settings have a private rollback. The actual user profile selects the verified public **Gemma 4 E2B Q4_0**, reusing existing cached bytes without a network download; Qwen is retained. Startup bytes are unchanged. Other existing preference values were preserved, with the legacy disabled battery-model flag migrated to equivalent `battery_model=same`. Screenshot/terminal consent was not enabled.

- **Frozen CLI and worker:** actual Gemma generation passed in both, with exit code 0; the worker streamed ` help.` and shut down successfully.
- **Normal LaunchServices typing:** the exact frozen app, in a fixture-only profile, visibly offered grey same-line ` appreciate it.`. Two real consecutive Tabs produced `Thank you for your help with the report. I really appreciate it`, without a literal Tab. No other prediction app was running and the owned host's OS prediction was disabled. [Actual frozen pixels](brotypist/frozen-inline-0.4.0.png), [frozen runtime/signing/installer receipt](brotypist/frozen-0.4.0.json).
- **Native surfaces observed:** the field button measured 24×24 points above the field; the registered menu-bar item measured 24×24 at an on-screen position. A later AX extras query was unavailable, so final field-menu tracking/status-menu Quit is not claimed as a live pass. Normal OS Quit stopped the isolated frozen app. Frozen focused-window-context/full acceptance also remains unestablished: a key passed through without an acknowledged offer and a later probe aborted on its foreground guard.
- **Installer:** actual DMG read-only mount, drag-install layout, copied signature/version/source, visible own-PID settings, normal Quit with exit 0, unchanged isolated settings, data preservation on removal and detach all passed.
- **Signing:** the existing Apple Development identity/team and exact designated requirement were retained. Deep/strict verification passed; hardened runtime is enabled with only `com.apple.security.automation.apple-events`. This is not Developer ID distribution or notarization. Installed executable SHA-256: `b18b24049085c4e0c68cf11f786ed1a2223fa8d348b783ce549768d25dc669c4`.
- **Real installation:** one normally launched instance was observed at the canonical unversioned app path with Gemma selected and global/default predictions enabled. [Installed model/data-preservation and disposable-document cleanup receipt](brotypist/installed-0.4.0.json).
- **Published private release:** [`v0.4.0`](https://github.com/renan-brasilio/brotypist/releases/tag/v0.4.0) is published, not draft or pre-release, with exactly DMG, ZIP, `release-manifest.json` and `SHA256SUMS`. All four files were independently downloaded from GitHub. Both package hashes and the manifest hash passed `shasum -a 256 -c SHA256SUMS`; the checksum file itself matched GitHub's recorded digest. Repository visibility remains private. [Publication receipt](brotypist/release-0.4.0.json).
- **Hosted CI limit:** [run 40313479248](https://github.com/renan-brasilio/brotypist/actions/runs/40313479248) never allocated a runner: the `metadata` job had no steps and `verify` was skipped. The owner-visible annotation says recent account payments failed or the spending limit needs increasing. This is not a failed source-test result, and hosted CI is not claimed green. No billing, spending limit, token permission or account setting was changed; the exercised checks above are local native checks.

These checks are bounded proof, not complete [redacted reference] compatibility, calibrated grammar quality, a visible-latency SLA or RAM superiority. See the explicit Docs/terminal/RTL and held-platform limits below.

## 0.4.0 source integration — 2026-10-26

This is source/runtime evidence, not a claim that a new frozen bundle is installed or published. The signed application and installer must have their own receipts.

- **Integrated regressions:** 879 passed, 21 skipped in 16.66 seconds after the final document-provider safeguard and neutral setup wording. Independent native/privacy and frozen-entrypoint/signing reviews reported no actionable static findings; neither review substitutes for runtime proof.
- **Actual inline pixels:** the independently sourced Gemma 4 worker produced ` appreciate it.` in the owned native `NSTextView`, whose own OS inline predictions were disabled. [Uncomposited, owned-window capture](brotypist/native-inline-gemma4-0.4.0.png). The transparent grey words use the host's native font/baseline; no ordinary suggestion card or popup is present.
- **Repeated acceptance:** two consecutive real Tab events changed the owned native field from `Thank you for your help with the report. I really` to `Thank you for your help with the report. I really appreciate it`. No literal Tab was inserted; the host recorded no forwarded Tab key events. This establishes the candidate's ordered two-word acceptance, not the original app's acceptance behavior.
- **Actual worker IPC:** [synthetic receipt](brotypist/gemma4-worker-0.4.0.json) records the real private worker, cold preparation, a grounded Room Cedar continuation and safe absence for constrained partial-word alternatives. A warm contextual result began at 230.49 ms; the cold first result took 2755.79 ms. These individual samples are not visible desktop latency percentiles or an SLA.
- **Current model evaluation:** private `gemma4-current-final-v2.json` ran all 19 synthetic-writing-v2 cases through the current native Metal provider with 19 mechanical passes. It includes seven writing languages and boundary/context/utility cases. Two of five alternatives cases produced no offer. Semantic/grammar quality remains ungraded; no all-language-quality guarantee follows from mechanical checks.
- **Native settings:** all eleven panes were exercised in the final source smoke with a native visible-window property, registered status item and menu Quit entry. Neither Qt nor llama_cpp was imported into the UI process. That smoke does not establish human-visible comparison or typing; the separate owned-window capture above supplies the latter's visual evidence.
- **Terminal scope:** native Terminal.app exposed a true bounded unsent Codex 0.162.1 draft, session/process/CG-window ownership and independent glyph spans. A separate live raster verification recovered `SFMonoTerminal-Regular`, 12 pt, fixed advance 7 pt and a baseline matching real window pixels. End-to-end terminal acceptance is not yet established; no iTerm, other-agent, mux or shell compatibility is claimed. Appearance consent is separate, and terminal input consent never includes scrollback/OCR context.
- **Docs safety:** the disposable Aside Google Doc exposed mismatching native action/readback versus persisted preferences. Its provider is now explicitly refused for preference mutations; the setup offer remains metadata only, with manual screen-reader + braille instructions. This is not a live Docs typing pass or a claim that all browser providers were exercised. The native helper never fabricates a caret or enables ordinary acceptance.

Screenshots/context consent remain opt-in; existing saved consent is not enabled by this integration. Linux/Windows product builds and iCloud remain deferred. No proprietary [redacted reference] code, artwork or weights is redistributed.

## Earlier macOS source candidate — 2026-10-25/26

This historical checkpoint records the earlier investigation and synthetic source observations, not the final integration run above. At that point the installed `/Applications/BroTypist.app` remained **0.3.3**, the starting HEAD was **`1423961`**, and the parity changes were unreleased/uncommitted. Historical release/install claims below apply only to their named revision and artifacts.

### Established source/native observations

- The previously exercised full Python suite reported **605 passed, 21 skipped in 14.60 seconds, no warnings**. That predates the current benchmark-contract/Docs/terminal integration; it is not evidence that the latest edits pass.
- The actual source AppKit smoke exercised all **11 panes**, menu and Quit with no model or Qt loaded at idle. That all-pane run occurred while the screen was locked: it is **not visual or typing proof**.
- A separate unlocked, owned synthetic `NSTextView` observation used the real independently obtained Gemma 4 model. Grey same-line ` appreciate it.` rendered; after the cold model was ready, actual Tab inserted ` appreciate ` without a literal Tab. Actual focused-window Accessibility context from a synthetic Cedar/Tuesday-10-AM fixture produced ` Tuesday at 10 AM `. The private capture is `candidate-typing-live.png`, not a published screenshot or installed-build receipt.
- The original [redacted reference] **2026.5.1 build 83** showed grey same-line ` appreciate it.` and had `gemma-4-E2B-Q4_C.gguf` open during real prediction. The host-only fixture showed no ghost after the original quit, with the host's OS inline prediction disabled. Original synthetic Tab acceptance was **not established**. A source rapid-Tab experiment aborted safely on user focus change; it is not a pass.
- Full private native analysis completed with REA **6.8.0**, Ghidra **12.1** and Temurin **21** in **738 seconds**. See [the version/hash-bound native findings and model identities]([redacted reference].md#deep-native-and-live-model-pass--recorded-2026-10-25-reviewed-2026-10-26). The earlier missing-provider failures below remain historical evidence. No Java/Ghidra product dependency or proprietary recovered source/assets/weights was added.

The screenshots, native dossiers and synthetic result JSONs named here remain private. No raw decompilation, user writing, personal document identifiers/paths, secrets or private image is added to this repository.

### Real public Google-model samples and provenance

Public Gemma 4 E2B Q4_0 and Gemma 3 1B Q4_K_M files were independently acquired, revision/size/SHA-256 bound and actually executed with Metal. Their exact identities and terms are in [[redacted reference].md]([redacted reference].md#independent-google-weights-not-a-copy-of-the-original). Qwen was preserved. Larger pinned catalogue entries have **not** been executed.

Private `google-family-real-sample.json`, created **2026-10-25 16:30 UTC**, recorded **15/15 mechanical cases for each model**. Provenance: provider SHA-256 `4acd0f1fde6052a265e90b3bf927810eeefbb0742af489d91c1be9a7dc04816a`, benchmark SHA-256 `b0386ceec52075bf6aa0f0019974aff55e735d6787226b64f103e96b88f0a970`. This was the earlier 15-case utility revision; utilities subsequently changed, and semantic/grammar quality was not graded.

Private `gemma-metal-attention-ab.json`, created **2026-10-25 18:20 UTC**, used identical Gemma 4 weights in three fresh process rounds per attention variant, 15 cases each: **45/45 mechanical cases per variant**. It records the same provider/benchmark hashes above, with writing SHA-256 `9387510283712cca255f1dbdb0508f2d16c021ce3b633bd1d51c6f083a389cf0`.

| Observed provider-stream measurement | Flash-attention experiment | Baseline |
| --- | ---: | ---: |
| Median first displayable text, 45 samples | 178.57 ms | 173.17 ms |
| Median total stream, 45 samples | 225.55 ms | 225.84 ms |
| Process-lifetime peak RSS, round 1 | 3,260,645,376 bytes | 3,447,422,976 bytes |
| Process-lifetime peak RSS, round 2 | 3,378,987,008 bytes | 3,443,081,216 bytes |
| Process-lifetime peak RSS, round 3 | 3,371,352,064 bytes | 3,440,017,408 bytes |

There is **no proven latency win**; the flash-attention experiment is **not enabled in the product**. First-display timing is around provider streaming, not last-keystroke-to-visible desktop latency: it excludes native capture, debounce, worker IPC and rendering. Fresh process does not mean cleared OS/Metal/file caches. Process-lifetime peak RSS includes mapped/shared pages and is neither incremental per-case allocation nor complete GPU/physical footprint. The original's one-time **207,264 KiB RSS** sample is not comparable. Separate native cold-first observations were approximately **2.55–3 seconds**, with one warm context case around **0.5 seconds**; these are descriptive samples, not an SLA, superiority or memory-parity claim.

### Failed utility experiments and corrected consumer contract

The private historical artifacts are retained as failures/limitations of their recorded source revisions; their totals are **not** recomputed or relabelled as a current pass:

| Private synthetic artifact / creation time (UTC, 2026-10-25) | Provider SHA-256 (abbreviated) | Recorded mechanical totals, Gemma 4 / Gemma 3 |
| --- | --- | --- |
| `google-utility-boundaries.json`, 18:32 | `f8ac04402777…` | 15/16 and 13/16; overall fail/inconclusive |
| `google-utility-boundaries-gbnf.json`, 18:46 | `ead89e0a6d51…` | 19/19 and 15/19; overall fail/inconclusive |
| `google-caret-alternatives.json`, 18:58 | `eaebecfb3bb5…` | 16/19 and 15/19; overall fail/inconclusive |

The JSON prefix-replay and forced-novelty/high-temperature experiments were removed from the product after malformed/empty results and semantically bad partial-word suggestions. For example, the last artifact's mechanically accepted partial-word list included `eme.` and `ule.` after the synthetic `feed` prefix: printable strings and a full list do not establish sensible words. Other cases returned only one/two valid choices or no output; the old benchmark's unconditional **3–5/nonempty** requirement incorrectly marked those cardinalities as failures. The artifacts remain evidence of the old sampler/validator and ungraded semantic defects, not of the current implementation.

The current provider instead reuses the ordinary native continuation/token-healing path at temperature **0.35**, at most **five** samples, targeting **up to three** distinct valid continuations and excluding the current anchor. Especially at partial words, three novel valid choices may not exist. Fewer choices or no offer are honest outcomes; neither is a quality score. Synonyms remain a separate selected-word replacement contract.

`tools/benchmark_writing.py` now identifies new reports as **`synthetic-writing-v2`, schema 2**. Alternatives permit **zero to three** strict distinct choices, with no anchor/prefix replay, correct partial-word/trailing-space/unspaced/new-word boundaries, no displayed controls and no raw/displayed thought/tool framing or quoted-injection success. Empty output and a strict empty array are safe absence only when the other mechanical checks pass; malformed output, runtime failure, missing native generation, unclosed iterators and token-budget violations still fail. Selected-word synonyms retain their 3–5 strict-choice/nonempty checks.

Each utility row reports parsed and reported choice counts, empty/no-output state and safe absence; summaries separate per-kind choice counts, empties, safe absences and mechanical failures. First-display timing remains absent for no offer. Semantic, grammar, language and factual grounding remain **not evaluated**, irrespective of cardinality or mechanical success. **No new model execution or benchmark regrading was performed for this contract correction.**

### Integration and evidence unknowns

- The document-bound Google Docs metadata/accessibility helper and opt-in Terminal/iTerm Automation path are in development. No live Docs/terminal compatibility pass is asserted here. Metadata does not prove a writable snapshot/caret, and a permission request/preflight is not a verified grant. Ordinary completion must stay silent when native safety/typography cannot be established.
- Current integration still needs the integration owner's complete source checks, source-native UI/typing and repeated acceptance evidence, then a separately authorized build/install verification. The earlier **605/21** result and installed **0.3.3** app do not supply that proof.
- No complete [redacted reference] compatibility, original Tab-acceptance parity, universal browser/terminal support, calibrated multilingual grammar quality, latency SLA, total GPU/RAM comparison, network privacy audit, exact private prompt/quantization or training/personalization equivalence is established.
- Linux/Windows product work and iCloud work are deferred. Screenshots/context appearance remain explicit opt-ins, not evidence of silently enabled installed preferences; licensing/paywall/account/telemetry imitation remains out of scope.

## BroTypist 0.3.3 — compact field controls and renewed reference review

| Final integrated source suite | Passed | Skipped |
| --- | ---: | ---: |
| macOS / native AppKit | 361 | 21 |
| Linux / portable Qt | 403 | 1 |

- **Actual field menus:** source-native AppKit displayed only current-app predictions, temporary pause/resume and Settings; no custom dashboard header was present and menu-bar Quit remained. [Native capture](brotypist/minimal-field-menu-0.3.3.png). The real portable `QMenu` also displayed those actions in a private offscreen profile. [Portable capture](brotypist/minimal-field-menu-qt-0.3.3.png). These use synthetic fixture context, not personal writing; the portable capture is not native-desktop input proof.
- **Artwork boundaries:** full-size SVG derivatives retain the I-beam and two pills; compact logical sizes use the stem and one pill. Native field glyphs assemble explicit 1×/2× template representations. A logical-size Qt icon engine separates 32pt@2× from 64pt@1× rather than confusing equal physical raster dimensions. Ten real widget-icon size/DPR cases verify physical resolution and visible pill geometry.
- **Workflowz review:** named pool `BroTypistReferenceReview` reviewed menu lifecycle/focus, artwork/geometry and reference/model claims independently. Two verified artwork/geometry findings were corrected: the accessory now hides instead of clamping into previous text rows at a left edge, and application-wide small icons retain the compact drawing. The multiline-overlap regression failed before the fix and passed afterward. Menu actions retain the pre-tracking app/domain and execute only after tracking closes.
- **Fresh original-app evidence:** REA captured the running [redacted reference] General picker selecting/recommending **Gemma 4 E2B (2.8 GB)** and static Gemma4/llama.cpp/GGUF strings. See [reference evidence and model decision]([redacted reference].md#reference-correction-pass--2026-10-25), [sanitized receipt](brotypist/reference-correction-0.3.3.json). Raw records remain private. The original review process was quit normally after inspection; no preference controls were changed.
- **Signed local handoff:** implementation `a3674a16f34aa29a12a643a9bbe7e943600b1740` was built as arm64 0.3.3. The actual frozen CLI and model worker generated output; the signed DMG passed read-only mount, copy/install layout, actual settings-window launch, normal Quit, external-data preservation and detach. The installed `/Applications/BroTypist.app` reports 0.3.3, runs as one instance and exposes its native menu-bar item. All settings and startup bytes were unchanged after launch; the selected model file size/mtime were unchanged. A CRC-verified private previous-app rollback is retained. [Installed receipt](brotypist/installed-0.3.3.json). Apple Development signing is not notarization; DMG/ZIP were built locally, not published as a new GitHub release.

**Limits at the 0.3.3 revision:** that build retained its independently pinned Qwen writer. No Gemma download, adapter or performance/quality benchmark had been performed in that pass. Upstream llama.cpp support was not validation of the packaged Python binding. Deep decompilation providers were unavailable; original live typing, exact field-menu contents, prompts and runtime model execution were then unproven. The later source-only observations above narrow specific unknowns, not this historical artifact's scope. Strict native input restrictions, browser-font limitations, unavailable Wayland ordinary completion and unverified Windows-native behavior remain unchanged. This was not complete [redacted reference] parity or a new GitHub release.

## BroTypist 0.3.2 — strict native inline completion

Implementation source: `3077722c09a03e635d0392f92f13f449d0cb509a`. The following replaces, rather than extends, the older popup/Wayland compatibility observations below.

| Exercised source suite | Passed | Skipped |
| --- | ---: | ---: |
| macOS / native AppKit | 353 | 20 |
| Linux / portable Qt and native safety regressions | 390 | 1 |

- **Actual packaged inline rendering:** the signed arm64 app ran its real model against a separate native `NSTextView`. The grey completion used the host font and baseline immediately after the caret, without a card, border or shadow. The same-window synthetic reference specified Tuesday at 10 AM in the Cedar room; the completion used those details. [Actual screen pixels](brotypist/inline-native-0.3.2.png), [receipt and limits](brotypist/native-inline-0.3.2.json). The image is a guarded screen-region capture, not a composite.
- **Input and privacy:** the native-controller probe accepted the next word through a real external-process Tab event and preserved host focus. Its bounded Accessibility context omitted the synthetic secure sibling. Clipboard work is marshalled to the Qt GUI thread and reauthorizes before/after reading; context jobs cancel and reauthorize before publication.
- **Packaged permissions and menu:** actual Privacy labels read `Granted` for Accessibility, Input Monitoring and Screen Recording. Prediction context uses Accessibility only, not Screen Recording. The installed app exposed its menu-bar item and `Quit BroTypist`; pressing that actual quick-menu item terminated the installed app normally. It was restarted afterward.
- **Installer and inference:** the signed 0.3.2 DMG mounted read-only, exposed the drag-install layout, copied a valid signed bundle, printed the correct CLI version, showed a real settings window and quit normally. Its isolated uninstall preserved external user data. The frozen CLI and inference worker both generated real model output and exited successfully. DMG and ZIP were built locally; no new GitHub release was published by this change. The Apple Development signature is not a notarization or Developer ID distribution claim.
- **Installed preferences:** only the user-approved debounce (`150 ms`), idle retention (`300 seconds`) and Accessibility window-context switch were changed. Other settings, the existing model and login-item configuration were preserved; the prior signed bundle and preferences have a private local rollback.
- **Artwork:** canonical normal/dark/small SVG artwork from the supplied handoff is retained byte-for-byte. ICNS/ICO/PNG derivatives use that artwork, without the previous additional enclosure/mask rewriting; native small/Retina UI images load the ICNS representation.

**Important limits:** this is transparent caret-aligned native overlay text, not host-document mutation or native IME pre-edit. Chrome on this Mac exposed font size but no trustworthy font name, so strict ordinary completion stayed silent; focused-document Accessibility context still worked. Wayland ordinary completion is disabled after a real nested-compositor test showed unsafe stale-selection behavior and no proven inactive styling; the unsafe pre-edit ABI/path was removed. The Linux suite/build is not Linux-desktop inline compatibility proof. Windows-native execution remains unverified. RTL/composing/unverifiable controls stay silent. No universal-input, visible-latency percentile or multilingual-quality claim is made.

**Test correction:** the old installed BroTypist and Grammarly Desktop were still active during early probes. Grammarly owned an overlapping popup and was temporarily quit with approval for the final isolated check, then reopened. An early manual-run-loop harness checked event/panel flags without reliable drawing; those results and its event timings are not visual acceptance proof. The final renderer check used a real AppKit application loop; the final model screenshot came from the packaged app.

## BroTypist / Ghost Caret — 2026-10-24

The private repository was renamed in place to `renan-brasilio/brotypist`; the GitHub API still reports `private: true`. The public app, CLI and new packaging paths use BroTypist. Established OS/data/credential namespaces are intentionally unchanged.

| Exercised source suite | Passed | Skipped |
| --- | ---: | ---: |
| macOS / native AppKit | 289 | 16 |
| Linux / portable Qt and native-Wayland safety regressions | 317 | 1 |

- **Real macOS windows:** all six settings destinations and six Welcome steps were shown. Actual permission-state gating, the persistent 18×16 template status glyph, existing model/shortcut preservation and normal native-menu Quit were exercised. The current Qwen model produced a real suggestion in the owned Welcome field, and native acceptance inserted its next word. Neither Qt nor `llama_cpp` was imported into the UI process. [Receipt](brotypist/macos-design-smoke.json).
- **Real KDE Wayland desktop:** the six portable settings routes and six Welcome steps were shown. The actual model generated a suggestion; a Tab event accepted the next word. A selection containing a non-BMP character suppressed insertion without an invalid-cursor error. KDE's watcher registered the actual GUI process with Title `BroTypist`, Status `Active` and an exported `Quit BroTypist` action; closing Settings preserved the agent, and normal Quit stopped it. [Receipt](brotypist/linux-design-smoke.json).
- **Visual proof:** [Mac settings](brotypist/macos-settings-general.png), [Mac Welcome](brotypist/macos-welcome-1.png), [Mac real prediction](brotypist/macos-welcome-real-prediction.png), [Linux settings](brotypist/linux-settings-general.png), [Linux Welcome](brotypist/linux-welcome-1.png), [Linux real prediction](brotypist/linux-welcome-real-prediction.png). Mac captures use exact owned window IDs, not a full personal desktop; portable captures use owned Qt widgets. Test profiles contain synthetic text and blank custom instructions/rules.
- **Artwork:** the installed Icon Composer exporter successfully generated the original normal/dark/tinted assets and native system mask; CoreGraphics and `iconutil` generated the Mac raster/ICNS payload without Qt.

Limits: these are source-UI and owned-field prediction checks, not new third-party-input compatibility proof or Windows-native verification. Source-host Mac permission results do not establish a differently signed installed app's grants. The KDE UI smoke did not change the production input-method selection. The redesign preserves the current inference engine, models and acceptance defaults; it is not a new multilingual-quality claim. Historical release assets were not rewritten or republished.

## Historical Inklow 0.3.0 baseline — 2026-10-24

The remaining sections retain the original observations and filenames; they are not presented as reruns of the BroTypist redesign.

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

[REA [redacted reference] report]([redacted reference].md), [sanitized native observations]([redacted reference]-menu-reference.json). In the historical 0.3.0 pass, [redacted reference]'s settings were observed directly, but no live accepted [redacted reference] prediction, exact prompt, performance comparison, or network-privacy audit was established. Later bounded native/model observations are dated above. The implementation is independent and does not claim proprietary feature/model equivalence.

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

The initial direct-executable GUI smoke inherited its terminal host's permission and was **not** normal-launch authorization proof. A separate LaunchServices doctor initially reported Accessibility denied despite global enablement and valid weights. After the user enabled the app, Inklow-specific macOS TCC logs explicitly reported a saved-code-requirement mismatch: old `e92c98aeb3db2441d9c84347fc452a7367df3cdf` versus current `4186c8bd94732224bc18206da17109865326eaaa`. With explicit user approval, only `tccutil reset Accessibility com.inklow.Inklow` was run. The user then manually added/enabled the current app. **A fresh normal LaunchServices doctor reported Accessibility granted, and the actual normally launched frozen GUI repeated the complete empty-badge/inline/card/real-Tab/secure-read-only smoke successfully.** No permission grant was automated and no other app's permission was reset.

[Observed installation/input/permission receipt](input-integration-repair.json). Source revision: `b39cd0a46dda37c30479d616219fa02323df9249`. [Follow-up three-platform CI](https://github.com/renan-brasilio/inklow/actions/runs/40071249762) passed Windows, macOS, and Ubuntu, including source/settings/native-package/frozen-inference stages. Normal-launch permission remains a separate user-controlled gate.

The user could not find an Accessibility entry in Privacy & Security. Actual System Settings AX metadata confirmed the current permission page is named **Device Control and Data Access**. This is not the Accessibility category in the sidebar. The OS grant remains user-controlled; diagnostic navigation did not toggle any permission.

That input-integration bundle was ad-hoc signed with a hash-specific designated requirement. Rebuilding could invalidate an older approval even under the same app name. The certificate-backed local build below supersedes that signing limitation without weakening the requirement or introducing a private TCC entitlement.

## Icon-only accessory and usable native menus

Source revision `893a8db395f9d46fecd25a042b73d1e82a10d4b9` replaces the field wordmark with a transparent 24-point icon-only accessory. On macOS it sits six points above the text line; top-edge placement uses safe outside-field space or hides the accessory rather than covering the input. Windows/Linux use a 24-point icon four logical pixels above the line, with their own safe edge placement.

The native popup previously allowed controller polling to continue during menu tracking, which could hide the popup's anchor; shortcut interception also competed with menu navigation. The accessory now owns a separate copied menu, polling/shortcut interception pauses while menus track, and the selected action runs after closure against freshly captured focus. The recorder's preexisting disabled-shortcut state is preserved. Qt follows the same menu-session rule, including nested menus.

The actual certificate-signed frozen GUI was exercised against a disposable native text field containing only `cat`: its icon was above the text, native button activation opened the real menu, the popup remained open through two seconds of tracking, Pause persisted, the status menu resumed the app, and Quit terminated it with exit code 0. The fixture text was unchanged. Activation used native accessibility `AXPress`, not physical mouse-event routing. This direct launch inherited the terminal host's authorization and is **not** normal-launch input-permission proof. [Accessory surface](icon-only-above-line.png). [Signed-build/accessory/inference receipt](icon-menu-repair.json).

A separate **normal LaunchServices launch of `/Applications/Inklow.app`** verified the actual menu-bar glyph entirely within an active display, Pause and Resume persistence, continued icon presence through both actions, two seconds of native menu tracking, and actual Quit termination. The launch client exited 0; LaunchServices did not expose the GUI's numeric exit status. [Visible glyph](menu-bar-icon.png), [actual quick menu including Quit](status-menu-with-quit.png), [normal-launch status-menu receipt](native-status-menu-smoke.json).

Thaw had assigned `com.inklow.Inklow:inklow.statusItem` to Hidden. Its exact live Inklow tile received the supported **Move to Visible** accessibility action, which returned 0 and persisted that item in Visible. No global reveal/recovery, automation enablement, other item action, or change to the global new-item default was performed. A comparison of the other persisted section ordering differed; the complete pre-action map was not retained, so preservation of every other persisted ordering has **not** been established. The app keeps its status item registered/visible, but cannot override OS menu-bar auto-hiding, display overflow, or an external manager's later policy.

The current installed executable SHA-256 is `e2b79cc84162901ab55bad18c9f0ae955ca6db830e50ae0c2df4dd84abf8c30d`; deep/strict signature validation passed. It uses an existing Apple Development identity, including the final sealed bundle, with a certificate-backed designated requirement rather than a content-specific cdhash. No private key was exported and no notarization is claimed. Settings bytes were unchanged during installation; model/history data was not removed. Frozen CLI and worker inference passed under the signed build.

**User-controlled permission transition:** a normal LaunchServices `doctor` reported Accessibility denied after the signing-identity transition, and the new normal-launch field smoke could not display an accessory. The first user-reported re-add did not resolve it: Inklow-specific OS logs explicitly rejected the saved old cdhash requirement `4186c8bd94732224bc18206da17109865326eaaa`. With fresh explicit user authorization, only `tccutil reset Accessibility com.inklow.Inklow` was run, and reported success. The user then manually added/enabled the current app in **Privacy & Security → Device Control and Data Access**. No grant was toggled by the assistant; no other app/service was reset.

The **actual installed normally launched GUI then passed the complete field/menu scenario**: 24-point icon above the text, native accessory activation, two seconds of open-menu tracking, actual Pause selection/persistence, a fully on-screen menu-bar icon, actual Resume selection/persistence, and Quit termination. The synthetic `cat` input remained unchanged. [Final normal-launch receipt](native-menu-smoke.json). An initial post-grant attempt closed during tracking; the subsequent uninterrupted, foreground-checked run passed in 10.62 seconds with no intervening source change or app rebuild. This is actual native accessibility-action evidence, not physical mouse-device coverage or model-quality evidence.

Local integrated suite: **252 passed, 1 skipped in 11.26 seconds**. The macOS skip is the intentionally absent Qt module, not a portable pass. [CI run 40077282052](https://github.com/renan-brasilio/inklow/actions/runs/40077282052) passed macOS, Windows, and Ubuntu at the source revision above, including regressions, actual settings/source inference, native packaging, frozen inference, and artifact upload. Windows/Linux remain headless CI evidence, not interactive desktop accessibility certification.

## Native installer release — 2026-10-24

[Published private release `v0.3.0`](https://github.com/renan-brasilio/inklow/releases/tag/v0.3.0) packages source **`510c5fb9dd0b869cec144c367b653630cd4b8d8b`**. It contains an Apple Silicon macOS DMG/ZIP and Linux amd64 DEB/TAR.GZ, plus `SHA256SUMS` and [the actual native receipts](https://github.com/renan-brasilio/inklow/releases/download/v0.3.0/release-manifest.json). The app itself remains named **Inklow**.

| Native verification environment | Regression passes | Skips | Runtime proof |
| --- | ---: | ---: | --- |
| macOS arm64 | 252 | 1 | Real source/frozen CPU inference; native settings; read-only DMG mount, drag-install layout, copied-app signature/version/source checks, visible own-process settings and normal quit; profile/data preservation; detach |
| Ubuntu 24.04 amd64, isolated self-hosted X11/D-Bus container | 275 | 1 | Real source/frozen CPU inference; actual DEB install and portable launch; mapped, visible/enabled AT-SPI settings with the real Linux bridge ready; purge removes program/integration while preserving external settings/model/history sentinels |

The Linux caption failure was a smoke-harness assumption, not an application startup deadlock: Qt exposed `Inklow Settings — Inklow`. The final smoke uses process ownership, native window type/map state, positive geometry and accessible settings controls rather than a fixed caption. Missing GTK theme dependencies on the minimal build host were installed inside the disposable container; build prerequisites now name them explicitly. No production host packages or services were changed, and the owned container was stopped after evidence/artifact extraction.

After publication, **all six assets were downloaded from GitHub**. `shasum -a 256 -c SHA256SUMS` passed for the four native packages and manifest. GitHub's asset digests matched the sealed local files; the release was observed published, not draft, with exactly the intended asset set. The repository remained private.

**Limits:** Windows MSI/EXE packaging builds, but its corrected native installer/runtime smoke has not run; Windows binaries are intentionally unpublished and listed as pending in the README, notes and manifest. macOS distribution is ad-hoc signed, not Developer ID-signed or notarized. Linux verification used X11/Xvfb, not Wayland or third-party interactive typing. The Linux smoke explicitly uses scoped SIGTERM cleanup rather than claiming a normal-menu Quit test. Unfinished multilingual prediction/permission-status experiments were not included, and the installed `/Applications/Inklow.app` was not replaced by this release task.

## Linux model-progress hotfix — 0.3.1 — 2026-10-24

The reported 14% stall was a stale Models dialog, not an incomplete model file. The existing Qwen2.5 1.5B Q4_K_M file was already complete: **1,117,320,736 bytes**, SHA-256 `6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e`, matching the pinned preset. No model was redownloaded, removed, or selected during this repair.

Source commit **`0622e307872fdfcb1f8fc70f86dc5acb5bd6edc0`** gives the Models dialog its own visible-only refresh timer, independent of parent Settings visibility and input/menu polling. Unchanged inventories do not rebuild the dropdown. A regression held a checksum-verified synthetic download at 14%, let the actual downloader finish with its parent hidden and controller polling stopped, and required the dialog to enable its installed-model action without manual refresh. It failed before the fix. The candidate passed the native Linux suite (**276 passed, 1 skipped**) and macOS suite (**252 passed, 2 skipped**).

The actual CachyOS source installation was fast-forwarded to that commit and restarted as **0.3.1** without forced termination. Its environment had no `llama-cpp-python` package; the declared **0.3.36** dependency was then built and installed into the existing application venv with a CPU-only AVX2/FMA/F16C baseline. This did not require system-package, desktop-session, or permission changes.

Actual runtime observations, not an offscreen replacement:

- The visible native Qt Models dialog reported **“Installed; choose Use installed model to select it.”** for **Balanced — Qwen2.5 1.5B Writing**.
- **Use installed model** was enabled; **Cancel download** was disabled. The selected model remained **None**, preserving the existing preference.
- The production CLI used those cached weights in an isolated profile with CPU inference. Synthetic prefix `Thank you for your` produced ` cooperation.`; exit status **0**, complete invocation **1.62 seconds**. This is one cold invocation, not a sustained latency or grammar benchmark.

**Remaining limit:** the real desktop is Wayland. `inklow doctor` explicitly reported global typing unavailable, including XWayland applications. Settings/model readiness and actual inference are verified; system-wide typing in this desktop session is not. No logout, X11 installation, compositor changes, or broader permissions were performed. This is a source hotfix; published 0.3.0 installer assets were not replaced.

## Native KDE Wayland source candidate — 2026-10-24

The candidate's actual production entry point, controller, native client, renderer, AT-SPI adapter, and cached Qwen2.5 1.5B worker ran together on CachyOS, **KWin 6.8.1 / Qt 6.12.0**, inside an owned virtual compositor using llvmpipe. The harness had a private session bus, runtime/config/data directories, and settings profile; it did not replace the user's compositor, read personal fields, select a model in the real profile, or alter KDE's selected input method. A test-only Wayland fake-input client injected keys solely into that owned compositor; production insertion uses the private input-method protocol, not fake input.

The final native run **passed all 27 checks in 11.712 seconds**. This duration covers the whole synthetic scenario, not suggestion latency. Observed:

1. The compositor-provided input-method FD was consumed before QApplication construction and removed from the environment before Qt opened its separate normal display connection.
2. The real model offered ` dig a hole, place ` for `The three steps for planting a tree are to`. KWin reported a visible, nonminimized native input-method window with positive geometry; the native buffer was actually attached.
3. The production quick menu opened with actionable entries and dismissed without discarding the offer.
4. A compositor-delivered Tab produced exactly one actual Qt input-method commit, ` dig `, without forwarding the consumed Tab into the target. Stable field identity survived the changed native revision. Full acceptance committed the remaining `a hole, place `, yielding the exact offered result.
5. Edited revisions, active selections, real password controls, native sensitive-data hints, and changed field focus refused stale insertion. Protected states also refused forced generation and hid the panel. Unconsumed Tab and Shift+Tab reached the real target exactly once per press/release with their original modifiers.
6. Stopping the real agent closed the native client/notifier and terminated its actual inference worker.

Qt's runtime password transition stopped sending further IM state in this fixture, leaving an old native revision observable. The independent AT-SPI protection gate nevertheless refused capture, generation, and insertion. Native sensitive-hint rejection was exercised separately by re-entering an ordinary editable control with its actual sensitive-data hint set; it must not be conflated with the password-mode case.

[Complete synthetic receipt](wayland-native-smoke.json). [Production panel pixels](wayland-native-panel.png) and [actual Qt target after acceptance](wayland-native-target.png) are client-generated images, **not compositor screenshots**; visible mapping is independently established by the receipt's KWin scene rows. KWin attributes the private input-panel connection to its own PID, so the scene observer recognizes the input-method role inside the owned compositor rather than assuming it shares the Python PID.

Integrated regressions: **Linux 306 passed, 1 skipped (12.95 seconds)**; **macOS 280 passed, 4 skipped (11.82 seconds)**. The native C library compiled with `-Wall -Wextra -Werror`. The missing protocol definitions were supplied through a private build SDK extracted from a signature-verified Arch package, not installed into the host OS.

**Limits and deployment state:** this is an unreleased source candidate, not a rebuilt 0.3.0 installer or a new installed Mac build. The exercised target explicitly uses text-input-v3; default Qt text-input-v2 did not supply a complete usable context in this environment. Other applications/compositors, XWayland, full compositor screenshots, actual desktop input-method activation, and installed/frozen Wayland packaging are not established by this run. KDE selection remains a separate explicit user action. Atomic cross-app rewriting remains disabled on Linux. The real desktop and its current model selection were left unchanged.

## Purpose-first KWin context bootstrap — 2026-10-25

The first real-desktop check exposed a connected client whose native context never became ready: automatic polling ran, but no field-value read or suggestion occurred. On the owned active text-input-v3 field, asking KWin to resend its already-active context made automatic generation and actual word/full acceptance work. This was a missing-state bootstrap, not a model download or model-quality failure.

The client now reports incomplete initial state without exposing text. The bridge asynchronously checks KWin's existing active/supported context and requests its authoritative state, first establishing purpose and then obtaining safe surrounding state when necessary. Each phase is bounded to the same context lifetime; stale, stopped, inactive, unsupported, ready, and known-sensitive states do not continue the bootstrap. Private-socket, purpose, AT-SPI pre-read rules, exact snapshot matching, and confirmed-insertion checks remain in place. Selection and visibility mode are not changed.

The final integrated Linux suite passed **318 tests, 1 skipped in 12.77 seconds**; macOS passed **280 tests, 16 skipped in 11.56 seconds**. The additional Qt-only cases exercise eligibility after both bootstrap phases and rejection after focus changes, sensitive-state transitions, unsupported/inactive replies, and teardown.

[The bootstrap native receipt](wayland-bootstrap-smoke.json) passed all **27 real protocol/UI/privacy/acceptance checks in 21.24 seconds** in the private virtual compositor. Its GUI observer now uses genuinely asynchronous requests; synchronously waiting for the agent while it queried the target's AT-SPI endpoint produced a test-only wait cycle. The target event loop remains free during those reads. No native model, insertion, or key-forwarding behavior was mocked, and no new installer publication is established by this run.

## Approved actual-desktop activation — 2026-10-25

After explicit approval to enable the candidate, source **`22e05e6b56de580414c59289db4c79429fd62282`** was installed in the existing CachyOS source checkout and its native library compiled there. The cached checksum-verified Qwen2.5 1.5B model was reused. The KDE input-method entry was selected without changing desktop sessions or installing system packages.

The **clean, uninstrumented installed application** then passed an actual-desktop check in **9.7 seconds**. The owned synthetic Qt/text-input-v3 field generated a suggestion automatically, with no force command, manual context resynchronization, diagnostic launcher, or observer monkeypatches. KWin reported a visible native input-method panel. A focus-checked compositor Tab committed exactly ` dig ` and was not forwarded into the target; the real above-Tab key committed `a hole, place the tree in the `. The final field was exactly `The three steps for planting a tree are to dig a hole, place the tree in the `. This is a functional sample, not a language-quality or latency benchmark.

[Actual desktop receipt](wayland-desktop-smoke.json), [target after native acceptance](wayland-desktop-target.png), and [clean activation/tray/data-preservation receipt](wayland-desktop-activation.json). The image is the owned Qt widget's generated pixels, not an unrestricted desktop screenshot; compositor mapping is separately recorded in the receipt.

The running application had a registered **Active** status item titled **Inklow**, with **Quit Inklow** in its real exported quick menu. Normal Quit was exercised during installation without forced process termination; the final clean instance was left running. Its launcher points directly to `python -m inklow --background --wayland-input-method`, not a diagnostic wrapper. Temporary diagnostic launchers and desktop-test scripts were removed.

The private rollback retained the original profile and KDE configuration. Comparing existing JSON preference keys found **no changed values**; the only changed KDE configuration entry was **Wayland:InputMethod**, the explicitly approved selection. The selected cached model remained the 1.5B preset; no 7B promotion, model redownload, Mac replacement, or new installer release occurred. GUI tests briefly focused only the owned synthetic window. An initial desktop observer encountered KWin's list-length-derived script-ID collision; its owned observer was unloaded and configured scripts restarted through their manager without changing plugin configuration. Subsequent desktop observations used unique owned plugin names rather than numeric script-object identity.

Compatibility remains bounded to the exercised KDE/text-input-v3/AT-SPI path. Other applications, all languages, other compositors, and Windows installation are not certified by this result.

