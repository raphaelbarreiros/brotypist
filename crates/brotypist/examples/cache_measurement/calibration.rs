//! Numerical comparisons, not semantic scores or universal floating-point tolerances.
use anyhow::{Result, ensure};
use serde_json::{Value, json};

pub const METRICS: [&str; 10] = [
    "maximum_absolute_error",
    "rms_error",
    "kl_cold_other",
    "bit_mismatch_rate",
    "argmax_disagreement",
    "top20_set_loss",
    "top20_order_disagreement",
    "greedy_text_disagreement",
    "sampler_decode_count_disagreement",
    "finish_reason_disagreement",
];

pub fn top(logits: &[f32]) -> Vec<usize> {
    let mut ids: Vec<usize> = Vec::with_capacity(logits.len().min(20));
    // Equal numerical values (including signed zero) are ordered by token ID.
    // Keep only the selected IDs rather than sorting/allocating a vocabulary.
    for id in 0..logits.len() {
        if ids.len() == 20 && logits[id] <= logits[ids[19]] {
            continue;
        }
        let position = ids
            .binary_search_by(|&existing| {
                logits[id]
                    .partial_cmp(&logits[existing])
                    .unwrap()
                    .then(existing.cmp(&id))
            })
            .unwrap_err();
        if ids.len() == 20 {
            ids.pop();
        }
        ids.insert(position, id);
    }
    ids
}

fn sum(values: impl Iterator<Item = f64>) -> f64 {
    let (mut total, mut correction) = (0.0, 0.0);
    for value in values {
        let adjusted = value - correction;
        let next = total + adjusted;
        correction = (next - total) - adjusted;
        total = next;
    }
    total
}

/// Reviewed numerical calculation, usable without inventing a text observation.
pub fn compare_logits(cold: &[f32], other: &[f32]) -> Result<Value> {
    ensure!(
        !cold.is_empty() && cold.len() == other.len(),
        "Logit vocabulary mismatch"
    );
    ensure!(
        cold.iter().chain(other).all(|v| v.is_finite()),
        "Non-finite logits"
    );
    let maximum = cold
        .iter()
        .zip(other)
        .map(|(&a, &b)| (f64::from(a) - f64::from(b)).abs())
        .fold(0.0, f64::max);
    let rms = (sum(cold
        .iter()
        .zip(other)
        .map(|(&a, &b)| (f64::from(a) - f64::from(b)).powi(2)))
        / cold.len() as f64)
        .sqrt();
    let mismatches = cold
        .iter()
        .zip(other)
        .filter(|(a, b)| a.to_bits() != b.to_bits())
        .count();
    let cold_max = cold.iter().copied().fold(f32::NEG_INFINITY, f32::max) as f64;
    let other_max = other.iter().copied().fold(f32::NEG_INFINITY, f32::max) as f64;
    // Shift before exponentiation and subtraction; no f32 softmax, clipping or epsilon.
    let cold_log_z = sum(cold.iter().map(|&x| (f64::from(x) - cold_max).exp())).ln();
    let other_log_z = sum(other.iter().map(|&x| (f64::from(x) - other_max).exp())).ln();
    let kl_raw = sum(cold.iter().zip(other).map(|(&a, &b)| {
        let log_p = f64::from(a) - cold_max - cold_log_z;
        let log_q = f64::from(b) - other_max - other_log_z;
        log_p.exp() * (log_p - log_q)
    }));
    let cold_top = top(cold);
    let other_top = top(other);
    let overlap = cold_top.iter().filter(|id| other_top.contains(id)).count();
    let order = cold_top == other_top;
    Ok(json!({
        "compared_logits":cold.len(), "bit_mismatches":mismatches,
        "bit_exact":mismatches == 0, "bit_mismatch_rate":mismatches as f64 / cold.len() as f64,
        "maximum_absolute_error":maximum, "rms_error":rms,
        "kl_cold_other":kl_raw.max(0.0), "kl_signed_f64_accumulator":kl_raw,
        "kl_rounding_policy":"Nonnegative KL report; retain signed compensated f64 accumulator; no epsilon",
        "cold_argmax":cold_top[0], "other_argmax":other_top[0],
        "argmax_disagreement":u8::from(cold_top[0] != other_top[0]),
        "cold_top20_token_ids":cold_top, "other_top20_token_ids":other_top,
        "top20_overlap":overlap, "top20_set_loss":1.0 - overlap as f64 / cold_top.len() as f64,
        "top20_order_exact":order, "top20_order_disagreement":u8::from(!order)
    }))
}

pub fn compare(cold: &[f32], other: &[f32], cold_text: &str, other_text: &str) -> Result<Value> {
    let mut result = compare_logits(cold, other)?;
    result["greedy_text_exact"] = json!(cold_text == other_text);
    result["greedy_text_disagreement"] = json!(u8::from(cold_text != other_text));
    Ok(result)
}

/// Every observed paired metric must be no worse than its fresh/fresh counterpart.
/// This is a deterministic dominance gate, not a nonsignificance test.
pub fn excess(subject: &Value, floor: &Value) -> Vec<&'static str> {
    METRICS
        .into_iter()
        .filter(|key| subject[*key].as_f64().unwrap() > floor[*key].as_f64().unwrap())
        .collect()
}

pub fn distribution(rows: &[&Value]) -> Value {
    let mut result = json!({"paired_observations":rows.len(), "metrics":{}});
    for key in METRICS {
        let mut values: Vec<_> = rows.iter().map(|row| row[key].as_f64().unwrap()).collect();
        values.sort_by(f64::total_cmp);
        if values.is_empty() {
            result["metrics"][key] = Value::Null;
            continue;
        }
        let quantile =
            |p: f64| values[((p * values.len() as f64).ceil() as usize).saturating_sub(1)];
        result["metrics"][key] = json!({
            "nonzero_count":values.iter().filter(|&&x| x != 0.0).count(),
            "nonzero_rate":values.iter().filter(|&&x| x != 0.0).count() as f64 / values.len() as f64,
            "mean":sum(values.iter().copied()) / values.len() as f64,
            "quantiles_nearest_rank":{"min":values[0], "p50":quantile(0.5), "p90":quantile(0.9), "p95":quantile(0.95), "max":values[values.len()-1]}
        });
    }
    result
}

pub fn paired_distribution(rows: &[Value], subject: &str, floor: &str) -> Value {
    let pairs: Vec<_> = rows
        .iter()
        .filter_map(|row| {
            Some((
                &row["id"],
                row["comparisons"].get(subject)?,
                row["comparisons"].get(floor)?,
            ))
        })
        .collect();
    let subjects: Vec<_> = pairs.iter().map(|(_, value, _)| *value).collect();
    let floors: Vec<_> = pairs.iter().map(|(_, _, value)| *value).collect();
    let subject_distribution = distribution(&subjects);
    let floor_distribution = distribution(&floors);
    let mut deltas = json!({});
    let mut no_excess = !pairs.is_empty() && pairs.len() == rows.len();
    for metric in METRICS {
        if pairs.is_empty() {
            deltas[metric] = Value::Null;
            continue;
        }
        let s = &subject_distribution["metrics"][metric];
        let f = &floor_distribution["metrics"][metric];
        let rate_delta = s["nonzero_rate"].as_f64().unwrap() - f["nonzero_rate"].as_f64().unwrap();
        let mut quantile_delta = json!({});
        no_excess &= rate_delta <= 0.0;
        for quantile in ["min", "p50", "p90", "p95", "max"] {
            let delta = s["quantiles_nearest_rank"][quantile].as_f64().unwrap()
                - f["quantiles_nearest_rank"][quantile].as_f64().unwrap();
            quantile_delta[quantile] = json!(delta);
            no_excess &= delta <= 0.0;
        }
        deltas[metric] = json!({"subject_minus_fresh_nonzero_rate":rate_delta,
            "subject_minus_fresh_quantiles":quantile_delta});
    }
    json!({"subject_comparison":subject,"fresh_floor_comparison":floor,
        "paired_corpus_ids":pairs.iter().map(|(id,_,_)| *id).collect::<Vec<_>>(),
        "paired_units":pairs.len(),"required_units":rows.len(),
        "subject_distribution":subject_distribution,"fresh_distribution":floor_distribution,
        "observed_deltas":deltas,"no_observed_aggregate_excess":no_excess,
        "uncertainty":"Descriptive paired corpus differences only; no confidence or power claim. Missing pairs fail rather than being interpreted as zero excess."})
}
