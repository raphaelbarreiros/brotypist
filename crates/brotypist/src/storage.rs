//! Compatible encrypted history and separate, plaintext aggregate statistics.
//!
//! Construction never opens SQLite or consults credentials. Only explicit
//! `unlock` (or a consented one-operation unlock) can load a history key.
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::Duration;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Days, Local, NaiveDate, Timelike, Utc};
use rand::{RngCore, rngs::OsRng};
use regex::Regex;
use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use unicode_general_category::{GeneralCategory, get_general_category};
use unicode_normalization::UnicodeNormalization;
use zeroize::Zeroizing;

use crate::config::normalize_domain;
use crate::paths;
use crate::unicode_compat::UnicodeCaseFold;

const AAD: &[u8] = b"inklow-history-v1";
const MAX_AUTHORSHIP_SPANS: usize = 1024;
const LEARNING_ENTRIES: usize = 200;
const AUTHENTICATION_ERROR: &str = "Encrypted history could not be authenticated. Recording/personalization stopped; clear history to remove damaged records.";
const UNAVAILABLE_ERROR: &str = "Encrypted history is unavailable. Unlock your OS credential store; plaintext recording is never used.";
const LOCKED_ERROR: &str =
    "Encrypted history is locked. Confirm this action to unlock it from OS secure storage.";

static SENSITIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
    r"(?i)(?:password|passwd|api[_ -]?key|access[_ -]?token|secret|one[ -]time|verification[ -]code)[\s\u{001c}-\u{001f}]*[:=]|-----BEGIN [A-Z ]*PRIVATE KEY-----"
).expect("constant sensitive-pattern expression")
});
static SENSITIVE_CARD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:\p{Nd}[ -]?){13,19}").expect("constant payment-card expression")
});
static SENSITIVE_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:sk|ghp|github_pat)[_-][A-Za-z0-9_-]{16,}")
        .expect("constant credential-token expression")
});
// Python's Unicode \w includes letters/numbers/underscore, but not combining
// marks. Rust regex's \w is broader, so spell these classes out explicitly.
static TOKENS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\p{L}\p{Nl}\p{No}]+(?:['’][\p{L}\p{Nl}\p{No}]+)*")
        .expect("constant token expression")
});
static WORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\p{L}\p{Nl}\p{No}][\p{L}\p{N}_’'-]{3,31}")
        .expect("constant vocabulary expression")
});
static ACCEPTED_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\p{L}\p{N}]+(?:[’'-][\p{L}\p{N}]+)*").expect("constant acceptance expression")
});
static SENTENCES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^.!?\n]+[.!?]+").expect("constant sentence expression"));
const LANGUAGES: &[(&str, &str)] = &[
    (
        "en",
        "the and is are we our with for before during which this that don't doesn't can't won't",
    ),
    (
        "pt",
        "de da do das dos e é são nós nossa com para durante reunião próxima mais uma",
    ),
    (
        "es",
        "el los las y es son nosotros nuestra con para durante reunión próxima más una",
    ),
    (
        "fr",
        "le les des et est sont nous notre avec pour pendant réunion prochaine plus une",
    ),
    (
        "de",
        "der die das und ist sind wir unser mit für während nächste mehr eine",
    ),
];
static LANGUAGE_WORDS: LazyLock<Vec<(&'static str, Vec<&'static str>)>> = LazyLock::new(|| {
    LANGUAGES
        .iter()
        .map(|(language, words)| (*language, words.split_whitespace().collect()))
        .collect()
});

/// UI/runtime record. Wire payloads intentionally use `app`, `time` and
/// `authorship: 1`, not these convenience field names (nor an embedded id).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryRecord {
    pub id: String,
    pub app_id: String,
    pub domain: String,
    pub text: String,
    pub created_at: String,
    pub authored_spans: Vec<(usize, usize)>,
}

/// Allowlisted native backends implement this narrow interface. Tests can inject
/// a wholly private backend; no global keyring backend is ever used or changed.
pub trait HistoryCredentials: Send + Sync {
    fn get_password(&self, service: &str, account: &str) -> Result<Option<String>>;
    fn set_password(&self, service: &str, account: &str, password: &str) -> Result<()>;
}

type KeyLoader = Box<dyn Fn() -> Result<[u8; 32]> + Send + Sync>;
enum KeySource {
    Loader(KeyLoader),
    Credentials(Box<dyn HistoryCredentials>),
}

#[derive(Default)]
struct HistoryState {
    key: Option<Zeroizing<[u8; 32]>>,
    broken: bool,
    error: String,
    learning: Option<Vec<LearningEntry>>,
}

pub struct HistoryStore {
    path: PathBuf,
    key_source: KeySource,
    state: Mutex<HistoryState>,
}

impl HistoryStore {
    pub fn open() -> Result<Self> {
        Ok(Self::with_credentials(
            paths::config_dir()?.join("local.sqlite3"),
            Box::new(OsCredentials::default()),
        ))
    }

    /// Inject a private database path and key; remains locked until `unlock`.
    pub fn with_path_and_key(path: impl Into<PathBuf>, key: [u8; 32]) -> Self {
        let key = Zeroizing::new(key);
        Self::with_key_loader(path, Box::new(move || Ok(*key)))
    }

    pub fn with_key_loader(path: impl Into<PathBuf>, loader: KeyLoader) -> Self {
        Self {
            path: path.into(),
            key_source: KeySource::Loader(loader),
            state: Mutex::default(),
        }
    }

    /// This constructor also permits isolated lost-key/backend refusal tests.
    pub fn with_credentials(
        path: impl Into<PathBuf>,
        credentials: Box<dyn HistoryCredentials>,
    ) -> Self {
        Self {
            path: path.into(),
            key_source: KeySource::Credentials(credentials),
            state: Mutex::default(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn state(&self) -> Result<MutexGuard<'_, HistoryState>> {
        self.state
            .lock()
            .map_err(|_| anyhow!("History state lock is unavailable."))
    }

    pub fn unlock(&mut self) -> Result<()> {
        let mut state = self.state()?;
        self.unlock_state(&mut state)
    }

    fn unlock_state(&self, state: &mut HistoryState) -> Result<()> {
        ensure!(!state.broken, "{}", state.error);
        if state.key.is_some() {
            return Ok(());
        }
        let result = match &self.key_source {
            KeySource::Loader(loader) => loader().map(Zeroizing::new),
            KeySource::Credentials(credentials) => {
                load_history_key(&self.path, credentials.as_ref())
            }
        };
        match result {
            Ok(key) => {
                state.key = Some(key);
                state.error.clear();
                Ok(())
            }
            Err(_) => {
                state.key = None;
                state.learning = None;
                state.error = UNAVAILABLE_ERROR.to_owned();
                // Credential/native errors can contain secrets. Keep the public
                // error fixed, including its chain, rather than interpolating it.
                bail!(UNAVAILABLE_ERROR)
            }
        }
    }

    pub fn lock(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.key = None;
            state.learning = None;
        }
    }

    pub fn is_unlocked(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.key.is_some())
            .unwrap_or(false)
    }

    /// Call only after field eligibility/recording consent. Unlock itself is an
    /// explicit consent boundary; this never loads a key for a locked store.
    pub fn record(
        &mut self,
        app: &str,
        domain: &str,
        text: &str,
        authored_spans: &[(usize, usize)],
    ) -> Result<bool> {
        if !self.is_unlocked() {
            return Ok(false);
        }
        let mut identifier = [0u8; 16];
        OsRng
            .try_fill_bytes(&mut identifier)
            .map_err(|_| anyhow!("History randomness unavailable."))?;
        let identifier: String = identifier
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        self.record_identified_at(&identifier, app, domain, text, authored_spans, Utc::now())
    }

    /// Stable session ids replace an earlier snapshot rather than double-counting
    /// its authored evidence. Times are UTC ISO 8601 as in Python's payload.
    pub fn record_identified_at(
        &mut self,
        id: &str,
        app: &str,
        domain: &str,
        text: &str,
        authored_spans: &[(usize, usize)],
        now: DateTime<Utc>,
    ) -> Result<bool> {
        if !history_safe(text) {
            return Ok(false);
        }
        let Ok(domain) = scope_domain(domain) else {
            return Ok(false);
        };
        let mut state = self.state()?;
        let Some(key) = state.key.as_ref() else {
            return Ok(false);
        };
        let spans = if authored_fragments(text, authored_spans).is_empty() {
            &[][..]
        } else {
            authored_spans
        };
        let payload = Zeroizing::new(serde_json::to_vec(&json!({
            "app": scalar_prefix(app, 512), "domain": domain, "text": text,
            "authorship": 1, "authored_spans": spans,
            "time": python_timestamp(now),
        }))?);
        let sealed = seal_record(key, id, &payload)?;
        self.connect()?.execute(
            "INSERT OR REPLACE INTO history VALUES (?,?)",
            params![id, sealed],
        )?;
        state.learning = None;
        Ok(true)
    }

    pub fn records(
        &self,
        app: Option<&str>,
        domain: Option<&str>,
        limit: usize,
    ) -> Result<Vec<HistoryRecord>> {
        let domain = domain.map(scope_domain).transpose()?;
        let mut state = self.state()?;
        // Authenticate all rows before filtering: corruption must not be hidden
        // by an unrelated current filter. limit applies to matching rows.
        let entries = self.entries(&mut state, None)?;
        Ok(entries
            .into_iter()
            .filter(|entry| {
                app.is_none_or(|app| entry.record.app_id == app)
                    && domain
                        .as_ref()
                        .is_none_or(|domain| entry.record.domain == *domain)
            })
            .take(limit)
            .map(|entry| entry.record)
            .collect())
    }

    fn entries(&self, state: &mut HistoryState, limit: Option<usize>) -> Result<Vec<Entry>> {
        let key = state.key.as_ref().ok_or_else(|| anyhow!(LOCKED_ERROR))?;
        if !self.path.try_exists()? {
            return Ok(Vec::new());
        }
        let connection = self.connect()?;
        let mut query =
            connection.prepare("SELECT id,sealed FROM history ORDER BY rowid DESC LIMIT ?")?;
        let rows = query.query_map(
            [limit
                .map(|n| i64::try_from(n).unwrap_or(i64::MAX))
                .unwrap_or(-1)],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )?;
        let cipher =
            Aes256Gcm::new_from_slice(key.as_ref()).map_err(|_| anyhow!(UNAVAILABLE_ERROR))?;
        let result = (|| -> Result<Vec<Entry>> {
            let mut entries = Vec::new();
            for row in rows {
                let (id, sealed) = row?;
                let plaintext = Zeroizing::new(open_with_cipher(&cipher, &id, &sealed)?);
                let value: Value = serde_json::from_slice(&plaintext)?;
                let object = value
                    .as_object()
                    .ok_or_else(|| anyhow!("Invalid history record."))?;
                let text = object
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("Invalid history text."))?;
                let app = optional_string(object.get("app"))?;
                let domain = optional_string(object.get("domain"))?;
                let spans = parse_spans(object.get("authored_spans"));
                entries.push(Entry {
                    record: HistoryRecord {
                        id,
                        app_id: app.to_owned(),
                        domain: domain.to_owned(),
                        text: text.to_owned(),
                        created_at: object
                            .get("time")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        authored_spans: spans,
                    },
                    authored: object.get("authorship").and_then(Value::as_u64) == Some(1),
                });
            }
            Ok(entries)
        })();
        match result {
            Ok(entries) => Ok(entries),
            Err(_) => {
                state.key = None;
                state.learning = None;
                state.broken = true;
                state.error = AUTHENTICATION_ERROR.to_owned();
                bail!(AUTHENTICATION_ERROR)
            }
        }
    }

    pub fn delete(&mut self, app: Option<&str>, domain: Option<&str>) -> Result<usize> {
        self.delete_with_unlock(app, domain, false)
    }

    /// Full deletion needs no key. Scoped deletion may explicitly unlock for
    /// this operation, and relocks even if authentication or deletion fails.
    pub fn delete_with_unlock(
        &mut self,
        app: Option<&str>,
        domain: Option<&str>,
        unlock: bool,
    ) -> Result<usize> {
        ensure!(
            app.is_none() || domain.is_none(),
            "Delete history for one app or one domain, not both."
        );
        let domain = domain.map(normalize_domain).transpose()?;
        let mut state = self.state()?;
        state.learning = None;
        if !self.path.try_exists()? {
            if app.is_none() && domain.is_none() {
                state.broken = false;
                state.error.clear();
            }
            return Ok(0);
        }
        let deleted = if app.is_none() && domain.is_none() {
            let count = self.connect()?.execute("DELETE FROM history", [])?;
            state.broken = false;
            state.error.clear();
            count
        } else {
            self.temporarily_unlocked(&mut state, unlock, |state| {
                let identifiers: Vec<String> = self
                    .entries(state, None)?
                    .into_iter()
                    .filter(|entry| {
                        if let Some(domain) = &domain {
                            entry.record.domain == *domain
                        } else {
                            Some(entry.record.app_id.as_str()) == app
                        }
                    })
                    .map(|entry| entry.record.id)
                    .collect();
                let mut connection = self.connect()?;
                let transaction = connection.transaction()?;
                let mut count = 0;
                for identifier in identifiers {
                    count += transaction.execute("DELETE FROM history WHERE id=?", [identifier])?;
                }
                transaction.commit()?;
                Ok(count)
            })?
        };
        self.connect()?.execute_batch("VACUUM")?;
        state.learning = None;
        Ok(deleted)
    }

    fn temporarily_unlocked<T>(
        &self,
        state: &mut HistoryState,
        unlock: bool,
        action: impl FnOnce(&mut HistoryState) -> Result<T>,
    ) -> Result<T> {
        let temporary = state.key.is_none();
        if temporary {
            ensure!(unlock, LOCKED_ERROR);
            self.unlock_state(state)?;
        }
        let result = action(state);
        if temporary {
            state.key = None;
            state.learning = None;
        }
        result
    }

    pub fn history_counts(&self, unlock: bool) -> Result<Value> {
        let mut state = self.state()?;
        if !self.path.try_exists()? {
            return Ok(json!({"available": true, "apps": {}, "domains": {}}));
        }
        if state.key.is_none() && !unlock {
            return Ok(json!({"available": false, "apps": {}, "domains": {}}));
        }
        self.temporarily_unlocked(&mut state, unlock, |state| {
            let mut apps = BTreeMap::<String, u64>::new();
            let mut domains = BTreeMap::<String, u64>::new();
            for entry in self.entries(state, None)? {
                *apps.entry(entry.record.app_id).or_default() += 1;
                if !entry.record.domain.is_empty() {
                    *domains.entry(entry.record.domain).or_default() += 1;
                }
            }
            Ok(json!({"available": true, "apps": apps, "domains": domains}))
        })
    }

    /// Ciphertext metadata only; never unlocks or decrypts writing.
    pub fn history_info(&self) -> Result<Value> {
        let state = self.state()?;
        let (count, bytes) = if self.path.try_exists()? {
            self.connect()?.query_row(
                "SELECT count(*),coalesce(sum(length(sealed)),0) FROM history",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )?
        } else {
            (0, 0)
        };
        Ok(
            json!({"count": count, "bytes": bytes, "available": state.key.is_some(), "error": state.error}),
        )
    }

    /// `source_allowed` is the current controller policy, evaluated both for
    /// this request and each stored source. It must include consent/exclusions.
    /// Website evidence is exact-host; domainless evidence is app-local.
    pub fn personalization(
        &self,
        app: &str,
        domain: &str,
        prefix: &str,
        strength: u32,
        source_allowed: &dyn Fn(&str, &str) -> bool,
    ) -> Result<String> {
        if strength == 0 {
            return Ok(String::new());
        }
        let Ok(domain) = scope_domain(domain) else {
            return Ok(String::new());
        };
        let language = writing_language(prefix);
        if language.is_empty() || !source_allowed(app, &domain) {
            return Ok(String::new());
        }
        let mut state = self.state()?;
        if state.key.is_none() {
            return Ok(String::new());
        }
        if state.learning.is_none() {
            let mut index = Vec::new();
            for entry in self.entries(&mut state, Some(LEARNING_ENTRIES))? {
                if !entry.authored {
                    continue;
                }
                let fragments: Vec<String> =
                    authored_fragments(&entry.record.text, &entry.record.authored_spans)
                        .iter()
                        .map(|fragment| fragment.nfc().collect())
                        .collect();
                let authored = Zeroizing::new(fragments.join("\n"));
                let entry_language = writing_language(&authored);
                if !history_safe(&authored) || entry_language.is_empty() {
                    continue;
                }
                let (vocabulary, features) = style_features(&fragments);
                index.push(LearningEntry {
                    app: entry.record.app_id,
                    domain: entry.record.domain,
                    language: entry_language,
                    vocabulary,
                    features,
                });
            }
            state.learning = Some(index);
        }
        let mut words = BTreeMap::<String, u64>::new();
        let mut totals = Features::default();
        let mut records = 0;
        for entry in state.learning.as_ref().expect("initialized learning index") {
            let same_context = if domain.is_empty() {
                entry.domain.is_empty() && entry.app == app
            } else {
                entry.domain == domain
            };
            if !same_context
                || entry.language != language
                || !source_allowed(&entry.app, &entry.domain)
            {
                continue;
            }
            records += 1;
            for (word, count) in &entry.vocabulary {
                *words.entry(word.clone()).or_default() += count;
            }
            totals.add(&entry.features);
        }
        if records == 0 || totals.words == 0 {
            return Ok(String::new());
        }
        let mut style = BTreeMap::<&str, f64>::new();
        if totals.sentences > 0 {
            style.insert(
                "mean_sentence_words",
                rounded(totals.sentence_words as f64 / totals.sentences as f64, 1),
            );
            style.insert(
                "lowercase_sentence_start_ratio",
                rounded(totals.lowercase_starts as f64 / totals.sentences as f64, 2),
            );
        }
        for (name, count) in [
            ("commas_per_100_words", totals.commas),
            ("exclamations_per_100_words", totals.exclamations),
            ("questions_per_100_words", totals.questions),
            ("contractions_per_100_words", totals.contractions),
        ] {
            style.insert(name, rounded(100.0 * count as f64 / totals.words as f64, 1));
        }
        let mut ranked: Vec<_> = words.into_iter().collect();
        ranked.sort_by(|(a, ac), (b, bc)| bc.cmp(ac).then_with(|| a.cmp(b)));
        let cap = ((strength as f64 * 0.24).round_ties_even() as usize).clamp(1, 24);
        let vocabulary: Vec<_> = ranked
            .into_iter()
            .filter(|(_, frequency)| *frequency >= 2)
            .take(cap)
            .map(|(word, _)| word)
            .collect();
        Ok(serde_json::to_string(
            &json!({"strength": strength.min(100), "language": language,
                                      "records": records, "style": style, "vocabulary": vocabulary}),
        )?)
    }

    pub fn record_statistics(&mut self, app: &str, text: &str, new_completion: bool) -> Result<()> {
        self.record_statistics_at(app, text, new_completion, Local::now().date_naive())
    }

    pub fn record_statistics_at(
        &mut self,
        app: &str,
        text: &str,
        new_completion: bool,
        day: NaiveDate,
    ) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let _state = self.state()?;
        let words = i64::try_from(ACCEPTED_WORDS.find_iter(text).count())?;
        let characters = i64::try_from(text.chars().count())?;
        self.connect()?.execute(
            "INSERT INTO statistics VALUES (?,?,?,?,?) ON CONFLICT(day,app) DO UPDATE SET completions=completions+excluded.completions, words=words+excluded.words, characters=characters+excluded.characters",
            params![day.to_string(), scalar_prefix(app, 512), i64::from(new_completion), words, characters],
        )?;
        Ok(())
    }

    /// Rolling days including today; zero requests all history (future dates are
    /// still excluded). Grouping is exactly `day`, `app`, or `month`.
    pub fn statistics(&self, days: u32, grouping: &str) -> Result<Value> {
        self.statistics_at(days, grouping, Local::now().date_naive())
    }

    pub fn statistics_period_at(
        &self,
        period: &str,
        grouping: &str,
        today: NaiveDate,
    ) -> Result<Value> {
        let days = match period {
            "today" | "day" => 1,
            "week" => 7,
            "month" => 30,
            "year" => 365,
            "all" => 0,
            _ => bail!("Statistics period must be today, week, month, year, or all."),
        };
        self.statistics_at(days, grouping, today)
    }

    pub fn statistics_at(&self, days: u32, grouping: &str, today: NaiveDate) -> Result<Value> {
        let expression = match grouping {
            "day" => "day",
            "app" => "app",
            "month" => "substr(day,1,7)",
            _ => bail!("Statistics grouping must be day, app, or month."),
        };
        let start = if days == 0 {
            "0000".to_owned()
        } else {
            today
                .checked_sub_days(Days::new(u64::from(days - 1)))
                .ok_or_else(|| anyhow!("Statistics period out of range."))?
                .to_string()
        };
        let _state = self.state()?;
        let mut rows = Vec::new();
        let mut totals = [0i64; 3];
        let mut current = [0i64; 3];
        let mut active_days = 0i64;
        if self.path.try_exists()? {
            let connection = self.connect()?;
            let sql = format!(
                "SELECT {expression},sum(completions),sum(words),sum(characters) FROM statistics WHERE day>=? AND day<=? GROUP BY {expression} ORDER BY {expression}"
            );
            let mut statement = connection.prepare(&sql)?;
            let result = statement.query_map(params![start, today.to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    [
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ],
                ))
            })?;
            for row in result {
                let (label, values) = row?;
                for (total, value) in totals.iter_mut().zip(values) {
                    *total += value;
                }
                rows.push(json!({"label": label, "completions": values[0], "words": values[1], "characters": values[2],
                                 "estimated_seconds": (values[2] as f64 / 5.0).round_ties_even() as i64}));
            }
            current = connection.query_row("SELECT coalesce(sum(completions),0),coalesce(sum(words),0),coalesce(sum(characters),0) FROM statistics WHERE day=?", [today.to_string()], |row|
                Ok([row.get(0)?, row.get(1)?, row.get(2)?]))?;
            active_days = connection.query_row(
                "SELECT count(DISTINCT day) FROM statistics WHERE day>=? AND day<=?",
                params![start, today.to_string()],
                |row| row.get(0),
            )?;
        }
        let estimated = (totals[2] as f64 / 5.0).round_ties_even() as i64;
        let average = |value: i64| {
            if active_days == 0 {
                0.0
            } else {
                rounded(value as f64 / active_days as f64, 1)
            }
        };
        Ok(
            json!({"totals": counters(totals), "rows": rows, "today": counters(current), "estimated_seconds": estimated,
                  "active_days": active_days, "daily_average": {"completions": average(totals[0]), "words": average(totals[1]),
                  "characters": average(totals[2]), "estimated_seconds": average(estimated)}}),
        )
    }

    pub fn clear_statistics(&mut self) -> Result<()> {
        let _state = self.state()?;
        if self.path.try_exists()? {
            self.connect()?.execute("DELETE FROM statistics", [])?;
        }
        Ok(())
    }

    fn connect(&self) -> Result<Connection> {
        let parent = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut directory = fs::DirBuilder::new();
        directory.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            directory.mode(0o700);
        }
        directory.create(parent)?;
        // Create privately before SQLite opens it; don't leave a permissive
        // creation window even with a permissive process umask.
        let mut options = fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        drop(options.open(&self.path)?);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
        }
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(Duration::from_secs(2))?;
        connection.execute_batch("PRAGMA secure_delete=ON; CREATE TABLE IF NOT EXISTS history (id TEXT PRIMARY KEY, sealed BLOB NOT NULL); CREATE TABLE IF NOT EXISTS statistics (day TEXT, app TEXT, completions INTEGER, words INTEGER, characters INTEGER, PRIMARY KEY(day,app));")?;
        Ok(connection)
    }
}

struct Entry {
    record: HistoryRecord,
    authored: bool,
}
struct LearningEntry {
    app: String,
    domain: String,
    language: String,
    vocabulary: BTreeMap<String, u64>,
    features: Features,
}

#[derive(Default)]
struct Features {
    words: u64,
    commas: u64,
    exclamations: u64,
    questions: u64,
    contractions: u64,
    sentences: u64,
    sentence_words: u64,
    lowercase_starts: u64,
}
impl Features {
    fn add(&mut self, other: &Self) {
        self.words += other.words;
        self.commas += other.commas;
        self.exclamations += other.exclamations;
        self.questions += other.questions;
        self.contractions += other.contractions;
        self.sentences += other.sentences;
        self.sentence_words += other.sentence_words;
        self.lowercase_starts += other.lowercase_starts;
    }
}

/// Python-compatible scalar-offset span extraction. A single malformed span
/// rejects the entire proof; fragments cutting a letter/mark token are trimmed.
pub fn authored_fragments<'a>(text: &'a str, spans: &[(usize, usize)]) -> Vec<&'a str> {
    if spans.len() > MAX_AUTHORSHIP_SPANS {
        return Vec::new();
    }
    let mut boundaries: Vec<usize> = text.char_indices().map(|(offset, _)| offset).collect();
    boundaries.push(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut previous = 0;
    let mut fragments = Vec::new();
    for &(mut start, mut end) in spans {
        if !(previous <= start && start < end && end <= chars.len()) {
            return Vec::new();
        }
        previous = end;
        if start > 0 && letter_or_mark(chars[start - 1]) {
            while start < end
                && (letter_or_mark(chars[start]) || matches!(chars[start], '\'' | '’'))
            {
                start += 1;
            }
        }
        if end < chars.len() && letter_or_mark(chars[end]) {
            while end > start
                && (letter_or_mark(chars[end - 1]) || matches!(chars[end - 1], '\'' | '’'))
            {
                end -= 1;
            }
        }
        if start < end {
            fragments.push(&text[boundaries[start]..boundaries[end]]);
        }
    }
    fragments
}

pub fn history_safe(text: &str) -> bool {
    text.trim_matches(python_whitespace).chars().count() >= 80
        && text
            .split(python_whitespace)
            .filter(|word| !word.is_empty())
            .count()
            >= 12
        && text.chars().count() <= 32000
        && !has_sensitive_pattern(text)
}

fn python_whitespace(character: char) -> bool {
    character.is_whitespace() || matches!(character, '\u{1c}'..='\u{1f}')
}

fn python_word(character: char) -> bool {
    character == '_'
        || matches!(
            get_general_category(character),
            GeneralCategory::UppercaseLetter
                | GeneralCategory::LowercaseLetter
                | GeneralCategory::TitlecaseLetter
                | GeneralCategory::ModifierLetter
                | GeneralCategory::OtherLetter
                | GeneralCategory::DecimalNumber
                | GeneralCategory::LetterNumber
                | GeneralCategory::OtherNumber
        )
}

fn python_word_boundary(text: &str, offset: usize) -> bool {
    text[..offset].chars().next_back().is_some_and(python_word)
        != text[offset..].chars().next().is_some_and(python_word)
}

fn has_sensitive_pattern(text: &str) -> bool {
    if SENSITIVE.is_match(text) {
        return true;
    }
    let mut previous_word = false;
    for (offset, character) in text.char_indices() {
        let word = python_word(character);
        // Python's word class excludes marks and join controls. Rust regex's
        // Unicode boundary does not, so evaluate these boundaries explicitly.
        if !previous_word && word {
            let suffix = &text[offset..];
            if matches!(character, 's' | 'S' | 'ſ' | 'g' | 'G') && SENSITIVE_TOKEN.is_match(suffix)
            {
                return true;
            }
            if get_general_category(character) == GeneralCategory::DecimalNumber {
                if let Some(candidate) = SENSITIVE_CARD.find(suffix) {
                    let mut digits = 0;
                    // The Python regex may backtrack from its greedy end to a
                    // boundary after any of 13..=19 digits (or its separator).
                    for (relative, value) in candidate.as_str().char_indices() {
                        if get_general_category(value) == GeneralCategory::DecimalNumber {
                            digits += 1;
                        }
                        if digits >= 13
                            && python_word_boundary(text, offset + relative + value.len_utf8())
                        {
                            return true;
                        }
                    }
                }
            }
        }
        previous_word = word;
    }
    false
}

pub fn seal_record(key: &[u8; 32], id: &str, payload: &[u8]) -> Result<Vec<u8>> {
    let mut nonce = [0u8; 12];
    OsRng
        .try_fill_bytes(&mut nonce)
        .map_err(|_| anyhow!("History randomness unavailable."))?;
    seal_with_nonce(key, id, payload, &nonce)
}

fn seal_with_nonce(key: &[u8; 32], id: &str, payload: &[u8], nonce: &[u8; 12]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| anyhow!("Invalid history key."))?;
    let aad = associated_data(id);
    let encrypted = cipher
        .encrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: payload,
                aad: &aad,
            },
        )
        .map_err(|_| anyhow!("History encryption failed."))?;
    let mut sealed = Vec::with_capacity(12 + encrypted.len());
    sealed.extend_from_slice(nonce);
    sealed.extend_from_slice(&encrypted);
    Ok(sealed)
}

pub fn open_record(key: &[u8; 32], id: &str, sealed: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| anyhow!("Invalid history key."))?;
    open_with_cipher(&cipher, id, sealed)
}
fn open_with_cipher(cipher: &Aes256Gcm, id: &str, sealed: &[u8]) -> Result<Vec<u8>> {
    ensure!(sealed.len() >= 28, AUTHENTICATION_ERROR);
    cipher
        .decrypt(
            Nonce::from_slice(&sealed[..12]),
            Payload {
                msg: &sealed[12..],
                aad: &associated_data(id),
            },
        )
        .map_err(|_| anyhow!(AUTHENTICATION_ERROR))
}
fn associated_data(id: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(AAD.len() + id.len());
    aad.extend_from_slice(AAD);
    aad.extend_from_slice(id.as_bytes());
    aad
}
fn scalar_prefix(text: &str, count: usize) -> &str {
    &text[..text
        .char_indices()
        .nth(count)
        .map(|(offset, _)| offset)
        .unwrap_or(text.len())]
}
fn scope_domain(domain: &str) -> Result<String> {
    if domain.is_empty() {
        Ok(String::new())
    } else {
        normalize_domain(domain)
    }
}
fn optional_string(value: Option<&Value>) -> Result<&str> {
    match value {
        None => Ok(""),
        Some(Value::String(text)) => Ok(text),
        _ => bail!("Invalid history record."),
    }
}
fn parse_spans(value: Option<&Value>) -> Vec<(usize, usize)> {
    let Some(Value::Array(spans)) = value else {
        return Vec::new();
    };
    if spans.len() > MAX_AUTHORSHIP_SPANS {
        return Vec::new();
    }
    spans
        .iter()
        .map(|span| {
            let bounds = span.as_array()?;
            if bounds.len() != 2 {
                return None;
            }
            Some((
                usize::try_from(bounds[0].as_u64()?).ok()?,
                usize::try_from(bounds[1].as_u64()?).ok()?,
            ))
        })
        .collect::<Option<Vec<_>>>()
        .unwrap_or_default()
}
fn letter_or_mark(character: char) -> bool {
    matches!(
        get_general_category(character),
        GeneralCategory::UppercaseLetter
            | GeneralCategory::LowercaseLetter
            | GeneralCategory::TitlecaseLetter
            | GeneralCategory::ModifierLetter
            | GeneralCategory::OtherLetter
            | GeneralCategory::NonspacingMark
            | GeneralCategory::SpacingMark
            | GeneralCategory::EnclosingMark
    )
}
fn casefold(text: &str) -> String {
    text.case_fold().collect()
}
fn writing_language(text: &str) -> String {
    let normalized: String = text.nfc().collect();
    let start = normalized
        .char_indices()
        .rev()
        .nth(3999)
        .map(|(offset, _)| offset)
        .unwrap_or(0);
    let words: Vec<String> = TOKENS
        .find_iter(&normalized[start..])
        .map(|word| casefold(word.as_str()))
        .collect();
    let mut scores: Vec<_> = LANGUAGE_WORDS
        .iter()
        .map(|(language, markers)| {
            (
                words
                    .iter()
                    .filter(|word| markers.contains(&word.as_str()))
                    .count(),
                *language,
            )
        })
        .collect();
    scores.sort_by(|a, b| b.cmp(a));
    if scores[0].0 >= 2 && scores[0].0 >= scores[1].0 + 2 {
        scores[0].1.to_owned()
    } else {
        String::new()
    }
}
fn style_features(fragments: &[String]) -> (BTreeMap<String, u64>, Features) {
    let mut words = BTreeMap::<String, u64>::new();
    let mut features = Features::default();
    for fragment in fragments {
        let tokens: Vec<_> = TOKENS.find_iter(fragment).collect();
        features.words += tokens.len() as u64;
        features.commas += fragment.matches(',').count() as u64;
        features.exclamations += fragment.matches('!').count() as u64;
        features.questions += fragment.matches('?').count() as u64;
        features.contractions += tokens
            .iter()
            .filter(|token| token.as_str().contains(['\'', '’']))
            .count() as u64;
        for sentence in SENTENCES.find_iter(fragment) {
            let mut tokens = TOKENS.find_iter(sentence.as_str());
            if let Some(first) = tokens.next() {
                features.sentences += 1;
                features.sentence_words += 1 + tokens.count() as u64;
                features.lowercase_starts += u64::from(
                    first
                        .as_str()
                        .chars()
                        .next()
                        .is_some_and(char::is_lowercase),
                );
            }
        }
        for word in WORDS.find_iter(fragment) {
            let word = casefold(word.as_str());
            if !LANGUAGE_WORDS
                .iter()
                .any(|(_, markers)| markers.contains(&word.as_str()))
            {
                *words.entry(word).or_default() += 1;
            }
        }
    }
    let mut ranked: Vec<_> = words.into_iter().collect();
    ranked.sort_by(|(a, ac), (b, bc)| bc.cmp(ac).then_with(|| a.cmp(b)));
    (ranked.into_iter().take(128).collect(), features)
}
fn rounded(value: f64, digits: i32) -> f64 {
    let factor = 10f64.powi(digits);
    (value * factor).round_ties_even() / factor
}
fn counters(values: [i64; 3]) -> Value {
    json!({"completions": values[0], "words": values[1], "characters": values[2]})
}

fn python_timestamp(now: DateTime<Utc>) -> String {
    let base = now.format("%Y-%m-%dT%H:%M:%S");
    let micros = now.nanosecond() / 1000;
    if micros == 0 {
        format!("{base}+00:00")
    } else {
        format!("{base}.{micros:06}+00:00")
    }
}

/// Resolve like pathlib.Path.resolve(strict=False), without creating the home.
fn resolved_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => {
                resolved.push(other.as_os_str());
                match fs::canonicalize(&resolved) {
                    Ok(canonical) => resolved = canonical,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }
    Ok(resolved)
}

pub fn credential_account(config_directory: &Path) -> Result<String> {
    let directory = resolved_path(config_directory)?;
    let text = directory
        .to_str()
        .ok_or_else(|| anyhow!("History directory is not Unicode."))?;
    // Windows canonicalize emits verbatim paths; pathlib strips the extended
    // prefix before converting the path to the string used for this identity.
    #[cfg(target_os = "windows")]
    let text = if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        std::borrow::Cow::Owned(format!(r"\\{unc}"))
    } else {
        std::borrow::Cow::Borrowed(text.strip_prefix(r"\\?\").unwrap_or(text))
    };
    let digest = Sha256::digest(text.as_bytes());
    let prefix: String = digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(format!("history-v1-{prefix}"))
}

fn load_history_key(
    path: &Path,
    credentials: &dyn HistoryCredentials,
) -> Result<Zeroizing<[u8; 32]>> {
    let directory = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let account = credential_account(directory)?;
    let encoded = match credentials.get_password("Inklow", &account)? {
        Some(encoded) => Zeroizing::new(encoded),
        None => {
            // No writes, schema initialization or replacement key until it is
            // proven that no existing encrypted history would be stranded.
            if path.try_exists()? {
                let connection =
                    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
                let count: i64 =
                    connection.query_row("SELECT count(*) FROM history", [], |row| row.get(0))?;
                ensure!(
                    count == 0,
                    "The history key is missing; clear unrecoverable encrypted history before creating a new key."
                );
            }
            let mut key = Zeroizing::new([0u8; 32]);
            OsRng
                .try_fill_bytes(key.as_mut())
                .map_err(|_| anyhow!("History randomness unavailable."))?;
            let encoded = Zeroizing::new(STANDARD.encode(key.as_ref()));
            credentials.set_password("Inklow", &account, &encoded)?;
            let retained = credentials
                .get_password("Inklow", &account)?
                .map(Zeroizing::new);
            ensure!(
                retained.as_ref().map(|value| value.as_str()) == Some(encoded.as_str()),
                "The OS credential store did not retain the history key."
            );
            encoded
        }
    };
    let decoded = Zeroizing::new(
        STANDARD
            .decode(encoded.as_bytes())
            .context("Invalid stored history key.")?,
    );
    ensure!(decoded.len() == 32, "The stored history key is invalid.");
    let mut key = Zeroizing::new([0u8; 32]);
    key.copy_from_slice(&decoded);
    Ok(key)
}

#[derive(Default)]
struct OsCredentials {
    #[cfg(target_os = "linux")]
    backend: Mutex<Option<LinuxBackend>>,
}

#[cfg(target_os = "linux")]
impl OsCredentials {
    fn backend(&self) -> Result<LinuxBackend> {
        let mut selected = self
            .backend
            .lock()
            .map_err(|_| anyhow!("Credential backend selection unavailable."))?;
        if let Some(backend) = *selected {
            return Ok(backend);
        }
        let backend = linux_backend()?;
        *selected = Some(backend);
        Ok(backend)
    }
}

#[cfg(target_os = "macos")]
impl HistoryCredentials for OsCredentials {
    fn get_password(&self, service: &str, account: &str) -> Result<Option<String>> {
        use keyring::credential::CredentialApi;
        let credential = keyring::macos::MacCredential::new_with_target(None, service, account)?;
        optional_password(credential.get_password())
    }
    fn set_password(&self, service: &str, account: &str, password: &str) -> Result<()> {
        use keyring::credential::CredentialApi;
        keyring::macos::MacCredential::new_with_target(None, service, account)?
            .set_password(password)?;
        Ok(())
    }
}

#[cfg(target_os = "windows")]
impl HistoryCredentials for OsCredentials {
    fn get_password(&self, service: &str, account: &str) -> Result<Option<String>> {
        use keyring::credential::CredentialApi;
        let primary =
            keyring::windows::WinCredential::new_with_target(Some(service), service, account)?;
        match primary.get_attributes() {
            Ok(attributes) if attributes.get("username").map(String::as_str) == Some(account) => {
                return optional_password(primary.get_password());
            }
            Ok(_) | Err(keyring::Error::NoEntry) => {}
            Err(error) => return Err(error.into()),
        }
        let compound = format!("{account}@{service}");
        let credential =
            keyring::windows::WinCredential::new_with_target(Some(&compound), service, account)?;
        optional_password(credential.get_password())
    }
    fn set_password(&self, service: &str, account: &str, password: &str) -> Result<()> {
        use keyring::credential::CredentialApi;
        let primary =
            keyring::windows::WinCredential::new_with_target(Some(service), service, account)?;
        match primary.get_attributes() {
            Ok(attributes) => {
                let previous_user = attributes
                    .get("username")
                    .ok_or_else(|| anyhow!("Credential username unavailable."))?;
                let previous_password = Zeroizing::new(primary.get_password()?);
                let compound = format!("{previous_user}@{service}");
                keyring::windows::WinCredential::new_with_target(
                    Some(&compound),
                    service,
                    previous_user,
                )?
                .set_password(&previous_password)?;
            }
            Err(keyring::Error::NoEntry) => {}
            Err(error) => return Err(error.into()),
        }
        primary.set_password(password)?;
        Ok(())
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn optional_password(result: keyring::Result<String>) -> Result<Option<String>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(target_os = "linux")]
impl HistoryCredentials for OsCredentials {
    fn get_password(&self, service: &str, account: &str) -> Result<Option<String>> {
        match self.backend()? {
            LinuxBackend::SecretService => secret_service_get(service, account),
            LinuxBackend::KWallet(bus, path) => kwallet_get(bus, path, service, account),
        }
    }
    fn set_password(&self, service: &str, account: &str, password: &str) -> Result<()> {
        match self.backend()? {
            LinuxBackend::SecretService => secret_service_set(service, account, password),
            LinuxBackend::KWallet(bus, path) => kwallet_set(bus, path, service, account, password),
        }
    }
}

#[cfg(target_os = "linux")]
fn secret_service_collection(
    service: &dbus_secret_service::SecretService,
) -> Result<dbus_secret_service::Collection<'_>> {
    let collection = resolve_default_collection(
        || match service.get_default_collection() {
            Ok(collection) => Ok(Some(collection)),
            Err(dbus_secret_service::Error::NoResult) => Ok(None),
            Err(error) => Err(error.into()),
        },
        || Ok(service.create_collection("Default", "default")?),
    )?;
    // Match Python SecretStorage: create only a missing default alias, and
    // unlock that collection before either lookup or creation of an item.
    // Cancellation/access failure is never interpreted as an absent alias.
    read_unlocked_secret(
        || Ok(collection.is_locked()?),
        || Ok(collection.unlock()?),
        || Ok(()),
    )?;
    Ok(collection)
}

#[cfg(target_os = "linux")]
fn secret_service_get(service: &str, account: &str) -> Result<Option<String>> {
    let connection =
        dbus_secret_service::SecretService::connect(dbus_secret_service::EncryptionType::Dh)?;
    let collection = secret_service_collection(&connection)?;
    let attributes = std::collections::HashMap::from([("service", service), ("username", account)]);
    let items = collection.search_items(attributes)?;
    let Some(item) = items.into_iter().next() else {
        return Ok(None);
    };
    read_unlocked_secret(
        || Ok(item.is_locked()?),
        || Ok(item.unlock()?),
        || Ok(Some(String::from_utf8(item.get_secret()?)?)),
    )
}

#[cfg(target_os = "linux")]
fn secret_service_set(service: &str, account: &str, password: &str) -> Result<()> {
    let connection =
        dbus_secret_service::SecretService::connect(dbus_secret_service::EncryptionType::Dh)?;
    let collection = secret_service_collection(&connection)?;
    let attributes = std::collections::HashMap::from([
        ("service", service),
        ("username", account),
        ("application", "Python keyring library"),
    ]);
    collection.create_item(
        &format!("Password for '{account}' on '{service}'"),
        attributes,
        password.as_bytes(),
        true,
        "text/plain",
    )?;
    Ok(())
}

#[cfg(any(target_os = "linux", test))]
fn resolve_default_collection<T>(
    lookup: impl FnOnce() -> Result<Option<T>>,
    create: impl FnOnce() -> Result<T>,
) -> Result<T> {
    match lookup()? {
        Some(collection) => Ok(collection),
        None => create(),
    }
}

#[cfg(any(target_os = "linux", test))]
fn read_unlocked_secret<T>(
    is_locked: impl Fn() -> Result<bool>,
    unlock: impl FnOnce() -> Result<()>,
    read: impl FnOnce() -> Result<T>,
) -> Result<T> {
    if is_locked()? {
        unlock()?;
        ensure!(!is_locked()?, "OS credential store unlock was refused.");
    }
    read()
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
enum LinuxBackend {
    SecretService,
    KWallet(&'static str, &'static str),
}

#[cfg(target_os = "linux")]
fn linux_backend() -> Result<LinuxBackend> {
    let connection = dbus::blocking::Connection::new_session()?;
    let bus = connection.with_proxy(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        Duration::from_secs(2),
    );
    let (active,): (Vec<String>,) = bus.method_call("org.freedesktop.DBus", "ListNames", ())?;
    let (activatable,): (Vec<String>,) =
        bus.method_call("org.freedesktop.DBus", "ListActivatableNames", ())?;
    let viable = |name: &str| {
        active.iter().any(|value| value == name) || activatable.iter().any(|value| value == name)
    };
    let wallet_viable = |name: &str| {
        active.iter().any(|value| value == name) && activatable.iter().any(|value| value == name)
    };
    let kwallet5 = "org.kde.kwalletd5";
    let kde = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .any(|desktop| desktop == "KDE");
    // Match Python's priorities: KDE's KWallet5 5.1, SecretService 5,
    // non-KDE KWallet5 4.9, KWallet4 one point lower. An unlock refusal
    // never falls through to creating a different backend/key.
    if kde && wallet_viable(kwallet5) {
        return Ok(LinuxBackend::KWallet(kwallet5, "/modules/kwalletd5"));
    }
    if viable("org.freedesktop.secrets") {
        return Ok(LinuxBackend::SecretService);
    }
    if wallet_viable(kwallet5) {
        return Ok(LinuxBackend::KWallet(kwallet5, "/modules/kwalletd5"));
    }
    if wallet_viable("org.kde.kwalletd") {
        return Ok(LinuxBackend::KWallet(
            "org.kde.kwalletd",
            "/modules/kwalletd",
        ));
    }
    bail!("No supported operating-system credential store is available.")
}

#[cfg(target_os = "linux")]
fn kwallet_operation<T>(
    bus: &str,
    path: &str,
    action: impl FnOnce(&dbus::blocking::Proxy<'_, &dbus::blocking::Connection>, i32, &str) -> Result<T>,
) -> Result<T> {
    let connection = dbus::blocking::Connection::new_session()?;
    let proxy = connection.with_proxy(bus, path, Duration::from_secs(120));
    let app = "BroTypist";
    let (wallet,): (String,) = proxy.method_call("org.kde.KWallet", "networkWallet", ())?;
    let (handle,): (i32,) = proxy.method_call("org.kde.KWallet", "open", (wallet, 0i64, app))?;
    ensure!(handle >= 0, "OS credential store unlock was cancelled.");
    action(&proxy, handle, app)
}

#[cfg(target_os = "linux")]
fn kwallet_get(bus: &str, path: &str, service: &str, account: &str) -> Result<Option<String>> {
    kwallet_operation(bus, path, |proxy, handle, app| {
        let (exists,): (bool,) = proxy.method_call(
            "org.kde.KWallet",
            "hasEntry",
            (handle, service, account, app),
        )?;
        if exists {
            let (password,): (String,) = proxy.method_call(
                "org.kde.KWallet",
                "readPassword",
                (handle, service, account, app),
            )?;
            return Ok(Some(password));
        }
        // Python keyring's old folder format was account@service in Python.
        // Read it without moving/deleting it or changing credential identity.
        let legacy = format!("{account}@{service}");
        let (legacy_exists,): (bool,) = proxy.method_call(
            "org.kde.KWallet",
            "hasEntry",
            (handle, "Python", &legacy, app),
        )?;
        if !legacy_exists {
            return Ok(None);
        }
        let (password,): (String,) = proxy.method_call(
            "org.kde.KWallet",
            "readPassword",
            (handle, "Python", legacy, app),
        )?;
        Ok(Some(password))
    })
}

#[cfg(target_os = "linux")]
fn kwallet_set(bus: &str, path: &str, service: &str, account: &str, password: &str) -> Result<()> {
    kwallet_operation(bus, path, |proxy, handle, app| {
        let (folder,): (bool,) =
            proxy.method_call("org.kde.KWallet", "hasFolder", (handle, service, app))?;
        if !folder {
            let (created,): (bool,) =
                proxy.method_call("org.kde.KWallet", "createFolder", (handle, service, app))?;
            ensure!(created, "OS credential folder could not be created.");
        }
        let (status,): (i32,) = proxy.method_call(
            "org.kde.KWallet",
            "writePassword",
            (handle, service, account, password, app),
        )?;
        ensure!(
            status == 0,
            "OS credential store did not retain history key."
        );
        Ok(())
    })
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
impl HistoryCredentials for OsCredentials {
    fn get_password(&self, _: &str, _: &str) -> Result<Option<String>> {
        bail!("No supported operating-system credential store is available.")
    }
    fn set_password(&self, _: &str, _: &str, _: &str) -> Result<()> {
        bail!("No supported operating-system credential store is available.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tempfile::TempDir;

    const TEXT: &str = "We discussed observability and instrumentation during the planning meeting. The observability review needs more instrumentation examples before the next team meeting.";
    const PORTUGUESE: &str = "Nós discutimos observabilidade e instrumentação durante a reunião de planejamento. A observabilidade precisa de mais instrumentação para a próxima reunião da equipe.";

    fn now() -> DateTime<Utc> {
        "2026-04-15T12:00:00+00:00".parse().unwrap()
    }
    fn store() -> (TempDir, HistoryStore) {
        let temp = TempDir::new().unwrap();
        let mut store =
            HistoryStore::with_path_and_key(temp.path().join("local.sqlite3"), [b'a'; 32]);
        store.unlock().unwrap();
        (temp, store)
    }
    fn record(store: &mut HistoryStore, id: &str, app: &str, domain: &str, text: &str) {
        assert!(
            store
                .record_identified_at(id, app, domain, text, &[(0, text.chars().count())], now())
                .unwrap()
        );
    }
    fn learn(store: &HistoryStore) -> String {
        store
            .personalization("writer", "", TEXT, 90, &|_, _| true)
            .unwrap()
    }
    fn insert_raw(store: &HistoryStore, id: &str, value: Value) {
        let sealed = seal_with_nonce(
            &[b'a'; 32],
            id,
            &serde_json::to_vec(&value).unwrap(),
            &[7; 12],
        )
        .unwrap();
        store
            .connect()
            .unwrap()
            .execute(
                "INSERT OR REPLACE INTO history VALUES (?,?)",
                params![id, sealed],
            )
            .unwrap();
    }

    #[test]
    fn crypto_nonce_identifier_key_and_payload_are_authenticated() {
        let key = [3; 32];
        let payload = "{\"app\":\"writer\",\"text\":\"😀 é\"}".as_bytes();
        let sealed = seal_with_nonce(&key, "synthetic-😀", payload, &[5; 12]).unwrap();
        assert_eq!(&sealed[..12], &[5; 12]);
        assert_eq!(sealed.len(), 12 + payload.len() + 16);
        assert_eq!(
            sealed,
            seal_with_nonce(&key, "synthetic-😀", payload, &[5; 12]).unwrap()
        );
        assert_eq!(open_record(&key, "synthetic-😀", &sealed).unwrap(), payload);
        assert!(open_record(&key, "different", &sealed).is_err());
        assert!(open_record(&[4; 32], "synthetic-😀", &sealed).is_err());
        for index in [0, 12, sealed.len() - 1] {
            let mut damaged = sealed.clone();
            damaged[index] ^= 1;
            assert!(open_record(&key, "synthetic-😀", &damaged).is_err());
        }
        for size in 0..28 {
            assert!(open_record(&key, "synthetic-😀", &sealed[..size]).is_err());
        }
    }

    #[test]
    fn construction_locked_reads_and_rejections_never_load_keys_or_create_database() {
        let temporary = TempDir::new().unwrap();
        let loads = Arc::new(AtomicUsize::new(0));
        let observed = loads.clone();
        let mut store = HistoryStore::with_key_loader(
            temporary.path().join("nested/local.sqlite3"),
            Box::new(move || {
                observed.fetch_add(1, Ordering::SeqCst);
                bail!("synthetic refusal");
            }),
        );
        assert!(!store.is_unlocked());
        assert_eq!(learn(&store), "");
        assert!(store.records(None, None, 100).is_err());
        assert!(!store.record("writer", "", TEXT, &[]).unwrap());
        assert_eq!(
            store.history_counts(false).unwrap(),
            json!({"available": true, "apps": {}, "domains": {}})
        );
        assert_eq!(store.history_info().unwrap()["count"], 0);
        assert_eq!(store.statistics(7, "day").unwrap()["active_days"], 0);
        assert_eq!(loads.load(Ordering::SeqCst), 0);
        assert!(
            store
                .unlock()
                .unwrap_err()
                .to_string()
                .contains("plaintext")
        );
        assert_eq!(loads.load(Ordering::SeqCst), 1);
        assert!(!store.path.exists());
        assert!(!store.path.parent().unwrap().exists());
    }

    #[test]
    fn payload_schema_is_compatible_and_plaintext_never_reaches_database() {
        let (_temporary, mut store) = store();
        record(&mut store, "session", "writer", "Example.COM.", TEXT);
        let bytes = fs::read(store.path()).unwrap();
        assert!(
            !bytes
                .windows(TEXT.len())
                .any(|window| window == TEXT.as_bytes())
        );
        assert!(!bytes.windows(13).any(|window| window == b"observability"));
        let sealed: Vec<u8> = store
            .connect()
            .unwrap()
            .query_row("SELECT sealed FROM history WHERE id='session'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let payload: Value =
            serde_json::from_slice(&open_record(&[b'a'; 32], "session", &sealed).unwrap()).unwrap();
        assert_eq!(
            payload,
            json!({"app": "writer", "domain": "example.com", "text": TEXT, "authorship": 1,
                                  "authored_spans": [[0, TEXT.chars().count()]], "time": "2026-04-15T12:00:00+00:00"})
        );
        assert_eq!(store.history_info().unwrap()["count"], 1);
        record(&mut store, "session", "writer", "", TEXT);
        assert_eq!(store.history_info().unwrap()["count"], 1);
        assert_eq!(
            store.statistics(0, "day").unwrap()["totals"]["completions"],
            0
        );
    }

    #[test]
    fn secrets_short_text_and_invalid_domains_are_refused_without_disk_writes() {
        let (_temporary, mut store) = store();
        for text in [
            "Small message".to_owned(),
            format!("{TEXT} api_key: synthetic-sensitive-value"),
            format!("{TEXT} 4111 1111 1111 1111"),
            format!("{TEXT} ghp_abcdefghijklmnopqr"),
            format!("{TEXT} -----BEGIN RSA PRIVATE KEY-----"),
            "word ".repeat(7000),
        ] {
            assert!(
                !store
                    .record_identified_at("one", "writer", "", &text, &[], now())
                    .unwrap()
            );
        }
        assert!(
            !store
                .record("writer", "https://example.com/path", TEXT, &[])
                .unwrap()
        );
        assert!(!store.path().exists());
    }

    #[test]
    fn corruption_latches_until_full_deletion_and_does_not_clear_statistics() {
        let (_temporary, mut store) = store();
        record(&mut store, "one", "writer", "", TEXT);
        store
            .record_statistics_at("writer", "accepted", true, now().date_naive())
            .unwrap();
        store
            .connect()
            .unwrap()
            .execute("UPDATE history SET sealed=zeroblob(length(sealed))", [])
            .unwrap();
        assert!(
            learn_result(&store)
                .unwrap_err()
                .to_string()
                .contains("authenticated")
        );
        assert!(!store.is_unlocked());
        assert!(
            store
                .unlock()
                .unwrap_err()
                .to_string()
                .contains("authenticated")
        );
        assert!(store.delete(Some("writer"), None).is_err());
        assert_eq!(store.delete(None, None).unwrap(), 1);
        store.unlock().unwrap();
        assert_eq!(
            store.statistics_at(0, "app", now().date_naive()).unwrap()["totals"]["completions"],
            1
        );
        assert_eq!(learn(&store), "");
    }
    fn learn_result(store: &HistoryStore) -> Result<String> {
        store.personalization("writer", "", TEXT, 90, &|_, _| true)
    }

    #[test]
    fn malformed_authenticated_payloads_latch_but_legacy_payloads_stay_app_scoped() {
        for malformed in [
            json!([]),
            json!({"text": 12}),
            json!({"text": TEXT, "app": false}),
            json!({"text": TEXT, "domain": null}),
        ] {
            let (_temporary, store) = store();
            insert_raw(&store, "broken", malformed);
            assert!(store.records(None, None, 1).is_err());
            assert!(!store.is_unlocked());
        }
        let (_temporary, store) = store();
        insert_raw(
            &store,
            "legacy",
            json!({"app": "writer", "text": TEXT, "time": "2026-01-01T00:00:00+00:00"}),
        );
        let rows = store.records(None, None, 100).unwrap();
        assert_eq!(rows[0].domain, "");
        assert!(rows[0].authored_spans.is_empty());
        assert_eq!(learn(&store), "");
    }

    #[derive(Default)]
    struct SyntheticCredentials {
        password: Mutex<Option<String>>,
        reads: AtomicUsize,
        writes: AtomicUsize,
        retain: bool,
    }
    impl HistoryCredentials for Arc<SyntheticCredentials> {
        fn get_password(&self, service: &str, account: &str) -> Result<Option<String>> {
            assert_eq!(service, "Inklow");
            assert!(account.starts_with("history-v1-"));
            assert_eq!(account.len(), 35);
            self.reads.fetch_add(1, Ordering::SeqCst);
            Ok(self.password.lock().unwrap().clone())
        }
        fn set_password(&self, _: &str, _: &str, password: &str) -> Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            if self.retain {
                *self.password.lock().unwrap() = Some(password.to_owned());
            }
            Ok(())
        }
    }

    #[test]
    fn missing_existing_key_never_gets_replaced_and_full_clear_keeps_credentials() {
        let (_temporary, mut initial) = store();
        record(&mut initial, "one", "writer", "", TEXT);
        let backend = Arc::new(SyntheticCredentials {
            retain: true,
            ..Default::default()
        });
        let mut reopened =
            HistoryStore::with_credentials(initial.path(), Box::new(backend.clone()));
        assert!(reopened.unlock().is_err());
        assert_eq!(backend.writes.load(Ordering::SeqCst), 0);
        assert_eq!(reopened.history_info().unwrap()["count"], 1);
        assert_eq!(reopened.delete(None, None).unwrap(), 1);
        reopened.unlock().unwrap();
        assert_eq!(backend.writes.load(Ordering::SeqCst), 1);
        assert_eq!(
            STANDARD
                .decode(backend.password.lock().unwrap().as_ref().unwrap())
                .unwrap()
                .len(),
            32
        );
        reopened.delete(None, None).unwrap();
        assert_eq!(backend.writes.load(Ordering::SeqCst), 1);
        assert!(backend.password.lock().unwrap().is_some());
    }

    #[test]
    fn invalid_or_unretained_credentials_never_create_database() {
        for encoded in [
            "not base64!".to_owned(),
            STANDARD.encode([1u8; 31]),
            STANDARD.encode([1u8; 33]),
        ] {
            let temporary = TempDir::new().unwrap();
            let backend = Arc::new(SyntheticCredentials {
                password: Mutex::new(Some(encoded)),
                retain: true,
                ..Default::default()
            });
            let mut store = HistoryStore::with_credentials(
                temporary.path().join("local.sqlite3"),
                Box::new(backend.clone()),
            );
            assert!(store.unlock().is_err());
            assert!(!store.path().exists());
            assert_eq!(backend.writes.load(Ordering::SeqCst), 0);
        }
        let temporary = TempDir::new().unwrap();
        let backend = Arc::new(SyntheticCredentials::default());
        let mut store = HistoryStore::with_credentials(
            temporary.path().join("local.sqlite3"),
            Box::new(backend),
        );
        assert!(store.unlock().is_err());
        assert!(!store.path().exists());
    }

    #[test]
    fn account_hash_is_resolved_path_scoped_without_creating_profile() {
        let temporary = TempDir::new().unwrap();
        let first = temporary.path().join("missing/../profile");
        let second = temporary.path().join("profile");
        assert_eq!(
            credential_account(&first).unwrap(),
            credential_account(&second).unwrap()
        );
        assert_ne!(
            credential_account(&second).unwrap(),
            credential_account(&temporary.path().join("other")).unwrap()
        );
        assert!(!second.exists());
    }

    #[test]
    fn exact_domain_deletion_temporarily_unlocks_and_relocks() {
        let (_temporary, mut store) = store();
        for (id, domain) in [
            ("a", "example.com"),
            ("b", "docs.example.com"),
            ("c", "notexample.com"),
            ("d", ""),
        ] {
            record(&mut store, id, "browser", domain, TEXT);
        }
        insert_raw(&store, "legacy", json!({"app": "browser", "text": TEXT}));
        store.lock();
        assert_eq!(learn(&store), "");
        assert!(!store.record("browser", "", TEXT, &[]).unwrap());
        assert!(
            !store.history_counts(false).unwrap()["available"]
                .as_bool()
                .unwrap()
        );
        assert!(store.delete(None, Some("example.com")).is_err());
        assert!(
            store
                .delete_with_unlock(Some("browser"), Some("example.com"), true)
                .is_err()
        );
        assert_eq!(
            store
                .delete_with_unlock(None, Some("Example.COM."), true)
                .unwrap(),
            1
        );
        assert!(!store.is_unlocked());
        assert_eq!(
            store.history_counts(true).unwrap(),
            json!({"available": true, "apps": {"browser": 4}, "domains": {"docs.example.com": 1, "notexample.com": 1}})
        );
        assert!(!store.is_unlocked());
        store.unlock().unwrap();
        assert_eq!(
            store
                .records(Some("browser"), Some("docs.example.com"), 1)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(store.delete(Some("browser"), None).unwrap(), 4);
    }

    #[test]
    fn temporary_unlock_relocks_on_authentication_error() {
        let (_temporary, mut store) = store();
        record(&mut store, "a", "writer", "", TEXT);
        store
            .connect()
            .unwrap()
            .execute("UPDATE history SET sealed=x'00'", [])
            .unwrap();
        store.lock();
        assert!(store.history_counts(true).is_err());
        assert!(!store.is_unlocked());
        assert!(store.unlock().is_err());
    }

    #[test]
    fn authored_unicode_offsets_are_preserved_and_normalization_only_affects_learning() {
        let mut evidence = Vec::new();
        for decomposed in [false, true] {
            let (_temporary, mut store) = store();
            let before = "😀 A geração segredofantasma segredofantasma. ";
            let after = " Outra geração segredofantasma segredofantasma.";
            let normalize = |text: &str| -> String {
                if decomposed {
                    text.nfd().collect()
                } else {
                    text.nfc().collect()
                }
            };
            let before = normalize(before);
            let body = normalize(PORTUGUESE);
            let after = normalize(after);
            let text = format!("{before}{body}{after}");
            let spans = [(
                before.chars().count(),
                before.chars().count() + body.chars().count(),
            )];
            assert!(
                store
                    .record_identified_at("canonical", "writer", "", &text, &spans, now())
                    .unwrap()
            );
            let path = store.path().to_path_buf();
            store.lock();
            let mut store = HistoryStore::with_path_and_key(path, [b'a'; 32]);
            store.unlock().unwrap();
            let rows = store.records(None, None, 10).unwrap();
            assert_eq!(rows[0].text, text);
            assert_eq!(rows[0].authored_spans, spans);
            assert_eq!(
                authored_fragments(&rows[0].text, &rows[0].authored_spans),
                [body.as_str()]
            );
            for query in [
                normalize("Nós na próxima reunião"),
                "Nós na próxima reunião".to_owned(),
            ] {
                let learned = store
                    .personalization("writer", "", &query, 90, &|_, _| true)
                    .unwrap();
                let value: Value = serde_json::from_str(&learned).unwrap();
                assert_eq!(value["language"], "pt");
                assert!(
                    value["vocabulary"]
                        .as_array()
                        .unwrap()
                        .contains(&json!("instrumentação"))
                );
                assert!(!learned.contains("segredofantasma"));
                evidence.push(value);
            }
        }
        assert!(evidence.windows(2).all(|values| values[0] == values[1]));
    }

    #[test]
    fn unicode15_history_vocabulary_merges_case_without_changing_authored_spans() {
        let (_temporary, mut store) = store();
        let before = "😀 e\u{301} generated introduction. ";
        let body = format!(
            "{TEXT} \u{10570}\u{10571}\u{10572}\u{10573} \u{10597}\u{10598}\u{10599}\u{1059a} ᲐᲑᲒᲓ აბგდ Ꞔustom ꞔustom."
        );
        let text = format!("{before}{body} Generated ending.");
        let spans = [(
            before.chars().count(),
            before.chars().count() + body.chars().count(),
        )];
        assert!(
            store
                .record_identified_at("unicode15", "writer", "", &text, &spans, now())
                .unwrap()
        );
        let rows = store.records(None, None, 10).unwrap();
        assert_eq!(rows[0].text, text);
        assert_eq!(rows[0].authored_spans, spans);
        assert_eq!(
            authored_fragments(&rows[0].text, &rows[0].authored_spans),
            [body.as_str()]
        );
        let learned: Value = serde_json::from_str(&learn(&store)).unwrap();
        let vocabulary = learned["vocabulary"].as_array().unwrap();
        // Each spelling occurs once: only case-insensitive aggregation reaches
        // the two-occurrence threshold for personalized vocabulary.
        for word in ["\u{10597}\u{10598}\u{10599}\u{1059a}", "აბგდ", "ꞔustom"] {
            assert!(vocabulary.contains(&json!(word)), "{word}: {vocabulary:?}");
        }
    }

    #[test]
    fn invalid_and_partial_token_authorship_never_teaches_generated_words() {
        let (_temporary, mut store) = store();
        record(&mut store, "partial", "writer", "", TEXT);
        let expected: Value = serde_json::from_str(&learn(&store)).unwrap();
        for edge_start in [false, true] {
            for cut in [4, 5] {
                let token = "xeno\u{301}phantom";
                let text = format!("{TEXT} {token} {token}");
                let n = TEXT.chars().count();
                let t = token.chars().count();
                let mut spans = vec![(0, n)];
                for start in [n + 1, n + t + 2] {
                    spans.push(if edge_start {
                        (start + cut, start + t)
                    } else {
                        (start, start + cut)
                    });
                }
                assert!(
                    store
                        .record_identified_at("partial", "writer", "", &text, &spans, now())
                        .unwrap()
                );
                assert_eq!(
                    serde_json::from_str::<Value>(&learn(&store)).unwrap(),
                    expected
                );
            }
        }
        for spans in [
            vec![(3, 2)],
            vec![(0, 9999)],
            vec![(0, 80), (70, 90)],
            vec![(0, 1); 1025],
        ] {
            assert!(
                store
                    .record_identified_at("partial", "writer", "", TEXT, &spans, now())
                    .unwrap()
            );
            assert!(
                store.records(None, None, 1).unwrap()[0]
                    .authored_spans
                    .is_empty()
            );
            assert_eq!(learn(&store), "");
        }
    }

    #[test]
    fn generated_legacy_policy_language_and_scope_gates_apply_to_every_request() {
        let (_temporary, mut store) = store();
        assert!(
            store
                .record_identified_at("generated", "writer", "", TEXT, &[], now())
                .unwrap()
        );
        assert_eq!(learn(&store), "");
        record(&mut store, "authored", "writer", "", TEXT);
        let before = learn(&store);
        assert!(!before.is_empty());
        assert_eq!(
            store
                .personalization("writer", "", TEXT, 90, &|_, _| false)
                .unwrap(),
            ""
        );
        assert_eq!(learn(&store), before);
        assert_eq!(
            store
                .personalization("other", "", TEXT, 90, &|_, _| true)
                .unwrap(),
            ""
        );
        assert_eq!(
            store
                .personalization("writer", "", "x y z", 90, &|_, _| true)
                .unwrap(),
            ""
        );
        record(&mut store, "pt", "writer", "", PORTUGUESE);
        assert!(!learn(&store).contains("observabilidade"));
        record(&mut store, "host", "browser", "chat.example", TEXT);
        assert_ne!(
            store
                .personalization("other-browser", "chat.example", TEXT, 90, &|_, _| true)
                .unwrap(),
            ""
        );
        assert_eq!(
            store
                .personalization("browser", "other.example", TEXT, 90, &|_, _| true)
                .unwrap(),
            ""
        );
        // Current source exclusions are checked even after the index exists.
        assert_eq!(
            store
                .personalization("other-browser", "chat.example", TEXT, 90, &|app, _| app
                    != "browser")
                .unwrap(),
            ""
        );
        store.delete(Some("writer"), None).unwrap();
        assert_eq!(learn(&store), "");
        store.lock();
        assert_eq!(learn(&store), "");
    }

    #[test]
    fn lexical_features_are_bounded_strength_scaled_and_never_replay_sentences() {
        let (_temporary, mut store) = store();
        let tokens: Vec<String> = (0..64)
            .map(|n| {
                format!(
                    "term{}{}",
                    char::from(b'a' + n / 8),
                    char::from(b'a' + n % 8)
                )
            })
            .collect();
        let text = format!("{TEXT} {} {}.", tokens.join(" "), tokens.join(" "));
        record(&mut store, "bounded", "writer", "", &text);
        assert_eq!(
            store
                .personalization("writer", "", TEXT, 0, &|_, _| true)
                .unwrap(),
            ""
        );
        let mut sizes = Vec::new();
        for strength in [20, 55, 90, 100] {
            let result = store
                .personalization("writer", "", TEXT, strength, &|_, _| true)
                .unwrap();
            assert!(result.chars().count() <= 2048);
            assert!(!result.contains(TEXT));
            let value: Value = serde_json::from_str(&result).unwrap();
            let size = value["vocabulary"].as_array().unwrap().len();
            assert!(size <= 24);
            sizes.push(size);
        }
        assert!(sizes.windows(2).all(|pair| pair[0] < pair[1]));
        let chat = "we check the telemetry with our team and we review the telemetry before sharing the report with our team";
        record(&mut store, "chat", "chat", "", chat);
        let result: Value = serde_json::from_str(
            &store
                .personalization("chat", "", TEXT, 90, &|_, _| true)
                .unwrap(),
        )
        .unwrap();
        assert!(result["style"].get("mean_sentence_words").is_none());
        assert!(
            result["vocabulary"]
                .as_array()
                .unwrap()
                .contains(&json!("telemetry"))
        );
    }

    #[test]
    fn only_latest_two_hundred_entries_contribute_to_learning() {
        let (_temporary, mut store) = store();
        record(&mut store, "old-authored", "writer", "", TEXT);
        for n in 0..200 {
            assert!(
                store
                    .record_identified_at(&format!("generated-{n}"), "writer", "", TEXT, &[], now())
                    .unwrap()
            );
        }
        assert_eq!(learn(&store), "");
        record(&mut store, "old-authored", "writer", "", TEXT);
        assert!(!learn(&store).is_empty());
    }

    #[test]
    fn statistics_match_rolling_periods_scalar_counts_grouping_and_active_averages() {
        let (_temporary, mut store) = store();
        let today = now().date_naive();
        store
            .record_statistics_at("writer", " first ", true, today)
            .unwrap();
        store
            .record_statistics_at("writer", "second", false, today)
            .unwrap();
        store
            .record_statistics_at("mail", "sent yesterday", true, today - Days::new(1))
            .unwrap();
        store
            .record_statistics_at("archive", "old", true, today - Days::new(60))
            .unwrap();
        store
            .record_statistics_at("future", "excluded", true, today + Days::new(1))
            .unwrap();
        let current = store.statistics_period_at("today", "app", today).unwrap();
        assert_eq!(
            current["totals"],
            json!({"completions": 1, "words": 2, "characters": 13})
        );
        let week = store.statistics_at(7, "day", today).unwrap();
        assert_eq!(week["active_days"], 2);
        assert_eq!(week["daily_average"]["characters"], 13.5);
        assert_eq!(week["rows"][0]["estimated_seconds"], 3);
        assert_eq!(week["rows"][1]["estimated_seconds"], 3);
        assert_eq!(
            store.statistics_period_at("all", "month", today).unwrap()["totals"]["completions"],
            3
        );
        assert_eq!(
            store
                .statistics_at(1, "day", today - Days::new(30))
                .unwrap()["active_days"],
            0
        );
        assert!(store.statistics_period_at("invalid", "day", today).is_err());
        assert!(store.statistics_at(7, "invalid", today).is_err());
        let bytes = fs::read(store.path()).unwrap();
        assert!(!bytes.windows(14).any(|value| value == b"sent yesterday"));
        store
            .record_statistics_at("unicode", "😀 don't e\u{301}", false, today)
            .unwrap();
        let unicode = store.statistics_at(1, "app", today).unwrap();
        let row = unicode["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["label"] == "unicode")
            .unwrap();
        assert_eq!(row["characters"], 10);
        assert_eq!(row["words"], 2);
        assert_eq!(row["completions"], 0);
        record(&mut store, "preserved", "writer", "", TEXT);
        store.clear_statistics().unwrap();
        assert_eq!(
            store.statistics_at(0, "day", today).unwrap()["active_days"],
            0
        );
        assert_eq!(store.history_info().unwrap()["count"], 1);
    }

    #[test]
    fn style_features_distinguish_informal_and_formal_authored_writing() {
        let (_temporary, mut store) = store();
        let informal = "we don't wait! we don't stop! we don't worry! the observability review is ready! the instrumentation review is ready! we don't need another observability review!";
        let formal = "The observability review, which includes the instrumentation results, is ready for the team to evaluate carefully before the next meeting, and the observability report, which covers the instrumentation review, will support the entire planning process.";
        record(&mut store, "chat", "chat", "", informal);
        record(&mut store, "work", "writer", "", formal);
        let chat: Value = serde_json::from_str(
            &store
                .personalization("chat", "", TEXT, 90, &|_, _| true)
                .unwrap(),
        )
        .unwrap();
        let work: Value = serde_json::from_str(&learn(&store)).unwrap();
        for feature in [
            "exclamations_per_100_words",
            "contractions_per_100_words",
            "lowercase_sentence_start_ratio",
        ] {
            assert!(
                chat["style"][feature].as_f64().unwrap() > work["style"][feature].as_f64().unwrap()
            );
        }
        assert!(
            chat["style"]["mean_sentence_words"].as_f64().unwrap()
                < work["style"]["mean_sentence_words"].as_f64().unwrap()
        );
    }

    #[test]
    fn python_whitespace_and_microsecond_timestamp_semantics_are_preserved() {
        let text = TEXT.replace(' ', "\u{1c}");
        assert!(history_safe(&text));
        assert!(!history_safe(&format!("{TEXT} password\u{1c}: synthetic")));
        assert_eq!(python_timestamp(now()), "2026-04-15T12:00:00+00:00");
        assert_eq!(
            python_timestamp(now().with_nanosecond(123_000_000).unwrap()),
            "2026-04-15T12:00:00.123000+00:00"
        );
        assert_eq!(
            python_timestamp(now().with_nanosecond(123_456_789).unwrap()),
            "2026-04-15T12:00:00.123456+00:00"
        );
    }

    #[test]
    fn sensitive_heuristics_use_python_boundaries_not_rust_unicode_boundaries() {
        for suffix in ["\u{301}", "\u{200d}", "\u{203f}"] {
            assert!(!history_safe(&format!("{TEXT} 4111111111111111{suffix}")));
            assert!(!history_safe(&format!(
                "{TEXT} {suffix}sk_abcdefghijklmnop"
            )));
            assert!(!history_safe(&format!(
                "{TEXT} {suffix}ghp_abcdefghijklmnop"
            )));
            assert!(!history_safe(&format!(
                "{TEXT} {suffix}github_pat_abcdefghijklmnop"
            )));
        }
        assert!(!history_safe(&format!("{TEXT} 4111111111111111 111x")));
        assert!(!history_safe(&format!("{TEXT} x1111 1111 1111 1111 1111")));
        assert!(history_safe(&format!("{TEXT} 41111111111111111111")));
        assert!(history_safe(&format!("{TEXT} xsk_abcdefghijklmnop")));
        assert!(history_safe(&format!(
            "{TEXT} café\u{301} 👩\u{200d}💻 ordinary authored writing"
        )));
    }

    #[test]
    fn missing_default_collection_is_created_but_access_failures_never_create_one() {
        use std::cell::Cell;
        let creates = Cell::new(0);
        let create = || {
            creates.set(creates.get() + 1);
            Ok("new-default")
        };
        assert_eq!(
            resolve_default_collection(|| Ok(Some("existing-default")), create).unwrap(),
            "existing-default"
        );
        assert_eq!(creates.get(), 0);
        assert_eq!(
            resolve_default_collection(|| Ok(None), create).unwrap(),
            "new-default"
        );
        assert_eq!(creates.get(), 1);
        let denied = resolve_default_collection(
            || -> Result<Option<&str>> { bail!("synthetic access denied") },
            create,
        );
        assert!(denied.is_err());
        assert_eq!(creates.get(), 1);
        let cancelled = resolve_default_collection(
            || Ok(None),
            || -> Result<&str> { bail!("synthetic collection creation cancelled") },
        );
        assert!(cancelled.is_err());
    }

    #[test]
    fn legacy_secret_items_unlock_before_read_and_refusal_never_reads() {
        use std::cell::{Cell, RefCell};
        let locked = Cell::new(true);
        let calls = RefCell::new(Vec::new());
        let value = read_unlocked_secret(
            || {
                calls.borrow_mut().push("locked");
                Ok(locked.get())
            },
            || {
                calls.borrow_mut().push("unlock");
                locked.set(false);
                Ok(())
            },
            || {
                calls.borrow_mut().push("read");
                Ok("synthetic-secret")
            },
        )
        .unwrap();
        assert_eq!(value, "synthetic-secret");
        assert_eq!(*calls.borrow(), ["locked", "unlock", "locked", "read"]);

        calls.borrow_mut().clear();
        let result = read_unlocked_secret(
            || Ok(true),
            || {
                calls.borrow_mut().push("unlock");
                Ok(())
            },
            || {
                calls.borrow_mut().push("read");
                Ok("must-not-read")
            },
        );
        assert!(result.is_err());
        assert_eq!(*calls.borrow(), ["unlock"]);

        calls.borrow_mut().clear();
        let result = read_unlocked_secret(
            || Ok(true),
            || -> Result<()> {
                calls.borrow_mut().push("unlock");
                bail!("synthetic cancellation")
            },
            || {
                calls.borrow_mut().push("read");
                Ok("must-not-read")
            },
        );
        assert!(result.is_err());
        assert_eq!(*calls.borrow(), ["unlock"]);

        calls.borrow_mut().clear();
        let value = read_unlocked_secret(
            || Ok(false),
            || {
                calls.borrow_mut().push("unlock");
                Ok(())
            },
            || {
                calls.borrow_mut().push("read");
                Ok("already-unlocked")
            },
        )
        .unwrap();
        assert_eq!(value, "already-unlocked");
        assert_eq!(*calls.borrow(), ["read"]);
    }

    #[cfg(unix)]
    #[test]
    fn database_and_new_directories_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = TempDir::new().unwrap();
        let mut store =
            HistoryStore::with_path_and_key(temporary.path().join("nested/local.sqlite3"), [1; 32]);
        store
            .record_statistics_at("writer", "accepted", true, now().date_naive())
            .unwrap();
        assert_eq!(
            fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(store.path().parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}
