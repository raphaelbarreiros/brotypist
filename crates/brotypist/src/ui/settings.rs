//! Portable native settings share the existing three-way draft contract, not a second preference policy.
use crate::config::Config;
use anyhow::{Context, Result, ensure};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

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
        })
    }
    pub fn value(&self, field: &str) -> Option<&Value> {
        self.draft.get(field)
    }
    pub fn set(&mut self, field: &str, value: Value) -> Result<()> {
        ensure!(self.baseline.contains_key(field), "Unknown settings field.");
        self.draft.insert(field.into(), value);
        Ok(())
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
