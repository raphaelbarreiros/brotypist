"""Evaluate synthetic local writing, or attach explicit review labels to saved outputs.

Generation uses benchmark_writing's real production provider, private temporary
profile and exclusive mode-0600 artifact. No user settings/documents are read.
Review is offline: --review-input REPORT --reviews LABELS --output NEW_REPORT.
Labels contain reviews with sample_id, reviewer, reviewer_kind (human/assistant),
labels by rubric dimension, and notes. No automatic semantic quality score exists.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re

QUALITY_CORPUS = Path(__file__).with_name("writing_quality.json")
REVIEW_LABELS = frozenset({"unreviewed", "acceptable", "unacceptable", "uncertain", "not_applicable"})


def quality_corpus() -> dict:
    return json.loads(QUALITY_CORPUS.read_text(encoding="utf-8"))


def quality_observation(case: dict, text: str, choices: list[str] | None = None) -> dict:
    """Declare literal evidence separately from inspectable, ungraded semantics."""
    kind = case.get("kind", "completion")
    insertions = [text] if kind == "completion" else (choices or [])
    prefix, suffix = case["prefix"], case.get("suffix", "")
    # Synonyms replace selection; it must not be put back into the assembled draft.
    assembled = [prefix + insertion + suffix for insertion in insertions]
    dimensions = case.get("dimensions") or ["grammar", "language", "lexical_join"]
    if suffix and "suffix_join" not in dimensions:
        dimensions = [*dimensions, "suffix_join"]
    if (case.get("context") or case.get("reference_sources")) and "grounding" not in dimensions:
        dimensions = [*dimensions, "grounding"]
    literal_checks = []
    for index, insertion in enumerate(insertions):
        for rule in case.get("fact_checks", []):
            contradicted = any(re.search(pattern, insertion) for pattern in rule["contradiction_patterns"])
            supported = bool(re.search(rule["supported_pattern"], insertion))
            literal_checks.append({"id": rule["id"], "insertion_index": index,
                                   "status": "contradiction_observed" if contradicted else
                                   "supported_literal_observed" if supported else "not_observed",
                                   "scope": rule["scope"]})
    identity = {"case": case["id"], "prefix": prefix, "suffix": suffix,
                "selection": case.get("selection", ""), "kind": kind, "output": text,
                "context": case.get("context", ""), "reference_sources": case.get("reference_sources", []),
                "instructions": case.get("instructions", "")}
    sample_id = hashlib.sha256(json.dumps(identity, ensure_ascii=False, sort_keys=True).encode("utf-8")).hexdigest()
    return {"status": "not_evaluated", "sample_id": sample_id,
            "reviewer": None, "reviewer_kind": None,
            "labels": {dimension: "unreviewed" for dimension in dimensions},
            "criteria": list(case.get("semantic_criteria", [])), "assembled_drafts": assembled,
            "literal_fact_checks": literal_checks,
            "availability": "offered" if insertions and any(insertions) else "no_offer",
            "note": "Literal/mechanical checks are not semantic acceptance. Review exact assembled drafts; assistant labels are not calibrated human validation."}


def attach_reviews(report: dict, review_file: dict) -> dict:
    """Attach labels only to matching output identities, never mutate raw outputs."""
    rubric = quality_corpus()["rubric"]
    reviews = {}
    for review in review_file["reviews"]:
        identity = review["sample_id"]
        if identity in reviews:
            raise ValueError("Duplicate review sample_id")
        if not isinstance(review.get("reviewer"), str) or not review["reviewer"].strip():
            raise ValueError("Every review requires an explicit reviewer identity")
        if review.get("reviewer_kind") not in {"human", "assistant"}:
            raise ValueError("reviewer_kind must be human or assistant")
        labels = review.get("labels")
        if not isinstance(labels, dict) or not labels or any(
                dimension not in rubric or label not in REVIEW_LABELS for dimension, label in labels.items()):
            raise ValueError("Invalid rubric dimension or review label")
        if not isinstance(review.get("notes"), str) or not review["notes"].strip():
            raise ValueError("Every review requires a reason in notes")
        reviews[identity] = review
    rows = [row for session in report.get("sessions", []) for row in session.get("cases", [])]
    if not rows:
        rows = report.get("cases", [])
    known = {row.get("semantic_evaluation", {}).get("sample_id") for row in rows}
    if set(reviews) - known:
        raise ValueError("Review sample_id does not match a saved output")
    for row in rows:
        semantic = row.get("semantic_evaluation", {})
        review = reviews.get(semantic.get("sample_id"))
        if review is None:
            continue
        if set(review["labels"]) != set(semantic["labels"]):
            raise ValueError("Review must label every declared dimension, without adding dimensions")
        semantic.update({"reviewer": review["reviewer"], "reviewer_kind": review["reviewer_kind"],
                         "labels": review["labels"], "notes": review["notes"],
                         "status": "partially_reviewed" if "unreviewed" in review["labels"].values() else "reviewed",
                         "reviewed_at": datetime.now(timezone.utc).isoformat()})
    report["review_methodology"] = quality_corpus()["review_policy"]
    report["semantic_quality_status"] = "review_labels_attached_not_calibrated" if reviews else "not_evaluated"
    if "summary" in report:
        report["summary"]["semantic_quality_status"] = report["semantic_quality_status"]
    return report


def write_private_report(path: Path, report: dict) -> None:
    fd = os.open(path.expanduser(), os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as artifact:
        json.dump(report, artifact, ensure_ascii=False, indent=2)
        artifact.write("\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    model = parser.add_mutually_exclusive_group()
    model.add_argument("--preset", choices=["tiny", "balanced"], default=None)
    model.add_argument("--model", type=Path, help="Existing GGUF; does not select or download weights.")
    parser.add_argument("--acceleration", choices=["auto", "cpu"], default="auto")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--runs", type=int, default=2)
    parser.add_argument("--completion-length", choices=["word", "short", "medium", "long"], default="medium")
    parser.add_argument("--max-tokens", type=int, help="Override ordinary production token cap; utility tasks use 128.")
    parser.add_argument("--review-input", type=Path, help="Saved v3 report, reviewed offline without inference.")
    parser.add_argument("--reviews", type=Path, help="Explicit reviewer labels keyed by saved sample_id.")
    args = parser.parse_args()
    if args.review_input or args.reviews:
        if not args.review_input or not args.reviews or args.model or args.preset:
            parser.error("review mode requires --review-input and --reviews, without a model/preset")
        try:
            report = json.loads(args.review_input.read_text(encoding="utf-8"))
            labels = json.loads(args.reviews.read_text(encoding="utf-8"))
            reviewed = attach_reviews(report, labels)
            reviewed["review_source_sha256"] = hashlib.sha256(args.review_input.read_bytes()).hexdigest()
            write_private_report(args.output, reviewed)
        except (OSError, ValueError, KeyError, TypeError) as exc:
            parser.error(str(exc))
        return 0
    # Import the inference-facing harness only for generation. Review needs no
    # llama_cpp, app configuration, desktop framework or third-party evaluator.
    from benchmark_writing import main as benchmark_main
    if args.model:
        path = args.model
    else:
        from inklow.models import get_preset, is_installed, model_path
        preset = get_preset(args.preset or "balanced")
        if not is_installed(preset):
            parser.error("Model not installed; download explicitly before evaluation")
        path = model_path(preset)
    command = ["--model", str(path), "--output", str(args.output), "--repetitions", str(args.runs),
               "--acceleration", args.acceleration, "--completion-length", args.completion_length]
    if args.max_tokens is not None:
        command.extend(["--max-tokens", str(args.max_tokens)])
    return benchmark_main(command)


if __name__ == "__main__":
    raise SystemExit(main())
