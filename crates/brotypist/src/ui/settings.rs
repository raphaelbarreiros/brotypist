//! Portable native settings share the existing three-way draft contract, not a second preference policy.
use crate::config::Config;
use anyhow::{Context, Result, ensure};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

/// Standalone preparation has submission-relative terminal observations, not writing captures.
pub fn preparation_diagnostics(snapshot: &crate::diagnostics::DiagnosticSnapshot) -> String {
    use crate::diagnostics::TerminalReason;
    let mut outcomes = [0usize; 7];
    for preparation in &snapshot.preparations {
        outcomes[match preparation.reason {
            TerminalReason::Completed => 0,
            TerminalReason::Cancelled => 1,
            TerminalReason::Superseded => 2,
            TerminalReason::Released => 3,
            TerminalReason::Stopped => 4,
            TerminalReason::Failed => 5,
            TerminalReason::Unknown => 6,
        }] += 1;
    }
    let mut summary = format!(
        "Standalone preparation buffer: {} retained terminal observations · {} preparation records lost.\nRetained preparation outcomes: {} completed · {} cancelled · {} superseded · {} released · {} stopped · {} failed · {} unknown.\nThese bounded observations are not session totals; {} lost terminal reports may omit any work kind. No retained preparation does not mean no preparation occurred.",
        snapshot.preparations.len(),
        snapshot.lost_preparations,
        outcomes[0],
        outcomes[1],
        outcomes[2],
        outcomes[3],
        outcomes[4],
        outcomes[5],
        outcomes[6],
        snapshot.lost_reports
    );
    if let Some(preparation) = snapshot.preparations.last() {
        let duration = |value: Option<u64>| {
            value
                .map(|us| format!("{us} µs"))
                .unwrap_or_else(|| "unavailable".into())
        };
        summary.push_str(&format!(
            "\nLatest retained standalone preparation: {:?} · Cancelled: {} · Failed: {} · Native work: {}.\nPreparation stage offsets from its own submission: ready {}, proceed {}, first result {}, terminal {}. Writing capture and native draw: unavailable for standalone preparation.",
            preparation.reason,
            preparation.cancelled,
            preparation.failed,
            if preparation.metrics.is_some() {
                "reported"
            } else {
                "unavailable"
            },
            duration(preparation.ready_us),
            duration(preparation.proceed_us),
            duration(preparation.first_result_us),
            duration(preparation.terminal_us)
        ));
    } else {
        summary.push_str("\nStandalone preparation stages and native work: unavailable; no retained terminal observation.");
    }
    summary
}

/// Apply only edits relative to baseline. Concurrent untouched fields/rules survive.
pub fn merge_mapping_edits(
    current: &Map<String, Value>,
    baseline: &Map<String, Value>,
    draft: &Map<String, Value>,
) -> Map<String, Value> {
    let mut result = current.clone();
    let keys: BTreeSet<&str> = baseline
        .keys()
        .chain(draft.keys())
        .map(String::as_str)
        .collect();
    for key in keys {
        let before = baseline.get(key);
        let after = draft.get(key);
        if before == after {
            continue;
        }
        match after {
            None => {
                result.remove(key);
            }
            Some(Value::Object(after)) if before.is_some_and(Value::is_object) => {
                let empty = Map::new();
                let previous = before.and_then(Value::as_object).unwrap();
                let current = result.get(key).and_then(Value::as_object).unwrap_or(&empty);
                let merged = merge_mapping_edits(current, previous, after);
                result.insert(key.into(), Value::Object(merged));
            }
            Some(value) => {
                result.insert(key.into(), value.clone());
            }
        }
    }
    result
}

#[derive(Clone, Debug)]
pub struct SettingsDraft {
    baseline: Map<String, Value>,
    draft: Map<String, Value>,
    explicit_fields: BTreeSet<String>,
}
impl SettingsDraft {
    pub fn new(config: &Config) -> Result<Self> {
        let baseline = serde_json::to_value(config)?
            .as_object()
            .context("Settings must be an object")?
            .clone();
        Ok(Self {
            draft: baseline.clone(),
            baseline,
            explicit_fields: BTreeSet::new(),
        })
    }
    /// Preserve the original baseline so untouched concurrent edits survive commit.
    /// Only differences infer field intent; immediate actions must call `set`,
    /// including when the selected value already equals the committed value.
    pub fn from_configs(baseline: &Config, candidate: &Config) -> Result<Self> {
        let mut result = Self::new(baseline)?;
        result.draft = serde_json::to_value(candidate)?
            .as_object()
            .context("Settings must be an object")?
            .clone();
        result.explicit_fields.extend(
            result
                .draft
                .iter()
                .filter(|(key, value)| result.baseline.get(*key) != Some(*value))
                .map(|(key, _)| key.clone()),
        );
        Ok(result)
    }
    pub fn value(&self, field: &str) -> Option<&Value> {
        self.draft.get(field)
    }
    pub fn set(&mut self, field: &str, value: Value) -> Result<()> {
        ensure!(self.baseline.contains_key(field), "Unknown settings field.");
        self.draft.insert(field.into(), value);
        self.explicit_fields.insert(field.into());
        Ok(())
    }
    /// User intent survives a return to the baseline without changing merge/dirtiness semantics.
    pub fn has_intent(&self, field: &str) -> bool {
        self.explicit_fields.contains(field)
    }
    pub fn has_model_intent(&self) -> bool {
        [
            "backend",
            "model_path",
            "acceleration",
            "endpoint",
            "model",
            "prompt_style",
            "model_id",
            "battery_model",
        ]
        .iter()
        .any(|field| self.has_intent(field))
    }
    pub fn is_dirty(&self) -> bool {
        self.baseline != self.draft
    }
    pub fn apply_to(&self, current: &Config) -> Result<Config> {
        let value = serde_json::to_value(current)?;
        let current = value.as_object().context("Settings must be an object")?;
        let mut candidate: Config = serde_json::from_value(Value::Object(merge_mapping_edits(
            current,
            &self.baseline,
            &self.draft,
        )))?;
        candidate.validate()?;
        Ok(candidate)
    }
    /// Refresh external changes without losing unsaved local edits or mutating the active config.
    pub fn refresh(&mut self, current: &Config) -> Result<()> {
        let candidate = self.apply_to(current)?;
        self.baseline = serde_json::to_value(current)?.as_object().unwrap().clone();
        self.draft = serde_json::to_value(candidate)?
            .as_object()
            .unwrap()
            .clone();
        Ok(())
    }
    /// Call after confirmed save, explicit Revert or Discard, never before persistence succeeds.
    pub fn reset(&mut self, current: &Config) -> Result<()> {
        *self = Self::new(current)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn explicit_baseline_value_retains_intent_without_overwriting_concurrent_values() {
        let original = Config::default();
        let mut draft = SettingsDraft::new(&original).unwrap();
        draft
            .set("acceleration", json!(original.acceleration))
            .unwrap();
        assert!(draft.has_intent("acceleration"));
        assert!(!draft.is_dirty());
        let mut current = original.clone();
        current.acceleration = "cpu".into();
        assert_eq!(draft.apply_to(&current).unwrap().acceleration, "cpu");
        draft.refresh(&current).unwrap();
        assert!(draft.has_intent("acceleration"));
        draft.reset(&current).unwrap();
        assert!(!draft.has_intent("acceleration"));
        assert!(
            !SettingsDraft::from_configs(&original, &original)
                .unwrap()
                .has_intent("acceleration")
        );
    }
    #[test]
    fn staged_instructions_do_not_undo_concurrent_model_or_consent_changes() {
        let original = Config::default();
        let mut draft = SettingsDraft::new(&original).unwrap();
        draft
            .set(
                "custom_instructions",
                json!("Continue in Brazilian Portuguese."),
            )
            .unwrap();
        let mut current = original.clone();
        current.model_path = "/synthetic/new-model.gguf".into();
        current.global_enabled = false;
        current.record_history = true;
        let candidate = draft.apply_to(&current).unwrap();
        assert_eq!(candidate.model_path, current.model_path);
        assert!(!candidate.global_enabled);
        assert!(candidate.record_history);
        assert_eq!(
            candidate.custom_instructions,
            "Continue in Brazilian Portuguese."
        );
        assert_eq!(current.custom_instructions, original.custom_instructions);
    }
    #[test]
    fn nested_rule_edits_and_deletions_rebase_without_clobbering_other_scopes() {
        let baseline = json!({"app:editor":{"enabled":true,"record_history":false},"domain:old.test":{"enabled":false}});
        let draft = json!({"app:editor":{"enabled":false,"record_history":false}});
        let current = json!({"app:editor":{"enabled":true,"record_history":true,"tab_enabled":false},"domain:old.test":{"enabled":false},"domain:new.test":{"enabled":true}});
        let merged = merge_mapping_edits(
            current.as_object().unwrap(),
            baseline.as_object().unwrap(),
            draft.as_object().unwrap(),
        );
        assert_eq!(
            Value::Object(merged),
            json!({"app:editor":{"enabled":false,"record_history":true,"tab_enabled":false},"domain:new.test":{"enabled":true}})
        );
    }
    #[test]
    fn invalid_staged_value_does_not_mutate_or_reset_the_active_config() {
        let current = Config::default();
        let mut draft = SettingsDraft::new(&current).unwrap();
        draft.set("debounce_ms", json!(false)).unwrap();
        assert!(draft.apply_to(&current).is_err());
        assert!(draft.is_dirty());
        assert_eq!(draft.value("debounce_ms"), Some(&json!(false)));
        draft.reset(&current).unwrap();
        assert!(!draft.is_dirty());
    }
}
