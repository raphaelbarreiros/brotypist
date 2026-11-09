//! Native-control descriptions and non-visual Windows settings policy.
use crate::{config::Config, ui::settings::SettingsDraft};
use anyhow::{Result, ensure};
use serde_json::{Value, json};

#[derive(Clone, Copy)]
pub enum Kind {
    Check,
    Text,
    Multiline,
    Number,
    Choice(&'static [&'static str]),
}
#[derive(Clone, Copy)]
pub struct Field {
    pub pane: usize,
    pub key: &'static str,
    pub label: &'static str,
    pub kind: Kind,
}
macro_rules! fields { ($(($p:expr,$k:literal,$l:literal,$t:expr)),* $(,)?) => { pub const FIELDS: &[Field] = &[$(Field { pane:$p,key:$k,label:$l,kind:$t }),*]; }; }
use Kind::*;
pub const PANES: &[&str] = &[
    "General",
    "AI Models",
    "Autocomplete",
    "Languages",
    "Privacy",
    "About",
];
fields![
    (0, "global_enabled", "Enable predictions", Check),
    (0, "enabled", "Enable in eligible apps by default", Check),
    (
        0,
        "launch_at_login",
        "Launch at login (explicit Save applies registration)",
        Check
    ),
    (0, "show_accessory", "Show field accessory", Check),
    (
        0,
        "statistics_enabled",
        "Count accepted completions (aggregate only)",
        Check
    ),
    (0, "setup_completed", "Setup completed", Check),
    (
        1,
        "backend",
        "Local backend",
        Choice(&["gguf", "ollama", "openai"])
    ),
    (
        1,
        "model_path",
        "GGUF path (imports reference your original file)",
        Text
    ),
    (
        1,
        "model_id",
        "Selected catalog ID (empty for imported files)",
        Text
    ),
    (1, "endpoint", "Literal loopback server URL", Text),
    (1, "model", "Local server model name", Text),
    (1, "acceleration", "Acceleration", Choice(&["auto", "cpu"])),
    (
        1,
        "prompt_style",
        "Prompt style",
        Choice(&["continuation", "writing"])
    ),
    (1, "max_tokens", "Maximum generated tokens (1–128)", Number),
    (
        1,
        "idle_release_seconds",
        "Release idle model after seconds (0 keeps ready)",
        Number
    ),
    (
        1,
        "battery_on_demand",
        "Battery: generate only when requested",
        Check
    ),
    (
        1,
        "battery_model",
        "Battery model ID (same or any selected installed model)",
        Text
    ),
    (
        1,
        "battery_shorter",
        "Battery: prefer shorter completions",
        Check
    ),
    (
        2,
        "complete_inside_text",
        "Complete inside existing text",
        Check
    ),
    (
        2,
        "completion_length",
        "Maximum completion length",
        Choice(&["word", "short", "medium", "long"])
    ),
    (
        2,
        "debounce_ms",
        "Typing pause in milliseconds (100–5000)",
        Number
    ),
    (2, "min_chars", "Minimum typed characters (1–1000)", Number),
    (
        2,
        "trailing_space",
        "Include trailing space when accepting",
        Check
    ),
    (
        2,
        "punctuation_mode",
        "Trailing punctuation",
        Choice(&["full", "always", "never"])
    ),
    (
        2,
        "escape_behavior",
        "Escape behavior",
        Choice(&["pause", "app", "passthrough"])
    ),
    (
        2,
        "escape_pause_seconds",
        "Escape pause duration in seconds",
        Number
    ),
    (2, "emoji_enabled", "Find emoji by typing :name", Check),
    (
        2,
        "emoticons_enabled",
        "Convert emoticons (requires emoji enabled)",
        Check
    ),
    (
        2,
        "emoji_skin_tone",
        "Skin tone (0 default, 1–5 light to dark)",
        Number
    ),
    (
        2,
        "emoji_include_neutral",
        "Include neutral-tone alternatives",
        Check
    ),
    (
        2,
        "emoji_gender",
        "Emoji gender",
        Choice(&["neutral", "female", "male"])
    ),
    (2, "alternatives_enabled", "Offer alternative words", Check),
    (
        2,
        "alternatives_mode",
        "Show alternatives",
        Choice(&["shortcut", "pause"])
    ),
    (
        2,
        "alternatives_delay_ms",
        "Alternatives delay milliseconds",
        Number
    ),
    (2, "synonyms_enabled", "Offer selected-text synonyms", Check),
    (
        2,
        "synonyms_mode",
        "Show synonyms",
        Choice(&["selection", "shortcut"])
    ),
    (2, "synonyms_delay_ms", "Synonym delay milliseconds", Number),
    (
        3,
        "writing_language",
        "Writing language (auto follows your draft; e.g. en-GB, pt-BR)",
        Text
    ),
    (
        3,
        "spelling_language",
        "Regional spelling (model convention; native dictionary unavailable)",
        Text
    ),
    (
        3,
        "custom_instructions",
        "Appended language, tone and vocabulary instructions (up to 8000 characters)",
        Multiline
    ),
    (
        3,
        "typo_suppression",
        "Spelling suppression unavailable on Windows",
        Check
    ),
    (
        3,
        "typo_fixes",
        "Native dictionary corrections unavailable on Windows",
        Check
    ),
    (
        4,
        "clipboard_context",
        "Include clipboard text (independent consent)",
        Check
    ),
    (
        4,
        "screen_context",
        "Include focused-window Accessibility text",
        Check
    ),
    (
        4,
        "screenshot_context",
        "Focused-window OCR unavailable on Windows (saved preference retained)",
        Check
    ),
    (
        4,
        "screenshot_appearance",
        "Appearance sampling unavailable on Windows",
        Check
    ),
    (
        4,
        "terminal_completion",
        "Terminal completion unavailable on Windows (saved preference retained)",
        Check
    ),
    (
        4,
        "context_chars",
        "Maximum typed context (64–8000 characters)",
        Number
    ),
    (
        4,
        "record_history",
        "Learn from encrypted writing by default",
        Check
    ),
    (
        4,
        "use_history",
        "Use encrypted writing for personalization (separate consent)",
        Check
    ),
    (
        4,
        "history_retention_days",
        "Forget dated history older than days (0 keeps history)",
        Number
    ),
    (
        4,
        "record_unaccepted",
        "Learn even when no suggestion was accepted",
        Check
    ),
    (
        4,
        "personalization_strength",
        "Learning evidence selection strength (0–100; not training progress)",
        Number
    ),
    (
        4,
        "excluded_apps",
        "Hard exclusions: app identifier/name substrings (one per line)",
        Multiline
    ),
    (
        5,
        "check_updates_automatically",
        "Check signed releases automatically",
        Check
    ),
];

pub fn unavailable(key: &str) -> bool {
    matches!(
        key,
        "typo_suppression"
            | "typo_fixes"
            | "screenshot_appearance"
            | "screenshot_context"
            | "terminal_completion"
            | "improve_compatibility"
    )
}

pub fn effective_preferences(
    config: &Config,
    app: &str,
    domain: Option<&str>,
    require_domain: bool,
) -> String {
    use crate::config::ScopedPreference;
    [
        ("Record history", ScopedPreference::RecordHistory),
        ("Use history", ScopedPreference::UseHistory),
        ("Clipboard", ScopedPreference::ClipboardContext),
        ("Accessibility", ScopedPreference::ScreenContext),
        (
            "OCR (unavailable on Windows; saved preference)",
            ScopedPreference::ScreenshotContext,
        ),
        (
            "Appearance (unavailable on Windows; saved preference)",
            ScopedPreference::ScreenshotAppearance,
        ),
    ]
    .into_iter()
    .map(|(name, preference)| {
        let resolved =
            config.resolve_preference(preference, app, domain.unwrap_or(""), require_domain);
        format!(
            "{name}: {} · {:?}",
            if resolved.value {
                "Enabled"
            } else {
                "Disabled"
            },
            resolved.origin
        )
    })
    .chain([
        format!(
            "Terminal: unavailable on Windows · saved global preference: {}",
            if config.terminal_completion {
                "Enabled"
            } else {
                "Disabled"
            }
        ),
        "Compatibility: unavailable on Windows (macOS-only; saved scoped preferences retained)"
            .into(),
    ])
    .collect::<Vec<_>>()
    .join("\r\n")
}

/// Only a matching one-shot Saved receipt acknowledges this draft generation.
pub fn saved_draft(
    draft: &mut SettingsDraft,
    config: &Config,
    submitted: Option<u64>,
    current: u64,
) -> Result<bool> {
    let matching = submitted == Some(current);
    if matching {
        draft.reset(config)?;
    } else {
        draft.refresh(config)?;
    }
    Ok(matching)
}

pub fn control_value(field: Field, text: &str, checked: bool, selected: usize) -> Result<Value> {
    Ok(match field.kind {
        Check => json!(checked),
        Number => json!(text.trim().parse::<u32>()?),
        Choice(values) => json!(
            values
                .get(selected)
                .ok_or_else(|| anyhow::anyhow!("Choose a valid setting."))?
        ),
        _ if field.key == "excluded_apps" => json!(
            text.lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        ),
        _ => json!(text),
    })
}

pub const RULE_FLAGS: &[(&str, &str)] = &[
    ("enabled", "Completions"),
    ("tab_enabled", "Accept with Tab"),
    ("record_history", "Encrypted learning"),
    (
        "improve_compatibility",
        "Improve compatibility (macOS only; saved preference retained)",
    ),
    ("complete_in_code_editors", "Complete in code editors"),
    ("use_history", "Use encrypted learning"),
    (
        "clipboard_context",
        "Clipboard text (global consent required)",
    ),
    (
        "screen_context",
        "Accessibility text (global consent required)",
    ),
    (
        "screenshot_context",
        "OCR unavailable on Windows (saved preference retained)",
    ),
    ("screenshot_appearance", "Appearance unavailable on Windows"),
];
pub fn put_rule(
    draft: &mut SettingsDraft,
    previous: Option<&str>,
    kind: &str,
    identity: &str,
    values: &[usize],
    instructions: Option<&str>,
) -> Result<String> {
    ensure!(
        matches!(kind, "app" | "domain"),
        "Choose application or website."
    );
    let identity = identity.trim();
    ensure!(
        !identity.is_empty(),
        "Enter an exact application ID or hostname."
    );
    let key = format!(
        "{kind}:{}",
        if kind == "domain" {
            crate::config::normalize_domain(identity)?
        } else {
            identity.into()
        }
    );
    let mut rules = draft
        .value("app_rules")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    ensure!(
        previous == Some(key.as_str()) || !rules.contains_key(&key),
        "A rule already exists for that identifier. Edit the existing rule instead."
    );
    let mut rule = previous
        .and_then(|old| rules.get(old))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for ((name, _), value) in RULE_FLAGS.iter().zip(values) {
        if unavailable(name) {
            continue;
        }
        match value {
            0 => {
                rule.remove(*name);
            }
            1 => {
                rule.insert((*name).into(), json!(true));
            }
            2 => {
                rule.insert((*name).into(), json!(false));
            }
            _ => anyhow::bail!("Invalid inheritance choice."),
        }
    }
    if let Some(value) = instructions {
        rule.insert("custom_instructions".into(), json!(value));
    } else {
        rule.remove("custom_instructions");
    }
    if let Some(old) = previous {
        rules.remove(old);
    }
    rules.insert(key.clone(), Value::Object(rule));
    draft.set("app_rules", Value::Object(rules))?;
    Ok(key)
}
pub fn remove_rule(draft: &mut SettingsDraft, key: &str) -> Result<()> {
    let mut rules = draft
        .value("app_rules")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    rules.remove(key);
    draft.set("app_rules", Value::Object(rules))
}
#[derive(Default)]
pub struct WelcomeState {
    pub step: usize,
    pub selected: String,
    pub ready: String,
    pub access_reviewed: bool,
}
impl WelcomeState {
    pub fn select(&mut self, id: String) {
        if self.selected != id {
            self.ready.clear();
        }
        self.selected = id;
    }
    pub fn can_continue(&self) -> bool {
        match self.step {
            2 => self.access_reviewed,
            3 => !self.selected.is_empty() && self.selected == self.ready,
            _ => true,
        }
    }
}

/// Welcome inference cannot borrow external context, history or statistics consent.
pub fn preview_config(draft: &SettingsDraft, current: &Config) -> Result<Config> {
    let mut config = draft.apply_to(current)?;
    config.clipboard_context = false;
    config.screen_context = false;
    config.screenshot_context = false;
    config.screenshot_appearance = false;
    config.record_history = false;
    config.use_history = false;
    config.record_unaccepted = false;
    config.statistics_enabled = false;
    config.app_rules.clear();
    config.excluded_apps.clear();
    Ok(config)
}

pub fn cleanup_complete(controller: bool, preview: bool, updates: bool) -> bool {
    controller && preview && updates
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_config_field_has_a_real_control_or_structured_editor() {
        let value = serde_json::to_value(Config::default()).unwrap();
        for key in value.as_object().unwrap().keys() {
            assert!(
                FIELDS.iter().any(|f| f.key == key)
                    || ["shortcuts", "app_rules"].contains(&key.as_str()),
                "{key}"
            );
        }
    }
    #[test]
    fn language_and_retention_fields_preserve_values_without_native_spelling_claims() {
        let c = Config::default();
        let mut draft = SettingsDraft::new(&c).unwrap();
        for (key, value) in [
            ("writing_language", "pt-BR"),
            ("spelling_language", "en-GB"),
            ("history_retention_days", "30"),
        ] {
            let field = *FIELDS.iter().find(|field| field.key == key).unwrap();
            draft
                .set(key, control_value(field, value, false, 0).unwrap())
                .unwrap();
        }
        let candidate = draft.apply_to(&c).unwrap();
        assert_eq!(candidate.writing_language, "pt-BR");
        assert_eq!(candidate.spelling_language, "en-GB");
        assert_eq!(candidate.history_retention_days, 30);
        assert!(!unavailable("spelling_language"));
        assert!(
            unavailable("typo_fixes")
                && unavailable("typo_suppression")
                && unavailable("screenshot_appearance")
        );
    }
    #[test]
    fn expanded_scope_controls_keep_false_inheritance_and_global_context_cap() {
        let c = Config::default();
        let mut draft = SettingsDraft::new(&c).unwrap();
        let mut flags = vec![0; RULE_FLAGS.len()];
        for (key, value) in [
            ("record_history", 1),
            ("use_history", 2),
            ("clipboard_context", 1),
            ("screen_context", 2),
        ] {
            flags[RULE_FLAGS
                .iter()
                .position(|(name, _)| *name == key)
                .unwrap()] = value;
        }
        put_rule(
            &mut draft,
            None,
            "app",
            "owned.exe",
            &flags,
            Some("  café 😀\n"),
        )
        .unwrap();
        let candidate = draft.apply_to(&c).unwrap();
        assert_eq!(candidate.app_rules["app:owned.exe"]["use_history"], false);
        assert!(!candidate.app_rules["app:owned.exe"].contains_key("screenshot_context"));
        assert_eq!(
            candidate.app_rules["app:owned.exe"]["custom_instructions"],
            "  café 😀\n"
        );
        let report = effective_preferences(&candidate, "owned.exe", None, false);
        assert!(report.contains("Record history: Enabled · App"));
        assert!(report.contains("Use history: Disabled · App"));
        assert!(report.contains("Clipboard: Disabled · GlobalConsentDenied"));
        assert!(
            effective_preferences(&candidate, "owned.exe", None, true)
                .contains("Record history: Disabled · DomainUnavailable")
        );
    }
    #[test]
    fn model_and_late_saved_receipts_cannot_reset_newer_instruction_drafts() {
        let baseline = Config::default();
        let mut draft = SettingsDraft::new(&baseline).unwrap();
        draft
            .set("custom_instructions", json!("New draft: e\u{301} 😀\n"))
            .unwrap();
        let mut model_saved = baseline.clone();
        model_saved.model_path = "selected.gguf".into();
        assert!(!saved_draft(&mut draft, &model_saved, None, 2).unwrap());
        assert!(draft.is_dirty());
        let mut late_saved = model_saved.clone();
        late_saved.custom_instructions = "Older submitted draft".into();
        assert!(!saved_draft(&mut draft, &late_saved, Some(1), 2).unwrap());
        assert_eq!(
            draft.value("custom_instructions"),
            Some(&json!("New draft: e\u{301} 😀\n"))
        );
        assert_eq!(
            draft.apply_to(&late_saved).unwrap().model_path,
            "selected.gguf"
        );
        let matching = draft.apply_to(&late_saved).unwrap();
        assert!(saved_draft(&mut draft, &matching, Some(2), 2).unwrap());
        assert!(!draft.is_dirty());
    }
    #[test]
    fn unavailable_preferences_survive_rule_edit_rename_and_save() {
        let mut config = Config::default();
        config.screenshot_context = true;
        config.terminal_completion = true;
        config.app_rules.insert(
            "app:old.exe".into(),
            [
                ("screenshot_context".into(), json!(true)),
                ("screenshot_appearance".into(), json!(false)),
                ("improve_compatibility".into(), json!(true)),
            ]
            .into(),
        );
        let mut draft = SettingsDraft::new(&config).unwrap();
        let mut flags = vec![0; RULE_FLAGS.len()];
        flags[RULE_FLAGS
            .iter()
            .position(|(name, _)| *name == "complete_in_code_editors")
            .unwrap()] = 1;
        put_rule(
            &mut draft,
            Some("app:old.exe"),
            "app",
            "new.exe",
            &flags,
            None,
        )
        .unwrap();
        let saved = draft.apply_to(&config).unwrap();
        assert!(saved.screenshot_context && saved.terminal_completion);
        assert!(!saved.app_rules.contains_key("app:old.exe"));
        let rule = &saved.app_rules["app:new.exe"];
        assert_eq!(rule["screenshot_context"], true);
        assert_eq!(rule["screenshot_appearance"], false);
        assert_eq!(rule["improve_compatibility"], true);
        assert_eq!(rule["complete_in_code_editors"], true);
        for key in [
            "screenshot_context",
            "terminal_completion",
            "improve_compatibility",
        ] {
            assert!(unavailable(key));
        }
        assert!(!unavailable("complete_in_code_editors"));
    }
    #[test]
    fn colliding_add_and_rename_preserve_rules_and_dirty_edits() {
        let mut baseline = Config::default();
        baseline.app_rules.insert(
            "app:source.exe".into(),
            [("enabled".into(), json!(false))].into(),
        );
        baseline.app_rules.insert(
            "domain:example.com".into(),
            [("record_history".into(), json!(false))].into(),
        );
        let mut draft = SettingsDraft::new(&baseline).unwrap();
        draft
            .set("custom_instructions", json!("Unsaved global text"))
            .unwrap();
        put_rule(
            &mut draft,
            Some("app:source.exe"),
            "app",
            "source.exe",
            &[2, 0, 0, 0, 1],
            Some("Unsaved scoped text"),
        )
        .unwrap();
        let before = draft.value("app_rules").unwrap().clone();
        for previous in [None, Some("app:source.exe")] {
            assert!(
                put_rule(
                    &mut draft,
                    previous,
                    "domain",
                    "Example.COM.",
                    &[1, 1, 1, 0, 0],
                    Some("Must not replace destination"),
                )
                .is_err()
            );
            assert_eq!(draft.value("app_rules"), Some(&before));
            assert_eq!(
                draft.value("custom_instructions"),
                Some(&json!("Unsaved global text"))
            );
            assert!(draft.is_dirty());
        }
        let mut concurrent = baseline.clone();
        concurrent.model_path = "new.gguf".into();
        concurrent
            .app_rules
            .get_mut("app:source.exe")
            .unwrap()
            .insert("use_history".into(), json!(false));
        draft.refresh(&concurrent).unwrap();
        let merged = draft.apply_to(&concurrent).unwrap();
        assert_eq!(merged.model_path, "new.gguf");
        assert_eq!(merged.custom_instructions, "Unsaved global text");
        assert_eq!(
            merged.app_rules["domain:example.com"]["record_history"],
            false
        );
        let source = &merged.app_rules["app:source.exe"];
        assert_eq!(source["enabled"], false);
        assert_eq!(source["use_history"], false);
        assert_eq!(source["complete_in_code_editors"], true);
        assert!(!source.contains_key("tab_enabled"));
        assert_eq!(source["custom_instructions"], "Unsaved scoped text");
    }
    #[test]
    fn rules_distinguish_inheritance_false_and_empty_instructions() {
        let c = Config::default();
        let mut draft = SettingsDraft::new(&c).unwrap();
        put_rule(
            &mut draft,
            None,
            "domain",
            "Example.COM.",
            &[0, 2, 1, 0, 0],
            Some(""),
        )
        .unwrap();
        let merged = draft.apply_to(&c).unwrap();
        let rule = &merged.app_rules["domain:example.com"];
        assert!(!rule.contains_key("enabled"));
        assert_eq!(rule["tab_enabled"], false);
        assert_eq!(rule["record_history"], true);
        assert_eq!(rule["custom_instructions"], "");
    }
    #[test]
    fn draft_rebase_preserves_concurrent_model_and_unrelated_rules() {
        let c = Config::default();
        let mut draft = SettingsDraft::new(&c).unwrap();
        draft.set("clipboard_context", json!(true)).unwrap();
        put_rule(&mut draft, None, "app", "owned.exe", &[2, 0, 0, 0, 0], None).unwrap();
        let mut current = c.clone();
        current.model_path = "new.gguf".into();
        current.app_rules.insert(
            "app:other.exe".into(),
            [("enabled".into(), json!(false))].into(),
        );
        draft.refresh(&current).unwrap();
        let merged = draft.apply_to(&current).unwrap();
        assert_eq!(merged.model_path, "new.gguf");
        assert!(merged.clipboard_context);
        assert_eq!(merged.app_rules.len(), 2);
        draft.reset(&current).unwrap();
        assert!(!draft.is_dirty());
        assert!(!draft.apply_to(&current).unwrap().clipboard_context);
    }
    #[test]
    fn selected_pending_model_never_borrows_readiness() {
        let mut w = WelcomeState {
            step: 3,
            ..Default::default()
        };
        w.select("one".into());
        w.ready = "one".into();
        assert!(w.can_continue());
        w.select("two".into());
        assert!(!w.can_continue());
        w.ready = "one".into();
        assert!(!w.can_continue());
    }
    #[test]
    fn system_access_review_is_explicit_not_permission_success() {
        let mut w = WelcomeState {
            step: 2,
            ..Default::default()
        };
        assert!(!w.can_continue());
        w.access_reviewed = true;
        assert!(w.can_continue());
    }
    #[test]
    fn ephemeral_preview_never_inherits_external_privacy_consents() {
        let mut c = Config::default();
        c.clipboard_context = true;
        c.screen_context = true;
        c.screenshot_context = true;
        c.screenshot_appearance = true;
        c.record_history = true;
        c.use_history = true;
        c.record_unaccepted = true;
        c.app_rules.insert(
            "app:secret.exe".into(),
            [("record_history".into(), json!(true))].into(),
        );
        let d = SettingsDraft::new(&c).unwrap();
        let p = preview_config(&d, &c).unwrap();
        assert!(
            !p.clipboard_context
                && !p.screen_context
                && !p.screenshot_context
                && !p.screenshot_appearance
        );
        assert!(
            !p.record_history && !p.use_history && !p.record_unaccepted && !p.statistics_enabled
        );
        assert!(p.app_rules.is_empty());
        assert!(c.record_history);
        assert!(d.value("clipboard_context") == Some(&json!(true)));
    }
    #[test]
    fn cleanup_does_not_release_gui_guards_until_every_owner_stops() {
        for mask in 0..8 {
            assert_eq!(
                cleanup_complete(mask & 1 != 0, mask & 2 != 0, mask & 4 != 0),
                mask == 7
            );
        }
    }
    #[test]
    fn malformed_numeric_controls_fail_instead_of_resetting() {
        let f = *FIELDS.iter().find(|f| f.key == "max_tokens").unwrap();
        assert!(control_value(f, "-1", false, 0).is_err());
        assert!(control_value(f, "129", false, 0).is_ok());
        let mut d = SettingsDraft::new(&Config::default()).unwrap();
        d.set("max_tokens", control_value(f, "129", false, 0).unwrap())
            .unwrap();
        assert!(d.apply_to(&Config::default()).is_err());
    }
}
