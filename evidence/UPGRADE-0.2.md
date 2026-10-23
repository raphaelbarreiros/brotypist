# Inklow 0.2 upgrade evidence

## What changed and why

The original implementation loaded its model lazily, forced CPU inference, waited for an entire completion, and started with an English 135M model. These are code-level causes consistent with the reported latency/quality problems, not a measured latency baseline of the user's running process. The user's original process and live settings were left untouched to preserve its unsaved document.

The replacement preloads and warms the model off the UI thread, uses available GPU offload, streams completed-word candidates, cancels superseded generation, and preserves useful typed-through candidates without another inference. Revision/generation/field/text/caret validation still gates display and acceptance. Native loading/prompt evaluation cannot always be interrupted immediately; local HTTP reads remain timeout-bounded rather than instantly cancellable.

The recommended model is checksum-pinned Qwen2.5-1.5B-Instruct Q4_K_M, with a versioned ChatML assistant-draft prefill (`writing.py`, `qwen-draft-v2`), bilingual examples, and a one-sentence/eight-word ceiling. Custom base models retain direct continuation. Tiny remains available for low-resource machines. Model download/selection and preference changes remain explicit.

## Actual inference samples

Machine: Apple M1 Pro, 16 GB unified memory, macOS 27.2, Python 3.12.14. Config: 24 tokens, 200 ms debounce. Synthetic drafts only; no user documents/settings were read by the evaluator. GPU and CPU evaluations ran serially.

| Final recommended model | Samples | Nonempty suggestions | First usable word median | Observed p95 | Preparation |
| --- | ---: | ---: | ---: | ---: | ---: |
| Auto / Metal | 32 | 28 | 62.5 ms | 86.84 ms | 429.93 ms |
| Forced CPU | 16 | 13 | 113.7 ms | 264.67 ms | 1,390.68 ms |

The percentile uses the sorted observed sample at floor(0.95 × (n−1)); empty suggestions are omitted from first-word timing. Timings start at provider invocation: **debounce, typing, global polling, and rendering are not included**. These are warm-machine observations, not a cold-start bound or universal speedup. The first Metal library compilation can take seconds. In a separate actual Qt resident-editor run, first visible ghost text appeared **459.7 ms after synthetic typing**, including the configured debounce.

Raw outputs (including failures/empty results):
- [Final Auto/Metal](upgrade-balanced-auto.json)
- [Final forced CPU](upgrade-balanced-cpu.json)
- [Tiny on the new streaming pipeline](upgrade-tiny-cpu.json) — not an original-version latency benchmark.
- [Rejected Qwen3 Base trial](upgrade-qwen3-base-rejected.json) — historical `balanced` trial label, different model hash; rejected because of translation leakage, placeholders and low-quality continuations. It is not the shipped preset.

Observed final examples, first run:
- `I look forward to hearing` → ` from you soon.`
- `Fico no aguardo da sua` → ` resposta.`
- `Gostaria de agradecer pela aten` → `ção.`
- `A colaboração entre as equipes é importante porque` → ` cada um tem suas próprias habilidades.`

**Known quality failures:** `I would appreciate your feed` → ` back.` is a bad partial-word completion. The Portuguese project-next-step case produced a punctuation-only placeholder and was withheld, not repaired into an invented answer. Suffix overlap may correctly leave an empty suggestion. Sampling remains nondeterministic; these examples are not a calibrated bilingual quality score or a factuality guarantee.

Reproduce with explicitly downloaded models:

```sh
uv run python tools/evaluate_predictions.py --preset balanced --acceleration auto --output /tmp/inklow-auto.json
uv run python tools/evaluate_predictions.py --preset balanced --acceleration cpu --runs 1 --output /tmp/inklow-cpu.json
```

## Actual resident utility surface

[Machine-readable resident smoke](upgrade-resident-smoke.json) records the actual macOS tray being available/visible, model preloading, real streamed model output, accepting a streamed word, shared pause/resume and timed-pause actions, close-to-tray/reopen retaining unsaved text, installed-model selection being available, an explicit UTF-8 save, and worker/tray shutdown. The smoke used isolated temporary preferences and a synthetic draft, not the user's editor.

- [Editor screenshot](upgrade-editor.png)
- [Tray menu screenshot](upgrade-tray-menu.png)
- [Models screenshot](upgrade-models.png)

The tray has Show/Hide Editor, Pause/Resume, 5/15/60-minute pause, opt-in global integration, Models, Settings and Exclusions, Diagnostics/Permissions, About and explicit Quit. The window remains recoverable when no usable tray is available. There is no login-item installation. File saves are explicit and atomic, with unsaved-change guards; no autosave/history.

## Native application

The local build produces `dist/Inklow.app`, installed at `/Applications/Inklow.app`. It includes an original multi-resolution icon and runtime/native inference libraries, but no model weights or user settings. Identifier: `com.inklow.Inklow`; version: `0.2.0`; icon: `app.icns`. macOS resource signing is finalized after adding the ownership marker; builds verify their final signature before publication. The signature is ad-hoc, not notarization.

The native smoke uses isolated HOME/preferences, scopes accessibility inspection and window capture to its own launched PID, and invokes the real native Quit action. [Native receipt](upgrade-installed-app.json), [installed window](upgrade-installed-app.png), and [actual running application icon](upgrade-installed-icon.png). Frozen-runtime Tiny CPU completion is separately exercised by `tools/smoke_bundle.py`, not by a fake inference backend.

Windows builds produce a GUI executable plus console CLI; Linux builds include application/desktop integration. CI builds on each target OS and exercises frozen-runtime inference. Interactive Windows/Linux global-input and native tray behavior still require real desktop verification; this work does not convert those boundaries into claims of full platform parity. Wayland system-wide interception remains explicitly unavailable.

## Verification receipts

Local deterministic suite: 78 passed, including stale-stream rejection, cancellation, typed-through acceptance, protected-field guards, model corruption/cancel safety, save/quit lifecycle, native-build overwrite/rollback/private-payload boundaries, prompt-role escaping and placeholder abstention. Native UI smoke also found and fixed two issues that tests alone had missed: an untouched document's formatting marking it dirty, and final application Quit being vetoed by its own visible window's close handler.

Published implementation `dd37002e96137e662c60765a8529c17d683387ea` passed [run 40023107453](https://github.com/renan-brasilio/inklow/actions/runs/40023107453) on **macOS, Windows and Ubuntu**. All three jobs passed regressions, real local inference, offscreen desktop launch, wheel/native application builds, frozen-runtime inference and artifact upload. [Machine-readable upgrade receipt](upgrade-ci-run.json). Detailed check annotations were inaccessible to the CLI token (403), but job/step status reads succeeded. Earlier 0.1 evidence remains in `ci-run.json`; it is not the 0.2 result.
