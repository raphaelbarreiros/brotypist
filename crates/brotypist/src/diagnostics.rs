//! Content-free, session-only request diagnostics. Recording never samples native
//! state or persists data. Owners supply monotonic observation instants; publication
//! is not a paint. Only an actual native draw callback may call `record_draw`.
//! Trace creation follows guarded eligible capture; an unavailable input timestamp
//! never implies a protected-field event. Trace offsets start at the first capture
//! start, while terminal-report offsets retain their separate submission origin.
use crate::providers::RequestMetrics;
use serde::Serialize;
use std::collections::VecDeque;
use std::time::Instant;

pub const TRACE_CAPACITY: usize = 128;
pub const TERMINAL_REPORT_CAPACITY: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkKind {
    Generated,
    RetainedGhost,
    Regenerated,
    Alternative,
    Preparation,
    LocalTool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Capture,
    Preparation,
    Ready,
    Proceed,
    ProviderPublication,
    NativeDraw,
}

/// Actual native presentation surfaces, not request kinds or authority revisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PresentationLane {
    Inline,
    Chooser,
}

/// Caller observations collected before creating a trace. `None` is unavailable
/// input notification time, not zero wait. Capture includes only the guarded
/// capture operation; queue/debounce wait precedes `started`.
#[derive(Clone, Copy, Debug)]
pub struct CaptureTiming {
    pub input_notified: Option<Instant>,
    pub started: Instant,
    pub finished: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Accepted,
    Dismissed,
    NoOffer,
    Cancelled,
    Superseded,
    Released,
    Stopped,
    AuthorityDenied,
    PrivacyDenied,
    Ineligible,
    StaleView,
    ProviderFailed,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalReason {
    Completed,
    Cancelled,
    Superseded,
    Released,
    Stopped,
    Failed,
    Unknown,
}

/// Supervisor observations, not provider execution timestamps. Missing native
/// metrics mean unavailable work, never zero. Instants are never serialized.
#[derive(Clone, Copy, Debug)]
pub struct TerminalReport {
    pub request_id: u64,
    pub preparing: bool,
    pub metrics: Option<RequestMetrics>,
    pub cancelled: bool,
    pub failed: bool,
    pub reason: TerminalReason,
    pub submitted: Instant,
    pub ready: Option<Instant>,
    pub proceed: Option<Instant>,
    pub first_result: Option<Instant>,
    pub terminal: Instant,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct TerminalSnapshot {
    pub request_id: u64,
    pub preparing: bool,
    pub metrics: Option<RequestMetrics>,
    pub cancelled: bool,
    pub failed: bool,
    pub reason: TerminalReason,
    pub ready_us: Option<u64>,
    pub proceed_us: Option<u64>,
    pub first_result_us: Option<u64>,
    pub terminal_us: Option<u64>,
}

fn elapsed(origin: Instant, at: Instant) -> Option<u64> {
    at.checked_duration_since(origin)
        .map(|duration| duration.as_micros().min(u64::MAX as u128) as u64)
}

impl TerminalReport {
    /// All offsets are relative to client submission, not capture or provider start.
    pub fn snapshot(self) -> TerminalSnapshot {
        TerminalSnapshot {
            request_id: self.request_id,
            preparing: self.preparing,
            metrics: self.metrics,
            cancelled: self.cancelled,
            failed: self.failed,
            reason: self.reason,
            ready_us: self.ready.and_then(|at| elapsed(self.submitted, at)),
            proceed_us: self.proceed.and_then(|at| elapsed(self.submitted, at)),
            first_result_us: self.first_result.and_then(|at| elapsed(self.submitted, at)),
            terminal_us: elapsed(self.submitted, self.terminal),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct TraceSnapshot {
    pub trace_id: u64,
    pub request_id: Option<u64>,
    pub lane: PresentationLane,
    pub view_serial: u64,
    pub kind: WorkKind,
    /// Input notification to first capture start; unavailable stays `None`.
    pub input_wait_us: Option<u64>,
    /// First capture end minus first capture start.
    pub first_capture_us: u64,
    /// Sum of nonoverlapping capture durations, excluding gaps; saturates at u64::MAX.
    pub total_capture_us: u64,
    /// Offsets from first capture START, indexed by Stage declaration order.
    /// Capture is the first capture END, not a zero-valued origin marker.
    pub stage_us: [Option<u64>; 6],
    pub outcome: Option<Outcome>,
    pub outcome_us: Option<u64>,
    pub submitted_us: Option<u64>,
    pub first_publication_us: Option<u64>,
    pub first_draw_us: Option<u64>,
    pub terminal: Option<TerminalSnapshot>,
}

/// Deliberately not a generic JSON container: no strings, paths, target metadata,
/// authored text, token IDs, or arbitrary errors can enter an exported snapshot.
#[derive(Clone, Debug, Serialize)]
pub struct DiagnosticSnapshot {
    pub version: u32,
    pub traces: Vec<TraceSnapshot>,
    /// Standalone worker preparations, with offsets from their own submission.
    pub preparations: Vec<TerminalSnapshot>,
    pub lost_preparations: u64,
    pub evicted_traces: u64,
    pub lost_reports: u64,
    pub unmatched_reports: u64,
    pub rejected_draws: u64,
}

struct Trace {
    snapshot: TraceSnapshot,
    captured: Instant,
    last_capture_finished: Instant,
}

#[derive(Clone, Copy, Default)]
struct LaneState {
    view_serial: u64,
    published: Option<u64>,
    closed: bool,
}

/// Fixed retention; allocation occurs at construction and explicit snapshot only.
/// Trace IDs are never reused, including after eviction. View serials are
/// caller-owned diagnostic IDs, monotonic independently within each lane.
/// A changed/cleared lane revokes its old draw attribution, never another lane's.
pub struct DiagnosticBook {
    traces: VecDeque<Trace>,
    preparations: VecDeque<TerminalSnapshot>,
    lost_preparations: u64,
    next_trace: u64,
    lanes: [LaneState; 2],
    evicted_traces: u64,
    lost_reports: u64,
    unmatched_reports: u64,
    rejected_draws: u64,
}

impl Default for DiagnosticBook {
    fn default() -> Self {
        Self::new()
    }
}

impl DiagnosticBook {
    pub fn new() -> Self {
        Self {
            traces: VecDeque::with_capacity(TRACE_CAPACITY),
            preparations: VecDeque::with_capacity(TERMINAL_REPORT_CAPACITY),
            lost_preparations: 0,
            next_trace: 0,
            lanes: [LaneState::default(); 2],
            evicted_traces: 0,
            lost_reports: 0,
            unmatched_reports: 0,
            rejected_draws: 0,
        }
    }

    /// Call only after guarded eligible capture. Invalid timing or a stale lane
    /// serial rejects the observation without changing any lane or trace.
    pub fn begin(
        &mut self,
        kind: WorkKind,
        lane: PresentationLane,
        view_serial: u64,
        timing: CaptureTiming,
    ) -> Option<u64> {
        if view_serial < self.lanes[lane as usize].view_serial {
            return None;
        }
        let first_capture_us = elapsed(timing.started, timing.finished)?;
        let input_wait_us = match timing.input_notified {
            Some(at) => Some(elapsed(at, timing.started)?),
            None => None,
        };
        let trace_id = self.next_trace.checked_add(1)?;
        self.advance_view(lane, view_serial);
        self.next_trace = trace_id;
        if self.traces.len() == TRACE_CAPACITY {
            if let Some(trace) = self.traces.pop_front() {
                for lane in &mut self.lanes {
                    if lane.published == Some(trace.snapshot.trace_id) {
                        lane.published = None;
                        lane.closed = true;
                    }
                }
            }
            self.evicted_traces = self.evicted_traces.saturating_add(1);
        }
        self.traces.push_back(Trace {
            captured: timing.started,
            last_capture_finished: timing.finished,
            snapshot: TraceSnapshot {
                trace_id,
                request_id: None,
                lane,
                view_serial,
                kind,
                input_wait_us,
                first_capture_us,
                total_capture_us: first_capture_us,
                stage_us: [Some(first_capture_us), None, None, None, None, None],
                outcome: None,
                outcome_us: None,
                terminal: None,
                submitted_us: None,
                first_publication_us: None,
                first_draw_us: None,
            },
        });
        Some(trace_id)
    }

    /// Add subsequent guarded capture work without moving the first-capture
    /// origin or counting idle gaps. Overlapping/reversed intervals are rejected.
    pub fn record_capture(&mut self, trace_id: u64, started: Instant, finished: Instant) -> bool {
        let Some(trace) = self
            .traces
            .iter_mut()
            .find(|trace| trace.snapshot.trace_id == trace_id)
        else {
            return false;
        };
        let Some(duration) = elapsed(started, finished) else {
            return false;
        };
        if trace.snapshot.outcome.is_some() || started < trace.last_capture_finished {
            return false;
        }
        trace.snapshot.total_capture_us = trace.snapshot.total_capture_us.saturating_add(duration);
        trace.last_capture_finished = finished;
        true
    }

    pub fn bind_request(&mut self, trace_id: u64, request_id: u64) -> bool {
        if request_id == 0
            || self
                .traces
                .iter()
                .any(|trace| trace.snapshot.request_id == Some(request_id))
        {
            return false;
        }
        let Some(trace) = self
            .traces
            .iter_mut()
            .find(|trace| trace.snapshot.trace_id == trace_id)
        else {
            return false;
        };
        if trace.snapshot.request_id.is_some()
            || matches!(
                trace.snapshot.kind,
                WorkKind::RetainedGhost | WorkKind::LocalTool
            )
        {
            return false;
        }
        trace.snapshot.request_id = Some(request_id);
        true
    }

    pub fn record_stage(&mut self, trace_id: u64, stage: Stage, at: Instant) -> bool {
        // Capture is supplied explicitly; paint/publication require a lane fence.
        if matches!(
            stage,
            Stage::Capture | Stage::ProviderPublication | Stage::NativeDraw
        ) {
            return false;
        }
        self.stage(trace_id, stage, at)
    }

    fn stage(&mut self, trace_id: u64, stage: Stage, at: Instant) -> bool {
        let Some(trace) = self
            .traces
            .iter_mut()
            .find(|trace| trace.snapshot.trace_id == trace_id)
        else {
            return false;
        };
        let Some(offset) = elapsed(trace.captured, at) else {
            return false;
        };
        let index = stage as usize;
        if trace.snapshot.outcome.is_some()
            || trace.snapshot.stage_us[index].is_some()
            || trace.snapshot.stage_us[..index]
                .iter()
                .flatten()
                .any(|previous| *previous > offset)
            || trace.snapshot.stage_us[index + 1..]
                .iter()
                .flatten()
                .any(|next| *next < offset)
        {
            return false;
        }
        trace.snapshot.stage_us[index] = Some(offset);
        true
    }

    /// Advance only this lane. Equal IDs are idempotent and cannot reopen a
    /// cleared view; each changed presentation must receive a fresh diagnostic ID.
    pub fn advance_view(&mut self, lane: PresentationLane, view_serial: u64) -> bool {
        let state = &mut self.lanes[lane as usize];
        if view_serial < state.view_serial {
            return false;
        }
        if view_serial > state.view_serial {
            *state = LaneState {
                view_serial,
                published: None,
                closed: false,
            };
        }
        true
    }

    /// Clear/hide this lane and prevent republishing the same diagnostic view.
    /// A stale clear cannot revoke a newer view.
    pub fn clear_view(&mut self, lane: PresentationLane, view_serial: u64) -> bool {
        if !self.advance_view(lane, view_serial) {
            return false;
        }
        let state = &mut self.lanes[lane as usize];
        state.published = None;
        state.closed = true;
        true
    }

    /// Authority loss invalidates every surface, without discarding terminal
    /// report attribution. Fresh views must use fresh per-lane diagnostic IDs.
    pub fn invalidate_all_views(&mut self) {
        for state in &mut self.lanes {
            state.published = None;
            state.closed = true;
        }
    }

    /// Bind the actual offered view, which may advance after capture. Replacing
    /// a streamed view resets its draw timestamp; first-publication/draw latency
    /// remains separately available. Old callbacks cannot paint the replacement.
    pub fn publish(
        &mut self,
        trace_id: u64,
        lane: PresentationLane,
        view_serial: u64,
        at: Instant,
    ) -> bool {
        let state = self.lanes[lane as usize];
        if view_serial < state.view_serial
            || (view_serial == state.view_serial && (state.closed || state.published.is_some()))
        {
            return false;
        }
        let Some(trace) = self
            .traces
            .iter_mut()
            .find(|trace| trace.snapshot.trace_id == trace_id)
        else {
            return false;
        };
        let Some(offset) = elapsed(trace.captured, at) else {
            return false;
        };
        if trace.snapshot.outcome.is_some()
            || trace.snapshot.lane != lane
            || at < trace.last_capture_finished
            || trace.snapshot.view_serial > view_serial
            || trace
                .snapshot
                .stage_us
                .iter()
                .flatten()
                .any(|stage| *stage > offset)
        {
            return false;
        }
        if trace.snapshot.stage_us[Stage::ProviderPublication as usize].is_some()
            && trace.snapshot.view_serial == view_serial
        {
            return false;
        }
        trace.snapshot.view_serial = view_serial;
        trace.snapshot.first_publication_us.get_or_insert(offset);
        trace.snapshot.stage_us[Stage::ProviderPublication as usize] = Some(offset);
        trace.snapshot.stage_us[Stage::NativeDraw as usize] = None;
        self.advance_view(lane, view_serial);
        self.lanes[lane as usize].published = Some(trace_id);
        true
    }

    pub fn record_draw(
        &mut self,
        trace_id: u64,
        lane: PresentationLane,
        view_serial: u64,
        at: Instant,
    ) -> bool {
        let state = self.lanes[lane as usize];
        if state.published != Some(trace_id) || state.view_serial != view_serial || state.closed {
            self.rejected_draws = self.rejected_draws.saturating_add(1);
            return false;
        }
        if !self.stage(trace_id, Stage::NativeDraw, at) {
            return false;
        }
        if let Some(trace) = self
            .traces
            .iter_mut()
            .find(|trace| trace.snapshot.trace_id == trace_id)
        {
            trace.snapshot.first_draw_us = trace
                .snapshot
                .first_draw_us
                .or(trace.snapshot.stage_us[Stage::NativeDraw as usize]);
        }
        true
    }

    pub fn finish(&mut self, trace_id: u64, outcome: Outcome, at: Instant) -> bool {
        let Some(trace) = self
            .traces
            .iter_mut()
            .find(|trace| trace.snapshot.trace_id == trace_id)
        else {
            return false;
        };
        let Some(offset) = elapsed(trace.captured, at) else {
            return false;
        };
        if trace.snapshot.outcome.is_some()
            || at < trace.last_capture_finished
            || trace
                .snapshot
                .stage_us
                .iter()
                .flatten()
                .any(|stage| *stage > offset)
        {
            return false;
        }
        trace.snapshot.outcome = Some(outcome);
        trace.snapshot.outcome_us = Some(offset);
        for state in &mut self.lanes {
            if state.published == Some(trace_id) {
                state.published = None;
                state.closed = true;
            }
        }
        true
    }

    /// Preparation reports have independent bounded retention, not fake captures.
    /// Writing matches survive outcome/view revocation; late reports never attach
    /// to a newer request. Unbound writing reports increment unmatched_reports.
    pub fn attach_terminal(&mut self, report: TerminalReport) -> bool {
        if report.preparing {
            if self.preparations.len() == TERMINAL_REPORT_CAPACITY {
                self.preparations.pop_front();
                self.lost_preparations = self.lost_preparations.saturating_add(1);
            }
            self.preparations.push_back(report.snapshot());
            return true;
        }
        let Some(trace) = self
            .traces
            .iter_mut()
            .find(|trace| trace.snapshot.request_id == Some(report.request_id))
        else {
            self.unmatched_reports = self.unmatched_reports.saturating_add(1);
            return false;
        };
        if trace.snapshot.terminal.is_some() {
            return false;
        }
        trace.snapshot.submitted_us = elapsed(trace.captured, report.submitted);
        // This is supervisor dispatch evidence, not runtime queue admission.
        // It may arrive after presentation/outcome retirement.
        trace.snapshot.stage_us[Stage::Proceed as usize] =
            report.proceed.and_then(|at| elapsed(trace.captured, at));
        trace.snapshot.terminal = Some(report.snapshot());
        true
    }

    /// Supply the delta since the previous observation of the worker's cumulative
    /// lost_terminal_reports counter, not that cumulative value repeatedly.
    pub fn note_lost_reports(&mut self, count: u64) {
        self.lost_reports = self.lost_reports.saturating_add(count);
    }

    pub fn snapshot(&self) -> DiagnosticSnapshot {
        DiagnosticSnapshot {
            version: 2,
            traces: self
                .traces
                .iter()
                .map(|trace| trace.snapshot.clone())
                .collect(),
            preparations: self.preparations.iter().copied().collect(),
            lost_preparations: self.lost_preparations,
            evicted_traces: self.evicted_traces,
            lost_reports: self.lost_reports,
            unmatched_reports: self.unmatched_reports,
            rejected_draws: self.rejected_draws,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn capture(at: Instant) -> CaptureTiming {
        CaptureTiming {
            input_notified: None,
            started: at,
            finished: at,
        }
    }

    #[test]
    fn standalone_preparation_retention_is_bounded_and_loss_saturates() {
        let mut book = DiagnosticBook::new();
        let now = Instant::now();
        let mut report = TerminalReport {
            request_id: 1,
            preparing: true,
            metrics: None,
            cancelled: false,
            failed: false,
            reason: TerminalReason::Completed,
            submitted: now,
            ready: None,
            proceed: None,
            first_result: None,
            terminal: now,
        };
        for id in 1..=TERMINAL_REPORT_CAPACITY as u64 + 2 {
            report.request_id = id;
            assert!(book.attach_terminal(report));
        }
        let snapshot = book.snapshot();
        assert_eq!(snapshot.preparations.len(), TERMINAL_REPORT_CAPACITY);
        assert_eq!(snapshot.preparations[0].request_id, 3);
        assert_eq!(snapshot.lost_preparations, 2);
        book.lost_preparations = u64::MAX;
        assert!(book.attach_terminal(report));
        book.note_lost_reports(u64::MAX);
        book.note_lost_reports(1);
        assert_eq!(book.snapshot().lost_preparations, u64::MAX);
        assert_eq!(book.snapshot().lost_reports, u64::MAX);
    }

    #[test]
    fn standalone_preparations_retain_terminal_causes_without_writing_traces() {
        let mut book = DiagnosticBook::new();
        let now = Instant::now();
        for (id, reason) in [
            TerminalReason::Completed,
            TerminalReason::Failed,
            TerminalReason::Released,
        ]
        .into_iter()
        .enumerate()
        {
            assert!(book.attach_terminal(TerminalReport {
                request_id: id as u64 + 1,
                preparing: true,
                metrics: None,
                cancelled: reason == TerminalReason::Released,
                failed: reason == TerminalReason::Failed,
                reason,
                submitted: now,
                ready: None,
                proceed: None,
                first_result: None,
                terminal: now + Duration::from_micros(17),
            }));
        }
        let snapshot = book.snapshot();
        assert!(snapshot.traces.is_empty());
        assert_eq!(snapshot.unmatched_reports, 0);
        assert_eq!(snapshot.preparations.len(), 3);
        assert_eq!(snapshot.preparations[0].reason, TerminalReason::Completed);
        assert_eq!(snapshot.preparations[1].reason, TerminalReason::Failed);
        assert_eq!(snapshot.preparations[2].reason, TerminalReason::Released);
        for report in snapshot.preparations {
            assert_eq!(report.terminal_us, Some(17));
            assert_eq!(report.proceed_us, None);
        }
    }

    #[test]
    fn proceed_stage_uses_terminal_dispatch_evidence_even_after_trace_finishes() {
        let mut book = DiagnosticBook::new();
        let now = Instant::now();
        for (id, proceed) in [None, Some(now + Duration::from_micros(30))]
            .into_iter()
            .enumerate()
        {
            let trace = book
                .begin(
                    WorkKind::Generated,
                    PresentationLane::Inline,
                    id as u64 + 1,
                    capture(now),
                )
                .unwrap();
            assert!(book.bind_request(trace, id as u64 + 1));
            assert!(book.finish(trace, Outcome::Cancelled, now + Duration::from_micros(40)));
            assert!(book.attach_terminal(TerminalReport {
                request_id: id as u64 + 1,
                preparing: false,
                metrics: None,
                cancelled: true,
                failed: false,
                reason: TerminalReason::Cancelled,
                submitted: now + Duration::from_micros(10),
                ready: Some(now + Duration::from_micros(20)),
                proceed,
                first_result: None,
                terminal: now + Duration::from_micros(50),
            }));
        }
        let snapshot = book.snapshot();
        assert_eq!(snapshot.traces[0].stage_us[Stage::Proceed as usize], None);
        assert_eq!(
            snapshot.traces[1].stage_us[Stage::Proceed as usize],
            Some(30)
        );
        assert_eq!(snapshot.traces[1].terminal.unwrap().proceed_us, Some(20));
    }

    #[test]
    fn input_wait_first_capture_and_total_work_are_distinct() {
        let mut book = DiagnosticBook::new();
        let now = Instant::now();
        let trace = book
            .begin(
                WorkKind::Generated,
                PresentationLane::Inline,
                1,
                CaptureTiming {
                    input_notified: Some(now),
                    started: now + Duration::from_micros(11),
                    finished: now + Duration::from_micros(18),
                },
            )
            .unwrap();
        assert!(book.record_capture(
            trace,
            now + Duration::from_micros(30),
            now + Duration::from_micros(33)
        ));
        assert!(book.publish(
            trace,
            PresentationLane::Inline,
            1,
            now + Duration::from_micros(40)
        ));
        let snapshot = book.snapshot();
        let trace = &snapshot.traces[0];
        assert_eq!(trace.input_wait_us, Some(11));
        assert_eq!(trace.first_capture_us, 7);
        assert_eq!(trace.total_capture_us, 10);
        assert_eq!(trace.stage_us[Stage::Capture as usize], Some(7));
        assert_eq!(trace.first_publication_us, Some(29));
    }

    #[test]
    fn unavailable_input_time_is_not_zero_or_inferred() {
        let mut book = DiagnosticBook::new();
        let now = Instant::now();
        book.begin(
            WorkKind::RetainedGhost,
            PresentationLane::Inline,
            1,
            capture(now),
        )
        .unwrap();
        book.begin(
            WorkKind::LocalTool,
            PresentationLane::Chooser,
            1,
            CaptureTiming {
                input_notified: Some(now),
                started: now,
                finished: now,
            },
        )
        .unwrap();
        let snapshot = book.snapshot();
        assert_eq!(snapshot.traces[0].input_wait_us, None);
        assert_eq!(snapshot.traces[1].input_wait_us, Some(0));
        assert_eq!(snapshot.traces[0].first_capture_us, 0);
        assert_eq!(snapshot.traces[0].outcome, None);
        let value = serde_json::to_value(snapshot).unwrap();
        assert!(value["traces"][0]["input_wait_us"].is_null());
        assert_eq!(value["traces"][1]["input_wait_us"], 0);
        assert_eq!(value["traces"][1]["kind"], "local_tool");
    }

    #[test]
    fn simultaneous_ghost_and_chooser_keep_independent_draw_attribution() {
        let mut book = DiagnosticBook::new();
        let now = Instant::now();
        let ghost = book
            .begin(
                WorkKind::Generated,
                PresentationLane::Inline,
                80,
                capture(now),
            )
            .unwrap();
        assert!(book.publish(ghost, PresentationLane::Inline, 80, now));
        let chooser = book
            .begin(
                WorkKind::Alternative,
                PresentationLane::Chooser,
                1,
                capture(now),
            )
            .unwrap();
        assert!(book.publish(chooser, PresentationLane::Chooser, 1, now));
        assert!(!book.record_draw(ghost, PresentationLane::Chooser, 80, now));
        assert!(!book.publish(chooser, PresentationLane::Inline, 81, now));
        assert!(book.record_draw(
            ghost,
            PresentationLane::Inline,
            80,
            now + Duration::from_micros(3)
        ));
        assert!(book.record_draw(
            chooser,
            PresentationLane::Chooser,
            1,
            now + Duration::from_micros(5)
        ));
        let snapshot = book.snapshot();
        assert_eq!(snapshot.traces[0].first_draw_us, Some(3));
        assert_eq!(snapshot.traces[1].first_draw_us, Some(5));
    }

    #[test]
    fn stale_draw_and_generic_stage_cannot_paint_new_offer() {
        let mut book = DiagnosticBook::new();
        let now = Instant::now();
        let old = book
            .begin(
                WorkKind::Generated,
                PresentationLane::Inline,
                1,
                capture(now),
            )
            .unwrap();
        assert!(book.publish(old, PresentationLane::Inline, 1, now));
        let new = book
            .begin(
                WorkKind::Regenerated,
                PresentationLane::Inline,
                2,
                capture(now),
            )
            .unwrap();
        assert!(book.publish(new, PresentationLane::Inline, 2, now));
        assert!(!book.record_draw(old, PresentationLane::Inline, 1, now));
        assert!(!book.record_draw(old, PresentationLane::Inline, 2, now));
        assert!(!book.record_stage(new, Stage::NativeDraw, now));
        assert!(!book.record_stage(new, Stage::ProviderPublication, now));
        assert!(!book.record_stage(new, Stage::Capture, now));
        assert!(book.record_draw(
            new,
            PresentationLane::Inline,
            2,
            now + Duration::from_micros(7)
        ));
        assert_eq!(book.snapshot().traces[1].stage_us[5], Some(7));
        assert_eq!(book.snapshot().rejected_draws, 2);
    }

    #[test]
    fn hidden_lanes_and_authority_revocation_reject_callbacks() {
        let mut book = DiagnosticBook::new();
        let now = Instant::now();
        let ghost = book
            .begin(
                WorkKind::Generated,
                PresentationLane::Inline,
                1,
                capture(now),
            )
            .unwrap();
        let chooser = book
            .begin(
                WorkKind::Alternative,
                PresentationLane::Chooser,
                1,
                capture(now),
            )
            .unwrap();
        assert!(!book.record_draw(ghost, PresentationLane::Inline, 1, now));
        assert!(book.publish(ghost, PresentationLane::Inline, 1, now));
        assert!(book.publish(chooser, PresentationLane::Chooser, 1, now));
        assert!(book.clear_view(PresentationLane::Chooser, 1));
        assert!(!book.record_draw(chooser, PresentationLane::Chooser, 1, now));
        assert!(!book.publish(chooser, PresentationLane::Chooser, 1, now));
        assert!(book.record_draw(ghost, PresentationLane::Inline, 1, now));
        assert!(book.publish(ghost, PresentationLane::Inline, 2, now));
        assert!(!book.clear_view(PresentationLane::Inline, 1));
        book.invalidate_all_views();
        assert!(!book.record_draw(ghost, PresentationLane::Inline, 2, now));
        assert!(!book.publish(ghost, PresentationLane::Inline, 2, now));
    }

    #[test]
    fn refresh_of_one_lane_preserves_the_other_and_first_observations() {
        let mut book = DiagnosticBook::new();
        let now = Instant::now();
        let ghost = book
            .begin(
                WorkKind::Generated,
                PresentationLane::Inline,
                1,
                capture(now),
            )
            .unwrap();
        let chooser = book
            .begin(
                WorkKind::Alternative,
                PresentationLane::Chooser,
                1,
                capture(now),
            )
            .unwrap();
        assert!(book.publish(
            ghost,
            PresentationLane::Inline,
            2,
            now + Duration::from_micros(4)
        ));
        assert!(book.publish(
            chooser,
            PresentationLane::Chooser,
            1,
            now + Duration::from_micros(4)
        ));
        assert!(book.record_draw(
            ghost,
            PresentationLane::Inline,
            2,
            now + Duration::from_micros(5)
        ));
        assert!(book.publish(
            ghost,
            PresentationLane::Inline,
            3,
            now + Duration::from_micros(8)
        ));
        assert!(!book.record_draw(
            ghost,
            PresentationLane::Inline,
            2,
            now + Duration::from_micros(9)
        ));
        assert!(book.record_draw(
            chooser,
            PresentationLane::Chooser,
            1,
            now + Duration::from_micros(9)
        ));
        let snapshot = book.snapshot();
        assert_eq!(snapshot.traces[0].first_publication_us, Some(4));
        assert_eq!(snapshot.traces[0].first_draw_us, Some(5));
        assert_eq!(snapshot.traces[0].stage_us[5], None);
        assert!(book.record_draw(
            ghost,
            PresentationLane::Inline,
            3,
            now + Duration::from_micros(10)
        ));
        assert_eq!(book.snapshot().traces[0].stage_us[5], Some(10));
    }

    #[test]
    fn retention_is_bounded_and_revocation_keeps_exact_unknown_report() {
        let mut book = DiagnosticBook::new();
        let now = Instant::now();
        let old = book
            .begin(
                WorkKind::Generated,
                PresentationLane::Inline,
                1,
                capture(now),
            )
            .unwrap();
        assert!(book.bind_request(old, 70));
        assert!(book.finish(old, Outcome::Cancelled, now));
        let retained = book
            .begin(
                WorkKind::RetainedGhost,
                PresentationLane::Inline,
                2,
                capture(now),
            )
            .unwrap();
        assert!(!book.bind_request(retained, 71));
        assert!(book.attach_terminal(TerminalReport {
            request_id: 70,
            preparing: false,
            metrics: None,
            cancelled: true,
            failed: false,
            reason: TerminalReason::Cancelled,
            submitted: now,
            ready: None,
            proceed: None,
            first_result: None,
            terminal: now,
        }));
        assert!(
            book.snapshot().traces[0]
                .terminal
                .unwrap()
                .metrics
                .is_none()
        );
        for view in 3..=130 {
            book.begin(
                WorkKind::Alternative,
                PresentationLane::Chooser,
                view,
                capture(now),
            )
            .unwrap();
        }
        let snapshot = book.snapshot();
        assert_eq!(snapshot.traces.len(), TRACE_CAPACITY);
        assert_eq!(snapshot.evicted_traces, 2);
        assert_eq!(snapshot.traces[0].trace_id, 3);
    }

    #[test]
    fn monotonic_stage_capture_and_lane_observations_are_enforced() {
        let mut book = DiagnosticBook::new();
        let now = Instant::now();
        assert!(
            book.begin(
                WorkKind::Generated,
                PresentationLane::Inline,
                1,
                CaptureTiming {
                    input_notified: Some(now + Duration::from_micros(1)),
                    started: now,
                    finished: now,
                }
            )
            .is_none()
        );
        assert!(
            book.begin(
                WorkKind::Generated,
                PresentationLane::Inline,
                1,
                CaptureTiming {
                    input_notified: None,
                    started: now + Duration::from_micros(1),
                    finished: now,
                }
            )
            .is_none()
        );
        let trace = book
            .begin(
                WorkKind::Generated,
                PresentationLane::Inline,
                4,
                capture(now),
            )
            .unwrap();
        assert!(book.record_capture(trace, now, now + Duration::from_micros(4)));
        assert!(!book.record_capture(
            trace,
            now + Duration::from_micros(3),
            now + Duration::from_micros(5)
        ));
        assert!(!book.finish(trace, Outcome::NoOffer, now));
        assert!(book.record_stage(trace, Stage::Ready, now + Duration::from_micros(9)));
        assert!(!book.record_stage(trace, Stage::Proceed, now));
        assert!(book.advance_view(PresentationLane::Inline, 5));
        assert!(!book.publish(
            trace,
            PresentationLane::Inline,
            4,
            now + Duration::from_micros(10)
        ));
        assert!(
            book.begin(
                WorkKind::Generated,
                PresentationLane::Inline,
                4,
                capture(now)
            )
            .is_none()
        );
        assert!(
            book.begin(
                WorkKind::LocalTool,
                PresentationLane::Chooser,
                1,
                capture(now)
            )
            .is_some()
        );
        assert_eq!(book.snapshot().traces[0].total_capture_us, 4);
    }
}
