//! Strict, backward-compatible preferences. Draft text never belongs here.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    pub backend: String,
    pub model_path: String,
    pub endpoint: String,
    pub model: String,
    pub debounce_ms: u32,
    pub min_chars: u32,
    pub max_tokens: u32,
    pub context_chars: u32,
    pub enabled: bool,
    pub global_enabled: bool,
    pub acceleration: String,
    pub prompt_style: String,
    pub excluded_apps: Vec<String>,
    pub setup_completed: bool,
    pub launch_at_login: bool,
    pub check_updates_automatically: bool,
    pub show_accessory: bool,
    pub complete_inside_text: bool,
    pub completion_length: String,
    pub idle_release_seconds: u32,
    pub trailing_space: bool,
    pub punctuation_mode: String,
    pub escape_behavior: String,
    pub escape_pause_seconds: u32,
    pub shortcuts: BTreeMap<String, String>,
    pub clipboard_context: bool,
    pub screen_context: bool,
    pub screenshot_context: bool,
    pub screenshot_appearance: bool,
    pub terminal_completion: bool,
    pub typo_suppression: bool,
    pub typo_fixes: bool,
    pub custom_instructions: String,
    pub record_history: bool,
    pub record_unaccepted: bool,
    pub personalization_strength: u32,
    pub statistics_enabled: bool,
    pub emoji_enabled: bool,
    pub emoticons_enabled: bool,
    pub emoji_skin_tone: u32,
    pub emoji_include_neutral: bool,
    pub emoji_gender: String,
    pub alternatives_enabled: bool,
    pub alternatives_mode: String,
    pub alternatives_delay_ms: u32,
    pub synonyms_enabled: bool,
    pub synonyms_mode: String,
    pub synonyms_delay_ms: u32,
    pub battery_on_demand: bool,
    pub battery_model: String,
    pub battery_shorter: bool,
    pub model_id: String,
    pub app_rules: BTreeMap<String, BTreeMap<String, Value>>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            backend: "gguf".into(),
            model_path: String::new(),
            endpoint: "http://127.0.0.1:11434".into(),
            model: "qwen3:0.6b".into(),
            debounce_ms: 150,
            min_chars: 1,
            max_tokens: 24,
            context_chars: 2000,
            enabled: true,
            global_enabled: true,
            acceleration: "auto".into(),
            prompt_style: "continuation".into(),
            excluded_apps: [
                "terminal",
                "iterm",
                "powershell",
                "cmd.exe",
                "windowsterminal",
                "konsole",
                "gnome-terminal",
                "kitty",
                "wezterm",
                "alacritty",
                "1password",
                "bitwarden",
                "keepass",
                "keychain",
                "lastpass",
            ]
            .map(String::from)
            .to_vec(),
            setup_completed: false,
            launch_at_login: false,
            check_updates_automatically: true,
            show_accessory: true,
            complete_inside_text: true,
            completion_length: "medium".into(),
            idle_release_seconds: 300,
            trailing_space: true,
            punctuation_mode: "full".into(),
            escape_behavior: "pause".into(),
            escape_pause_seconds: 3,
            shortcuts: crate::shortcuts::default_shortcuts(),
            clipboard_context: false,
            screen_context: false,
            screenshot_context: false,
            screenshot_appearance: false,
            terminal_completion: false,
            typo_suppression: false,
            typo_fixes: false,
            custom_instructions: String::new(),
            record_history: false,
            record_unaccepted: false,
            personalization_strength: 50,
            statistics_enabled: true,
            emoji_enabled: true,
            emoticons_enabled: true,
            emoji_skin_tone: 0,
            emoji_include_neutral: true,
            emoji_gender: "neutral".into(),
            alternatives_enabled: false,
            alternatives_mode: "shortcut".into(),
            alternatives_delay_ms: 1500,
            synonyms_enabled: true,
            synonyms_mode: "selection".into(),
            synonyms_delay_ms: 1500,
            battery_on_demand: false,
            battery_model: "same".into(),
            battery_shorter: true,
            model_id: String::new(),
            app_rules: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn validate(&mut self) -> Result<()> {
        for (name, value, choices) in [
            (
                "backend",
                self.backend.as_str(),
                &["gguf", "ollama", "openai"][..],
            ),
            ("acceleration", &self.acceleration, &["auto", "cpu"][..]),
            (
                "prompt_style",
                &self.prompt_style,
                &["continuation", "writing"][..],
            ),
            (
                "completion_length",
                &self.completion_length,
                &["word", "short", "medium", "long"][..],
            ),
            (
                "escape_behavior",
                &self.escape_behavior,
                &["pause", "app", "passthrough"][..],
            ),
            (
                "emoji_gender",
                &self.emoji_gender,
                &["neutral", "female", "male"][..],
            ),
            (
                "alternatives_mode",
                &self.alternatives_mode,
                &["shortcut", "pause"][..],
            ),
            (
                "punctuation_mode",
                &self.punctuation_mode,
                &["full", "always", "never"][..],
            ),
            (
                "synonyms_mode",
                &self.synonyms_mode,
                &["selection", "shortcut"][..],
            ),
        ] {
            ensure!(
                choices.contains(&value),
                "{name} must be one of {}",
                choices.join(", ")
            );
        }
        for (name, value, low, high) in [
            ("debounce_ms", self.debounce_ms, 100, 5000),
            ("min_chars", self.min_chars, 1, 1000),
            ("max_tokens", self.max_tokens, 1, 128),
            ("context_chars", self.context_chars, 64, 8000),
            ("idle_release_seconds", self.idle_release_seconds, 0, 3600),
            ("escape_pause_seconds", self.escape_pause_seconds, 1, 3600),
            (
                "personalization_strength",
                self.personalization_strength,
                0,
                100,
            ),
            ("emoji_skin_tone", self.emoji_skin_tone, 0, 5),
            (
                "alternatives_delay_ms",
                self.alternatives_delay_ms,
                100,
                10000,
            ),
            ("synonyms_delay_ms", self.synonyms_delay_ms, 100, 10000),
        ] {
            ensure!(
                (low..=high).contains(&value),
                "{name} must be an integer between {low} and {high}"
            );
        }
        ensure!(
            self.custom_instructions.chars().count() <= 8000,
            "Custom instructions cannot exceed 8000 characters"
        );
        ensure!(
            self.battery_model == "same" || crate::models::get_preset(&self.battery_model).is_ok(),
            "battery_model must be 'same' or an available model preset"
        );
        ensure!(
            self.model_id.is_empty() || crate::models::get_preset(&self.model_id).is_ok(),
            "model_id must be empty or an available model preset"
        );
        ensure!(
            self.excluded_apps
                .iter()
                .all(|s| !s.trim().is_empty() && s.chars().count() <= 512),
            "Excluded apps must be a list of non-empty app identifiers"
        );
        let mut shortcuts = crate::shortcuts::default_shortcuts();
        for (action, value) in &self.shortcuts {
            ensure!(
                shortcuts.contains_key(action),
                "Shortcuts contain an unknown action: {action}"
            );
            shortcuts.insert(action.clone(), crate::shortcuts::normalize_shortcut(value)?);
        }
        let active: Vec<_> = shortcuts.values().filter(|s| !s.is_empty()).collect();
        ensure!(
            active.len() == active.iter().collect::<BTreeSet<_>>().len(),
            "Assign each active shortcut to only one action"
        );
        self.shortcuts = shortcuts;
        ensure!(
            self.app_rules.len() <= 1000,
            "App rules must contain at most 1000 entries"
        );
        let mut rules = BTreeMap::new();
        for (key, rule) in &self.app_rules {
            let (kind, identity) = key
                .split_once(':')
                .context("Rules require app:EXACT_APP_ID or domain:host keys")?;
            ensure!(
                matches!(kind, "app" | "domain") && !identity.trim().is_empty(),
                "Rules require app:EXACT_APP_ID or domain:host keys"
            );
            for (name, value) in rule {
                match name.as_str() {
                    "custom_instructions" => ensure!(
                        value.as_str().is_some_and(|s| s.chars().count() <= 8000),
                        "Per-app instructions must be text up to 8000 characters"
                    ),
                    "enabled"
                    | "tab_enabled"
                    | "record_history"
                    | "improve_compatibility"
                    | "complete_in_code_editors" => {
                        ensure!(value.is_boolean(), "Rule {name} must be a boolean")
                    }
                    _ => bail!("Invalid per-app rule field: {name}"),
                }
            }
            let key = if kind == "domain" {
                format!("domain:{}", normalize_domain(identity)?)
            } else {
                key.clone()
            };
            // Optional fields remain absent; false is a meaningful override, not inheritance.
            rules.insert(key, rule.clone());
        }
        self.app_rules = rules;
        if self.backend != "gguf" {
            local_endpoint(&self.endpoint)?;
            ensure!(
                !self.model.trim().is_empty(),
                "Set a model name for the local inference server"
            );
        }
        Ok(())
    }
    pub fn provider_identity(&self) -> Vec<String> {
        if self.backend == "gguf" {
            vec![
                self.backend.clone(),
                self.model_path.clone(),
                self.acceleration.clone(),
            ]
        } else {
            vec![
                self.backend.clone(),
                self.endpoint.clone(),
                self.model.clone(),
            ]
        }
    }
}

/// Python IDNA2003-compatible nameprep followed by ASCII hostname encoding.
pub fn normalize_domain(host: &str) -> Result<String> {
    let folded = caseless::default_case_fold_str(host.trim().trim_end_matches('.'));
    // RFC 3454 B.1 characters are mapped to nothing by Python's IDNA codec.
    let mapped: String = folded.chars().filter(|&c| !matches!(c, '\u{00ad}' | '\u{034f}' | '\u{1806}' | '\u{180b}'..='\u{180d}' | '\u{200b}'..='\u{200d}' | '\u{2060}' | '\u{fe00}'..='\u{fe0f}' | '\u{feff}')).nfkc().collect();
    let value =
        idna::domain_to_ascii(&mapped).map_err(|e| anyhow::anyhow!("Invalid domain: {e}"))?;
    ensure!(
        !value.is_empty()
            && value.len() <= 253
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-')),
        "Domains must contain a hostname, not a URL or path"
    );
    ensure!(
        value
            .trim_end_matches('.')
            .split('.')
            .all(|label| !label.is_empty() && label.len() <= 63),
        "Invalid domain label length"
    );
    Ok(value)
}

/// Validate authority as supplied, before a URL library can rewrite numeric hosts.
/// Transport owners must independently disable proxies and redirects.
pub fn local_endpoint(endpoint: &str) -> Result<String> {
    let invalid =
        || anyhow::anyhow!("Use a literal loopback server URL, e.g. http://127.0.0.1:11434");
    ensure!(
        !endpoint
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
            && !endpoint.contains(['?', '#', '\\']),
        "Use a literal loopback server URL without credentials, query, fragment or controls"
    );
    let (scheme, rest) = endpoint.split_once("://").ok_or_else(invalid)?;
    ensure!(
        scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"),
        "Use HTTP(S) for the local inference server"
    );
    let authority = rest.split('/').next().unwrap_or("");
    ensure!(
        !authority.contains('@'),
        "Local server credentials are not allowed"
    );
    let (host, port) = if let Some(ipv6) = authority.strip_prefix('[') {
        let (host, remainder) = ipv6.split_once(']').ok_or_else(invalid)?;
        ensure!(
            host.parse::<std::net::Ipv6Addr>().is_ok(),
            "Bracketed server hosts must be literal IPv6 addresses"
        );
        let port = if remainder.is_empty() {
            None
        } else {
            Some(remainder.strip_prefix(':').ok_or_else(invalid)?)
        };
        (host, port)
    } else {
        match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    let ip: IpAddr = host.parse().map_err(|_| invalid())?;
    ensure!(
        ip.is_loopback(),
        "Inference server must use a literal loopback address"
    );
    if let Some(port) = port {
        ensure!(
            !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()),
            "Invalid local server port"
        );
        let port: u16 = port.parse().map_err(|_| invalid())?;
        ensure!(port > 0, "Local server port must not be zero");
    }
    Ok(endpoint.trim_end_matches('/').to_owned())
}

pub fn config_path() -> Result<PathBuf> {
    Ok(crate::paths::config_dir()?.join("settings.json"))
}
pub fn load_config() -> Result<Config> {
    load_config_from(&config_path()?)
}
pub fn save_config(config: &Config) -> Result<()> {
    save_config_to(config, &config_path()?)
}

/// Explicit paths permit disposable profiles without changing process environment.
pub fn load_config_from(path: &Path) -> Result<Config> {
    match std::fs::read(path) {
        Ok(bytes) => config_from_json(&bytes)
            .with_context(|| format!("Cannot load settings {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A dangling symlink is a broken saved profile, not a new installation.
            if std::fs::symlink_metadata(path).is_ok() {
                bail!("Settings path is a dangling symlink: {}", path.display());
            }
            Ok(Config {
                model_id: crate::models::recommended_preset(crate::models::physical_memory_bytes())
                    .id
                    .clone(),
                custom_instructions: default_instructions(None),
                ..Config::default()
            })
        }
        Err(error) => {
            Err(error).with_context(|| format!("Cannot read settings {}", path.display()))
        }
    }
}

fn config_from_json(bytes: &[u8]) -> Result<Config> {
    let mut value: Value = serde_json::from_slice(bytes)?;
    let data = value
        .as_object_mut()
        .context("Settings must be an object")?;
    if !data.contains_key("battery_model") {
        data.insert(
            "battery_model".into(),
            Value::String(
                if data.get("battery_smaller_model") == Some(&Value::Bool(true))
                    && crate::models::get_preset("tiny").is_ok()
                {
                    "tiny"
                } else {
                    "same"
                }
                .into(),
            ),
        );
    } else if data["battery_model"]
        .as_str()
        .is_some_and(|id| id != "same" && crate::models::get_preset(id).is_err())
    {
        data.insert("battery_model".into(), Value::String("same".into()));
    }
    if !data.contains_key("model_id")
        || data["model_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty() && crate::models::get_preset(id).is_err())
    {
        // Only an existing GGUF filename identifies a legacy selection.
        let filename = data
            .get("model_path")
            .and_then(Value::as_str)
            .and_then(|s| Path::new(s).file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let id = if data.get("backend").map_or(true, |v| v == "gguf") && !filename.is_empty() {
            crate::models::presets()
                .iter()
                .find(|p| p.filename == filename)
                .map(|p| p.id.as_str())
                .unwrap_or("")
        } else {
            ""
        };
        data.insert("model_id".into(), Value::String(id.into()));
    }
    let mut config: Config = serde_json::from_value(value)?;
    config.validate()?;
    Ok(config)
}

pub fn save_config_to(config: &Config, path: &Path) -> Result<()> {
    let mut normalized = config.clone();
    normalized.validate()?;
    let directory = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    crate::paths::private_directory(directory)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".settings-")
        .tempfile_in(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    serde_json::to_writer_pretty(temporary.as_file_mut(), &normalized)?;
    temporary.write_all(b"\n")?;
    temporary.as_file_mut().flush()?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|e| {
        anyhow::anyhow!(
            "Cannot atomically replace settings {}: {}",
            path.display(),
            e.error
        )
    })?;
    #[cfg(unix)]
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}

pub fn default_instructions(locale: Option<&str>) -> String {
    let system;
    let identifier = match locale.filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => {
            system = system_locale();
            &system
        }
    };
    let identifier = identifier.replace('-', "_");
    let parts: Vec<_> = identifier.split('_').filter(|s| !s.is_empty()).collect();
    let language = parts
        .first()
        .filter(|s| s.chars().all(char::is_alphabetic))
        .map(|s| caseless::default_case_fold_str(s))
        .unwrap_or_else(|| "en".into());
    let region = parts
        .iter()
        .skip(1)
        .rev()
        .find(|s| {
            (s.len() == 2 && s.chars().all(char::is_alphabetic))
                || (s.len() == 3 && s.bytes().all(|c| c.is_ascii_digit()))
        })
        .map(|s| s.to_uppercase())
        .unwrap_or_default();
    let language_name = english_name("language", &language);
    let region_name = if region.is_empty() {
        String::new()
    } else {
        english_name("region", &region)
    };
    if region_name.is_empty() || region_name == region {
        format!(
            "Write in {language_name} by default. If the surrounding text is in another language, continue in that language."
        )
    } else {
        format!(
            "Write in {language_name} by default, using {region_name} spelling, vocabulary and punctuation, and {region_name} conventions for dates, numbers and currency. If the surrounding text is in another language, continue in that language."
        )
    }
}

fn system_locale() -> String {
    #[cfg(target_os = "macos")]
    if let Some(locale) = mac_locale::system_locale() {
        return locale;
    }
    for name in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(name) {
            let value = value
                .split('.')
                .next()
                .unwrap_or("")
                .split('@')
                .next()
                .unwrap_or("");
            if !value.is_empty() && !matches!(value, "C" | "POSIX") {
                return value.to_owned();
            }
        }
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetUserDefaultLocaleName(buffer: *mut u16, count: i32) -> i32;
        }
        let mut buffer = [0u16; 85];
        let count = unsafe { GetUserDefaultLocaleName(buffer.as_mut_ptr(), buffer.len() as i32) };
        if count > 1 {
            return String::from_utf16_lossy(&buffer[..count as usize - 1]);
        }
    }
    "en_US".into()
}

fn english_name(kind: &str, code: &str) -> String {
    #[cfg(target_os = "macos")]
    if let Some(name) = mac_locale::english_name(kind, code) {
        return name;
    }
    if kind != "language" {
        return code.into();
    }
    match code {
        "ar" => "Arabic",
        "ca" => "Catalan",
        "cs" => "Czech",
        "da" => "Danish",
        "de" => "German",
        "el" => "Greek",
        "en" => "English",
        "es" => "Spanish",
        "fi" => "Finnish",
        "fr" => "French",
        "he" => "Hebrew",
        "hi" => "Hindi",
        "hu" => "Hungarian",
        "id" => "Indonesian",
        "it" => "Italian",
        "ja" => "Japanese",
        "ko" => "Korean",
        "nb" => "Norwegian Bokmål",
        "nl" => "Dutch",
        "pl" => "Polish",
        "pt" => "Portuguese",
        "ro" => "Romanian",
        "ru" => "Russian",
        "sk" => "Slovak",
        "sv" => "Swedish",
        "th" => "Thai",
        "tr" => "Turkish",
        "uk" => "Ukrainian",
        "vi" => "Vietnamese",
        "zh" => "Chinese",
        _ => code,
    }
    .into()
}

#[cfg(target_os = "macos")]
mod mac_locale {
    use std::ffi::{CString, c_char, c_void};
    type Ref = *const c_void;
    const UTF8: u32 = 0x08000100;
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(value: Ref);
        fn CFStringCreateWithCString(allocator: Ref, text: *const c_char, encoding: u32) -> Ref;
        fn CFStringGetLength(value: Ref) -> isize;
        fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
        fn CFStringGetCString(value: Ref, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
        fn CFArrayGetCount(array: Ref) -> isize;
        fn CFArrayGetValueAtIndex(array: Ref, index: isize) -> Ref;
        fn CFLocaleCopyPreferredLanguages() -> Ref;
        fn CFLocaleCopyCurrent() -> Ref;
        fn CFLocaleCreate(allocator: Ref, identifier: Ref) -> Ref;
        fn CFLocaleGetValue(locale: Ref, key: Ref) -> Ref;
        fn CFLocaleCopyDisplayNameForPropertyValue(locale: Ref, key: Ref, value: Ref) -> Ref;
        static kCFLocaleCountryCode: Ref;
        static kCFLocaleLanguageCode: Ref;
    }
    struct Owned(Ref);
    impl Owned {
        fn new(value: Ref) -> Option<Self> {
            (!value.is_null()).then_some(Self(value))
        }
    }
    impl Drop for Owned {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0) }
        }
    }
    fn cf_string(text: &str) -> Option<Owned> {
        let text = CString::new(text).ok()?;
        Owned::new(unsafe { CFStringCreateWithCString(std::ptr::null(), text.as_ptr(), UTF8) })
    }
    fn string(value: Ref) -> Option<String> {
        if value.is_null() {
            return None;
        }
        let length = unsafe { CFStringGetMaximumSizeForEncoding(CFStringGetLength(value), UTF8) }
            .checked_add(1)?;
        if !(1..=1024 * 1024).contains(&length) {
            return None;
        }
        let mut bytes = vec![0u8; length as usize];
        if unsafe { CFStringGetCString(value, bytes.as_mut_ptr().cast(), length, UTF8) } == 0 {
            return None;
        }
        let end = bytes.iter().position(|&b| b == 0)?;
        bytes.truncate(end);
        String::from_utf8(bytes).ok()
    }
    pub(super) fn system_locale() -> Option<String> {
        let languages = Owned::new(unsafe { CFLocaleCopyPreferredLanguages() })?;
        if unsafe { CFArrayGetCount(languages.0) } == 0 {
            return None;
        }
        let preferred = string(unsafe { CFArrayGetValueAtIndex(languages.0, 0) })?;
        let language = preferred
            .split(['-', '_'])
            .next()
            .filter(|s| !s.is_empty())?;
        let locale = Owned::new(unsafe { CFLocaleCopyCurrent() })?;
        let region =
            string(unsafe { CFLocaleGetValue(locale.0, kCFLocaleCountryCode) }).unwrap_or_default();
        Some(if region.is_empty() {
            language.to_owned()
        } else {
            format!("{language}_{region}")
        })
    }
    pub(super) fn english_name(kind: &str, code: &str) -> Option<String> {
        let english_id = cf_string("en")?;
        let locale = Owned::new(unsafe { CFLocaleCreate(std::ptr::null(), english_id.0) })?;
        let value = cf_string(code)?;
        let key = unsafe {
            if kind == "language" {
                kCFLocaleLanguageCode
            } else {
                kCFLocaleCountryCode
            }
        };
        let name =
            Owned::new(unsafe { CFLocaleCopyDisplayNameForPropertyValue(locale.0, key, value.0) })?;
        string(name.0).filter(|s| !s.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn from(value: Value) -> Result<Config> {
        config_from_json(&serde_json::to_vec(&value).unwrap())
    }

    #[test]
    fn defaults_preserve_every_scalar_and_independent_consent() {
        let mut config = Config::default();
        config.validate().unwrap();
        assert!(
            config.enabled
                && config.global_enabled
                && config.statistics_enabled
                && config.check_updates_automatically
        );
        assert_eq!(
            (
                config.debounce_ms,
                config.min_chars,
                config.max_tokens,
                config.context_chars,
                config.idle_release_seconds
            ),
            (150, 1, 24, 2000, 300)
        );
        assert!(
            !config.clipboard_context
                && !config.screen_context
                && !config.screenshot_context
                && !config.screenshot_appearance
                && !config.terminal_completion
                && !config.record_history
                && !config.record_unaccepted
        );
        assert_eq!(
            serde_json::to_value(&config)
                .unwrap()
                .as_object()
                .unwrap()
                .len(),
            53
        );
        assert!(config.custom_instructions.is_empty() && config.model_id.is_empty());
    }
    #[test]
    fn strict_types_reject_boolean_numbers_null_and_wrong_collections() {
        for (key, value) in [
            ("idle_release_seconds", json!(-1)),
            ("idle_release_seconds", json!(true)),
            ("debounce_ms", json!(100.0)),
            ("screen_context", json!(1)),
            ("record_unaccepted", json!("yes")),
            ("typo_fixes", json!(1)),
            ("screenshot_context", json!("yes")),
            ("battery_model", json!(true)),
            ("model_id", json!(null)),
            ("model_path", json!(1)),
            ("excluded_apps", json!("terminal")),
            ("excluded_apps", json!([1])),
            ("shortcuts", json!([])),
            ("app_rules", json!([])),
        ] {
            let mut value_map = serde_json::Map::new();
            value_map.insert(key.into(), value);
            assert!(from(Value::Object(value_map)).is_err(), "{key}");
        }
        for key in serde_json::to_value(Config::default())
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
        {
            let mut object = serde_json::Map::new();
            object.insert(key.clone(), Value::Null);
            assert!(from(Value::Object(object)).is_err(), "null {key}");
        }
    }
    #[test]
    fn numeric_boundaries_are_inclusive_and_all_outside_values_fail() {
        for (name, low, high) in [
            ("debounce_ms", 100, 5000),
            ("min_chars", 1, 1000),
            ("max_tokens", 1, 128),
            ("context_chars", 64, 8000),
            ("idle_release_seconds", 0, 3600),
            ("escape_pause_seconds", 1, 3600),
            ("personalization_strength", 0, 100),
            ("emoji_skin_tone", 0, 5),
            ("alternatives_delay_ms", 100, 10000),
            ("synonyms_delay_ms", 100, 10000),
        ] {
            for boundary in [low, high] {
                let mut m = serde_json::Map::new();
                m.insert(name.into(), json!(boundary));
                assert!(from(Value::Object(m)).is_ok(), "{name}={boundary}");
            }
            for invalid in [low - 1, high + 1] {
                let mut m = serde_json::Map::new();
                m.insert(name.into(), json!(invalid));
                assert!(from(Value::Object(m)).is_err(), "{name}={invalid}");
            }
        }
    }
    #[test]
    fn enums_and_unknown_rule_fields_are_not_silently_reset() {
        for value in [
            json!({"backend":"cloud"}),
            json!({"completion_length":"unbounded"}),
            json!({"emoji_gender":"unknown"}),
            json!({"synonyms_mode":"always"}),
            json!({"punctuation_mode":"sometimes"}),
            json!({"app_rules":{"app:writer":{"record_history":1}}}),
            json!({"app_rules":{"app:writer":{"text":"never a draft"}}}),
            json!({"app_rules":{"app:code":{"complete_in_code_editors":"yes"}}}),
            json!({"app_rules":{"domain:https://private.example/path":{"enabled":false}}}),
            json!({"app_rules":{"app: ":{}}}),
            json!({"shortcuts":{"unknown":"A"}}),
            json!({"shortcuts":{"word":"Tab","all":"Tab"}}),
        ] {
            assert!(from(value.clone()).is_err(), "{value}");
        }
        let mut config = Config::default();
        config.battery_model = "removed".into();
        assert!(config.validate().is_err());
        let mut config = Config::default();
        config.model_id = "removed".into();
        assert!(config.validate().is_err());
    }
    #[test]
    fn unicode_lengths_are_scalars_and_rules_preserve_absence_vs_false() {
        let mut config = from(json!({"enabled":false,"global_enabled":true,"custom_instructions":"😀".repeat(8000),"excluded_apps":["設定".repeat(256)],"app_rules":{"app:Exact.App":{"enabled":false},"app:inherits":{},"domain:BÜCHER.example.":{"tab_enabled":false,"custom_instructions":""}}})).unwrap();
        assert!(!config.enabled && config.global_enabled);
        assert_eq!(config.app_rules["app:Exact.App"]["enabled"], false);
        assert!(!config.app_rules["app:inherits"].contains_key("enabled"));
        assert_eq!(
            config.app_rules["domain:xn--bcher-kva.example"]["custom_instructions"],
            ""
        );
        config.custom_instructions.push('a');
        assert!(config.validate().is_err());
        let mut too_many = Config::default();
        for n in 0..1001 {
            too_many
                .app_rules
                .insert(format!("app:{n}"), BTreeMap::new());
        }
        assert!(too_many.validate().is_err());
    }
    #[test]
    fn domain_nameprep_and_invalid_url_cases() {
        for (input, expected) in [
            (" BÜCHER.example. ", "xn--bcher-kva.example"),
            ("faß.de", "fass.de"),
            ("EXAMPLE.COM...", "example.com"),
            ("caf\u{e9}.example", "xn--caf-dma.example"),
            ("cafe\u{301}.example", "xn--caf-dma.example"),
            ("soft\u{ad}hyphen.example", "softhyphen.example"),
        ] {
            assert_eq!(normalize_domain(input).unwrap(), expected);
        }
        for input in [
            "",
            "   ",
            "https://example.com",
            "example.com/path",
            "a@b",
            "127.0.0.1:80",
            "a b",
            "example.com?x",
            "a..b",
        ] {
            assert!(normalize_domain(input).is_err(), "{input}");
        }
        assert!(normalize_domain(&format!("{}.example", "x".repeat(64))).is_err());
    }
    #[test]
    fn endpoint_refuses_remote_ambiguous_ports_and_authorities() {
        for input in [
            "https://example.com",
            "http://localhost:11434",
            "http://192.168.1.2:8080",
            "http://127.0.0.1.evil.example",
            "http://user:pass@127.0.0.1:8080",
            "http://@127.0.0.1",
            "file:///tmp/model",
            "http://127.0.0.1:8080?forward=remote",
            "http://127.0.0.1#fragment",
            "http://127.0.0.1:0",
            "http://127.0.0.1:65536",
            "http://127.0.0.1:",
            "http://127.1",
            "http://2130706433",
            "http://0x7f000001",
            "http://[::2]",
            "http://[::1]:-1",
            "http://[::1]:80:90",
            "http://[::1]evil",
            "http://127.0.0.1\\@remote",
            "http://127.0.0.1/\nremote",
        ] {
            assert!(local_endpoint(input).is_err(), "{input}");
        }
        assert!(local_endpoint("http://[127.0.0.1]").is_err());
        for input in [
            "http://127.0.0.1:11434/",
            "http://[::1]:8080/v1",
            "https://127.255.255.254:65535/path",
            "http://[0:0:0:0:0:0:0:1]/",
        ] {
            assert_eq!(local_endpoint(input).unwrap(), input.trim_end_matches('/'));
        }
        assert!(from(json!({"backend":"gguf","endpoint":"not active"})).is_ok());
        assert!(from(json!({"backend":"ollama","endpoint":"http://localhost"})).is_err());
        assert!(from(json!({"backend":"openai","model":"  "})).is_err());
    }
    #[test]
    fn legacy_migration_retains_saved_model_consent_and_schedule() {
        let preset = &crate::models::presets()[0];
        let config=from(json!({"keep_running":false,"unknown_draft":"not persisted","global_enabled":false,"enabled":true,"model_path":format!("/models/{}",preset.filename),"custom_instructions":"","battery_smaller_model":true,"debounce_ms":400,"idle_release_seconds":0,"screen_context":false,"clipboard_context":true})).unwrap();
        assert_eq!(config.model_id, preset.id);
        assert_eq!(config.battery_model, "tiny");
        assert!(config.custom_instructions.is_empty());
        assert!(
            !config.global_enabled
                && config.enabled
                && config.clipboard_context
                && !config.screen_context
        );
        assert_eq!((config.debounce_ms, config.idle_release_seconds), (400, 0));
        let serialized = serde_json::to_value(&config).unwrap();
        assert!(
            serialized.get("keep_running").is_none() && serialized.get("unknown_draft").is_none()
        );
        let saved=from(json!({"model_path":"/custom/own.gguf","custom_instructions":"Mine.","battery_model":"removed","model_id":"removed"})).unwrap();
        assert_eq!(saved.model_id, "");
        assert_eq!(saved.battery_model, "same");
        assert_eq!(saved.model_path, "/custom/own.gguf");
        assert_eq!(saved.custom_instructions, "Mine.");
        let remote =
            from(json!({"backend":"ollama","model_path":format!("/models/{}",preset.filename)}))
                .unwrap();
        assert!(remote.model_id.is_empty());
    }
    #[test]
    fn missing_profile_is_seeded_without_writing_and_existing_empty_is_not_seeded() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("missing/設定/settings.json");
        let config = load_config_from(&path).unwrap();
        assert!(!config.model_id.is_empty() && !config.custom_instructions.is_empty());
        assert!(!path.parent().unwrap().exists());
        assert!(from(json!({})).unwrap().custom_instructions.is_empty());
        for bytes in [b"null".as_slice(), b"[]", b"{", b"\xff"] {
            std::fs::write(temp.path().join("bad"), bytes).unwrap();
            assert!(load_config_from(&temp.path().join("bad")).is_err());
        }
        assert!(load_config_from(temp.path()).is_err());
    }
    #[test]
    fn atomic_save_normalizes_a_copy_and_preserves_previous_file_on_validation_error() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("João/設定/settings.json");
        let mut config = Config::default();
        config
            .shortcuts
            .insert("force".into(), "Control+AboveTab".into());
        config.app_rules.insert(
            "domain:EXAMPLE.COM.".into(),
            BTreeMap::from([("enabled".into(), json!(false))]),
        );
        save_config_to(&config, &path).unwrap();
        let loaded = load_config_from(&path).unwrap();
        assert!(loaded.app_rules.contains_key("domain:example.com"));
        assert_eq!(loaded.shortcuts["force"], "Ctrl+AboveTab");
        assert!(config.app_rules.contains_key("domain:EXAMPLE.COM."));
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        let bytes = std::fs::read(&path).unwrap();
        config.max_tokens = 0;
        assert!(save_config_to(&config, &path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn provider_identity_ignores_request_only_and_inactive_fields() {
        let base = Config::default();
        let mut edited = base.clone();
        edited.prompt_style = "writing".into();
        edited.max_tokens = 64;
        edited.endpoint = "http://127.0.0.1:12345".into();
        assert_eq!(base.provider_identity(), edited.provider_identity());
        edited.model_path = "/models/other.gguf".into();
        assert_ne!(base.provider_identity(), edited.provider_identity());
        let mut remote = base.clone();
        remote.backend = "ollama".into();
        let mut edited = remote.clone();
        edited.acceleration = "cpu".into();
        edited.model_path = "unused".into();
        assert_eq!(remote.provider_identity(), edited.provider_identity());
        edited.model = "another".into();
        assert_ne!(remote.provider_identity(), edited.provider_identity());
    }
    #[test]
    fn locale_seeds_known_language_without_permanent_english_fallback() {
        for (locale, name) in [
            ("pt_BR", "Portuguese"),
            ("de-DE", "German"),
            ("zh-Hant-TW", "Chinese"),
            ("nb_NO", "Norwegian Bokmål"),
            ("ja", "Japanese"),
        ] {
            let instructions = default_instructions(Some(locale));
            assert!(
                instructions.starts_with(&format!("Write in {name} by default")),
                "{instructions}"
            );
            assert!(instructions.ends_with(
                "If the surrounding text is in another language, continue in that language."
            ));
        }
        #[cfg(target_os = "macos")]
        {
            let instructions = default_instructions(Some("pt_BR"));
            assert!(instructions.contains("Brazil"));
            assert!(instructions.contains("dates, numbers and currency"));
        }
        assert!(
            default_instructions(Some("123_invalid")).starts_with("Write in English by default")
        );
    }
}
