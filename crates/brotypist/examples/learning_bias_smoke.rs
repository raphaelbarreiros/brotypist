//! Isolated synthetic experiment, not a production learning policy or adoption.
//! Register with required-features = ["native-smoke"]. Never opens normal profiles.
use anyhow::{Result, ensure};
use brotypist::{
    config::{Config, ScopedPreference},
    inference_native::{NativeModel, SamplerOptions, Tokenizer},
    providers::{FramingCache, GenerationOptions, guided_family, prompt_tokens},
    storage::{HistoryStore, authored_fragments, open_record, seal_record},
};
use chrono::{Duration, Utc};
use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{fs::OpenOptions, io::Write, path::PathBuf, process::ExitCode, sync::atomic::AtomicBool};
use unicode_normalization::UnicodeNormalization;

const KEY: [u8; 32] = [0x69; 32];
const MAX_PAIRS: usize = 4096;
const MAX_ROWS: usize = 200; // Current raw-row learning window, before positive filtering.
const MAX_ROW_TOKENS: usize = 256;
const SEEDS: [u32; 3] = [17, 41, 113];

#[derive(Deserialize)]
struct History {
    id: String,
    app: String,
    domain: String,
    text: String,
}
#[derive(Deserialize)]
struct Case {
    id: String,
    prefix: String,
    app: String,
    domain: String,
    criteria: Vec<String>,
}
#[derive(Deserialize)]
struct Fixtures {
    histories: Vec<History>,
    short_histories: Vec<String>,
    replacement: String,
    cases: Vec<Case>,
}

// Standard maximum-likelihood token bigrams with unigram backoff. Fixed caps,
// saturating integer counts, no vendor algorithm or recovered constants.
struct Candidate {
    pairs: Vec<(i32, i32, u32)>,
    trained_rows: usize,
    tokens: usize,
}
impl Candidate {
    fn new() -> Self {
        Self {
            pairs: Vec::with_capacity(MAX_PAIRS),
            trained_rows: 0,
            tokens: 0,
        }
    }
    fn clear(&mut self) {
        self.pairs.clear();
        self.trained_rows = 0;
        self.tokens = 0;
    }
    fn count(&mut self, previous: i32, token: i32) {
        if let Some(pair) = self
            .pairs
            .iter_mut()
            .find(|p| p.0 == previous && p.1 == token)
        {
            pair.2 = pair.2.saturating_add(1);
        } else if self.pairs.len() < MAX_PAIRS {
            self.pairs.push((previous, token, 1));
        }
    }
    fn rebuild(
        &mut self,
        model: &NativeModel,
        store: &HistoryStore,
        probe: &mut HistoryStore,
        case: &Case,
        allowed: &dyn Fn(&str, &str) -> bool,
    ) -> Result<()> {
        self.clear();
        // Unlike personalization's deliberate locked no-offer behavior, this
        // evaluation must distinguish unavailable evidence from an empty corpus.
        ensure!(store.is_unlocked(), "evaluation evidence is locked");
        let rows = store.records(None, None, MAX_ROWS)?;
        if !allowed(&case.app, &case.domain) {
            return Ok(());
        }
        let cutoff = Utc::now() - Duration::days(30);
        for row in rows {
            let same_scope = if case.domain.is_empty() {
                row.domain.is_empty() && row.app_id == case.app
            } else {
                row.domain == case.domain
            };
            if !same_scope || !allowed(&row.app_id, &row.domain) {
                continue;
            }
            let Ok(time) = chrono::DateTime::parse_from_rfc3339(&row.created_at) else {
                continue;
            };
            if time.with_timezone(&Utc) < cutoff || time.with_timezone(&Utc) > Utc::now() {
                continue;
            }
            // HistoryRecord exposes spans but not the authenticated authorship
            // version. Verify that marker from this private fixture's real sealed
            // payload before projecting through the public eligibility oracle.
            let sealed: Vec<u8> = Connection::open(store.path())?.query_row(
                "SELECT sealed FROM history WHERE id = ?",
                [&row.id],
                |r| r.get(0),
            )?;
            let plaintext = open_record(&KEY, &row.id, &sealed)?;
            let payload: Value = serde_json::from_slice(&plaintext)?;
            if payload["authorship"].as_u64() != Some(1) {
                continue;
            }
            ensure!(payload["text"].as_str() == Some(row.text.as_str()));
            let fragments = authored_fragments(&row.text, &row.authored_spans);
            if fragments.is_empty() {
                continue;
            }
            // Use the current public feature path itself as the eligibility
            // oracle: exact authored-only size/safety/language thresholds, no
            // copied private detector or relaxed short-text collection rule.
            probe.delete(None, None)?;
            if !probe.record_identified_at(
                &row.id,
                &row.app_id,
                &row.domain,
                &row.text,
                &row.authored_spans,
                time.with_timezone(&Utc),
            )? {
                continue;
            }
            if probe
                .personalization(&case.app, &case.domain, &case.prefix, 100, allowed)?
                .is_empty()
            {
                continue;
            }
            self.trained_rows += 1;
            let mut remaining = MAX_ROW_TOKENS;
            for fragment in fragments {
                if remaining == 0 {
                    break;
                }
                let normalized: String = fragment.nfc().collect();
                let tokens = model.tokenize(&normalized, false, false)?;
                let tokens = &tokens[..tokens.len().min(remaining)];
                for (index, &token) in tokens.iter().enumerate() {
                    self.count(-1, token);
                    if index != 0 {
                        self.count(tokens[index - 1], token);
                    }
                }
                self.tokens += tokens.len();
                remaining -= tokens.len();
            }
        }
        ensure!(self.pairs.len() <= MAX_PAIRS && self.tokens <= MAX_ROWS * MAX_ROW_TOKENS);
        Ok(())
    }
    fn bias(&self, model: &NativeModel, prefix: &str) -> Result<Vec<(i32, f32)>> {
        let tokens = model.tokenize(prefix, false, false)?;
        let last = tokens.last().copied().unwrap_or(-1);
        let context = if self.pairs.iter().any(|p| p.0 == last) {
            last
        } else {
            -1
        };
        let mut ranked: Vec<_> = self
            .pairs
            .iter()
            .filter(|p| p.0 == context)
            .map(|p| (p.1, p.2))
            .collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let maximum = ranked.first().map_or(1, |p| p.1) as f32;
        // Static initial-context bias for the entire short sample; repetition
        // risk is measured, not concealed by calling this a dynamic LM.
        Ok(ranked
            .into_iter()
            .take(64)
            .map(|(token, count)| (token, 0.5 * count as f32 / maximum))
            .collect())
    }
}
fn reset(model: &mut NativeModel, candidate: &mut Candidate) -> Result<()> {
    candidate.clear();
    model.diagnostic_set_logit_bias(&[])?;
    model.diagnostic_clear_cache();
    Ok(())
}
fn prepare(
    model: &mut NativeModel,
    candidate: &mut Candidate,
    store: &HistoryStore,
    probe: &mut HistoryStore,
    case: &Case,
    allowed: &dyn Fn(&str, &str) -> bool,
) -> Result<Vec<(i32, f32)>> {
    reset(model, candidate)?;
    if let Err(error) = candidate.rebuild(model, store, probe, case, allowed) {
        reset(model, candidate)?;
        return Err(error);
    }
    candidate.bias(model, &case.prefix)
}
fn generate(
    model: &mut NativeModel,
    config: &Config,
    case: &Case,
    evidence: &str,
    bias: &[(i32, f32)],
    seed: u32,
) -> Result<(String, u64)> {
    model.diagnostic_set_logit_bias(&[])?;
    model.diagnostic_clear_cache();
    model.diagnostic_set_logit_bias(bias)?;
    generate_current(model, config, case, evidence, seed)
}
// Deliberately does not clear native state: revocation/forgetting assertions use
// this seam so an accidental reset inside generation cannot mask leaked bias.
fn generate_current(
    model: &mut NativeModel,
    config: &Config,
    case: &Case,
    evidence: &str,
    seed: u32,
) -> Result<(String, u64)> {
    let options = GenerationOptions {
        personalization: evidence.into(),
        ..Default::default()
    };
    let tokens = prompt_tokens(
        model,
        config,
        &case.prefix,
        "",
        &options,
        &mut FramingCache::default(),
        None,
    )?;
    let family = guided_family(model)?;
    let mut output = String::new();
    model.diagnostic_set_next_seed(seed)?;
    let result = model.generate(
        &tokens,
        config.max_tokens as usize,
        SamplerOptions::for_family(Some(&family), false),
        None,
        &[],
        &AtomicBool::new(false),
        &mut |chunk, _| {
            output.push_str(chunk);
            Ok(true)
        },
    );
    let work = model.work().sampled_tokens;
    result?;
    ensure!(
        model.diagnostic_last_sampler_seed() == Some(seed),
        "unmatched sampler seed"
    );
    Ok((output, work))
}
fn authored(store: &mut HistoryStore, id: &str, app: &str, domain: &str, text: &str) -> Result<()> {
    ensure!(store.record_identified_at(
        id,
        app,
        domain,
        text,
        &[(0, text.chars().count())],
        Utc::now()
    )?);
    Ok(())
}
fn raw_insert(store: &HistoryStore, id: &str, payload: Value) -> Result<()> {
    let sealed = seal_record(&KEY, id, &serde_json::to_vec(&payload)?)?;
    Connection::open(store.path())?.execute(
        "INSERT OR REPLACE INTO history VALUES (?,?)",
        params![id, sealed],
    )?;
    Ok(())
}
fn repeated_bigrams(text: &str) -> usize {
    let words: Vec<_> = text.split_whitespace().collect();
    let mut seen = std::collections::BTreeSet::new();
    words
        .windows(2)
        .filter(|pair| !seen.insert((pair[0], pair[1])))
        .count()
}
fn run(receipt: &mut Value) -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut model_path = None;
    let mut output_path = None;
    let mut raw = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model" => model_path = args.next().map(PathBuf::from),
            "--output" => output_path = args.next().map(PathBuf::from),
            "--include-synthetic-completions" => raw = true,
            _ => anyhow::bail!("unknown argument"),
        }
    }
    let model_path = model_path.ok_or_else(|| anyhow::anyhow!("explicit model required"))?;
    let output_path =
        output_path.ok_or_else(|| anyhow::anyhow!("explicit new receipt path required"))?;
    ensure!(model_path.is_file() && !output_path.exists());
    let fixtures: Fixtures = serde_json::from_str(include_str!("learning_bias_cases.json"))?;
    let temporary = tempfile::Builder::new()
        .prefix("brotypist-learning-bias-")
        .tempdir()?;
    let mut store = HistoryStore::with_path_and_key(temporary.path().join("history.sqlite3"), KEY);
    let mut probe =
        HistoryStore::with_path_and_key(temporary.path().join("eligibility.sqlite3"), KEY);
    store.unlock()?;
    probe.unlock()?;
    let mut stages = Vec::new();
    for row in &fixtures.histories {
        if row.id == "en" {
            let generated = "😀 phantomword phantomword. ";
            let text = format!("{generated}{}", row.text);
            let start = generated.chars().count();
            ensure!(store.record_identified_at(
                &row.id,
                &row.app,
                &row.domain,
                &text,
                &[(start, start + row.text.chars().count())],
                Utc::now()
            )?);
        } else {
            authored(&mut store, &row.id, &row.app, &row.domain, &row.text)?;
        }
    }
    for (index, short) in fixtures.short_histories.iter().enumerate() {
        ensure!(!store.record_identified_at(
            &format!("short-{index}"),
            "writer",
            "",
            short,
            &[(0, short.chars().count())],
            Utc::now()
        )?);
    }
    let en = &fixtures.histories[0].text;
    ensure!(store.record_identified_at("generated", "writer", "", en, &[], Utc::now())?);
    let padded = format!("{} {}", fixtures.short_histories[0], en);
    ensure!(store.record_identified_at(
        "short-authored",
        "writer",
        "",
        &padded,
        &[(0, fixtures.short_histories[0].chars().count())],
        Utc::now()
    )?);
    raw_insert(
        &store,
        "legacy",
        json!({"app":"writer","domain":"","text":en,
        "authored_spans":[[0,en.chars().count()]],"time":Utc::now().to_rfc3339()}),
    )?;
    authored(&mut store, "negative", "writer", "", en)?;
    raw_insert(
        &store,
        "negative",
        json!({"app":"writer","domain":"","text":en,
        "authorship":1,"authored_spans":[[4,2]],"time":Utc::now().to_rfc3339()}),
    )?;
    // Reopen after deliberately injected legacy/negative rows; no stale in-memory index.
    store.lock();
    store.unlock()?;
    let threshold = format!("{}{}", "a ".repeat(11), "b".repeat(58));
    ensure!(threshold.chars().count() == 80 && threshold.split_whitespace().count() == 12);
    ensure!(probe.record_identified_at(
        "minimum",
        "writer",
        "",
        &threshold,
        &[(0, 80)],
        Utc::now()
    )?);
    ensure!(!probe.record_identified_at(
        "under-scalars",
        "writer",
        "",
        &threshold[..79],
        &[(0, 79)],
        Utc::now()
    )?);
    let under_words = format!("{}{}", "a ".repeat(10), "b".repeat(60));
    ensure!(under_words.chars().count() == 80 && under_words.split_whitespace().count() == 11);
    ensure!(!probe.record_identified_at(
        "under-words",
        "writer",
        "",
        &under_words,
        &[(0, 80)],
        Utc::now()
    )?);
    // Storage admission does not imply compatibility with this English query;
    // the public language feature gate remains authoritative.
    ensure!(
        probe
            .personalization("writer", "", &fixtures.cases[0].prefix, 100, &|_, _| true)?
            .is_empty()
    );
    probe.delete(None, None)?;
    stages.push(json!({"stage":"current_collection_thresholds","short_rejected":fixtures.short_histories.len(),
        "short_authored_padded_row_stored":true,"minimum_80_scalars_12_words_checked":true,
        "language_feature_gate_checked":true,"broader_collection_proposed_only":true}));
    receipt["stage"] = json!("model_open");
    let mut model = NativeModel::open(&model_path, "auto")?;
    let config = Config {
        prompt_style: "writing".into(),
        max_tokens: 32,
        ..Default::default()
    };
    let mut candidate = Candidate::new();
    let mut results = Vec::new();
    for (index, case) in fixtures.cases.iter().enumerate() {
        receipt["stage"] = json!("matched_generation");
        ensure!(!case.criteria.is_empty());
        let bias = prepare(
            &mut model,
            &mut candidate,
            &store,
            &mut probe,
            case,
            &|_, _| true,
        )?;
        let evidence =
            store.personalization(&case.app, &case.domain, &case.prefix, 100, &|_, _| true)?;
        if matches!(case.id.as_str(), "foreign-scope" | "different-domain") {
            ensure!(bias.is_empty() && candidate.trained_rows == 0 && evidence.is_empty());
        }
        let counters = json!({"trained_rows":candidate.trained_rows,"training_tokens":candidate.tokens,
            "pairs":candidate.pairs.len(),"derived_capacity_bytes":candidate.pairs.capacity()*std::mem::size_of::<(i32,i32,u32)>(),
            "bias_tokens":bias.len(),"feature_evidence_present":!evidence.is_empty()});
        for seed in SEEDS {
            for (arm, features, biases) in [
                ("none", "", &[][..]),
                ("current_features", evidence.as_str(), &[][..]),
                ("bounded_bigram", "", bias.as_slice()),
            ] {
                let (text, sampled) = generate(&mut model, &config, case, features, biases, seed)?;
                let mut row = json!({"case_index":index,"arm":arm,"seed":seed,"sampled_tokens":sampled,
                    "output_scalars":text.chars().count(),"repeated_bigrams":repeated_bigrams(&text),
                    "semantic_review":"required_not_automatically_passed","counters":counters});
                if raw {
                    row["synthetic_completion"] = json!(text);
                }
                results.push(row);
            }
        }
    }
    receipt["stage"] = json!("revocation_and_forgetting");
    let case = &fixtures.cases[0];
    let initial_bias = prepare(
        &mut model,
        &mut candidate,
        &store,
        &mut probe,
        case,
        &|_, _| true,
    )?;
    ensure!(!initial_bias.is_empty());
    let initial_pairs = candidate.pairs.clone();
    // Generated, negative and legacy rows must be observationally irrelevant.
    let clean_path = temporary.path().join("clean.sqlite3");
    let mut clean = HistoryStore::with_path_and_key(clean_path, KEY);
    clean.unlock()?;
    for row in &fixtures.histories {
        authored(&mut clean, &row.id, &row.app, &row.domain, &row.text)?;
    }
    let clean_bias = prepare(
        &mut model,
        &mut candidate,
        &clean,
        &mut probe,
        case,
        &|_, _| true,
    )?;
    ensure!(candidate.pairs == initial_pairs && clean_bias == initial_bias);
    stages.push(
        json!({"stage":"generated_legacy_negative_and_short_authored_excluded","passed":true}),
    );
    let (baseline, _) = generate(&mut model, &config, case, "", &[], 17)?;
    let _ = generate(&mut model, &config, case, "", &initial_bias, 17)?;
    let default_policy = Config::default();
    let denied = prepare(
        &mut model,
        &mut candidate,
        &store,
        &mut probe,
        case,
        &|app, domain| {
            default_policy
                .resolve_preference(ScopedPreference::UseHistory, app, domain, false)
                .value
        },
    )?;
    ensure!(denied.is_empty() && candidate.pairs.is_empty());
    let (revoked, _) = generate_current(&mut model, &config, case, "", 17)?;
    ensure!(baseline == revoked);
    let domain_case = fixtures
        .cases
        .iter()
        .find(|c| c.id == "exact-domain")
        .unwrap();
    ensure!(
        prepare(
            &mut model,
            &mut candidate,
            &store,
            &mut probe,
            domain_case,
            &|app, _| app != "browser"
        )?
        .is_empty()
    );
    stages.push(json!({"stage":"default_denied_and_source_consent_revoked","passed":true,"model_matches_no_learning":true}));
    authored(&mut store, "en", "writer", "", &fixtures.replacement)?;
    prepare(
        &mut model,
        &mut candidate,
        &store,
        &mut probe,
        case,
        &|_, _| true,
    )?;
    ensure!(candidate.pairs != initial_pairs);
    authored(&mut clean, "en", "writer", "", &fixtures.replacement)?;
    let replaced = candidate.pairs.clone();
    prepare(
        &mut model,
        &mut candidate,
        &clean,
        &mut probe,
        case,
        &|_, _| true,
    )?;
    ensure!(candidate.pairs == replaced);
    stages.push(json!({"stage":"stable_id_replacement_matches_fresh_evidence","passed":true}));
    let replacement_bias = candidate.bias(&model, &case.prefix)?;
    let replacement_features =
        store.personalization("writer", "", &case.prefix, 100, &|_, _| true)?;
    let _ = generate(
        &mut model,
        &config,
        case,
        &replacement_features,
        &replacement_bias,
        17,
    )?;
    reset(&mut model, &mut candidate)?;
    ensure!(store.delete(Some("writer"), None)? > 0);
    let forgotten = prepare(
        &mut model,
        &mut candidate,
        &store,
        &mut probe,
        case,
        &|_, _| true,
    )?;
    ensure!(candidate.pairs.is_empty() && forgotten.is_empty());
    ensure!(
        store
            .personalization("writer", "", &case.prefix, 100, &|_, _| true)?
            .is_empty()
    );
    let (after_forget, _) = generate_current(&mut model, &config, case, "", 17)?;
    ensure!(after_forget == baseline);
    stages.push(json!({"stage":"forget_clears_derived_bias_and_native_cache_before_generation","passed":true,"model_matches_no_learning":true}));
    // Bound is the latest 200 raw rows, including negative rows, not 200 positives.
    let mut recent = HistoryStore::with_path_and_key(temporary.path().join("recent.sqlite3"), KEY);
    recent.unlock()?;
    ensure!(recent.record_identified_at(
        "old",
        "writer",
        "",
        en,
        &[(0, en.chars().count())],
        Utc::now() - Duration::days(31)
    )?);
    ensure!(
        prepare(
            &mut model,
            &mut candidate,
            &recent,
            &mut probe,
            case,
            &|_, _| true
        )?
        .is_empty()
    );
    authored(&mut recent, "positive", "writer", "", en)?;
    for n in 0..200 {
        ensure!(recent.record_identified_at(
            &format!("negative-{n}"),
            "writer",
            "",
            en,
            &[],
            Utc::now()
        )?);
    }
    ensure!(
        prepare(
            &mut model,
            &mut candidate,
            &recent,
            &mut probe,
            case,
            &|_, _| true
        )?
        .is_empty()
    );
    ensure!(
        recent
            .personalization("writer", "", &case.prefix, 100, &|_, _| true)?
            .is_empty()
    );
    stages.push(json!({"stage":"recent_raw_row_window_and_experiment_age_bound","passed":true,"raw_rows":202,"candidate_age_days":30}));
    store.lock();
    ensure!(
        prepare(
            &mut model,
            &mut candidate,
            &store,
            &mut probe,
            case,
            &|_, _| true
        )
        .is_err()
    );
    ensure!(candidate.pairs.is_empty() && !store.is_unlocked());
    store.unlock()?;
    Connection::open(store.path())?.execute("UPDATE history SET sealed = zeroblob(40)", [])?;
    ensure!(
        prepare(
            &mut model,
            &mut candidate,
            &store,
            &mut probe,
            case,
            &|_, _| true
        )
        .is_err()
    );
    ensure!(candidate.pairs.is_empty());
    stages.push(
        json!({"stage":"locked_and_tampered_evidence_errors_not_empty_success","passed":true}),
    );
    reset(&mut model, &mut candidate)?;
    drop(model);
    drop(store);
    drop(probe);
    drop(clean);
    drop(recent);
    temporary.close()?;
    *receipt = json!({"schema":"brotypist-independent-learning-bias/1","status":"completed",
        "native_sampler_comparison":true,"production_adoption":false,"semantic_acceptance":"pending_human_case_review",
        "raw_synthetic_completions_included":raw,"temporary_storage_removed":true,
        "limits":{"rows":MAX_ROWS,"tokens_per_row":MAX_ROW_TOKENS,"count_pairs":MAX_PAIRS,"bias_tokens":64,"maximum_positive_bias":0.5},
        "method":"current public prompt/features; raw native generation without provider output fencing; static initial-context bigram bias with unigram backoff",
        "stages":stages,"cases":results});
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(output_path)?;
    serde_json::to_writer_pretty(&mut output, receipt)?;
    output.write_all(b"\n")?;
    output.sync_all()?;
    Ok(())
}
fn main() -> ExitCode {
    let mut receipt =
        json!({"schema":"brotypist-independent-learning-bias/1","stage":"arguments_and_storage"});
    match run(&mut receipt) {
        Ok(()) => {
            println!("{}", json!({"status":"completed","receipt_written":true}));
            ExitCode::SUCCESS
        }
        Err(_) => {
            eprintln!(
                "{}",
                json!({"status":"failed","stage":receipt["stage"],"details_redacted":true})
            );
            ExitCode::FAILURE
        }
    }
}
