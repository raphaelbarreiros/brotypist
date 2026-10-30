//! Immutable pre-read policy. Build on configuration changes; keyboard dispatch allocates no chords.
use super::Action;
use crate::{
    config::Config,
    shortcuts::{Chord, parse_shortcut},
};
use serde_json::Value;
use std::collections::BTreeMap;
use unicode_casefold::UnicodeCaseFold;

type Rule = BTreeMap<String, Value>;
const CHROMIUM: &[&str] = &[
    "com.google.chrome",
    "com.google.chrome.beta",
    "com.google.chrome.dev",
    "com.google.chrome.canary",
    "org.chromium.chromium",
    "com.microsoft.edgemac",
    "com.microsoft.edgemac.beta",
    "com.microsoft.edgemac.dev",
    "com.microsoft.edgemac.canary",
    "com.brave.browser",
    "com.brave.browser.beta",
    "com.brave.browser.nightly",
    "com.operasoftware.opera",
    "com.operasoftware.operagx",
    "com.vivaldi.vivaldi",
    "company.thebrowser.browser",
    "company.thebrowser.dia",
];
const OTHER_BROWSERS: &[&str] = &[
    "com.apple.safari",
    "com.apple.safaritechnologypreview",
    "org.mozilla.firefox",
    "org.mozilla.firefoxdeveloperedition",
    "org.mozilla.nightly",
    "app.zen-browser.zen",
    "com.kagi.kagimacos",
    "com.kagi.kagimacos.rc",
    "at.studio.asidebrowser",
];
const BROWSER_NAMES: &[&str] = &[
    "safari",
    "safari technology preview",
    "chrome",
    "chromium",
    "chromium-browser",
    "google-chrome",
    "google-chrome-stable",
    "firefox",
    "firefox-esr",
    "msedge",
    "microsoft-edge",
    "microsoft-edge-stable",
    "brave",
    "brave-browser",
    "opera",
    "vivaldi",
    "vivaldi-bin",
    "iexplore",
    "zen-browser",
];
const CODE_EDITORS: &[&str] = &[
    "com.microsoft.vscode",
    "com.microsoft.vscodeinsiders",
    "com.vscodium",
    "com.todesktop.230313mzl4w4u92",
    "com.exafunction.windsurf",
    "com.apple.dt.xcode",
    "dev.zed.zed",
    "dev.zed.zed-preview",
    "com.sublimetext.3",
    "com.sublimetext.4",
    "com.panic.nova",
    "com.barebones.bbedit",
    "com.macromates.textmate",
    "org.vim.macvim",
    "com.google.android.studio",
];
fn fold(value: &str) -> String {
    value.case_fold().collect()
}
pub fn is_chromium_bundle(value: &str) -> bool {
    CHROMIUM.iter().any(|x| value.eq_ignore_ascii_case(x))
}
pub fn is_code_editor(value: &str) -> bool {
    CODE_EDITORS.iter().any(|x| value.eq_ignore_ascii_case(x))
        || value
            .get(..14)
            .is_some_and(|p| p.eq_ignore_ascii_case("com.jetbrains."))
}
pub fn is_browser(identifiers: &[&str]) -> bool {
    identifiers.iter().any(|value| {
        let name = value.rsplit(['/', '\\']).next().unwrap_or(value);
        let executable_suffix = name.len().checked_sub(4).filter(|start| {
            name.get(*start..)
                .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".exe"))
        });
        let name = executable_suffix.map_or(name, |start| &name[..start]);
        CHROMIUM
            .iter()
            .chain(OTHER_BROWSERS)
            .any(|x| value.eq_ignore_ascii_case(x))
            || BROWSER_NAMES.iter().any(|x| name.eq_ignore_ascii_case(x))
    })
}
pub fn native_domain(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    // Preserve Python's IDNA/nameprep contract. Url::host_str already applies UTS46,
    // which would change identities such as faß.example before normalize_domain.
    let authority = value
        .get(value.find("://")? + 3..)?
        .split(['/', '?', '#'])
        .next()?;
    let host_port = authority.rsplit('@').next()?;
    let host = if host_port.starts_with('[') {
        host_port.get(1..host_port.find(']')?)?
    } else {
        host_port.split(':').next()?
    };
    crate::config::normalize_domain(host).ok()
}

#[derive(Clone, Debug)]
pub struct ObservationPolicy {
    pub global_enabled: bool,
    pub default_enabled: bool,
    exclusions: Vec<String>,
    apps: BTreeMap<String, Rule>,
    domains: BTreeMap<String, Rule>,
    bindings: Vec<(Chord, Action)>,
    denied_domains: bool,
}
impl ObservationPolicy {
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        let mut apps = BTreeMap::new();
        let mut domains = BTreeMap::new();
        for (key, value) in &config.app_rules {
            if let Some(app) = key.strip_prefix("app:") {
                apps.insert(app.into(), value.clone());
            }
            if let Some(domain) = key.strip_prefix("domain:") {
                domains.insert(domain.into(), value.clone());
            }
        }
        let denied_domains = domains
            .values()
            .any(|r| r.get("enabled") == Some(&Value::Bool(false)));
        let mut bindings = Vec::new();
        for (name, binding) in &config.shortcuts {
            if binding.is_empty() {
                continue;
            }
            let action = match name.as_str() {
                "word" => Action::Word,
                "all" => Action::All,
                "force" => Action::Force,
                "app_toggle" => Action::AppToggle,
                "toggle" => Action::Toggle,
                "alternatives" => Action::Alternatives,
                "synonyms" => Action::Synonyms,
                _ => continue,
            };
            bindings.push((parse_shortcut(binding)?, action));
        }
        Ok(Self {
            global_enabled: config.global_enabled,
            default_enabled: config.enabled,
            exclusions: config
                .excluded_apps
                .iter()
                .filter(|s| !s.is_empty())
                .map(|s| fold(s))
                .collect(),
            apps,
            domains,
            bindings,
            denied_domains,
        })
    }
    pub fn rule_bool(&self, app: &str, domain: &str, name: &str, default: bool) -> bool {
        let app_value = self
            .apps
            .get(app)
            .and_then(|r| r.get(name))
            .and_then(Value::as_bool)
            .unwrap_or(default);
        self.domains
            .get(domain)
            .and_then(|r| r.get(name))
            .and_then(Value::as_bool)
            .unwrap_or(app_value)
    }
    pub fn excluded(&self, identifiers: &[&str]) -> bool {
        identifiers.iter().any(|value| {
            let value = fold(value);
            self.exclusions
                .iter()
                .any(|exclusion| value.contains(exclusion))
        })
    }
    pub fn allowed(&self, app: &str, domain: &str, names: &[&str], editor_probe: bool) -> bool {
        if !self.global_enabled || self.excluded(&[app]) || self.excluded(names) {
            return false;
        }
        if domain.is_empty() && self.denied_domains && (is_browser(&[app]) || is_browser(names)) {
            return false;
        }
        if is_code_editor(app)
            && !editor_probe
            && self
                .apps
                .get(app)
                .and_then(|r| r.get("complete_in_code_editors"))
                .and_then(Value::as_bool)
                != Some(true)
        {
            return false;
        }
        self.rule_bool(app, domain, "enabled", self.default_enabled)
    }
    /// Modifiers are normalized before publication; matching does not allocate on the hook path.
    pub fn key_action(
        &self,
        key: &str,
        modifiers: &[&str],
        platform: &str,
        app: &str,
        domain: &str,
    ) -> Option<Action> {
        if platform == "macos" && key == "Tab" && modifiers == ["Alt"] {
            return Some(Action::Bypass);
        }
        if key == "Tab" && !self.rule_bool(app, domain, "tab_enabled", true) {
            return None;
        }
        for (binding, action) in &self.bindings {
            if binding.key == key
                && binding.modifiers.len() == modifiers.len()
                && binding.modifiers.iter().zip(modifiers).all(|(a, b)| a == b)
            {
                return Some(action.clone());
            }
        }
        if key == "Escape" && modifiers.is_empty() {
            return Some(Action::Dismiss);
        }
        if modifiers.is_empty() && key.len() == 1 {
            let digit = key.as_bytes()[0];
            if (b'1'..=b'9').contains(&digit) {
                return Some(Action::Choice((digit - b'1') as usize));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_browser_domain_blocks_before_read_when_site_denied() {
        let mut config = Config::default();
        config.app_rules.insert(
            "domain:private.example".into(),
            BTreeMap::from([("enabled".into(), Value::Bool(false))]),
        );
        let policy = ObservationPolicy::new(&config).unwrap();
        assert!(!policy.allowed("com.google.Chrome", "", &[], false));
        assert!(policy.allowed("com.google.Chrome", "public.example", &[], false));
        assert!(!policy.allowed("com.google.Chrome", "private.example", &[], false));
        assert!(policy.allowed("ordinary.editor", "", &[], false));
    }
    #[test]
    fn explicit_editor_opt_in_and_exclusion_precedence() {
        let mut config = Config::default();
        let policy = ObservationPolicy::new(&config).unwrap();
        assert!(!policy.allowed("com.microsoft.VSCode", "", &[], false));
        config.app_rules.insert(
            "app:com.microsoft.VSCode".into(),
            BTreeMap::from([
                ("complete_in_code_editors".into(), Value::Bool(true)),
                ("enabled".into(), Value::Bool(true)),
            ]),
        );
        assert!(ObservationPolicy::new(&config).unwrap().allowed(
            "com.microsoft.VSCode",
            "",
            &[],
            false
        ));
        config.excluded_apps.push("vscode".into());
        assert!(!ObservationPolicy::new(&config).unwrap().allowed(
            "com.microsoft.VSCode",
            "",
            &[],
            false
        ));
    }
    #[test]
    fn native_host_rules_keep_python_idna_and_mixed_case_executable_identity() {
        assert_eq!(
            native_domain("https://faß.example/path?q=1"),
            Some("fass.example".into())
        );
        assert!(is_browser(&["C:\\Program Files\\Google\\Chrome.Exe"]));
        assert!(!is_browser(&["unrelated-arc.exe"]));
        assert_eq!(native_domain("file:///private/path"), None);
    }
    #[test]
    fn physical_shortcuts_do_not_override_disabled_tab_or_injected_modifiers() {
        let mut config = Config::default();
        config.app_rules.insert(
            "app:editor".into(),
            BTreeMap::from([("tab_enabled".into(), Value::Bool(false))]),
        );
        let policy = ObservationPolicy::new(&config).unwrap();
        assert_eq!(policy.key_action("Tab", &[], "macos", "editor", ""), None);
        assert_eq!(
            policy.key_action("Tab", &["Alt"], "macos", "editor", ""),
            Some(Action::Bypass)
        );
        assert_eq!(
            policy.key_action("AboveTab", &["Ctrl"], "macos", "editor", ""),
            Some(Action::Force)
        );
        assert_eq!(
            policy.key_action("AboveTab", &["Ctrl", "Alt"], "macos", "editor", ""),
            None
        );
    }
}
