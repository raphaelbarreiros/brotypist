//! Scalar-indexed observations and pure suggestion proposals; no native edit effects.
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, ops::Deref, sync::{Arc, OnceLock}};
use unicode_general_category::{get_general_category, GeneralCategory as G};
use unicode_casefold::UnicodeCaseFold;

pub type Rect = [f64; 4];
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ScalarIndex(pub usize);
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Utf8Offset(pub usize);
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Utf16Offset(pub usize);

/// Exact character boundaries, including the terminal boundary. Interior UTF-8
/// bytes and UTF-16 surrogate offsets deliberately have no scalar counterpart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextIndex {
    utf8: Vec<usize>,
    utf16: Vec<usize>,
}
impl Default for TextIndex { fn default() -> Self { Self::new("") } }
impl TextIndex {
    pub fn new(text: &str) -> Self {
        let mut utf8 = Vec::new();
        let mut utf16 = Vec::new();
        let mut units = 0;
        for (byte, c) in text.char_indices() {
            utf8.push(byte);
            utf16.push(units);
            units += c.len_utf16();
        }
        utf8.push(text.len());
        utf16.push(units);
        Self { utf8, utf16 }
    }
    pub fn scalar_len(&self) -> usize { self.utf8.len().saturating_sub(1) }
    pub fn scalar_to_utf8(&self, index: ScalarIndex) -> Option<Utf8Offset> {
        self.utf8.get(index.0).copied().map(Utf8Offset)
    }
    pub fn scalar_to_utf16(&self, index: ScalarIndex) -> Option<Utf16Offset> {
        self.utf16.get(index.0).copied().map(Utf16Offset)
    }
    pub fn utf8_to_scalar(&self, offset: Utf8Offset) -> Option<ScalarIndex> {
        self.utf8.binary_search(&offset.0).ok().map(ScalarIndex)
    }
    pub fn utf16_to_scalar(&self, offset: Utf16Offset) -> Option<ScalarIndex> {
        self.utf16.binary_search(&offset.0).ok().map(ScalarIndex)
    }
    pub fn scalar_range_to_utf8(&self, start: usize, length: usize) -> Option<std::ops::Range<usize>> {
        let end = start.checked_add(length)?;
        Some(self.scalar_to_utf8(ScalarIndex(start))?.0..self.scalar_to_utf8(ScalarIndex(end))?.0)
    }
    pub fn utf16_range_to_scalar(&self, start: usize, length: usize) -> Option<(usize, usize)> {
        let end = start.checked_add(length)?;
        let first = self.utf16_to_scalar(Utf16Offset(start))?.0;
        let last = self.utf16_to_scalar(Utf16Offset(end))?.0;
        Some((first, last - first))
    }
}

#[derive(Debug)]
struct TextRevision { text: Box<str>, index: OnceLock<TextIndex> }
/// Immutable text revision. Cloning snapshots shares text and its lazily built
/// offset map; replacing text creates a new revision, never a stale cached map.
#[derive(Clone, Debug)]
pub struct TextBuffer(Arc<TextRevision>);
impl TextBuffer {
    pub fn index(&self) -> &TextIndex { self.0.index.get_or_init(|| TextIndex::new(&self.0.text)) }
    pub fn scalar_len(&self) -> usize { self.index().scalar_len() }
    pub fn slice(&self, start: usize, length: usize) -> Option<&str> {
        self.get(self.index().scalar_range_to_utf8(start, length)?)
    }
}
impl From<String> for TextBuffer {
    fn from(text: String) -> Self { Self(Arc::new(TextRevision { text: text.into_boxed_str(), index: OnceLock::new() })) }
}
impl From<&str> for TextBuffer { fn from(text: &str) -> Self { text.to_owned().into() } }
impl Default for TextBuffer { fn default() -> Self { "".into() } }
impl Deref for TextBuffer { type Target = str; fn deref(&self) -> &str { &self.0.text } }
impl AsRef<str> for TextBuffer { fn as_ref(&self) -> &str { self } }
impl fmt::Display for TextBuffer { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self) } }
impl PartialEq for TextBuffer { fn eq(&self, other: &Self) -> bool { Arc::ptr_eq(&self.0, &other.0) || self.0.text == other.0.text } }
impl Eq for TextBuffer {}
impl Serialize for TextBuffer { fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> { serializer.serialize_str(self) } }
impl<'de> Deserialize<'de> for TextBuffer {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> { String::deserialize(deserializer).map(Into::into) }
}

pub(crate) fn is_mark(c: char) -> bool { matches!(get_general_category(c), G::NonspacingMark | G::SpacingMark | G::EnclosingMark) }
pub(crate) fn is_letter(c: char) -> bool { matches!(get_general_category(c), G::UppercaseLetter | G::LowercaseLetter | G::TitlecaseLetter | G::ModifierLetter | G::OtherLetter) }
pub(crate) fn is_alphanumeric(c: char) -> bool { is_letter(c) || matches!(get_general_category(c), G::DecimalNumber | G::LetterNumber | G::OtherNumber) }
pub(crate) fn is_punctuation(c: char) -> bool { matches!(get_general_category(c), G::ConnectorPunctuation | G::DashPunctuation | G::OpenPunctuation | G::ClosePunctuation | G::InitialPunctuation | G::FinalPunctuation | G::OtherPunctuation) }
pub(crate) fn is_space(c: char) -> bool { c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c) }
pub(crate) fn casefold(text: &str) -> String { text.case_fold().collect() }
pub(crate) fn trim(text: &str) -> &str { text.trim_matches(is_space) }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct InlineRender {
    pub font_name: String,
    pub font_size: f64,
    pub baseline: f64,
    pub clip_rect: Rect,
    pub ascent: f64,
    pub descent: f64,
    pub direction: String,
    pub source: String,
    pub font_weight: i32,
    pub italic: bool,
    pub system_font: bool,
    pub system_weight: f64,
    pub text_rgb: Option<[f64; 3]>,
    pub background_rgb: Option<[f64; 3]>,
    pub suffix: String,
    pub suffix_width: f64,
    pub fixed_advance: f64,
}
impl Default for InlineRender {
    fn default() -> Self {
        Self { font_name: String::new(), font_size: 0.0, baseline: 0.0, clip_rect: [0.0;4], ascent: 0.0, descent: 0.0,
            direction: "ltr".into(), source: String::new(), font_weight: 400, italic: false, system_font: false,
            system_weight: 0.0, text_rgb: None, background_rgb: None, suffix: String::new(), suffix_width: 0.0, fixed_advance: 0.0 }
    }
}
impl InlineRender {
    pub fn valid(&self, caret: Option<Rect>) -> bool {
        let Some(caret) = caret else { return false };
        if self.font_name.is_empty() || self.source.is_empty() || self.direction != "ltr"
            || !self.clip_rect.iter().chain(caret.iter()).chain([
                self.font_size, self.baseline, self.ascent, self.descent, self.system_weight,
                self.suffix_width, self.fixed_advance].iter()).all(|v| v.is_finite())
            || !(1.0..=300.0).contains(&self.font_size) || self.ascent <= 0.0 || self.descent < 0.0
            || !(0.0..=300.0).contains(&self.fixed_advance) || self.clip_rect[2] <= 0.0 || self.clip_rect[3] <= 0.0 {
            return false;
        }
        for color in [self.text_rgb, self.background_rgb].into_iter().flatten() {
            if !color.iter().all(|c| c.is_finite() && (0.0..=1.0).contains(c)) { return false; }
        }
        let [x, y, width, height] = self.clip_rect;
        let [cx, cy, _, ch] = caret;
        if !self.suffix.is_empty() {
            if self.suffix.contains(['\r', '\n', '\t']) || self.suffix_width <= 0.0
                || cx + self.suffix_width > x + width
                || (!trim(&self.suffix).is_empty() && (self.text_rgb.is_none() || self.background_rgb.is_none())) { return false; }
        } else if self.suffix_width != 0.0 { return false; }
        ch > 0.0 && x <= cx && x + width - cx >= 1.0 && y <= cy && cy + ch <= y + height + 1.0
            && cy < self.baseline && self.baseline <= cy + ch + 1.0
            && y <= self.baseline - self.ascent && self.baseline + self.descent <= y + height + 1.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Snapshot {
    pub app_id: String,
    pub field_id: String,
    pub text: TextBuffer,
    pub caret: usize,
    pub selection_length: usize,
    pub caret_rect: Option<Rect>,
    pub protected: bool,
    pub domain: String,
    pub multiline: bool,
    pub field_rect: Option<Rect>,
    pub native_anchor: String,
    pub inline_render: Option<InlineRender>,
    pub native_inline: bool,
    pub window_id: String,
    pub window_revision: String,
    pub window_pid: i32,
    pub window_bounds: Option<Rect>,
    pub on_demand_only: bool,
    pub can_replace: bool,
}
impl Default for Snapshot {
    fn default() -> Self {
        Self { app_id: String::new(), field_id: String::new(), text: TextBuffer::default(), caret: 0,
            selection_length: 0, caret_rect: None, protected: false, domain: String::new(), multiline: true,
            field_rect: None, native_anchor: String::new(), inline_render: None, native_inline: false,
            window_id: String::new(), window_revision: String::new(), window_pid: 0, window_bounds: None,
            on_demand_only: false, can_replace: true }
    }
}
/// Writing/policy identity intentionally excludes layout and window authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotKey {
    pub app_id: String, pub field_id: String, pub text: TextBuffer, pub caret: usize,
    pub selection_length: usize, pub protected: bool, pub domain: String,
    pub multiline: bool, pub on_demand_only: bool, pub can_replace: bool,
}
/// Native window/document identity, independent of text and screen position.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Authority {
    pub app_id: String, pub field_id: String, pub domain: String,
    pub window_id: String, pub window_revision: String, pub window_pid: i32,
}
impl SnapshotKey {
    pub fn matches(&self, snapshot: &Snapshot) -> bool {
        self.app_id == snapshot.app_id && self.field_id == snapshot.field_id && self.text == snapshot.text
            && self.caret == snapshot.caret && self.selection_length == snapshot.selection_length
            && self.protected == snapshot.protected && self.domain == snapshot.domain && self.multiline == snapshot.multiline
            && self.on_demand_only == snapshot.on_demand_only && self.can_replace == snapshot.can_replace
    }
}
impl Authority {
    pub fn matches(&self, snapshot: &Snapshot) -> bool {
        self.app_id == snapshot.app_id && self.field_id == snapshot.field_id && self.domain == snapshot.domain
            && self.window_id == snapshot.window_id && self.window_revision == snapshot.window_revision && self.window_pid == snapshot.window_pid
    }
}
impl Snapshot {
    pub fn text_index(&self) -> &TextIndex { self.text.index() }
    pub fn valid_range(&self) -> bool { self.text.index().scalar_range_to_utf8(self.caret, self.selection_length).is_some() }
    pub fn prefix(&self) -> &str { self.text.slice(0, self.caret).unwrap_or("") }
    pub fn suffix(&self) -> &str {
        let Some(length) = self.text.scalar_len().checked_sub(self.caret) else { return "" };
        self.text.slice(self.caret, length).unwrap_or("")
    }
    pub fn selected_text(&self) -> &str { self.text.slice(self.caret, self.selection_length).unwrap_or("") }
    pub fn selection_suffix(&self) -> &str {
        let Some(start) = self.caret.checked_add(self.selection_length) else { return "" };
        let Some(length) = self.text.scalar_len().checked_sub(start) else { return "" };
        self.text.slice(start, length).unwrap_or("")
    }
    pub fn line_suffix(&self) -> &str { self.suffix().split(['\r', '\n']).next().unwrap_or("") }
    pub fn inline_safe(&self) -> bool {
        if self.protected || self.selection_length != 0 || !self.valid_range() { return false; }
        if self.native_inline && !self.native_anchor.is_empty() { return self.line_suffix().is_empty(); }
        self.inline_render.as_ref().is_some_and(|r| r.valid(self.caret_rect) && r.suffix == self.line_suffix())
    }
    pub fn anchor_rect(&self) -> Option<Rect> { self.caret_rect.or(self.field_rect) }
    pub fn has_anchor(&self) -> bool { self.anchor_rect().is_some() || !self.native_anchor.is_empty() }
    pub fn eligible(&self, min_chars: usize) -> bool {
        !self.protected && self.selection_length == 0 && self.valid_range()
            && trim(self.prefix()).chars().count() >= min_chars
    }
    pub fn same_key(&self, other: &Self) -> bool {
        self.app_id == other.app_id && self.field_id == other.field_id && self.text == other.text
            && self.caret == other.caret && self.selection_length == other.selection_length
            && self.protected == other.protected && self.domain == other.domain && self.multiline == other.multiline
            && self.on_demand_only == other.on_demand_only && self.can_replace == other.can_replace
    }
    pub fn same_authority(&self, other: &Self) -> bool {
        self.app_id == other.app_id && self.field_id == other.field_id && self.domain == other.domain
            && self.window_id == other.window_id && self.window_revision == other.window_revision && self.window_pid == other.window_pid
    }
    pub fn key(&self) -> SnapshotKey {
        SnapshotKey { app_id: self.app_id.clone(), field_id: self.field_id.clone(), text: self.text.clone(), caret: self.caret,
            selection_length: self.selection_length, protected: self.protected, domain: self.domain.clone(), multiline: self.multiline,
            on_demand_only: self.on_demand_only, can_replace: self.can_replace }
    }
    pub fn authority(&self) -> Authority {
        Authority { app_id: self.app_id.clone(), field_id: self.field_id.clone(), domain: self.domain.clone(),
            window_id: self.window_id.clone(), window_revision: self.window_revision.clone(), window_pid: self.window_pid }
    }
}

fn accept_end(suggestion: &str, mut end: usize, trailing_space: bool, trailing_punctuation: bool) -> usize {
    if !trailing_space { end = suggestion[..end].trim_end_matches(is_space).len(); }
    if !trailing_punctuation {
        let token_end = suggestion[..end].trim_end_matches(is_space).len();
        let punctuation_start = suggestion[..token_end].trim_end_matches(is_punctuation).len();
        if punctuation_start < token_end && punctuation_start > 0 && !trim(&suggestion[..punctuation_start]).is_empty() { end = punctuation_start; }
    }
    end
}
fn word_end(suggestion: &str, trailing_space: bool, trailing_punctuation: bool) -> usize {
    // Python's \\s*\\S+\\s*: initial separators, one token, then its separators.
    let mut state = 0;
    let mut end = suggestion.len();
    for (i, c) in suggestion.char_indices() {
        match (state, is_space(c)) {
            (0, false) => state = 1,
            (1, true) => state = 2,
            (2, false) => { end = i; break; }
            _ => {}
        }
    }
    accept_end(suggestion, end, trailing_space, trailing_punctuation)
}
pub fn take_word(suggestion: &str, trailing_space: bool, trailing_punctuation: bool) -> (String, String) {
    let end = word_end(suggestion, trailing_space, trailing_punctuation);
    let remainder = &suggestion[end..];
    (suggestion[..end].into(), if !trailing_space && trim(remainder).is_empty() { String::new() } else { remainder.into() })
}

/// A proposed acceptance updates only pure predicted state. Native confirmation,
/// rollback/invalidation and history accounting remain the runtime's job.
#[derive(Clone, Debug, Default)]
pub struct SuggestionState { pub snapshot: Option<Snapshot>, pub text: String, pub generation: u64 }
impl SuggestionState {
    pub fn new() -> Self { Self::default() }
    pub fn invalidate(&mut self) -> u64 {
        self.generation = self.generation.checked_add(1).expect("suggestion generation exhausted");
        self.snapshot = None;
        self.text.clear();
        self.generation
    }
    pub fn offer(&mut self, generation: u64, snapshot: Snapshot, text: String) -> bool {
        if generation != self.generation || !snapshot.eligible(1) { return false; }
        self.snapshot = Some(snapshot);
        self.text = text;
        true
    }
    pub fn advance(&mut self, current: &Snapshot) -> bool {
        let Some(previous) = &self.snapshot else { return false };
        if self.text.is_empty() || !current.eligible(1) || !previous.same_authority(current)
            || current.multiline != previous.multiline || current.on_demand_only != previous.on_demand_only
            || current.can_replace != previous.can_replace || current.caret <= previous.caret { return false; }
        let Some(inserted) = current.text.slice(previous.caret, current.caret - previous.caret) else { return false };
        if !self.text.starts_with(inserted) || !current.prefix().starts_with(previous.prefix())
            || current.prefix().len() != previous.prefix().len() + inserted.len() || current.suffix() != previous.suffix() { return false; }
        self.text.drain(..inserted.len());
        self.snapshot = Some(current.clone());
        self.generation = self.generation.checked_add(1).expect("suggestion generation exhausted");
        true
    }
    pub fn accept(&mut self, current: &Snapshot, full: bool, trailing_space: bool,
                  trailing_punctuation: bool, visible: Option<&str>) -> Option<String> {
        if !self.snapshot.as_ref().is_some_and(|s| s.same_key(current) && s.same_authority(current)) || !current.eligible(1) {
            self.invalidate(); return None;
        }
        if visible.is_some_and(|v| v.is_empty() || !self.text.starts_with(v)) { return None; }
        let limit = visible.map_or(self.text.len(), str::len);
        let mut end = if full { accept_end(&self.text, limit, trailing_space, trailing_punctuation) }
            else { word_end(&self.text, trailing_space, trailing_punctuation) };
        if full && limit < self.text.len() && trailing_space {
            for c in self.text[end..].chars() {
                if !is_space(c) || matches!(c, '\r' | '\n') { break; }
                end += c.len_utf8();
            }
        } else if !full && end > limit && !trim(&self.text[limit..end]).is_empty() { return None; }
        if end == 0 { return None; }
        let mut accepted = self.text[..end].to_owned();
        let mut remainder = self.text[end..].to_owned();
        if !trailing_space && trim(&remainder).is_empty() { remainder.clear(); }
        if trailing_space && !full && remainder.is_empty() && current.suffix().is_empty()
            && !accepted.chars().next_back().is_some_and(is_space) && accepted.chars().any(is_alphanumeric) { accepted.push(' '); }
        let mut predicted = current.clone();
        let mut text = String::with_capacity(current.text.len() + accepted.len());
        text.push_str(current.prefix()); text.push_str(&accepted); text.push_str(current.suffix());
        predicted.text = text.into();
        predicted.caret += accepted.chars().count();
        predicted.caret_rect = None; predicted.inline_render = None; predicted.native_inline = false; predicted.native_anchor.clear();
        self.generation = self.generation.checked_add(1).expect("suggestion generation exhausted");
        self.text = remainder;
        self.snapshot = Some(predicted);
        Some(accepted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(text: &str, caret: usize) -> Snapshot { Snapshot { app_id: "writer".into(), field_id: "body".into(), text: text.into(), caret, ..Default::default() } }
    #[test]
    fn exact_unicode_offsets_and_immutable_revision() {
        let text: TextBuffer = "a🖋e\u{301}界".into();
        let map = text.index();
        assert_eq!(map.scalar_len(), 5);
        assert_eq!(map.scalar_to_utf8(ScalarIndex(2)), Some(Utf8Offset(5)));
        assert_eq!(map.scalar_to_utf16(ScalarIndex(2)), Some(Utf16Offset(3)));
        assert_eq!(map.utf16_to_scalar(Utf16Offset(2)), None);
        assert_eq!(map.utf8_to_scalar(Utf8Offset(2)), None);
        assert_eq!(map.scalar_range_to_utf8(usize::MAX, 1), None);
        assert_eq!(map.utf16_range_to_scalar(1, 2), Some((1, 1)));
        assert!(std::ptr::eq(text.index(), text.clone().index()));
        assert_eq!(serde_json::to_string(&text).unwrap(), "\"a🖋é界\"");
        let mut s = snapshot("Olá 🖋|", 5);
        assert_eq!((s.prefix(), s.suffix()), ("Olá 🖋", "|"));
        s.caret = 99; assert!(!s.eligible(1)); assert_eq!(s.prefix(), "");
        s.caret = 2; s.selection_length = usize::MAX; assert!(!s.valid_range());
    }
    #[test]
    fn generations_suffix_and_repeated_unicode_proposals() {
        let s = snapshot("Olá 🖋|existing suffix", 5);
        let mut state = SuggestionState::new();
        assert!(state.offer(0, s.clone(), " café amanhã".into()));
        assert_eq!(state.accept(&s, false, true, true, None).as_deref(), Some(" café "));
        assert!(!state.offer(0, s, " stale".into()));
        let predicted = state.snapshot.clone().unwrap();
        assert_eq!(predicted.suffix(), "|existing suffix");
        assert_eq!(state.accept(&predicted, true, true, true, None).as_deref(), Some("amanhã"));
        assert_eq!(state.snapshot.as_ref().unwrap().text.as_ref(), "Olá 🖋 café amanhã|existing suffix");
        assert_eq!(state.generation, 2);
    }
    #[test]
    fn visible_word_boundaries_and_whitespace() {
        for (visible, full, space, expected, rest) in [
            (" arr", false, true, None, " arrive tomorrow"),
            (" arrive", false, true, Some(" arrive "), "tomorrow"),
            (" arrive", false, false, Some(" arrive"), " tomorrow"),
            (" arr", true, true, Some(" arr"), "ive tomorrow"),
            (" arrive tom", true, true, Some(" arrive tom"), "orrow"),
        ] {
            let s = snapshot("We will", 7); let mut state = SuggestionState::new();
            state.offer(0, s.clone(), " arrive tomorrow".into());
            assert_eq!(state.accept(&s, full, space, true, Some(visible)).as_deref(), expected);
            assert_eq!(state.text, rest);
        }
        assert_eq!(take_word("  café,\t tomorrow", true, true), ("  café,\t ".into(), "tomorrow".into()));
        assert_eq!(take_word(" café。 ", true, false), (" café".into(), "。 ".into()));
        assert_eq!(take_word("。 ", true, false), ("。 ".into(), "".into()));
    }
    #[test]
    fn authority_and_policy_changes_cannot_carry_predictions() {
        let s = snapshot("We will", 7); let mut changed = s.clone(); changed.window_id = "other-window".into();
        assert!(s.same_key(&changed)); assert!(!s.same_authority(&changed));
        let mut state = SuggestionState::new(); state.offer(0, s.clone(), " arrive".into());
        assert_eq!(state.accept(&changed, true, true, true, None), None); assert!(state.text.is_empty());
        state.offer(state.generation, s.clone(), " arrive".into());
        changed.text = "We will arrive".into(); changed.caret = 14;
        assert!(!state.advance(&changed));
        changed.window_id.clear(); assert!(state.advance(&changed)); assert!(state.text.is_empty());
        assert!(!state.offer(1, s, " stale".into()));
    }
    #[test]
    fn inline_geometry_and_suffix_colors() {
        let caret = [20.0, 10.0, 1.0, 20.0];
        let mut r = InlineRender { font_name: "System".into(), source: "native".into(), font_size: 14.0,
            baseline: 25.0, ascent: 14.0, descent: 3.0, clip_rect: [0.0, 0.0, 100.0, 40.0], ..Default::default() };
        assert!(r.valid(Some(caret))); r.suffix = "host".into(); r.suffix_width = 30.0;
        assert!(!r.valid(Some(caret))); r.text_rgb = Some([0.0;3]); r.background_rgb = Some([1.0;3]); assert!(r.valid(Some(caret)));
        r.suffix_width = 90.0; assert!(!r.valid(Some(caret))); r.suffix_width = 30.0;
        r.text_rgb = Some([f64::NAN, 0.0, 0.0]); assert!(!r.valid(Some(caret)));
        let mut s = snapshot("word\nnext", 4); s.native_inline = true; s.native_anchor = "owned".into(); assert!(s.inline_safe());
        s.caret = 2; assert!(!s.inline_safe());
    }
    #[test]
    fn selection_anchors_and_policy_identity_are_independent_of_geometry() {
        let mut s = snapshot("Olá 🖋 café\nnext", 6); s.selection_length = 4;
        assert_eq!(s.selected_text(), "café"); assert_eq!(s.selection_suffix(), "\nnext");
        assert!(!s.eligible(1)); assert!(!s.has_anchor());
        let key = s.key(); let authority = s.authority();
        s.field_rect = Some([0.0, 0.0, 100.0, 20.0]);
        assert!(s.has_anchor()); assert!(key.matches(&s)); assert!(authority.matches(&s));
        s.window_revision = "another-document".into();
        assert!(key.matches(&s)); assert!(!authority.matches(&s));
        s.selection_length = usize::MAX;
        assert_eq!(s.selected_text(), ""); assert_eq!(s.selection_suffix(), ""); assert!(!s.inline_safe());
    }
    #[test]
    fn word_final_spacing_and_withheld_punctuation_remain_acceptible() {
        let original = snapshot("We will", 7);
        let mut state = SuggestionState::new(); state.offer(0, original.clone(), " arrive".into());
        assert_eq!(state.accept(&original, false, true, true, None).as_deref(), Some(" arrive "));
        let predicted = state.snapshot.clone().unwrap(); let generation = state.generation;
        assert_eq!(state.accept(&predicted, false, true, true, None), None);
        assert_eq!(state.generation, generation);
        let mut state = SuggestionState::new(); state.offer(0, original.clone(), " arrive。 ".into());
        assert_eq!(state.accept(&original, true, false, false, None).as_deref(), Some(" arrive"));
        let predicted = state.snapshot.clone().unwrap();
        assert_eq!(state.accept(&predicted, true, false, false, None).as_deref(), Some("。"));
        assert!(state.text.is_empty());
        let mut state = SuggestionState::new(); state.offer(0, original.clone(), " arrive".into());
        let mut changed = original.clone(); changed.can_replace = false;
        assert_eq!(state.accept(&changed, false, true, true, None), None);
        assert!(state.snapshot.is_none());
    }
}
