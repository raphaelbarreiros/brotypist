> Historical record: Named third-party research references were removed during repository-history cleanup. This edited historical record is not an original authenticated receipt. Recorded build identifiers and measurements describe their original observations.

# Inklow

Local-first predictive typing for macOS, Windows, and Linux. An independent implementation inspired by [redacted reference]'s documented behavior—not a port or copy of its proprietary code.

![Inklow editor with a streamed local suggestion](evidence/upgrade-editor.png)

## Run
On this Mac, open `/Applications/Inklow.app`. A native bundle includes the Python/Qt runtime and application icon; it does not include model weights. Existing settings are preserved. **Models… → Balanced → Use installed model** upgrades an existing Tiny installation; downloads/selections are explicit.


Python 3.11+ is required; Python 3.12 is recommended. The desktop uses PySide6. Local GGUF inference uses llama-cpp-python, which may need a C/C++ toolchain (Xcode Command Line Tools, Visual Studio C++ Build Tools, or gcc/clang).

With [uv](https://docs.astral.sh/uv/):

```sh
git clone git@github.com:renan-brasilio/inklow.git
cd inklow
uv sync --python 3.12 --extra local
uv run inklow model download
uv run inklow
```

`model download` now explicitly downloads the recommended **Qwen2.5-1.5B-Instruct Q4_K_M** writing model (1,117,320,736 bytes). It verifies SHA-256 against an immutable revision, stores weights outside the checkout, and selects guided draft-prefill prompting. `model download --preset tiny` retains the 101 MB English starter for constrained machines. **Models…** provides the same download, cancellation, installed-model selection, and custom GGUF selection in the app. The guided model handles English/Portuguese short drafts better in our samples; partial words and difficult contexts still fail. No quality guarantee.

On Windows, the same `uv` commands work in PowerShell after cloning. For a standard Python installation instead of uv:

```sh
python -m venv .venv
# Activate .venv using your shell's normal command.
python -m pip install -e ".[local]"
inklow model download
inklow
```

GGUF inference automatically uses GPU offload when the installed llama.cpp build supports it (Metal on this Apple Silicon build). Settings can force CPU; other systems remain portable with CPU inference. Models preload/warm up off the UI thread. Predictions stream completed words, cancel obsolete work, and reuse matching typed-through suggestions. New settings default to 200 ms debounce and 24 tokens; existing preferences are not overwritten. CPU-only/GPU support depends on the native llama.cpp build, not merely the operating system.

Ollama (`/api/generate`, raw) and OpenAI-compatible (`/v1/completions`) local servers remain supported. Direct continuation is appropriate for custom base models; **Guided writing** is specifically a Qwen2.5 ChatML draft-prefill profile, not a generic chat template. Use a literal loopback URL such as `http://127.0.0.1:11434` or `http://[::1]:8080`. DNS names, remote hosts, credentials in URLs, proxy routing, and inference redirects are refused. Server backends do not require the `local` extra.

## Use

- Pause after typing to see a prediction. Ghost text is painted, not inserted into the document.
- **Tab:** accept the next word, preserving spacing and punctuation.
- **Shift+Tab:** accept the whole suggestion. This intentionally differs from [redacted reference]'s layout-dependent key-above-Tab shortcut.
- **Escape:** dismiss and pause predictions for three seconds.
- Modified navigation keys are not intercepted globally; with no suggestion, normal keys pass through.
- Menu-bar/system-tray **Suggest in Other Applications**, or the Settings checkbox, enables opt-in system-wide mode. Leave Inklow running and switch to a supported writing app.
- Pause predictions or exclude an app without changing the inference backend.

The menu-bar/system-tray icon provides Show/Hide Editor, Pause/Resume, timed pauses (5/15/60 minutes), global-mode control, Models, Settings and Exclusions, Diagnostics and Permissions, About, and explicit Quit. Closing the window hides it and preserves its in-memory document when a usable tray and “Keep running” are enabled. `--no-tray` keeps a normal window; `--background` requires a usable tray and otherwise shows the window so the app remains recoverable.

**File → New/Open/Save/Save As** supports UTF-8 documents with atomic explicit saves and unsaved-change confirmation on replacement/quit. No autosave or document history is recorded. Hidden unsaved text survives only while the process runs; save important writing.

## Platform support and boundaries

| Platform | Built-in editor | System-wide integration | Verification |
| --- | --- | --- | --- |
| macOS | Qt editor with local inference | AX text/caret capture; direct selected-text insertion or guarded Quartz Unicode input; conditional acceptance keys | Real editor smoke and full model → native NSTextView → overlay → Tab → insertion smoke exercised on Apple Silicon |
| Windows | Same Qt editor and prediction engine | UI Automation; native Edit insertion via `EM_REPLACESEL`; observable modern UIA Text/TextEdit providers via Unicode `SendInput` | Hosted safety tests, real inference, offscreen desktop launch, and wheel packaging passed; interactive global integration remains unverified |
| Linux/X11 | Same Qt editor and prediction engine | AT-SPI Text/EditableText and replayable X11 key grabs | Hosted safety tests, real inference, offscreen desktop launch, and wheel packaging passed; interactive global integration remains unverified |
| Linux/Wayland | Same Qt editor and prediction engine | Explicitly unavailable, including XWayland; no privileged keylogging workaround | Global-mode refusal is implemented; not a claim of system-wide Wayland support |

System-wide compatibility depends on the target app exposing editability, protection, selection, and caret bounds. Unsupported controls are skipped. Some Qt/macOS fields allow safe insertion but expose no caret bounds; they are not offered global predictions. Password/read-only fields and excluded apps are checked before reading values. macOS/Linux IME safety depends on composition metadata exposed by the target provider; hidden composition cannot be reliably detected. Windows modern controls require observable TextEditPattern composition state and may therefore be skipped. Elevated Windows apps require matching integrity privileges; Inklow does not bypass UIPI. These APIs do not provide an atomic cross-app transaction: focus can change between validation and insertion, so do not enable global predictions in sensitive apps.

### macOS permissions

Enable Accessibility and Input Monitoring for **Inklow.app** (or the Python/terminal host when running from source) in System Settings → Privacy & Security. A source-run permission grant need not cover the newly bundled application. No permission dialogs are clicked automatically. Diagnostics reports capability/setup information. Password managers and terminals are excluded by default. There is no automatic login-item installation or background service.

### Linux native prerequisites

The editor does not require AT-SPI, but Qt needs native runtime libraries even in offscreen mode. On a minimal Debian/Ubuntu installation, install `libegl1`, `libgl1`, `libopengl0`, and `libxkbcommon0`. For system-wide X11 integration, also install `python3-pyatspi`, `python3-gi`, `at-spi2-core`, `python3-venv`, and your compiler toolchain. Use the **distribution Python**, since managed uv Python installations do not see distro GI bindings:

```sh
/usr/bin/python3 -m venv --system-site-packages .venv
.venv/bin/python -m pip install -e ".[local]"
.venv/bin/inklow
```

Run in the logged-in desktop user's X11/D-Bus session with accessibility enabled. Some Qt target apps need `QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1` before launch. Missing prerequisites produce explicit diagnostics rather than simulated integration.

## Privacy

- No typing history, analytics, personal training corpus, cloud inference, or clipboard replacement.
- Typed context lives in process memory and, for server backends, in the local server's memory. A local server is trusted software: Inklow cannot control that server's own logging or network behavior.
- Settings, explicitly downloaded models, and documents you explicitly open/save are persisted. There is no automatic document persistence. OS swap/crash collection is outside this guarantee.
- Exclusions are applied in native adapters before text access. Secure fields are refused using OS accessibility metadata; correctness still depends on the target application's accessibility provider.
- Every inference result is tied to a generation and exact field/text/caret snapshot. Acceptance rechecks the current snapshot to reject stale results.
- Model downloads are the only built-in public network operation and happen only on explicit CLI or Models-dialog download actions.

## Evidence and architecture

- [[redacted reference] analysis and REA evidence references](evidence/[redacted reference].md)
- [Raw exported REA bundle](evidence/[redacted reference]-rea.json)
- [Observed Inklow verification and limitations](evidence/VERIFICATION.md)
- [0.2 upgrade: latency, model-quality samples, resident UI and packaging evidence](evidence/UPGRADE-0.2.md)

Shared modules: `core.py` (snapshot/generation/acceptance/typed-through reuse), `providers.py` (streaming GGUF/local HTTP), `writing.py` (versioned guided prompt), `models.py` (verified acquisition), `config.py` (settings), `ui_worker.py` (preload/cancellation/newest pending), `ui_editor.py` (non-mutating painting), `desktop.py` (controller/settings), `ui_shell.py` (menus/tray/document lifecycle), `ui_models.py` (model management), `resources.py` (identity/assets), and `native/` (OS capture/input). OS imports are lazy; missing native capabilities do not break the editor.

## Development

```sh
uv sync --extra dev --extra local --extra build
uv run pytest -q
uv run inklow doctor
uv build
uv run python tools/build_app.py
```

GitHub Actions runs safety regressions, syntax checks, diagnostics, checksum-verified Tiny acquisition, real CPU inference, an offscreen desktop launch, wheel builds, and native PyInstaller application builds with frozen-runtime inference on macOS, Windows, and Ubuntu. Native archives are uploaded as workflow artifacts. Mac output is `dist/Inklow.app`; Windows is `dist/Inklow/Inklow.exe` with console sibling `Inklow-cli.exe`; Linux is `dist/Inklow/Inklow` with a relocatable launcher and desktop integration script. Build with the target OS/Python/llama.cpp architecture. Models/settings are never included. `--replace` only replaces a marked Inklow build and publishes with rollback; macOS final resource signatures are checked. The local Mac bundle is ad-hoc signed, not Apple-notarized. No Windows Authenticode signing is configured.

Earlier 0.1 verification: [40014680596 — three OS jobs passed](https://github.com/renan-brasilio/inklow/actions/runs/40014680596), implementation `58eea8964fc7fbf9139b1e4a19bb347e0e54b80a`; receipt [evidence/ci-run.json](evidence/ci-run.json). See the upgrade evidence for current verification. Green CI is not proof of interactive Windows/Linux system-wide integration.

## Model attribution
Recommended model: [Qwen/Qwen2.5-1.5B-Instruct-GGUF](https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF), Apache-2.0, revision `91cad51170dc346986eccefdc2dd33a9da36ead9`, file `qwen2.5-1.5b-instruct-q4_k_m.gguf`, SHA-256 `6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e`.


Starter model: [ggml-org/SmolLM2-135M-GGUF](https://huggingface.co/ggml-org/SmolLM2-135M-GGUF), based on [HuggingFaceTB/SmolLM2-135M](https://huggingface.co/HuggingFaceTB/SmolLM2-135M), Apache-2.0. GGUF revision `1076b85d135017971845b19b8d07df1479155988`; file SHA-256 `e3131339bf4e8065265593d4fd8f7bb7ff2d3abff1edb5618aa1197b89cad9f5`. Weights are not committed or bundled with Inklow. PySide6/Qt and other dependencies retain their respective licenses; this private repository does not relicense them.
