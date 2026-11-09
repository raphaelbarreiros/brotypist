//! Ordinary AX geometry. Coordinates stay in AX's top-left screen points.
//! Native observations are authority: incomplete trees, offsets, fonts and rows
//! are unavailable rather than estimated. Tests inject the AX/font boundaries.
use super::ax::{AxAttributedString, AxElement, AxRange, AxValue};
use crate::core::{InlineRender, Rect, ScalarIndex, Snapshot, TextBuffer, TextIndex, Utf16Offset};
use objc2::{AnyThread, rc::Retained, runtime::AnyObject};
use objc2_app_kit::{NSFont, NSFontAttributeName, NSLayoutManager};
use objc2_foundation::{NSAttributedString, NSDictionary, NSString};
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    time::{Duration, Instant},
};
use unicode_bidi::{BidiClass, bidi_class};
use unicode_normalization::char::canonical_combining_class;

#[derive(Clone, Debug, PartialEq)]
pub struct AppearanceSample {
    pub text: String,
    pub rect: Rect,
}
#[derive(Clone, Debug, PartialEq)]
pub struct AppearanceEvidence {
    pub font_name: String,
    pub font_family: String,
    pub font_size: f64,
    pub samples: Vec<AppearanceSample>,
    pub caret_rect: Rect,
    pub clip_rect: Rect,
    pub suffix: Option<AppearanceSample>,
    pub ax_render: Option<InlineRender>,
    /// Independently observed terminal cell advance; zero for ordinary AX text.
    pub fixed_advance: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum GapFailure {
    SampleEndUnavailable,
    CaretUnavailable,
    GapUnavailable,
    RawEndpointRowMismatch,
    RawEndpointHeightMismatch,
    GapHeightMismatch,
    CapturedCaretHeightMismatch,
    CaretXMismatch,
    CaretRowMismatch,
    EndpointOrderInvalid,
    NormalizedEndpointRowMismatch,
    GapRowMismatch,
    GapStartMismatch,
    GapEndMismatch,
    SampleEndClipped,
    GapClipped,
}

/// Opt-in, pass-local observations only: no text, ranges, counts or identities.
/// The bridge may retain a failure only after its final snapshot barriers pass.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub(super) struct GapDiagnostic {
    reason: Option<GapFailure>,
    captured_caret: Rect,
    clip: Rect,
    raw_sample_end: Option<Rect>,
    raw_caret: Option<Rect>,
    raw_gap: Option<Rect>,
    normalized_sample_end: Option<Rect>,
    normalized_caret: Option<Rect>,
}
impl GapDiagnostic {
    pub(super) fn retain_failure(self, retained: &mut Option<Self>) {
        if self.reason.is_some() {
            *retained = Some(self);
        }
    }
}

const MAX_NODES: usize = 128;
const MAX_DEPTH: usize = 8;
const MAX_MAPPING_WORK: usize = 4096;
const TREE_TIME: Duration = Duration::from_millis(50);
const ZOOM_LEVELS: [f64; 18] = [
    0.25,
    1.0 / 3.0,
    0.5,
    2.0 / 3.0,
    0.75,
    0.8,
    0.85,
    0.9,
    1.1,
    1.15,
    1.25,
    1.5,
    1.75,
    2.0,
    2.5,
    3.0,
    4.0,
    5.0,
];
const TYPOGRAPHY: [&str; 6] = [
    "AXFont",
    "AXSuperscript",
    "NSBaselineOffset",
    "AXMarkedText",
    "NSMarkedClauseSegment",
    "NSMarkedClauseSegmentAttributeName",
];

trait Value {
    fn string(&self) -> Option<String>;
    fn number(&self) -> Option<f64>;
    fn truthy(&self) -> bool;
    fn dictionary(&self, key: &str) -> Option<Self>
    where
        Self: Sized;
}
trait Attributed {
    type Value: Value;
    fn text(&self) -> Option<String>;
    fn len(&self) -> usize;
    fn longest(
        &self,
        key: &str,
        index: usize,
        limit: AxRange,
    ) -> Option<(Option<Self::Value>, AxRange)>;
}
trait Backend {
    type Node: Clone;
    type Attributed: Attributed;
    type Font;
    fn now(&self) -> Duration;
    fn identity(&self, node: &Self::Node) -> u64;
    fn same(&self, left: &Self::Node, right: &Self::Node) -> bool {
        self.identity(left) == self.identity(right)
    }
    fn string(&self, node: &Self::Node, name: &str) -> Option<String>;
    fn boolean(&self, node: &Self::Node, name: &str) -> bool;
    fn element(&self, node: &Self::Node, name: &str) -> Option<Self::Node>;
    fn children(&self, node: &Self::Node) -> Option<Vec<Self::Node>>;
    fn has_class(&self, node: &Self::Node, name: &str) -> bool;
    fn same_process(&self, left: &Self::Node, right: &Self::Node) -> bool;
    fn checked_bool(&self, node: &Self::Node, name: &str) -> Result<Option<bool>, ()> {
        Ok(Some(self.boolean(node, name)))
    }
    fn checked_string(&self, node: &Self::Node, name: &str) -> Result<Option<String>, ()> {
        Ok(self.string(node, name))
    }
    fn parent(&self, node: &Self::Node) -> Result<Option<Self::Node>, ()> {
        Ok(self.element(node, "AXParent"))
    }
    fn checked_children(&self, node: &Self::Node) -> Result<Vec<Self::Node>, ()> {
        Ok(self.children(node).unwrap_or_default())
    }
    fn frame(&self, node: &Self::Node) -> Option<Rect>;
    fn selected_range(&self, node: &Self::Node) -> Option<AxRange>;
    fn bounds(&self, node: &Self::Node, range: AxRange) -> Option<Rect>;
    // Some(Err) distinguishes malformed present attributes from absence.
    fn visible(&self, node: &Self::Node) -> Option<Result<AxRange, ()>>;
    fn attributed(&self, node: &Self::Node, range: AxRange) -> Option<Self::Attributed>;
    fn font(&self, name: &str, size: f64) -> Option<Self::Font>;
    fn system_font(&self, size: f64, weight: f64) -> Option<Self::Font>;
    fn font_name(&self, font: &Self::Font) -> String;
    fn width(&self, font: &Self::Font, text: &str) -> f64;
    fn metrics(&self, font: &Self::Font) -> (f64, f64);
    fn line_metrics(&self, font: &Self::Font) -> (f64, f64);
}

fn protected<B: Backend>(backend: &B, node: &B::Node, secure: &str) -> Option<bool> {
    if backend
        .checked_bool(node, "AXHidden")
        .ok()?
        .unwrap_or(false)
        || backend
            .checked_bool(node, "AXProtectedContent")
            .ok()?
            .unwrap_or(false)
        || backend.checked_string(node, "AXSubrole").ok()?.as_deref() == Some("AXSecureTextField")
        || (!secure.is_empty()
            && backend
                .checked_string(node, "AXRoleDescription")
                .ok()?
                .as_deref()
                == Some(secure))
    {
        return Some(true);
    }
    Some(false)
}
fn valid_rect(rect: Rect, collapsed: bool) -> Option<Rect> {
    (rect.iter().all(|v| v.is_finite())
        && rect[3] > 0.0
        && (rect[2] > 0.0 || (collapsed && rect[2] == 0.0)))
        .then_some(rect)
}
fn ancestry<B: Backend>(backend: &B, field: &B::Node, secure: &str) -> Option<Vec<B::Node>> {
    let mut chain = Vec::with_capacity(8);
    let mut next = Some(field.clone());
    for _ in 0..32 {
        let Some(node) = next else { return Some(chain) };
        if chain.iter().any(|old| backend.same(old, &node))
            || protected(backend, &node, secure) != Some(false)
        {
            return None;
        }
        next = backend.parent(&node).ok()?;
        chain.push(node);
    }
    None // Even exactly 32 nodes is a truncated observation, as in the AX oracle.
}
fn contains_rect(outer: Rect, inner: Rect) -> bool {
    inner[0] >= outer[0]
        && inner[1] >= outer[1]
        && inner[0] + inner[2] <= outer[0] + outer[2]
        && inner[1] + inner[3] <= outer[1] + outer[3]
}

// Monaco's textarea is an input relay, not the painted document viewport.
// Resolve its related native clipping node instead of widening its tiny frame.
// This supplies no caret/text geometry: field-local UTF-16 ranges, native glyph
// bounds and font calibration still have to agree in `inline_checked`.
fn monaco_relay_viewport<B: Backend>(
    backend: &B,
    chain: &[B::Node],
    secure: &str,
    rect: Rect,
) -> Option<Rect> {
    // Ordinary native text fields need no renderer metadata or sibling scan.
    if rect[2] > 2.0 {
        return None;
    }
    let field = chain.first()?;
    let guard = chain.get(1)?;
    let editor = chain.get(2)?;
    if backend.string(field, "AXRole").as_deref() != Some("AXTextArea")
        || !backend.has_class(field, "inputarea")
        || backend.string(guard, "AXRole").as_deref() != Some("AXGroup")
        || !backend.has_class(guard, "overflow-guard")
        || backend.string(editor, "AXRole").as_deref() != Some("AXGroup")
        || !backend.has_class(editor, "monaco-editor")
    {
        return None;
    }
    let window = backend.element(field, "AXWindow")?;
    for node in [guard, editor] {
        if !backend.same_process(field, node)
            || !backend
                .element(node, "AXWindow")
                .is_some_and(|owner| backend.same(&owner, &window))
        {
            return None;
        }
    }
    let guard_rect = valid_rect(backend.frame(guard)?, false)?;
    let editor_rect = valid_rect(backend.frame(editor)?, false)?;
    if !contains_rect(editor_rect, guard_rect) || !contains_rect(guard_rect, rect) {
        return None;
    }
    let deadline = backend.now().checked_add(TREE_TIME)?;
    let children = backend.checked_children(guard).ok()?;
    if children.len() > MAX_NODES {
        return None;
    }
    let mut viewport = None;
    for child in children {
        if backend.now() >= deadline {
            return None;
        }
        if backend.string(&child, "AXRole").as_deref() != Some("AXGroup")
            || !backend.has_class(&child, "lines-content")
            || !backend.has_class(&child, "monaco-editor-background")
        {
            continue;
        }
        if viewport.is_some()
            || protected(backend, &child, secure) != Some(false)
            || !backend.same_process(field, &child)
            || !backend
                .element(&child, "AXWindow")
                .is_some_and(|owner| backend.same(&owner, &window))
            || !backend
                .parent(&child)
                .ok()?
                .is_some_and(|parent| backend.same(&parent, guard))
        {
            return None;
        }
        let bounds = valid_rect(backend.frame(&child)?, false)?;
        if !contains_rect(guard_rect, bounds) || !contains_rect(bounds, rect) {
            return None;
        }
        viewport = Some(bounds);
    }
    (backend.now() < deadline).then_some(viewport).flatten()
}

fn visible_clip<B: Backend>(
    backend: &B,
    field: &B::Node,
    secure: &str,
    rect: Rect,
) -> Option<Rect> {
    let chain = ancestry(backend, field, secure)?;
    let window = backend.element(field, "AXWindow").or_else(|| {
        chain
            .iter()
            .skip(1)
            .find(|node| backend.string(node, "AXRole").as_deref() == Some("AXWindow"))
            .cloned()
    });
    let window = window.as_ref();
    let raw = valid_rect(rect, false)?;
    let mut clip = monaco_relay_viewport(backend, &chain, secure, raw).unwrap_or(raw);
    for parent in chain.iter().skip(1) {
        if backend.string(parent, "AXRole").as_deref() == Some("AXApplication") {
            break;
        }
        if let Some(bounds) = backend.frame(parent).and_then(|r| valid_rect(r, false)) {
            let left = clip[0].max(bounds[0]);
            let top = clip[1].max(bounds[1]);
            let right = (clip[0] + clip[2]).min(bounds[0] + bounds[2]);
            let bottom = (clip[1] + clip[3]).min(bounds[1] + bounds[3]);
            clip = valid_rect([left, top, right - left, bottom - top], false)?;
        }
        if window.is_some_and(|window| backend.same(parent, window)) {
            break;
        }
    }
    Some(clip)
}

fn bounds<B: Backend>(backend: &B, field: &B::Node, secure: &str, range: AxRange) -> Option<Rect> {
    range.location.checked_add(range.length)?;
    if let Some(rect) = backend
        .bounds(field, range)
        .and_then(|r| valid_rect(r, range.length == 0))
    {
        return Some(rect);
    }
    let deadline = backend.now().checked_add(TREE_TIME)?;
    let mut stack = vec![(field.clone(), 0)];
    let mut visited = Vec::with_capacity(MAX_NODES);
    let mut leaves = Vec::new();
    let mut field_text = String::new();
    let mut field_scalars = 0;
    while let Some((node, depth)) = stack.pop() {
        if visited.len() >= MAX_NODES || backend.now() >= deadline {
            return None;
        }
        if visited.iter().any(|old| backend.same(old, &node)) {
            return None;
        }
        visited.push(node.clone());
        if protected(backend, &node, secure) != Some(false) {
            return None;
        }
        let role = backend.checked_string(&node, "AXRole").ok()?;
        if depth > 0
            && (matches!(
                role.as_deref(),
                Some("AXTextField" | "AXTextArea" | "AXComboBox")
            ) || backend
                .checked_bool(&node, "AXEditable")
                .ok()?
                .unwrap_or(false))
        {
            return None;
        }
        if backend.now() >= deadline {
            return None;
        }
        if depth == 0 {
            if !matches!(role.as_deref(), Some("AXTextField" | "AXTextArea")) {
                return None;
            }
            field_text = backend.string(&node, "AXValue")?;
            field_scalars = field_text.chars().count();
            if field_scalars > 1_000_000 {
                return None;
            }
        } else if role.as_deref() == Some("AXStaticText") {
            if !backend.checked_children(&node).ok()?.is_empty() || backend.now() >= deadline {
                return None;
            }
            let text = backend.string(&node, "AXValue")?;
            if text.chars().count() > field_scalars {
                return None;
            }
            if !text.is_empty() {
                leaves.push((node, text));
            }
            continue;
        }
        let children = backend.checked_children(&node).ok()?;
        if !children.is_empty()
            && (depth >= MAX_DEPTH || children.len() > MAX_NODES - visited.len() - stack.len())
        {
            return None;
        }
        stack.extend(children.into_iter().rev().map(|child| (child, depth + 1)));
    }
    if leaves.is_empty()
        || leaves
            .iter()
            .map(|(_, text)| text.chars().count())
            .sum::<usize>()
            > field_scalars
    {
        return None;
    }
    let index = TextIndex::new(&field_text);
    let (start, _) = index.utf16_range_to_scalar(range.location, range.length)?;
    let start = index.scalar_to_utf8(ScalarIndex(start))?.0;
    let end_scalar = index
        .utf16_to_scalar(Utf16Offset(range.location + range.length))?
        .0;
    let end = index.scalar_to_utf8(ScalarIndex(end_scalar))?.0;
    // Enumerate complete literal placements. Only paragraph separators between
    // leaves may be absent. A second solution is ambiguity, not traversal order.
    let mut pending = vec![(0usize, 0usize, Vec::<(usize, usize)>::new())];
    let mut mapping = None;
    let mut work = 0;
    while let Some((leaf, mut cursor, spans)) = pending.pop() {
        if backend.now() >= deadline {
            return None;
        }
        if leaf == leaves.len() {
            if cursor == field_text.len() {
                if mapping.is_some() {
                    return None;
                }
                mapping = Some(spans);
            }
            continue;
        }
        let text = &leaves[leaf].1;
        loop {
            work += 1;
            if work > MAX_MAPPING_WORK || backend.now() >= deadline {
                return None;
            }
            if field_text[cursor..].starts_with(text) {
                let stop = cursor + text.len();
                let mut next = spans.clone();
                next.push((cursor, stop));
                pending.push((leaf + 1, stop, next));
            }
            if leaf == 0 {
                break;
            }
            match field_text[cursor..].chars().next() {
                Some(c @ ('\r' | '\n' | '\u{2028}' | '\u{2029}')) => cursor += c.len_utf8(),
                _ => break,
            }
        }
    }
    let mapping = mapping?;
    let mut candidates = mapping
        .iter()
        .enumerate()
        .filter(|(_, (left, right))| *left <= start && end <= *right);
    let (leaf, (left, _)) = candidates.next()?;
    if candidates.next().is_some() || backend.now() >= deadline {
        return None;
    }
    let relative = range
        .location
        .checked_sub(field_text[..*left].encode_utf16().count())?;
    let result = backend
        .bounds(
            &leaves[leaf].0,
            AxRange {
                location: relative,
                length: range.length,
            },
        )
        .and_then(|r| valid_rect(r, range.length == 0));
    (backend.now() < deadline).then_some(result).flatten()
}

fn rtl(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(
            bidi_class(c),
            BidiClass::R
                | BidiClass::AL
                | BidiClass::AN
                | BidiClass::RLE
                | BidiClass::RLO
                | BidiClass::RLI
        )
    })
}
fn caret<B: Backend>(
    backend: &B,
    field: &B::Node,
    secure: &str,
    text: &TextBuffer,
    at: usize,
    location: usize,
) -> Option<Rect> {
    if text.index().scalar_to_utf16(ScalarIndex(at))?.0 != location {
        return None;
    }
    let previous = text.slice(0, at)?.chars().next_back();
    let next = text
        .slice(at, text.scalar_len().checked_sub(at)?)?
        .chars()
        .next();
    let observed = bounds(
        backend,
        field,
        secure,
        AxRange {
            location,
            length: 0,
        },
    );
    let Some(mut rect) = observed else {
        let c = previous?;
        let before = text.slice(0, at)?;
        let line = before.rsplit(['\r', '\n']).next()?;
        if at != text.scalar_len()
            || matches!(c, '\r' | '\n')
            || canonical_combining_class(c) != 0
            || rtl(line)
        {
            return None;
        }
        let glyph = bounds(
            backend,
            field,
            secure,
            AxRange {
                location: location.checked_sub(c.len_utf16())?,
                length: c.len_utf16(),
            },
        )?;
        return (glyph[2] > 0.0).then_some([glyph[0] + glyph[2], glyph[1], 1.0, glyph[3]]);
    };
    let glyph = if let Some(c) = next.filter(|c| !matches!(c, '\r' | '\n')) {
        bounds(
            backend,
            field,
            secure,
            AxRange {
                location,
                length: c.len_utf16(),
            },
        )
    } else if let Some(c) = previous.filter(|c| !matches!(c, '\r' | '\n')) {
        bounds(
            backend,
            field,
            secure,
            AxRange {
                location: location.checked_sub(c.len_utf16())?,
                length: c.len_utf16(),
            },
        )
    } else {
        return None;
    };
    if let Some(glyph) = glyph {
        rect = caret_glyph_row(rect, glyph);
    }
    rect[2] = rect[2].max(1.0);
    Some(rect)
}

/// AX zero-range rectangles can precede their glyph row vertically. Preserve
/// native X and height; only a fresh, compatible glyph rectangle supplies Y.
fn caret_glyph_row(mut rect: Rect, glyph: Rect) -> Rect {
    if (glyph[3] - rect[3]).abs() <= 1.0
        && (0.0..=rect[3] + 1.0).contains(&(glyph[1] - rect[1]))
        && rect[0] <= glyph[0] + glyph[2] + 1.0
    {
        rect[1] = glyph[1];
    }
    rect
}

fn anchor_matches<B: Backend>(
    backend: &B,
    field: &B::Node,
    secure: &str,
    snapshot: &Snapshot,
    selection: AxRange,
) -> bool {
    if backend.selected_range(field) != Some(selection) {
        return false;
    }
    // Focus identity alone does not fence a caret move or a layout update in
    // the same field while the attributed/font/appearance reads are in flight.
    if snapshot.caret_rect.is_some_and(|expected| {
        caret(
            backend,
            field,
            secure,
            &snapshot.text,
            snapshot.caret,
            selection.location,
        ) != Some(expected)
    }) {
        return false;
    }
    if snapshot.inline_render.as_ref().is_some_and(|render| {
        snapshot.field_rect.is_none_or(|raw| {
            !contains_rect(raw, render.clip_rect)
                && visible_clip(backend, field, secure, raw) != Some(render.clip_rect)
        })
    }) {
        return false;
    }
    backend.frame(field) == snapshot.field_rect
        && backend
            .element(field, "AXWindow")
            .and_then(|window| backend.frame(&window))
            == snapshot.window_bounds
        && backend.selected_range(field) == Some(selection)
}

fn tolerance(width: f64) -> f64 {
    0.75_f64.max(width.abs() * 0.012)
}
fn system_weight(name: &str) -> Option<f64> {
    let name = name.to_lowercase();
    if [
        "italic",
        "oblique",
        "condensed",
        "compressed",
        "expanded",
        "mono",
        "rounded",
        "serif",
    ]
    .iter()
    .any(|word| name.contains(word))
    {
        return None;
    }
    [
        ("ultralight", -0.8),
        ("thin", -0.6),
        ("semibold", 0.3),
        ("light", -0.4),
        ("medium", 0.23),
        ("heavy", 0.56),
        ("black", 0.62),
        ("emphasized", 0.4),
        ("bold", 0.4),
        ("regular", 0.0),
    ]
    .iter()
    .find_map(|(word, weight)| name.contains(word).then_some(*weight))
    .or(Some(0.0))
}
fn font_candidate<B: Backend>(
    backend: &B,
    name: &str,
    family: &str,
    size: f64,
) -> Option<(B::Font, bool, f64)> {
    if name.starts_with('.') || family.starts_with('.') {
        let weight = system_weight(if name.is_empty() { family } else { name })?;
        return Some((backend.system_font(size, weight)?, true, weight));
    }
    if name.is_empty() {
        return None;
    }
    let font = backend.font(name, size)?;
    (backend.font_name(&font) == name).then_some((font, false, 0.0))
}
fn calibrate<B: Backend>(
    backend: &B,
    font: &B::Font,
    text: &str,
    part_text: &str,
    width: f64,
    part_width: f64,
    browser: bool,
    coordinate_tolerance: f64,
) -> Option<f64> {
    let measured = backend.width(font, text);
    let part = (!part_text.is_empty()).then(|| backend.width(font, part_text));
    if !measured.is_finite() || measured <= 0.0 || part.is_some_and(|p| !p.is_finite() || p <= 0.0)
    {
        return None;
    }
    if (width - measured).abs() <= coordinate_tolerance.max(tolerance(measured))
        && part.is_none_or(|p| (part_width - p).abs() <= coordinate_tolerance.max(tolerance(p)))
    {
        return Some(1.0);
    }
    let part = part?;
    if !browser || text.chars().count() < 8 || width < 30.0 {
        return None;
    }
    let ratio = width / measured;
    let zoom = ZOOM_LEVELS
        .into_iter()
        .min_by(|a, b| (a - ratio).abs().total_cmp(&(b - ratio).abs()))?;
    ((ratio - zoom).abs() <= 0.01 * zoom
        && (part_width - part * zoom).abs() <= coordinate_tolerance.max(tolerance(part * zoom)))
    .then_some(zoom)
}
fn fitted<B: Backend>(
    backend: &B,
    span: &str,
    whole: Rect,
    rect: Rect,
    clip: Rect,
) -> Option<InlineRender> {
    if crate::core::trim(span).is_empty() {
        return None;
    }
    let seed = backend.system_font(13.0, 0.0)?;
    let width = backend.width(&seed, span);
    if !width.is_finite() || width <= 0.0 {
        return None;
    }
    let mut size = 13.0 * whole[2] / width;
    if !size.is_finite() || !(1.0..=300.0).contains(&size) {
        return None;
    }
    let mut font = backend.system_font(size, 0.0)?;
    let (mut ascent, mut descent) = backend.metrics(&font);
    if ascent + descent > rect[3] {
        size *= rect[3] / (ascent + descent);
        font = backend.system_font(size, 0.0)?;
        (ascent, descent) = backend.metrics(&font);
    }
    let render = InlineRender {
        font_name: backend.font_name(&font),
        font_size: size,
        baseline: rect[1] + (rect[3] - ascent - descent) / 2.0 + ascent,
        clip_rect: clip,
        ascent,
        descent,
        source: "macos-ax-caret-row".into(),
        system_font: true,
        ..InlineRender::default()
    };
    render.valid(Some(rect)).then_some(render)
}

/// Calibration may look past a bounded horizontal whitespace gap, but never
/// treats that gap as font advance or as text to render before the suggestion.
fn preceding_sample(before: &str) -> Option<(&str, &str)> {
    const MAX_GAP_SCALARS: usize = 128;
    let (last, character) = before
        .char_indices()
        .rev()
        .take(MAX_GAP_SCALARS + 1)
        .find(|(_, character)| !character.is_whitespace())?;
    let end = last + character.len_utf8();
    let gap = &before[end..];
    if gap.is_empty()
        || gap.chars().any(|character| {
            (character.is_control() && character != '\t')
                || matches!(character, '\u{2028}' | '\u{2029}')
        })
    {
        return None;
    }
    let tail = before[..end].rsplit('\t').next()?;
    let start = tail.char_indices().rev().nth(31).map_or(0, |(at, _)| at);
    Some((&tail[start..], gap))
}

fn native_gap_anchor<B: Backend>(
    backend: &B,
    field: &B::Node,
    sample_end: usize,
    location: usize,
    caret: Rect,
    clip: Rect,
    diagnostic: Option<&mut Option<GapDiagnostic>>,
) -> Option<Rect> {
    let mut observed = diagnostic.map(|slot| {
        slot.insert(GapDiagnostic {
            reason: Some(GapFailure::SampleEndUnavailable),
            captured_caret: caret,
            clip,
            raw_sample_end: None,
            raw_caret: None,
            raw_gap: None,
            normalized_sample_end: None,
            normalized_caret: None,
        })
    });
    // Preserve the original read order and early exits. Diagnostics never ask
    // for a range that production would not otherwise have read.
    // All three observations remain direct: no tree-derived or previous-glyph
    // caret fallback can establish the tab's unknown horizontal advance.
    let raw_anchor = backend.bounds(
        field,
        AxRange {
            location: sample_end,
            length: 0,
        },
    );
    if let Some(observed) = observed.as_deref_mut() {
        observed.raw_sample_end = raw_anchor;
    }
    let anchor = valid_rect(raw_anchor?, true)?;
    if let Some(observed) = observed.as_deref_mut() {
        observed.reason = Some(GapFailure::CaretUnavailable);
    }
    let raw_current = backend.bounds(
        field,
        AxRange {
            location,
            length: 0,
        },
    );
    if let Some(observed) = observed.as_deref_mut() {
        observed.raw_caret = raw_current;
    }
    let current = valid_rect(raw_current?, true)?;
    if let Some(observed) = observed.as_deref_mut() {
        observed.reason = Some(GapFailure::GapUnavailable);
    }
    let raw_gap = backend.bounds(
        field,
        AxRange {
            location: sample_end,
            length: location.checked_sub(sample_end)?,
        },
    );
    if let Some(observed) = observed.as_deref_mut() {
        observed.raw_gap = raw_gap;
    }
    let gap = valid_rect(raw_gap?, false)?;
    // Establish raw row/height agreement BEFORE normalization, unchanged.
    let reason = if (anchor[1] - current[1]).abs() > 1.0 {
        Some(GapFailure::RawEndpointRowMismatch)
    } else if (anchor[3] - current[3]).abs() > 1.0 {
        Some(GapFailure::RawEndpointHeightMismatch)
    } else if (gap[3] - current[3]).abs() > 1.0 {
        Some(GapFailure::GapHeightMismatch)
    } else if (current[3] - caret[3]).abs() > 1.0 {
        Some(GapFailure::CapturedCaretHeightMismatch)
    } else {
        None
    };
    if let Some(observed) = observed.as_deref_mut() {
        observed.reason = reason;
    }
    if reason.is_some() {
        return None;
    }
    // Use the same measured-glyph convention as ordinary caret capture, not a
    // guessed line offset. The gap must still join both actual native X values.
    let anchor = caret_glyph_row(anchor, gap);
    let current = caret_glyph_row(current, gap);
    let reason = if (current[0] - caret[0]).abs() > 1.5 {
        Some(GapFailure::CaretXMismatch)
    } else if (current[1] - caret[1]).abs() > 1.0 {
        Some(GapFailure::CaretRowMismatch)
    } else if anchor[0] >= current[0] {
        Some(GapFailure::EndpointOrderInvalid)
    } else if (anchor[1] - current[1]).abs() > 1.0 {
        Some(GapFailure::NormalizedEndpointRowMismatch)
    } else if (gap[1] - current[1]).abs() > 1.0 {
        Some(GapFailure::GapRowMismatch)
    } else if (gap[0] - anchor[0]).abs() > 1.5 {
        Some(GapFailure::GapStartMismatch)
    } else if (gap[0] + gap[2] - current[0]).abs() > 1.5 {
        Some(GapFailure::GapEndMismatch)
    } else if !contains_rect(clip, anchor) {
        Some(GapFailure::SampleEndClipped)
    } else if !contains_rect(clip, gap) {
        Some(GapFailure::GapClipped)
    } else {
        None
    };
    if let Some(observed) = observed {
        observed.normalized_sample_end = Some(anchor);
        observed.normalized_caret = Some(current);
        observed.reason = reason;
    }
    if reason.is_some() {
        return None;
    }
    Some(anchor)
}

fn inline<B: Backend>(
    backend: &B,
    field: &B::Node,
    secure: &str,
    text: &TextBuffer,
    at: usize,
    location: usize,
    rect: Option<Rect>,
    field_rect: Option<Rect>,
    browser: bool,
    diagnostic: &mut &'static str,
    gap_diagnostic: Option<&mut Option<GapDiagnostic>>,
) -> (Option<InlineRender>, Option<AppearanceEvidence>) {
    inline_checked(
        backend,
        field,
        secure,
        text,
        at,
        location,
        rect,
        field_rect,
        browser,
        diagnostic,
        gap_diagnostic,
    )
    .unwrap_or((None, None))
}
fn inline_checked<B: Backend>(
    backend: &B,
    field: &B::Node,
    secure: &str,
    text: &TextBuffer,
    at: usize,
    location: usize,
    rect: Option<Rect>,
    field_rect: Option<Rect>,
    browser: bool,
    diagnostic: &mut &'static str,
    gap_diagnostic: Option<&mut Option<GapDiagnostic>>,
) -> Option<(Option<InlineRender>, Option<AppearanceEvidence>)> {
    *diagnostic = "caret_geometry_unverified";
    let rect = valid_rect(rect?, false)?;
    *diagnostic = "field_geometry_unverified";
    let field_rect = valid_rect(field_rect?, false)?;
    *diagnostic = "caret_utf16_unverified";
    if at == 0 || text.index().scalar_to_utf16(ScalarIndex(at))?.0 != location {
        return None;
    }
    *diagnostic = "caret_row_text_unverified";
    let before = text.slice(0, at)?.rsplit(['\r', '\n']).next()?;
    let after = text
        .slice(at, text.scalar_len().checked_sub(at)?)?
        .split(['\r', '\n'])
        .next()?;
    if before.is_empty() {
        *diagnostic = "caret_row_prefix_empty";
        return None;
    }
    if after.contains('\t') {
        *diagnostic = "caret_row_suffix_tab";
        return None;
    }
    if rtl(before) || rtl(after) {
        *diagnostic = "caret_row_direction_unverified";
        return None;
    }
    let tail = before.rsplit('\t').next()?;
    let mut gap = "";
    let count = tail.chars().count();
    let mut span = &tail[tail
        .char_indices()
        .nth(count.saturating_sub(32))
        .map_or(tail.len(), |(i, _)| i)..];
    if crate::core::trim(span).is_empty() {
        *diagnostic = if tail.is_empty() {
            "caret_row_sample_after_tab_empty"
        } else {
            "caret_row_sample_whitespace"
        };
        (span, gap) = preceding_sample(before)?;
    }
    let separated = !gap.is_empty();
    let gap_units = gap.encode_utf16().count();
    let sample_end = location.checked_sub(gap_units)?;
    let suffix_units = after.encode_utf16().count();
    *diagnostic = "visible_range_unverified";
    if let Some(shown) = backend.visible(field) {
        let shown = shown.ok()?;
        if (shown.location != 0 || shown.length != 0)
            && (shown.location >= location
                || location.checked_add(suffix_units)?
                    > shown.location.checked_add(shown.length)?)
        {
            return None;
        }
    }
    *diagnostic = "caret_clipping_unverified";
    let clip = visible_clip(backend, field, secure, field_rect)?;
    if rect[0] < clip[0]
        || rect[0] + 1.0 > clip[0] + clip[2]
        || rect[1] < clip[1]
        || rect[1] + rect[3] > clip[1] + clip[3] + 1.0
    {
        return None;
    }
    let anchor = if separated {
        *diagnostic = "caret_row_gap_geometry_unverified";
        native_gap_anchor(
            backend,
            field,
            sample_end,
            location,
            rect,
            clip,
            gap_diagnostic,
        )?
    } else {
        rect
    };
    *diagnostic = "same_line_glyph_bounds_unverified";
    let mut whole;
    loop {
        let base = sample_end.checked_sub(span.encode_utf16().count())?;
        whole = bounds(
            backend,
            field,
            secure,
            AxRange {
                location: base,
                length: sample_end - base,
            },
        );
        if whole.is_some_and(|r| {
            r[2] > 0.0
                && (r[1] - anchor[1]).abs() <= 1.0
                && r[3] <= anchor[3] + 1.0
                && (r[0] + r[2] - anchor[0]).abs() <= 1.5
        }) {
            break;
        }
        let half = span.chars().count().div_ceil(2);
        span = &span[span.char_indices().nth(half).map_or(span.len(), |(i, _)| i)..];
        if span.is_empty() {
            return None;
        }
    }
    if crate::core::trim(span).is_empty() {
        return None;
    }
    let sample_units = span.encode_utf16().count();
    let caret_offset = sample_units.checked_add(gap_units)?;
    let base = sample_end.checked_sub(sample_units)?;
    let expected_units = caret_offset.checked_add(suffix_units)?;
    *diagnostic = "attributed_text_unverified";
    let attributed = backend.attributed(
        field,
        AxRange {
            location: base,
            length: expected_units,
        },
    )?;
    let expected = attributed.text()?;
    let prefix_bytes = span.len().checked_add(gap.len())?;
    if attributed.len() != expected_units
        || expected.len() != prefix_bytes + after.len()
        || !expected.starts_with(span)
        || expected.get(span.len()..prefix_bytes) != Some(gap)
        || expected.get(prefix_bytes..) != Some(after)
    {
        return None;
    }
    let expected_index = TextIndex::new(&expected);
    *diagnostic = "typography_run_unverified";
    let mut run_start = 0;
    let mut run_end = expected_units;
    let mut descriptor = None;
    for (i, key) in TYPOGRAPHY.iter().enumerate() {
        let (value, run) = attributed.longest(
            key,
            caret_offset.checked_sub(1)?,
            AxRange {
                location: 0,
                length: expected_units,
            },
        )?;
        let end = run.location.checked_add(run.length)?;
        if run.location > caret_offset - 1 || end <= caret_offset - 1 || end > expected_units {
            return None;
        }
        expected_index.utf16_to_scalar(Utf16Offset(run.location))?;
        expected_index.utf16_to_scalar(Utf16Offset(end))?;
        if (i >= 4 && value.is_some())
            || ((1..4).contains(&i) && value.as_ref().is_some_and(Value::truthy))
        {
            return None;
        }
        run_start = run_start.max(run.location);
        run_end = run_end.min(end);
        if i == 0 {
            descriptor = value;
        }
    }
    if run_start > 0 {
        // `expected` was proved byte-exactly equal to span + gap + after above.
        // Reuse only that immutable conversion, never native geometry/security.
        let scalar = expected_index.utf16_to_scalar(Utf16Offset(run_start))?;
        span = span.get(expected_index.scalar_to_utf8(scalar)?.0..)?;
    }
    if crate::core::trim(span).is_empty() {
        return None;
    }
    let span_location = base.checked_add(run_start)?;
    if run_start > 0 {
        whole = bounds(
            backend,
            field,
            secure,
            AxRange {
                location: span_location,
                length: sample_end - span_location,
            },
        );
    }
    let name = descriptor
        .as_ref()
        .and_then(|d| d.dictionary("AXFontName"))
        .and_then(|v| v.string())
        .unwrap_or_default();
    let family = descriptor
        .as_ref()
        .and_then(|d| d.dictionary("AXFontFamily"))
        .and_then(|v| v.string())
        .unwrap_or_default();
    *diagnostic = "font_descriptor_unverified";
    if name.is_empty() && family.is_empty() {
        if separated {
            return None;
        }
        return Some((
            if after.is_empty() {
                fitted(backend, span, whole?, rect, clip)
            } else {
                None
            },
            None,
        ));
    }
    *diagnostic = "same_line_font_samples_unverified";
    let size = descriptor.as_ref()?.dictionary("AXFontSize")?.number()?;
    if !size.is_finite() || !(1.0..=300.0).contains(&size) {
        return None;
    }
    let half = span.chars().count() / 2;
    let part_text = if half > 0 {
        &span[span.char_indices().nth(half)?.0..]
    } else {
        ""
    };
    if half > 0 && crate::core::trim(part_text).is_empty() {
        return None;
    }
    let part = if half > 0 {
        let units = span[..span.len() - part_text.len()].encode_utf16().count();
        bounds(
            backend,
            field,
            secure,
            AxRange {
                location: span_location + units,
                length: sample_end - span_location - units,
            },
        )
    } else {
        whole
    }?;
    let whole = whole?;
    if whole[2] <= 0.0
        || part[2] <= 0.0
        || (whole[1] - part[1]).abs() > 1.0
        || (whole[3] - part[3]).abs() > 1.0
        || (whole[0] + whole[2] - anchor[0]).abs() > 1.5
        || (part[0] + part[2] - anchor[0]).abs() > 1.5
        || anchor[1] < whole[1] - 1.0
        || anchor[1] > whole[1] + whole[3]
        || (separated && (!contains_rect(clip, whole) || !contains_rect(clip, part)))
    {
        return None;
    }
    // Vision's existing contract requires samples ending at the real caret.
    // Separated calibration proves only direct AX rendering; never fabricate
    // a caret-adjacent sample or allocate evidence that Vision cannot validate.
    let evidence = (!separated).then(|| {
        let mut samples = vec![AppearanceSample {
            text: span.into(),
            rect: whole,
        }];
        if half > 0 {
            samples.push(AppearanceSample {
                text: part_text.into(),
                rect: part,
            });
        }
        AppearanceEvidence {
            font_name: name.clone(),
            font_family: family.clone(),
            font_size: size,
            samples,
            caret_rect: rect,
            clip_rect: clip,
            suffix: None,
            ax_render: None,
            fixed_advance: 0.0,
        }
    });
    let whole_advance = anchor[0] - whole[0];
    let part_advance = anchor[0] - part[0];
    let coordinate_tolerance = if [anchor[0], whole[0], part[0]]
        .iter()
        .all(|v| v.fract() == 0.0)
    {
        1.0
    } else {
        0.75
    };
    *diagnostic = "font_identity_unverified";
    let Some((mut font, system, weight)) = font_candidate(backend, &name, &family, size) else {
        return Some((None, evidence));
    };
    *diagnostic = "font_advances_unverified";
    let Some(scale) = calibrate(
        backend,
        &font,
        span,
        part_text,
        whole_advance,
        part_advance,
        browser,
        coordinate_tolerance,
    ) else {
        return Some((None, evidence));
    };
    *diagnostic = "scaled_font_unverified";
    let effective = size * scale;
    if scale != 1.0 {
        let original_name = backend.font_name(&font);
        let scaled = if system {
            backend.system_font(effective, weight)
        } else {
            backend.font(&original_name, effective)
        };
        let Some(scaled) = scaled else {
            return Some((None, evidence));
        };
        let scaled_width = backend.width(&scaled, span);
        if backend.font_name(&scaled) != original_name
            || !scaled_width.is_finite()
            || (scaled_width - whole_advance).abs() > tolerance(whole_advance)
        {
            return Some((None, evidence));
        }
        font = scaled;
    }
    *diagnostic = "font_baseline_unverified";
    let (ascent, descent) = backend.metrics(&font);
    if !ascent.is_finite() || ascent <= 0.0 || !descent.is_finite() || descent < 0.0 {
        return Some((None, evidence));
    }
    let (top, height) = (whole[1], whole[3]);
    let mut baselines = Vec::with_capacity(2);
    let typographic_line = (height - ascent - descent).abs() <= 1.0;
    if typographic_line {
        baselines.push(top + ascent);
    }
    let (line_height, baseline_offset) = backend.line_metrics(&font);
    let line_tolerance = if height.fract() == 0.0 { 1.0 } else { 0.75 };
    let native_line = (height - line_height).abs() <= line_tolerance;
    if native_line {
        baselines.push(top + baseline_offset);
    }
    if browser
        && baselines.is_empty()
        && ascent + descent < height
        && height <= 3.0 * (ascent + descent)
    {
        baselines.push(top + (height - ascent - descent) / 2.0 + ascent);
    }
    if baselines.is_empty()
        || baselines.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            - baselines.iter().copied().fold(f64::INFINITY, f64::min)
            > 0.75
    {
        return Some((None, evidence));
    }
    let baseline = if browser {
        baselines[0]
    } else {
        *baselines.last()?
    };
    // Use the verified native fragment only when both row models agree.
    // Preserve its baseline and the original host clip; do not grow the clip
    // to hide the nominal ascender's rounding discrepancy on the first row.
    let (ascent, descent) = if !browser && native_line && typographic_line {
        (baseline_offset, line_height - baseline_offset)
    } else {
        (ascent, descent)
    };
    *diagnostic = "inline_render_invalid";
    let mut render = InlineRender {
        font_name: backend.font_name(&font),
        font_size: effective,
        baseline,
        clip_rect: clip,
        ascent,
        descent,
        source: "macos-ax-attributed-run".into(),
        system_font: system,
        system_weight: if system { weight } else { 0.0 },
        ..InlineRender::default()
    };
    if !render.valid(Some(rect)) {
        return Some((None, evidence));
    }
    *diagnostic = "suffix_typography_unverified";
    if after.is_empty() {
        return Some((Some(render), None));
    }
    if run_end < expected_units {
        return None;
    }
    let suffix_rect = bounds(
        backend,
        field,
        secure,
        AxRange {
            location,
            length: suffix_units,
        },
    )?;
    let expected_width = backend.width(&font, after);
    if !expected_width.is_finite()
        || (suffix_rect[1] - top).abs() > 1.0
        || (suffix_rect[3] - height).abs() > 1.0
        || (suffix_rect[0] - rect[0]).abs() > 1.5
        || suffix_rect[2] <= 0.0
        || (suffix_rect[2] - expected_width).abs() > tolerance(expected_width)
    {
        return None;
    }
    if crate::core::trim(after).is_empty() {
        render.suffix = after.into();
        render.suffix_width = suffix_rect[2];
        return Some((render.valid(Some(rect)).then_some(render), None));
    }
    let mut evidence = evidence?;
    evidence.suffix = Some(AppearanceSample {
        text: after.into(),
        rect: suffix_rect,
    });
    evidence.ax_render = Some(render);
    Some((None, Some(evidence)))
}

impl Value for AxValue {
    fn string(&self) -> Option<String> {
        self.as_string()
    }
    fn number(&self) -> Option<f64> {
        self.as_number()
    }
    fn truthy(&self) -> bool {
        self.as_bool()
            .or_else(|| self.as_number().map(|n| n != 0.0))
            .or_else(|| self.as_string().map(|s| !s.is_empty()))
            .unwrap_or(true)
    }
    fn dictionary(&self, key: &str) -> Option<Self> {
        self.dictionary_value(key)
    }
}
impl Attributed for AxAttributedString {
    type Value = AxValue;
    fn text(&self) -> Option<String> {
        self.text()
    }
    fn len(&self) -> usize {
        self.len_utf16()
    }
    fn longest(
        &self,
        key: &str,
        index: usize,
        limit: AxRange,
    ) -> Option<(Option<AxValue>, AxRange)> {
        self.attribute_longest(key, index, limit)
    }
}
/// Content-free acquisition counts; these are not latency or host-coverage proof.
#[derive(Clone, Copy, Default, Debug, serde::Serialize)]
pub(super) struct FrameReadCounts {
    pub requests: u64,
    pub hits: u64,
    pub native_calls: u64,
    pub failures: u64,
    pub invalidations: u64,
}

// Fixed storage, exact native equality, and no negative caching. A failed read
// clears all prior evidence; a changed authorization permanently revokes a pass.
pub(super) struct FrameReads<K, N> {
    identity: K,
    entries: [Option<(N, Rect)>; 16],
    enabled: bool,
    invalidated: bool,
    counts: FrameReadCounts,
}
impl<K: PartialEq, N: Clone + PartialEq> FrameReads<K, N> {
    pub(super) fn new(identity: K, enabled: bool) -> Self {
        Self {
            identity,
            entries: std::array::from_fn(|_| None),
            enabled,
            invalidated: false,
            counts: FrameReadCounts::default(),
        }
    }
    fn revoke(&mut self) {
        self.entries.fill_with(|| None);
        if !self.invalidated {
            self.counts.invalidations += 1;
        }
        self.invalidated = true;
    }
    pub(super) fn read(
        &mut self,
        identity: &K,
        node: &N,
        read: impl FnOnce() -> Option<Rect>,
        still_valid: impl FnOnce() -> bool,
    ) -> Option<Rect> {
        self.counts.requests += 1;
        if self.invalidated || &self.identity != identity {
            self.revoke();
            return None;
        }
        if self.enabled {
            if let Some((_, rect)) = self.entries.iter().flatten().find(|(old, _)| old == node) {
                let rect = *rect;
                if !still_valid() {
                    self.revoke();
                    return None;
                }
                self.counts.hits += 1;
                return Some(rect);
            }
        }
        let result = read();
        if !still_valid() {
            self.revoke();
            return None;
        }
        let Some(rect) = result else {
            self.entries.fill_with(|| None);
            self.counts.failures += 1;
            return None;
        };
        if self.enabled {
            if let Some(slot) = self.entries.iter_mut().find(|slot| slot.is_none()) {
                *slot = Some((node.clone(), rect));
            }
        }
        Some(rect)
    }
}

#[derive(Clone, Copy, PartialEq)]
struct ReadIdentity<'a> {
    field: &'a AxElement,
    window: &'a AxElement,
    document: &'a str,
    pid: i32,
    revision: u64,
    epoch: u64,
}

/// Borrowed identity from the freshly authorized focus, never an authority
/// replacement. The caller must end this pass before any fresh capture barrier.
pub(super) struct CaptureReadScope<'a> {
    pub field: &'a AxElement,
    pub window: &'a AxElement,
    pub document: &'a str,
    pub pid: i32,
    pub revision: u64,
    pub epoch: u64,
    pub current: &'a dyn Fn() -> (u64, u64, bool),
    pub reuse: bool,
}
struct FramePass<'a> {
    reads: RefCell<FrameReads<ReadIdentity<'a>, AxElement>>,
    current: &'a dyn Fn() -> (u64, u64, bool),
    native_calls: Cell<u64>,
}
impl FramePass<'_> {
    fn valid(&self) -> bool {
        let (revision, epoch, dirty) = (self.current)();
        let reads = self.reads.borrow();
        !reads.invalidated
            && !dirty
            && revision == reads.identity.revision
            && epoch == reads.identity.epoch
    }
    fn frame(&self, node: &AxElement) -> Option<Rect> {
        if !self.valid() {
            self.reads.borrow_mut().revoke();
            return None;
        }
        let identity = self.reads.borrow().identity;
        self.reads.borrow_mut().read(
            &identity,
            node,
            || node.frame_counted(&|| self.native_calls.set(self.native_calls.get() + 1)),
            || {
                let (revision, epoch, dirty) = (self.current)();
                !dirty && revision == identity.revision && epoch == identity.epoch
            },
        )
    }
}

struct Native<'a> {
    start: Instant,
    pass: Option<FramePass<'a>>,
}
impl Native<'_> {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            pass: None,
        }
    }
}
thread_local! {
    static LAYOUT: RefCell<Option<Retained<NSLayoutManager>>> = const { RefCell::new(None) };
}
#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    fn CTLineCreateWithAttributedString(string: *const c_void) -> *const c_void;
    fn CTLineGetTypographicBounds(
        line: *const c_void,
        ascent: *mut f64,
        descent: *mut f64,
        leading: *mut f64,
    ) -> f64;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: *const c_void);
}
impl Backend for Native<'_> {
    type Node = AxElement;
    type Attributed = AxAttributedString;
    type Font = Retained<NSFont>;
    fn now(&self) -> Duration {
        self.start.elapsed()
    }
    fn identity(&self, node: &AxElement) -> u64 {
        node.identity()
    }
    fn same(&self, left: &AxElement, right: &AxElement) -> bool {
        left == right
    }
    fn string(&self, node: &AxElement, name: &str) -> Option<String> {
        node.string(name)
    }
    fn boolean(&self, node: &AxElement, name: &str) -> bool {
        node.bool(name).unwrap_or(false)
    }
    fn has_class(&self, node: &AxElement, name: &str) -> bool {
        node.attribute("AXDOMClassList")
            .ok()
            .flatten()
            .and_then(|value| value.as_strings())
            .is_some_and(|classes| classes.iter().any(|class| class == name))
    }
    fn same_process(&self, left: &AxElement, right: &AxElement) -> bool {
        left.pid().is_some_and(|pid| right.pid() == Some(pid))
    }
    fn element(&self, node: &AxElement, name: &str) -> Option<AxElement> {
        node.element(name)
    }
    fn selected_range(&self, node: &AxElement) -> Option<AxRange> {
        node.range("AXSelectedTextRange")
    }
    fn children(&self, node: &AxElement) -> Option<Vec<AxElement>> {
        node.children()
    }
    fn checked_bool(&self, node: &AxElement, name: &str) -> Result<Option<bool>, ()> {
        node.attribute(name)
            .map_err(|_| ())?
            .map(|v| v.as_bool().ok_or(()))
            .transpose()
    }
    fn checked_string(&self, node: &AxElement, name: &str) -> Result<Option<String>, ()> {
        node.attribute(name)
            .map_err(|_| ())?
            .map(|v| v.as_string().ok_or(()))
            .transpose()
    }
    fn parent(&self, node: &AxElement) -> Result<Option<AxElement>, ()> {
        node.attribute("AXParent")
            .map_err(|_| ())?
            .map(|v| v.as_element().ok_or(()))
            .transpose()
    }
    fn checked_children(&self, node: &AxElement) -> Result<Vec<AxElement>, ()> {
        Ok(node
            .attribute("AXChildren")
            .map_err(|_| ())?
            .map(|v| v.as_elements().ok_or(()))
            .transpose()?
            .unwrap_or_default())
    }
    fn frame(&self, node: &AxElement) -> Option<Rect> {
        match &self.pass {
            Some(pass) => pass.frame(node),
            None => node.frame(),
        }
    }
    fn bounds(&self, node: &AxElement, range: AxRange) -> Option<Rect> {
        node.bounds_for_range(range)
    }
    fn visible(&self, node: &AxElement) -> Option<Result<AxRange, ()>> {
        match node.attribute("AXVisibleCharacterRange") {
            Ok(value) => value.map(|value| value.as_range().ok_or(())),
            Err(_) => Some(Err(())),
        }
    }
    fn attributed(&self, node: &AxElement, range: AxRange) -> Option<AxAttributedString> {
        node.attributed_string_for_range(range)
    }
    fn font(&self, name: &str, size: f64) -> Option<Self::Font> {
        NSFont::fontWithName_size(&NSString::from_str(name), size)
    }
    fn system_font(&self, size: f64, weight: f64) -> Option<Self::Font> {
        Some(NSFont::systemFontOfSize_weight(size, weight))
    }
    fn font_name(&self, font: &Self::Font) -> String {
        font.fontName().to_string()
    }
    fn width(&self, font: &Self::Font, text: &str) -> f64 {
        // NSFont and the attributed string are private, immutable objects on the
        // calling AX thread; no views, shared TextKit storage or UI are touched.
        // NSAttributedString is toll-free bridged to CFAttributedString. CoreText
        // borrows it for Create, whose owned CTLine is released exactly once.
        unsafe {
            let object: &AnyObject = font;
            let attributes = NSDictionary::from_slices(&[NSFontAttributeName], &[object]);
            let attributed = NSAttributedString::initWithString_attributes(
                NSAttributedString::alloc(),
                &NSString::from_str(text),
                Some(&attributes),
            );
            let line = CTLineCreateWithAttributedString(Retained::as_ptr(&attributed).cast());
            if line.is_null() {
                return f64::NAN;
            }
            let width = CTLineGetTypographicBounds(
                line,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            CFRelease(line);
            width
        }
    }
    fn metrics(&self, font: &Self::Font) -> (f64, f64) {
        (font.ascender(), font.descender().abs())
    }
    fn line_metrics(&self, font: &Self::Font) -> (f64, f64) {
        // The cached layout manager stays on its owning AX thread and is never
        // attached to a view/text storage. AppKit's metric methods access no UI
        // state; neither the manager nor fonts cross a thread boundary.
        LAYOUT.with(|layout| {
            let mut layout = layout.borrow_mut();
            let layout = layout.get_or_insert_with(NSLayoutManager::new);
            (
                layout.defaultLineHeightForFont(font),
                layout.defaultBaselineOffsetForFont(font),
            )
        })
    }
}

pub fn field_ancestry(field: &AxElement, secure_description: &str) -> Option<Vec<AxElement>> {
    ancestry(&Native::new(), field, secure_description)
}
pub fn range_bounds(field: &AxElement, secure_description: &str, range: AxRange) -> Option<Rect> {
    bounds(&Native::new(), field, secure_description, range)
}
pub fn capture_anchor_matches(
    field: &AxElement,
    secure_description: &str,
    snapshot: &Snapshot,
    selection: AxRange,
) -> bool {
    anchor_matches(
        &Native::new(),
        field,
        secure_description,
        snapshot,
        selection,
    )
}

pub(super) struct CaptureGeometry {
    pub caret_rect: Option<Rect>,
    pub field_rect: Option<Rect>,
    pub window_rect: Option<Rect>,
    pub render: Option<InlineRender>,
    pub evidence: Option<AppearanceEvidence>,
    pub counts: FrameReadCounts,
}

pub(super) fn capture_geometry(
    scope: CaptureReadScope<'_>,
    secure: &str,
    text: &TextBuffer,
    caret_index: usize,
    selection: AxRange,
    browser: bool,
    diagnostic: &mut &'static str,
    gap_diagnostic: Option<&mut Option<GapDiagnostic>>,
) -> Option<CaptureGeometry> {
    let backend = Native {
        start: Instant::now(),
        pass: Some(FramePass {
            reads: RefCell::new(FrameReads::new(
                ReadIdentity {
                    field: scope.field,
                    window: scope.window,
                    document: scope.document,
                    pid: scope.pid,
                    revision: scope.revision,
                    epoch: scope.epoch,
                },
                scope.reuse,
            )),
            current: scope.current,
            native_calls: Cell::new(0),
        }),
    };
    let caret_rect = caret(
        &backend,
        scope.field,
        secure,
        text,
        caret_index,
        selection.location,
    );
    let field_rect = backend.frame(scope.field);
    let (render, evidence) = if selection.length == 0 {
        inline(
            &backend,
            scope.field,
            secure,
            text,
            caret_index,
            selection.location,
            caret_rect,
            field_rect,
            browser,
            diagnostic,
            gap_diagnostic,
        )
    } else {
        (None, None)
    };
    // visible_clip may already have read this exact window while clipping the
    // inline run. Reuse only within this pass, not at the later anchor barrier.
    let window_rect = backend.frame(scope.window);
    let pass = backend.pass.as_ref()?;
    if !pass.valid() {
        return None;
    }
    let mut counts = pass.reads.borrow().counts;
    counts.native_calls = pass.native_calls.get();
    Some(CaptureGeometry {
        caret_rect,
        field_rect,
        window_rect,
        render,
        evidence,
        counts,
    })
    // Dropping backend releases every retained entry before the caller resumes.
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        collections::{HashMap, HashSet},
    };

    #[test]
    fn pass_frames_reuse_only_exact_authorized_identity_and_end_at_barriers() {
        let identity = (1, 2, "document", 42, 7, 9);
        let rect = [1., 2., 30., 40.];
        let calls = Cell::new(0);
        let read = || {
            calls.set(calls.get() + 1);
            Some(rect)
        };
        let mut pass = FrameReads::new(identity, true);
        assert_eq!(pass.read(&identity, &1, read, || true), Some(rect));
        assert_eq!(pass.read(&identity, &1, read, || true), Some(rect));
        assert_eq!(calls.get(), 1);
        assert_eq!(pass.counts.hits, 1);
        // A distinct native element is a miss, even in the same authorized pass.
        assert_eq!(pass.read(&identity, &3, read, || true), Some(rect));
        assert_eq!(calls.get(), 2);
        drop(pass);
        let mut fresh_barrier = FrameReads::new(identity, true);
        assert_eq!(fresh_barrier.read(&identity, &1, read, || true), Some(rect));
        assert_eq!(calls.get(), 3);
        for changed in [
            (3, 2, "document", 42, 7, 9),
            (1, 3, "document", 42, 7, 9),
            (1, 2, "other", 42, 7, 9),
            (1, 2, "document", 43, 7, 9),
            (1, 2, "document", 42, 8, 9),
            (1, 2, "document", 42, 7, 10),
        ] {
            let mut pass = FrameReads::new(identity, true);
            assert_eq!(pass.read(&identity, &1, read, || true), Some(rect));
            assert_eq!(pass.read(&changed, &1, read, || true), None);
            assert_eq!(pass.read(&identity, &1, read, || true), None);
            assert!(pass.invalidated);
        }
    }

    #[test]
    fn pass_frame_failure_discards_entries_and_mid_read_revocation_rejects_result() {
        let rect = [1., 2., 30., 40.];
        let mut pass = FrameReads::new(1, true);
        assert_eq!(pass.read(&1, &1, || Some(rect), || true), Some(rect));
        assert_eq!(pass.read(&1, &2, || None, || true), None);
        // A failed pair cannot preserve even an earlier successful entry.
        assert_eq!(pass.read(&1, &1, || None, || true), None);
        assert_eq!(pass.counts.hits, 0);
        assert_eq!(pass.read(&1, &1, || Some(rect), || false), None);
        assert_eq!(pass.read(&1, &1, || panic!("revoked pass"), || true), None);
    }

    #[test]
    fn pass_frames_are_bounded_and_disabled_baseline_always_reads() {
        let rect = [1., 2., 30., 40.];
        let mut pass = FrameReads::new(1, true);
        for node in 0..32 {
            assert_eq!(pass.read(&1, &node, || Some(rect), || true), Some(rect));
        }
        assert_eq!(pass.entries.iter().flatten().count(), 16);
        assert_eq!(pass.read(&1, &31, || None, || true), None);
        let calls = Cell::new(0);
        let mut baseline = FrameReads::new(1, false);
        for _ in 0..2 {
            assert_eq!(
                baseline.read(
                    &1,
                    &1,
                    || {
                        calls.set(calls.get() + 1);
                        Some(rect)
                    },
                    || true
                ),
                Some(rect)
            );
        }
        assert_eq!(calls.get(), 2);
        assert_eq!(baseline.counts.hits, 0);
    }

    #[derive(Clone, Debug, PartialEq)]
    enum TestValue {
        String(String),
        Number(f64),
        Bool(bool),
        Dictionary(HashMap<String, TestValue>),
    }
    impl Value for TestValue {
        fn string(&self) -> Option<String> {
            if let Self::String(s) = self {
                Some(s.clone())
            } else {
                None
            }
        }
        fn number(&self) -> Option<f64> {
            if let Self::Number(n) = self {
                Some(*n)
            } else {
                None
            }
        }
        fn truthy(&self) -> bool {
            match self {
                Self::String(s) => !s.is_empty(),
                Self::Number(n) => *n != 0.0,
                Self::Bool(b) => *b,
                Self::Dictionary(d) => !d.is_empty(),
            }
        }
        fn dictionary(&self, key: &str) -> Option<Self> {
            if let Self::Dictionary(d) = self {
                d.get(key).cloned()
            } else {
                None
            }
        }
    }
    #[derive(Clone)]
    struct TestAttributed {
        text: String,
        runs: Vec<(AxRange, HashMap<String, TestValue>)>,
        forced_range: Option<AxRange>,
    }
    impl Attributed for TestAttributed {
        type Value = TestValue;
        fn text(&self) -> Option<String> {
            Some(self.text.clone())
        }
        fn len(&self) -> usize {
            self.text.encode_utf16().count()
        }
        fn longest(
            &self,
            key: &str,
            index: usize,
            limit: AxRange,
        ) -> Option<(Option<TestValue>, AxRange)> {
            let position = self
                .runs
                .iter()
                .position(|(r, _)| r.location <= index && index < r.location + r.length)?;
            let value = self.runs[position].1.get(key);
            let range = self.runs[position].0;
            let mut start = range.location;
            let mut end = start + range.length;
            for (r, attributes) in self.runs[..position].iter().rev() {
                if r.location + r.length != start || attributes.get(key) != value {
                    break;
                }
                start = r.location;
            }
            for (r, attributes) in self.runs[position + 1..].iter() {
                if r.location != end || attributes.get(key) != value {
                    break;
                }
                end = r.location + r.length;
            }
            start = start.max(limit.location);
            end = end.min(limit.location + limit.length);
            Some((
                value.cloned(),
                self.forced_range.unwrap_or(AxRange {
                    location: start,
                    length: end - start,
                }),
            ))
        }
    }
    #[derive(Clone, Default)]
    struct Node {
        strings: HashMap<String, String>,
        classes: HashSet<String>,
        pid: i32,
        flags: HashSet<String>,
        unreadable: HashSet<String>,
        elements: HashMap<String, usize>,
        children: Vec<usize>,
        frame: Option<Rect>,
    }
    #[derive(Clone)]
    struct TestFont {
        name: String,
        size: f64,
    }
    struct Host {
        nodes: Vec<Node>,
        rows: HashMap<(usize, usize, usize), Rect>,
        reads: RefCell<Vec<(usize, String)>>,
        frame_reads: RefCell<Option<FrameReads<u64, usize>>>,
        requests: RefCell<Vec<(usize, usize, usize)>>,
        forbidden: HashSet<(usize, String)>,
        clock: Cell<Duration>,
        expire_read: Option<(usize, String)>,
        expire_bounds: Option<usize>,
        selection: Cell<Option<AxRange>>,
        selection_after_bounds: Option<AxRange>,
        visible: Option<Result<AxRange, ()>>,
        attributed: Option<TestAttributed>,
        widths: HashMap<String, f64>,
        ascent: f64,
        descent: f64,
        line_height: f64,
        baseline: f64,
        reject_font: bool,
        substitute_font: bool,
    }
    impl Host {
        fn tree(text: &str, leaves: &[&str]) -> Self {
            let mut field = Node::default();
            field.strings.insert("AXRole".into(), "AXTextArea".into());
            field.strings.insert("AXValue".into(), text.into());
            field.children = (1..=leaves.len()).collect();
            let mut nodes = vec![field];
            for text in leaves {
                let mut node = Node::default();
                node.strings.insert("AXRole".into(), "AXStaticText".into());
                node.strings.insert("AXValue".into(), (*text).into());
                nodes.push(node);
            }
            Self {
                nodes,
                rows: HashMap::new(),
                reads: RefCell::new(Vec::new()),
                frame_reads: RefCell::new(None),
                requests: RefCell::new(Vec::new()),
                forbidden: HashSet::new(),
                clock: Cell::new(Duration::ZERO),
                expire_read: None,
                expire_bounds: None,
                selection: Cell::new(None),
                selection_after_bounds: None,
                visible: None,
                attributed: None,
                widths: HashMap::new(),
                ascent: 16.0,
                descent: 4.0,
                line_height: 20.0,
                baseline: 16.0,
                reject_font: false,
                substitute_font: false,
            }
        }
        fn metrics(text: &str) -> Self {
            let mut host = Self::tree(text, &[]);
            host.attributed = Some(TestAttributed {
                text: text.into(),
                runs: vec![(
                    r(0, text.encode_utf16().count()),
                    font_attributes("Host-Regular"),
                )],
                forced_range: None,
            });
            host.rows.extend([
                ((0, 0, 11), [40.0, 40.0, 60.0, 20.0]),
                ((0, 5, 6), [70.0, 40.0, 30.0, 20.0]),
                ((0, 11, 5), [100.0, 40.0, 25.0, 20.0]),
            ]);
            host.widths.extend([
                ("Hello world".into(), 60.0),
                (" world".into(), 30.0),
                (" tail".into(), 25.0),
            ]);
            host
        }
        fn read(&self, node: usize, key: &str) {
            let read = (node, key.to_string());
            assert!(
                !self.forbidden.contains(&read),
                "forbidden content read: {read:?}"
            );
            if self.expire_read.as_ref() == Some(&read) {
                self.clock.set(Duration::from_secs(1));
            }
            self.reads.borrow_mut().push(read);
        }
        fn metrics_result(
            &self,
            text: &str,
            at: usize,
            rect: Rect,
            browser: bool,
        ) -> (Option<InlineRender>, Option<AppearanceEvidence>) {
            let text = TextBuffer::from(text);
            inline(
                self,
                &0,
                "secure",
                &text,
                at,
                text.index().scalar_to_utf16(ScalarIndex(at)).unwrap().0,
                Some(rect),
                Some([0.0, 0.0, 400.0, 200.0]),
                browser,
                &mut "not_observed",
                None,
            )
        }
    }
    impl Backend for Host {
        type Node = usize;
        type Attributed = TestAttributed;
        type Font = TestFont;
        fn now(&self) -> Duration {
            self.clock.get()
        }
        fn identity(&self, node: &usize) -> u64 {
            *node as u64
        }
        fn string(&self, node: &usize, key: &str) -> Option<String> {
            self.read(*node, key);
            self.nodes[*node].strings.get(key).cloned()
        }
        fn boolean(&self, node: &usize, key: &str) -> bool {
            self.read(*node, key);
            self.nodes[*node].flags.contains(key)
        }
        fn has_class(&self, node: &usize, name: &str) -> bool {
            self.read(*node, "AXDOMClassList");
            !self.nodes[*node].unreadable.contains("AXDOMClassList")
                && self.nodes[*node].classes.contains(name)
        }
        fn same_process(&self, left: &usize, right: &usize) -> bool {
            self.nodes[*left].pid > 0 && self.nodes[*left].pid == self.nodes[*right].pid
        }
        fn element(&self, node: &usize, key: &str) -> Option<usize> {
            self.read(*node, key);
            self.nodes[*node].elements.get(key).copied()
        }
        fn children(&self, node: &usize) -> Option<Vec<usize>> {
            self.read(*node, "AXChildren");
            Some(self.nodes[*node].children.clone())
        }
        fn checked_bool(&self, node: &usize, key: &str) -> Result<Option<bool>, ()> {
            if self.nodes[*node].unreadable.contains(key)
                || self.nodes[*node].strings.contains_key(key)
            {
                self.read(*node, key);
                return Err(());
            }
            Ok(Some(self.boolean(node, key)))
        }
        fn checked_string(&self, node: &usize, key: &str) -> Result<Option<String>, ()> {
            if self.nodes[*node].unreadable.contains(key) {
                self.read(*node, key);
                return Err(());
            }
            Ok(self.string(node, key))
        }
        fn parent(&self, node: &usize) -> Result<Option<usize>, ()> {
            if self.nodes[*node].unreadable.contains("AXParent") {
                self.read(*node, "AXParent");
                return Err(());
            }
            Ok(self.element(node, "AXParent"))
        }
        fn checked_children(&self, node: &usize) -> Result<Vec<usize>, ()> {
            if self.nodes[*node].unreadable.contains("AXChildren") {
                self.read(*node, "AXChildren");
                return Err(());
            }
            Ok(self.children(node).unwrap_or_default())
        }
        fn frame(&self, node: &usize) -> Option<Rect> {
            let read = || {
                self.read(*node, "frame");
                self.nodes[*node].frame
            };
            match self.frame_reads.borrow_mut().as_mut() {
                Some(pass) => pass.read(&1, node, read, || true),
                None => read(),
            }
        }
        fn selected_range(&self, node: &usize) -> Option<AxRange> {
            self.read(*node, "AXSelectedTextRange");
            self.selection.get()
        }
        fn bounds(&self, node: &usize, range: AxRange) -> Option<Rect> {
            self.requests
                .borrow_mut()
                .push((*node, range.location, range.length));
            if self.expire_bounds == Some(*node) {
                self.clock.set(Duration::from_secs(1));
            }
            if let Some(selection) = self.selection_after_bounds {
                self.selection.set(Some(selection));
            }
            self.rows
                .get(&(*node, range.location, range.length))
                .copied()
        }
        fn visible(&self, _: &usize) -> Option<Result<AxRange, ()>> {
            self.visible
        }
        fn attributed(&self, _: &usize, range: AxRange) -> Option<TestAttributed> {
            let mut attributed = self.attributed.clone()?;
            let index = TextIndex::new(&attributed.text);
            let (start, length) = index.utf16_range_to_scalar(range.location, range.length)?;
            let bytes = index.scalar_range_to_utf8(start, length)?;
            // Requests for a shrunken soft-wrap prefix use native local runs.
            if range.location > 0 && attributed.runs.len() == 1 {
                attributed.runs[0].0 = r(0, range.length);
            }
            attributed.text = attributed.text[bytes].into();
            Some(attributed)
        }
        fn font(&self, name: &str, size: f64) -> Option<TestFont> {
            (!self.reject_font).then(|| TestFont {
                name: if self.substitute_font {
                    "Fallback"
                } else {
                    name
                }
                .into(),
                size,
            })
        }
        fn system_font(&self, size: f64, _: f64) -> Option<TestFont> {
            Some(TestFont {
                name: ".SFNS-Regular".into(),
                size,
            })
        }
        fn font_name(&self, font: &TestFont) -> String {
            font.name.clone()
        }
        fn width(&self, font: &TestFont, text: &str) -> f64 {
            self.widths.get(text).copied().unwrap_or(f64::NAN) * font.size / 13.0
        }
        fn metrics(&self, font: &TestFont) -> (f64, f64) {
            (
                self.ascent * font.size / 13.0,
                self.descent * font.size / 13.0,
            )
        }
        fn line_metrics(&self, font: &TestFont) -> (f64, f64) {
            (
                self.line_height * font.size / 13.0,
                self.baseline * font.size / 13.0,
            )
        }
    }
    fn r(location: usize, length: usize) -> AxRange {
        AxRange { location, length }
    }
    fn font_attributes(name: &str) -> HashMap<String, TestValue> {
        HashMap::from([(
            "AXFont".into(),
            TestValue::Dictionary(HashMap::from([
                ("AXFontName".into(), TestValue::String(name.into())),
                ("AXFontSize".into(), TestValue::Number(13.0)),
            ])),
        )])
    }
    const RECT: Rect = [120.0, 240.0, 9.0, 21.0];
    const CARET: Rect = [100.0, 40.0, 1.0, 20.0];

    #[test]
    fn clipping_reuses_window_but_security_and_capture_anchor_stay_fresh() {
        let mut host = Host::tree("synthetic", &[]);
        let field_rect = [0., 0., 400., 200.];
        let window_rect = [0., 0., 500., 300.];
        host.nodes[0].frame = Some(field_rect);
        host.nodes[0].elements.insert("AXParent".into(), 1);
        host.nodes[0].elements.insert("AXWindow".into(), 1);
        let mut window = Node::default();
        window.strings.insert("AXRole".into(), "AXWindow".into());
        window.frame = Some(window_rect);
        host.nodes.push(window);
        host.selection.set(Some(r(9, 0)));
        host.rows.insert((0, 9, 0), CARET);
        host.rows.insert((0, 8, 1), [90., 40., 10., 20.]);
        let snapshot = Snapshot {
            text: "synthetic".into(),
            caret: 9,
            caret_rect: Some(CARET),
            field_rect: Some(field_rect),
            window_bounds: Some(window_rect),
            ..Default::default()
        };
        *host.frame_reads.borrow_mut() = Some(FrameReads::new(1, true));
        assert_eq!(
            visible_clip(&host, &0, "secure", field_rect),
            Some(field_rect)
        );
        assert_eq!(host.frame(&1), Some(window_rect));
        assert_eq!(
            host.reads
                .borrow()
                .iter()
                .filter(|(node, name)| *node == 1 && name == "frame")
                .count(),
            1
        );
        // Reusing geometry never reuses the security metadata around it.
        host.nodes[1].flags.insert("AXProtectedContent".into());
        assert_eq!(visible_clip(&host, &0, "secure", field_rect), None);
        host.nodes[1].flags.clear();
        host.frame_reads.borrow_mut().take(); // End pass BEFORE the guard.
        assert!(anchor_matches(&host, &0, "secure", &snapshot, r(9, 0)));
        assert_eq!(
            host.reads
                .borrow()
                .iter()
                .filter(|(node, name)| *node == 1 && name == "frame")
                .count(),
            2
        );
        host.nodes[1].frame = Some([1., 0., 500., 300.]);
        assert!(!anchor_matches(&host, &0, "secure", &snapshot, r(9, 0)));
        assert!(
            host.reads
                .borrow()
                .iter()
                .filter(|(_, name)| name == "AXSelectedTextRange")
                .count()
                >= 3
        );
    }

    #[test]
    fn capture_anchor_fences_same_field_selection_and_geometry_races_without_content_reads() {
        let mut host = Host::tree("synthetic", &[]);
        host.forbidden.insert((0, "AXValue".into()));
        host.forbidden.insert((0, "AXChildren".into()));
        host.nodes[0].frame = Some([0.0, 0.0, 400.0, 200.0]);
        host.selection.set(Some(r(9, 0)));
        host.rows.insert((0, 9, 0), CARET);
        host.rows.insert((0, 8, 1), [90.0, 40.0, 10.0, 20.0]);
        let snapshot = Snapshot {
            text: "synthetic".into(),
            caret: 9,
            caret_rect: Some(CARET),
            field_rect: host.nodes[0].frame,
            ..Default::default()
        };
        assert!(anchor_matches(&host, &0, "secure", &snapshot, r(9, 0)));

        host.requests.borrow_mut().clear();
        for selection in [None, Some(r(8, 0)), Some(r(8, 1))] {
            host.selection.set(selection);
            assert!(!anchor_matches(&host, &0, "secure", &snapshot, r(9, 0)));
            assert!(host.requests.borrow().is_empty());
        }
        host.selection.set(Some(r(9, 0)));
        host.selection_after_bounds = Some(r(8, 0));
        assert!(!anchor_matches(&host, &0, "secure", &snapshot, r(9, 0)));

        host.selection_after_bounds = None;
        host.selection.set(Some(r(9, 0)));
        host.rows.insert((0, 9, 0), [101.0, 40.0, 1.0, 20.0]);
        assert!(!anchor_matches(&host, &0, "secure", &snapshot, r(9, 0)));
        host.rows.insert((0, 9, 0), CARET);
        host.nodes[0].frame = Some([1.0, 0.0, 400.0, 200.0]);
        assert!(!anchor_matches(&host, &0, "secure", &snapshot, r(9, 0)));
        host.nodes[0].frame = snapshot.field_rect;
        host.nodes.push(Node {
            frame: Some([0.0, 0.0, 800.0, 600.0]),
            ..Default::default()
        });
        host.nodes[0].elements.insert("AXWindow".into(), 1);
        let mut snapshot = snapshot;
        snapshot.window_bounds = host.nodes[1].frame;
        assert!(anchor_matches(&host, &0, "secure", &snapshot, r(9, 0)));
        host.nodes[1].frame = Some([0.0, 1.0, 800.0, 600.0]);
        assert!(!anchor_matches(&host, &0, "secure", &snapshot, r(9, 0)));
    }

    #[test]
    fn direct_native_geometry_never_reads_content() {
        let mut host = Host::tree("unrelated", &["other"]);
        host.forbidden.insert((0, "AXValue".into()));
        host.forbidden.insert((0, "AXChildren".into()));
        host.rows.insert((0, 2, 1), RECT);
        assert_eq!(bounds(&host, &0, "secure", r(2, 1)), Some(RECT));
        assert!(host.reads.borrow().is_empty());
    }
    #[test]
    fn exact_utf16_leaf_offsets_and_paragraph_gaps() {
        for (text, location, length) in [
            ("hello", 3, 0),
            ("hello", 5, 0),
            ("hello", 1, 3),
            ("😀a", 3, 0),
            ("😀a", 0, 2),
            ("a😀b", 3, 1),
            ("e\u{301}!", 1, 1),
            ("hello\nworld", 6, 5),
        ] {
            let mut host = Host::tree(text, &[text]);
            host.rows.insert((1, location, length), RECT);
            assert_eq!(
                bounds(&host, &0, "secure", r(location, length)),
                Some(RECT),
                "{text}"
            );
            assert_eq!(
                *host.requests.borrow(),
                vec![(0, location, length), (1, location, length)]
            );
        }
        for separator in ["", "\n", "\r", "\r\n", "\u{2028}", "\u{2029}", "\n\n"] {
            let text = format!("😀a{separator}bc");
            let mut host = Host::tree(&text, &["😀a", "bc"]);
            host.rows.insert((2, 1, 1), RECT);
            assert_eq!(
                bounds(
                    &host,
                    &0,
                    "secure",
                    r(4 + separator.encode_utf16().count(), 1)
                ),
                Some(RECT)
            );
            assert_eq!(host.requests.borrow().last(), Some(&(2, 1, 1)));
        }
        let mut host = Host::tree("a\nb", &["a", "\n", "b"]);
        host.rows.insert((3, 0, 1), RECT);
        assert_eq!(bounds(&host, &0, "secure", r(2, 1)), Some(RECT));
    }
    #[test]
    fn incomplete_ambiguous_and_surrogate_interior_mappings_fail_closed() {
        for (text, leaves) in [
            ("hello", vec!["other"]),
            ("hello", vec!["hell"]),
            ("a b", vec!["a", "b"]),
            ("a\u{fffc}b", vec!["a", "b"]),
            ("\na", vec!["a"]),
            ("a\n", vec!["a"]),
            ("\u{feff}", vec![""]),
            ("", vec!["\u{feff}"]),
            ("a\n\nb", vec!["a", "\n", "b"]),
        ] {
            let host = Host::tree(text, &leaves);
            assert_eq!(bounds(&host, &0, "secure", r(0, 0)), None, "{text:?}");
            assert_eq!(host.requests.borrow().len(), 1);
        }
        for range in [r(1, 0), r(0, 1), r(1, 1), r(0, 4), r(usize::MAX, 1)] {
            let host = Host::tree("😀a", &["😀a"]);
            assert_eq!(bounds(&host, &0, "secure", range), None);
            assert!(host.requests.borrow().iter().all(|request| request.0 == 0));
        }
        for (text, range) in [
            ("abcd", r(1, 2)),
            ("ab\ncd", r(1, 3)),
            ("ab\ncd", r(2, 1)),
            ("abcd", r(2, 0)),
        ] {
            let host = Host::tree(text, &["ab", "cd"]);
            assert_eq!(bounds(&host, &0, "secure", range), None);
            assert_eq!(host.requests.borrow().len(), 1);
        }
    }
    #[test]
    fn protected_and_editable_nodes_are_rejected_before_content() {
        for node in 0..=1 {
            for key in [
                "AXHidden",
                "AXProtectedContent",
                "AXSubrole",
                "AXRoleDescription",
            ] {
                let mut host = Host::tree("hello", &["hello"]);
                match key {
                    "AXSubrole" => {
                        host.nodes[node]
                            .strings
                            .insert(key.into(), "AXSecureTextField".into());
                    }
                    "AXRoleDescription" => {
                        host.nodes[node].strings.insert(key.into(), "secure".into());
                    }
                    _ => {
                        host.nodes[node].flags.insert(key.into());
                    }
                }
                host.forbidden
                    .extend([(node, "AXValue".into()), (node, "AXChildren".into())]);
                assert_eq!(bounds(&host, &0, "secure", r(1, 0)), None);
            }
        }
        for role in [
            "AXTextArea",
            "AXTextField",
            "AXComboBox",
            "AXGroup",
            "AXStaticText",
        ] {
            let mut host = Host::tree("hello", &["hello"]);
            host.nodes[1].strings.insert("AXRole".into(), role.into());
            if matches!(role, "AXGroup" | "AXStaticText") {
                host.nodes[1].flags.insert("AXEditable".into());
            }
            host.forbidden
                .extend([(1, "AXValue".into()), (1, "AXChildren".into())]);
            assert_eq!(bounds(&host, &0, "secure", r(1, 0)), None);
        }
        let mut host = Host::tree("ab", &["a", "b"]);
        host.nodes[2].flags.insert("AXProtectedContent".into());
        host.forbidden.insert((2, "AXValue".into()));
        host.rows.insert((1, 0, 1), RECT);
        assert_eq!(bounds(&host, &0, "secure", r(0, 1)), None);
        assert_eq!(host.requests.borrow().len(), 1);
    }
    #[test]
    fn complete_tree_node_depth_cycle_and_work_budgets() {
        for (count, accepted) in [(127, true), (128, false)] {
            let text = "a".repeat(count);
            let leaves = vec!["a"; count];
            let mut host = Host::tree(&text, &leaves);
            host.rows.insert((count, 0, 1), RECT);
            assert_eq!(
                bounds(&host, &0, "secure", r(count - 1, 1)),
                accepted.then_some(RECT)
            );
        }
        for (depth, accepted) in [(8, true), (9, false)] {
            let mut host = Host::tree("a", &["a"]);
            for _ in 1..depth {
                host.nodes.push(Node::default());
            }
            let mut parent = 0;
            for child in 2..host.nodes.len() {
                host.nodes[parent].children = vec![child];
                parent = child;
            }
            host.nodes[parent].children = vec![1];
            host.rows.insert((1, 0, 1), RECT);
            assert_eq!(
                bounds(&host, &0, "secure", r(0, 1)),
                accepted.then_some(RECT)
            );
        }
        for children in [vec![0], vec![1, 1]] {
            let mut host = Host::tree("hello", &["hello"]);
            host.nodes[0].children = children;
            assert_eq!(bounds(&host, &0, "secure", r(0, 1)), None);
        }
        let host = Host::tree(&format!("a{}b", "\n".repeat(4097)), &["a", "b"]);
        assert_eq!(bounds(&host, &0, "secure", r(4098, 1)), None);
        assert_eq!(host.requests.borrow().len(), 1);
    }
    #[test]
    fn deadline_and_invalid_rectangles_never_supply_partial_proof() {
        let mut host = Host::tree("hello", &["hello"]);
        host.expire_read = Some((1, "AXRole".into()));
        host.forbidden.insert((1, "AXValue".into()));
        assert_eq!(bounds(&host, &0, "secure", r(1, 0)), None);
        let mut host = Host::tree("hello", &["hello"]);
        host.expire_bounds = Some(1);
        host.rows.insert((1, 1, 0), RECT);
        assert_eq!(bounds(&host, &0, "secure", r(1, 0)), None);
        for rect in [
            [f64::NAN, 240.0, 9.0, 21.0],
            [120.0, f64::INFINITY, 9.0, 21.0],
            [120.0, 240.0, -1.0, 21.0],
            [120.0, 240.0, 9.0, 0.0],
            [120.0, 240.0, 0.0, 21.0],
        ] {
            let mut host = Host::tree("hello", &["hello"]);
            host.rows.insert((1, 0, 1), rect);
            assert_eq!(bounds(&host, &0, "secure", r(0, 1)), None);
        }
        let mut host = Host::tree("hello", &["hello"]);
        host.rows.insert((1, 5, 0), [120.0, 240.0, 0.0, 21.0]);
        assert_eq!(
            bounds(&host, &0, "secure", r(5, 0)),
            Some([120.0, 240.0, 0.0, 21.0])
        );
    }
    #[test]
    fn ancestry_and_window_clipping_keep_protection_beyond_the_window() {
        let mut host = Host::tree("", &[]);
        host.nodes
            .extend([Node::default(), Node::default(), Node::default()]);
        for (node, role) in [(1, "AXScrollArea"), (2, "AXWindow"), (3, "AXApplication")] {
            host.nodes[node - 1]
                .elements
                .insert("AXParent".into(), node);
            host.nodes[node]
                .strings
                .insert("AXRole".into(), role.into());
        }
        host.nodes[1].frame = Some([150.0, 250.0, 200.0, 100.0]);
        host.nodes[2].frame = Some([100.0, 200.0, 500.0, 300.0]);
        host.nodes[3].frame = Some([0.0, 0.0, 1.0, 1.0]);
        let field = [120.0, 240.0, 400.0, 180.0];
        for explicit in [false, true] {
            if explicit {
                host.nodes[0].elements.insert("AXWindow".into(), 2);
            }
            assert_eq!(
                visible_clip(&host, &0, "secure", field),
                Some([150.0, 250.0, 200.0, 100.0])
            );
            host.nodes[3].flags.insert("AXProtectedContent".into());
            assert_eq!(visible_clip(&host, &0, "secure", field), None);
            host.nodes[3].flags.clear();
        }
        host.nodes[1].elements.insert("AXParent".into(), 0);
        assert_eq!(ancestry(&host, &0, "secure"), None);
        let mut host = Host::tree("", &[]);
        for node in 1..32 {
            host.nodes.push(Node::default());
            host.nodes[node - 1]
                .elements
                .insert("AXParent".into(), node);
        }
        assert_eq!(ancestry(&host, &0, "secure"), None);
        host.nodes[30].elements.clear();
        assert_eq!(ancestry(&host, &0, "secure").unwrap().len(), 31);
    }
    fn monaco_relay_host() -> Host {
        let mut host = Host::metrics("Hello world");
        host.nodes.extend((0..5).map(|_| Node::default()));
        for (node, role) in [
            (1, "AXGroup"),
            (2, "AXGroup"),
            (3, "AXWindow"),
            (4, "AXApplication"),
            (5, "AXGroup"),
        ] {
            host.nodes[node]
                .strings
                .insert("AXRole".into(), role.into());
        }
        for (node, parent) in [(0, 1), (1, 2), (2, 3), (3, 4), (5, 1)] {
            host.nodes[node].elements.insert("AXParent".into(), parent);
        }
        for node in [0, 1, 2, 3, 5] {
            host.nodes[node].elements.insert("AXWindow".into(), 3);
        }
        for node in &mut host.nodes {
            node.pid = 7;
        }
        host.nodes[0].classes.insert("inputarea".into());
        host.nodes[1].classes.insert("overflow-guard".into());
        host.nodes[2].classes.insert("monaco-editor".into());
        host.nodes[5]
            .classes
            .extend(["lines-content".into(), "monaco-editor-background".into()]);
        host.nodes[0].frame = Some([99.0, 40.0, 1.0, 20.0]);
        host.nodes[1].frame = Some([0.0, 0.0, 240.0, 140.0]);
        host.nodes[2].frame = host.nodes[1].frame;
        host.nodes[3].frame = Some([-20.0, -20.0, 400.0, 300.0]);
        // The native text viewport excludes the gutter; never use the window
        // or the whole overflow-guard as an invented paint rectangle.
        host.nodes[5].frame = Some([20.0, 20.0, 180.0, 100.0]);
        host.nodes[1].children = vec![0, 5];
        host.selection.set(Some(r(11, 0)));
        host.rows.insert((0, 11, 0), CARET);
        host.rows.insert((0, 10, 1), [95.0, 40.0, 5.0, 20.0]);
        host
    }

    #[test]
    fn monaco_textarea_relay_uses_proven_native_viewport_not_one_pixel_input_frame() {
        let host = monaco_relay_host();
        let raw = host.nodes[0].frame.unwrap();
        assert_eq!(visible_clip(&host, &0, "secure", raw), host.nodes[5].frame);
        let (render, _) = inline(
            &host,
            &0,
            "secure",
            &"Hello world".into(),
            11,
            11,
            Some(CARET),
            Some(raw),
            false,
            &mut "not_observed",
            None,
        );
        assert_eq!(render.unwrap().clip_rect, host.nodes[5].frame.unwrap());
        assert_eq!(host.nodes[0].frame, Some(raw));
    }

    #[test]
    fn monaco_relay_viewport_requires_exact_topology_owner_and_unique_native_clip() {
        for case in 0..9 {
            let mut host = monaco_relay_host();
            let raw = host.nodes[0].frame.unwrap();
            match case {
                0 => host.nodes[0].classes.clear(),
                1 => host.nodes[1].classes.clear(),
                2 => host.nodes[2].classes.clear(),
                3 => host.nodes[5].pid = 8,
                4 => {
                    host.nodes[5].elements.insert("AXWindow".into(), 4);
                }
                5 => {
                    let duplicate = host.nodes[5].clone();
                    host.nodes.push(duplicate);
                    host.nodes[1].children.push(6);
                }
                6 => {
                    host.nodes[5].elements.insert("AXParent".into(), 2);
                }
                7 => {
                    host.nodes[5].unreadable.insert("AXDOMClassList".into());
                }
                8 => host.nodes[5].frame = Some([20.0, 20.0, 400.0, 100.0]),
                _ => unreachable!(),
            }
            assert_eq!(
                visible_clip(&host, &0, "secure", raw),
                Some(raw),
                "case {case}"
            );
        }
        for flag in ["AXHidden", "AXProtectedContent"] {
            let mut host = monaco_relay_host();
            host.nodes[5].flags.insert(flag.into());
            let raw = host.nodes[0].frame.unwrap();
            assert_eq!(visible_clip(&host, &0, "secure", raw), Some(raw));
        }
    }

    #[test]
    fn monaco_relay_does_not_synthesize_missing_native_edit_context_geometry() {
        let mut host = monaco_relay_host();
        host.nodes[0].classes = HashSet::from(["native-edit-context".into()]);
        host.rows.clear();
        host.rows.insert((0, 11, 0), [0.0, 1117.0, 0.0, 0.0]);
        assert_eq!(
            caret(&host, &0, "secure", &"Hello world".into(), 11, 11),
            None
        );
    }

    #[test]
    fn monaco_relay_capture_anchor_revalidates_viewport_motion_and_protection() {
        let mut host = monaco_relay_host();
        let raw = host.nodes[0].frame.unwrap();
        let snapshot = Snapshot {
            text: "Hello world".into(),
            caret: 11,
            caret_rect: Some(CARET),
            field_rect: Some(raw),
            window_bounds: host.nodes[3].frame,
            inline_render: Some(InlineRender {
                clip_rect: host.nodes[5].frame.unwrap(),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(anchor_matches(&host, &0, "secure", &snapshot, r(11, 0)));
        host.nodes[5].frame = Some([21.0, 20.0, 180.0, 100.0]);
        assert!(!anchor_matches(&host, &0, "secure", &snapshot, r(11, 0)));
        host.nodes[5].frame = Some(snapshot.inline_render.as_ref().unwrap().clip_rect);
        host.nodes[5].flags.insert("AXProtectedContent".into());
        assert!(!anchor_matches(&host, &0, "secure", &snapshot, r(11, 0)));
    }

    #[test]
    fn monaco_relay_resolution_skips_full_width_native_fields() {
        let mut host = monaco_relay_host();
        let raw = [20.0, 20.0, 180.0, 100.0];
        host.nodes[0].frame = Some(raw);
        for node in 0..host.nodes.len() {
            host.forbidden.insert((node, "AXDOMClassList".into()));
            host.forbidden.insert((node, "AXChildren".into()));
        }
        assert_eq!(visible_clip(&host, &0, "secure", raw), Some(raw));
    }

    #[test]
    fn ordinary_inline_anchor_skips_renderer_metadata_and_extra_ancestry_reads() {
        let mut host = monaco_relay_host();
        let raw = [0.0, 0.0, 240.0, 140.0];
        host.nodes[0].frame = Some(raw);
        host.forbidden.insert((0, "AXParent".into()));
        host.forbidden.insert((0, "AXDOMClassList".into()));
        host.forbidden.insert((1, "AXChildren".into()));
        let snapshot = Snapshot {
            text: "Hello world".into(),
            caret: 11,
            caret_rect: Some(CARET),
            field_rect: Some(raw),
            window_bounds: host.nodes[3].frame,
            inline_render: Some(InlineRender {
                clip_rect: [20.0, 20.0, 180.0, 100.0],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(anchor_matches(&host, &0, "secure", &snapshot, r(11, 0)));
    }

    #[test]
    fn caret_row_correction_preserves_x_and_document_end_requires_ltr_glyph() {
        let mut host = Host::tree("abc", &[]);
        host.rows.insert((0, 2, 0), [400.0, 40.0, 0.0, 20.0]);
        host.rows.insert((0, 2, 1), [390.0, 60.0, 10.0, 20.0]);
        assert_eq!(
            caret(&host, &0, "secure", &"abc".into(), 2, 2),
            Some([400.0, 60.0, 1.0, 20.0])
        );
        host.rows.insert((0, 2, 1), [390.0, 20.0, 10.0, 20.0]);
        assert_eq!(
            caret(&host, &0, "secure", &"abc".into(), 2, 2),
            Some([400.0, 40.0, 1.0, 20.0])
        );
        for (text, at, location, accepted) in [
            ("abc", 3, 3, true),
            ("abc", 2, 2, false),
            ("abc\n", 4, 4, false),
            ("אבג", 3, 3, false),
            ("e\u{301}", 2, 2, false),
            ("😀", 1, 2, true),
            ("😀", 1, 1, false),
        ] {
            let mut host = Host::tree(text, &[]);
            let units = text.chars().next_back().unwrap().len_utf16();
            if location >= units {
                host.rows
                    .insert((0, location - units, units), [90.0, 60.0, 10.0, 20.0]);
            }
            assert_eq!(
                caret(&host, &0, "secure", &text.into(), at, location),
                accepted.then_some([100.0, 60.0, 1.0, 20.0]),
                "{text:?}"
            );
        }
    }
    #[test]
    fn exact_attributed_font_uses_glyph_baseline_not_caret_height() {
        let host = Host::metrics("Hello world");
        let (render, evidence) = host.metrics_result("Hello world", 11, CARET, false);
        let render = render.unwrap();
        assert_eq!(
            (render.font_name.as_str(), render.font_size, render.baseline),
            ("Host-Regular", 13.0, 56.0)
        );
        assert!(evidence.is_none());
        assert_eq!(
            host.metrics_result("Hello world", 11, [100.0, 40.0, 1.0, 30.0], false)
                .0
                .unwrap()
                .baseline,
            56.0
        );
        for private in [".AppleSystemUIFont", ".SFNS-Semibold"] {
            let mut host = Host::metrics("Hello world");
            host.attributed.as_mut().unwrap().runs[0].1 = font_attributes(private);
            let render = host
                .metrics_result("Hello world", 11, CARET, false)
                .0
                .unwrap();
            assert!(render.system_font);
            assert_eq!(render.font_name, ".SFNS-Regular");
        }
        for private in [
            ".SFNS-Italic",
            ".SFNS-Rounded",
            ".SFNS-Mono",
            ".SFNS-Condensed",
        ] {
            assert!(system_weight(private).is_none());
        }
    }
    #[test]
    fn longest_typography_runs_ignore_spelling_but_not_font_or_marked_boundaries() {
        let mut host = Host::metrics("Hello world");
        let font = font_attributes("Host-Regular");
        let mut spelling = font.clone();
        spelling.insert("AXMisspelled".into(), TestValue::Bool(true));
        host.attributed.as_mut().unwrap().runs = vec![(r(0, 5), spelling), (r(5, 6), font.clone())];
        assert!(
            host.metrics_result("Hello world", 11, CARET, false)
                .0
                .is_some()
        );
        for key in &TYPOGRAPHY[1..] {
            let mut host = Host::metrics("Hello world");
            host.attributed.as_mut().unwrap().runs[0]
                .1
                .insert((*key).into(), TestValue::Number(1.0));
            let (render, evidence) = host.metrics_result("Hello world", 11, CARET, false);
            assert!(render.is_none() && evidence.is_none(), "{key}");
        }
        for key in [
            "NSMarkedClauseSegment",
            "NSMarkedClauseSegmentAttributeName",
        ] {
            let mut host = Host::metrics("Hello world");
            host.attributed.as_mut().unwrap().runs[0]
                .1
                .insert(key.into(), TestValue::Number(0.0));
            assert_eq!(
                host.metrics_result("Hello world", 11, CARET, false),
                (None, None)
            );
        }
    }
    #[test]
    fn attributed_identity_ranges_and_surrogate_boundaries_are_checked() {
        for range in [r(0, 10), r(0, 12), r(11, 0), r(usize::MAX, 2)] {
            let mut host = Host::metrics("Hello world");
            host.attributed.as_mut().unwrap().forced_range = Some(range);
            assert_eq!(
                host.metrics_result("Hello world", 11, CARET, false),
                (None, None)
            );
        }
        let mut host = Host::metrics("Hello world");
        host.attributed.as_mut().unwrap().text = "Other draft".into();
        assert_eq!(
            host.metrics_result("Hello world", 11, CARET, false),
            (None, None)
        );
        let mut host = Host::metrics("😀abc");
        host.rows.insert((0, 0, 5), [40.0, 40.0, 60.0, 20.0]);
        host.attributed.as_mut().unwrap().forced_range = Some(r(1, 4));
        assert_eq!(host.metrics_result("😀abc", 4, CARET, false), (None, None));
    }
    #[test]
    fn typography_run_reuses_exact_unicode_prefix_conversion() {
        let text = "😀e\u{301}world";
        let mut host = Host::metrics(text);
        // The run starts after an astral scalar and a decomposed accent.
        // Its local UTF-16 offset is not a scalar or UTF-8 byte offset.
        host.attributed.as_mut().unwrap().runs = vec![
            (r(0, 4), font_attributes("Other")),
            (r(4, 5), font_attributes("Host-Regular")),
        ];
        host.rows.extend([
            ((0, 0, 9), [40.0, 40.0, 60.0, 20.0]),
            ((0, 4, 5), [70.0, 40.0, 30.0, 20.0]),
            ((0, 6, 3), [82.0, 40.0, 18.0, 20.0]),
        ]);
        host.widths
            .extend([("world".into(), 30.0), ("rld".into(), 18.0)]);
        let render = host.metrics_result(text, 8, CARET, false).0.unwrap();
        assert_eq!(render.font_name, "Host-Regular");
        assert!(host.requests.borrow().contains(&(0, 4, 5)));
        assert!(host.requests.borrow().contains(&(0, 6, 3)));
    }
    #[test]
    fn two_span_calibration_uses_caret_advances_and_integer_rounding() {
        let mut host = Host::metrics("Hello world");
        host.attributed.as_mut().unwrap().runs = vec![
            (r(0, 10), font_attributes("Other")),
            (r(10, 1), font_attributes("Host-Regular")),
        ];
        host.widths.insert("d".into(), 6.0);
        for (width, accepted) in [(7.0, true), (10.0, false)] {
            host.rows.insert((0, 10, 1), [94.0, 40.0, width, 20.0]);
            assert_eq!(
                host.metrics_result("Hello world", 11, CARET, false)
                    .0
                    .is_some(),
                accepted
            );
        }
        let mut host = Host::metrics("ally");
        host.rows.insert((0, 0, 4), [43.0, 40.0, 22.0, 20.0]);
        host.widths
            .extend([("ally".into(), 20.5029296875), ("ly".into(), 10.1943359375)]);
        for (x, accepted) in [(53.0, true), (52.0, false)] {
            host.rows.insert((0, 2, 2), [x, 40.0, 65.0 - x, 20.0]);
            assert_eq!(
                host.metrics_result("ally", 4, [64.0, 40.0, 1.0, 20.0], false)
                    .0
                    .is_some(),
                accepted
            );
        }
    }
    #[test]
    fn offered_browser_zoom_requires_independent_part_and_recreated_font() {
        let mut host = Host::metrics("Hello world");
        host.rows.insert((0, 0, 11), [10.0, 40.0, 90.0, 30.0]);
        host.rows.insert((0, 5, 6), [55.0, 40.0, 45.0, 30.0]);
        let rect = [100.0, 40.0, 1.0, 30.0];
        assert!(
            host.metrics_result("Hello world", 11, rect, false)
                .0
                .is_none()
        );
        assert_eq!(
            host.metrics_result("Hello world", 11, rect, true)
                .0
                .unwrap()
                .font_size,
            19.5
        );
        host.rows.insert((0, 5, 6), [50.0, 40.0, 50.0, 30.0]);
        assert!(
            host.metrics_result("Hello world", 11, rect, true)
                .0
                .is_none()
        );
        let font = TestFont {
            name: "Host-Regular".into(),
            size: 13.0,
        };
        assert_eq!(
            calibrate(
                &host,
                &font,
                "Hello world",
                " world",
                72.0,
                36.0,
                true,
                0.75
            ),
            None
        );
    }
    #[test]
    fn caret_row_refusals_distinguish_literal_tabs_from_blank_samples_and_direction() {
        for (value, at, expected) in [
            ("Hello\n", 6, "caret_row_prefix_empty"),
            ("Hello\tworld", 5, "caret_row_suffix_tab"),
            ("שלום", 4, "caret_row_direction_unverified"),
            ("Hello שלום", 5, "caret_row_direction_unverified"),
            ("Hello\t", 6, "caret_row_gap_geometry_unverified"),
            ("Hello\t  ", 8, "caret_row_gap_geometry_unverified"),
            (
                "Hello                                ",
                37,
                "caret_row_gap_geometry_unverified",
            ),
        ] {
            let host = Host::metrics(value);
            let text = TextBuffer::from(value);
            let mut diagnostic = "not_observed";
            let result = inline(
                &host,
                &0,
                "secure",
                &text,
                at,
                text.index().scalar_to_utf16(ScalarIndex(at)).unwrap().0,
                Some(CARET),
                Some([0.0, 0.0, 400.0, 200.0]),
                false,
                &mut diagnostic,
                None,
            );
            assert_eq!(result, (None, None), "{expected}");
            assert_eq!(diagnostic, expected);
            if expected == "caret_row_gap_geometry_unverified" {
                // Eligible text is insufficient: the native sample-end
                // anchor is absent, so no direct render or vision fallback.
                assert!(!host.requests.borrow().is_empty());
            } else {
                assert!(host.requests.borrow().is_empty());
            }
        }
    }
    fn trailing_tab_host(gap: &str, width: f64) -> (Host, String, usize, Rect) {
        let text = format!("Hello world{gap}");
        let location = text.encode_utf16().count();
        let mut host = Host::metrics(&text);
        let caret = [100.0 + width, 40.0, 1.0, 20.0];
        host.rows.extend([
            ((0, 11, 0), [100.0, 40.0, 0.0, 20.0]),
            ((0, location, 0), caret),
            ((0, 11, location - 11), [100.0, 40.0, width, 20.0]),
        ]);
        (host, text, location, caret)
    }
    #[test]
    fn gap_diagnostics_preserve_refusals_and_native_read_order_without_content() {
        for (defect, expected) in [
            ("sample_missing", GapFailure::SampleEndUnavailable),
            ("caret_missing", GapFailure::CaretUnavailable),
            ("gap_missing", GapFailure::GapUnavailable),
            ("raw_row", GapFailure::RawEndpointRowMismatch),
            ("endpoint_order", GapFailure::EndpointOrderInvalid),
            ("gap_end", GapFailure::GapEndMismatch),
            ("sample_clipped", GapFailure::SampleEndClipped),
        ] {
            let (mut host, _, location, caret) = trailing_tab_host("\t", 43.0);
            let mut clip = [0.0, 0.0, 400.0, 200.0];
            match defect {
                "sample_missing" => {
                    host.rows.remove(&(0, 11, 0));
                }
                "caret_missing" => {
                    host.rows.remove(&(0, location, 0));
                }
                "gap_missing" => {
                    host.rows.remove(&(0, 11, 1));
                }
                "raw_row" => host.rows.get_mut(&(0, 11, 0)).unwrap()[1] = 20.0,
                "endpoint_order" => host.rows.get_mut(&(0, 11, 0)).unwrap()[0] = caret[0],
                "gap_end" => host.rows.get_mut(&(0, 11, 1)).unwrap()[2] = 33.0,
                "sample_clipped" => clip = [101.0, 0.0, 299.0, 200.0],
                _ => unreachable!(),
            }
            let baseline = native_gap_anchor(&host, &0, 11, location, caret, clip, None);
            let reads = host.requests.borrow().clone();
            host.requests.borrow_mut().clear();
            let mut diagnostic = None;
            let observed =
                native_gap_anchor(&host, &0, 11, location, caret, clip, Some(&mut diagnostic));
            assert_eq!(observed, baseline);
            assert!(observed.is_none());
            assert_eq!(*host.requests.borrow(), reads);
            let diagnostic = diagnostic.unwrap();
            assert_eq!(diagnostic.reason, Some(expected));
            assert_eq!(diagnostic.captured_caret, caret);
            assert_eq!(diagnostic.clip, clip);
            if defect == "sample_missing" {
                assert_eq!(reads.len(), 1);
                assert!(diagnostic.raw_sample_end.is_none());
                assert!(diagnostic.raw_caret.is_none());
                assert!(diagnostic.raw_gap.is_none());
            }
            let exported = serde_json::to_value(diagnostic).unwrap();
            for (key, value) in exported.as_object().unwrap() {
                if key == "reason" {
                    assert!(value.is_string());
                } else if !value.is_null() {
                    let rect = value
                        .as_array()
                        .expect("only numeric rectangles may be exported");
                    assert_eq!(rect.len(), 4);
                    assert!(rect.iter().all(serde_json::Value::is_number));
                }
            }
            let serialized = serde_json::to_string(&exported).unwrap();
            for private in ["Hello", "world", "location", "length", "field_id", "pid"] {
                assert!(!serialized.contains(private));
            }
        }
    }
    #[test]
    fn gap_diagnostics_keep_raw_coordinates_and_do_not_replace_failure_with_success() {
        let (mut host, _, location, caret) = trailing_tab_host("\t", 43.0);
        host.rows.get_mut(&(0, 11, 0)).unwrap()[1] = 20.0;
        host.rows.get_mut(&(0, location, 0)).unwrap()[1] = 20.0;
        host.rows.get_mut(&(0, 11, 1)).unwrap()[2] = 33.0;
        let clip = [0.0, 0.0, 400.0, 200.0];
        let mut diagnostic = None;
        assert!(
            native_gap_anchor(&host, &0, 11, location, caret, clip, Some(&mut diagnostic),)
                .is_none()
        );
        let failure = diagnostic.unwrap();
        // The narrow gap does not reach the current endpoint, so normalization
        // cannot relabel that endpoint as if a compatible glyph were observed.
        assert_eq!(failure.raw_sample_end.unwrap()[1], 20.0);
        assert_eq!(failure.normalized_sample_end.unwrap()[1], 40.0);
        assert_eq!(failure.raw_caret.unwrap()[1], 20.0);
        assert_eq!(failure.normalized_caret.unwrap()[1], 20.0);
        let mut retained = None;
        failure.retain_failure(&mut retained);
        assert_eq!(retained, Some(failure));

        host.rows.get_mut(&(0, 11, 1)).unwrap()[2] = 43.0;
        let mut ready = None;
        assert!(
            native_gap_anchor(&host, &0, 11, location, caret, clip, Some(&mut ready),).is_some()
        );
        let ready = ready.unwrap();
        assert!(ready.reason.is_none());
        ready.retain_failure(&mut retained);
        assert_eq!(retained, Some(failure));
    }
    #[test]
    fn trailing_tab_uses_independent_native_sample_and_caret_anchors() {
        for gap in ["\t", "\t  ", "\t\t"] {
            for width in [17.0, 43.0, 89.0] {
                let (host, text, location, caret) = trailing_tab_host(gap, width);
                let (render, evidence) =
                    host.metrics_result(&text, text.chars().count(), caret, false);
                let render = render.expect("same-row native tab gap has a verified font sample");
                assert_eq!(render.font_name, "Host-Regular");
                assert_eq!(render.font_size, 13.0);
                assert_eq!(render.baseline, 56.0);
                assert!(render.valid(Some(caret)));
                assert!(render.suffix.is_empty());
                assert_eq!(render.fixed_advance, 0.0);
                assert!(evidence.is_none());
                for range in [(0, 11, 0), (0, location, 0), (0, 11, location - 11)] {
                    assert!(host.requests.borrow().contains(&range));
                }
                // This backend has no tab/gap font width. Success must come
                // from native anchors, not an estimated or measured tab width.
                assert!(!host.widths.contains_key(gap));
            }
        }
    }
    fn trailing_tab_offset_row_host() -> (Host, String, usize, Rect) {
        let (mut host, text, location, mut caret) = trailing_tab_host("\t", 43.0);
        // Owned AppKit evidence: raw AX zero ranges y=207/h=19, glyph row
        // y=226/h=19. Translate that same convention to y=21/40 here.
        for rect in host.rows.values_mut() {
            rect[3] = 19.0;
        }
        host.rows.get_mut(&(0, 11, 0)).unwrap()[1] = 21.0;
        host.rows.get_mut(&(0, location, 0)).unwrap()[1] = 21.0;
        host.rows.insert((0, 10, 1), [94.0, 40.0, 6.0, 19.0]);
        host.ascent = 15.0;
        host.descent = 4.0;
        host.line_height = 19.0;
        host.baseline = 15.0;
        caret[3] = 19.0;
        (host, text, location, caret)
    }
    #[test]
    fn trailing_tab_uses_glyph_row_with_observed_offset_zero_range_convention() {
        let (host, value, location, expected_caret) = trailing_tab_offset_row_host();
        let text = TextBuffer::from(value.as_str());
        let observed = caret(&host, &0, "secure", &text, text.scalar_len(), location);
        assert_eq!(observed, Some(expected_caret));
        let (render, evidence) =
            host.metrics_result(&value, text.scalar_len(), observed.unwrap(), false);
        let render = render.expect("native glyph row resolves the offset zero-range convention");
        assert_eq!(render.baseline, 55.0);
        assert_eq!(render.source, "macos-ax-attributed-run");
        assert!(render.valid(Some(expected_caret)));
        assert!(evidence.is_none());
        // Neither font calibration nor row normalization changes raw AX X.
        assert_eq!(host.rows[&(0, 11, 0)][0], 100.0);
        assert_eq!(host.rows[&(0, location, 0)][0], expected_caret[0]);
    }
    #[test]
    fn trailing_tab_offset_row_proof_rejects_changed_zero_and_glyph_rows() {
        for defect in [
            "zero_rows_disagree",
            "zero_rows_outside_glyph",
            "sample_zero_height_changed",
            "both_zero_heights_changed",
            "gap_height_changed",
            "captured_caret_height_changed",
            "gap_glyph_row_changed",
            "sample_glyph_row_changed",
            "zero_range_missing",
        ] {
            let (mut host, value, location, mut caret) = trailing_tab_offset_row_host();
            match defect {
                "zero_rows_disagree" => host.rows.get_mut(&(0, 11, 0)).unwrap()[1] = 40.0,
                "zero_rows_outside_glyph" => {
                    host.rows.get_mut(&(0, 11, 0)).unwrap()[1] = 0.0;
                    host.rows.get_mut(&(0, location, 0)).unwrap()[1] = 0.0;
                }
                "sample_zero_height_changed" => {
                    host.rows.get_mut(&(0, 11, 0)).unwrap()[3] = 21.0;
                }
                "both_zero_heights_changed" => {
                    host.rows.get_mut(&(0, 11, 0)).unwrap()[3] = 21.0;
                    host.rows.get_mut(&(0, location, 0)).unwrap()[3] = 21.0;
                }
                "gap_height_changed" => host.rows.get_mut(&(0, 11, 1)).unwrap()[3] = 21.0,
                "captured_caret_height_changed" => caret[3] = 21.0,
                "gap_glyph_row_changed" => host.rows.get_mut(&(0, 11, 1)).unwrap()[1] = 60.0,
                "sample_glyph_row_changed" => {
                    host.rows.get_mut(&(0, 0, 11)).unwrap()[1] = 60.0;
                    host.rows.get_mut(&(0, 5, 6)).unwrap()[1] = 60.0;
                }
                "zero_range_missing" => {
                    host.rows.remove(&(0, location, 0));
                }
                _ => unreachable!(),
            }
            assert_eq!(
                host.metrics_result(&value, value.chars().count(), caret, false),
                (None, None),
                "{defect}"
            );
        }
    }
    #[test]
    fn trailing_tab_sample_ranges_preserve_utf16_offsets_after_non_bmp_text() {
        let text = "😀\nHello world\t";
        let mut host = Host::metrics(text);
        host.rows.clear();
        host.rows.extend([
            ((0, 3, 11), [40.0, 40.0, 60.0, 20.0]),
            ((0, 8, 6), [70.0, 40.0, 30.0, 20.0]),
            ((0, 14, 0), [100.0, 40.0, 0.0, 20.0]),
            ((0, 15, 0), [143.0, 40.0, 1.0, 20.0]),
            ((0, 14, 1), [100.0, 40.0, 43.0, 20.0]),
        ]);
        let (render, evidence) =
            host.metrics_result(text, text.chars().count(), [143.0, 40.0, 1.0, 20.0], false);
        assert_eq!(render.unwrap().baseline, 56.0);
        assert!(evidence.is_none());
        for range in [(0, 3, 11), (0, 8, 6), (0, 14, 0), (0, 15, 0), (0, 14, 1)] {
            assert!(host.requests.borrow().contains(&range));
        }
    }
    #[test]
    fn trailing_tab_refuses_missing_changed_or_wrapped_native_gap_geometry() {
        for defect in [
            "sample_anchor_missing",
            "caret_anchor_missing",
            "gap_missing",
            "sample_other_row",
            "caret_changed",
            "gap_other_row",
            "gap_multiline",
            "gap_start_changed",
            "gap_end_changed",
        ] {
            let (mut host, text, location, caret) = trailing_tab_host("\t", 43.0);
            match defect {
                "sample_anchor_missing" => {
                    host.rows.remove(&(0, 11, 0));
                }
                "caret_anchor_missing" => {
                    host.rows.remove(&(0, location, 0));
                }
                "gap_missing" => {
                    host.rows.remove(&(0, 11, 1));
                }
                "sample_other_row" => host.rows.get_mut(&(0, 11, 0)).unwrap()[1] = 20.0,
                "caret_changed" => host.rows.get_mut(&(0, location, 0)).unwrap()[0] = 153.0,
                "gap_other_row" => host.rows.get_mut(&(0, 11, 1)).unwrap()[1] = 60.0,
                "gap_multiline" => host.rows.get_mut(&(0, 11, 1)).unwrap()[3] = 40.0,
                "gap_start_changed" => host.rows.get_mut(&(0, 11, 1)).unwrap()[0] = 90.0,
                "gap_end_changed" => host.rows.get_mut(&(0, 11, 1)).unwrap()[2] = 53.0,
                _ => unreachable!(),
            }
            assert_eq!(
                host.metrics_result(&text, text.chars().count(), caret, false),
                (None, None),
                "{defect}"
            );
        }
    }
    #[test]
    fn trailing_tab_requires_current_typography_and_direct_ax_font_proof() {
        for defect in [
            "gap_font_changed",
            "gap_baseline_changed",
            "gap_text_changed",
            "gap_marked_text",
            "missing_font",
            "unavailable_font",
            "uncalibrated_font",
        ] {
            let (mut host, text, _, caret) = trailing_tab_host("\t", 43.0);
            match defect {
                "gap_font_changed" | "gap_baseline_changed" => {
                    let mut gap = font_attributes("Host-Regular");
                    if defect == "gap_font_changed" {
                        gap = font_attributes("Other");
                    } else {
                        gap.insert("NSBaselineOffset".into(), TestValue::Number(2.0));
                    }
                    host.attributed.as_mut().unwrap().runs =
                        vec![(r(0, 11), font_attributes("Host-Regular")), (r(11, 1), gap)];
                }
                "gap_text_changed" => {
                    host.attributed.as_mut().unwrap().text = "Hello world ".into();
                }
                "gap_marked_text" => {
                    host.attributed.as_mut().unwrap().runs[0]
                        .1
                        .insert("AXMarkedText".into(), TestValue::Bool(true));
                }
                "missing_font" => host.attributed.as_mut().unwrap().runs[0].1.clear(),
                "unavailable_font" => host.reject_font = true,
                "uncalibrated_font" => {
                    host.widths.insert("Hello world".into(), 100.0);
                }
                _ => unreachable!(),
            }
            assert_eq!(
                host.metrics_result(&text, text.chars().count(), caret, false),
                (None, None),
                "{defect}"
            );
        }
    }
    #[test]
    fn trailing_tab_does_not_search_unbounded_whitespace_for_a_sample() {
        let gap = "\t".repeat(129);
        let (host, text, _, caret) = trailing_tab_host(&gap, 43.0);
        assert_eq!(
            host.metrics_result(&text, text.chars().count(), caret, false),
            (None, None)
        );
        assert!(host.requests.borrow().is_empty());
    }
    #[test]
    fn soft_wrap_samples_shrink_to_observed_row() {
        let text = "previous row. Hello world";
        let mut host = Host::metrics(text);
        host.rows.clear();
        host.rows.extend([
            ((0, 0, 25), [0.0, 40.0, 100.0, 40.0]),
            ((0, 13, 12), [0.0, 40.0, 100.0, 40.0]),
            ((0, 19, 6), [70.0, 60.0, 30.0, 20.0]),
            ((0, 22, 3), [85.0, 60.0, 15.0, 20.0]),
        ]);
        host.widths.insert("rld".into(), 15.0);
        let (render, evidence) = host.metrics_result(text, 25, [100.0, 60.0, 1.0, 20.0], false);
        assert_eq!(render.unwrap().baseline, 76.0);
        assert!(evidence.is_none());
    }
    #[test]
    fn visible_range_sentinel_and_clipped_rows_gate_attributed_reads() {
        for (shown, accepted) in [(r(0, 0), true), (r(0, 5), false), (r(0, 11), true)] {
            let mut host = Host::metrics("Hello world");
            host.visible = Some(Ok(shown));
            assert_eq!(
                host.metrics_result("Hello world", 11, CARET, false)
                    .0
                    .is_some(),
                accepted
            );
        }
        let mut host = Host::metrics("Hello world");
        host.visible = Some(Err(()));
        assert_eq!(
            host.metrics_result("Hello world", 11, CARET, false),
            (None, None)
        );
        let mut host = Host::metrics("Hello world");
        host.nodes.push(Node::default());
        host.nodes[0].elements.insert("AXParent".into(), 1);
        host.nodes[1].frame = Some([0.0, 60.0, 400.0, 140.0]);
        assert_eq!(
            host.metrics_result("Hello world", 11, CARET, false),
            (None, None)
        );
    }
    #[test]
    fn fitted_system_font_is_eol_only_and_never_claims_host_identity() {
        for (text, accepted) in [("Hello world", true), ("Hello world tail", false)] {
            let mut host = Host::metrics(text);
            host.attributed.as_mut().unwrap().runs[0].1.clear();
            let (render, evidence) = host.metrics_result(text, 11, CARET, false);
            assert_eq!(render.is_some(), accepted);
            assert!(evidence.is_none());
            if let Some(render) = render {
                assert!(render.system_font);
                assert_eq!(render.source, "macos-ax-caret-row");
                assert_eq!(render.baseline, 56.0);
            }
        }
    }
    // Released 0.5.4 synthetic TextEdit fixture: independent NSLayoutManager
    // layout places the baseline at 10 in a 13-point first-row fragment.
    const TEXTEDIT_TEXT: &str = "I would appreciate your feedb";
    const TEXTEDIT_CARET: Rect = [416.05419921875, 947.0, 1.0, 13.0];

    fn textedit_first_row_host() -> Host {
        let mut host = Host::metrics(TEXTEDIT_TEXT);
        let attributes = &mut host.attributed.as_mut().unwrap().runs[0].1;
        *attributes = font_attributes("Menlo-Regular");
        let TestValue::Dictionary(font) = attributes.get_mut("AXFont").unwrap() else {
            unreachable!()
        };
        font.insert("AXFontSize".into(), TestValue::Number(11.0));
        let normalization = 13.0 / 11.0;
        host.ascent = 10.21044921875 * normalization;
        host.descent = 2.59423828125 * normalization;
        host.line_height = 13.0 * normalization;
        host.baseline = 10.0 * normalization;
        host.widths = HashMap::from([
            (TEXTEDIT_TEXT.into(), 192.05419921875 * normalization),
            (TEXTEDIT_TEXT[14..].into(), 99.33837890625 * normalization),
        ]);
        host.rows = HashMap::from([
            ((0, 0, 29), [224.0, 947.0, 192.05419921875, 13.0]),
            ((0, 14, 15), [316.7158203125, 947.0, 99.33837890625, 13.0]),
            ((0, 28, 1), [409.431640625, 947.0, 6.62255859375, 13.0]),
            ((0, 29, 0), [416.05419921875, 934.0, 0.0, 13.0]),
        ]);
        host.nodes[0].frame = Some([214.0, 947.0, 656.0, 384.0]);
        host.visible = Some(Ok(r(0, 29)));
        host
    }

    fn textedit_first_row_result(
        host: &Host,
        browser: bool,
    ) -> (Option<InlineRender>, Option<AppearanceEvidence>) {
        let text = TextBuffer::from(TEXTEDIT_TEXT);
        let observed_caret = caret(host, &0, "secure", &text, 29, 29);
        inline(
            host,
            &0,
            "secure",
            &text,
            29,
            29,
            observed_caret,
            host.nodes[0].frame,
            browser,
            &mut "not_observed",
            None,
        )
    }

    #[test]
    fn native_first_row_preserves_baseline_and_uses_verified_line_fragment_envelope() {
        let host = textedit_first_row_host();
        assert_eq!(
            caret(&host, &0, "secure", &TEXTEDIT_TEXT.into(), 29, 29),
            Some(TEXTEDIT_CARET)
        );
        let (render, evidence) = textedit_first_row_result(&host, false);
        let render = render.expect("visible native first row must offer inline geometry");
        assert!(evidence.is_none());
        assert_eq!(render.font_name, "Menlo-Regular");
        assert_eq!(render.font_size, 11.0);
        assert_eq!(render.baseline, 957.0);
        assert!((render.ascent - 10.0).abs() < 1e-9);
        assert!((render.descent - 3.0).abs() < 1e-9);
        assert_eq!(render.clip_rect, host.nodes[0].frame.unwrap());
        assert!(render.valid(Some(TEXTEDIT_CARET)));
        // These are the bounds consumed by the overlay, not a shifted baseline
        // or an expanded host clip hiding the nominal ascender discrepancy.
        assert_eq!(render.baseline - render.ascent, 947.0);
        assert_eq!(render.baseline + render.descent, 960.0);

        let (browser, _) = textedit_first_row_result(&host, true);
        let browser = browser.unwrap();
        assert!((browser.ascent - 10.21044921875).abs() < 1e-9);
        assert!((browser.descent - 2.59423828125).abs() < 1e-9);
        assert!((browser.baseline - 957.21044921875).abs() < 1e-9);
    }

    #[test]
    fn native_line_envelope_does_not_repair_clipping_mismatch_or_unsafe_evidence() {
        for case in 0..11 {
            let mut host = textedit_first_row_host();
            match case {
                0 => host.nodes[0].frame.as_mut().unwrap()[1] += 0.25,
                1 => host.nodes[0].frame.as_mut().unwrap()[3] = 11.0,
                2 => host.baseline += 2.0 * 13.0 / 11.0,
                3 => {
                    host.widths.insert(TEXTEDIT_TEXT[14..].into(), 150.0);
                }
                4 => host.rows.get_mut(&(0, 14, 15)).unwrap()[1] += 2.0,
                5 => {
                    host.nodes[0].flags.insert("AXProtectedContent".into());
                }
                6 => host.ascent = f64::NAN,
                7 => host.descent = -1.0,
                8 => host.ascent = 0.0,
                9 => host.descent = f64::INFINITY,
                10 => host.ascent = 40.0,
                _ => unreachable!(),
            }
            assert!(
                textedit_first_row_result(&host, false).0.is_none(),
                "case {case}"
            );
        }
    }

    #[test]
    fn baseline_models_require_agreement_and_allow_only_observed_browser_leading() {
        for (height, accepted) in [(21.0, true), (22.0, false)] {
            let mut host = Host::metrics("Hello world");
            host.ascent = 15.0;
            host.rows.get_mut(&(0, 0, 11)).unwrap()[3] = height;
            host.rows.get_mut(&(0, 5, 6)).unwrap()[3] = height;
            assert_eq!(
                host.metrics_result("Hello world", 11, [100.0, 40.0, 1.0, height], false)
                    .0
                    .is_some(),
                accepted
            );
        }
        let mut host = Host::metrics("Hello world");
        host.baseline = 18.0;
        assert!(
            host.metrics_result("Hello world", 11, CARET, false)
                .1
                .is_some()
        );
        host.rows.get_mut(&(0, 0, 11)).unwrap()[3] = 30.0;
        host.rows.get_mut(&(0, 5, 6)).unwrap()[3] = 30.0;
        let rect = [100.0, 40.0, 1.0, 30.0];
        assert!(
            host.metrics_result("Hello world", 11, rect, false)
                .0
                .is_none()
        );
        assert_eq!(
            host.metrics_result("Hello world", 11, rect, true)
                .0
                .unwrap()
                .baseline,
            61.0
        );
    }
    #[test]
    fn unknown_or_substituted_fonts_offer_measured_evidence_not_guesses() {
        for substituted in [false, true] {
            let mut host = Host::metrics("Hello world");
            host.reject_font = !substituted;
            host.substitute_font = substituted;
            let (render, evidence) = host.metrics_result("Hello world", 11, CARET, false);
            assert!(render.is_none());
            let evidence = evidence.unwrap();
            assert_eq!(
                evidence
                    .samples
                    .iter()
                    .map(|s| s.text.as_str())
                    .collect::<Vec<_>>(),
                vec!["Hello world", " world"]
            );
            assert_eq!(evidence.caret_rect, CARET);
        }
        let mut host = Host::metrics("Hello world");
        host.widths.insert(" world".into(), 34.0);
        assert!(
            host.metrics_result("Hello world", 11, CARET, false)
                .1
                .is_some()
        );
    }
    #[test]
    fn suffix_displacement_requires_same_run_measured_width_and_optional_colours() {
        let host = Host::metrics("Hello world tail");
        let (render, evidence) = host.metrics_result("Hello world tail", 11, CARET, false);
        assert!(render.is_none());
        let evidence = evidence.unwrap();
        assert_eq!(evidence.suffix.as_ref().unwrap().text, " tail");
        assert_eq!(
            evidence.suffix.as_ref().unwrap().rect,
            [100.0, 40.0, 25.0, 20.0]
        );
        assert!(evidence.ax_render.unwrap().suffix.is_empty());
        let mut host = Host::metrics("Hello world ");
        host.widths.insert(" ".into(), 6.0);
        host.rows.insert((0, 11, 1), [100.0, 40.0, 6.0, 20.0]);
        let render = host
            .metrics_result("Hello world ", 11, CARET, false)
            .0
            .unwrap();
        assert_eq!((render.suffix.as_str(), render.suffix_width), (" ", 6.0));
        let mut host = Host::metrics("Hello world tail");
        host.attributed.as_mut().unwrap().runs = vec![
            (r(0, 11), font_attributes("Host-Regular")),
            (r(11, 5), font_attributes("Other")),
        ];
        assert_eq!(
            host.metrics_result("Hello world tail", 11, CARET, false),
            (None, None)
        );
        let mut host = Host::metrics("Hello world tail");
        host.rows.get_mut(&(0, 11, 5)).unwrap()[1] += 2.0;
        assert_eq!(
            host.metrics_result("Hello world tail", 11, CARET, false),
            (None, None)
        );
    }
    #[test]
    fn unreadable_or_malformed_privacy_and_incomplete_ancestry_are_not_absence() {
        for node in [0, 1] {
            for key in [
                "AXHidden",
                "AXProtectedContent",
                "AXSubrole",
                "AXRoleDescription",
                "AXRole",
            ] {
                let mut host = Host::tree("hello", &["hello"]);
                host.nodes[node].unreadable.insert(key.into());
                host.forbidden
                    .extend([(node, "AXValue".into()), (node, "AXChildren".into())]);
                assert_eq!(bounds(&host, &0, "secure", r(0, 1)), None, "{node}:{key}");
            }
            for key in ["AXHidden", "AXProtectedContent"] {
                let mut host = Host::tree("hello", &["hello"]);
                host.nodes[node].strings.insert(key.into(), "false".into());
                host.forbidden
                    .extend([(node, "AXValue".into()), (node, "AXChildren".into())]);
                assert_eq!(bounds(&host, &0, "secure", r(0, 1)), None);
            }
        }
        let mut host = Host::tree("hello", &["hello"]);
        host.nodes[1].unreadable.insert("AXChildren".into());
        host.forbidden.insert((1, "AXValue".into()));
        assert_eq!(bounds(&host, &0, "secure", r(0, 1)), None);
        let mut host = Host::tree("hello", &[]);
        host.nodes[0].unreadable.insert("AXParent".into());
        assert_eq!(ancestry(&host, &0, "secure"), None);
        assert_eq!(
            visible_clip(&host, &0, "secure", [0.0, 0.0, 400.0, 200.0]),
            None
        );
        let mut host = Host::tree("hello", &["hello"]);
        host.nodes[1].unreadable.insert("AXEditable".into());
        host.forbidden
            .extend([(1, "AXValue".into()), (1, "AXChildren".into())]);
        assert_eq!(bounds(&host, &0, "secure", r(0, 1)), None);
    }
}
