> Historical record: Named third-party research references were removed during repository-history cleanup. This edited historical record is not an original authenticated receipt. Recorded build identifiers and measurements describe their original observations.

# Inklow

Local-first predictive typing for macOS, Windows, and Linux. An independent implementation of a [redacted reference]-like menu-agent experience—not copied proprietary code, models, or assets.

![Native Inklow preferences](evidence/menu-first-general.png)

## What changed in 0.3

**No editor.** Inklow lives in the menu bar/system tray and suggests directly in supported applications. macOS uses native AppKit/PyObjC controls and nonactivating caret overlays; no Qt runtime is installed or bundled on macOS. Windows/Linux use native-themed Qt tray/settings surfaces with the same headless controller.

Preferences have Setup, General, Context, Personalization, Emoji, Shortcuts, Battery, App Settings, Labs, Statistics, and About panes. Closing preferences leaves the agent running; Quit stops hooks and its inference worker. If a tray cannot be created, preferences remain accessible as a fallback.

Models load **lazily in a separate process**. The process is terminated after 60 seconds idle by default, or on pause, model change, and quit. This releases model allocations, at the cost of a cold load on the next request. There is no idle preload or editor document in the resident process. A separately managed local inference server has its own lifecycle; Inklow cannot unload that server's model.

## Run

Native build output: `dist/Inklow.app` on macOS; `dist/Inklow/Inklow.exe` on Windows; `dist/Inklow/launch-inklow` on Linux. Native bundles include the runtime and original app/tray icons, **not weights or user data**. Existing saved preferences are preserved. A running older editor must be quit normally, with unsaved writing saved, before replacing its installed bundle.

On the development Mac, the current app is **`/Applications/Inklow.app`**. The older editor bundle was replaced after it was no longer running; the version-suffixed preview bundle was removed. Saved settings, downloaded models, and local history were retained. macOS permissions belong to the launched application: a successful terminal-launched smoke can inherit the terminal's permission and does not prove a normal Finder/LaunchServices launch is authorized.

Python 3.11+ is required; Python 3.12 is recommended. With [uv](https://docs.astral.sh/uv/):

```sh
git clone git@github.com:renan-brasilio/inklow.git
cd inklow
uv sync --python 3.12 --extra local
uv run inklow model download
uv run inklow --settings
```

For ordinary Python, create/activate a virtual environment, then `python -m pip install -e ".[local]"`. Windows PowerShell uses the same application commands. GGUF may require Xcode Command Line Tools, Visual Studio C++ Build Tools, or gcc/clang.

`model download` explicitly downloads the recommended **Qwen2.5-1.5B-Instruct Q4_K_M** writing model (1,117,320,736 bytes), validates SHA-256 against an immutable revision, and selects guided draft-prefill prompting. `model download --preset tiny` selects the 101 MB English starter. **General → Manage Models…** provides download/cancel, installed-preset selection, custom GGUF selection, CPU/automatic acceleration, and prompt format. Nothing downloads just because preferences open.

The guided model supports short multilingual draft continuations; output quality varies. It is not [redacted reference]'s model. GPU offload depends on the installed llama.cpp build (Metal on this Apple Silicon build); CPU is the portable fallback. Streaming, newest-request cancellation, and typed-through reuse avoid waiting for a full prediction unnecessarily.

Ollama (`/api/generate`, raw) and OpenAI-compatible (`/v1/completions`) local servers are also supported. Use a literal loopback URL such as `http://127.0.0.1:11434` or `http://[::1]:8080`. Remote hosts, DNS names, URL credentials, proxies, and inference redirects are refused. Guided writing is specifically a Qwen2.5 ChatML profile, not a universal chat template. Server backends do not require the `local` extra.

## Typing and menu controls

- **Tab:** accept the next word. Trailing space and punctuation are configurable.
- **Physical key above Tab:** accept the whole suggestion. macOS distinguishes ANSI and ISO geometry; an unrecognized geometry is left unassigned instead of hijacking another key. Shortcuts are editable.
- **Control + key above Tab:** request a completion immediately.
- **Escape:** dismiss and briefly pause by default; configurable app-disable/pass-through behavior. With no suggestion, normal keys pass through.
- **Option+Tab on macOS:** dismiss and pass a normal Tab to the target. OS Alt+Tab remains system-owned on Windows/Linux.
- Type `:smile` or a supported emoticon for an offline Unicode emoji chooser; numbered keys/click accept explicitly. Skin tone and gender preferences are separate.
- Select a word/short phrase for contextual synonyms, or enable alternative continuations in Labs. These use the local inference model and bounded structured results; model quality is not guaranteed.
- Menu: enable/disable globally, temporary current-app toggle, timed pause, settings panes, actual accepted-writing statistics, diagnostics/permission shortcuts, About, and Quit.
- A muted **inklow** menu badge is attached to the focused field's left edge, including empty fields; it does not require a model or a prediction. Native caret geometry enables inline ghost text. Where the provider exposes reliable field bounds but no usable caret rectangle, a field-attached suggestion card supports explicit acceptance instead of guessing a caret position.

New profiles enable global typing; password managers and terminals are excluded by default. **Enable completions by default** controls per-app inheritance, separately from the global master switch. App/domain rules can override completions, Tab acceptance, custom instructions, and recording. Temporary app toggles last for this process only. Website rules use exact native-observable hosts, never a guessed window title.

Launch-at-login is explicit and per-user, not a privileged service. Isolated `INKLOW_HOME` profiles cannot change login integration. Only Inklow-owned entries are removed when disabling it.

## Context, personalization, privacy

- Clipboard and screenshot context are **off by default**. Clipboard text is read only for eligible requests after opt-in. Screen context is macOS-only, uses ScreenCaptureKit/Vision local OCR of the eligible target's window, revalidates focus/protection before capture, and never writes images to disk. OS Screen Recording permission is required; no grant is automated.
- Custom instructions are ordinary settings text, not an encrypted secret store. Draft edits require explicit Save/Revert. Do not put credentials in instructions.
- Writing history is **off by default**. Enabling it requires AES-GCM encryption and a supported OS credential-store backend (macOS Keychain, Windows Credential Manager, Linux Secret Service/KWallet). There is no plaintext fallback. The encryption key is not stored beside the database.
- The recording switch is an inherited default, **not a kill switch for explicit app overrides**. An app set to record continues recording with the default off. Unaccepted typing requires a separate opt-in. History deletion and aggregate-statistics deletion are independent.
- Personalization uses relevant vocabulary from opted-in encrypted history; it does not train/fine-tune the model. No iCloud/cloud synchronization, account, subscription, automatic telemetry, or cloud inference is implemented.
- Aggregate local statistics count confirmed native insertions, not generated suggestions; they contain counts/application identifiers, not drafts. Statistics are independently disableable/deletable. Estimated time saved is a heuristic, not measured productivity.
- Native exclusions, secure/read-only checks, IME metadata, exact snapshot identity, and focus/caret revalidation guard capture and insertion. Target providers still determine what protection/composition metadata is observable. No accessibility API provides a cross-app atomic focus transaction.
- Model downloads are the only built-in public network operation and require explicit action. A local HTTP inference server remains trusted software: its own logging/network behavior is outside Inklow's control. OS swap/crash collection is also outside these guarantees.

## Platform boundaries

| Platform | Settings/agent | Global integration and limits |
| --- | --- | --- |
| macOS | Native AppKit, menu bar, no Qt | AX capture and guarded selected-text/Quartz insertion; native inline ghost/chooser; scoped-window OCR. Real isolated native interaction proof is documented below. |
| Windows | Qt tray and preferences | UIA capture; native Edit insertion and atomic replacement. Observable modern UIA controls support guarded insertion/existing-selection replacement; typed-suffix emoji replacement requires native Edit. Interactive Windows desktops remain unverified here. |
| Linux/X11 | Qt tray and preferences | AT-SPI Text/EditableText and X11 interception. Continuations/alternative insertions supported; emoji/emoticon conversion and selected-word synonyms disabled because AT-SPI lacks atomic replacement. Interactive Linux desktops remain unverified here. |
| Linux/Wayland | Preferences/tray where available | Global typing unavailable, including XWayland; no privileged interception workaround or fallback editor. |

Windows/Linux have no native browser URL observation or screenshot context in this implementation. Stored website deny rules fail closed for recognized browsers without an observable URL; website history exclusions suppress recording there. Elevated Windows targets require matching integrity privileges; Inklow does not bypass UIPI. Fields without reliable caret bounds/editability/composition metadata are skipped.

### macOS permissions

Enable Accessibility and Input Monitoring for **`/Applications/Inklow.app`** in System Settings → Privacy & Security. Terminal/Python permissions do **not** prove that the app launched normally has permission. If a stale Inklow entry points at a removed build, remove that entry and add the current app with the `+` control. Setup displays actual app-process status and opens the appropriate OS pane; no grant is automated. Restart Inklow after changing permission. The menu explicitly identifies missing required permissions. Screen Recording is optional and only needed for opted-in screen context.

### Linux prerequisites

Qt needs native runtime libraries, including in offscreen CI. Debian/Ubuntu: `libegl1 libgl1 libopengl0 libxkbcommon0`. X11 integration also needs `python3-pyatspi python3-gi at-spi2-core python3-venv` and compiler tools. Use the distribution Python to see GI bindings:

```sh
/usr/bin/python3 -m venv --system-site-packages .venv
.venv/bin/python -m pip install -e ".[local]"
.venv/bin/inklow
```

Run in the logged-in user's X11/D-Bus session with accessibility enabled. Some Qt targets require `QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1` before launch. Missing capabilities produce explicit diagnostics, not simulated integration.

## Evidence and implementation

- [REA [redacted reference] analysis, native settings observations, and limits](evidence/[redacted reference].md)
- [Sanitized process-scoped native reference evidence](evidence/[redacted reference]-menu-reference.json)
- [Observed Inklow verification and platform limits](evidence/VERIFICATION.md)
- [Passing 0.3 Windows/macOS/Ubuntu pipeline and native packages](https://github.com/renan-brasilio/inklow/actions/runs/40045550783)—source/frozen local inference and real settings event loops; not interactive Windows/Linux typing certification
- [Real native preferences interaction receipt](evidence/menu-first-native-ui.json)
- [Historical 0.2 latency/model samples](evidence/UPGRADE-0.2.md)—not a benchmark of 0.3

Shared code: `runtime.py` (headless controller), `core.py` (snapshot/acceptance), `process_client.py`/`process_worker.py` (isolated worker protocol/lifecycle), `providers.py`/`writing.py` (local inference), `storage.py` (encrypted history/aggregate statistics), `features.py` (Unicode lookup/structured utilities), `config.py`, `models.py`, and `native/`. `desktop.py` dispatches to `appkit_ui.py` or `qt_ui.py`; `system.py` owns explicit OS integration; `instance.py` prevents duplicate hooks per profile. The removed editor/window-worker APIs are not retained as compatibility shims.

## Development and packaging

```sh
uv sync --extra dev --extra local --extra build
uv run pytest -q
uv run inklow doctor
uv run python tools/smoke_desktop.py
uv build
uv run python tools/build_app.py
uv run python tools/smoke_bundle.py
```

GitHub Actions defines macOS/Windows/Ubuntu safety checks, actual source settings launch, verified Tiny acquisition, local CPU inference, wheel and native application builds, and frozen CLI plus isolated-worker inference. Workflow definitions are not proof that the current revision has passed; see the verification report for observed runs. Windows includes a console sibling `Inklow-cli.exe`; Linux includes relocatable desktop integration. Build on each target OS/architecture. macOS builds are ad-hoc signed, not notarized. `--replace` only replaces a marked Inklow build and retains rollback during publication. Bundles reject model weights, settings, databases, and encrypted-history payloads.

Windows/Linux x86 CI builds the inference backend from source with `GGML_NATIVE=OFF` and an explicit **AVX2/FMA/F16C** target instead of reusing a cached host-native wheel. These x86 artifacts require those CPU features. For another CPU/architecture, build the backend for that target before packaging; `build_app.py` bundles the installed native backend and cannot make an incompatible library portable. Upstream [build/reinstall guidance](https://github.com/abetlen/llama-cpp-python#installation) and [CPU build options](https://github.com/ggml-org/llama.cpp/blob/master/ggml/src/CMakeLists.txt).

## Attribution

Recommended model: [Qwen/Qwen2.5-1.5B-Instruct-GGUF](https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF), Apache-2.0, revision `91cad51170dc346986eccefdc2dd33a9da36ead9`, file `qwen2.5-1.5b-instruct-q4_k_m.gguf`, SHA-256 `6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e`.

Starter: [ggml-org/SmolLM2-135M-GGUF](https://huggingface.co/ggml-org/SmolLM2-135M-GGUF), Apache-2.0, revision `1076b85d135017971845b19b8d07df1479155988`, SHA-256 `e3131339bf4e8065265593d4fd8f7bb7ff2d3abff1edb5618aa1197b89cad9f5`. No weights are committed/bundled. The offline catalogue contains 3,944 fully qualified Unicode 17 emoji sequences with the Unicode License V3 embedded. Dependencies retain their respective licenses.
