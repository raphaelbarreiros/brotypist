# Feature-preservation acceptance register

Baseline: `6eaeed5fe56e1ea8dcf878b1b7498f922c26fd59`, 0.4.13. All rows are **required scope, not completed Rust work**. Source references below use `src/inklow/` unless another prefix is given. A safe unsupported outcome must remain unsupported until separately implemented and verified; do not replace it with a popup, guessed coordinates, clipboard rewrite, or synthetic success.

Each implementation packet names its row IDs. Closure records exact revision, executed scenario, observed result, platform, and limits. Unit/source tests alone cannot close native visual, permission, installation, or host-edit gates.

## Shared behavior

| ID | Existing contract | Rust acceptance evidence |
|---|---|---|
| P01 | `core.py:88-300`: exact original text; Unicode-scalar caret/selection; generations; typed-through advancement; suffix preservation; word/full acceptance; punctuation and trailing separators. | Python/Rust synthetic vectors for astral characters, NFC/NFD, combining marks, CJK, whitespace, suffix, visible-word boundaries and punctuation-only remainders. Explicit checked conversion to UTF-16/UTF-8 native offsets. |
| P02 | `runtime.py:238-348,551-616,1280-1432`: global/pause/default-app gates, debounce, minimum input, on-demand fields, capture revisions and tool precedence. | Deterministic event traces with injected logical clock; app/window/domain/capability changes; rapid typing; no stale result resurrection. Include R5-R7 from review. |
| P03 | `runtime.py:638-1050,1601-1865`: latest-only preparation, authorization around reads/submission, drawn-prefix/choice acknowledgment, one-use input claims and replay. | Superseded preparation, cancelled model stream, hidden/refused presentation, revision changes, repeated Tab, event replay and shutdown races. No host edit before fresh native authority. |
| P04 | `features.py`, `runtime.py:1384-1581`: explicit/automatic alternatives; selected synonyms; colon/emoticon emoji; real tone/gender variants; local typo suppression/fixes; Escape/bypass. | Preserve catalogue bytes/order, exact selection and replacement boundaries, anchored alternatives versus ghost Tab behavior, abstention, decoded-output safety, dismissed-tool suppression and append-only capability. |
| P05 | `spelling.py`, `native/macos_spelling.py`, `native/macos_language.py`: asynchronous dictionary/language plans, pending vs unavailable, Unicode range mapping, code/identifier exclusions, advisory language detection. | Native dictionary adapter plus independent boundary vectors; unsupported language does not silently become English; any accepting dictionary vetoes typo. No normalization of inserted/raw authored text. |
| P06 | `shortcuts.py`, native hook owners: canonical saved chords, physical Above-Tab, exact modifiers, injected/AltGr refusal, bypass Tab, conflict handling. | Saved mapping compatibility, non-US layouts, consumed press/release pairs, inactive/hidden suggestion pass-through, replay order and bounded owner shutdown on each OS. |
| P07 | `config.py:114-263,285-354`: every saved setting, strict validation, old-setting migrations, app/domain rules and inheritance. | Old-profile load/save round-trip without consent/model/timeout changes; malformed settings fail visibly rather than reset. Preserve master vs default-app vs persisted rules vs session overrides; IDNA domain matching and exact app IDs. Derived default-full case folding matches baseline Unicode 15, not the dependency's older Unicode 9 tables. |
| P08 | `storage.py:24-32,122-326,391-497`: encrypted history, authored spans, OS keychain, lock/corruption/lost-key handling. | Python-to-Rust and Rust-to-Python synthetic ciphertext compatibility, locked-store refusal, corrupt-record latch, missing-key refusal with existing data, authored/generated separation and scalar-offset bounds. Never plaintext fallback. |
| P09 | `storage.py:327-497`, `runtime.py:2101-2168`: optional local style/vocabulary, unaccepted-session consent, scoped deletion/unlearning, separate statistics. | Same app/exact domain/confident language; bounded authored evidence; no raw sentence replay/training. Delete cancels in-flight/derived context and avoids immediate recreation. Keep separate history/statistics deletion and key retention. Stats period/grouping/active-day/time-estimate behavior remains. |
| P10 | `system.py:288-465`, `runtime.py:638-982`: independent clipboard, AX window, OCR context and appearance consent; fresh bounded references. | Explicit independent opt-ins; focus/privacy rechecks before/after reads; blocked AX cannot trigger OCR fallback; timeout cannot reuse stale reference; clipboard GUI-thread marshaling; references are untrusted data, not instructions. |
| P11 | `power.py`, `system.py`, `appkit_power.py`, `runtime.py:788-858,1215-1259`: unknown/charging/battery state, on-demand, shorter completion, installed alternate model, idle unloading/residency. | Unknown is not low battery; installed-only alternate selection; default 300-second unload and explicit zero residency preserved; preparation generates/evaluates no writing. Mac low-battery help once per discharge at <=20%, real AC rearm, nonactivating presentation and reduced-motion/contrast behavior. |
| P12 | `instance.py`, `desktop.py`, `runtime.py:2183-2249`: profile lock, close-to-tray, guarded Quit, independent retryable cleanup and no-tray recovery. | Second instance isolation, failed/pending cleanup and repeated Quit; workers/hooks/background reads all actually exit; unsaved Save/Discard/Cancel preserved. Source-profile isolation must not register real autostart. |

Settings groups include backend/path/endpoint/model/acceleration/prompt, context/token/length/debounce/minimum/inside-text, acceptance whitespace/punctuation, shortcuts/Escape, startup/onboarding/accessory/updates, all context consents, terminal opt-in, spelling, instructions, recording/unaccepted sessions/learning strength, statistics, emoji preferences, alternatives/synonyms modes/delays, battery, exclusions and per-app/domain enabled/Tab/history/instructions/compatibility/code-editor rules. A config struct containing fields without functioning controls/callers is not parity.

## Models, providers and workers

| ID | Existing contract | Rust acceptance evidence |
|---|---|---|
| P13 | `models.py:22-328`: complete pinned catalogue, imports, capacity checks, license gates, progress/cancellation, atomic verified downloads. | Preserve IDs `gemma4-e2b`, `gemma4-e4b`, `gemma4-26b-a4b`, `gemma3-1b`, `gemma3-4b`, `qwen3-1.7b`, `qwen3-4b`, `qwen3-8b`, `qwen3-30b-a3b`, `balanced`, `tiny`; repository revisions, filenames, sizes, hashes, licenses, prompt styles and memory floors. No automatic acquisition/selection; cancelled/corrupt/stalled transfer preserves previous weights. Imports reference user files. |
| P14 | `providers.py:419-575`, `writing.py`: local GGUF on CPU/auto, supported Gemma/Qwen/MoE families and custom continuation; handwritten family prompts, sampling and token fitting. | Pin native llama.cpp revision initially; preserve tokenizer special/data span behavior, sampler order, EOG/stops and context/batch/KV semantics. Exercise cached Gemma4 and Tiny first; family compatibility and unavailable large-model execution are separate documented gates. Unsupported guided architecture fails visibly. |
| P15 | `providers.py:19-123,145-320,323-417`, `writing.py:132-263`: trusted framing/untrusted data, healing, cumulative streaming, output fences, reference/token budgets, alternatives/utilities. | Literal structural strings remain data; every healing chunk split; wrong/incomplete replay; UTF-8 assembly; suffix echo; decimal/CJK sentence boundaries; mandatory prompt overflow; 0-3 distinct anchored alternatives; utility abstention and decoded line/control/structural rejection. No exact generated-word identity required from probabilistic output; deterministic token/safety contracts are compared. |
| P16 | `providers.py:582-729`, `config.py:265-281`: Ollama and OpenAI-compatible completion servers in addition to GGUF. | Literal loopback HTTP(S) only; reject credentials/query/fragment/DNS/remote host/port zero. Proxies and redirects disabled. Preserve NDJSON/SSE framing, response bounds, timeout/cancellation behavior, path construction, `raw:true` and `keep_alive:0`; test with owned loopback servers. No cloud substitute. |
| P17 | `process_client.py`, `process_worker.py`: isolated model process, configuration-only prepare, ready/proceed barrier, epochs, latest pending request, cancellation grace, release watermark and tracked child teardown. | Line-delimited UTF-8 JSON <=256 KiB including newline; matching IDs; loading/prepared/ready/result/final/error/done; malformed frames; no draft evaluation before proceed; cold-load supersession; prompt/stream cancellation; model-switch/release races; zero/default idle; real child clean exit. Preserve field bounds, not merely message names. |
| P18 | `preview.py`, `appkit_welcome.py`, `qt_ui.py`: isolated owned-field real inference, selected pending-model readiness, ephemeral draft and no external history/context/insertion. | Actual EN/PT owned preview renders and widget readback on acceptance; changing focus/selection/IME cancels; choose/download/import/license/cancel/Continue flows; selecting a model alone neither predicts nor borrows another model's readiness. |

Existing worker bounds: prefix/suffix/context 8,000 characters, instructions 26,000, personalization 2,048, utility 24,000, anchor 4,096, word cap 1-32; positive IDs and exact booleans. Check the baseline implementation when freezing fixtures rather than inferring Rust byte limits from Python character limits.

## Native adapters and UI

| ID | Existing contract | Rust acceptance evidence |
|---|---|---|
| P19 | `native/base.py`, `native/macos.py`: policy-first AX observation, secure/hidden/protected/IME refusal, exact native browser document identity, field-local UTF-16 mapping, privacy before values. | Source vectors plus actual signed-app AX capture/refusal with owned fields; unavailable Secure Input blocks. Bounded traversal: 128 nodes/depth 8/50 ms/4,096 mapping-work units. Missing website identity fails closed when exclusions require it. |
| P20 | `native/macos.py`, `appkit_overlay.py`: CoreText/AX verified row/font/baseline/clip, exact visible text acknowledgment, nonfocus overlay/chooser, Spaces/display changes. | Signed app produces real grey same-line pixels, never popup fallback; hidden/order-refused text cannot arm acceptance; partial-visible words/complete choices; mixed scale/Space/appearance changes; outside field focus unchanged. Guarded suffix displacement cannot overlap unknown host content. |
| P21 | `native/macos.py:1309-1708`: native insertion/replacement, modifier/permission/focus revalidation, Unicode event chunks and queued-input replay. | Actual host readback and undo, word/full/repeated Tab, focus race, rejected/uncertain mutation, append-only Electron ordinary completion with emoji/synonym/typo replacement refused. No automatic retry after possible mutation; no whole-document/clipboard replacement. |
| P22 | `native/macos_terminal.py`, `macos_docs.py`, `macos_vision.py`, `macos_language.py`: special integrations and context/appearance privacy. | Terminal explicit opt-in, nonprompting PID-addressed Automation, exact signed Codex 0.162.1 renderer, one-use append lease; refuse shells/iTerm/multiplexers/commands/attachments/other builds. Docs action configures screen-reader+braille with verified readback; metadata is not writable caret support. Optional window-only capture/OCR, transient images, expiry/cancellation/revocation, fixed diagnostics; no claim of protected sibling pixel redaction. |
| P23 | `native/windows.py`: UIA/COM apartment ownership, native Edit UTF-16 offsets and undo, provider-range refusal, GDI inline metrics, native hooks. | Real Windows constrained/rejecting fields, selection/caret/emoji/IME/password/UIPI refusal, actual insertion readback and paired shortcuts. Exact existing UIA selections may replace; arbitrary UIA range movement remains refused. Native build/test lane required; cross-compile is insufficient. |
| P24 | `native/linux.py`: X11 AT-SPI capture/privacy/composition, Pango inline evidence, insertion-only edits, X11 grab/conflict/replay. | Real X11 fields and session D-Bus; explicit inactive composition evidence for inline; byte-length/scalar conversion; no delete-then-insert rewrite; conflict refusal and input replay/cleanup. |
| P25 | `native/wayland.py`, `wayland_client.c/.h`: KDE private input-method transport and guarded manual controls. | Real KWin inherited socket captured before toolkit init; no public-display fallback; safe purpose/excerpt agreement; input pairing and forwarding; confirmed commit; SHM buffer/deferred destruction/zeroization; transport loss. **Ordinary inline/preedit remains disabled**. Other compositors and universal text-input versions are separate work, not implied by Rust. |
| P26 | `appkit_ui.py`, `appkit_welcome.py`, `appkit_power.py`: complete native Mac product surface. | Setup, General, AI Models, Context, Personalization, Emoji, Shortcuts, Battery, App Settings, Labs, Statistics, About, Support; six-group resident menu and minimal field menu; captured app/site action intent; dirty instruction editors across panes/resizing; actual shortcuts/VoiceOver identifiers; model and updater state controls; Finder reopen and navigation-only URL scheme. User correction: no battery card/percentage/charging decoration or diagnostic/model dashboard headers in menu/status-item presentation; retain Battery Settings/power policy. Grouped native rows, titlebar clearance, visible appearance-aware selection and reachable scroll content are acceptance boundaries, not merely control existence. Preserve DESIGN identity but current user instructions win over stale examples. |
| P27 | `qt_ui.py`: complete portable Settings/Welcome/tray recovery surface. | General, AI Models, Autocomplete, Languages, Privacy, About, six-step Welcome, explicit Save/Revert rebasing concurrent changes, clear Quit without tray, model license/progress/cancel and updater. If replacing the toolkit, reimplement every control and focus/keyboard/accessibility contract; a new visual theme is not part of this migration. |

## Persistence, distribution and completion

| ID | Existing contract | Rust acceptance evidence |
|---|---|---|
| P28 | `resources.py`, `system.py`, `config.py`, `storage.py`, `models.py`: external identity/data continuity. | Retain `com.inklow.Inklow`, existing autostart/Windows Run `Inklow`/KDE desktop identity, signer/team/designated-requirement compatibility, Automation declarations and grants. Bundle-name equality alone does not prove TCC retention. Compare explicit platformdirs baseline paths, not Rust-library defaults. |
| P29 | `updates.py`, `update_keys.py`: pinned Ed25519 publisher identity and exact signed metadata; explicit download/install intent. | Existing schema, signed bytes/key, duplicate rejection, platform/version/commit, HTTPS redirect policy, streamed hash/size, private staging, cancellation, daily-check throttle and single-worker UI intent. Tampering/wrong OS/oversize/truncation refused; no embedded publisher secret. |
| P30 | `update_install.py`, `tools/package_*`, `BroTypist.spec`: offline updater, startup ACK/rollback, bundle/install ownership and uninstall preservation. | Same-filesystem owned destination, complete archive/native signature/architecture/provenance validation; approval plus actual parent exit; atomic exchange; nonce-bound real GUI-loop ACK; healthy and failed/delayed-start rollback; quarantine retained. Managed Debian uses package manager. Stable Windows per-user MSI GUID family. Real DMG/ZIP, DEB/portable and MSI/ZIP install/quit/remove checks preserve external settings/models/history. |
| P31 | `__main__.py`: full command-line surface and launch overrides. | help/version/doctor/complete/model list+download/license/no-select/update check+download; background/login/no-tray/paused/settings/panes/model-path/prompt/acceleration; KDE install/private launch; private worker dispatch. Launch overrides remain memory-only, explicit model selection persists. Models destination reachable. |
| P32 | `.github/workflows/ci.yml`, `tools/benchmark_writing.py`, existing native smoke tools and evidence. | Three native build/test lanes, actual model worker and own-window UI checks, target installers, recorded unsupported/held outcomes; deterministic golden/behavior suite; manual signed-host readback gates. Report latency/memory only when measured with equivalent scenarios; no universal typing or language-quality guarantee. Final shipped application and updater do not require Python/PySide/PyObjC/PyInstaller. |

## Exact compatibility bytes

- Configuration: `settings.json` at `INKLOW_HOME` or baseline `platformdirs.user_config_path("inklow", appauthor=False)`; model cache uses `user_cache_path("inklow", appauthor=False)/models` independently. Resolve and compare paths on every OS before any real-profile write.
- Database: `local.sqlite3` beside settings; existing `history(id TEXT PRIMARY KEY, sealed BLOB NOT NULL)` and `statistics(day, app, completions, words, characters)` keys/semantics.
- History: 12-byte nonce followed by AES-256-GCM ciphertext/tag; AAD `b"inklow-history-v1" + id.encode("utf-8")`; UTF-8 JSON with exact original text and scalar-authored spans.
- Credential service `Inklow`; account `history-v1-` plus first 24 hex SHA-256 characters of the resolved config-directory string; base64 32-byte key. A copied profile at a new path does not automatically address the original credential.
- Preserve build ownership marker `.inklow-build.json`, version/source/platform/architecture, `BroTypist` executable identity and URL scheme; Linux owned desktop markers and Windows installer upgrade family.

## Platform status must remain honest

macOS 15+ Apple Silicon is the primary supported target. Linux x86-64 remains experimental and requires actual compositor/session evidence. Windows has source implementations and dormant packaging but held publication. Current Windows/Linux capability gaps are not permission to omit their implemented features from conversion, nor a promise of new universal native support. Publication, installing over the normal app, notarization, new compositor support and iCloud/cloud features require separate authority/evidence.

## Bounded local preservation evidence

These observations supplement the required rows above; they do not close native
field, physical-input, permission, installer or release acceptance. The working
candidate is uncommitted. Each receipt records the relevant source/artifact hashes.

| Rows | Executed scenario | Observed result and limit |
|---|---|---|
| P05 | [Actual NSSpellChecker adapter](../../evidence/rust-migration/native-dictionary-current.json), fixed synthetic strings, exact regional selection and UTF-16 ranges | 22 structural checks passed. `en-GB` and `pt-BR` were exercised; exact `en-US` was unavailable and was not replaced by generic English. Actual suggestions were observed, not scored as model quality. No asynchronous-service or native-field acceptance claim. |
| P08 | [Independent Python/Rust AES-GCM and record-format interoperability](../../evidence/rust-migration/storage-interoperability-current.json) | 277 checks passed across EN/PT/NFD/non-BMP fixtures, both encryption directions, actual `HistoryStore` decoding and authentication refusals. Injected synthetic keys only; no OS-keychain migration or normal-profile access. |
| P08–P09 | [Standalone authenticated learning-store execution](../../evidence/rust-migration/learning-store-current.json) | 18 scenarios passed, including authored-only evidence, replacement, scoped consent/deletion, locking, tampering, retention atomicity and owned cleanup. No native authorship, model-quality benefit or automatic desktop lifecycle claim. |

## Current per-identifier reconciliation

This is a disposition of the preservation inventory, **not closure of native
acceptance**. Source/test seams below are in `crates/brotypist/src/` unless noted;
they identify implemented behavior and regression coverage, not an additional
execution claim. Earlier [Tab/emoji source gates](../../evidence/rust-migration/tab-emoji-current/receipt.json)
passed **743 reported Mac passes: 738 parent-suite plus five child-invocation
passes, 26 result summaries, one ignored**; locked/offline workspace/all-targets
`native-smoke` tests, binaries/examples and formatting passed. This is not a
distinct-scenario count. No native Windows/Linux run was added by this wave.
The [remaining-Tab follow-up](../../evidence/rust-migration/tab-followup-current/receipt.json)
adds safe same-row calibration, bounded dispatched-readback reconciliation and
the user-approved WhatsApp field-session reservation. The user confirms the
reported Tab and emoji bugs are fixed; seven exact edits and two no-offers were
retained in the final Tab run. This bounded result does not close the full
host/security/IME/release matrix or upgrade earlier failed candidates.

Historical [locked/offline RAM-unit gates](../../evidence/rust-migration/ram-units-current/receipt.json)
passed **735 reported macOS test passes** (730 parent-suite passes plus five child-
invocation passes; 26 result summaries, one ignored) and binaries/examples.
Linux ARM64 workspace/all-targets tests (569 library passes, one ignored,
plus other suites) and binaries/examples passed. The earlier 733-pass and 721-test Mac checkpoints remain
historical. These are local uncommitted-source checks, not signed/release
attestations or current native Windows execution.

Evidence lanes used below remain separate:

- **C — controlled Controller:** owned NSTextView, controlled provider, real
  WorkerClient/Controller/OverlayOwner, exact choices, word/full acceptance,
  invalidation/refusal and two native wake cycles with no routine post-submission
  polls. Not production external AX, physical keys or persisted authorship.
- **A — genuine AX adapter:** hookless MacBridge against an authenticated owned
  child, independent Unicode selection/edit readback and stale/foreign refusal.
  Not Controller/global hooks, Secure Input toggling or arbitrary external hosts.
- **M — ordinary owned-model:** real Gemma 4 E4B worker and owned overlay;
  **overall gate false**, two Portuguese positive draws/full-prefix acceptances
  and two English abstentions. No positive English/word-acceptance coverage.
  This is `--owned-model`, not canonical Welcome. Canonical `--owned-welcome`
  aborted on input interruption before model requests/edits, with empty cases
  and clean Controller shutdown. Its positive gate is failed/unexercised,
  not a permission/model failure or new bilingual pass.

C/A/M now have a [durable, separately scoped observation record](../../evidence/rust-migration/native-owned-observations.json).
Their older runs lack frozen whole-tree/executable attestation; publication of
the observations does not turn them into release evidence. Canonical Welcome
has its own pre-launch identity and failed input-interruption receipt. The
reproducible scope boundary remains in [the native harness](../../crates/brotypist/examples/native_desktop_smoke.rs).
Durable owned evidence is linked in the rows below or in the bounded P05/P08/P09
table above. Historical receipts retain their recorded source identities; they
do not verify this working candidate.

**L — Linux full native shell:** the [current ARM64 Xvfb receipt](../../evidence/rust-migration/ram-units-current/receipt.json)
passes 99 assertions, stage 45 and clean canonical Quit on current source.
It exercises actual English word/Portuguese full preview acceptance with GTK
readback, six-step Welcome, independent consent dialogs, model-selection
success/failure/cancellation, exact dirty Unicode instruction retention,
external/live rebase and concurrent updates across panes/resizing, guarded
close/Quit Cancel, and persistence failure/recovery. Three added assertions
exercise the real Models dialog/runtime catalog: unspecified memory is not
numeric zero, positive memory is retained, and browsing preserves runtime/saved
configuration and the Settings draft. No installation fixture, download request,
license consent or saved model selection is introduced by these checks.
The [earlier 96-assertion receipt](../../evidence/rust-migration/linux-native-shell-latest/receipt.json)
remains historical, with four parent-viewed images:
[EN preview](../../evidence/rust-migration/linux-native-shell-latest/preview-english-visible.png),
[PT acceptance](../../evidence/rust-migration/linux-native-shell-latest/preview-portuguese-accepted.png),
[independent consents](../../evidence/rust-migration/linux-native-shell-latest/privacy-independent-consents.png),
[compacted choices](../../evidence/rust-migration/linux-native-shell-latest/choices-eligible-subsequence.png).
The [pre-wave shell receipt](../../evidence/rust-migration/linux-native-shell-current/receipt.json)
is retained separately. Native UI delivery does not imply universal platform,
physical input, real IME, external-host, KWin, Windows or installer acceptance.

Latest scoped evidence supplements, rather than upgrades, the older lanes:

- [AX reuse](../../evidence/rust-migration/ax-read-reuse-current.json): actual
  frame acquisitions 3 → 2 for three requests and identical capture, independent
  native widget readback, fresh appearance/focus/anchor barriers retained.
  No latency, physical-input or full OS write-confinement claim.
- [Generated emoji notices](../../evidence/rust-migration/emoji-notice-current.json):
  Unicode emoji 17.0 and full Unicode License V3 actually appear; existing
  Cargo/native notices remain. No packaging or legal certification.
- [Word tools](../../evidence/rust-migration/word-tools-current.json): 80 fixed
  public-API checks (21 boundaries, 23 emoji, 18 decoded choices/ordinals,
  18 pure acceptance), not model semantics or native insertion. No trigger
  customization adopted.
- [Current target-owned GTK3/Qt6 clients](../../evidence/rust-migration/target-owned-client-current.json):
  compact ordinal 2, GTK pre-paint refusal and Qt selected/collapsed NFD/non-BMP
  replacement/readback/undo pass with synthetic input, not physical IME.
- [Stock AT-SPI](../../evidence/rust-migration/stock-atspi-current.json):
  four exact native captures, eight mutation refusals, unchanged text/carets,
  zero insertion attempts/confirmations. Missing inactive-composition,
  inline-render and range-rectangle evidence does not grant insertion.
- [Portable Windows decisions](../../evidence/rust-migration/portable-windows-decisions-current.json):
  earlier Mac/Linux gates covered generic runtime selected-replacement/readback,
  but excluded the Windows-only nested pure decision module. All 12 tests now
  compile and pass on both hosts through one canonical
  `platform::windows_decisions` under `cfg(any(target_os = "windows", test))`.
  Native Windows UIA, hook execution/lifecycle and MSVC/SDK remain unverified.
- [RAM units and Mac rendering](../../evidence/rust-migration/ram-units-current/receipt.json):
  zero/missing minimum memory is unspecified; Mac omits the line and portable
  surfaces show unavailable. Common allocation-free `ram_label` formatting uses
  two-decimal binary GiB for catalogue/detected RAM and insufficient-memory
  availability warnings; positive fractions are not integer-truncated (8.50 GiB,
  0.50 GiB; 1,000,000,000 bytes → 0.93 GiB). Physical RAM stays separate and unknown
  hardware stays unknown. Byte eligibility, capacity detection and download/transfer
  formatting are unchanged; no hardware-fit promise follows. Mac 86 captures/117
  geometry observations use explicit catalogue availability fixtures, not filesystem
  availability or model loading. Gemma's minimum now displays 8.00 GiB while its
  download remains 2.8 GB. Native NSView bitmaps are not full shell or WindowServer
  acceptance. The [historical catalogue-memory receipt](../../evidence/rust-migration/catalog-memory-current/receipt.json)
  preserves the pre-unit-fix 8.6 GB Mac observation; earlier raw-byte labels are historical.
- [Pre-NFC publication frontier](../../evidence/rust-migration/publication-frontier-current.json):
  192/192 texts/mechanical outcomes and 269/269 native-call outputs/seeds/non-timing
  work preserved at the historical **175/192** absolute baseline. Its three
  missing-`o` NFD joins are fixed by the correction below; 14 no-offers remain.
  Current full64 retains 117 known-terminal emissions before native return and
  211 exact nonempty finish-offer agreements, zero mismatches, over 269 calls.
  Ordering/bytes are observed, not compositor or latency acceptance.

### Accepted NFC replay correction and remaining gates

[NFC healing](../../evidence/rust-migration/nfc-healing-current.json) normalizes
only model replay after removing the exact authored suffix. Original NFD author,
reference and suffix bytes stay unchanged; grammar, replay allowance and output
stripping share the returned NFC replay. Original v19/v22 layout remains, not
all prompt bytes. No whitespace prohibition, language special case, rearrangement
or new preset is introduced. Full64 improves **175 → 178/192**, exactly three
target improvements, 189 unchanged texts/mechanical outcomes and zero regressions.
All 52 named long controls retain required outputs, non-timing work and seeds;
four publication callback-count changes belong only to that long comparison.
This does not assert every source hash matches the older long baseline.

[Remaining no-offer analysis](../../evidence/rust-migration/writing-no-offer-analysis-current.json)
accounts for all 14 unchanged absolute-quality failures: ten replay-only stops
and four replay-plus-comma stops, without execution errors or useful insertion
removed by cleanup. These are measured model abstentions, not external blockers
or proof of unfixability; no further production bug was justified.
[Post-NFC cache calibration](../../evidence/rust-migration/nfc-cache-calibration-current.json)
passes all 55 prompts/three gate classes. Direct healing passes 3/3 and
[historical short-probe worker targets](../../evidence/rust-migration/nfc-worker-healing-current.json)
pass 3/3, but that old command retains exit 1 and three already-completed,
inconclusive after-result probes. Before-proceed passed 3/3 without later outputs.
Current corrected-clock cancellation evidence below supersedes, not rewrites, it.

Current Linux ARM64 all-target tests/build and full native-shell 99 assertions
pass on the corrected source; earlier 96-assertion receipts remain historical.
These do not close the 15 held acceptance gates
or establish 192/192 quality or roadmap completion. All six layout/cache
candidates plus the fixed reminder remain rejected; saved-state failures remain
23/80. GPUI is not adopted; ordinary Wayland inline is disabled and platform
release holds remain.

### Current follow-up parity register (2026-11-07)

- **P17 cancellation:** [corrected-clock receipt](../../evidence/rust-migration/canonical-cancellation-current.json)
  records exit 0, both probes passing in all three sessions, three NFD target passes
  and acknowledged release/shutdown. The after-result probe alone uses a fixed
  ordinary garden-handover workload at 128 tokens/32 words, observing a nonempty
  nonfinal IPC result while the matching supervisor request is active; ordinary
  cases retain 24 tokens/medium length. Matching accounting, quiescence and
  retirement pass, not native interruption: native acknowledgement/interruption
  remain null and consumer suppression cannot prove producer stopping. Diagnostic
  observation/retention clocks now follow lock waits; operational grace/idle,
  ownership and accounting are unchanged. Both regressions passed in the Mac
  source gate (732 parent + five child = 737 reported passes, 26 summaries,
  one ignored; binaries/examples built).
- **P15 boundary/replay:** [no-healing experiment](../../evidence/rust-migration/no-offer-boundary-replay-current.json)
  is rejected. Both arms complete 54 requests without stream/projection errors,
  with 19 no-offers each. Original target-boundary no-offers improve 14/15 → 1/15,
  but trailing-space cases regress 0/21 → 18/21 and all three protective partial
  words split `feedback` into `feed back`. Full authored prefill changes replay
  metadata/grammar/stripping/reservation together, not grammar alone. The 8,000-byte
  selector with synthetic context wrapping and cold KV-cleared scheduling differs
  from retained full64. Japanese ASCII space is diagnostic, not natural-editing
  guidance. No production adoption or full64/long/per-keystroke adoption pass.
- **P14/P15 semantic evidence:** [assistant review](../../evidence/rust-migration/semantic-review-current.json)
  covers 89 unique IDs/all 192 occurrences, with 337 dimension labels (277
  acceptable, 25 unacceptable, 11 uncertain, 24 not applicable). Generated,
  mechanical, native and timing fields remain unchanged, including **178/192**
  and 14 no-offer failures. Blind second review of 29 flagged IDs plus six controls
  produces 143 agreements and 13 unadjudicated disagreements across 156 dimensions.
  Both sets remain; this selection-biased same-model audit is neither human
  validation nor population calibration. Agreed language leakage, unsupported
  timing and Portuguese synonym defects prevent any inference of universal quality.
- **P24/P27/P32 emulation:** [Linux AMD64 receipt](../../evidence/rust-migration/emulated-linux-current/receipt.json)
  adds actual x86-64 native application execution: 99 full-shell assertions,
  stage 45, clean exit, no error under Xvfb/Openbox/Picom. Locked/offline serial
  all-target source tests and bins/examples passed: 693 parent + six child =
  699 reported passes, 27 summaries, two ignored. The first concurrent library
  run retains 568 passes, three EAGAIN/eight-second UnixStream read timeouts and
  one ignored, without changed assertions/deadlines or cgroup PID/memory/OOM
  events. This is not merely a cross-build, but also not native Windows, physical
  typing, installer or release acceptance. **All 15 gates remain held**, and
  ordinary Wayland inline/preedit remains disabled.

### Tab/emoji follow-up parity (2026-11-08)

[Earlier receipt](../../evidence/rust-migration/tab-emoji-current/receipt.json):
permanent Tab regressions renew ordinary debounce only after verified Word/All
remainder exhaustion; retained remainders remain receipt-bound and true no-offer
results stay quiet. Six word accepts across two generations cover exact text/
caret, fresh paint, one-use claims and ordinary Tab pass-through. Corrected red:
110 runtime passes/two new failures; green: 112 runtime passes.

The final WhatsApp-only reservation persists after verified acceptance through
idle and genuine no-offer, until input, caret/focus, default Escape or policy/
safety revocation. Other apps retain the two-second continuation guard. A
reserved key cannot mutate, retry generation or accept/disarm later paint.
Pending native-readback confirmation remains separately bounded to two seconds.

`Snapshot.can_replace_emoji` defaults false and is key/policy/readback-bound.
Emoji-only admission/acceptance calls `NativeBridge::replace_emoji_range`;
ordinary replacement, synonyms and typo correction are unchanged. Native
WhatsApp's dedicated fallback requires the exact safe API capabilities, checked
scalar-to-UTF-16 selection, independent unchanged-text/exact-selection/authority
recapture and fresh foreground/field/window/IME/revision checks before one
PID-targeted event pair. Possible mutation after selection is `Indeterminate`,
never rejected-as-untouched or replayed. Only a dispatched dedicated emoji edit's
exact selected-only intermediate may remain pending under existing deadline,
revision, authority, privacy and policy checks; exact final glyph/text/caret
confirms once. Permanent runtime/native/core regressions cover stale, unrelated,
revoked, protected, expired and paused states without revival or capability
expansion. Ordinary append and append-only Electron limitations remain unchanged.

Actual expanded `--owned-choices` passed eight acceptance cases and retained
refusal/ghost-lifecycle checks: real WorkerClient/Controller/OverlayOwner,
ZWJ/alias/singleton emoticon, draw/typed receipt before real NSButton ordinal
1 → choice 0, exact independent readback, one edit, duplicate refusal and no new
provider request. The singleton PNGs were inspected. New explicit `--owned-emoji`
passed two real MacBridge/authenticated AppKit-child range replacements with
actual read-only AX selected text and writable range/value: synthetic `:coder:`
→ `👩‍💻` and `:-)` → `🙂`, astral/NFD prefix/suffix preserved, one marked event
pair/one native text change, exact independent UTF-8/UTF-16 text/caret. The first
case is backend range proof, not public-shortcode recognition. Generic range/
stale replay, secondary-field capability, read-only/foreign-window and secure
field boundaries were exercised, with clean close/stop. Original `--owned-ax`
still posts no keyboard events; only the new explicit mode permits
child-PID-targeted fixture events, refusing physical/foreign input before Unicode
inspection, without model/global hooks/permission requests.

Retained negatives include the first choices attempt stopped by physical input
with zero edits/requests and the first emoji attempt's fixture typed-CGEvent ABI
failure before completion. Correct typed API and capability-refusal assertion
corrections preceded fresh passes; guards/deadlines were not weakened.
Actual WhatsApp evidence is **nonmutating metadata only**, not native mutation.
No physical typing/clicks, undo, full host matrix or platform release acceptance
is inferred. Existing 178/192 model quality, 14 no-offers and all 15 holds remain.


| ID | Current source symbols / regression seams | Current disposition, own evidence and remaining gate |
|---|---|---|
| P01 | `core.rs`: `SuggestionState::advance`, `accept`; `exact_unicode_offsets_and_immutable_revision`, `visible_word_boundaries_and_whitespace`. | Implemented; C verifies exact whitespace/NFD/emoji and partial/full/duplicate handling; A verifies actual native selection mapping, including AppKit cluster snapping. [Target-owned clients](../../evidence/rust-migration/target-owned-client-acceptance.json) add Qt Unicode replacement/undo. Physical external-host word/full/repeated-Tab acceptance remains open. |
| P02 | `runtime.rs`: `Engine::capture`, `schedule`, `commit_confirmed_edit`; verified exhaustion, `continuation_guard_` and `whatsapp_tab_reservation_` regressions. | [Final follow-up](../../evidence/rust-migration/tab-followup-current/receipt.json) adds user-approved WhatsApp field-session intent across idle/no-offer, with strict scope revocation, real-paint priority and no unseen edits/extra work. Other apps keep the bounded guard. The user confirms the reported native WhatsApp repeated-Tab case; the broader physical/global-hook scheduling matrix remains open. |
| P03 | `runtime.rs`: presentation/one-use claims and emoji pending reconciliation; `core.rs`: `Snapshot::matches_pending_emoji_selection`; permanent intermediate/final, stale/revoked/expired/pause regressions. | [Current owned choices](../../evidence/rust-migration/tab-emoji-current/receipt.json) add eight acceptance cases with actual draw/typed receipt before NSButton action, one edit/duplicate refusal. Only dispatched dedicated emoji may retain exact selected-only state under existing invalidation/deadline gates; no revival/replay. Controlled native proof is not arbitrary-host visibility or physical acceptance. |
| P04 | `features.rs`: `emoji_choices`, `synonym_prompt`, `parse_choices`; `Snapshot.can_replace_emoji`; `NativeBridge::replace_emoji_range`; permanent capability-isolation regressions. | [Current owned proof](../../evidence/rust-migration/tab-emoji-current/receipt.json) covers ZWJ, alias and sole emoticon plus dedicated range backend. Ordinary replacement/synonyms/typos remain unchanged. Live WhatsApp evidence is nonmutating metadata only; synthetic `:coder:` backend span is not catalogue recognition. Historical emoji receipts retain their own scope; external-host tool/semantic acceptance remains open. |
| P05 | `spelling.rs`: `plan_languages`, `SpellingService::set_language`; `explicit_regional_dictionary_never_substitutes_another_region`, `typo_ranges_after_astral_and_cluster_normalization`; `platform/macos_spelling.rs`: `MacSpellBackend`. | Implemented plus bounded actual NSSpellChecker evidence in the P05 table above: 22 checks, en-GB/pt-BR exercised, exact en-US unavailable. Pending asynchronous service/native-field integration remains open; C/M disabled typo checks. |
| P06 | `shortcuts.rs`: `saved_chords_normalize_aliases_and_modifier_order`, `keyboard_geometry_does_not_hijack_the_other_key`; `platform/macos_input.rs`: `rejected_shortcut_replays_down_up_and_tail_exactly_once`; `platform/windows_decisions.rs`: `consumed_press_repeat_and_release_remain_paired_after_disable`. | Implemented; [12 pure Windows decision tests](../../evidence/rust-migration/portable-windows-decisions-current.json) compile/pass on both Mac/Linux through the canonical test-enabled module; earlier host gates excluded that module. [Target-owned clients](../../evidence/rust-migration/target-owned-client-acceptance.json) cover Qt release ownership with synthetic dispatch/XTest, not physical keys or Windows hooks. Per-OS non-US/AltGr/dead-key/conflict/replay and held-key shutdown acceptance remains open. |
| P07 | `config.rs`: `resolve_preference`, `resolve_instructions`, `strict_types_reject_boolean_numbers_null_and_wrong_collections`, `legacy_migration_retains_saved_model_consent_and_schedule`; `ui/settings.rs`: `SettingsDraft`. | Implemented; [configuration transactions](../../evidence/rust-migration/config-apply-evaluation.json) cover save/session/merge/failure/cancellation, [Unicode folding](../../evidence/rust-migration/unicode15-casefold.json) covers all scalars. L adds actual native Save/Revert, independent drafts and exact failed/recovery persistence receipts in the private Linux shell, still 99 assertions including unchanged runtime/saved configuration and draft after catalog browsing. Mac [86-capture/117-geometry RAM-unit render evidence](../../evidence/rust-migration/ram-units-current/receipt.json) is not control-to-persistence proof; full Mac/Windows native interaction remains open. |
| P08 | `storage.rs`: `HistoryStore`, `seal_record`; `crypto_nonce_identifier_key_and_payload_are_authenticated`, `missing_existing_key_never_gets_replaced_and_full_clear_keeps_credentials`. | Implemented; bounded table above records 277 independent Python/Rust interoperability checks and 18 actual store scenarios. Injected keys/private synthetic databases do not establish OS-keychain migration, native authorship or normal-profile continuity. |
| P09 | `storage.rs`: `personalization`, `refresh_learning`, `dirty_learning_reauthenticates_changed_ciphertext_at_the_same_identity`, `statistics_match_rolling_periods_scalar_counts_grouping_and_active_averages`; `runtime.rs`: `scoped_deletion_invalidates_derived_context_and_suppresses_unchanged_draft`. | Implemented; bounded learning-store receipt above covers 18 scenarios. [Learning evaluation](../../evidence/rust-migration/learning-bias-evaluation.json) has mixed semantic results and unadopted proposals; explicit harness resets are not automatic production lifecycle proof. Native provenance, desktop deletion lifecycle and model-quality benefit remain unclosed. |
| P10 | `system.rs`: `clipboard_gui_queue_never_reads_expired_cancelled_or_revoked_work`, `screenshot_appearance_never_authorizes_context_and_revoked_ocr_is_discarded`, `blocked_ax_is_not_an_ocr_fallback`. | Implemented source gates; [historical context rendering](../../evidence/rust-migration/macos-prediction-repair/native-context-render.json) is source-bounded synthetic AX/render evidence, not current context authority or causal quality. C/A/M did not exercise optional collection; real permission/revocation/focus boundaries remain open. |
| P11 | `power.rs`: `LowBatteryHelpPolicy`, `twenty_percent_inclusive_and_unknown_never_rearms`; `runtime.rs`: `battery_force_only_and_explicit_zero_residency_preserve_load_without_evaluation`. | Implemented; [configuration transactions](../../evidence/rust-migration/config-apply-evaluation.json) cover same-model policy and invalid distinct-target refusal, not successful distinct battery target or physical power transition. Actual <=20% help, AC rearm, nonactivation/accessibility and energy behavior remain unverified. |
| P12 | `instance.rs`: `another_process_cannot_share_ownership`; `runtime.rs`: `shutdown_retries_only_incomplete_owners_and_never_forgets_spelling_cleanup`; `ui/settings.rs`: `SettingsDraft`. | Implemented; C/M workers stopped and [configuration transactions](../../evidence/rust-migration/config-apply-evaluation.json) cover pending-operation shutdown. L proves guarded Quit Cancel, no-tray recovery and actual canonical Quit cleanup of controller/preview/clipboard/power owners through GTK exit. [Historical Windows shell](../../evidence/rust-migration/windows-native-desktop.json) covers owned close/Quit only. [GPUI experiment](../../evidence/rust-migration/gpui-owned-linux/receipt.json) remains rejected, including run-2 watchdog termination. Other desktop/repeated-Quit failure gates remain open. |
| P13 | `models.rs`: `model_availability`, `ram_labels_use_binary_gibibytes_and_preserve_fractions`, `fractional_memory_requirements_keep_byte_exact_eligibility`, `catalogue_selection_verifies_existing_bytes_without_publication_or_replacement`, `cancellation_before_publication_preserves_source_and_destination`, `corrupt_or_truncated_transfer_is_not_installable`; `assets/models.json`: all 11 required IDs. | Implemented; [RAM-unit evidence](../../evidence/rust-migration/ram-units-current/receipt.json) confirms common allocation-free two-decimal binary GiB formatting for RAM labels and insufficient-memory warnings, preserving positive fractions without integer truncation. Zero/missing minimum remains unspecified: Mac omits it, Linux/Windows show unavailable. Physical RAM stays separate and unknown hardware stays unknown; byte-exact eligibility, capacity detection and download/transfer formatting are unchanged, with no fit promise. Actual Linux Models-dialog checks retain 99 assertions using runtime catalog inventory without installation fixtures/download/consent/saved selection; Mac's 86 captures/117 geometry observations use explicit availability render fixtures, not filesystem/model availability. Windows surface is source-reviewed/unexecuted. [Historical catalogue-memory evidence](../../evidence/rust-migration/catalog-memory-current/receipt.json) retains the pre-unit-fix 8.6 GB Mac observation. [Retained-model smoke](../../evidence/rust-migration/model-storage-retention-smoke.json) records E2B hash/use after cache removal with its pre-correction identity limit. Current live download/license/import/cancellation UI matrix and unavailable-family execution remain separate gates. |
| P14 | `inference_native.rs`: `NativeModel::evaluate`, `family_sampler_defaults_match_source`, `constrained_shortlist_preserves_sampling_distribution_and_state`; `writing.rs`: `every_family_preserves_data_and_exact_prefill`. | [Post-NFC calibration](../../evidence/rust-migration/nfc-cache-calibration-current.json) passes 55 prompts/all three gate classes, 330 trajectories and zero trim failures. [Pre-NFC calibration](../../evidence/rust-migration/cache-schedule-calibration-current.json), [real-grammar shortlist parity](../../evidence/rust-migration/grammar-shortlist-parity-current.json) (114 units/456 trajectories) and [updated original live-reuse smoke](../../evidence/rust-migration/live-cache-reuse-calibrated-current.json) retain their historical passing scopes. [Calibrated saved policy](../../evidence/rust-migration/cache-policy-calibrated-current.json) still fails 23/80 admitted checkpoint cases (full/off 9, compact/off 3, full/on 8, compact/on 3); all four arms fail exact/numeric but pass decisions, while all three independent live controls are bit-exact. Physical-layout causation is inference, not proved; no ordinary KV defect/default change or performance claim follows. Historical negative receipts remain intact; current Tiny/family and saved-checkpoint adoption gates remain open. |
| P15 | `writing.rs`: original v22 Qwen/v19 Gemma-Qwen3 framing; `providers.rs`: `healing_removal_precedes_any_display_for_every_unicode_partition`, `mandatory_framing_instructions_and_suffix_still_require_capacity`, `alternatives_pin_words_abstain_deduplicate_and_cancel_atomically`. | Original production layout retained; [NFC replay](../../evidence/rust-migration/nfc-healing-current.json) fixes three missing-`o` NFD joins with authored bytes unchanged: **178/192**, 189 unchanged texts/mechanical outcomes and zero regressions. The [pre-NFC comparison](../../evidence/rust-migration/writing-full-corpus-parity-current.json) retains its historical 175/192 baseline and cancellation scope. All 52 long required outputs/work/seeds remain equal; four callback-count changes are scoped to that comparison. The [old short-probe worker receipt](../../evidence/rust-migration/nfc-worker-healing-current.json) retains exit 1 and three inconclusive after-result probes; current corrected-clock cancellation passes only the supervisor contract above. All six layout/cache candidates plus the fixed reminder remain rejected, as does the new no-healing experiment. Twelve literal checks, 21 alternative samples with five absences and six synonym samples with four choices each remain unchanged; assistant semantic labels are now attached, not calibrated or human-validated. [Utility admission](../../evidence/rust-migration/utility-grammar-admission-current.json) proves ten grammar fixtures including `[]`, not model abstention. Neither relative preservation nor semantic review changes the absolute quality failure; M's English no-offers still fail positive bilingual acceptance. |
| P16 | `config.rs`: `local_endpoint`; `providers.rs`: `server_contracts_are_raw_loopback_streaming_and_request_owned`, `active_ollama_and_sse_streams_can_outlive_the_read_timeout`, `redirects_errors_response_bounds_and_precancel_are_not_bypassed`. | Implemented owned-loopback regression seams under aggregate gates; no separate per-provider native receipt. [Configuration saves](../../evidence/rust-migration/config-apply-evaluation.json) explicitly did not probe endpoint availability. No cloud/provider availability claim. |
| P17 | `worker.rs`: bounded frame/request parsing and ready/proceed; `process_client.rs`: `WorkerClient`; real fixture-child tests cover latest-pending/cancellation/release/pipe retirement and validation tickets. | Implemented; C/M execute real child ownership/barrier/shutdown; [configuration transactions](../../evidence/rust-migration/config-apply-evaluation.json) record one worker and seven validation admissions. Those runs do not cover every cross-platform child/process failure or release package; historical [portable worker](../../evidence/rust-migration/windows-portable-worker.json) stays historical. |
| P18 | `preview.rs`: `PreviewSession`, `selection_never_borrows_another_models_readiness`, `only_exact_widget_readback_counts_and_dismiss_blocks_retry`; `ui/windows_settings.rs`: pending-model readiness tests. | L proves actual English word/Portuguese full GTK preview acceptance, independent document readback, readiness/selection, focus-loss and injected composition-signal refusal in six-step Linux Welcome. **Mac M still fails its positive EN/PT gate** (two EN abstentions, two PT positives), and is not canonical Welcome. Canonical Mac owned Welcome aborted on input interruption before any model request/edit, empty cases and clean Controller shutdown; positive gate unexercised, not permission/model failure. [Historical Windows Welcome/preview](../../evidence/rust-migration/windows-owned-ui.json) cannot close current Mac/Windows, real IME or full license/download acceptance. |
| P19 | `platform/macos.rs`: `MacBridge`, `browser_documents_keep_exact_url_binding_and_refuse_unknown_documents`; `macos_geometry.rs`: protected-before-content traversal tests. | Implemented; A proves genuine cross-process AX/Unicode/selection/geometry and refusal, not Controller/hooks/secure-field toggles. Current signed-host/TCC, browser documents and physical/native security gates remain open. AppKit's observed combining-cluster snap is not proof of accepting an interior cluster range. |
| P20 | `ui/macos_overlay.rs`: `OverlayOwner`, native draw/authority/clip/compact-choice tests; `platform/macos_geometry.rs`: verified inline metrics. | Implemented; C has actual owned ink and stale/clipped refusal, M positive PT draws; [current 86-capture/117-geometry render receipt](../../evidence/rust-migration/catalog-memory-current/receipt.json) adds owned raster/geometry only. No inference of WindowServer composition, Spaces/mixed-display behavior or physical host acceptance. |
| P21 | `platform/macos.rs`: `replace_emoji_range`, `classify_readback`; `core.rs`: exact pending emoji selection; permanent preflight/partial-mutation/readback regressions. | [New explicit owned emoji proof](../../evidence/rust-migration/tab-emoji-current/receipt.json) adds two real MacBridge/read-only-selected-text child replacements, one PID-targeted event pair/text change each, exact independent text/caret and stale/generic refusal. After-selection uncertainty is Indeterminate, never replayed; exact selected-only dispatch is pending, not Applied. Original A/`--owned-ax` stays no-post. Actual WhatsApp mutation, physical shortcuts/undo/Electron/permission-race matrix remain open. |
| P22 | `platform/macos_terminal.rs`: `DIAGNOSTICS`, signed-renderer/append-lease tests; `macos_docs.rs`: checked Docs enable/readback; `macos_vision.rs`: revocation/late-result/protected OCR tests. | Implemented narrow adapters/source seams, not newly exercised integrations. C/A/M exclude signed Codex, Docs and OCR. Current signed Codex host, Automation/TCC, Docs readback and optional capture/privacy execution remain open; metadata is not writable Docs support. |
| P23 | `platform/windows.rs`: UIA capture/native Edit mutation; canonical `platform/windows_decisions.rs`: `replacement_capability`, `replacement_matches_capture`, `classify_readback`. | Generic UIA replacement is selected-only, not arbitrary range movement; collapsed carets allow ordinary insertion and native Edit retains checked range support. Exact selected-readback permits selection-only capability loss after collapse while preserving authority, surrounding text and resulting caret. Earlier Mac/Linux gates covered generic runtime readback but excluded this pure module; [all 12 pure tests now compile/pass on both hosts](../../evidence/rust-migration/portable-windows-decisions-current.json) under `cfg(any(target_os = "windows", test))`, with one implementation used by native Windows. [Historical Windows build](../../evidence/rust-migration/windows-build-completion.json) is not current source execution. Current native Windows/MSVC/desktop remains unverified; UIA rejection, IME/UIPI, hook lifecycle, physical pairing/readback and publication gates remain open. |
| P24 | `platform/linux.rs`: insertion-only capture/edit path; `linux_tests.rs`: privacy/composition and atomic-replacement refusal seams. | Current Linux ARM64 aggregate gates and L passed. [Stock AT-SPI](../../evidence/rust-migration/stock-atspi-current.json) proves four captures/eight refusals/zero insertions, not positive inline support. [Current target-owned clients](../../evidence/rust-migration/target-owned-client-current.json) remain a distinct opt-in protocol. [System D-Bus cutover](../../evidence/rust-migration/linux-dbus-linkage-current.json), [CLI AT-SPI navigation](../../evidence/rust-migration/linux-cli-models-current/receipt.json) and [X11 conflict evidence](../../evidence/rust-migration/x11-shortcut-conflict-current.json) retain their own scopes. Real external-field/replay, physical keys/IME and x86-64 installer gates remain open. |
| P25 | `platform/wayland.rs`: `WaylandBridge::capture_native`/`confirm` force `native_inline=false`; `core.rs`: `manual_native_anchor_does_not_grant_target_owned_inline_capability`; `runtime.rs`: `manual_transport_without_inline_capability_refuses_ordinary_completion`, `manual_native_choices_without_inline_keep_dense_ordinals_and_confirmed_insertion`. | **Scout discrepancy fixed:** ordinary inline/preedit and completion-claim route retired, no popup substitute; manual panels/confirmed insertion and target-owned clients retained. Full Mac/Linux source gates passed; read-only safety review found no findings. [Actual KWin emulation](../../evidence/rust-migration/emulated-linux-current/receipt.json) now observes compositor/private-socket/protocol activity but strict exit 2 leaves identity-qualified product binding false; see prerequisites below. Full manual commit/physical IME/normal KDE selection remain unmet. [Historical KDE deployment](../../evidence/rust-migration/gamer-pc-0.5.0-deployment.json) lacked the private socket; old Python smoke receipts remain historical. |
| P26 | `ui/macos_settings.rs`: native pane list and model details; `macos_settings_panes.rs`: shared form controls; `macos_smoke.rs`: Unicode editor/selection retention and catalog-memory render seams. | Implemented; [current render-only schema-7 run](../../evidence/rust-migration/catalog-memory-current/receipt.json) passed 86 captures/117 geometry observations, retaining existing panes/Unicode scenarios and adding unspecified/positive memory labels. Catalog availability is an explicit render fixture, not filesystem availability/model loading. No Controller/services/profile inspection/hooks/actions; native NSView bitmaps are not WindowServer presentation. The [78-capture/107-geometry receipt](../../evidence/rust-migration/native-settings-current/receipt.json) and its six reviewed images remain historical. Full disposable Mac shell, persistence, actual menu/shortcut/VoiceOver/model/update flows and physical acceptance remain open. |
| P27 | `ui/linux_settings.rs`: `show_pane`, `owned_settings_controls_preserve_all_fields_and_draft_across_panes`; `ui/settings.rs`: `SettingsDraft`; `ui/linux_models.rs`/`ui/windows.rs`: model details; `windows_smoke.rs`: owned-pane/draft/Welcome seams. | First-open Models selection fixed by showing GTK children before GtkStack selection; [actual CLI receipt and reviewed PNG](../../evidence/rust-migration/linux-cli-models-current/receipt.json) prove that route and native Quit. L now adds the [99-assertion full private Linux shell](../../evidence/rust-migration/catalog-memory-current/receipt.json): six panes/Welcome steps, real EN/PT preview, independent consent dialogs, draft retention/rebase, Save/Revert, persistence failure/recovery, clean Quit and actual Models-dialog unspecified/positive memory with unchanged config/draft. Readiness waits for mapped window, complete populated catalog and active row within the existing five-second deadline. Earlier 96-assertion receipts remain historical. This closes those owned Linux scenarios, not real IME/physical input, external accessibility, full download/update flows or current Windows. Windows unavailable-memory labels are source-reviewed only. [Historical Windows desktop](../../evidence/rust-migration/windows-native-desktop.json) retains its scope; GPUI remains unadopted. |
| P28 | `paths.rs`: profile/cache/durable-model path resolution; `storage.rs`: `Inklow` credential identity/AAD; `system.rs`: autostart/desktop identity; `crates/xtask/src/package.rs`: installer identity derivation. | Implemented continuity contracts; [Unicode identity proof](../../evidence/rust-migration/unicode15-casefold.json) and [retained-model smoke](../../evidence/rust-migration/model-storage-retention-smoke.json) are bounded. [Historical 0.5.4 release](../../evidence/rust-migration/macos-0.5.4-release-verification.json) is not current-candidate installation/TCC proof. Clean signed candidate/normal identity/grant continuity remain separate authorized gates. |
| P29 | `updates.rs`: `UpdateService`, `signed_schema_and_asset_bounds_are_exact`, `constructor_snapshot_and_close_do_not_touch_profile`; `update_keys.rs`: pinned identity/key. | Implemented source/test contract; [historical signed release verification](../../evidence/rust-migration/macos-0.5.4-release-verification.json) proves only recorded asset/signature/install scope. Current UI/service intent and release packaging are not established; no install/publish authority. |
| P30 | `update_install.rs`: `cancel_and_wait`, `actual_atomic_promotion_and_rollback_preserve_complete_inodes`, `open_launcher_success_is_not_gui_health_and_cancelled_late_start_is_refused`, `unconfirmed_process_lifetime_refuses_rollback_and_keeps_both_builds`. | Implemented; [historical 0.5.0 transactions](../../evidence/rust-migration/macos-0.5.0-release-verification.json) distinguish genuine healthy old helpers from component negative fixtures; later release receipts remain historical. [Windows direct-client ownership](../../evidence/rust-migration/windows-direct-client-ownership.json) still fails HKLM ARP ownership. Current signed/clean-commit/prior-version/Tiny installer matrix missing; ARM64 Xvfb is not x86 installer proof. No install/publish authority. |
| P31 | `cli.rs`: launch/pane routing, `valid_pane`, matching prepared/done and selection-refusal tests; `ui/linux_settings.rs`: first-open Models selection. | [Actual source/binary-bound Linux CLI](../../evidence/rust-migration/linux-cli-models-current/receipt.json) passes `--settings --pane models --paused --no-tray`: native AI Models, AT-SPI Quit/exit 0, no lingering owned children, settings byte-identical with saved enablement true (memory-only pause). No model present/load/inference in that CLI run. L separately proves no-tray shell/Welcome recovery and real preview inference/clean Quit; it does not extend CLI flag coverage. Private ARM64 Xvfb scenarios do not establish all flags, physical desktop, Mac/Windows launch or installer acceptance. |
| P32 | `.github/workflows/ci.yml`: native lanes; `examples/native_desktop_smoke.rs`: separate owned Controller/model/AX/emoji scopes; shared source/native regression seams above. | **Reconciliation accounted for, full acceptance open.** [Earlier Mac Tab/emoji gate](../../evidence/rust-migration/tab-emoji-current/receipt.json) reports 743 passes (738 parent + five child), 26 summaries, one ignored, not distinct scenarios. Earlier Mac/Linux/portable-Windows, native-shell and render receipts remain historical checkpoints. No new native Windows/Linux, Mac bilingual Welcome, physical host/TCC/KWin, signed candidate, upgrade/Tiny, power or full disposable-shell acceptance. GPUI is not adopted, ordinary Wayland inline is disabled, and all 15 gates remain held; no install/publication claim. |

### Remaining prerequisites and authority

Current Windows/MSVC/native desktop execution remains unavailable. Genuine KWin
5.27.11 now ran under nested X11/software Mesa in [Linux AMD64 emulation](../../evidence/rust-migration/emulated-linux-current/receipt.json):
a native GTK Wayland editor mapped with exact NFD/non-BMP readback and focus
property true, but `window_active=false`. The compositor-inherited connected
AF_UNIX peer was validated; product logs record input-method/input-panel bind
requests and activation. Final post-fix strict probe **exit 2 / partial** retains
10/13 passing gates: D-Bus `ListNames` returned no `org.kde.KWin*` names and
executable observations showed the Rosetta interpreter despite matching process
start ticks. Exact D-Bus identity, product executable identity and identity-qualified
binding remain false. Raw binds are not acceptance.
Missing EGLImage pixmap support/compositing disabled and missing KDE decoration/
QML modules are logged limits. An owned image was captured, no model/inference
ran, and forced cleanup is not canonical Quit/release. The corrected UP-interface/
no-IPv4/IPv6-route guards passed; the initial down-tunnel refusal stays historical.
Actual pre-cleanup execution exposed a production bug: keyboard grab → deactivate
→ `keyboard.release` caused invalid-object-11 `wl_display.error`. The
`wayland_wire.rs::drop_keyboard` correction destroys only the local version-1
proxy and retains owning-context teardown. Actual post-fix grab → deactivate →
context destroy has no release and no protocol errors; product and KWin logs
each improve from one/zero pre-/during-cleanup errors to zero/zero. The complete
product log is retained and the new no-pre-cleanup-protocol-error gate passes.
Post-fix serial locked/offline Linux all-target tests plus bins/examples passed
again (693 parent + six child = 699 reported, 27 summaries, two ignored); parent
formatting passed. The 99-assertion X11 native-shell pass predates this Wayland-only
correction; unchanged X11/UI source is not a claim of a post-fix X11 rerun.
Full manual commit, physical IME and normal KDE selection remain unmet, with all
15 gates held. Neither ARM64 Xvfb nor this actual AMD64 emulation certifies the
x86-64 installer or an ordinary compositor/IME session. A signed, clean-commit candidate, current Tiny execution,
prior-version upgrade/rollback matrix, physical host/input tests, TCC/grant
continuity and a disposable complete Mac shell remain missing gates. This
reconciliation grants no permission to install over the normal app or publish a release.
The separately itemized baseline findings are reconciled in
[review.md](review.md#current-r1r13-disposition).
