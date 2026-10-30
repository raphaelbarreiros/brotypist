# BroTypist — Design Specification (Direction A: "Ghost Caret")

Visual direction and interaction examples. Units are points (pt). "Light / Dark" values give both appearances. Hex values are sRGB. Current implemented behavior, privacy constraints and compatibility contracts take precedence over forward-looking examples below. References to MLX, Sparkle or an MIT license are historical proposals, not implemented dependencies or a first-party license grant.

---

## 1. Identity

### App icon
- macOS squircle (use the system icon mask via Icon Composer).
- **Background:** vertical gradient from `#30343D` (top) to `#16181D` (bottom), with a 1 px inner stroke of white at 8% opacity.
- **Caret (I-beam):** white. On a 100-unit grid: stem `x 24, y 27, w 6, h 46, r 3`; top serif `x 19, y 27, w 16, h 4, r 2`; bottom serif `x 19, y 69, w 16, h 4, r 2`.
- **Ghost words:** two pills in mint `#5BD1A5`, vertically centered. Pill 1 is `x 40, y 46, w 22, h 8, r 4` at 75% opacity; pill 2 is `x 66, y 46, w 14, h 8, r 4` at 35% opacity.
- **Dark variant:** background `#0E0F12`, inner stroke white at 12%, caret `#E9E9EC`, mint at 85% and 40%.
- **Tinted variant:** the system tints the background; caret and pills are white (pills at 70% and 35%).
- **Small sizes:** at 32 pt and below, drop the serifs and merge the two pills into one solid mint pill (see `design/icons/app-icon-small.svg`).
- **Meaning:** a cursor, then the words that come next. Don't add sparkles, robots or gradients beyond this one.

### Wordmark
- "Bro" in SF Pro Display Bold, followed by "Typist" in SF Pro Display Semibold set in the ghost color. Tracking −2.5%.
- Light background: Bro `#15171C`, Typist `#A7A9AF`. Dark background: Bro `#F2F2F3`, Typist `#6E717A`.
- **Concept:** "Typist" is the predicted completion of "Bro". Never set "Typist" in mint.
- Lockup: icon to the left at 1.2× the cap height, gap 16 pt. Clearspace equals the caret height.

### Menu-bar glyph (template image, 18×16 pt, monochrome)
| State | Drawing | VoiceOver label |
|---|---|---|
| Active | caret `x2 y2 w2.4 h12 r1.2` plus a filled pill `x7 y6.5 w9 h3 r1.5` at 50% | "BroTypist, active" |
| Paused | caret plus two pause bars `x9` and `x13`, `y5 w1.8 h6` | "BroTypist, paused until 11:41" |
| Loading | caret plus 3 dots `cx 8 / 11.5 / 15, r1.3` at 100%, 60% and 30% opacity, cycling every 0.6 s (static with Reduce Motion) | "BroTypist, loading model" |
| Off | caret at 45% plus an outlined pill at 45% | "BroTypist, predictions off" |

All glyphs are in `design/icons/`. Mark them as template images (`isTemplate = true`).

---

## 2. Color tokens

Brand colors (icon, onboarding welcome and finish, About):
| Token | Hex |
|---|---|
| brand.graphite | `#16181D` |
| brand.slate | `#30343D` |
| brand.mint | `#5BD1A5` |
| brand.mintInk | `#0B6B4C` |
| brand.ghost | `#A7A9AF` |
| brand.paper | `#F4F4F1` |

Semantic tokens (asset catalog with Any, Dark, and High Contrast variants):
| Token | Light | Dark | Use |
|---|---|---|---|
| ghost.text | `#A2A4AA` | `#6C6F77` | inline suffix (≈ tertiaryLabel) |
| ghost.textHighVis | `#6E7078` | `#9A9CA3` | when Increase Contrast is on (≥ 4.5:1) |
| status.active | `#1FA97A` | `#5BD1A5` | status dot, success icon |
| status.activeText | `#0B6B4C` | `#6FDDB2` | "Active" label |
| status.paused | `#D99A1E` | `#F0B65A` | paused square |
| status.pausedText | `#8A5A00` | `#F0B65A` | "Paused" label |
| status.loading | systemBlue | systemBlue | progress |
| status.error | systemRed | systemRed | failures (label text `#B42318` in light mode) |
| chip.background | `.regularMaterial` (popover) | same | floating chip |
| chip.border | black 12% | white 14% | chip stroke (2 pt with Increase Contrast) |

Everything else uses system colors: `labelColor`, `secondaryLabelColor`, `separatorColor`, `controlAccentColor`, `windowBackgroundColor`. Never hard-code the accent color.

## 3. Typography (system fonts only, Dynamic Type where macOS supports it)
| Role | Font |
|---|---|
| Onboarding title | SF Pro, 26 pt Bold, tracking −0.02em |
| Pane title | 15 pt Semibold |
| Row / menu / body | 13 pt Regular |
| Help text | 11–11.5 pt, secondaryLabel |
| Section header (Settings) | 11 pt Semibold, uppercase, secondaryLabel |
| Data / model IDs / shortcuts | SF Mono 11.5–12.5 pt |

## 4. Spacing, radii, materials
- Spacing scale: 4, 8, 12, 16, 20, 24, 32.
- Settings rows are at least 40 pt tall, with a 14 pt horizontal inset inside groups. Window content padding is 24. Onboarding side padding is 56.
- Radii: key cap 6, chip 8, group 10, menu panel 12. Windows use the system radius.
- Materials: menu panel `.menu`, chip `.popover` / `.regularMaterial`, settings sidebar `.sidebar`. No custom glass or blur.
- Shadows only on floating surfaces. Chip: y 6, blur 16, black 14% (40% in dark mode). Panel: y 12, blur 32, 18%.

---

## 5. Menu bar panel — `MenuBarExtra(.window)`, width 300

From top to bottom, with 6 pt panel padding:
1. **Header row** (padding 8/10): icon at 28 pt · "BroTypist" (13 Semibold) above a subtitle "Predicting in {App}" (11, secondary) · on the right, a status badge (`● Active`).
2. **Model card** (inset 4 pt, radius 8, control background, hairline border): "Model" on the left and "on-device · MLX" on the right (11, secondary). Below that, the model name in SF Mono 12.5 Medium, e.g. `Llama 3.2 1B Instruct · 4-bit`. Below that, `38 tok/s · 0.9 GB memory` (11, secondary).
3. Row "Predictions" with a switch.
4. Row "Pause for 1 hour" with the shortcut `⌥⌘P` shown on the right.
5. Row "Pause until tomorrow".
6. Row "Turn off in {App}" (adds the app to the excluded list).
7. Separator.
8. "Settings…" `⌘,`
9. "Quit BroTypist" `⌘Q`

Rows are 30 pt tall with a 6 pt radius, and the highlight uses the accent color. Shortcuts show at 55% opacity.

**Paused panel:** subtitle "Resumes at 11:41", badge `■ Paused`, then a full-width "Resume now" button in place of rows 2–6.
**Loading panel:** subtitle "Warming up model…", badge "Loading", then a 4 pt progress bar with the model ID and a percentage below it.
**Error state:** badge `▲ Needs attention`, a one-line reason, and a "Open Settings" button.

---

## 6. Onboarding — `Window(id: "onboarding")`, 760×580, hidden title bar

- **Top bar** (44 pt): traffic lights on the left; on the right, a progress indicator of 6 segments, each 28×4 with radius 2. Completed and current segments use labelColor; the rest use separator.
- **Bottom bar** (64 pt, top hairline): "Back" (secondary button, hidden on step 1) · a hint string on the right in 12 pt secondary · the primary button.
- Primary button labels: "Get started" → "Continue" ×3 → "Finish" → "Start typing".

| # | Title | Content | Primary enabled when |
|---|---|---|---|
| 1 | Welcome to Bro**Typist** (the "Typist" in ghost color) | icon at 112, tagline, then 3 cards: **Runs on this Mac** — "A small local model — no account, no cloud." · **Stays out of the way** — "Faint suggestions. Tab to take one, keep typing to ignore it." · **Free and open source** — "MIT licensed. Read every line on GitHub." Hint: "Takes about two minutes" | always |
| 2 | Everything stays on this Mac | Sub: "Predictions are generated by a model running on your Apple silicon. BroTypist makes no network requests while you type." A diagram: *Text before cursor → Local model (MLX) → Suggestion*, with a crossed-out cloud labelled "No cloud". Two columns: **What BroTypist reads** — "Up to 1,000 characters before the cursor in the field you're typing in — only while that field has focus." **What it never does** — "Store keystrokes, read password or secure fields, or send text anywhere. Diagnostics are off unless you turn them on." | always |
| 3 | Two macOS permissions | Sub: "macOS asks you to approve these in System Settings. BroTypist opens the right pane and notices when you're done." Two **PermissionCards**: **Accessibility** — "Reads the text just before your cursor and inserts a suggestion when you accept it." **Input Monitoring** — "Notices Tab and Esc only while a suggestion is showing, so the keys keep working normally everywhere else." Each card shows an "Open System Settings" button, which becomes `✓ Allowed` (activeText). Footnote about secure fields and apps that don't expose text. Hint: "Allow both to continue" | both permissions granted |
| 4 | Choose a model | Sub: "Downloaded once, then used offline. This Mac has {N} GB of memory — any of these will run well." (Adapt the sentence to the Mac's RAM.) A radio list (see the model catalog in §10), selected row with an accent border and a light accent fill. Below it: a "Download {name}" button, which becomes a progress bar plus "Downloading {name} — {pct}% · from Hugging Face, verified by checksum", which becomes "{name} is ready." Right side: link "Use a model file on disk…". Hint: "Download a model to continue" | the selected model is downloaded |
| 5 | Try it | Sub: "Type below. A faint suggestion appears after the cursor — press Tab to accept, Esc to dismiss, or just keep typing." A 17 pt **TryItField** with placeholder "Try: I was thinking we could". Key hints `⇥ Tab accept · esc dismiss` and a live status on the right ("Waiting for you to type…" / "Suggestion ready" / "Accepted 1 suggestion — that's the whole trick."). Footnote: "Also try: …" | always |
| 6 | You're all set | success check, "BroTypist lives in your menu bar. Click it to pause, switch models or open Settings." A menu-bar illustration with the glyph highlighted (2 pt accent ring). A summary group: Model · Accept / dismiss `⇥ / esc` · "Launch at login" checkbox (on). | always |

The window is reopened from About ("Show Welcome…") or when no model is installed.

---

## 7. Settings — `Settings` scene → `NavigationSplitView`, min 900×620

- **Sidebar** 210 pt, sidebar material, rows 30 pt. Each row has an icon in a 20 pt colored rounded square (radius 5) holding a white SF Symbol: General `gearshape` (gray) · AI Models `cpu` (green `#1FA97A`) · Autocomplete `text.cursor` (blue) · Languages `globe` (indigo) · Privacy `lock.shield` (graphite) · About `info.circle` (gray). At the sidebar's foot: a status dot plus "Active · {model}" or "Predictions off".
- **Detail:** 52 pt header with the pane title, then `Form(.grouped)`. Section headers are 11 pt uppercase.
- `⌘1`–`⌘6` switch panes.

**General**
- Group: Launch BroTypist at login [switch, on] · Enable predictions [switch, on; help: "Turn off to keep BroTypist running without suggesting anything."] · Toggle predictions shortcut [recorder `⌥⌘P`]
- MENU BAR: Show icon in menu bar [on] · Show active app in menu [on]
- NOTIFICATIONS: When a model finishes downloading [on] · When paused predictions resume [off]

**AI Models**
- Status card: pulsing green dot · "Local inference running" · `MLX · {chip} · {RAM} GB · {tok/s} tok/s · first token {ms} ms` · on the right, "{x} GB memory in use".
- INSTALLED: a row per model with its name (Semibold), a mono meta line (`4-bit · 0.7 GB on disk · ~1.2 GB RAM`), an "In use" pill or a "Use" button, and a `•••` menu (Reveal in Finder, Remove…).
- AVAILABLE: catalog rows with a "Download" button · link "Add a model from disk (MLX / GGUF)…".
- PERFORMANCE: Mode [Picker: Balanced / Battery saver / Fastest] · Unload model after 10 minutes idle [on] · Pause on battery below 20% [on].

**Autocomplete**
- Prediction delay [Slider 0–800 ms, step 50, default 150, value shown in mono on the right]
- Suggestion length [Segmented: Word / Phrase / Sentence; default Phrase]
- Start after [Stepper 1–12 characters; default 3]
- Aggressiveness [Slider labelled Cautious ↔ Eager; default 45]. Help: "Cautious shows suggestions only when the model is confident; Eager suggests more often and accepts lower confidence."
- SHORTCUTS: Accept suggestion `⇥ Tab` · Accept next word `⌥ →` · Dismiss `esc` · "Use → instead of Tab in code editors and terminals" [on; help: "Tab keeps its normal meaning in apps where it indents or completes."] · Floating suggestion when inline isn't available [on]

**Languages**
- Detect language automatically [on; help: "Uses the text you've typed in the current field, on-device."] · Preferred language [Picker]
- ENABLED LANGUAGES: a checkbox list with a quality label on the right (Excellent / Very good / Limited — "Limited" shown in pausedText color). Footnote: "Quality depends on the selected model. Qwen2.5 3B covers the most languages."

**Privacy**
- Info card with a shield-check icon (activeText): **All processing happens on this Mac** — "BroTypist only uses the network to download models you choose and to check for updates. Text you type is never stored or sent."
- Sensitive-field protection [on] — "Skips fields that look like card numbers, one-time codes or addresses. Password fields are always skipped."
- Two columns: EXCLUDED APPS (list with +/−; defaults: password managers, Terminal, Keychain Access) · EXCLUDED WEBSITES (host patterns with +/−; note: "Works in browsers that expose the page URL to Accessibility (Safari, Chromium-based).")
- Diagnostic logging [off] — "Timing and error events only, saved locally. Never contains typed text."

**About** (centered)
- Icon at 88 · wordmark · `Version {x} (build {n}) · Apple silicon` · "Check for Updates…" button (Sparkle).
- Group: Source (link to the GitHub repo) · License (MIT) · Contributors (View on GitHub) · Acknowledgements (MLX, model licenses…).
- Footer: "BroTypist is free. If it saves you time, support development" (link to GitHub Sponsors).

---

## 8. Text prediction — presentation

### Render tiers (decide on every caret or focus change)
| Tier | Condition | Rendering |
|---|---|---|
| 1 Inline | caret rect known, host font known, caret at end of line, suffix fits the visible field, no marked text or selection | Ghost suffix drawn in the host font and size, `ghost.text` color, baseline-aligned 1 pt after the caret, wrapping to the field width. Clip with an 18 pt fade at the field edge. |
| 2 Chip | caret rect known but tier 1 fails (unknown font, text to the right of the caret, overflow) | Floating chip; its left edge at caret x, its top at the caret rect's bottom + 6 pt |
| 3 Anchored | only the focused element's frame is known | Chip pinned under the field's bottom-left; it leads with the last typed word in secondary color ("…step") |
| 0 Silent | secure field, excluded app or host, no AX text, Secure Event Input on, full-screen game | nothing shown, nothing read |

Note: macOS offers no public API for drawing inside another app's text view. Tier 1 is an overlay positioned over the app, so check the geometry every time and fall back when it can't be trusted.

### Floating chip
- Height 28 · horizontal padding 10 · gap 8 · radius 8 · `.popover` material · border per `chip.border` · shadow per §4.
- Content: completion (13 pt labelColor, full contrast — the chip itself signals that this is a suggestion) · key cap showing the user's accept key (`⇥`, `→`, `⌥⇥`).
- Maximum 44 characters; beyond that, cut at a word boundary and add "…".
- Optional: a language tag (`FR`, 10 pt Semibold, secondary) when it differs from the preferred language; an alternatives counter `2/3` plus `⌥]` when alternatives are enabled.
- **Placement:** flips above the caret line when within 40 pt of the visible screen bottom (Dock included). Clamp 8 pt from screen edges.

### Lifecycle
1. **Typing:** nothing shows while keystrokes arrive faster than `delayMs`.
2. **Suggest:** fade in over 120 ms with a 2 pt rise (opacity only with Reduce Motion). Never show a spinner at the caret.
3. **Refine:** typed characters that match the suggestion are consumed from the ghost immediately, with no new request.
4. **Diverge:** a mismatched character hides the suggestion at once; request a new one after the delay.
5. **Accept:** Tab inserts the full suggestion; ⌥→ inserts the next word only. A single undo in the host app reverts it.
6. **Auto-hide:** on click, scroll, window move, app switch, Mission Control, selection change, or after 8 s idle (fade out 80 ms).

### Context-specific behavior
- **Chat apps:** Return sends only the text the user typed and never accepts.
- **Code editors and terminals:** the per-app profile makes `→` the accept key and leaves Tab alone.
- **Email and long-form writing:** "Sentence" length is allowed.
- **Small fields:** if the suffix overflows the field, switch to the chip.
- **Caret mid-line:** use the chip and offer a short insertion rather than a continuation.

### Window
`NSPanel`, `[.nonactivatingPanel, .borderless]`, `level = .popUpMenu`, `ignoresMouseEvents = true`, clear background, `canBecomeKey/Main = false`, `collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .transient]`. Optional: `sharingType = .none`.

---

## 9. Preferences (`@AppStorage`)
| Key | Type | Default |
|---|---|---|
| predictionsEnabled | Bool | true |
| launchAtLogin | Bool | true |
| showMenuBarIcon | Bool | true |
| showActiveAppInMenu | Bool | true |
| notifyDownloadFinished | Bool | true |
| notifyResume | Bool | false |
| activeModelID | String | "llama-3.2-1b-instruct-4bit" |
| performanceMode | enum balanced/battery/fast | balanced |
| unloadWhenIdle | Bool | true |
| pauseOnLowBattery | Bool | true |
| delayMs | Int | 150 |
| suggestionLength | enum word/phrase/sentence | phrase |
| minChars | Int | 3 |
| aggressiveness | Int 0–100 (maps to confidence threshold, 45 ≈ 0.55) | 45 |
| acceptKey / acceptWordKey / dismissKey | Shortcut | ⇥ / ⌥→ / esc |
| arrowAcceptInCodeApps | Bool | true |
| floatingFallback | Bool | true |
| autoDetectLanguage | Bool | true |
| preferredLanguage | String | system language |
| enabledLanguages | [String] | en, fr, de, pt-BR |
| sensitiveFieldProtection | Bool | true |
| excludedBundleIDs | [String] | password managers, com.apple.Terminal, com.apple.keychainaccess |
| excludedHostPatterns | [String] | [] |
| diagnosticsEnabled | Bool | false |

## 10. Model catalog (initial; verify IDs, licenses and sizes before shipping)
| Display | Tag | Download | RAM |
|---|---|---|---|
| Llama 3.2 1B Instruct, 4-bit | Recommended · fastest | ~0.7 GB | ~1.2 GB |
| Qwen2.5 3B Instruct, 4-bit | Best multilingual | ~1.8 GB | ~2.6 GB |
| Gemma 3 4B, 4-bit | Highest quality | ~2.6 GB | ~3.6 GB |

Show each model's license and require acceptance before download where the license requires it. Recommend a default based on the Mac's RAM: 8 GB → Llama 1B; 16 GB or more → any.

## 11. States
- **Loading:** "Warming up {model}…", a 4 pt progress bar, "Predictions resume automatically. Usually under 5 s."
- **Empty:** "No model installed", "BroTypist needs one local model to make predictions.", primary button "Choose a Model…".
- **Error:** `▲ Couldn't load {model}`, the reason (e.g. "Not enough free memory (needs ~3.6 GB, 2.1 GB available)."), buttons "Try Again" and "Use {smaller model}".
- **Success:** `✓ {model} is ready`, "Checksum verified · {size}. Switch to it now?", button "Use This Model".
- **Secure input:** the menu bar shows Paused with the subtitle "Secure input active".

## 12. Accessibility and keyboard
- Every control is reachable with Tab and ⇧Tab. The sidebar uses ↑↓. ⌘1–6 switch panes, ⌘W closes, Esc cancels sheets.
- The shortcut recorder rejects bare letters, Return and ⌘Q, and warns about conflicts with the frontmost app.
- Status is always conveyed by shape and text as well as color (● active, ■ paused, ◌ loading, ▲ error, ○ off).
- **Increase Contrast:** `ghost.textHighVis` and a 2 pt chip border. **Reduce Motion:** no rise and static loading dots. **Reduce Transparency:** opaque chip.
- **VoiceOver:** the menu-bar item announces its state. Suggestions are announced at low priority ("Suggestion: … Tab to accept."), throttled to one per 3 s, with a setting to turn this off.
- Hit targets in BroTypist's own windows follow macOS control sizes (regular controls).
