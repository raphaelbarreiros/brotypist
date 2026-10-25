"""Run synthetic writing cases through actual configured local model backends.

This is an observational sample, not a calibrated quality benchmark. It never
reads user documents or settings and never downloads weights implicitly.
"""
from __future__ import annotations

import argparse
from dataclasses import replace
import json
from pathlib import Path
from threading import Event
from time import perf_counter

from inklow.config import Config
from inklow.models import get_preset, is_installed, model_path
from inklow.providers import create_provider

CASES = (
    ("email-en", "Thank you for your", ""),
    ("reply-en", "I look forward to hearing", ""),
    ("meeting-en", "Could we schedule a meeting for", ""),
    ("handoff-en", "The report is attached. Please let me know if", ""),
    ("context-en", "We need to finish the project before Friday. The next step is to", ""),
    ("partial-en", "I would appreciate your feed", ""),
    ("suffix-en", "I look forward to", " tomorrow."),
    ("fact-en", "The capital of France is", ""),
    ("email-pt", "Muito obrigado pela sua", ""),
    ("reply-pt", "Fico no aguardo da sua", ""),
    ("meeting-pt", "Podemos marcar uma reunião para", ""),
    ("handoff-pt", "O relatório está em anexo. Por favor, me avise se", ""),
    ("context-pt", "Precisamos terminar o projeto até sexta-feira. O próximo passo é", ""),
    ("partial-pt", "Gostaria de agradecer pela aten", ""),
    ("suffix-pt", "Espero conversar com você", " amanhã."),
    ("accent-pt", "A colaboração entre as equipes é importante porque", ""),
)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--preset", choices=["tiny", "balanced"], default="balanced")
    parser.add_argument("--acceleration", choices=["auto", "cpu"], default="auto")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--runs", type=int, default=2)
    args = parser.parse_args()
    if not 1 <= args.runs <= 10:
        parser.error("runs must be between 1 and 10")
    preset = get_preset(args.preset)
    if not is_installed(preset):
        parser.error("Model not installed. Explicitly download it with brotypist model download --preset ... --no-select")
    config = replace(Config(), model_path=str(model_path(preset)), acceleration=args.acceleration, prompt_style=preset.prompt_style)
    provider = create_provider(config)
    report = {"preset": preset.id, "sha256": preset.sha256,
              "acceleration": config.acceleration, "max_tokens": config.max_tokens,
              "debounce_ms": config.debounce_ms,
              "prompt_style": config.prompt_style,
              "limitations": "Synthetic observational samples; no human-calibrated quality score. Timing excludes typing/debounce and includes neither a claim of universal latency nor a factuality guarantee.",
              "cases": []}
    try:
        started = perf_counter()
        provider.prepare()
        report["prepare_ms"] = round((perf_counter() - started) * 1000, 2)
        for run in range(args.runs):
            for case_id, prefix, suffix in CASES:
                started = perf_counter()
                first_ms = None
                text = ""
                updates = 0
                for text in provider.stream(prefix, suffix, Event()):
                    if text and first_ms is None:
                        first_ms = round((perf_counter() - started) * 1000, 2)
                    updates += 1
                row = {"case": case_id, "run": run + 1, "prefix": prefix, "suffix": suffix,
                       "suggestion": text, "first_usable_ms": first_ms,
                       "total_ms": round((perf_counter() - started) * 1000, 2),
                       "stream_updates": updates}
                report["cases"].append(row)
                print(json.dumps(row, ensure_ascii=False), flush=True)
    finally:
        provider.close()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
