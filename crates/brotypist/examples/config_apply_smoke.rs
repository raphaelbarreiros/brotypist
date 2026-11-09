//! Explicit headless configuration acceptance; native-smoke only, never a product entry.
//! Uses one real WorkerClient and an explicitly unavailable native factory.
//! Preparation diagnostics below are real worker terminal reports; no writing
//! capture, presentation, physical input, or native paint is inferred from them.
//! Battery coverage uses canonical `same` semantics: policy edits do not create
//! a second target; a prompt-style change still cold-prepares the supplied path.
//! Distinct battery targets require pinned catalogue bytes in private model
//! storage. Only their invalid-fixture rejection is exercised here: no weights
//! are downloaded, copied, linked, or relabelled to manufacture a successful target.
use anyhow::{Context, Result, bail, ensure};
use brotypist::{
    config::{self, Config},
    diagnostics::{TerminalReason, TerminalReport},
    models,
    platform::{ContextRead, DetachedContextRead},
    process_client::{ReleaseTicket, WorkerClient, WorkerEvent},
    providers::GenerationOptions,
    runtime::{
        AsyncPreparer, BackgroundReaders, ConfigApplyReceipt, ConfigApplyStage, Controller, Engine,
        MonotonicClock, PendingConfigApply, Readers, SharedStorage, Worker,
    },
    storage::HistoryStore,
    system::PowerStatus,
    ui::settings::SettingsDraft,
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::Write,
    num::NonZeroU64,
    path::{Path, PathBuf},
    process::ExitCode,
    rc::Rc,
    sync::{Arc, Mutex, atomic::AtomicU64},
    thread,
    time::{Duration, Instant},
};

#[derive(Default)]
struct PreparationEvidence {
    prepared: bool,
    done: bool,
    pid: Option<u64>,
    target: Option<Config>,
}
#[derive(Default)]
struct Evidence {
    releases: usize,
    acknowledged_admissions: usize,
    validations: BTreeMap<u64, PreparationEvidence>,
}
struct ObservedWorker {
    inner: WorkerClient,
    evidence: Rc<RefCell<Evidence>>,
}
impl Worker for ObservedWorker {
    fn prepare(&mut self, c: &Config) -> Result<u64> {
        self.inner.prepare(c)
    }
    fn submit(&mut self, c: &Config, p: &str, s: &str, o: &GenerationOptions) -> Result<u64> {
        self.inner.submit(c, p, s, o)
    }
    fn proceed(&mut self, id: u64) -> Result<()> {
        self.inner.proceed(id)
    }
    fn poll(&mut self) -> Vec<WorkerEvent> {
        let events = self.inner.poll();
        let pid = self.inner.status()["worker_pid"].as_u64();
        let mut evidence = self.evidence.borrow_mut();
        for event in &events {
            if let Some(observed) = evidence.validations.get_mut(&event.id) {
                match event.kind.as_str() {
                    "prepared" => {
                        observed.prepared = true;
                        observed.pid = pid;
                    }
                    "done" => observed.done = true,
                    _ => {}
                }
            }
        }
        events
    }
    fn cancel(&mut self) -> Result<()> {
        self.inner.cancel()
    }
    fn release(&mut self) -> Result<ReleaseTicket> {
        let ticket = self.inner.release()?;
        self.evidence.borrow_mut().releases += 1;
        Ok(ticket)
    }
    fn release_complete(&self, ticket: ReleaseTicket) -> bool {
        self.inner.release_complete(ticket)
    }
    fn prepare_for_validation(&mut self, c: &Config, ticket: ReleaseTicket) -> Result<NonZeroU64> {
        ensure!(
            self.inner.release_complete(ticket),
            "Validation lacked acknowledged release"
        );
        let id = self.inner.prepare_for_validation(c, ticket)?;
        let mut evidence = self.evidence.borrow_mut();
        evidence.acknowledged_admissions += 1;
        ensure!(
            evidence
                .validations
                .insert(
                    id.get(),
                    PreparationEvidence {
                        target: Some(c.clone()),
                        ..PreparationEvidence::default()
                    },
                )
                .is_none(),
            "Validation request identity was reused"
        );
        Ok(id)
    }
    fn set_wakeup(&mut self, callback: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.inner.set_wakeup(callback);
    }
    fn take_terminal_report(&mut self) -> Option<TerminalReport> {
        self.inner.take_terminal_report()
    }
    fn lost_terminal_reports(&self) -> u64 {
        self.inner.lost_terminal_reports()
    }
    fn set_idle_timeout(&mut self, seconds: u32) {
        self.inner.set_idle_timeout(seconds);
    }
    fn status(&self) -> Value {
        self.inner.status()
    }
    fn request_stop(&mut self) -> Result<()> {
        self.inner.request_stop()
    }
    fn is_stopped(&mut self) -> bool {
        self.inner.is_stopped()
    }
}
struct PrivateReaders {
    destination: Rc<RefCell<PathBuf>>,
}
impl Readers for PrivateReaders {
    fn power(&mut self) -> PowerStatus {
        PowerStatus::default()
    }
    fn persist(&mut self, config: &Config) -> Result<()> {
        config::save_config_to(config, &self.destination.borrow())
    }
}
struct NoExternalReaders;
impl BackgroundReaders for NoExternalReaders {
    fn clipboard(
        &self,
        _: Arc<dyn Fn() -> bool + Send + Sync>,
        _: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<String> {
        bail!("Headless smoke forbids clipboard reads")
    }
    fn screen(
        &self,
        _: &brotypist::core::Snapshot,
        _: &dyn DetachedContextRead,
        _: &dyn Fn() -> bool,
        _: usize,
        _: bool,
        _: bool,
    ) -> Result<ContextRead> {
        bail!("Headless smoke forbids screen reads")
    }
}
fn receive(controller: &Controller, pending: &PendingConfigApply) -> Result<ConfigApplyReceipt> {
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        // Events are deliberately not serialized: ordinary status/error events may contain paths.
        let _ = controller.poll();
        if let Some(receipt) = pending.try_recv()? {
            ensure!(
                pending.try_recv().is_err(),
                "More than one terminal receipt"
            );
            return Ok(receipt);
        }
        ensure!(
            Instant::now() < deadline,
            "Configuration receipt deadline exceeded"
        );
        thread::sleep(Duration::from_millis(10));
    }
}
fn shutdown(controller: &mut Controller) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !controller.shutdown()? {
        ensure!(
            Instant::now() < deadline,
            "Owned shutdown deadline exceeded"
        );
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
fn saved(receipt: ConfigApplyReceipt) -> Result<Config> {
    match receipt {
        ConfigApplyReceipt::Saved { config } => Ok(config),
        _ => bail!("Expected Saved receipt"),
    }
}
fn persisted(path: &Path) -> Result<Config> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn preparation_report(controller: &Controller, id: u64, reason: TerminalReason) -> Result<Value> {
    // A terminal event may have been consumed after this pump's diagnostic drain.
    let _ = controller.poll();
    let snapshot = controller.diagnostics();
    ensure!(
        snapshot.traces.is_empty()
            && snapshot.lost_reports == 0
            && snapshot.lost_preparations == 0
            && snapshot.unmatched_reports == 0,
        "Headless preparation invented a writing trace or lost terminal evidence"
    );
    let report = snapshot
        .preparations
        .iter()
        .find(|report| report.request_id == id)
        .context("Matching real validation terminal report was not retained")?;
    ensure!(
        report.preparing
            && report.reason == reason
            && report.terminal_us.is_some()
            && report.ready_us.is_none()
            && report.proceed_us.is_none()
            && report.first_result_us.is_none(),
        "Preparation terminal report contained writing stages or the wrong cause"
    );
    ensure!(
        report.failed == (reason == TerminalReason::Failed),
        "Preparation failure flag did not match its terminal cause"
    );
    Ok(serde_json::to_value(report)?)
}
fn promotion(
    controller: &Controller,
    evidence: &Rc<RefCell<Evidence>>,
    before: usize,
) -> Result<Value> {
    let evidence = evidence.borrow();
    ensure!(
        evidence.validations.len() == before + 1,
        "Expected exactly one cold validation"
    );
    let (&id, proof) = evidence
        .validations
        .last_key_value()
        .context("Missing validation")?;
    ensure!(
        proof.prepared && proof.done,
        "Missing matching prepared and done"
    );
    let target = proof.target.as_ref().context("Missing admitted target")?;
    let committed = controller.config();
    ensure!(
        target.provider_identity() == committed.provider_identity()
            && target.prompt_style == committed.prompt_style
            && target.model_id == committed.model_id
            && target.battery_model == committed.battery_model,
        "Prepared target does not match the committed configuration"
    );
    let status = controller.status();
    ensure!(
        proof.pid.is_some() && proof.pid == status["worker_pid"].as_u64(),
        "Prepared worker was not promoted"
    );
    ensure!(
        status["model_loaded"] == true,
        "Prepared model owner was not retained"
    );
    drop(evidence);
    Ok(
        json!({"matching_prepared_done":true,"acknowledged_release":true,
        "prepared_child_retained":true,"cold_validations":1,
        "preparation_terminal":preparation_report(controller, id, TerminalReason::Completed)?}),
    )
}
fn private_home() -> Result<PathBuf> {
    ensure!(
        std::env::var("BROTYPIST_CONFIG_APPLY_SMOKE").as_deref() == Ok("1"),
        "Explicit smoke opt-in required"
    );
    let home =
        PathBuf::from(std::env::var_os("INKLOW_HOME").context("Private INKLOW_HOME required")?);
    ensure!(home.is_absolute(), "Private profile must be absolute");
    let meta = fs::symlink_metadata(&home)?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "Private profile must be a real directory"
    );
    // A newly empty direct child of the OS temporary directory cannot be an existing app profile.
    ensure!(
        home.parent()
            .context("Missing profile parent")?
            .canonicalize()?
            == std::env::temp_dir().canonicalize()?,
        "Private profile must be a direct child of the OS temporary directory"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
            "Private profile ownership or permissions refused"
        );
    }
    #[cfg(not(unix))]
    bail!(
        "This harness requires the Unix private-directory ownership check; Windows ACL acceptance is unimplemented"
    );
    ensure!(
        fs::read_dir(&home)?.next().is_none(),
        "Private profile must be unused and empty"
    );
    Ok(home)
}
fn exercise(
    controller: &mut Controller,
    model: &Path,
    home: &Path,
    settings: &Path,
    destination: &Rc<RefCell<PathBuf>>,
    evidence: &Rc<RefCell<Evidence>>,
    cases: &mut Vec<Value>,
) -> Result<()> {
    let model = model.to_str().context("Non-Unicode model path")?.to_owned();
    let initial = controller.config();
    let before = evidence.borrow().validations.len();
    let selection =
        controller.select_model(model.clone(), "writing".into(), Some(String::new()))?;
    ensure!(
        selection.try_recv()?.is_none(),
        "Selection did not enter pending transaction"
    );
    let mut staged = SettingsDraft::new(&initial)?;
    staged.set(
        "custom_instructions",
        json!("  Teste privado e\u{301}\nexact bytes  "),
    )?;
    let mut external = SettingsDraft::new(&initial)?;
    external.set("writing_language", json!("pt"))?;
    let external = controller.apply_settings(external, true)?;
    ensure!(
        external.operation_id() != selection.operation_id(),
        "Operation identity collision"
    );
    let refreshed = saved(receive(controller, &external)?)?;
    staged.refresh(&refreshed)?;
    let selected = saved(receive(controller, &selection)?)?;
    ensure!(
        selected.writing_language == "pt" && selected.model_path == model,
        "Model transaction lost unrelated external change"
    );
    ensure!(
        persisted(settings)? == selected,
        "Saved model differs from persistence"
    );
    cases.push(
        json!({"case":"cold-selection-concurrent-save", "receipt":"saved",
        "proof":promotion(controller, evidence, before)?}),
    );
    let merged = controller.apply_settings(staged, true)?;
    let merged = saved(receive(controller, &merged)?)?;
    ensure!(
        merged.writing_language == "pt"
            && merged.custom_instructions == "  Teste privado e\u{301}\nexact bytes  ",
        "Draft refresh lost exact edits or concurrent value"
    );
    ensure!(
        persisted(settings)? == merged,
        "Merged configuration was not persisted"
    );
    cases.push(
        json!({"case":"draft-refresh-and-merge","receipt":"saved","exact_edits_preserved":true}),
    );

    let disk_before = fs::read(settings)?;
    let mut edit = SettingsDraft::new(&controller.config())?;
    edit.set("writing_language", json!("en"))?;
    let pending = controller.apply_settings(edit, false)?;
    let ConfigApplyReceipt::Applied { config } = receive(controller, &pending)? else {
        bail!("Expected Applied receipt")
    };
    ensure!(
        config.writing_language == "en"
            && controller.config() == config
            && fs::read(settings)? == disk_before,
        "Applied changed persistence or lost current value"
    );
    cases.push(
        json!({"case":"session-only-apply","receipt":"applied","persistence_unchanged":true}),
    );
    let pending = controller.apply_settings(SettingsDraft::new(&controller.config())?, true)?;
    saved(receive(controller, &pending)?)?;

    // `same` is the only supported arbitrary-path battery selection. These
    // power policies affect use, not preparation identity or target count.
    let before = evidence.borrow().validations.len();
    let releases = evidence.borrow().releases;
    let mut battery = SettingsDraft::new(&controller.config())?;
    battery.set("battery_model", json!("same"))?;
    battery.set("battery_on_demand", json!(true))?;
    battery.set("battery_shorter", json!(true))?;
    let pending = controller.apply_settings(battery, true)?;
    let battery = saved(receive(controller, &pending)?)?;
    ensure!(
        battery.battery_model == "same"
            && battery.battery_on_demand
            && battery.battery_shorter
            && battery.model_path == model
            && persisted(settings)? == battery
            && evidence.borrow().validations.len() == before
            && evidence.borrow().releases > releases,
        "Same-model battery policy edit revalidated or lost settings/disabled-owner release"
    );
    cases.push(
        json!({"case":"same-model-battery-policies","receipt":"saved",
        "cold_validations":0,"disabled_owner_release_requested":true,"battery_target":"same",
        "physical_power_transition_exercised":false}),
    );
    let disk_before = fs::read(settings)?;
    let before = evidence.borrow().validations.len();
    let mut direct = SettingsDraft::new(&controller.config())?;
    direct.set("prompt_style", json!("continuation"))?;
    let pending = controller.apply_settings(direct, false)?;
    let ConfigApplyReceipt::Applied { config } = receive(controller, &pending)? else {
        bail!("Expected model Applied receipt")
    };
    ensure!(
        config.prompt_style == "continuation"
            && controller.config() == config
            && fs::read(settings)? == disk_before,
        "Session model application changed persistence"
    );
    cases.push(
        json!({"case":"cold-session-model-apply","receipt":"applied",
        "battery_target":"same","same_supplied_model_path":true,
        "meaningful_configuration_difference":"prompt_style","validation_targets":1,
        "persistence_unchanged":true,"proof":promotion(controller, evidence, before)?}),
    );
    let before = evidence.borrow().validations.len();
    let pending = controller.select_model(model.clone(), "writing".into(), Some(String::new()))?;
    let restored = saved(receive(controller, &pending)?)?;
    ensure!(
        restored.prompt_style == "writing" && persisted(settings)? == restored,
        "Guided model restoration was not persisted"
    );
    cases.push(json!({"case":"guided-model-restore","receipt":"saved",
        "proof":promotion(controller, evidence, before)?}));
    let before = evidence.borrow().validations.len();
    let releases = evidence.borrow().releases;
    let mut battery = SettingsDraft::new(&controller.config())?;
    battery.set("battery_on_demand", json!(false))?;
    battery.set("battery_shorter", json!(false))?;
    let pending = controller.apply_settings(battery, true)?;
    let restored = saved(receive(controller, &pending)?)?;
    ensure!(
        !restored.battery_on_demand
            && !restored.battery_shorter
            && persisted(settings)? == restored
            && evidence.borrow().validations.len() == before
            && evidence.borrow().releases > releases,
        "Battery policy restoration lost settings or disabled-owner release"
    );

    // The real battery selector accepts catalogue IDs, not custom paths.
    // A deliberately invalid *private* catalogue file must fail integrity
    // before any worker admission, and must not commit either target.
    let current = controller.config();
    let disk_before = fs::read(settings)?;
    let before = evidence.borrow().validations.len();
    let preset = models::get_preset(models::DEFAULT_PRESET)?;
    let fixture = models::model_path(preset)?;
    ensure!(
        fixture.parent() == Some(home.join("models").as_path()),
        "Battery fixture escaped private model storage"
    );
    fs::create_dir_all(
        fixture
            .parent()
            .context("Missing battery fixture directory")?,
    )?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&fixture)?;
    file.write_all(b"deliberately invalid private battery GGUF fixture")?;
    drop(file);
    let mut battery = SettingsDraft::new(&current)?;
    battery.set("battery_model", json!(preset.id))?;
    let pending = controller.apply_settings(battery, true)?;
    ensure!(
        matches!(
            receive(controller, &pending)?,
            ConfigApplyReceipt::Failed {
                stage: ConfigApplyStage::Integrity,
                ..
            }
        ),
        "Invalid catalogue battery target did not fail integrity"
    );
    ensure!(
        controller.config() == current
            && fs::read(settings)? == disk_before
            && evidence.borrow().validations.len() == before,
        "Invalid battery target committed settings or admitted preparation"
    );
    fs::remove_file(&fixture)?;
    cases.push(json!({"case":"invalid-private-battery-target","receipt":"failed",
        "stage":"integrity","current_and_saved_preserved":true,
        "cold_validations":0,"successful_distinct_battery_target_exercised":false,
        "distinct_target_blocker":"requires pinned catalogue bytes in private storage; supplied arbitrary-path GGUF cannot be relabelled"}));
    // Saving loopback settings is local configuration validation only. These
    // deliberately unavailable destinations must not be contacted or prepared.
    for backend in ["ollama", "openai"] {
        for (endpoint, name) in [
            ("http://127.0.0.1:9", "unavailable-first"),
            ("http://127.0.0.1:10", "unavailable-second"),
        ] {
            let before = evidence.borrow().validations.len();
            let releases = evidence.borrow().releases;
            let mut draft = SettingsDraft::new(&controller.config())?;
            draft.set("backend", json!(backend))?;
            draft.set("endpoint", json!(endpoint))?;
            draft.set("model", json!(name))?;
            let pending = controller.apply_settings(draft, true)?;
            let config = saved(receive(controller, &pending)?)?;
            ensure!(
                config.backend == backend
                    && config.endpoint == endpoint
                    && config.model == name
                    && persisted(settings)? == config,
                "Loopback configuration was not saved exactly"
            );
            ensure!(
                evidence.borrow().validations.len() == before
                    && evidence.borrow().releases > releases
                    && controller.status()["worker_pid"].is_null(),
                "Loopback settings created a model worker or skipped owner retirement"
            );
        }
        cases.push(
            json!({"case":"loopback-configuration-save","backend":backend,
            "receipt":"saved","endpoint_and_model_edits_saved":true,
            "availability_checked":false,"cold_validations":0}),
        );
    }

    let before = evidence.borrow().validations.len();
    let pending = controller.select_model(model.clone(), "writing".into(), Some(String::new()))?;
    saved(receive(controller, &pending)?)?;
    cases.push(
        json!({"case":"gguf-restored-after-loopback-save","receipt":"saved",
        "proof":promotion(controller, evidence, before)?}),
    );

    let current = controller.config();
    let disk_before = fs::read(settings)?;
    let bad = home.join("invalid-candidate.gguf");
    fs::write(&bad, b"deliberately invalid owned GGUF fixture")?;
    let pending = controller.select_model(
        bad.to_str().context("Non-Unicode fixture")?.into(),
        "writing".into(),
        Some(String::new()),
    )?;
    ensure!(
        matches!(
            receive(controller, &pending)?,
            ConfigApplyReceipt::Failed {
                stage: ConfigApplyStage::Preparation,
                ..
            }
        ),
        "Invalid candidate did not fail preparation"
    );
    ensure!(
        controller.config() == current && fs::read(settings)? == disk_before,
        "Invalid candidate changed current or saved model"
    );
    let failed_id = *evidence
        .borrow()
        .validations
        .last_key_value()
        .context("Missing failed validation")?
        .0;
    cases.push(json!({"case":"invalid-candidate","receipt":"failed","stage":"preparation","current_and_saved_preserved":true,
        "preparation_terminal":preparation_report(controller, failed_id, TerminalReason::Failed)?}));

    let blocked = home.join("directory-not-a-settings-file");
    fs::create_dir(&blocked)?;
    *destination.borrow_mut() = blocked;
    let outcome = (|| -> Result<()> {
        let pending = controller.apply_settings(SettingsDraft::new(&current)?, true)?;
        ensure!(
            matches!(
                receive(controller, &pending)?,
                ConfigApplyReceipt::Failed {
                    stage: ConfigApplyStage::Persistence,
                    ..
                }
            ),
            "Unchanged-value Save did not report actual persistence failure"
        );
        ensure!(
            controller.config() == current && fs::read(settings)? == disk_before,
            "Failed unchanged-value save mutated state"
        );
        Ok(())
    })();
    *destination.borrow_mut() = settings.to_path_buf();
    outcome?;
    cases.push(json!({"case":"unchanged-save-directory-destination","receipt":"failed","stage":"persistence","current_and_saved_preserved":true}));
    let pending = controller.apply_settings(SettingsDraft::new(&current)?, true)?;
    saved(receive(controller, &pending)?)?;
    cases.push(json!({"case":"persistence-recovery","receipt":"saved"}));

    for replacement in [json!("writing"), json!(false)] {
        let before = evidence.borrow().validations.len();
        let mut first = SettingsDraft::new(&controller.config())?;
        first.set("prompt_style", json!("continuation"))?;
        let first = controller.apply_settings(first, true)?;
        let mut latest = SettingsDraft::new(&controller.config())?;
        latest.set("prompt_style", replacement.clone())?;
        let latest = controller.apply_settings(latest, true)?;
        ensure!(
            matches!(first.try_recv()?, Some(ConfigApplyReceipt::Superseded))
                && first.try_recv().is_err(),
            "Ordinary explicit model intent did not supersede the older operation once"
        );
        let receipt = receive(controller, &latest)?;
        if replacement.is_string() {
            saved(receipt)?;
        } else {
            ensure!(
                matches!(
                    receipt,
                    ConfigApplyReceipt::Failed {
                        stage: ConfigApplyStage::Validation,
                        ..
                    }
                ),
                "Invalid ordinary model intent was not rejected"
            );
        }
        ensure!(
            controller.config() == current
                && persisted(settings)? == current
                && evidence.borrow().validations.len() == before,
            "Newer ordinary model intent changed committed state or admitted stale validation"
        );
    }
    cases.push(
        json!({"case":"pre-pump-ordinary-model-return-and-invalid-intent",
        "older_receipt":"superseded","latest_receipts":["saved","validation-failed"],
        "committed_state_preserved":true,"inflight_gpu_race_exercised":false}),
    );

    // No pump between submission and cancellation/supersession: these assertions
    // prove operation-bound pre-admission ordering, not an in-flight GPU race.
    let first = controller.select_model(model.clone(), "writing".into(), Some(String::new()))?;
    let second = controller.select_model(model.clone(), "writing".into(), Some(String::new()))?;
    ensure!(
        first.operation_id() != second.operation_id(),
        "Operation identity collision"
    );
    ensure!(
        matches!(first.try_recv()?, Some(ConfigApplyReceipt::Superseded)),
        "Missing supersession receipt"
    );
    ensure!(first.try_recv().is_err(), "Duplicate supersession receipt");
    controller.cancel_config_apply(first.operation_id())?;
    ensure!(
        second.try_recv()?.is_none(),
        "Stale identity cancelled replacement"
    );
    controller.cancel_config_apply(second.operation_id())?;
    ensure!(
        matches!(second.try_recv()?, Some(ConfigApplyReceipt::Cancelled)),
        "Missing cancellation receipt"
    );
    ensure!(second.try_recv().is_err(), "Duplicate cancellation receipt");
    cases.push(json!({"case":"pre-pump-supersession-and-identity-cancel","receipts":["superseded","cancelled"],"stale_cancel_ignored":true,"inflight_gpu_race_exercised":false}));

    // Admission is an observable ownership boundary: the pump that admits
    // preparation returns before consuming its events. No elapsed-time guess
    // is used to claim the child is loading, running on a GPU, or still busy.
    let before = evidence.borrow().validations.len();
    let pending =
        controller.select_model(model.clone(), "continuation".into(), Some(String::new()))?;
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        let _ = controller.poll();
        ensure!(
            pending.try_recv()?.is_none(),
            "Admitted operation committed before its observed ownership boundary"
        );
        if evidence.borrow().validations.len() > before {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "Validation admission deadline exceeded"
        );
        thread::sleep(Duration::from_millis(10));
    }
    {
        let observed = evidence.borrow();
        ensure!(
            observed.validations.len() == before + 1,
            "Admission boundary contained unexpected validation requests"
        );
        let proof = observed
            .validations
            .last_key_value()
            .context("Missing admission")?
            .1;
        ensure!(
            !proof.prepared && !proof.done,
            "Admission boundary had already consumed preparation completion"
        );
    }
    controller.cancel_config_apply(pending.operation_id())?;
    ensure!(
        matches!(pending.try_recv()?, Some(ConfigApplyReceipt::Cancelled))
            && pending.try_recv().is_err()
            && controller.config() == current
            && persisted(settings)? == current,
        "Admission-bound cancellation duplicated its receipt or committed the candidate"
    );
    cases.push(
        json!({"case":"admitted-before-controller-completion-cancel",
        "receipt":"cancelled","acknowledged_release_before_admission":true,
        "matching_terminal_receipt_once":true,"current_and_saved_preserved":true,
        "worker_execution_stage_known":false,"inflight_gpu_race_exercised":false,
        "integrity_hash_race_exercised":false}),
    );

    let before = evidence.borrow().validations.len();
    let pending = controller.select_model(model.clone(), "writing".into(), Some(String::new()))?;
    let recovered = saved(receive(controller, &pending)?)?;
    ensure!(
        recovered == current && persisted(settings)? == current,
        "Model recovery changed configuration"
    );
    cases.push(json!({"case":"cold-model-recovery","receipt":"saved","proof":promotion(controller, evidence, before)?}));
    let pending = controller.select_model(model, "writing".into(), Some(String::new()))?;
    let _ = controller.shutdown()?;
    ensure!(
        matches!(pending.try_recv()?, Some(ConfigApplyReceipt::Stopped)),
        "Pending shutdown receipt missing"
    );
    ensure!(pending.try_recv().is_err(), "Duplicate shutdown receipt");
    shutdown(controller)?;
    ensure!(
        controller.config() == current && persisted(settings)? == current,
        "Shutdown mutated committed configuration"
    );
    cases.push(json!({"case":"shutdown-pending-operation","receipt":"stopped","owned_shutdown_complete":true}));
    let snapshot = controller.diagnostics();
    ensure!(
        snapshot.traces.is_empty()
            && snapshot.lost_preparations == 0
            && snapshot.lost_reports == 0
            && snapshot.unmatched_reports == 0
            && snapshot.preparations.len() == evidence.borrow().validations.len()
            && evidence.borrow().validations.keys().all(|id| snapshot
                .preparations
                .iter()
                .any(|report| report.request_id == *id)),
        "Not every real worker validation retained its standalone terminal diagnostics"
    );
    Ok(())
}
fn run() -> Result<i32> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args == [OsString::from("_worker")] {
        return brotypist::cli::run(vec!["_worker".into()]);
    }
    ensure!(
        cfg!(feature = "native-smoke"),
        "Build requires native-smoke"
    );
    ensure!(
        args.len() == 1,
        "Usage: config_apply_smoke <existing-read-only-local-gguf>"
    );
    let home = private_home()?;
    let model = PathBuf::from(&args[0]).canonicalize()?;
    ensure!(
        model.is_file() && !model.starts_with(home.canonicalize()?),
        "Model must be an existing external file"
    );
    let settings = home.join("settings.json");
    let initial = Config {
        backend: "gguf".into(),
        model_path: model.to_str().context("Non-Unicode model")?.into(),
        model_id: String::new(),
        prompt_style: "writing".into(),
        enabled: false,
        global_enabled: false,
        launch_at_login: false,
        check_updates_automatically: false,
        setup_completed: true,
        record_history: false,
        record_unaccepted: false,
        use_history: false,
        history_retention_days: 0,
        personalization_strength: 0,
        statistics_enabled: false,
        screen_context: false,
        clipboard_context: false,
        screenshot_context: false,
        screenshot_appearance: false,
        typo_suppression: false,
        typo_fixes: false,
        idle_release_seconds: 0,
        battery_model: "same".into(),
        battery_on_demand: false,
        battery_shorter: false,
        custom_instructions: String::new(),
        ..Config::default()
    };
    config::save_config_to(&initial, &settings)?;
    let evidence = Rc::new(RefCell::new(Evidence::default()));
    let destination = Rc::new(RefCell::new(settings.clone()));
    let storage: SharedStorage = Arc::new(Mutex::new(Box::new(HistoryStore::with_key_loader(
        home.join("owned-history.sqlite3"),
        Box::new(|| bail!("Config smoke must never unlock history")),
    ))));
    let epoch = Arc::new(AtomicU64::new(1));
    let preparer = AsyncPreparer::new(storage.clone(), epoch.clone(), Arc::new(NoExternalReaders))?;
    let worker = ObservedWorker {
        inner: WorkerClient::new()?,
        evidence: evidence.clone(),
    };
    let mut controller = Controller::with_engine(Engine::with_native_factory(
        initial,
        Box::new(worker),
        storage,
        Box::new(preparer),
        None,
        Box::new(PrivateReaders {
            destination: destination.clone(),
        }),
        Box::new(MonotonicClock),
        epoch,
        Box::new(|_| bail!("Headless owned configuration smoke has no native target")),
    )?)?;
    let mut cases = Vec::new();
    let result = exercise(
        &mut controller,
        &model,
        &home,
        &settings,
        &destination,
        &evidence,
        &mut cases,
    );
    let stopped = shutdown(&mut controller).is_ok();
    let evidence = evidence.borrow();
    let passed = result.is_ok() && stopped;
    let receipt = json!({"schema":"brotypist-config-apply-smoke/1","passed":passed,"cases":cases,
        "worker_clients_constructed":1,"release_requests":evidence.releases,
        "acknowledged_validation_admissions":evidence.acknowledged_admissions,
        "matching_prepared_done_pairs":evidence.validations.values().filter(|v| v.prepared && v.done).count(),
        "preparation_diagnostics":controller.diagnostics(),
        "shutdown_complete":stopped,"native_bridge_created":false,"external_readers_enabled":false,
        "quality_certification":false,"immutable_file_attestation":false,
        "native_ui_or_physical_input_exercised":false});
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(home.join("config-apply-receipt.json"))?;
    serde_json::to_writer_pretty(&mut output, &receipt)?;
    output.write_all(b"\n")?;
    output.sync_all()?;
    println!("{}", serde_json::to_string(&receipt)?);
    Ok(if passed { 0 } else { 1 })
}
fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(_) => {
            // Deliberately do not print arbitrary provider errors, paths or configuration text.
            eprintln!("Configuration smoke refused setup or could not write its bounded receipt.");
            ExitCode::FAILURE
        }
    }
}
