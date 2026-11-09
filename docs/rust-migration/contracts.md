# Implementation contract — approved native Rust migration

2026-10-30: user selected Rust-owned OS-native UI (not Tauri or Swift shell) and approved installing Rust 1.99.1/Cargo. Toolchain installation succeeded; resolve the pinned binaries in the current environment rather than reusing an earlier machine's absolute paths. Parent owns workspace/manifests/lockfile and integration checks.

Production workspace will use one application crate at `crates/brotypist`, organized into the boundaries in README.md rather than empty per-boundary crates. Root Cargo workspace and public module registration are integration-owned. Edition 2024, `anyhow::Result<T>` for service errors, serde JSON compatibility, standard platform cfgs. No Python sidecar in final runtime. Source baseline remains immutable while migration is in progress.

Derived caseless matching uses **Unicode 15.0.0 default-full, non-Turkic case folding**, matching the immutable CPython 3.12 baseline. The canonical `unicode_compat` iterator is shared by the app, independent updater and tools; no direct Unicode 9 folding remains in consumers. It does not normalize or rewrite authored text. Caller-owned NFC boundaries remain explicit. Exhaustive scalar folding equivalence does not certify normalization, category or regex-table equivalence.

## Current Unreleased contracts

The implementation below extends the original migration packet; these are
source contracts, not a claim that the complete roadmap or native acceptance
has passed.

### Settings, ownership and receipts

- `Config::resolve_preference` is the canonical global → app → exact-domain
  resolver. Application keys retain platform identity; `normalize_domain`
  canonicalizes exact hosts, not wildcard/suffix scopes. Missing scoped fields
  inherit. Required unavailable browser identity fails closed. Clipboard,
  focused-window text, screenshot context and screenshot appearance cannot
  exceed their independent global consent ceilings. Recording/use may instead
  opt in individual scopes. Appearance does not grant context extraction.
- `writing_language` and `spelling_language` default to `auto`. Explicit writing
  language is a default that yields to surrounding writing; regional spelling
  contributes model conventions and selects exact available macOS dictionaries,
  never a substituted region. Windows/Linux native dictionary correction remains
  unavailable. `resolve_instructions` prepends these defaults to unchanged
  authored global/app/domain instructions; mandatory worker security instructions
  are not replaced.
- `runtime::config_apply` binds a unique `ConfigOperationId` to its receipt.
  `Saved` certifies successful persistence; `Applied` is session-only.
  `Failed { stage, message }`, `Cancelled`, `Superseded` and `Stopped` are terminal
  non-success outcomes. Equality with an old config is not proof of a new save.
  Drafts rebase onto current settings rather than overwriting unrelated changes;
  obsolete operation completions cannot promote a newer selection.
- GGUF selection/preparation changes release the existing inference owner and
  cold-prepare through that same worker, with catalogue size/hash checks where
  applicable and actual prompt-template compatibility before persistence.
  Primary and configured distinct battery targets participate. This is load/
  compatibility evidence, not semantic-quality acceptance. Supported loopback
  settings are validated and saved without preparation/probes: availability of
  the server or named model is **not checked**.
- CLI model download/selection acquires resident ownership before validation.
  If the resident app is running, selection is refused with guidance to use AI
  Models, rather than creating a second inference owner. Launch overrides do
  not silently persist preferences.
  The [source-bound Linux CLI receipt](../../evidence/rust-migration/linux-cli-models-current/receipt.json)
  proves first-open AI Models, actual AT-SPI Quit/exit 0 and no lingering owned
  children; byte-identical settings preserve saved enablement under `--paused`.
  System D-Bus linkage avoids duplicate vendored runtime exports without package
  upgrades; GTK children must be shown before GtkStack pane selection. This is
  navigation-only, not inference, full Settings or other-platform acceptance.
- General retains everyday enable/pause/startup controls; Setup retains native
  readiness and permission guidance; About retains version/notices/update
  information. Closing Settings does not quit the resident app. Existing bundle,
  profile, credential and updater identities remain unchanged; the updater is
  separately linked and does not become an inference owner.

### Tab renewal and emoji-only native transactions

The [2026-11-08 receipt](../../evidence/rust-migration/tab-emoji-current/receipt.json)
separates source regressions, controlled native component execution and
nonmutating WhatsApp metadata. None is actual WhatsApp mutation or physical
typing acceptance.

- After verified Word/All completion exhausts its remainder,
  `runtime::commit_confirmed_edit` clears the attempted marker and restarts
  ordinary debounce. Retained remainders remain receipt-bound; genuine no-offer
  attempts remain quiet. Ordinary Tab passes through outside the applicable
  reservation/guard below. Permanent
  `successive_word_acceptances_renew_completion_after_verified_remainder_exhaustion`
  and `full_acceptance_renews_once_and_empty_result_does_not_retry` cover exact
  text/caret, fresh paint, single use and renewal without automatic retries.
- Exact native `net.whatsapp.WhatsApp` verified ordinary completion acceptance
  creates field-session accept-word intent. It remains for the exact current
  draft, field/authority and authored-input revision beyond idle and genuine
  no-offer. Without an eligible visible claim, reserved keys consume without
  edits, acceptance accounting, model retries, new requests or future acceptance.
  Other apps keep the maximum two-second renewal/repaint guard, with terminal
  no-offer and expiry restoring ordinary behavior.
- Both are distinct from rendered `Claim` authority. Hooks perform no AX reads;
  the owner requires fresh full snapshot/key/authority and existing policy,
  native-readiness and security checks. Real rendered claims take precedence;
  a previously admitted nonmutating key cannot accept or disarm later paint.
  Authored input, caret/selection, field/focus, default Escape, config/policy,
  pause/stop, safety refusal and new unconfirmed mutation revoke intent without
  revival. Non-completion tools and unknown edits cannot arm it. Binding
  precedence is unchanged; arbitrary Escape rebinding and independent
  post-permission revocation are not claimed as exercised.
- Successful ordinary Unicode posting is not yet an applied edit. Only the
  observed `selection_or_geometry_changed_after_capture` race plus fresh safe
  target proof can yield `Dispatched` despite immediate missing readback.
  `pending_edit_capture_is_transient` defaults false and consumes one-shot native
  proof for that actual capture; it cannot fabricate a snapshot or bypass claim
  authorization. Existing pending completion may survive those proved missing
  observations, but exact confirmation must precede its original two-second
  deadline, including slow/late reads. Native lease storage expires without
  renewal. `field_value_unavailable` conflates absent/error/malformed data and
  remains nondeferrable, as do other unproved stages, errors and mismatches.
  This two-second confirmation bound is independent of WhatsApp's verified
  field-session reservation. No delivered mutation or its initiating key is replayed.
- Trailing-Tab calibration uses a bounded preceding sample, fresh direct native
  sample-end/caret/gap bounds, a common glyph row and matching typography through
  the gap. Raw endpoint rows/heights and measured gap/caret heights must agree
  before applying ordinary caret capture's existing compatible-glyph Y
  normalization. Native X positions remain unchanged; no guessed Tab advance,
  fitted-font fallback or separated Vision evidence is allowed.
  [Follow-up evidence](../../evidence/rust-migration/tab-followup-current/receipt.json)
  separates owned rows, unavailable non-BMP font coverage, earlier failed live
  candidates and the final user-confirmed WhatsApp result. A validated real
  multirow whitespace gap remains correctly refused; the reservation prevents
  extra Tabs from creating it, without deleting existing text.
- `Snapshot.can_replace_emoji` defaults false and participates in snapshot keys,
  policy comparisons and confirmed readback. It is an additional emoji-only
  capability, not a change to ordinary `can_replace`. Only emoji admission,
  presentation and acceptance may use either capability. Synonyms, corrections,
  other range tools and ordinary shortcut arming retain their existing gates.
- Ancillary appearance-job identity in `macos_vision.rs::snapshot_binding` must
  include the emoji capability as well; its existing binding regression covers
  that distinction. This is identity consistency, not a new native-write
  capability. The owned emoji fixtures do not exercise or certify the native
  appearance pipeline; source-gate evidence remains separate.
- `NativeBridge::replace_emoji_range` is the purpose-specific emoji operation;
  its default preserves generic `replace_range` for ordinarily capable backends.
  Dedicated-only support requires the native override. Generic replacement and
  existing Unicode append remain unchanged; unsupported backends do not grant
  the new capability.
- Mac's fallback requires native WhatsApp identity (or explicit authenticated
  owned-fixture enrollment in `native-smoke`), safe focus/field/IME/geometry and
  policy, no Chromium node, writable selected range/value and missing ordinary
  selected-text replacement. Approved live metadata observed these API facts;
  it did not mutate WhatsApp or inspect writing/history.
- Preflight Unicode input before changing selection; select only the declared
  checked scalar-to-UTF-16 range. Independently recapture unchanged full text,
  exact selection, authority and capabilities. After event allocation, freshly
  revalidate foreground/field/window/IME/revision before exactly one
  `CGEventPostToPid` keydown/up pair to the verified process. No session-global
  fallback, whole-message AXValue rewrite, Backspace, clipboard or replay.
- Any uncertainty once selection may have changed is `Indeterminate`, not
  `Rejected` with a no-mutation promise. Explicit emoji range intent is required
  for classification; dispatch alone is not `Applied`.
  `Snapshot::matches_pending_emoji_selection` is allocation-free and requires a
  positive bounded scalar range, valid unprotected snapshots, identical immutable
  text, exact intended caret/selection, same authority and every policy/capability
  field, including both replacement flags, multiline and on-demand-only.
- Only an already dispatched emoji edit whose prior dedicated capability was
  true may retain that exact selected-only intermediate. Existing deadline,
  revision/epoch, authority, privacy, pause and policy invalidation still apply
  throughout ordinary observation reconciliation; no unrelated selection/text
  may survive, revive or replay. Exact final glyph/text/caret and capability
  consistency are required for confirmation. Permanent runtime/native/core
  regressions cover intermediate → final once-only and stale/foreign/revoked/
  protected/expired cases, plus no synonym/correction capability expansion.

Actual expanded `--owned-choices` passed eight acceptance cases and retained
refusal/ghost-lifecycle checks using the real controlled-provider
WorkerClient/Controller/OverlayOwner and owned NSTextViews. ZWJ, `Olá  :sorriso`
→ `Olá  😄` and `Olá  <3` → `Olá  ❤️` include native draw and typed receipt before
actual NSButton ordinal 1 → choice 0, exact independent readback, one edit,
duplicate refusal and no new provider submission; singleton images were inspected.
New explicit `--owned-emoji` exercised real MacBridge against a separate
authenticated AppKit child whose AX selected text is actually read-only while
range/value are writable. Synthetic `:coder:` → `👩‍💻` and `:-)` → `🙂` each
preserved astral/NFD prefix/suffix with one marked event pair, one native text
change and exact independent UTF-8/UTF-16 text/caret. The synthetic range case
does not assert that `:coder:` is a public shortcode. Generic range/stale replay
were refused, a second field lost the dedicated capability, read-only/foreign
window capture was refused, secure-field handling and clean close/stop exercised.

Original `--owned-ax` retains its no-keyboard-posting contract. Only the new
explicit mode permits child-PID-targeted fixture events; physical/foreign input
is refused before Unicode inspection, with no model/global hooks/permission
request. Retained negatives: the first choices run stopped on physical input
with zero edits/requests; a later hands-off run passed. The first emoji run hit a
fixture NSEvent/CGEvent typed-ABI mismatch before completion; the corrected typed
API passed in a fresh profile without unchecked message sending. The secondary
field assertion was corrected to capability refusal, not a false no-capture
requirement. No guard/deadline was loosened. These proofs do not certify actual
WhatsApp edits, physical typing/clicks, undo, full host/platform acceptance,
model quality, latency or power; all 15 holds and 178/192 quality remain unchanged.

Regression evidence retains the corrected Tab red (110 runtime passes/two new
failures) and green (112 runtime passes), plus emoji red (seven passes/two
failures) before the final passing source gate. Earlier receipts/counts are
historical checkpoints, not overwritten or promoted to broader acceptance.

### Authenticated learning and forgetting

- `record_history` and `use_history` are independent and default false in new
  profiles. Neither disabling one nor enabling the other deletes history.
  Only profiles lacking global `use_history` receive the one-time legacy
  migration from global recording and existing scoped recording choices; after
  that marker exists, absent scoped use values inherit rather than remigrate.
- `HistoryStore::personalization` considers the newest bounded 200 raw rows
  before filtering authored evidence, language, scope and current/source
  consent. Domainless history stays app-local; website evidence matches the
  exact host. Locking or unavailable evidence does not permit plaintext fallback.
- Incremental refresh keys cached features by record ID **and SHA-256 of the
  sealed bytes**. Changed ciphertext must authenticate and parse again; a stable
  ID alone cannot preserve old vocabulary. Failed refresh discards stale cached
  evidence. Authentication/malformed-payload failures latch the locked/broken
  state; operational SQLite failures do not falsely declare key corruption.
- Strength is not a training percentage or probability. Zero returns no
  personalization. Positive strength limits recurring vocabulary to
  `clamp(round_ties_even(strength * 0.24), 1, 24)` items, each occurring at least
  twice, ranked by frequency then lexical order. The JSON also carries bounded
  strength, language, record count and aggregate style (sentence length,
  lowercase starts, punctuation and contractions). It does not replay sentences
  or update weights, and style is not multiplied by a training fraction.
- `history_retention_days == 0` means indefinite retention. Positive retention
  uses authenticated dates and a UTC cutoff after worker release, without
  silently unlocking secure history. Unknown timestamps survive and are counted;
  age is not inferred from row order. Explicit app/domain/all-history deletion
  and expiry invalidate derived evidence, queued matching records and affected
  sessions; unchanged deleted observations are suppressed against immediate
  recreation. Aggregate statistics have separate deletion controls.

### Diagnostic and acceptance boundaries

Explicit local exports retain content-free bounded request/session accounting
and standalone preparation reports. Unknown work remains unknown/unreported,
not zero. Actual draw callbacks are distinct from publication or worker timing,
and do not establish compositor visibility, physical latency, energy use or
universal insertion compatibility.

Earlier [Tab/emoji source gates](../../evidence/rust-migration/tab-emoji-current/receipt.json)
passed **743 reported macOS passes (738 parent-suite plus five child-invocation
passes), 26 result summaries, one ignored**, with locked/offline workspace/
all-targets `native-smoke` tests, binaries/examples and formatting. This is not
a distinct-scenario count; no new native Windows/Linux run is claimed.

Historical [RAM-unit source gates](../../evidence/rust-migration/ram-units-current/receipt.json)
passed with **735 reported macOS test passes across 26 result summaries**
(730 parent-suite passes plus five child-invocation passes; one ignored), plus binaries/examples,
and Linux ARM64 locked/offline workspace/all-target tests (**569 library passes,
one ignored**, plus other suites)
and binaries/examples.
The [portable Windows decision receipt](../../evidence/rust-migration/portable-windows-decisions-current.json)
retains commands, results and source hashes: Rust 1.99.1 resolved through `PATH`,
`cargo test --offline --locked --workspace --all-targets --features brotypist/native-smoke`
and `cargo build --offline --locked --workspace --bins --examples --features brotypist/native-smoke`.
The prior integrated-NFC **721 Mac tests/26 suites, one ignored**, and Linux
**556 library tests plus other suites** remain historical source checkpoints.
These are local uncommitted-source checks, not native Windows/MSVC/SDK
typechecking, signing or release attestations.
Scoped evidence:
[277 storage checks](../../evidence/rust-migration/storage-interoperability-current.json),
[18 learning scenarios](../../evidence/rust-migration/learning-store-current.json),
[22 dictionary checks](../../evidence/rust-migration/native-dictionary-current.json)
(en-GB/pt-BR exercised; en-US unavailable), and
[historical Mac render-only observations](../../evidence/rust-migration/native-settings-current/receipt.json)
(78 captures, 107 geometry checks, four Unicode editor/selection resize observations;
six reviewed images retained). The [current RAM-unit receipt and images](../../evidence/rust-migration/ram-units-current/receipt.json)
record **86 Mac captures / 117 geometry observations**, retaining existing panes
and Unicode scenarios. Balanced's zero minimum omits the memory line; Gemma 4
E2B's 8,589,934,592 bytes display as **8.00 GiB**. The [historical catalogue-memory
receipt](../../evidence/rust-migration/catalog-memory-current/receipt.json) retains
its pre-unit-fix **8.6 GB** Mac observation; earlier raw-byte labels and the
733-pass checkpoint remain historical. Native NSView bitmaps use explicit
catalogue/availability render fixtures with no Controller/services or dispatched
actions; they do not prove installation, model loading, physical input or
compositor presentation.

The same current receipt records the actual Linux native shell's **99 assertions**,
stage 45, `clean_exit=true`, `error=null`. All previous 96 checks remain: EN word/PT
full preview, real model selection success/failure/cancellation, instructions
save/cancel, dirty Unicode retention, external/live refresh and concurrent updates,
independent consent dialogs, guarded close/Quit Cancel, persistence failure/
recovery and owner cleanup. Three new actual-dialog assertions cover two real
runtime catalogue states (Balanced zero → unavailable; Gemma 4 E2B 8,589,934,592
bytes → **8.00 GiB**) and unchanged runtime/saved configuration and Settings draft
after browsing. No installation fixture, download, license consent or
model-selection save occurred. Readiness requires the mapped window, complete
populated catalogue and active row within the existing five-second deadline,
without a fixed sleep. Commands and raw/source digests provide durable provenance;
post-run source capture is not a frozen prelaunch identity.
The [historical integrated-NFC 96-assertion/stage-42 receipt](../../evidence/rust-migration/linux-native-shell-latest/receipt.json)
retains its prior intermediate identity; the [pre-wave receipt](../../evidence/rust-migration/linux-native-shell-current/receipt.json)
also remains historical. Controlled GTK widget interaction passed separately.
Full Mac persistence/interaction, physical typing and native Windows acceptance
remain unexecuted; owned-widget dispatch is not real IME, KWin or release proof.

**Ordinary calibrated reuse passes; whole-state checkpoints are not adopted.**
Exact matched schedules remain strict; cross-schedule drift is judged against
independent fresh/fresh controls, without relaxing epsilon. Historical failed
comparisons remain intact. [Post-NFC ordinary calibration](../../evidence/rust-migration/nfc-cache-calibration-current.json)
passes all 55 exact/calibrated-numerical/calibrated-decision rows across 330
trajectories, with 99,954 prompt decoded, 2,610 generation decoded, 2,940 sampled
tokens and zero trim failures. The [earlier calibration](../../evidence/rust-migration/cache-schedule-calibration-current.json)
retains its original scope. The [earlier live smoke](../../evidence/rust-migration/live-cache-reuse-calibrated-current.json)
passes production exact/numerical/decision gates and compact exact safety with
eviction recovery; compact numerical/decision calibration is not applicable.
This does not assert universal batch invariance.

The [whole-state policy receipt](../../evidence/rust-migration/cache-policy-calibrated-current.json)
completed all 80 admitted cases: 23 exact/numerical failures (full/off 9,
compact/off 3, full/on 8, compact/on 3). Each arm passes bounded decision
agreement, but fails adoption gates; three independent live controls agree
bit-for-bit. Historical sequence-state missing-history conclusions do not
apply to whole-state `seq_id=-1`, which includes occupied cells without that
filter. Restore packs cells/resets its head; physical layout as the cause of
drift remains **[INFERENCE]**. No ordinary KV defect is established.

[Real-grammar wrapper parity](../../evidence/rust-migration/grammar-shortlist-parity-current.json)
passes 114 units / 456 trajectories on the measured model/backend and fixed
seeds, not universal sampling, semantics, diverse alternatives search or
instrumented shortlist fast-path coverage. [Ten utility grammar admission
fixtures](../../evidence/rust-migration/utility-grammar-admission-current.json)
include `[]`; that is not the model choosing abstention.
The [pre-fix full64 comparison](../../evidence/rust-migration/writing-full-corpus-parity-current.json)
retains its historical 175/192 result. The [integrated NFC correction](../../evidence/rust-migration/nfc-healing-current.json)
now passes **178/192**: all three missing-`o` NFD joins are fixed, only those
three generated texts change, and there are zero regressions. The remaining
189 text/mechanical/native-raw/seed/non-timing-work samples match the pre-fix
current-source control. Twelve literal checks remain observed without
contradiction/missing facts; assistant semantic labels are now attached but
neither human-validated nor calibrated (see the follow-up register below).

`native_prompt_tokens` first removes the exact authored healing suffix, then
normalizes only model replay to NFC (already-NFC remains borrowed).
Prompt tokenization, production/diagnostic grammar, replay metadata,
tokenizer-derived token allowance and replay stripping share the same returned
NFC bytes. Authored NFD text, references and suffix bytes are never rewritten;
64-scalar/CJK limits and original layout, cleaner, utility/server, cancellation
and stable-publication contracts are unchanged. Completed NFD words still
permit valid space-leading continuation. The 24-stream/12-pair NFC–NFD controls
match projections, token IDs, grammar, insertions, raw outputs, seeds and
non-timing work: 12 positive English completed-`café`/Japanese observations,
12 explicit no-offers for Portuguese completed-`café`/a different partial-word
sentence. Structural parity is not universal prompt quality or host-edit proof.

Fourteen remaining absolute quality failures are ten replay-only and four
replay-plus-comma raw stops, with no useful continuation, no execution error
and ample capacity; cleanup discarded no useful insertion. They are not
external blockers or evidence of unfixability. Terminal token IDs are unknown,
so EOG specifically is **[INFERENCE]**. Full64 still exits 1; no further
production defect was justified by that investigation and fixtures are not weakened.

The 52 long controls retain identical inputs/final outputs, native non-timing
work, observed seeds and failure/no-offer/cancellation states; four callback
counts differ. `completed_run=true`, `failed_requests=0`. Both
`contract_matched=false` flags mean `--match-report` was omitted; the parent
independently compared all rows and contract fields, not an automatic strict
preflight pass. This is explicit `context_chars=8000`, not production allocation.
Earlier baseline native C/Rust hashes differ; only the current pre/post-NFC
full64 comparison isolates the correction. No latency claim follows.

The [canonical-worker NFC receipt](../../evidence/rust-migration/nfc-worker-healing-current.json)
passes three target cases with `o preventiva é crucial `, protocol completion,
no errors and acknowledged release/shutdown. This historical short-probe receipt
retains **command exit 1** and three `inconclusive_request_already_completed`
after-result probes. Before-proceed cancellation passed three rounds; neither
route admitted later outputs. Raw child exits remain unobserved. Current
corrected-clock cancellation evidence below does not rewrite that old result.

### Current follow-up contract register (2026-11-07)

- [Canonical cancellation](../../evidence/rust-migration/canonical-cancellation-current.json)
  now passes the observed supervisor active-window/accounting/retirement contract:
  parent exit 0, both probes and acknowledged release/shutdown in three sessions.
  A fixed ordinary 128-token/32-word after-result probe observes a nonempty nonfinal
  IPC result while the matching request is active; ordinary cases retain 24 tokens /
  medium length. Native cancellation acknowledgement and interruption remain null:
  consumer silence is not proof of producer interruption. Actual observation/
  retention timestamps now follow lock waits, rather than reusing step-entry time;
  operational grace/idle rules, ownership and accounting are unchanged. The two
  clock regressions passed in the 737-reported-pass Mac source gate (732 parent +
  five child, 26 summaries, one ignored), not a physical-freshness or GPU proof.
- [No-healing boundary/replay experiment](../../evidence/rust-migration/no-offer-boundary-replay-current.json)
  is rejected, not a production contract change. Both arms completed 54 requests
  with 19 no-offers and no stream/projection errors. Original target-boundary
  no-offers improve 14/15 → 1/15, but trailing-space no-offers regress 0/21 → 18/21
  and all three protective partial-word requests split `feedback` into `feed back`.
  Full authored prefill changes replay metadata/grammar/stripping/reservation
  together. Synthetic context selection (8,000-byte budget) and cold KV-cleared
  scheduling differ from retained full64; Japanese ASCII space is diagnostic only.
  No full64/long/per-keystroke adoption gate or no-healing adoption follows.
- [Assistant semantic review](../../evidence/rust-migration/semantic-review-current.json)
  attaches 337 dimension labels to 89 unique IDs covering all 192 occurrences:
  277 acceptable, 25 unacceptable, 11 uncertain, 24 not applicable. Original
  generated/mechanical/native/timing fields remain unchanged: **178/192**, with
  14 no-offer failures. The selected blind audit (29 flagged IDs + six controls)
  has 143 agreements and 13 unadjudicated disagreements across 156 dimensions.
  Preserve both sets; no automatic overall grade, human validation or population
  calibration. Portuguese leakage, unsupported timing and malformed Portuguese
  synonyms are agreed defects, not evidence that all other utility judgments pass.
- [Actual Linux AMD64 execution](../../evidence/rust-migration/emulated-linux-current/receipt.json)
  adds real emulated x86-64 native full-shell execution: 99 assertions, stage 45,
  clean exit and no error. Serial locked/offline all-target source tests plus
  binaries/examples passed (693 parent + six child = 699 reported passes,
  27 summaries, two ignored). The initial concurrent library run retains three
  EAGAIN/eight-second UnixStream read timeouts (568 passes, one ignored); no
  deadlines/assertions changed and no cgroup PID/memory/OOM events were observed.
  Source tests and Xvfb/Openbox/Picom UI evidence are not native Windows,
  physical-input, installer or release acceptance. **All 15 gates remain held**;
  ordinary Wayland inline/preedit remains disabled.


All seven [isolated prompt/cache decisions](../../evidence/rust-migration/prompt-cache-ablation-observations.json)
remain complete and unadopted. Reference freezing regresses grounding/work;
learned freezing preserves evidence/output/decoded work and reduces API calls,
without measured CPU-time benefit. Production keeps original v19/v22
full-prefill/full-SWA/flash-off. Historical saved-state/learning negatives
remain intact. GPUI evaluation is complete with the candidate rejected and
native presentation retained; unmet IME/assistive/resource/all-platform proof
is not a native product pass. No performance, power or physical-input latency
conclusion follows.

### Completed source-wave invariants

- [Transaction-local AX frame reuse](../../evidence/rust-migration/ax-read-reuse-current.json)
  reduces three native frame acquisitions to two for three requests with one
  hit and identical captures. It never substitutes for fresh content, security
  or authority checks; reuse ends before appearance/focus/anchor barriers.
  Independent native widget readback remains mandatory. The owned-child,
  hookless MacBridge run is not Controller/global-hook/secure-field or
  arbitrary-host acceptance.
- [Historical pre-NFC stable publication](../../evidence/rust-migration/publication-frontier-current.json)
  preserves that wave's 192 text/mechanical/error outcomes and 269 native
  raw-output/seed/non-timing-work records against its pre-wave control.
  Both cancellation kinds and clean owner close pass three rounds in that
  scope, with the historical 175/192 absolute result. Current NFC full64
  retains 269 corpus native calls, 117 nonempty known-terminal emissions
  before native return and 211 exact nonempty final-offer agreements, zero
  mismatches (58 unavailable comparisons). This is synchronous ordering,
  not worker/compositor/GPU latency or universal semantics; current quality
  is 178/192, still an absolute failure.
- Generic UIA grants replacement only for its exact existing selection, not
  arbitrary range movement; a collapsed caret still allows insertion. Native
  Edit retains checked range selection. Before any effect, current content,
  selection and authority must match. Independent readback confirms unchanged
  surrounding text, collapsed selection and exact scalar caret for the requested
  insertion or its nonempty scalar prefix. A selection-derived capability change
  after the edit is not an authority change; uncertain outcomes are not retried.
  [Portable Windows decisions](../../evidence/rust-migration/portable-windows-decisions-current.json)
  now use canonical `platform::windows_decisions` under
  `cfg(any(target_os = "windows", test))`, shared with native Windows hooks.
  All **12 pure decision tests executed and passed on both Mac and Linux**.
  Previously the module was nested only under the Windows platform gate;
  generic Runtime selected-replacement/readback tests were already exercised.
  `selected_replacement_capability_loss_counts_once_only_for_exact_readback`
  also passes in the current gates. This is not native Win32/MSVC/SDK
  typechecking, desktop execution or hook-lifecycle acceptance.
- [Current target-owned GTK3/Qt6 acceptance](../../evidence/rust-migration/target-owned-client-current.json)
  passes compact ordinals, pre-paint refusal and selected/collapsed Unicode/NFD
  replacement/readback/undo. Native UTF-16 active caret/length and scalar
  replacement ranges are not interchangeable. Synthetic dispatch/private XTest
  does not establish physical typing, real IME or general Wayland inline support.
- [Deterministic word tools](../../evidence/rust-migration/word-tools-current.json)
  pass 80 public-API checks for word boundaries, emoji, decoded choices/ordinals
  and pure acceptance, not model semantics, next-word fallback or native insertion.
  No new trigger preference is justified.
- [Actual generated notices](../../evidence/rust-migration/emoji-notice-current.json)
  include embedded Unicode emoji data 17.0, its source and full Unicode License
  V3 while preserving Cargo/native notices. This is inventory coverage, not
  packaging/install or legal certification.
- [Stock AT-SPI capture/refusal](../../evidence/rust-migration/stock-atspi-current.json)
  records four exact native snapshots against independent GTK readback and eight
  unchanged rejected mutation attempts. Inactive composition is not established,
  inline/range geometry is unavailable and no positive insertion was attempted
  or confirmed. Successful capture/refusal does not grant insertion capability.

Implementation/evaluation completion is distinct from the unchanged
[public acceptance gates](parity.md#remaining-prerequisites-and-authority):
the three NFD failures are fixed but 14 raw-stop/no-useful-continuation quality
failures remain; there is no accepted decoded-work candidate, demonstrated
learning relevance gain, or measured power/input-to-compositor improvement.
Current Windows/MSVC desktop, full KWin manual acceptance, physical/mixed-display/
Spaces, assistive and permission-revocation acceptance, full disposable Mac shell
and release prerequisites remain unmet. [Actual emulated KWin execution](../../evidence/rust-migration/emulated-linux-current/receipt.json)
is partial, not unavailable tooling: KWin 5.27.11 nested X11/software Mesa mapped
a native GTK Wayland editor with exact NFD/non-BMP readback and focus property
true (`window_active=false`). A compositor-inherited connected AF_UNIX stream
peer was validated and product logs show input-method/input-panel bind requests
and activation. Final post-fix strict exit 2 remains, with 10/13 gates passing:
`ListNames` returned no `org.kde.KWin*` names and executable observations showed
the Rosetta interpreter despite matching process start ticks. Exact D-Bus identity,
product executable identity and identity-qualified binding remain false.
Raw requests do not grant acceptance. Missing EGLImage pixmap
support/compositing disabled and KDE decoration/QML modules are logged limits.
The owned image is evidence only; no model/inference ran and forced process
cleanup is not canonical Quit/release. Corrected UP-interface/no-route guards
passed; the earlier down-tunnel helper refusal stays historical. Full manual
commit, physical IME and normal KDE selection remain unmet; all 15 gates stay held.
The probe also verified a real production teardown correction:
`wayland_wire.rs::drop_keyboard` destroys the local version-1 keyboard proxy
without sending `keyboard.release`; owning-context teardown remains. Before the
fix, grab → deactivate → release produced invalid-object-11 `wl_display.error`
before cleanup. After the fix, actual grab → deactivate → context destroy has
no release or protocol error. Product and KWin logs each change from one/zero
pre-/during-cleanup errors to zero/zero, with the complete product log retained;
the new no-pre-cleanup-protocol-error gate passes. Post-fix Linux serial locked/
offline all-target tests and bins/examples passed again (693 parent + six child =
699 reported, 27 summaries, two ignored), as did parent formatting. X11's
99-assertion native-shell receipt predates this Wayland-only change; X11/UI source
is unchanged, but no post-fix X11 rerun is claimed.
Canonical Mac Welcome aborted before inference on input interruption; no positive
bilingual pass or automatic retry is claimed.

### Native settings and platform contracts

- Catalog model size, minimum physical memory and license name/URL are canonical
  metadata, not measured runtime memory or guaranteed fit. Zero minimum memory
  means unspecified, not zero consumption: Mac omits the line like missing
  metadata, GTK uses its unavailable fallback, and Windows displays `Unavailable`
  without a static bytes suffix. The common allocation-free `ram_label` formatter
  uses two-decimal binary GiB for catalogue/detected RAM labels and insufficient-memory
  availability warnings. Positive fractions are not integer-truncated: 8.50 GiB,
  0.50 GiB and 1,000,000,000 bytes → 0.93 GiB. Download/transfer formatting remains
  unchanged (Gemma's download still shows 2.8 GB); eligibility stays byte-exact.
  Eligibility and capacity detection are unchanged: `model_availability` rejects
  insufficient physical memory only for a positive minimum threshold. Physical
  RAM stays separate and unknown hardware stays unknown. Windows presentation
  remains source-reviewed/unexecuted; download license acceptance stays explicit.
- Ordinary `max_tokens` is 1–128, further bounded by the completion-length
  profile; utility requests keep their independent 128-token cap.
- Mac hard-exclusion add/remove edits preserve case-insensitive app identifier/
  name substring semantics. Built-in terminal identifiers retain the explicit
  AI-prompt exception; custom exclusions are hard blocks, shell prompts excluded.
- Windows shortcut collisions cannot silently erase/reassign another action.
  OCR, terminal, native dictionary and Mac compatibility capabilities remain
  unavailable; preserving saved values is not enabling support.
- Language defaults, nonempty global, app and exact-host instructions append;
  empty scoped text cannot erase inheritance. Strength zero suppresses learned
  guidance without changing recording/use consent or deleting history. Context
  consent ceilings remain independent.
- Statistics expose year periods and month grouping. Estimated seconds are
  accepted characters divided by five, not measured productivity. X11 diagnostics
  distinguish actual shortcut conflicts, stopped hooks and disabled interception
  from prediction enable/pause state.
- Wayland source is manual-only: no ordinary inline/preedit or popup fallback.
  Explicit KDE input-method selection is required for that manual lane and does
  not expand it. Independent target-owned GTK3/Qt6 clients retain separate
  ownership, contracts and acceptance; no Wayland placement follows from them.
  Windows/Linux remain alpha and release holds are unchanged.


## Independent first implementation packets

These do not require unproven UI/model bindings. Each owner writes complete owned modules plus relevant behavior tests, no builds/tests/formatters mid-flight. Dependency requirements go to parent; no Cargo edits.

### Configuration owner

Own `crates/brotypist/src/config.rs`, `paths.rs`, `shortcuts.rs` and module-local tests. Public API:

- `pub struct Config`: same public field names as Python Config at `config.py:121-177`; serde Serialize/Deserialize/Clone/Debug/PartialEq. String enums remain String at this boundary; numeric config fields use `u32`; booleans exact; `excluded_apps: Vec<String>`, `shortcuts: BTreeMap<String,String>`, `app_rules: BTreeMap<String,BTreeMap<String,serde_json::Value>>`. serde defaulting must preserve existing defaults, not silently accept wrong typed fields.
- `impl Default for Config`, `Config::validate(&mut self) -> Result<()>`, `Config::provider_identity(&self) -> Vec<String>`.
- `pub fn load_config() -> Result<Config>`, `save_config(&Config) -> Result<()>`, `config_path() -> Result<PathBuf>`, `default_instructions(locale: Option<&str>) -> String`, `normalize_domain(&str) -> Result<String>`, `local_endpoint(&str) -> Result<String>`.
- `paths::{config_dir, cache_dir, model_dir, update_dir}() -> Result<PathBuf>`: preserve profile, cache and OS-registration identities. Models now use durable app data, or `INKLOW_HOME/models` for isolated profiles; private profiles never inspect the normal legacy model cache.
- `config::model_storage_migration_needed(&Config) -> Result<bool>` is a path/catalog-only eligibility check. `config::migrate_model_storage(&mut Config, &models::DownloadCancellation) -> Result<bool>` verifies and retains selected old-default primary and configured battery weights without deleting old bytes or saving settings. Its return value reports primary-path adoption only; battery-only retention requires no settings write. Existing durable battery selections remain authoritative. The runtime owns background work, latest-primary checks and path-only adoption into the latest settings.
- `shortcuts::normalize_shortcut(&str) -> Result<String>`, `default_shortcuts() -> BTreeMap<String,String>`, `parse_shortcut(&str) -> Result<Chord>`; `Chord {pub key:String,pub modifiers:Vec<String>}`. Physical key mapping helpers preserve source behavior.
- Initially validate catalog IDs against the exact baseline stable ID set; models owner later shares a single catalog source during integration, no circular config/models dependency needed.

### Storage owner

Own `crates/brotypist/src/storage.rs` and module-local tests. Depend on `config::Config`, `paths`, no core/native modules. Public API:

- `pub struct HistoryStore`; `HistoryStore::open() -> Result<Self>` uses baseline directory/db and credential identity, **does not unlock/create a key merely by opening**.
- `unlock(&mut self) -> Result<()>`, `lock(&mut self)`, `is_unlocked(&self)->bool`, `record(&mut self, app:&str,domain:&str,text:&str,authored_spans:&[(usize,usize)]) -> Result<bool>`, `records(&self, app:Option<&str>,domain:Option<&str>,limit:usize) -> Result<Vec<HistoryRecord>>`, `delete(&mut self,app:Option<&str>,domain:Option<&str>)->Result<usize>`.
- `HistoryRecord` public id/app_id/domain/text/created_at Strings and authored_spans Vec<(usize,usize)>; exact serialized payload naming/version must match Python, not these convenient Rust field names.
- Compatibility helpers `seal_record(key:&[u8;32],id:&str,payload:&[u8])->Result<Vec<u8>>` and `open_record(...)` support isolated cross-language smoke. Preserve AES-GCM nonce/AAD/keyring formats, authored scalar offsets, corruption/lost-key refusal and no plaintext fallback. Never access normal credentials/profile during implementation/checks.
- `record_statistics(&mut self,app:&str,text:&str,new_completion:bool)->Result<()>`, `statistics(&self,days:u32,grouping:&str)->Result<serde_json::Value>`, `clear_statistics(&mut self)->Result<()>`, `personalization(&self,app:&str,domain:&str,prefix:&str,strength:u32,source_allowed:&dyn Fn(&str,&str)->bool)->Result<String>`. Explicit identified-session/time inputs and temporary-unlock/count/info operations are approved additions; do not infer consent from source names.
- If source method semantics require additional input (language/time/consent), expose explicit typed request/additional methods; do not weaken gating to fit a signature. Notify parent of changes.

### Pure core/features owner

Own `crates/brotypist/src/core.rs`, `features.rs`, `spelling.rs` and module-local tests; shared snapshot/render types belong in `core.rs`, not a duplicate api module. Depend on config/shortcuts public contracts above.

- `pub type Rect = [f64;4]`; `InlineRender` and `Snapshot` have baseline public field names and types (Strings, usize scalar offsets, Option<Rect>, booleans, optional InlineRender, i32 window_pid). Derive Clone/Debug/Serialize/Deserialize/PartialEq where possible; defaults allow native constructors with `..Default::default()`.
- `Snapshot.text: TextBuffer`: immutable shared text (`Arc` ownership) with a lazy runtime-dependent checked `TextIndex` scalar/UTF-8/UTF-16 map. TextBuffer supports `From<String>`, `From<&str>`, `Deref<Target=str>`, `AsRef<str>`, Display, exact-text PartialEq and serde as a plain JSON string. Clones reuse text/maps; replacing the wrapper creates a fresh revision. No public mutable String plus cached-copy/fingerprint checks on every access. Native constructors use `text: observed.into()`.
- Snapshot methods `prefix()->&str`, `suffix()->&str`, `selected_text()->&str`, `line_suffix()->&str`, `inline_safe()->bool`, `has_anchor()->bool`, `anchor_rect()->Option<Rect>`, `eligible(min_chars:usize)->bool`, `same_key(&self,&Self)->bool`, `same_authority(&self,&Self)->bool`. Authority additionally compares window/document identities; geometry alone is not authority. Invalid scalar range yields safe ineligibility, never panic/native edit.
- `InlineRender::valid(caret:Option<Rect>)->bool` implements exact finite/clip/baseline/suffix-color rules.
- `SuggestionState` owns generation, snapshot and suggestion; expose `new/default`, `invalidate()->u64`, `offer(generation:u64,snapshot:Snapshot,text:String)->bool`, `advance(&Snapshot)->bool`, `accept(&Snapshot,full:bool,trailing_space:bool,trailing_punctuation:bool,visible:Option<&str>)->Option<String>`. Fields/accessors public for integration. Preserve pure update semantics; native confirmation is runtime responsibility.
- features preserve original named helpers: `length_profile(&str)->(u32,u32)`, `shorter_length(&str)->String`, `include_punctuation(&str,bool)->bool`, `current_word_only(prefix:&str,continuation:&str)->String`, `emoji_choices(prefix:&str,config:&Config)->Option<(String,Vec<String>)>`, `synonym_prompt(prefix,suffix,selection)->String`, `parse_choices(text:&str,max_choices:usize)->Vec<String>`; use embedded baseline emoji JSON, not generated substitutes. Reject decoded structural markers/line/control/format text and support safe utility abstention.
- Spelling module preserves complete extraction/language/range-mapping/planning/state contracts, with injected native dictionary backend; no fake spelling result when native backend unavailable. Additional result/backend types documented in handoff. This module does not run OS APIs or start app lifecycle.

## Parent integration obligations

Use language-server references when available before exported API changes. Parent adapts callsites to a single canonical API; no aliases/shims. Shared inferred signatures are a starting contract, not permission to reduce source behavior. An owner finding insufficient inputs must send concrete proposed contract changes before changing another owner's files. Full remaining controller/native/UI/provider/updater packets are dispatched only after their actual prerequisites stabilize.
