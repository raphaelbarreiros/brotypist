//! Disposable native-smoke observations only. No writing, app identities, model
//! tokens, wall-clock timestamps or protocol payloads enter this recorder.
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

const CAPACITY: usize = 4096;
const PREPARATION_LEAF: &str = "native-worker-preparation.json";

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Stage {
    NativeCapture,
    CandidateToSchedule,
    NativeContextPreparation,
    ContextQueued,
    ContextPreparation,
    ContextConsumed,
    WorkerSubmitted,
    WorkerDispatch,
    WorkerSpawn,
    WorkerLoading,
    WorkerPrepared,
    WorkerReady,
    WorkerProceed,
    WorkerFirstNonemptyResult,
    RuntimeReady,
    RuntimeProceed,
    FirstUsableCompletion,
    PresentationAccepted,
    PresentationRejected,
    RuntimeCleared,
    OwnerWakeScheduled,
    OwnerWakeConsumed,
    AppKitWakeDelivered,
    ProviderPreparation,
    ProviderModelLoad,
    NativeModelOpen,
    NativeBackendInitialization,
    NativeModelLoad,
    NativeContextCreation,
    NativeBatchAllocation,
    ProviderFramingTokenization,
    ProviderInitialEvaluation,
}

#[derive(Clone, Copy, Serialize)]
struct Observation {
    sequence: u64,
    stage: Stage,
    // Local preparation/worker sequence numbers, never model token identifiers.
    operation: Option<u64>,
    start_us: u64,
    elapsed_us: u64,
}

struct Buffer {
    observations: VecDeque<Observation>,
    capacity: usize,
    total: u64,
    dropped: u64,
}
impl Buffer {
    fn new(capacity: usize) -> Self {
        Self {
            observations: VecDeque::with_capacity(capacity),
            capacity,
            total: 0,
            dropped: 0,
        }
    }
    fn push(&mut self, mut observation: Observation) {
        self.total = self.total.saturating_add(1);
        observation.sequence = self.total;
        if self.observations.len() == self.capacity {
            self.observations.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.observations.push_back(observation);
    }
}

struct Recorder {
    epoch: Instant,
    home: PathBuf,
    buffer: Mutex<Buffer>,
}

fn recorder() -> Option<&'static Recorder> {
    static RECORDER: LazyLock<Option<Recorder>> = LazyLock::new(|| {
        // Both are explicit opt-ins. Never fall back to the normal profile.
        if std::env::var("INKLOW_NATIVE_STAGE_TRACE").ok().as_deref() != Some("disposable-profile")
        {
            return None;
        }
        let home = PathBuf::from(std::env::var_os("INKLOW_HOME")?);
        if !home.is_absolute() {
            return None;
        }
        Some(Recorder {
            epoch: Instant::now(),
            home,
            buffer: Mutex::new(Buffer::new(CAPACITY)),
        })
    });
    RECORDER.as_ref()
}

fn micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn append(
    recorder: &Recorder,
    stage: Stage,
    operation: Option<u64>,
    start: Instant,
    elapsed: Duration,
) {
    recorder
        .buffer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(Observation {
            sequence: 0,
            stage,
            operation,
            start_us: micros(start.saturating_duration_since(recorder.epoch)),
            elapsed_us: micros(elapsed),
        });
}

pub(crate) fn mark(stage: Stage, operation: Option<u64>) {
    if let Some(recorder) = recorder() {
        append(recorder, stage, operation, Instant::now(), Duration::ZERO);
    }
}

/// Includes eligibility/spelling gates as well as debounce; not just a timer.
pub(crate) fn scheduled(since: Instant, operation: u64) {
    if let Some(recorder) = recorder() {
        let now = Instant::now();
        append(
            recorder,
            Stage::CandidateToSchedule,
            Some(operation),
            since,
            now.saturating_duration_since(since),
        );
    }
}

pub(crate) struct Span(Option<(&'static Recorder, Stage, Option<u64>, Instant)>);
impl Span {
    pub(crate) fn start(stage: Stage, operation: Option<u64>) -> Self {
        Self(recorder().map(|recorder| (recorder, stage, operation, Instant::now())))
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        if let Some((recorder, stage, operation, start)) = self.0 {
            append(recorder, stage, operation, start, start.elapsed());
        }
    }
}

thread_local! {
    // Only the worker's provider-owning thread enters this scope.
    static PREPARATION_OPERATION: Cell<Option<u64>> = const { Cell::new(None) };
    static NATIVE_OPEN_SPAN: RefCell<Option<Span>> = const { RefCell::new(None) };
}

fn preparation_opted_in() -> bool {
    static ENABLED: LazyLock<bool> = LazyLock::new(|| {
        std::env::var("INKLOW_NATIVE_PREPARE_TRACE").ok().as_deref() == Some("disposable-profile")
            && recorder().is_some()
    });
    *ENABLED
}

pub(crate) fn preparation_active() -> bool {
    PREPARATION_OPERATION.with(|operation| operation.get().is_some())
}

pub(crate) fn preparation_span(stage: Stage) -> Span {
    PREPARATION_OPERATION.with(|operation| match operation.get() {
        Some(operation) => Span::start(stage, Some(operation)),
        None => Span(None),
    })
}

/// Synchronous private bridge callback; never installed outside opted-in worker
/// preparation. Normal builds omit both this callback and the C call sites.
pub(crate) extern "C" fn native_open_phase(phase: i32, entering: i32) {
    let stage = match phase {
        0 => Stage::NativeBackendInitialization,
        1 => Stage::NativeModelLoad,
        2 => Stage::NativeContextCreation,
        3 => Stage::NativeBatchAllocation,
        _ => return,
    };
    NATIVE_OPEN_SPAN.with(|span| {
        let previous = span.replace((entering != 0).then(|| preparation_span(stage)));
        drop(previous);
    });
}

/// Separate opt-in from the parent's stage trace: this scope is entered only
/// around actual worker preparation, never around streamed generation.
pub(crate) struct PreparationTrace {
    span: Span,
    previous: Option<u64>,
}
impl PreparationTrace {
    pub(crate) fn start(operation: u64) -> Self {
        let previous = PREPARATION_OPERATION.with(|active| {
            let previous = active.get();
            if preparation_opted_in() {
                active.set(Some(operation));
            }
            previous
        });
        Self {
            span: preparation_span(Stage::ProviderPreparation),
            previous,
        }
    }

    pub(crate) fn finish(self, succeeded: bool) {
        let active = self.span.0.is_some();
        // Close every measurement before snapshot allocation and disk I/O.
        drop(self);
        if active && export_preparation(succeeded).is_err() {
            // Never put errors/paths/content into the artifact or worker wire.
            eprintln!("BroTypist: worker preparation diagnostic export refused or failed.");
        }
    }
}
impl Drop for PreparationTrace {
    fn drop(&mut self) {
        PREPARATION_OPERATION.with(|active| active.set(self.previous));
    }
}

// Resolve even an absent normal profile through its existing ancestors. Export
// must not create or inspect files inside the normal profile to compare roots.
fn resolved_directory(path: &Path) -> Result<PathBuf> {
    let mut ancestor = path;
    let mut suffix = Vec::new();
    loop {
        match fs::canonicalize(ancestor) {
            Ok(mut resolved) => {
                for name in suffix.iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                suffix.push(
                    ancestor
                        .file_name()
                        .context("Invalid diagnostic profile root")?,
                );
                ancestor = ancestor
                    .parent()
                    .context("Invalid diagnostic profile root")?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn require_private_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Diagnostic export requires a real directory"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o077 == 0,
            "Diagnostic export requires a private owned directory"
        );
    }
    Ok(())
}

fn export_directory(home: &Path, normal: &Path) -> Result<PathBuf> {
    require_private_directory(home)?;
    let home = fs::canonicalize(home)?;
    let normal = resolved_directory(normal)?;
    ensure!(
        !home.starts_with(&normal) && !normal.starts_with(&home),
        "Diagnostic export refuses normal-profile overlap"
    );
    let directory = home.join("diagnostics");
    match fs::symlink_metadata(&directory) {
        Ok(_) => require_private_directory(&directory)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::paths::private_directory(&directory)?;
            require_private_directory(&directory)?;
        }
        Err(error) => return Err(error.into()),
    }
    ensure!(
        fs::canonicalize(&directory)? == directory,
        "Diagnostic export refuses redirected output"
    );
    Ok(directory)
}

/// Called only by the GUI owner on BroTypistExportNativeStages.
/// Snapshot allocation and disk I/O happen here, never on prediction paths.
pub(crate) fn export() -> Result<()> {
    let Some(recorder) = recorder() else {
        return Ok(());
    };
    let directory = export_directory(&recorder.home, &crate::paths::platform_config_dir()?)?;
    let value = {
        let buffer = recorder
            .buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        serde_json::json!({
            "schema": "brotypist-native-stages/1",
            "version": env!("CARGO_PKG_VERSION"),
            "capacity": CAPACITY,
            "total": buffer.total,
            "dropped": buffer.dropped,
            "observations": buffer.observations,
            "clock": "process_monotonic_microseconds",
            "presentation_boundary": "completion_runtime_validated_gui_acknowledgement_not_paint",
            "worker_boundary": "parent_supervisor_protocol_observation_not_child_compute",
            "operation_namespaces": "context_*/native_context_preparation/candidate_to_schedule=preparation; worker_*/runtime_ready/runtime_proceed/first_usable_completion=worker; absent=null; ids_are_local_to_each_owner",
            "span_semantics": "start_us_plus_elapsed_us; nested_spans_overlap; sequence_orders_recording_not_start",
        })
    };
    write_export(&directory, "native-stages.json", &value)
}

fn export_preparation(succeeded: bool) -> Result<()> {
    if !preparation_opted_in() {
        return Ok(());
    }
    let Some(recorder) = recorder() else {
        return Ok(());
    };
    let directory = export_directory(&recorder.home, &crate::paths::platform_config_dir()?)?;
    let value = {
        let buffer = recorder
            .buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        serde_json::json!({
            "schema": "brotypist-native-worker-preparation/1",
            "version": env!("CARGO_PKG_VERSION"),
            "capacity": CAPACITY,
            "total": buffer.total,
            "dropped": buffer.dropped,
            "observations": buffer.observations,
            "last_preparation_succeeded": succeeded,
            "clock": "child_process_monotonic_microseconds",
            "operation_namespace": "worker_request_id",
            "span_semantics": "start_us_plus_elapsed_us; nested_spans_overlap; sequence_orders_recording_not_start; failure_retains_attempted_spans",
            "native_open_boundary": "aggregate_native_model_open_contains_separate_backend_initialization_model_load_context_creation_batch_allocation",
            "evaluation_boundary": "initial_evaluate_including_backend_synchronize; not_generation",
            "export_boundary": "after_preparation_before_prepared_or_error_event; excluded_from_spans",
        })
    };
    write_export(&directory, PREPARATION_LEAF, &value)
}

fn write_export(directory: &Path, leaf: &str, value: &serde_json::Value) -> Result<()> {
    let destination = directory.join(leaf);
    match fs::symlink_metadata(&destination) {
        Ok(metadata) => ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "Diagnostic export refuses redirected output"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut temporary = tempfile::Builder::new()
        .prefix(".inklow-")
        .tempfile_in(directory)?;
    serde_json::to_writer_pretty(&mut temporary, value)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(destination)
        .map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_export_keeps_parent_artifact_and_private_permissions() {
        let root = tempfile::tempdir().unwrap();
        let isolated = root.path().join("isolated");
        crate::paths::private_directory(&isolated).unwrap();
        let directory = export_directory(&isolated, &root.path().join("normal")).unwrap();
        let parent = serde_json::json!({"schema": "parent"});
        let child = serde_json::json!({"schema": "child"});
        write_export(&directory, "native-stages.json", &parent).unwrap();
        write_export(&directory, PREPARATION_LEAF, &child).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &fs::read(directory.join("native-stages.json")).unwrap()
            )
            .unwrap(),
            parent
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &fs::read(directory.join(PREPARATION_LEAF)).unwrap()
            )
            .unwrap(),
            child
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                fs::metadata(directory.join(PREPARATION_LEAF))
                    .unwrap()
                    .mode()
                    & 0o077,
                0
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn child_export_refuses_leaf_symlinks_without_touching_target() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let isolated = root.path().join("isolated");
        let normal = root.path().join("normal");
        crate::paths::private_directory(&isolated).unwrap();
        crate::paths::private_directory(&normal).unwrap();
        let directory = export_directory(&isolated, &normal).unwrap();
        let target = normal.join("untouched.json");
        fs::write(&target, b"untouched").unwrap();
        symlink(&target, directory.join(PREPARATION_LEAF)).unwrap();
        assert!(write_export(&directory, PREPARATION_LEAF, &serde_json::json!({})).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"untouched");
        assert!(
            fs::symlink_metadata(directory.join(PREPARATION_LEAF))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn overflow_preserves_newest_observations_and_reports_loss() {
        let mut buffer = Buffer::new(2);
        for operation in 1..=4 {
            buffer.push(Observation {
                sequence: 0,
                stage: Stage::ContextQueued,
                operation: Some(operation),
                start_us: operation,
                elapsed_us: 0,
            });
        }
        assert_eq!(buffer.total, 4);
        assert_eq!(buffer.dropped, 2);
        let retained: Vec<_> = buffer
            .observations
            .iter()
            .map(|row| (row.sequence, row.operation))
            .collect();
        assert_eq!(retained, [(3, Some(3)), (4, Some(4))]);
    }

    #[test]
    fn export_refuses_normal_profile_and_ancestor_overlap_before_writing() {
        let root = tempfile::tempdir().unwrap();
        let normal = root.path().join("normal");
        crate::paths::private_directory(&normal).unwrap();
        let nested = normal.join("nested");
        crate::paths::private_directory(&nested).unwrap();
        for home in [normal.as_path(), nested.as_path(), root.path()] {
            assert!(export_directory(home, &normal).is_err());
            assert!(!home.join("diagnostics").exists());
        }
        let isolated = root.path().join("isolated");
        crate::paths::private_directory(&isolated).unwrap();
        let output = export_directory(&isolated, &root.path().join("absent-normal")).unwrap();
        assert_eq!(
            output,
            fs::canonicalize(&isolated).unwrap().join("diagnostics")
        );
    }

    #[cfg(unix)]
    #[test]
    fn export_refuses_profile_aliases_and_redirected_diagnostics() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let normal = root.path().join("normal");
        let isolated = root.path().join("isolated");
        crate::paths::private_directory(&normal).unwrap();
        crate::paths::private_directory(&isolated).unwrap();
        let alias = root.path().join("alias");
        symlink(&normal, &alias).unwrap();
        assert!(export_directory(&alias, &normal).is_err());
        symlink(&normal, isolated.join("diagnostics")).unwrap();
        assert!(export_directory(&isolated, &normal).is_err());
        assert!(!normal.join("native-stages.json").exists());
    }
}
