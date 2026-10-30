//! Offline attachment of explicit review labels to saved synthetic writing reports.
//!
//! No inference, application configuration, model lookup, or semantic grading occurs.
//! One review identifies exact request/output content, so it may annotate repeated
//! occurrences in both direct and worker paths. Timing/mechanical summaries remain
//! separate and unchanged; only their own semantic review status is updated.
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use std::{
    fs::OpenOptions,
    io::{BufWriter, Write},
};

const QUALITY_CORPUS: &str =
    include_str!("../../brotypist/examples/benchmark_writing/writing_quality.json");
const ATTACHED: &str = "review_labels_attached_not_calibrated";
const NOT_EVALUATED: &str = "not_evaluated";
const REVIEW_LABELS: &[&str] = &[
    "unreviewed",
    "acceptable",
    "unacceptable",
    "uncertain",
    "not_applicable",
];

/// CLI entry point, deliberately independent of source-root/version preflight.
///
/// Reads the saved report once, hashes those exact bytes, validates all reviews,
/// and creates a new review artifact. Inputs are never modified. The existing
/// output parent must already exist. Existing files, directories, and final-path
/// symlinks (including dangling ones) are refused without replacement.
///
/// Output `~`/`~/...` is expanded using HOME (USERPROFILE on Windows);
/// no home/profile files are read. Unix creation requests mode 0600.
/// Windows uses inherited ACLs and explicitly reports that POSIX 0600 is not
/// enforced; no owner-only Windows ACL guarantee is made.
/// A write/I/O failure may leave the newly created output incomplete.
pub fn run(review_input: &Path, reviews: &Path, output: &Path) -> Result<()> {
    let source = fs::read(review_input)
        .with_context(|| format!("reading saved review input {}", review_input.display()))?;
    let mut report = parse_json(&source).context("invalid saved review report")?;
    let labels = fs::read(reviews)
        .with_context(|| format!("reading explicit reviews {}", reviews.display()))?;
    let labels = parse_json(&labels).context("invalid explicit reviews")?;
    attach_reviews(&mut report, &labels)?;
    report["review_source_sha256"] = Value::String(format!("{:x}", Sha256::digest(&source)));
    write_private_report(&expand_output(output)?, &report)
}

#[derive(Clone, Copy)]
enum Rows {
    Session {
        worker: bool,
        session: usize,
        case: usize,
    },
    Flat(usize),
}

impl Rows {
    fn row(self, report: &Value) -> &Value {
        match self {
            Self::Session {
                worker,
                session,
                case,
            } => &report[session_key(worker)][session]["cases"][case],
            Self::Flat(case) => &report["cases"][case],
        }
    }

    fn semantic_mut(self, report: &mut Value) -> &mut Map<String, Value> {
        let row = match self {
            Self::Session {
                worker,
                session,
                case,
            } => &mut report[session_key(worker)][session]["cases"][case],
            Self::Flat(case) => &mut report["cases"][case],
        };
        // All target rows were checked before any report mutation.
        row["semantic_evaluation"]
            .as_object_mut()
            .expect("validated semantic object")
    }
}

fn session_key(worker: bool) -> &'static str {
    if worker {
        "worker_sessions"
    } else {
        "sessions"
    }
}

fn saved_rows(report: &Value) -> Result<Vec<Rows>> {
    ensure!(report.is_object(), "Saved review report must be an object");
    let mut rows = Vec::new();
    for worker in [false, true] {
        let key = session_key(worker);
        let Some(sessions) = report.get(key) else {
            continue;
        };
        let sessions = sessions
            .as_array()
            .with_context(|| format!("{key} must be an array"))?;
        for (session_index, session) in sessions.iter().enumerate() {
            ensure!(
                session.is_object(),
                "{key}[{session_index}] must be an object"
            );
            let Some(cases) = session.get("cases") else {
                continue;
            };
            let cases = cases
                .as_array()
                .with_context(|| format!("{key} cases must be an array"))?;
            for (case_index, case) in cases.iter().enumerate() {
                ensure!(case.is_object(), "Saved case must be an object");
                rows.push(Rows::Session {
                    worker,
                    session: session_index,
                    case: case_index,
                });
            }
        }
    }
    // v3 and v4 top-level cases are authored input metadata, not additional
    // output rows, when either path has session cases. Keep the old flat fallback.
    if rows.is_empty() {
        if let Some(cases) = report.get("cases") {
            let cases = cases.as_array().context("Saved cases must be an array")?;
            for (index, case) in cases.iter().enumerate() {
                ensure!(case.is_object(), "Saved case must be an object");
                rows.push(Rows::Flat(index));
            }
        }
    }
    Ok(rows)
}

fn nonblank<'a>(object: &'a Value, key: &str, message: &str) -> Result<&'a str> {
    let value = object
        .get(key)
        .and_then(Value::as_str)
        .context(message.to_owned())?;
    // Match Python str.strip, including its four ASCII information separators.
    ensure!(
        !value
            .trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
            .is_empty(),
        "{message}"
    );
    Ok(value)
}

/// Attach labels without changing any generated text, native calls, measurements,
/// case identity, methodology, or existing unrelated report fields.
///
/// Review sample IDs must be unique strings. Repeated saved IDs are
/// intentional (identical content across rounds/models/paths), not an ambiguity:
/// every matching occurrence must declare exactly the supplied rubric dimensions.
/// Validation of all reviews and matches completes before any mutation.
pub fn attach_reviews(report: &mut Value, review_file: &Value) -> Result<()> {
    let corpus: Value =
        serde_json::from_str(QUALITY_CORPUS).context("invalid included quality corpus")?;
    let rubric = corpus["rubric"]
        .as_object()
        .context("included corpus has no rubric")?;
    let policy = corpus["review_policy"]
        .as_str()
        .context("included corpus has no review policy")?;
    let entries = review_file
        .get("reviews")
        .and_then(Value::as_array)
        .context("Reviews must be an object containing a reviews array")?;
    let mut reviews = BTreeMap::new();
    for review in entries {
        let id = review
            .get("sample_id")
            .and_then(Value::as_str)
            .context("Every review requires a string sample_id")?;
        ensure!(
            !reviews.contains_key(id),
            "Duplicate review sample_id: {id}"
        );
        nonblank(
            review,
            "reviewer",
            "Every review requires an explicit reviewer identity",
        )?;
        ensure!(
            matches!(
                review.get("reviewer_kind").and_then(Value::as_str),
                Some("human" | "assistant")
            ),
            "reviewer_kind must be human or assistant"
        );
        nonblank(review, "notes", "Every review requires a reason in notes")?;
        let labels = review.get("labels").and_then(Value::as_object).context(
            "Invalid rubric dimension or review label: labels must be a nonempty object",
        )?;
        ensure!(
            !labels.is_empty()
                && labels.iter().all(|(dimension, label)| {
                    rubric.contains_key(dimension)
                        && label
                            .as_str()
                            .is_some_and(|label| REVIEW_LABELS.contains(&label))
                }),
            "Invalid rubric dimension or review label"
        );
        reviews.insert(id, review);
    }

    let rows = saved_rows(report)?;
    for key in ["summary", "worker_summary"] {
        if let Some(summary) = report.get(key) {
            ensure!(summary.is_object(), "{key} must be an object");
        }
    }
    let mut matches = Vec::new();
    let mut known = std::collections::BTreeSet::new();
    let mut direct_attached = false;
    let mut worker_attached = false;
    for location in rows {
        let row = location.row(report);
        let Some(semantic) = row.get("semantic_evaluation") else {
            continue;
        };
        ensure!(
            semantic.is_object(),
            "Saved semantic_evaluation must be an object"
        );
        let Some(id) = semantic.get("sample_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(review) = reviews.get(id) else {
            continue;
        };
        let declared = semantic
            .get("labels")
            .and_then(Value::as_object)
            .context("Saved semantic_evaluation labels must be an object")?;
        let supplied = review["labels"].as_object().expect("validated labels");
        ensure!(
            declared.len() == supplied.len()
                && declared
                    .keys()
                    .all(|dimension| supplied.contains_key(dimension)),
            "Review must label every declared dimension, without adding dimensions (sample_id {id})"
        );
        known.insert(id);
        match location {
            Rows::Session { worker: true, .. } => worker_attached = true,
            Rows::Session { worker: false, .. } | Rows::Flat(_) => direct_attached = true,
        }
        matches.push((location, *review));
    }
    for id in reviews.keys() {
        ensure!(
            known.contains(id),
            "Review sample_id does not match a saved output: {id}"
        );
    }
    // End immutable borrows before updating report. The review values borrow only
    // review_file; no full report/native output copies are needed.
    drop(known);
    let reviewed_at = Utc::now().to_rfc3339();
    for (location, review) in matches {
        let semantic = location.semantic_mut(report);
        for key in ["reviewer", "reviewer_kind", "labels", "notes"] {
            semantic.insert(key.to_owned(), review[key].clone());
        }
        let partial = review["labels"]
            .as_object()
            .expect("validated labels")
            .values()
            .any(|value| value == "unreviewed");
        semantic.insert(
            "status".into(),
            Value::String(
                if partial {
                    "partially_reviewed"
                } else {
                    "reviewed"
                }
                .into(),
            ),
        );
        semantic.insert("reviewed_at".into(), Value::String(reviewed_at.clone()));
    }
    report["review_methodology"] = Value::String(policy.to_owned());
    report["semantic_quality_status"] = Value::String(
        if reviews.is_empty() {
            NOT_EVALUATED
        } else {
            ATTACHED
        }
        .into(),
    );
    for (key, attached) in [
        ("summary", direct_attached),
        ("worker_summary", worker_attached),
    ] {
        if let Some(summary) = report.get_mut(key) {
            summary["semantic_quality_status"] =
                Value::String(if attached { ATTACHED } else { NOT_EVALUATED }.into());
        }
    }
    Ok(())
}

fn expand_output(path: &Path) -> Result<PathBuf> {
    if let Ok(remainder) = path.strip_prefix("~") {
        #[cfg(windows)]
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
        #[cfg(not(windows))]
        let home = std::env::var_os("HOME");
        let home = home.context("HOME/USERPROFILE is required to expand the output path")?;
        return Ok(PathBuf::from(home).join(remainder));
    }
    Ok(path.to_owned())
}

fn write_private_report(path: &Path, report: &Value) -> Result<()> {
    // create_new is atomic and refuses *all* existing final-path entries,
    // including symlinks; unlike a metadata precheck, there is no replace race.
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .with_context(|| format!("creating new reviewed report {}", path.display()))?;
    #[cfg(windows)]
    eprintln!("Review output uses inherited Windows ACLs; POSIX mode 0600 is not enforced.");
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, report).context("serializing reviewed report")?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(())
}

fn parse_json(bytes: &[u8]) -> Result<Value> {
    // Match the old JSON loader's last-key-wins object semantics.
    Ok(serde_json::from_slice(bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(id: &str) -> Value {
        json!({"generated_text":" é\t\n\"Ipê\" 🦀 ",
            "native_calls":[{"raw_generated_text":"<think>raw\u{0000}\r\n", "diagnostics":{"tokens":19}}],
            "semantic_evaluation":{"sample_id":id, "status":"not_evaluated",
                "reviewer":null,"reviewer_kind":null,
                "labels":{"grammar":"unreviewed","language":"unreviewed"},
                "assembled_drafts":["Prefix é\t\n\"Ipê\" 🦀 suffix"],
                "literal_fact_checks":[{"status":"supported_literal_observed"}]}})
    }
    fn report() -> Value {
        json!({"schema_version":3,"sessions":[{"label":"candidate","round":1,"cases":[row("sample-a")]}],
            "summary":{"semantic_quality_status":"not_evaluated","latency":{"n":1,"median":12.5}},
            "mechanical_status":"fail_or_inconclusive"})
    }
    fn review(id: &str, kind: &str) -> Value {
        json!({"sample_id":id,"reviewer":" Synthetic reviewer ","reviewer_kind":kind,
            "labels":{"grammar":"uncertain","language":"acceptable"},
            "notes":" Explicit synthetic plumbing label, not calibrated evaluation. "})
    }
    fn attach(report: &mut Value, entries: Vec<Value>) -> Result<()> {
        attach_reviews(report, &json!({"reviews":entries}))
    }

    #[test]
    fn human_and_assistant_reviews_preserve_raw_evidence_and_metadata() {
        for kind in ["human", "assistant"] {
            let mut report = report();
            let original = report.clone();
            let supplied = review("sample-a", kind);
            attach(&mut report, vec![supplied.clone()]).unwrap();
            let semantic = &report["sessions"][0]["cases"][0]["semantic_evaluation"];
            assert_eq!(semantic["status"], "reviewed");
            for key in ["reviewer", "reviewer_kind", "notes", "labels"] {
                assert_eq!(semantic[key], supplied[key]);
            }
            chrono::DateTime::parse_from_rfc3339(semantic["reviewed_at"].as_str().unwrap())
                .unwrap();
            assert_eq!(report["semantic_quality_status"], ATTACHED);
            assert_eq!(report["summary"]["semantic_quality_status"], ATTACHED);
            assert!(
                report["review_methodology"]
                    .as_str()
                    .unwrap()
                    .contains("not human validation")
            );
            // Removing only the allowed attachment fields recovers the exact input
            // Value, including every raw string byte and all nested diagnostics.
            let mut recovered = report;
            let target = &mut recovered["sessions"][0]["cases"][0];
            target["semantic_evaluation"] =
                original["sessions"][0]["cases"][0]["semantic_evaluation"].clone();
            recovered
                .as_object_mut()
                .unwrap()
                .remove("review_methodology");
            recovered
                .as_object_mut()
                .unwrap()
                .remove("semantic_quality_status");
            recovered["summary"]["semantic_quality_status"] = json!(NOT_EVALUATED);
            assert_eq!(recovered, original);
        }
    }

    #[test]
    fn every_allowed_label_is_explicit_and_unreviewed_is_partial() {
        for label in REVIEW_LABELS {
            let mut report = report();
            let mut supplied = review("sample-a", "human");
            supplied["labels"]["grammar"] = json!(label);
            attach(&mut report, vec![supplied]).unwrap();
            assert_eq!(
                report["sessions"][0]["cases"][0]["semantic_evaluation"]["status"],
                if *label == "unreviewed" {
                    "partially_reviewed"
                } else {
                    "reviewed"
                }
            );
        }
    }

    #[test]
    fn rejects_unknown_duplicate_and_malformed_reviews_without_mutation() {
        let valid = review("sample-a", "human");
        let mut invalid = vec![
            vec![review("unknown-output", "human")],
            vec![valid.clone(), valid.clone()],
        ];
        for (key, bad_values) in [
            ("sample_id", vec![json!(null), json!(17), json!(" ")]),
            (
                "reviewer",
                vec![
                    json!(null),
                    json!(false),
                    json!(" \n"),
                    json!("\u{1c}\u{1f}"),
                ],
            ),
            (
                "reviewer_kind",
                vec![json!(null), json!("calibrated_judge"), json!("Human")],
            ),
            ("notes", vec![json!(null), json!(23), json!("\t")]),
            (
                "labels",
                vec![
                    json!(null),
                    json!([]),
                    json!({}),
                    json!({"grammar":"great","language":"acceptable"}),
                    json!({"grammar":1,"language":"acceptable"}),
                    json!({"grammar":true,"language":"acceptable"}),
                    json!({"grammar":"acceptable"}),
                    json!({"grammar":"acceptable","language":"acceptable","grounding":"uncertain"}),
                    json!({"grammar":"acceptable","language":"acceptable","fabricated":"acceptable"}),
                ],
            ),
        ] {
            for value in bad_values {
                let mut supplied = valid.clone();
                supplied[key] = value;
                invalid.push(vec![supplied]);
            }
        }
        for entries in invalid {
            let mut report = report();
            let original = report.clone();
            assert!(attach(&mut report, entries).is_err());
            assert_eq!(report, original);
        }
        for malformed in [
            json!(null),
            json!({}),
            json!({"reviews":null}),
            json!({"reviews":{}}),
            json!({"reviews":[null]}),
        ] {
            assert!(attach_reviews(&mut report(), &malformed).is_err());
        }
    }

    #[test]
    fn all_target_dimensions_validate_before_any_attachment() {
        let mut report = report();
        report["sessions"][0]["cases"]
            .as_array_mut()
            .unwrap()
            .push(row("sample-b"));
        let original = report.clone();
        let mut invalid = review("sample-b", "assistant");
        invalid["labels"] = json!({"grammar":"uncertain"});
        assert!(attach(&mut report, vec![review("sample-a", "human"), invalid]).is_err());
        assert_eq!(report, original);
    }

    #[test]
    fn v4_same_identity_attaches_to_both_paths_without_pooling_metrics() {
        let mut report = report();
        report["schema_version"] = json!(4);
        report["sessions"][0]["measurement_path"] = json!("direct_native");
        report["sessions"]
            .as_array_mut()
            .unwrap()
            .push(json!({"label":"baseline","round":1,"cases":[row("sample-a")]}));
        report["worker_sessions"] = json!([{"label":"candidate","measurement_path":"canonical_worker","cases":[row("sample-a")]}]);
        report["worker_summary"] =
            json!({"semantic_quality_status":"not_evaluated","latency":{"n":1,"median":99.0}});
        report["cases"] = json!([{"id":"authored-input","prefix":"metadata only"}]);
        let direct_metrics = report["summary"]["latency"].clone();
        let worker_metrics = report["worker_summary"]["latency"].clone();
        attach(&mut report, vec![review("sample-a", "assistant")]).unwrap();
        for key in ["sessions", "worker_sessions"] {
            for session in report[key].as_array().unwrap() {
                assert_eq!(
                    session["cases"][0]["semantic_evaluation"]["reviewer_kind"],
                    "assistant"
                );
            }
        }
        assert_eq!(report["summary"]["semantic_quality_status"], ATTACHED);
        assert_eq!(
            report["worker_summary"]["semantic_quality_status"],
            ATTACHED
        );
        assert_eq!(report["summary"]["latency"], direct_metrics);
        assert_eq!(report["worker_summary"]["latency"], worker_metrics);
        assert!(report["cases"][0].get("semantic_evaluation").is_none());
    }

    #[test]
    fn only_the_reviewed_path_gets_attached_status() {
        for worker in [false, true] {
            let mut report = report();
            report["worker_sessions"] = json!([{"cases":[row("worker-only")]}]);
            report["worker_summary"] = json!({"semantic_quality_status":"not_evaluated"});
            let id = if worker { "worker-only" } else { "sample-a" };
            attach(&mut report, vec![review(id, "human")]).unwrap();
            assert_eq!(
                report["summary"]["semantic_quality_status"],
                if worker { NOT_EVALUATED } else { ATTACHED }
            );
            assert_eq!(
                report["worker_summary"]["semantic_quality_status"],
                if worker { ATTACHED } else { NOT_EVALUATED }
            );
        }
    }

    #[test]
    fn flat_legacy_report_and_empty_review_file_are_supported() {
        let mut flat_report = json!({"sessions":[],"cases":[row("flat")]});
        attach(&mut flat_report, vec![review("flat", "human")]).unwrap();
        assert_eq!(
            flat_report["cases"][0]["semantic_evaluation"]["status"],
            "reviewed"
        );
        let mut report = report();
        let saved = report["sessions"].clone();
        attach(&mut report, vec![]).unwrap();
        assert_eq!(report["semantic_quality_status"], NOT_EVALUATED);
        assert_eq!(report["summary"]["semantic_quality_status"], NOT_EVALUATED);
        assert_eq!(report["sessions"], saved);
    }

    #[test]
    fn json_loader_preserves_baseline_duplicate_key_semantics_and_rejects_malformed_json() {
        assert_eq!(
            parse_json(br#"{"labels":{"grammar":"acceptable","grammar":"uncertain"}}"#).unwrap(),
            json!({"labels":{"grammar":"uncertain"}})
        );
        for bytes in [
            br#"{"reviews":[]} trailing"#.as_slice(),
            br#"{"reviews": [}"#.as_slice(),
        ] {
            assert!(parse_json(bytes).is_err());
        }
        let sample = json!({"null":null,"bool":true,"signed":-2,"unsigned":u64::MAX,"float":1.5,"unicode":"Ipê 🦀","array":[null,false,[]]});
        assert_eq!(
            parse_json(&serde_json::to_vec(&sample).unwrap()).unwrap(),
            sample
        );
    }

    #[test]
    fn cli_hashes_exact_source_bytes_and_creates_new_output() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.json");
        let labels = dir.path().join("reviews.json");
        let output = dir.path().join("reviewed.json");
        let source = format!(" \n{}\n\n", serde_json::to_string(&report()).unwrap()).into_bytes();
        let review_bytes =
            serde_json::to_vec(&json!({"reviews":[review("sample-a", "assistant")]})).unwrap();
        fs::write(&input, &source).unwrap();
        fs::write(&labels, &review_bytes).unwrap();
        run(&input, &labels, &output).unwrap();
        assert_eq!(fs::read(&input).unwrap(), source);
        assert_eq!(fs::read(&labels).unwrap(), review_bytes);
        let saved: Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
        assert_eq!(
            saved["review_source_sha256"],
            format!("{:x}", Sha256::digest(&source))
        );
        assert_eq!(
            saved["sessions"][0]["cases"][0]["generated_text"],
            report()["sessions"][0]["cases"][0]["generated_text"]
        );
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read(&output).unwrap().last(), Some(&b'\n'));
        let first = fs::read(&output).unwrap();
        assert!(run(&input, &labels, &output).is_err());
        assert_eq!(fs::read(&output).unwrap(), first);
        assert!(run(&input, &labels, &input).is_err());
        assert_eq!(fs::read(&input).unwrap(), source);
    }

    #[cfg(unix)]
    #[test]
    fn output_refuses_existing_and_dangling_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.json");
        fs::write(&target, b"unchanged").unwrap();
        let link = dir.path().join("link.json");
        symlink(&target, &link).unwrap();
        assert!(write_private_report(&link, &report()).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"unchanged");
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let missing = dir.path().join("never-created.json");
        let dangling = dir.path().join("dangling.json");
        symlink(&missing, &dangling).unwrap();
        assert!(write_private_report(&dangling, &report()).is_err());
        assert!(!missing.exists());
    }

    #[test]
    fn output_refuses_existing_directories_and_missing_parents() {
        let dir = tempfile::tempdir().unwrap();
        assert!(write_private_report(dir.path(), &report()).is_err());
        let missing_parent = dir.path().join("no-parent/report.json");
        assert!(write_private_report(&missing_parent, &report()).is_err());
        assert!(!missing_parent.parent().unwrap().exists());
    }

    #[test]
    fn invalid_reviews_never_create_an_output_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.json");
        let reviews = dir.path().join("labels.json");
        let output = dir.path().join("output.json");
        fs::write(&input, serde_json::to_vec(&report()).unwrap()).unwrap();
        fs::write(&reviews, br#"{"reviews": [}"#).unwrap();
        assert!(run(&input, &reviews, &output).is_err());
        assert!(!output.exists());
        fs::write(
            &reviews,
            serde_json::to_vec(&json!({"reviews":[review("unknown", "human")]})).unwrap(),
        )
        .unwrap();
        assert!(run(&input, &reviews, &output).is_err());
        assert!(!output.exists());
    }
}
