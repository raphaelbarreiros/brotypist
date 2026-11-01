//! Latest-only, worker-owned native dictionary assessment. No OS API, lifecycle
//! initialization, dictionary fallback, or persistence lives in this module.
use crate::{
    core::{
        Authority, Snapshot, SnapshotKey, TextBuffer, TextIndex, Utf16Offset, casefold, is_letter,
        is_mark, is_punctuation,
    },
    features::safe_choice,
};
use anyhow::{Result, anyhow};
use std::{
    collections::{BTreeMap, HashMap},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use unicode_general_category::{GeneralCategory as G, get_general_category};
use unicode_normalization::UnicodeNormalization;

pub const TRAILING_LIMIT: usize = 4;
pub const MAX_WORD: usize = 48;
pub const CONTEXT_CHARS: usize = 400;
pub const VERDICT_CACHE: usize = 512;
pub const LANGUAGE_REFRESH: Duration = Duration::from_secs(60);
pub const MAX_LANGUAGES: usize = 4;
pub const HYPOTHESIS_FLOOR: f64 = 0.15;
pub const UNSUPPORTED_CONFIDENCE: f64 = 0.6;
pub const UNSUPPORTED_MASS: f64 = 0.35;
pub const GUESS_DISTANCE: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpellingUnavailable(pub String);
impl std::fmt::Display for SpellingUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for SpellingUnavailable {}
/// An owned native resource scope, dropped on the worker after each check.
/// Implementations may own a thread-affine autorelease pool and need not be Send.
pub trait SpellWorkScope {}
impl<T> SpellWorkScope for T {}
/// All checks and cleanup run on one lazy service worker, except close before
/// any work has started. UTF-16 ranges are (location, length), not byte offsets.
pub trait SpellBackend: Send + 'static {
    fn work_scope(&mut self) -> Result<Box<dyn SpellWorkScope>>;
    fn languages(&mut self) -> Result<(Vec<String>, Vec<String>)>;
    fn identify(&mut self, text: &str) -> Result<BTreeMap<String, f64>>;
    fn misspelled(&mut self, word: &str, language: &str) -> Result<Vec<(usize, usize)>>;
    fn has_completions(&mut self, partial: &str, language: &str) -> Result<bool>;
    fn suggestions(
        &mut self,
        text: &str,
        start: usize,
        length: usize,
        language: &str,
    ) -> Result<(String, Vec<String>)>;
    fn close(&mut self) -> Result<()>;
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpellingResult {
    pub key: SnapshotKey,
    pub authority: Authority,
    pub start: usize,
    pub length: usize,
    pub word: String,
    pub replacement: String,
    pub suspected: bool,
    pub language: String,
    pub reason: String,
    pub finished: bool,
}
impl SpellingResult {
    pub fn matches(&self, snapshot: &Snapshot) -> bool {
        self.key.matches(snapshot) && self.authority.matches(snapshot)
    }
    fn empty(snapshot: &Snapshot, reason: &str) -> Self {
        Self {
            key: snapshot.key(),
            authority: snapshot.authority(),
            start: snapshot.caret,
            length: 0,
            word: String::new(),
            replacement: String::new(),
            suspected: false,
            language: String::new(),
            reason: reason.into(),
            finished: false,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub finished: bool,
    pub tail: String,
    pub context: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CurrentWord {
    Word(Word),
    Skipped,
    NoWord,
}
fn joiner(c: char) -> bool {
    matches!(c, '\'' | '’' | 'ʼ' | '-' | '‐')
}
fn line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}' | '\u{b}' | '\u{c}')
}
fn separator(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\u{a0}' | '\u{202f}')
}
fn code_prefix(c: char) -> bool {
    "@#/\\.$:~=<>&%+*^|`_".contains(c)
}
fn word_char(c: char) -> bool {
    is_letter(c) || is_mark(c) || get_general_category(c) == G::DecimalNumber || c == '_'
}
fn at(text: &TextBuffer, index: usize) -> Option<char> {
    text.slice(index, 1)?.chars().next()
}
fn skippable(word: &str) -> bool {
    if word.chars().count() > MAX_WORD
        || word.chars().filter(|&c| is_letter(c)).count() < 2
        || word
            .chars()
            .any(|c| c == '_' || get_general_category(c) == G::DecimalNumber)
    {
        return true;
    }
    let mut cased = word
        .chars()
        .filter(|&c| is_letter(c) && (c.is_uppercase() || c.is_lowercase()))
        .peekable();
    let count = cased.clone().count();
    if count > 1 && cased.all(char::is_uppercase) {
        return true;
    }
    let mut letters = word.chars().filter(|&c| is_letter(c));
    let mut previous = letters.next();
    for current in letters {
        if previous.is_some_and(char::is_lowercase) && current.is_uppercase() {
            return true;
        }
        previous = Some(current);
    }
    false
}
pub fn current_word(snapshot: &Snapshot) -> CurrentWord {
    if snapshot.protected || snapshot.selection_length != 0 || !snapshot.valid_range() {
        return CurrentWord::Skipped;
    }
    let text = &snapshot.text;
    let caret = snapshot.caret;
    if at(text, caret).is_some_and(word_char) {
        return CurrentWord::NoWord;
    }
    let mut end = caret;
    while end > 0 && caret - end < TRAILING_LIMIT && !at(text, end - 1).is_some_and(word_char) {
        let c = at(text, end - 1).expect("checked scalar boundary");
        if line_break(c) || (!separator(c) && !is_punctuation(c)) {
            return CurrentWord::NoWord;
        }
        end -= 1;
    }
    if end == 0 || !at(text, end - 1).is_some_and(word_char) {
        return CurrentWord::NoWord;
    }
    let mut start = end;
    while start > 0 {
        let c = at(text, start - 1).expect("checked scalar boundary");
        if word_char(c) || (joiner(c) && start > 1 && at(text, start - 2).is_some_and(word_char)) {
            start -= 1;
        } else {
            break;
        }
    }
    let word = text
        .slice(start, end - start)
        .expect("checked scalar range");
    if (start > 0 && at(text, start - 1).is_some_and(code_prefix)) || skippable(word) {
        return CurrentWord::Skipped;
    }
    let gap = text.slice(end, caret - end).expect("checked scalar range");
    let joiner_tail = !gap.is_empty() && gap.chars().all(joiner);
    // Searching farther back than this bound cannot change the retained context.
    let mut context_start = end.saturating_sub(CONTEXT_CHARS);
    if start > context_start {
        let before = text
            .slice(context_start, start - context_start)
            .expect("checked scalar range");
        if let Some((relative, _)) = before
            .chars()
            .enumerate()
            .filter(|(_, c)| line_break(*c))
            .last()
        {
            context_start += relative + 1;
        }
    }
    CurrentWord::Word(Word {
        start,
        end,
        text: word.into(),
        finished: !gap.is_empty() && !joiner_tail,
        tail: if joiner_tail {
            gap.into()
        } else {
            String::new()
        },
        context: text
            .slice(context_start, end - context_start)
            .expect("checked scalar range")
            .into(),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClusterSpan {
    pub original_start: usize,
    pub original_end: usize,
    pub utf16_start: usize,
    pub utf16_end: usize,
}
/// Normalize only derived dictionary text; retain original scalar cluster spans.
pub fn clusters(word: &str) -> (String, Vec<ClusterSpan>) {
    let mut chars = word.char_indices().enumerate().peekable();
    let mut checked = String::with_capacity(word.len());
    let mut spans = Vec::new();
    let mut units = 0;
    while let Some((start, (byte, c))) = chars.next() {
        let mut end = start + 1;
        let mut byte_end = byte + c.len_utf8();
        while let Some(&(index, (offset, next))) = chars.peek() {
            if !is_mark(next) {
                break;
            }
            chars.next();
            end = index + 1;
            byte_end = offset + next.len_utf8();
        }
        let first_unit = units;
        for normalized in word[byte..byte_end].nfc() {
            units += normalized.len_utf16();
            checked.push(normalized);
        }
        spans.push(ClusterSpan {
            original_start: start,
            original_end: end,
            utf16_start: first_unit,
            utf16_end: units,
        });
    }
    (checked, spans)
}
pub fn utf16_slice(text: &str, start: usize, end: usize) -> Option<&str> {
    if end < start {
        return None;
    }
    let map = TextIndex::new(text);
    let first = map.utf16_to_scalar(Utf16Offset(start))?;
    let last = map.utf16_to_scalar(Utf16Offset(end))?;
    let range = map.scalar_range_to_utf8(first.0, last.0 - first.0)?;
    text.get(range)
}
fn indexed_u16_slice(text: &TextBuffer, start: usize, end: usize) -> Option<&str> {
    if end < start {
        return None;
    }
    let first = text.index().utf16_to_scalar(Utf16Offset(start))?.0;
    let last = text.index().utf16_to_scalar(Utf16Offset(end))?.0;
    text.slice(first, last - first)
}
pub fn distance(left: &str, right: &str) -> usize {
    let left: Vec<_> = casefold(left).chars().collect();
    let right: Vec<_> = casefold(right).chars().collect();
    let mut previous2 = vec![0; right.len() + 1];
    let mut previous: Vec<_> = (0..=right.len()).collect();
    let mut row = vec![0; right.len() + 1];
    for (i0, a) in left.iter().enumerate() {
        let i = i0 + 1;
        row[0] = i;
        for (j0, b) in right.iter().enumerate() {
            let j = j0 + 1;
            row[j] = (previous[j] + 1)
                .min(row[j - 1] + 1)
                .min(previous[j - 1] + usize::from(a != b));
            if i > 1 && j > 1 && *a == right[j - 2] && left[i - 2] == *b {
                row[j] = row[j].min(previous2[j - 2] + 1);
            }
        }
        std::mem::swap(&mut previous2, &mut previous);
        std::mem::swap(&mut previous, &mut row);
    }
    previous[right.len()]
}
fn match_case(original: &str, suggestion: &str) -> String {
    let mut chars = suggestion.chars();
    if let Some(first) = chars.next() {
        if original.chars().next().is_some_and(char::is_uppercase) && first.is_lowercase() {
            return first.to_uppercase().chain(chars).collect();
        }
    }
    suggestion.into()
}
fn base(tag: &str) -> String {
    casefold(tag.split(['-', '_']).next().unwrap_or(""))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LanguageStatus {
    Ok,
    Unsupported,
    Unavailable,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguagePlan {
    pub status: LanguageStatus,
    pub languages: Vec<String>,
    pub detected: String,
}
pub fn plan_languages(
    hypotheses: &BTreeMap<String, f64>,
    available: &[String],
    preferred: &[String],
) -> LanguagePlan {
    let mut variants = HashMap::new();
    for tag in preferred
        .iter()
        .filter(|tag| available.contains(tag))
        .chain(available.iter())
    {
        variants.entry(base(tag)).or_insert_with(|| tag.clone());
    }
    let mut ranked: Vec<_> = hypotheses
        .iter()
        .filter(|(tag, p)| !tag.is_empty() && tag.as_str() != "und" && p.is_finite() && **p > 0.0)
        .map(|(tag, p)| (*p, tag.as_str()))
        .collect();
    ranked.sort_by(|(a, at), (b, bt)| b.total_cmp(a).then_with(|| at.cmp(bt)));
    let detected = ranked.first().map_or("", |(_, tag)| *tag).to_owned();
    let supported: f64 = ranked
        .iter()
        .filter(|(_, tag)| variants.contains_key(&base(tag)))
        .map(|(p, _)| p)
        .sum();
    let unsupported: f64 = ranked
        .iter()
        .filter(|(_, tag)| !variants.contains_key(&base(tag)))
        .map(|(p, _)| p)
        .sum();
    if ranked.first().is_some_and(|(p, tag)| {
        !variants.contains_key(&base(tag))
            && (*p >= UNSUPPORTED_CONFIDENCE
                || (unsupported >= UNSUPPORTED_MASS && unsupported > supported))
    }) {
        return LanguagePlan {
            status: LanguageStatus::Unsupported,
            languages: Vec::new(),
            detected,
        };
    }
    let mut languages = Vec::new();
    for (p, tag) in ranked {
        if p >= HYPOTHESIS_FLOOR {
            if let Some(variant) = variants.get(&base(tag)) {
                if !languages.contains(variant) {
                    languages.push(variant.clone());
                }
            }
        }
    }
    for tag in preferred {
        if available.contains(tag) && !languages.contains(tag) {
            languages.push(tag.clone());
        }
    }
    languages.truncate(MAX_LANGUAGES);
    LanguagePlan {
        status: if languages.is_empty() {
            LanguageStatus::Unavailable
        } else {
            LanguageStatus::Ok
        },
        languages,
        detected,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    key: SnapshotKey,
    authority: Authority,
}
impl Identity {
    fn of(snapshot: &Snapshot) -> Self {
        Self {
            key: snapshot.key(),
            authority: snapshot.authority(),
        }
    }
}
struct Job {
    identity: Identity,
    word: Word,
}
impl Job {
    fn result(
        &self,
        reason: &str,
        start: usize,
        end: usize,
        replacement: String,
        language: String,
    ) -> SpellingResult {
        let word = if self.word.start <= start && start <= end && end <= self.word.end {
            self.identity
                .key
                .text
                .slice(start, end - start)
                .unwrap_or("")
                .into()
        } else {
            String::new()
        };
        SpellingResult {
            key: self.identity.key.clone(),
            authority: self.identity.authority.clone(),
            start,
            length: end.saturating_sub(start),
            word,
            replacement,
            suspected: reason == "typo",
            language,
            reason: reason.into(),
            finished: self.word.finished,
        }
    }
    fn whole_result(&self, reason: &str, language: String) -> SpellingResult {
        self.result(
            reason,
            self.word.start,
            self.word.end,
            String::new(),
            language,
        )
    }
}
struct BackendOwner(Box<dyn SpellBackend>, bool);
impl BackendOwner {
    fn try_close(&mut self) -> bool {
        if self.1 {
            return true;
        }
        self.1 = matches!(
            catch_unwind(AssertUnwindSafe(|| self.0.close())),
            Ok(Ok(()))
        );
        self.1
    }
}
impl Drop for BackendOwner {
    fn drop(&mut self) {
        if !self.1 {
            let _ = self.try_close();
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CacheKey {
    original: String,
    checked: String,
    finished: bool,
    tail: String,
    languages: Vec<String>,
}
#[derive(Clone)]
struct Verdict {
    reason: &'static str,
    start: usize,
    end: usize,
    replacement: String,
    language: String,
}
struct CachedVerdict {
    value: Verdict,
    used: u64,
}
struct Evaluator {
    backend: BackendOwner,
    languages: Option<(Vec<String>, Vec<String>)>,
    languages_at: Option<Instant>,
    verdicts: HashMap<CacheKey, CachedVerdict>,
    clock: u64,
}
impl Evaluator {
    fn new(backend: BackendOwner) -> Self {
        Self {
            backend,
            languages: None,
            languages_at: None,
            verdicts: HashMap::new(),
            clock: 0,
        }
    }
    fn check(&mut self, job: &Job) -> Result<SpellingResult> {
        let _scope = self.backend.0.work_scope()?;
        if self
            .languages_at
            .is_none_or(|at| at.elapsed() > LANGUAGE_REFRESH)
        {
            self.languages = Some(self.backend.0.languages()?);
            self.languages_at = Some(Instant::now());
            self.verdicts.clear();
        }
        let hypotheses = self.backend.0.identify(&job.word.context)?;
        let (available, preferred) = self.languages.as_ref().expect("language refresh succeeded");
        let plan = plan_languages(&hypotheses, available, preferred);
        if plan.status == LanguageStatus::Unsupported {
            return Ok(job.whole_result("unsupported-language", plan.detected));
        }
        if plan.status != LanguageStatus::Ok {
            return Ok(job.whole_result("unavailable", plan.detected));
        }
        let (checked, spans) = clusters(&job.word.text);
        let key = CacheKey {
            original: job.word.text.clone(),
            checked: checked.clone(),
            finished: job.word.finished,
            tail: job.word.tail.nfc().collect(),
            languages: plan.languages,
        };
        self.clock = self.clock.wrapping_add(1);
        let verdict = if let Some(cached) = self.verdicts.get_mut(&key) {
            cached.used = self.clock;
            cached.value.clone()
        } else {
            let verdict = self.verdict(&checked, &spans, &key)?;
            if self.verdicts.len() >= VERDICT_CACHE {
                if let Some(oldest) = self
                    .verdicts
                    .iter()
                    .min_by_key(|(_, entry)| entry.used)
                    .map(|(key, _)| key.clone())
                {
                    self.verdicts.remove(&oldest);
                }
            }
            self.verdicts.insert(
                key,
                CachedVerdict {
                    value: verdict.clone(),
                    used: self.clock,
                },
            );
            verdict
        };
        Ok(job.result(
            verdict.reason,
            job.word.start + verdict.start,
            job.word.start + verdict.end,
            verdict.replacement,
            verdict.language,
        ))
    }
    fn verdict(&mut self, checked: &str, spans: &[ClusterSpan], key: &CacheKey) -> Result<Verdict> {
        let whole = key.original.chars().count();
        let result = |reason, start, end, replacement, language| Verdict {
            reason,
            start,
            end,
            replacement,
            language,
        };
        let mut primary_ranges = Vec::new();
        for (i, language) in key.languages.iter().enumerate() {
            let ranges: Vec<_> = self
                .backend
                .0
                .misspelled(checked, language)?
                .into_iter()
                .filter(|(_, length)| *length > 0)
                .collect();
            if ranges.is_empty() {
                return Ok(result("ok", 0, whole, String::new(), language.clone()));
            }
            if i == 0 {
                primary_ranges = ranges;
            }
        }
        if !key.finished {
            let partial = format!("{checked}{}", key.tail);
            for language in &key.languages {
                if self.backend.0.has_completions(&partial, language)? {
                    return Ok(result("partial", 0, whole, String::new(), language.clone()));
                }
            }
        }
        let primary = key
            .languages
            .first()
            .ok_or_else(|| anyhow!("no spelling language"))?
            .clone();
        let units = spans
            .last()
            .ok_or_else(|| anyhow!("empty spelling word"))?
            .utf16_end;
        let (location, length) = primary_ranges
            .last()
            .copied()
            .ok_or_else(|| anyhow!("missing native spelling range"))?;
        let stop = location
            .checked_add(length)
            .unwrap_or(usize::MAX)
            .min(units);
        let location = location.min(units);
        if stop <= location {
            return Ok(result("typo", 0, whole, String::new(), primary));
        }
        let first = spans
            .iter()
            .find(|s| s.utf16_end > location)
            .ok_or_else(|| anyhow!("invalid spelling range"))?;
        let last = spans
            .iter()
            .rev()
            .find(|s| s.utf16_start < stop)
            .ok_or_else(|| anyhow!("invalid spelling range"))?;
        let checked: TextBuffer = checked.into();
        let flagged = indexed_u16_slice(&checked, location, stop)
            .ok_or_else(|| anyhow!("spelling range splits a surrogate"))?;
        let (correction, guesses) =
            self.backend
                .0
                .suggestions(&checked, location, stop - location, &primary)?;
        let suggestion = if !correction.is_empty() {
            correction.as_str()
        } else {
            guesses
                .first()
                .filter(|g| flagged.chars().count() >= 3 && distance(flagged, g) <= GUESS_DISTANCE)
                .map_or("", String::as_str)
        };
        let mut replacement = String::new();
        if !suggestion.is_empty() && suggestion != flagged && safe_choice(suggestion) {
            let leading = indexed_u16_slice(&checked, first.utf16_start, location)
                .ok_or_else(|| anyhow!("spelling range splits a surrogate"))?;
            let trailing = indexed_u16_slice(&checked, stop, last.utf16_end)
                .ok_or_else(|| anyhow!("spelling range splits a surrogate"))?;
            replacement = format!("{leading}{}{trailing}", match_case(flagged, suggestion));
            let original: TextBuffer = key.original.as_str().into();
            if original.slice(
                first.original_start,
                last.original_end - first.original_start,
            ) == Some(replacement.as_str())
            {
                replacement.clear();
            }
        }
        Ok(result(
            "typo",
            first.original_start,
            last.original_end,
            replacement,
            primary,
        ))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
struct ServiceState {
    pending: Option<Job>,
    inflight: Option<Identity>,
    desired: Option<Identity>,
    latest: Option<SpellingResult>,
    closed: bool,
    available: bool,
    backend: Option<BackendOwner>,
    worker: Option<JoinHandle<()>>,
    worker_done: bool,
    diagnostic: String,
    cleanup_epoch: u64,
    finalizing: bool,
}
struct Shared {
    state: Mutex<ServiceState>,
    changed: Condvar,
}
pub type Wake = Arc<dyn Fn() + Send + Sync + 'static>;
pub struct SpellingService {
    shared: Arc<Shared>,
    wake: Option<Wake>,
}
impl SpellingService {
    pub fn new(backend: Option<Box<dyn SpellBackend>>, wake: Option<Wake>) -> Self {
        let available = backend.is_some();
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(ServiceState {
                    pending: None,
                    inflight: None,
                    desired: None,
                    latest: None,
                    closed: false,
                    available,
                    backend: backend.map(|backend| BackendOwner(backend, false)),
                    worker: None,
                    worker_done: false,
                    cleanup_epoch: 0,
                    finalizing: false,
                    diagnostic: if available {
                        String::new()
                    } else {
                        "native spelling backend unavailable".into()
                    },
                }),
                changed: Condvar::new(),
            }),
            wake,
        }
    }
    pub fn diagnostic(&self) -> String {
        lock(&self.shared.state).diagnostic.clone()
    }
    pub fn assess(&self, snapshot: &Snapshot) -> Option<SpellingResult> {
        let mut state = lock(&self.shared.state);
        if !state.available || state.closed {
            return Some(SpellingResult::empty(snapshot, "unavailable"));
        }
        let word = match current_word(snapshot) {
            CurrentWord::Word(word) => word,
            other => {
                state.desired = None;
                state.pending = None;
                return Some(SpellingResult::empty(
                    snapshot,
                    if other == CurrentWord::Skipped {
                        "skipped"
                    } else {
                        "no-word"
                    },
                ));
            }
        };
        let identity = Identity::of(snapshot);
        state.desired = Some(identity.clone());
        if state.latest.as_ref().is_some_and(|result| {
            result.key == identity.key && result.authority == identity.authority
        }) {
            state.pending = None;
            return state.latest.clone();
        }
        if state.inflight.as_ref() == Some(&identity) {
            state.pending = None;
        } else if state
            .pending
            .as_ref()
            .is_none_or(|job| job.identity != identity)
        {
            state.pending = Some(Job { identity, word });
        }
        if state.worker.is_none() {
            // Move the backend only after thread creation succeeds. A failed
            // spawn must not silently discard an unclosed native owner.
            let (sender, receiver) = std::sync::mpsc::channel();
            let shared = Arc::clone(&self.shared);
            let wake = self.wake.clone();
            match thread::Builder::new()
                .name("brotypist-spelling".into())
                .spawn(move || {
                    if let Ok(backend) = receiver.recv() {
                        run(shared, backend, wake);
                    }
                }) {
                Ok(worker) => {
                    let backend = state
                        .backend
                        .take()
                        .expect("available backend before worker start");
                    if let Err(error) = sender.send(backend) {
                        state.backend = Some(error.0);
                        let _ = worker.join();
                        state.available = false;
                        state.pending = None;
                        state.desired = None;
                        state.diagnostic = "spelling worker startup failed".into();
                        return Some(SpellingResult::empty(snapshot, "unavailable"));
                    }
                    state.worker = Some(worker);
                }
                Err(_) => {
                    state.available = false;
                    state.pending = None;
                    state.desired = None;
                    state.diagnostic = "spelling worker unavailable".into();
                    return Some(SpellingResult::empty(snapshot, "unavailable"));
                }
            }
        }
        self.shared.changed.notify_all();
        None
    }
    pub fn invalidate(&self) {
        let mut state = lock(&self.shared.state);
        state.pending = None;
        state.desired = None;
        state.latest = None;
    }
    /// Closing is idempotent and never interrupts an uncertain native call. Wait
    /// at most 500ms; a still-running worker owns cleanup until that call returns.
    pub fn close(&self) -> bool {
        let mut state = lock(&self.shared.state);
        state.closed = true;
        state.pending = None;
        state.desired = None;
        state.latest = None;
        state.cleanup_epoch = state.cleanup_epoch.wrapping_add(1);
        self.shared.changed.notify_all();
        if state.worker.is_none() {
            if !state.backend.as_mut().is_none_or(BackendOwner::try_close) {
                state.diagnostic = "native spelling cleanup pending".into();
                return false;
            }
            state.backend = None;
            return true;
        }
        if state
            .worker
            .as_ref()
            .is_some_and(|worker| worker.thread().id() == thread::current().id())
        {
            return false;
        }
        let (mut state, _) = self
            .shared
            .changed
            .wait_timeout_while(state, Duration::from_millis(500), |state| {
                !state.worker_done
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.worker_done {
            return false;
        }
        let worker = state.worker.take();
        drop(state);
        if let Some(worker) = worker {
            let _ = worker.join();
        }
        true
    }
    pub fn is_stopped(&self) -> bool {
        let state = lock(&self.shared.state);
        state.closed && state.backend.is_none() && state.worker.is_none()
    }
}
impl Drop for SpellingService {
    fn drop(&mut self) {
        loop {
            if self.close() {
                return;
            }
            let mut state = lock(&self.shared.state);
            // A wake callback can release the last service reference on this
            // worker; that worker still owns and performs its own cleanup.
            if state
                .worker
                .as_ref()
                .is_some_and(|worker| worker.thread().id() == thread::current().id())
            {
                state.finalizing = true;
                self.shared.changed.notify_all();
                return;
            }
            drop(state);
            // Final ownership fallback, not a normal UI Quit path. A backend
            // that never returns/cannot close can keep Drop waiting indefinitely.
            thread::sleep(Duration::from_millis(10));
        }
    }
}
fn run(shared: Arc<Shared>, backend: BackendOwner, wake: Option<Wake>) {
    let mut evaluator = Evaluator::new(backend);
    loop {
        let job = {
            let mut state = lock(&shared.state);
            while state.pending.is_none() && !state.closed {
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            if state.closed {
                break;
            }
            let job = state.pending.take().expect("pending job");
            state.inflight = Some(job.identity.clone());
            job
        };
        let evaluated = catch_unwind(AssertUnwindSafe(|| evaluator.check(&job)));
        let (result, diagnostic) = match evaluated {
            Ok(Ok(result)) => (result, String::new()),
            Ok(Err(_)) => (
                job.whole_result("unavailable", String::new()),
                "native spelling check unavailable".into(),
            ),
            Err(_) => (
                job.whole_result("unavailable", String::new()),
                "native spelling check panicked".into(),
            ),
        };
        let publish = {
            let mut state = lock(&shared.state);
            state.inflight = None;
            state.diagnostic = diagnostic;
            let publish = !state.closed
                && state.pending.is_none()
                && state.desired.as_ref() == Some(&job.identity);
            if publish {
                state.latest = Some(result);
            }
            publish
        };
        if publish {
            if let Some(wake) = &wake {
                let _ = catch_unwind(AssertUnwindSafe(|| wake()));
            }
        }
    }
    // Failed cleanup remains owned by this live worker. Another close call
    // requests a retry; merely timing out never detaches/restarts the backend.
    loop {
        let epoch = lock(&shared.state).cleanup_epoch;
        if evaluator.backend.try_close() {
            break;
        }
        let mut state = lock(&shared.state);
        state.diagnostic = "native spelling cleanup pending".into();
        shared.changed.notify_all();
        while state.cleanup_epoch == epoch {
            if state.finalizing {
                let (next, _) = shared
                    .changed
                    .wait_timeout(state, Duration::from_millis(100))
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state = next;
                break;
            }
            state = shared
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
    drop(evaluator);
    let mut state = lock(&shared.state);
    state.inflight = None;
    state.worker_done = true;
    shared.changed.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    fn snapshot(text: &str) -> Snapshot {
        Snapshot {
            app_id: "mail".into(),
            field_id: "body".into(),
            text: text.into(),
            caret: text.chars().count(),
            ..Default::default()
        }
    }
    struct Dictionary {
        hypotheses: BTreeMap<String, f64>,
        entered: Option<mpsc::Sender<()>>,
        gate: Option<mpsc::Receiver<()>>,
        checked: Arc<Mutex<Vec<(String, String)>>>,
        fail_check: bool,
        close_allowed: Option<Arc<AtomicBool>>,
    }
    impl SpellBackend for Dictionary {
        fn work_scope(&mut self) -> Result<Box<dyn SpellWorkScope>> {
            Ok(Box::new(()))
        }
        fn languages(&mut self) -> Result<(Vec<String>, Vec<String>)> {
            Ok((vec!["en".into(), "pt_BR".into()], vec!["en".into()]))
        }
        fn identify(&mut self, _: &str) -> Result<BTreeMap<String, f64>> {
            Ok(self.hypotheses.clone())
        }
        fn misspelled(&mut self, text: &str, language: &str) -> Result<Vec<(usize, usize)>> {
            if self.fail_check {
                return Err(anyhow!("native dictionary unavailable"));
            }
            if let Some(entered) = self.entered.take() {
                entered.send(()).unwrap();
            }
            if let Some(gate) = self.gate.take() {
                gate.recv_timeout(Duration::from_secs(3)).unwrap();
            }
            lock(&self.checked).push((text.into(), language.into()));
            assert_eq!(text.nfc().collect::<String>(), text);
            let words: &[&str] = if language == "en" {
                &[
                    "we", "will", "receive", "send", "feedback", "the", "well", "known", "it's",
                ]
            } else {
                &["até", "amanhã", "café", "você"]
            };
            let mut ranges = Vec::new();
            let mut offset = 0;
            for part in text.split('-') {
                let units = part.encode_utf16().count();
                if !words.contains(&casefold(part).as_str()) {
                    ranges.push((offset, units));
                }
                offset += units + 1;
            }
            Ok(ranges)
        }
        fn has_completions(&mut self, partial: &str, language: &str) -> Result<bool> {
            let words: &[&str] = if language == "en" {
                &["receive", "feedback", "it's"]
            } else {
                &["amanhã", "café"]
            };
            let folded = casefold(partial);
            Ok(words
                .iter()
                .any(|word| word.starts_with(&folded) && *word != folded))
        }
        fn suggestions(
            &mut self,
            text: &str,
            start: usize,
            length: usize,
            _: &str,
        ) -> Result<(String, Vec<String>)> {
            let word = utf16_slice(text, start, start + length).unwrap();
            if word == "recieve" {
                return Ok((String::new(), vec!["receive".into(), "unrelated".into()]));
            }
            Ok((
                match word {
                    "amánha" => "amanhã",
                    "knwon" => "known",
                    _ => "",
                }
                .into(),
                Vec::new(),
            ))
        }
        fn close(&mut self) -> Result<()> {
            if self
                .close_allowed
                .as_ref()
                .is_some_and(|allowed| !allowed.load(Ordering::SeqCst))
            {
                return Err(anyhow!("native cleanup pending"));
            }
            Ok(())
        }
    }
    fn dictionary(hypotheses: &[(&str, f64)]) -> Dictionary {
        Dictionary {
            hypotheses: hypotheses
                .iter()
                .map(|(tag, p)| ((*tag).into(), *p))
                .collect(),
            entered: None,
            gate: None,
            checked: Arc::new(Mutex::new(Vec::new())),
            fail_check: false,
            close_allowed: None,
        }
    }
    fn settle(service: &SpellingService, snapshot: &Snapshot) -> SpellingResult {
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(result) = service.assess(snapshot) {
                return result;
            }
            assert!(Instant::now() < until, "spelling never settled");
            thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn typo_ranges_after_astral_and_cluster_normalization() {
        let service = SpellingService::new(Some(Box::new(dictionary(&[("en", 0.95)]))), None);
        for tail in ["", ". "] {
            let s = snapshot(&format!("🖋🙂 we will recieve{tail}"));
            let result = settle(&service, &s);
            assert_eq!((result.start, result.length), (11, 7));
            assert_eq!(result.word, "recieve");
            assert_eq!(result.replacement, "receive");
            assert!(result.suspected);
            assert_eq!(result.finished, !tail.is_empty());
        }
        service.close();
        let service = SpellingService::new(Some(Box::new(dictionary(&[("pt", 0.9)]))), None);
        for word in ["amánha", "ama\u{301}nha", "amánha"] {
            let text = format!("🙂 Até {word} na terça.");
            let mut s = snapshot(&text);
            s.caret = 6 + word.chars().count();
            let result = settle(&service, &s);
            assert_eq!((result.start, result.length), (6, word.chars().count()));
            assert_eq!(result.replacement, "amanhã");
            assert_eq!(result.language, "pt_BR");
        }
        assert_eq!(settle(&service, &snapshot("Até cafe\u{301}")).reason, "ok");
    }
    #[test]
    fn partial_finished_compound_and_dictionary_veto() {
        let service = SpellingService::new(Some(Box::new(dictionary(&[("en", 0.95)]))), None);
        assert_eq!(
            settle(&service, &snapshot("send the feedb")).reason,
            "partial"
        );
        assert!(settle(&service, &snapshot("send the feedb ")).suspected);
        let result = settle(&service, &snapshot("it is well-knwon "));
        assert_eq!(result.word, "knwon");
        assert_eq!(result.start, 11);
        assert_eq!(result.replacement, "known");
        assert_eq!(settle(&service, &snapshot("it's")).reason, "ok");
        let service = SpellingService::new(
            Some(Box::new(dictionary(&[("en", 0.55), ("pt", 0.4)]))),
            None,
        );
        let result = settle(&service, &snapshot("see you amanhã"));
        assert_eq!(result.reason, "ok");
        assert_eq!(result.language, "pt_BR");
    }
    #[test]
    fn unsupported_and_missing_backends_never_become_english_success() {
        let backend = dictionary(&[("sw", 0.92), ("en", 0.03)]);
        let checked = backend.checked.clone();
        let service = SpellingService::new(Some(Box::new(backend)), None);
        let result = settle(&service, &snapshot("ninakupenda sana rafikii"));
        assert_eq!(result.reason, "unsupported-language");
        assert_eq!(result.language, "sw");
        assert!(!result.suspected);
        assert!(lock(&checked).is_empty());
        let unavailable = SpellingService::new(None, None);
        let result = unavailable.assess(&snapshot("word")).unwrap();
        assert_eq!(result.reason, "unavailable");
        assert!(!unavailable.diagnostic().is_empty());
        assert!(!result.suspected);
    }
    #[test]
    fn extraction_preserves_code_and_typo_boundaries() {
        for text in [
            "@recieve",
            "snake_case",
            "camelCase",
            "HTTP",
            "word123",
            "a",
        ] {
            assert_eq!(
                current_word(&snapshot(text)),
                CurrentWord::Skipped,
                "{text}"
            );
        }
        for text in ["recieve\n", "recieve🙂", "recieve     ", "recieve\u{2028}"] {
            assert_eq!(current_word(&snapshot(text)), CurrentWord::NoWord, "{text}");
        }
        let CurrentWord::Word(word) = current_word(&snapshot("isn'")) else {
            panic!("missing unfinished contraction")
        };
        assert_eq!(word.tail, "'");
        assert!(!word.finished);
        let mut s = snapshot("we recieve");
        s.caret = 4;
        assert_eq!(current_word(&s), CurrentWord::NoWord);
        s.protected = true;
        assert_eq!(current_word(&s), CurrentWord::Skipped);
        let CurrentWord::Word(word) = current_word(&snapshot("前段\n中文詞語")) else {
            panic!("missing CJK prose")
        };
        assert_eq!(word.context, "中文詞語");
        assert_eq!(utf16_slice("🖋abc", 1, 2), None);
        assert_eq!(utf16_slice("🖋abc", 2, 5), Some("abc"));
        assert_eq!(distance("Straße", "STRASSE"), 0);
        assert_eq!(distance("recieve", "receive"), 1);
    }
    #[test]
    fn native_cluster_ranges_expand_without_rewriting_authored_text() {
        let (checked, spans) = clusters("a\u{301}🖋e\u{301}");
        assert_eq!(checked, "á🖋é");
        assert_eq!(
            spans,
            vec![
                ClusterSpan {
                    original_start: 0,
                    original_end: 2,
                    utf16_start: 0,
                    utf16_end: 1
                },
                ClusterSpan {
                    original_start: 2,
                    original_end: 3,
                    utf16_start: 1,
                    utf16_end: 3
                },
                ClusterSpan {
                    original_start: 3,
                    original_end: 5,
                    utf16_start: 3,
                    utf16_end: 4
                }
            ]
        );
        let available = vec!["en".into(), "pt_BR".into(), "pt_PT".into()];
        let plan = plan_languages(
            &BTreeMap::from([("pt".into(), 0.9)]),
            &available,
            &["pt_PT".into(), "en".into()],
        );
        assert_eq!(plan.languages, vec!["pt_PT", "en"]);
    }
    #[test]
    fn latest_only_window_authority_and_skip_revocation() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (gate_tx, gate_rx) = mpsc::channel();
        let mut backend = dictionary(&[("en", 0.95)]);
        backend.entered = Some(entered_tx);
        backend.gate = Some(gate_rx);
        let (wake_tx, wake_rx) = mpsc::channel();
        let wake = Arc::new(move || {
            let _ = wake_tx.send(());
        });
        let service = SpellingService::new(Some(Box::new(backend)), Some(wake));
        let first = snapshot("we will recieve");
        let mut other = first.clone();
        other.window_id = "other".into();
        assert!(service.assess(&first).is_none());
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(service.assess(&other).is_none());
        gate_tx.send(()).unwrap();
        wake_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let result = service.assess(&other).unwrap();
        assert!(result.matches(&other));
        assert!(!result.matches(&first));
        assert!(service.assess(&first).is_none());
        let mut skipped = first.clone();
        skipped.protected = true;
        assert_eq!(service.assess(&skipped).unwrap().reason, "skipped");
        service.close();
        assert_eq!(service.assess(&first).unwrap().reason, "unavailable");
    }
    #[test]
    fn backend_errors_are_unavailable_and_cleanup_is_retryable() {
        let allowed = Arc::new(AtomicBool::new(false));
        let mut backend = dictionary(&[("en", 0.95)]);
        backend.fail_check = true;
        backend.close_allowed = Some(allowed.clone());
        let service = SpellingService::new(Some(Box::new(backend)), None);
        let result = settle(&service, &snapshot("recieve"));
        assert_eq!(result.reason, "unavailable");
        assert!(!result.suspected);
        assert!(result.replacement.is_empty());
        assert!(!service.close());
        assert!(!service.is_stopped());
        assert_eq!(
            service.assess(&snapshot("other")).unwrap().reason,
            "unavailable"
        );
        allowed.store(true, Ordering::SeqCst);
        assert!(service.close());
        assert!(service.is_stopped());
        assert!(service.close());
    }
    #[test]
    fn closing_a_live_native_check_retains_owner_until_it_returns() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (gate_tx, gate_rx) = mpsc::channel();
        let mut backend = dictionary(&[("en", 0.95)]);
        backend.entered = Some(entered_tx);
        backend.gate = Some(gate_rx);
        let service = SpellingService::new(Some(Box::new(backend)), None);
        assert!(service.assess(&snapshot("recieve")).is_none());
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!service.close());
        assert!(!service.is_stopped());
        gate_tx.send(()).unwrap();
        assert!(service.close());
        assert!(service.is_stopped());
    }
}
