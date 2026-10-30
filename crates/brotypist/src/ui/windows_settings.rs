//! Native-control descriptions and non-visual Windows settings policy.
use crate::{config::Config, ui::settings::SettingsDraft};
use anyhow::{Result, ensure};
use serde_json::{Value, json};

#[derive(Clone, Copy)]
pub enum Kind { Check, Text, Multiline, Number, Choice(&'static [&'static str]) }
#[derive(Clone, Copy)]
pub struct Field { pub pane: usize, pub key: &'static str, pub label: &'static str, pub kind: Kind }
macro_rules! fields { ($(($p:expr,$k:literal,$l:literal,$t:expr)),* $(,)?) => { pub const FIELDS: &[Field] = &[$(Field { pane:$p,key:$k,label:$l,kind:$t }),*]; }; }
use Kind::*;
pub const PANES: &[&str] = &["General", "AI Models", "Autocomplete", "Languages", "Privacy", "About"];
fields![
(0,"global_enabled","Enable predictions",Check),
(0,"enabled","Enable in eligible apps by default",Check),
(0,"launch_at_login","Launch at login (explicit Save applies registration)",Check),
(0,"show_accessory","Show field accessory",Check),
(0,"statistics_enabled","Count accepted completions (aggregate only)",Check),
(0,"setup_completed","Setup completed",Check),
(1,"backend","Local backend",Choice(&["gguf","ollama","openai"])),
(1,"model_path","GGUF path (imports reference your original file)",Text),
(1,"model_id","Selected catalog ID (empty for imported files)",Text),
(1,"endpoint","Literal loopback server URL",Text),
(1,"model","Local server model name",Text),
(1,"acceleration","Acceleration",Choice(&["auto","cpu"])),
(1,"prompt_style","Prompt style",Choice(&["continuation","writing"])),
(1,"max_tokens","Maximum generated tokens (1–128)",Number),
(1,"idle_release_seconds","Release idle model after seconds (0 keeps ready)",Number),
(1,"battery_on_demand","Battery: generate only when requested",Check),
(1,"battery_model","Battery model ID (same or installed catalog model)",Text),
(1,"battery_shorter","Battery: prefer shorter completions",Check),
(2,"complete_inside_text","Complete inside existing text",Check),
(2,"completion_length","Maximum completion length",Choice(&["word","short","medium","long"])),
(2,"debounce_ms","Typing pause in milliseconds (100–5000)",Number),
(2,"min_chars","Minimum typed characters (1–1000)",Number),
(2,"trailing_space","Include trailing space when accepting",Check),
(2,"punctuation_mode","Trailing punctuation",Choice(&["full","always","never"])),
(2,"escape_behavior","Escape behavior",Choice(&["pause","app","passthrough"])),
(2,"escape_pause_seconds","Escape pause duration in seconds",Number),
(2,"emoji_enabled","Find emoji by typing :name",Check),
(2,"emoticons_enabled","Convert emoticons",Check),
(2,"emoji_skin_tone","Skin tone (0 default, 1–5 light to dark)",Number),
(2,"emoji_include_neutral","Include neutral-tone alternatives",Check),
(2,"emoji_gender","Emoji gender",Choice(&["neutral","female","male"])),
(2,"alternatives_enabled","Offer alternative words",Check),
(2,"alternatives_mode","Show alternatives",Choice(&["shortcut","pause"])),
(2,"alternatives_delay_ms","Alternatives delay milliseconds",Number),
(2,"synonyms_enabled","Offer selected-text synonyms",Check),
(2,"synonyms_mode","Show synonyms",Choice(&["selection","shortcut"])),
(2,"synonyms_delay_ms","Synonym delay milliseconds",Number),
(3,"custom_instructions","Language, tone and vocabulary instructions (up to 8000 characters)",Multiline),
(3,"typo_suppression","Suppress completion while local spelling is uncertain",Check),
(3,"typo_fixes","Offer native dictionary corrections",Check),
(4,"clipboard_context","Include clipboard text (independent consent)",Check),
(4,"screen_context","Include focused-window Accessibility text",Check),
(4,"screenshot_context","Include focused-window OCR (independent consent)",Check),
(4,"screenshot_appearance","Read focused-window appearance (independent consent)",Check),
(4,"terminal_completion","Enable narrowly supported terminal completion",Check),
(4,"context_chars","Maximum typed context (64–8000 characters)",Number),
(4,"record_history","Learn from encrypted writing by default",Check),
(4,"record_unaccepted","Learn even when no suggestion was accepted",Check),
(4,"personalization_strength","Style and vocabulary strength (0–100 percent)",Number),
(4,"excluded_apps","Always excluded exact app IDs (one per line)",Multiline),
(5,"check_updates_automatically","Check signed releases automatically",Check),
];

pub fn control_value(field: Field, text: &str, checked: bool, selected: usize) -> Result<Value> {
    Ok(match field.kind {
        Check => json!(checked),
        Number => json!(text.trim().parse::<u32>()?),
        Choice(values) => json!(values.get(selected).ok_or_else(|| anyhow::anyhow!("Choose a valid setting."))?),
        _ if field.key == "excluded_apps" => json!(text.lines().map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>()),
        _ => json!(text),
    })
}

pub const RULE_FLAGS: &[(&str,&str)] = &[("enabled","Completions"),("tab_enabled","Accept with Tab"),("record_history","Encrypted learning"),("improve_compatibility","Improve compatibility"),("complete_in_code_editors","Complete in code editors")];
pub fn put_rule(draft: &mut SettingsDraft, previous: Option<&str>, kind: &str, identity: &str, values: &[usize], instructions: Option<&str>) -> Result<String> {
    ensure!(matches!(kind,"app"|"domain"), "Choose application or website.");
    let identity = identity.trim(); ensure!(!identity.is_empty(),"Enter an exact application ID or hostname.");
    let key = format!("{kind}:{}",if kind == "domain" { crate::config::normalize_domain(identity)? } else { identity.into() });
    let mut rule = serde_json::Map::new();
    for ((name,_),value) in RULE_FLAGS.iter().zip(values) { match value { 0 => {},1 => {rule.insert((*name).into(),json!(true));},2 => {rule.insert((*name).into(),json!(false));},_ => anyhow::bail!("Invalid inheritance choice.") } }
    if let Some(value) = instructions { rule.insert("custom_instructions".into(),json!(value)); }
    let mut rules = draft.value("app_rules").and_then(Value::as_object).cloned().unwrap_or_default();
    if let Some(old) = previous { rules.remove(old); }
    rules.insert(key.clone(),Value::Object(rule)); draft.set("app_rules",Value::Object(rules))?; Ok(key)
}
pub fn remove_rule(draft: &mut SettingsDraft, key: &str) -> Result<()> {
    let mut rules = draft.value("app_rules").and_then(Value::as_object).cloned().unwrap_or_default(); rules.remove(key); draft.set("app_rules",Value::Object(rules))
}
#[derive(Default)]
pub struct WelcomeState { pub step: usize, pub selected: String, pub ready: String, pub access_reviewed: bool }
impl WelcomeState {
    pub fn select(&mut self, id: String) { if self.selected != id { self.ready.clear(); } self.selected = id; }
    pub fn can_continue(&self) -> bool {
        match self.step { 2 => self.access_reviewed, 3 => !self.selected.is_empty() && self.selected == self.ready, _ => true }
    }
}

/// Welcome inference cannot borrow external context, history or statistics consent.
pub fn preview_config(draft:&SettingsDraft,current:&Config)->Result<Config>{
    let mut config=draft.apply_to(current)?;
    config.clipboard_context=false;config.screen_context=false;
    config.screenshot_context=false;config.screenshot_appearance=false;
    config.record_history=false;config.record_unaccepted=false;config.statistics_enabled=false;
    config.app_rules.clear();config.excluded_apps.clear();
    Ok(config)
}

pub fn cleanup_complete(controller:bool,preview:bool,updates:bool)->bool{
    controller&&preview&&updates
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn every_config_field_has_a_real_control_or_structured_editor() {
        let value = serde_json::to_value(Config::default()).unwrap();
        for key in value.as_object().unwrap().keys() { assert!(FIELDS.iter().any(|f| f.key == key) || ["shortcuts","app_rules"].contains(&key.as_str()),"{key}"); }
    }
    #[test] fn rules_distinguish_inheritance_false_and_empty_instructions() {
        let c=Config::default(); let mut draft=SettingsDraft::new(&c).unwrap();
        put_rule(&mut draft,None,"domain","Example.COM.",&[0,2,1,0,0],Some("")).unwrap();
        let merged=draft.apply_to(&c).unwrap(); let rule=&merged.app_rules["domain:example.com"];
        assert!(!rule.contains_key("enabled")); assert_eq!(rule["tab_enabled"],false); assert_eq!(rule["record_history"],true); assert_eq!(rule["custom_instructions"],"");
    }
    #[test] fn draft_rebase_preserves_concurrent_model_and_unrelated_rules() {
        let c=Config::default(); let mut draft=SettingsDraft::new(&c).unwrap(); draft.set("clipboard_context",json!(true)).unwrap();
        put_rule(&mut draft,None,"app","owned.exe",&[2,0,0,0,0],None).unwrap();
        let mut current=c.clone(); current.model_path="new.gguf".into(); current.app_rules.insert("app:other.exe".into(),[("enabled".into(),json!(false))].into());
        draft.refresh(&current).unwrap(); let merged=draft.apply_to(&current).unwrap(); assert_eq!(merged.model_path,"new.gguf"); assert!(merged.clipboard_context); assert_eq!(merged.app_rules.len(),2);
        draft.reset(&current).unwrap(); assert!(!draft.is_dirty()); assert!(!draft.apply_to(&current).unwrap().clipboard_context);
    }
    #[test] fn selected_pending_model_never_borrows_readiness() { let mut w=WelcomeState {step:3,..Default::default()}; w.select("one".into()); w.ready="one".into(); assert!(w.can_continue()); w.select("two".into()); assert!(!w.can_continue()); w.ready="one".into(); assert!(!w.can_continue()); }
    #[test] fn system_access_review_is_explicit_not_permission_success() { let mut w=WelcomeState { step:2,..Default::default() }; assert!(!w.can_continue()); w.access_reviewed=true; assert!(w.can_continue()); }
    #[test] fn ephemeral_preview_never_inherits_external_privacy_consents() {
        let mut c=Config::default();c.clipboard_context=true;c.screen_context=true;c.screenshot_context=true;c.screenshot_appearance=true;c.record_history=true;c.record_unaccepted=true;
        c.app_rules.insert("app:secret.exe".into(),[("record_history".into(),json!(true))].into());
        let d=SettingsDraft::new(&c).unwrap();let p=preview_config(&d,&c).unwrap();
        assert!(!p.clipboard_context&&!p.screen_context&&!p.screenshot_context&&!p.screenshot_appearance);
        assert!(!p.record_history&&!p.record_unaccepted&&!p.statistics_enabled);assert!(p.app_rules.is_empty());
        assert!(c.record_history);assert!(d.value("clipboard_context")==Some(&json!(true)));
    }
    #[test] fn cleanup_does_not_release_gui_guards_until_every_owner_stops(){
        for mask in 0..8{assert_eq!(cleanup_complete(mask&1!=0,mask&2!=0,mask&4!=0),mask==7);}
    }
    #[test] fn malformed_numeric_controls_fail_instead_of_resetting() { let f=*FIELDS.iter().find(|f|f.key=="max_tokens").unwrap(); assert!(control_value(f,"-1",false,0).is_err()); assert!(control_value(f,"129",false,0).is_ok()); let mut d=SettingsDraft::new(&Config::default()).unwrap(); d.set("max_tokens",control_value(f,"129",false,0).unwrap()).unwrap(); assert!(d.apply_to(&Config::default()).is_err()); }
}
