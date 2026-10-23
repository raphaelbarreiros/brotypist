> Historical record: Named third-party research references were removed during repository-history cleanup. This edited historical record is not an original authenticated receipt. Recorded build identifiers and measurements describe their original observations.

# Changelog

## 0.3.0 — 2026-10-24

- Replace the editor with a menu-first agent: native AppKit preferences/overlays on macOS, native-themed Qt tray/preferences on Windows/Linux. Remove obsolete editor and in-process UI worker APIs; omit Qt from the Mac dependency/bundle.
- Add eleven native preference panes, explicit setup/permission links, compact General settings with expandable model management, configurable physical-key shortcuts, per-app/domain rules, custom-instruction Save/Revert, battery controls, and actual accepted-writing statistics.
- Move inference into a lazy, bounded stdio worker. Release the process after idle, pause, model change, and quit; preserve newest work during a same-model cold load. Document cold-load latency instead of promising instant predictions.
- Add opt-in OS-keychain-backed AES-GCM history, bounded vocabulary personalization, explicit deletion, and optional clipboard/scoped-window OCR context. Keep default recording/context off and disclose app recording overrides.
- Add offline Unicode 17 emoji/emoticon choices, skin/gender variants, contextual selected-word synonyms and alternative continuations. Improve utility prompts using actual failing/working local-model samples; do not invent dictionary fallbacks.
- Revalidate Windows UIA focus/protection before selection replacement, respect app/domain Tab precedence without disabling non-Tab shortcuts, preserve emoji precedence over pause alternatives, and fix macOS zero-length AX range row geometry using adjacent observed glyph bounds.
- Preserve REA-authenticated native reference evidence privately where preferences could contain personal text; publish sanitized records and real synthetic native UI/typing screenshots. Extend frozen-runtime verification to the actual isolated worker protocol.
- Fix a portable-settings field/method name collision found by Windows/Linux CI; exercise the actual Qt preferences and saved settings in an isolated offscreen smoke before retrying hosted verification.

## 0.2.0 — 2026-10-23

- Preload/warm up inference outside the UI thread, use available GPU offload, stream completed words, cancel obsolete work, and reuse matching typed-through suggestions. Keep snapshot/privacy guards on every update and acceptance.
- Recommend a checksum-pinned Qwen2.5 1.5B guided writing model instead of the 135M English starter; add a cancellable model manager, installed-model selection, explicit CPU/auto acceleration, and direct/guided prompt profiles.
- Add original application and active/paused tray icons, native menus, timed pause, Show/Hide, global-mode control, settings/exclusions, diagnostics, About, and explicit guarded Quit.
- Support resident close-to-tray with no-tray fallback, UTF-8 Open/Save/Save As with atomic writes, and unsaved-change protection. Keep a new untouched editor clean and allow final visible-window closure after workers stop, so native Quit completes safely.
- Build native macOS/Windows/Linux applications with bundled runtime and icons, separate Windows console CLI, and Linux desktop integration. Exclude models/settings; protect replacement with an ownership marker and rollback; finalize macOS signatures after adding resources.
- Extend three-OS CI to native builds and frozen-runtime real inference. Preserve synthetic English/Portuguese outputs, measured latency, actual resident UI screenshots, and explicit model-quality limitations.

## 0.1.0 — 2026-10-23

- Add an independent cross-platform Qt predictive-writing editor with real local inference, word/full acceptance, dismissal, undo, and IME/focus safeguards.
- Add local GGUF, Ollama, and OpenAI-compatible continuation backends; refuse remote/proxied/redirected inference routes.
- Add verified, immutable-revision starter-model acquisition without committing weights.
- Add opt-in macOS AX/Quartz, Windows UIA/native input, and Linux AT-SPI/X11 integration with protected-field and stale-snapshot checks.
- Use UTF-8 byte lengths for AT-SPI insertion while preserving character-based caret offsets.
- Normalize generated tabs to spaces so accepted predictions cannot navigate forms.
- Install native Qt libraries in headless Linux CI and exercise a real offscreen desktop launch on every OS.
- Explicitly refuse system-wide Wayland interception; retain the built-in editor.
- Preserve REA [redacted reference] evidence, annotated architectural inferences, actual Inklow screenshots, and bounded verification results.
- Add macOS/Windows/Ubuntu CI checks and installable wheel builds. Interactive Windows/Linux integration is not yet verified.
