# Prediction performance: quality-preserving implementation plan

**2026-11-06 release decision:** The maintainer tested the backed-up local M1
installation, reported that it feels better, and explicitly authorized a new
patch release. Version 0.5.4 packages the retained implementation; this human
acceptance is not a measured completion of every quality or latency target below.
Historical local-only boundaries describe their original checkpoints, not this
subsequent release authorization. Public history cleanup does not authorize
changing old signed build identities or discarding recorded application failures.

**Historical scope:** this document records the published 0.5.4 investigation
and implementation choices, not the current Unreleased candidate's settings or
diagnostic API. The candidate uses contiguous v19/v22 full-prefill, 64/32 batching,
full SWA and flash disabled. Its canonical request/session accounting and native
draw attribution do not preserve the old child-phase decomposition or synchronized
sampling timings losslessly. The old native-smoke recorder and wake-owner module
are retired rather than kept as parallel implementations. Current behavior,
evidence and remaining gates are in the [Unreleased register](README.md#current-unreleased-evidence-not-release-approval)
and [diagnostic contracts](contracts.md#diagnostic-and-acceptance-boundaries).

## Objective and constraints

Make BroTypist useful during continuous writing, not merely fast at producing its first raw token. Measure the first displayable insertion after hidden healing replay, actual overlay appearance, useful-offer coverage, and Tab acceptance. Retain the model, authored context, references, personalization policy, authorization checks, and cancellation safety. Model-only timings do not establish the desktop experience.

The available M1 Pro with 16 GiB is the acceptance target, as explicitly requested by Renan. M5 access is not a prerequisite. Warm targets are a median useful visible suggestion within 300 ms of the final edit and p95 within 600 ms, together with useful-offer coverage and writing quality. These are targets, not measured achievements or universal hardware guarantees. Cold preparation, genuine reference changes, and a sliding context window must be reported separately.

The completed sampling, TextEdit geometry and 128-token prefill implementations are recorded in the [verification report](README.md#27-textedit-geometry-stage-attribution-and-128-token-prefill) and [source-bound receipt](../../evidence/rust-migration/macos-textedit-prefill-unreleased.json). The remaining work below is not claimed as implemented by the authorized patch release.

## Established evidence

The earlier native prefill/output-row and clipboard-authorization changes are recorded in `../../evidence/rust-migration/macos-prediction-latency-unreleased.json`. They improved the 1,992-character desktop fixture from a 1,105 ms median to 963 ms, but did not establish an adequate overall experience. Those observations count WindowServer panel appearance, not first painted pixels.

A changing healing-tail description precedes the assistant-prefill draft. It invalidates otherwise reusable draft state on partial-word edits. References already precede the draft; proposing that ordering again would not fix the problem. Actual reference changes and left-sliding draft windows still require new causal evaluation.

Removing only the healing description was tested with the complete 64-case synthetic corpus, three rounds per build. Mechanical passes rose from 183/192 to 192/192, but blinded model review identified new malformed Chinese, Japanese suffix joins, and language switches. That ablation was rejected and the original prompt restored. More available output is not necessarily better writing.

The sequential corpus also exposed a measurement limitation: variable utility retries can shift later requests' sampler seeds. A matched seed sequence or independently initialized cases is required before attributing every changed output to a prompt revision. The early Chinese regression occurred before that drift and is sufficient to reject the ablation. Model review is not calibrated human evaluation.

The native grammar implementation decodes candidate UTF-8 across the full vocabulary before top-k filtering. Recorded calls spent roughly 25–30 ms per sampled token in sampling. This is a separate optimization opportunity that does not require altering model conditioning.

## Implementation order and promotion gates

### 1. Preserve sampling semantics while avoiding full-vocabulary grammar work

Use a bounded high-logit shortlist only when it provably contains at least k grammar-valid tokens. Every omitted score must be strictly below the shortlist boundary. Keep the original grammar-first path for insufficient valid candidates, ties, disabled/large top-k, and exceptional logits. Apply repetition penalties before this step, advance grammar only on acceptance, and retain the original downstream top-k/top-p/min-p/temperature/distribution order.

The shortlist must allocate no per-token storage and must not mutate the original candidate array before a fallback. Compare complete sampled distributions and evolving state against the original path, including low-ranked valid tokens, sparse masks, ties, masked scores, cloning, and reset. Then require the real-model corpus to retain output, offer, error, and literal-fact behavior. Real worker and desktop timings—not instrumented sampling time alone—decide whether to retain the optimization.

Implemented locally with fixed 128-candidate storage and in-place heap sorting; no per-token wrapper allocation. The original prompt, model, context and sampler parameters remain intact. Independent review led to explicit omitted-boundary-tie and untouched-fallback tests and removal of a libc-dependent `qsort` allocation. This is not an installed release.

### 2. Revisit prompt-cache layout only with stronger conditioning and independent quality evidence

Do not ship the rejected metadata-removal ablation or special-case its failing languages. A future layout must place changing task data after the reusable draft while preserving all authored characters and clear continuation conditioning. Compare the complete corpus with matched per-case seeds, plus realistic character-by-character traces, partial words, combining marks, CJK, suffixes, structural-token injection, and changed references. Reject newly corrupted insertions or unsupported facts even when aggregate availability improves. Do not reduce context or substitute a smaller model to produce a favorable latency number.

The independent per-case prerequisite is implemented in the direct runner: `fnv1a64-base-round-case-v1` derives a state from the base seed, zero-based round and full case ID. Diagnostic reset changes only RNG state, not KV/model ownership; production seed policy is unchanged. The complete 64-case corpus still runs three rounds. Existing English locale-preference, French agreement and Japanese suffix no-offer failures remain release-quality work; exact baseline equivalence does not fix them.

The retained local layout keeps at least 256 recent Unicode scalars in the model prefill and puts the complete earlier draft, as untrusted JSON, before changing healing metadata. It does not truncate context. Only incremental inline requests use this split: canonical long-context testing found invented Japanese schedules when batch alternatives used it, so alternatives retain full-draft prefill and their request provenance survives internal sampling. Utilities remain unchanged. An uncertain native language result may use the completed words of the same clause, with the original confidence gate; no threshold or language-specific exception was added.

The [M1 cache record](README.md#29-m1-independent-seeds-and-incremental-prefill) separates sustained callback gains from slower first-long requests, unchanged/shared quality failures and the remaining desktop gate. Full-draft relocation and both replay-wording experiments were rejected rather than promoted from aggregate mechanical counts.

### 3. Validate the actual writing workflow

Use an owned native text editor, one predictor at a time, and alternating run order. Exercise short English and Portuguese drafts, near-2,000-character drafts, continued typing, backspace, punctuation, reference changes, partial/full acceptance, and focus changes. Keep absence and timeout rates in the denominator. Save separate examples of genuinely painted ghost text and exact accepted text. Preserve existing cancellation/logit-validity recovery and stale-result checks.

The completed four-run baseline/candidate/candidate/baseline desktop trace used 64 offers and real final-character key events. Every Tab and Ctrl+Tab insertion passed readback and append-integrity checks. Warm short-input medians improved, but the 1,992-character draft did not. The first candidate launch still produced a 5.2-second request; a later candidate launch produced 564 ms. These are first-after-launch observations, not controlled cold-cache results.

Stage attribution is implemented in native-smoke only, with bounded content-free observations and isolated explicit export. Child preparation now separately measures backend initialization, model loading, context/batch allocation, framing and synchronized initial evaluation. In one fresh-process run, backend initialization took 5.418 seconds, model loading 1.798 seconds and initial evaluation 277 ms. Later preparations were around one second; no OS/Metal cache eviction was performed. A precompiled-Metal experiment preserved 12 matched worker outputs but did not improve warm generation, and its first initial evaluation took 1.403 seconds. Do not promote it from unmatched cold outliers or change preload policy to hide work before the stopwatch.

For sustained writing, measure append/backspace traces near the context-window boundary with stable and changed references. The next cache-layout experiment must improve these traces without dropping authored context or merely shifting work before the stopwatch. Capture useful-offer p50/p95, timeout/no-offer rates, cancellation drain and exact accepted text together. The current panel-appearance metric precedes proven paint; native presentation acknowledgement and separately inspected glyphs must not be conflated with first-painted-pixel timing.

The shared native prefill capacity is now 128 tokens. Full-corpus outputs/seeds/raw generations remained identical in 192 comparisons; canonical baseline/candidate/baseline worker medians improved modestly and real-model interrupted-prefill recovery passed in auto and CPU modes. Retain this bounded improvement, not a speculative larger batch. The long-reference worker still needs about 1.11 seconds, startup can need seconds, and writing-mode short desktop offers still took roughly 0.48–0.65 seconds in the measured run.

Next implementation order:

1. **Measure the retained candidate on the actual desktop.** Resume only with fresh foreground availability; use the retained candidate, alternating baseline/candidate order and realistic typing cadence. Keep absences/aborts and controlled-vs-uncontrolled cache labels. Do not reclaim focus after an unexpected switch. Historical private desktop harnesses are not shipped or retained as public tools.
2. **Use the child startup attribution.** The diagnostic implementation and fresh-process auto/CPU probes are complete. Keep the production embedded shader path: the isolated offline compilation trial is not a demonstrated end-to-end improvement. Before adopting precompiled assets, control cache conditions, cover idle reload/model changes, preserve BF16 and M5 tensor kernels, and verify build/package asset discovery on supported Macs.
3. **Verify event-driven handoff in the writing workflow.** A coalesced owner-thread wake and bounded immediate renderer acknowledgements are implemented without reducing the global poll interval. Real AppKit testing exposed copied NSEvents, invalidating pointer-identity matching; the owner now matches a per-instance event identifier. Retain fresh authorization and menu/shutdown isolation. Nonactivating lifecycle/idle checks do not replace matched typing-to-glyph p50/p95 measurements.
4. **Close the semantic-quality gaps.** Independent seeds and incremental-context reuse are implemented and exercised headlessly. Prioritize unsupported dates/rooms and alternatives, contextual usefulness, English locale-preference and French no-offer cases, and Japanese suffix behavior. Preserve the complete corpus and its failures; do not substitute mechanical validity, output length or baseline equivalence for quality. Distinguish safe abstention when a fact is unavailable from a missed useful completion.
5. **Re-run promotion gates on the M1.** Full corpus plus human/blinded quality review, real append/backspace/reference traces, active-generation cancellation drain, glyph/acceptance/focus safety, startup/idle reload and resident memory. Require the original 300 ms median / 600 ms p95 useful-offer targets together with quality and coverage; no parity/release claim from model-only timings. Do not block acceptance on an M5 test host.

## Delivery boundary

Record exercised checks, exact artifacts, rejected experiments, and remaining limits. Keep installed applications, normal saved preferences, permission grants, publication state and remote gaming hosts unchanged. Public receipts omit third-party research and must not be represented as unchanged authenticated originals. Local implementation and validation do not authorize a release.
