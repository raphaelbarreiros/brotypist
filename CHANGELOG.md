> Historical record: Named third-party research references were removed during repository-history cleanup. This edited historical record is not an original authenticated receipt. Recorded build identifiers and measurements describe their original observations.

# Changelog

## 0.3.2 — Unreleased

- Rename the public app, CLI, installers and private GitHub repository to BroTypist / `brotypist`. Keep established bundle, startup, credential-store and data identifiers so the visual rebrand does not reset settings, model caches, encrypted history or permission identity.
- Use the supplied Ghost Caret SVG artwork directly, including the thicker simplified small-size icon. Rasterize without added enclosure lighting/bevels; remove obsolete Composer exports and keep macOS generation Qt-free.
- Reorganize native AppKit and portable Qt preferences into General, AI Models, Autocomplete, Languages, Privacy and About, retaining the existing supported settings and configured defaults.
- Add six-step Welcome with actual OS-access status, explicit existing model management, truthful privacy disclosures and an ephemeral real-model try-it field. Reuse the current provider and acceptance rules; do not record preview text or retain a second preview worker after leaving the field.
- Replace ordinary suggestion chips with verified host-font, baseline-aligned, transparent grey inline text. Require safe native typography, line-end insertion, visible clipping and no composition/selection; unsupported inputs stay silent without consuming acceptance keys. Keep explicit alternatives separate.
- Keep source/release links private-access aware and preserve historical Inklow release assets and research evidence. This redesign does not add MLX, Sparkle, new models, new language-quality guarantees or changed acceptance keys.
- Add bounded, opt-in focused-window Accessibility context with protected/hidden-node pruning, exact field/window checks, relevance/deduplication, provenance and prompt budgets. Prefer the focused browser document over deeply nested browser chrome. Remove unsafe screenshot/OCR capture and its unused frameworks.
- Separate newest-only context collection from prediction preparation; revalidate policy before sensitive reads and discard revoked/stale results. Expose fixed content-free context and inline capability diagnostics.
- Default to 150 ms typing delay and five-minute model retention, preserving saved values. Dispatch prepared requests and streamed results onto the UI thread immediately; keep bounded fallback capture and result polling.
- Marshal Qt clipboard reads to the GUI thread with a bounded wait and fresh authorization; cancelled/timed-out queued reads cannot later access the clipboard.
- Fix native macOS baseline offset interpretation and reject subpixel-clipped invisible suggestions before acceptance.

- Add a native KWin input-method-v1 client over the compositor-provided private socket, prepared before Qt starts. Keep ordinary X11 operation separate; do not impersonate a screen reader or use raw-device/global input injection.
- Retain compositor-positioned brand and keyboard-navigable manual-menu surfaces with reusable buffers. Remove the new native preedit ABI after real KWin/Qt selection-loss and missing-style failures; Wayland ordinary completion is explicitly unavailable, not a popup fallback.
- Corroborate native surrounding text with privacy-gated AT-SPI metadata and exact current text/caret before insertion. Separate stable field/context identity from changing revision tokens; retain word-by-word acceptance while rejecting stale snapshots, selections, protected fields, and changed focus.
- Confirm the resulting target text before reporting an insertion as accepted. Keep atomic replacement, selected-text synonyms, and emoji rewriting disabled on Linux.
- Add explicit per-user input-method registration and a native KDE selector link without selecting the method, altering the desktop session, or selecting/downloading a model automatically. Document the tested text-input-v3 path and unsupported/incomplete protocol contexts; published 0.3.0 installers remain unchanged.
- Bootstrap missing KWin context state asynchronously, first establishing purpose and then safe surrounding state. Bound resynchronization to those initial phases, guard each reply against context changes, and do not alter input-method selection, visibility mode, protected-field policy, or native snapshot validation.

## 0.3.1 — 2026-10-24

- Keep model-download progress and verified model availability updating even when the parent Settings window is hidden or input/menu polling is paused. The Models dialog owns its refresh timer and stops it while hidden.
- Preserve model-dropdown selection/popups by avoiding inventory rebuilds when nothing changed. Existing downloaded weights are reused; no model migration, default change, or redownload is required for this UI fix.

## 0.3.0 — 2026-10-24

- Replace the editor with a menu-first agent: native AppKit preferences/overlays on macOS, native-themed Qt tray/preferences on Windows/Linux. Remove obsolete editor and in-process UI worker APIs; omit Qt from the Mac dependency/bundle.
- Add eleven native preference panes, explicit setup/permission links, compact General settings with expandable model management, configurable physical-key shortcuts, per-app/domain rules, custom-instruction Save/Revert, battery controls, and actual accepted-writing statistics.
- Move inference into a lazy, bounded stdio worker. Release the process after idle, pause, model change, and quit; preserve newest work during a same-model cold load. Document cold-load latency instead of promising instant predictions.
- Add opt-in OS-keychain-backed AES-GCM history, bounded vocabulary personalization, explicit deletion, and optional clipboard/scoped-window OCR context. Keep default recording/context off and disclose app recording overrides.
- Add offline Unicode 17 emoji/emoticon choices, skin/gender variants, contextual selected-word synonyms and alternative continuations. Improve utility prompts using actual failing/working local-model samples; do not invent dictionary fallbacks.
- Revalidate Windows UIA focus/protection before selection replacement, respect app/domain Tab precedence without disabling non-Tab shortcuts, preserve emoji precedence over pause alternatives, and fix macOS zero-length AX range row geometry using adjacent observed glyph bounds.
- Preserve REA-authenticated native reference evidence privately where preferences could contain personal text; publish sanitized records and real synthetic native UI/typing screenshots. Extend frozen-runtime verification to the actual isolated worker protocol.
- Fix a portable-settings field/method name collision found by Windows/Linux CI; exercise the actual Qt preferences and saved settings in an isolated offscreen smoke before retrying hosted verification.
- Make the portable settings smoke quit through Inklow's real application action, retaining the no-tray close-confirmation safeguard; bound the GUI smoke step so an unexpected dialog cannot stall the packaging pipeline.
- Build Windows/Linux x86 inference libraries with an explicit AVX2 baseline instead of cached host-native instructions; exercise the actual source worker before freezing as well as the packaged worker.
- Gate native model initialization before the next blocking stdin read, confirm request freshness after model readiness, send cancellation once per request, and cleanly retry failed warmups without retaining a broken model.
- Anchor a muted Inklow menu badge to supported field bounds even before typing. Use a field-attached suggestion card when native caret bounds are unavailable, while preserving exact focus/protection/snapshot checks and inline rendering when a caret is observable.
- Request only advertised per-app Electron/Chromium accessibility support, accept authoritative focused-element metadata when its redundant focus flag is absent, and revalidate native focus/protection around context reads.
- Consolidate the development Mac installation into `/Applications/Inklow.app`, remove the owned versioned preview, preserve saved data, and show missing app-process permissions explicitly rather than implying an idle agent is ready.
- Make the floating control icon-only and place it above text, using nonoverlapping screen-edge placement. Keep menus stable across native timer ticks, suspend shortcut interception during tracking, and defer actions until fresh focus can be checked.
- Retain a fixed-size visible status/tray item while open, preserve menu-agent lifetime when preferences close, and verify the quick-menu Quit path. Document third-party menu-manager visibility policy separately from application registration.
- Support an optional existing Apple code-signing identity for executables, libraries and the final Mac bundle instead of discarding it in the final resource-sealing step.
- Add native release packaging: a per-user Windows MSI with both application EXEs, a Linux Debian package with desktop/icons integration, and an Apple Silicon macOS DMG with drag-to-Applications layout. Include complete portable archives and SHA-256 checksums; keep installed application names unversioned. Windows installer publication remains deferred pending native runtime verification.
- Collect Linux AT-SPI/GI bindings, typelibs and native libraries into the frozen payload instead of relying on developer Python packages at runtime. Derive Debian shared-library dependencies from the actual ELF payload.
- Gate automatic release publication on all three native builds, real local inference, installer launch/removal and external-user-data preservation checks. Self-hosted manual publication requires an explicit target set and identifies unpublished targets instead of silently dropping failed platforms. Record the exact source revision and native receipts; publish from a draft only after the selected artifacts are present and verified.
- Rewrite the README around installation, first-run permissions/models, privacy, platform requirements, development, native builds and the versioned release workflow. Explicitly disclose unsigned/not-notarized distributions and unfinished multilingual prediction work.

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
