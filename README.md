> Historical record: Named third-party research references were removed during repository-history cleanup. This edited historical record is not an original authenticated receipt. Recorded build identifiers and measurements describe their original observations.

# Inklow

Local-first predictive typing for macOS, Windows, and Linux. An independent implementation inspired by [redacted reference]'s documented behavior—not a port or copy of its proprietary code.

![Inklow editor with a real local-model suggestion](evidence/inklow-editor.png)

## Run

Python 3.11+ is required; Python 3.12 is recommended. The desktop uses PySide6. Local GGUF inference uses llama-cpp-python, which may need a C/C++ toolchain (Xcode Command Line Tools, Visual Studio C++ Build Tools, or gcc/clang).

With [uv](https://docs.astral.sh/uv/):

```sh
git clone git@github.com:renan-brasilio/inklow.git
cd inklow
uv sync --python 3.12 --extra local
uv run inklow model download
uv run inklow
```

`model download` explicitly downloads a 101,016,128-byte SmolLM2-135M Q4_K_M base model from an immutable Hugging Face revision, verifies its SHA-256, stores it outside the checkout, and saves its path in settings. It is an English, low-memory starter model—not a quality benchmark. Choose a larger **base/continuation GGUF model** in Settings for better suggestions or other languages. Chat-only/thinking models are generally poor raw autocompleters.

On Windows, the same `uv` commands work in PowerShell after cloning. For a standard Python installation instead of uv:

```sh
python -m venv .venv
# Activate .venv using your shell's normal command.
python -m pip install -e ".[local]"
inklow model download
inklow
```

The default GGUF backend is CPU-based for portability. A custom accelerated llama.cpp/Ollama server can be selected in Settings. Ollama uses `/api/generate` with raw continuation; the OpenAI-compatible backend uses `/v1/completions`, not chat completion. Use a literal loopback URL such as `http://127.0.0.1:11434` or `http://[::1]:8080`. DNS names, remote hosts, credentials in URLs, proxy routing, and inference redirects are refused. Server backends do not require the `local` extra.

## Use

- Pause after typing to see a prediction. Ghost text is painted, not inserted into the document.
- **Tab:** accept the next word, preserving spacing and punctuation.
- **Shift+Tab:** accept the whole suggestion. This intentionally differs from [redacted reference]'s layout-dependent key-above-Tab shortcut.
- **Escape:** dismiss and pause predictions for three seconds.
- Modified navigation keys are not intercepted globally; with no suggestion, normal keys pass through.
- Settings → **Also suggest in supported applications** enables opt-in system-wide mode. Leave Inklow running and switch to a supported writing app.
- Pause predictions or exclude an app without changing the inference backend.

The built-in writing area is a scratch surface held in memory. It is **not a document editor with autosave**: copy anything you want to keep before closing. No document history is recorded.

## Platform support and boundaries

| Platform | Built-in editor | System-wide integration | Verification |
| --- | --- | --- | --- |
| macOS | Qt editor with local inference | AX text/caret capture; direct selected-text insertion or guarded Quartz Unicode input; conditional acceptance keys | Real editor smoke and full model → native NSTextView → overlay → Tab → insertion smoke exercised on Apple Silicon |
| Windows | Same Qt editor and prediction engine | UI Automation; native Edit insertion via `EM_REPLACESEL`; observable modern UIA Text/TextEdit providers via Unicode `SendInput` | Implementation present; interactive Windows verification not performed locally |
| Linux/X11 | Same Qt editor and prediction engine | AT-SPI Text/EditableText and replayable X11 key grabs | Implementation present; interactive Linux verification not performed locally |
| Linux/Wayland | Same Qt editor and prediction engine | Explicitly unavailable, including XWayland; no privileged keylogging workaround | Global-mode refusal is implemented; not a claim of system-wide Wayland support |

System-wide compatibility depends on the target app exposing editability, protection, selection, and caret bounds. Unsupported controls are skipped. Some Qt/macOS fields allow safe insertion but expose no caret bounds; they are not offered global predictions. Password/read-only fields and excluded apps are checked before reading values. macOS/Linux IME safety depends on composition metadata exposed by the target provider; hidden composition cannot be reliably detected. Windows modern controls require observable TextEditPattern composition state and may therefore be skipped. Elevated Windows apps require matching integrity privileges; Inklow does not bypass UIPI. These APIs do not provide an atomic cross-app transaction: focus can change between validation and insertion, so do not enable global predictions in sensitive apps.

### macOS permissions

Enable Accessibility and Input Monitoring for Inklow's Python/terminal host in System Settings → Privacy & Security. No permission dialogs are clicked automatically. `uv run inklow doctor` reports capability/setup information. Password managers and terminals are excluded by default. The app must continue running for global suggestions; there is no login-item installation or background service.

### Linux native prerequisites

The editor does not require AT-SPI. For system-wide X11 integration on Debian/Ubuntu, install `python3-pyatspi`, `python3-gi`, `at-spi2-core`, `python3-venv`, and your compiler toolchain. Use the **distribution Python**, since managed uv Python installations do not see distro GI bindings:

```sh
/usr/bin/python3 -m venv --system-site-packages .venv
.venv/bin/python -m pip install -e ".[local]"
.venv/bin/inklow
```

Run in the logged-in desktop user's X11/D-Bus session with accessibility enabled. Some Qt target apps need `QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1` before launch. Missing prerequisites produce explicit diagnostics rather than simulated integration.

## Privacy

- No typing history, analytics, personal training corpus, cloud inference, or clipboard replacement.
- Typed context lives in process memory and, for server backends, in the local server's memory. A local server is trusted software: Inklow cannot control that server's own logging or network behavior.
- Only settings and explicitly downloaded models are stored. OS swap/crash collection is outside this guarantee.
- Exclusions are applied in native adapters before text access. Secure fields are refused using OS accessibility metadata; correctness still depends on the target application's accessibility provider.
- Every inference result is tied to a generation and exact field/text/caret snapshot. Acceptance rechecks the current snapshot to reject stale results.
- Model downloads are the only built-in public network operation and happen only on the explicit download command.

## Evidence and architecture

- [[redacted reference] analysis and REA evidence references](evidence/[redacted reference].md)
- [Raw exported REA bundle](evidence/[redacted reference]-rea.json)
- [Observed Inklow verification and limitations](evidence/VERIFICATION.md)

Shared modules: `core.py` (snapshot/generation/acceptance), `providers.py` (GGUF and local HTTP inference), `config.py` (validated settings), `ui_worker.py` (one running inference plus newest pending), `ui_editor.py` (non-mutating painting), `desktop.py` (settings/controller), and `native/` (OS-specific capture/input). OS imports are lazy; missing native capabilities do not break the editor.

## Development

```sh
uv sync --extra dev --extra local
uv run pytest -q
uv run inklow doctor
uv build
```

GitHub Actions runs deterministic safety regressions, syntax checks, capability diagnostics, checksum-verified model acquisition, real local inference, and wheel builds on macOS, Windows, and Ubuntu. Green CI alone is **not** proof of interactive OS integration. No fake prediction backend is shipped.

## Model attribution

Starter model: [ggml-org/SmolLM2-135M-GGUF](https://huggingface.co/ggml-org/SmolLM2-135M-GGUF), based on [HuggingFaceTB/SmolLM2-135M](https://huggingface.co/HuggingFaceTB/SmolLM2-135M), Apache-2.0. GGUF revision `1076b85d135017971845b19b8d07df1479155988`; file SHA-256 `e3131339bf4e8065265593d4fd8f7bb7ff2d3abff1edb5618aa1197b89cad9f5`. Weights are not committed or bundled with Inklow. PySide6/Qt and other dependencies retain their respective licenses; this private repository does not relicense them.
