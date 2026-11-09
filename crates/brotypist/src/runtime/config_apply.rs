//! Operation-bound configuration commits. Validation shares the normal inference owner.
use super::*;
use crate::ui::settings::SettingsDraft;
use std::num::NonZeroU64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfigOperationId(NonZeroU64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigApplyStage {
    Validation,
    Ownership,
    Integrity,
    Preparation,
    Persistence,
    Release,
}
#[derive(Clone, Debug)]
pub enum ConfigApplyReceipt {
    /// Configuration was validated and persisted. GGUF changes additionally pass
    /// cold preparation; loopback backend availability is deliberately not checked.
    Saved {
        config: Config,
    },
    /// Configuration was applied for this session, without a persistence claim.
    /// A loopback backend receipt does not certify server or model availability.
    Applied {
        config: Config,
    },
    Failed {
        stage: ConfigApplyStage,
        message: String,
    },
    Cancelled,
    Superseded,
    Stopped,
}
pub struct PendingConfigApply {
    id: ConfigOperationId,
    receiver: Receiver<ConfigApplyReceipt>,
}
impl PendingConfigApply {
    pub fn operation_id(&self) -> ConfigOperationId {
        self.id
    }
    pub fn try_recv(&self) -> Result<Option<ConfigApplyReceipt>> {
        match self.receiver.try_recv() {
            Ok(receipt) => Ok(Some(receipt)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(anyhow!("Configuration operation has ended."))
            }
        }
    }
}
pub(super) struct Operation {
    id: ConfigOperationId,
    draft: SettingsDraft,
    persist: bool,
    reply: Option<SyncSender<ConfigApplyReceipt>>,
    candidate: Config,
    targets: Vec<Config>,
    target: usize,
    phase: Phase,
    cancel: models::DownloadCancellation,
    stage: ConfigApplyStage,
    deadline: Instant,
}
enum Phase {
    Release(crate::process_client::ReleaseTicket),
    Integrity {
        thread: JoinHandle<Result<()>>,
        ticket: crate::process_client::ReleaseTicket,
    },
    Retired,
    Prepare {
        id: NonZeroU64,
        prepared: bool,
    },
}
impl Operation {
    fn finish(&mut self, receipt: ConfigApplyReceipt) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.try_send(receipt);
        }
    }
}
impl Drop for Operation {
    fn drop(&mut self) {
        self.cancel.set();
        self.finish(ConfigApplyReceipt::Stopped);
    }
}
fn preparation_identity(c: &Config) -> (Vec<String>, String, String, String) {
    (
        c.provider_identity(),
        c.prompt_style.clone(),
        c.model_id.clone(),
        c.battery_model.clone(),
    )
}
fn targets(c: &Config, on_battery: bool) -> Result<Vec<Config>> {
    let mut result = Vec::with_capacity(2);
    if c.backend == "gguf" && c.battery_model != "same" {
        let preset = models::get_preset(&c.battery_model)?;
        let mut battery = c.clone();
        battery.model_path = models::model_path(preset)?.to_string_lossy().into_owned();
        battery.model_id = preset.id.clone();
        battery.prompt_style = preset.prompt_style.clone();
        result.push(battery);
    }
    result.push(c.clone());
    if on_battery {
        result.reverse();
    }
    Ok(result)
}
impl Controller {
    pub fn apply_settings(
        &self,
        draft: SettingsDraft,
        persist: bool,
    ) -> Result<PendingConfigApply> {
        self.engine
            .try_borrow_mut()
            .map_err(|_| anyhow!("Controller is busy."))?
            .begin_config_apply(draft, persist, false)
    }
    pub fn select_model(
        &self,
        path: String,
        prompt_style: String,
        model_id: Option<String>,
    ) -> Result<PendingConfigApply> {
        let baseline = self.config();
        let mut candidate = baseline.clone();
        candidate.backend = "gguf".into();
        candidate.model_path = path;
        candidate.prompt_style = prompt_style;
        let selected = crate::paths::expand_user(std::ffi::OsStr::new(&candidate.model_path)).ok();
        candidate.model_id = model_id.unwrap_or_else(|| {
            models::presets()
                .iter()
                .find(|p| {
                    selected
                        .as_ref()
                        .is_some_and(|path| models::model_path(p).ok().as_ref() == Some(path))
                })
                .map_or_else(String::new, |p| p.id.clone())
        });
        self.engine
            .try_borrow_mut()
            .map_err(|_| anyhow!("Controller is busy."))?
            .begin_config_apply(
                SettingsDraft::from_configs(&baseline, &candidate)?,
                true,
                true,
            )
    }
    pub fn cancel_config_apply(&self, id: ConfigOperationId) -> Result<()> {
        let mut engine = self
            .engine
            .try_borrow_mut()
            .map_err(|_| anyhow!("Controller is busy."))?;
        if engine.config_apply.as_ref().is_some_and(|op| op.id == id) {
            engine.retire_config_apply(ConfigApplyReceipt::Cancelled);
        }
        Ok(())
    }
}
impl Engine {
    pub(super) fn config_operation_status(&self) -> Value {
        self.config_apply.as_ref().map_or(Value::Null, |op| {
            let stage = match op.stage {
                ConfigApplyStage::Validation => "validation",
                ConfigApplyStage::Ownership => "ownership",
                ConfigApplyStage::Integrity => "integrity",
                ConfigApplyStage::Preparation => "preparation",
                ConfigApplyStage::Persistence => "persistence",
                ConfigApplyStage::Release => "release",
            };
            json!({"stage":stage})
        })
    }
    pub(super) fn begin_config_apply(
        &mut self,
        draft: SettingsDraft,
        persist: bool,
        force: bool,
    ) -> Result<PendingConfigApply> {
        static NEXT_OPERATION: AtomicU64 = AtomicU64::new(1);
        let serial = NEXT_OPERATION
            .try_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map_err(|_| anyhow!("Configuration operation identifiers exhausted."))?;
        let id = ConfigOperationId(NonZeroU64::new(serial).unwrap());
        let (reply, receiver) = mpsc::sync_channel(1);
        let pending = PendingConfigApply { id, receiver };
        if self.stopping {
            let _ = reply.try_send(ConfigApplyReceipt::Stopped);
            return Ok(pending);
        }
        if force || draft.has_model_intent() {
            self.model_intent_generation = self
                .model_intent_generation
                .checked_add(1)
                .expect("model intent generation exhausted");
            if let Some(migration) = &self.model_migration {
                migration.cancel.set();
            }
            self.retire_config_apply(ConfigApplyReceipt::Superseded);
        }
        let candidate = match draft.apply_to(&self.config) {
            Ok(c) => c,
            Err(e) => {
                let _ = reply.try_send(ConfigApplyReceipt::Failed {
                    stage: ConfigApplyStage::Validation,
                    message: e.to_string(),
                });
                return Ok(pending);
            }
        };
        let model_change =
            force || preparation_identity(&candidate) != preparation_identity(&self.config);
        if !model_change {
            let outcome = match self.apply_config(candidate, persist) {
                Ok(()) if persist => ConfigApplyReceipt::Saved {
                    config: self.config.clone(),
                },
                Ok(()) => ConfigApplyReceipt::Applied {
                    config: self.config.clone(),
                },
                Err(e) => ConfigApplyReceipt::Failed {
                    stage: ConfigApplyStage::Persistence,
                    message: e.to_string(),
                },
            };
            let _ = reply.try_send(outcome);
            return Ok(pending);
        }
        if self.preview {
            let _ = reply.try_send(ConfigApplyReceipt::Failed {
                stage: ConfigApplyStage::Ownership,
                message: "Close the owned preview before changing the model.".into(),
            });
            return Ok(pending);
        }
        let targets = match targets(&candidate, self.on_battery) {
            Ok(targets) => targets,
            Err(e) => {
                let _ = reply.try_send(ConfigApplyReceipt::Failed {
                    stage: ConfigApplyStage::Validation,
                    message: e.to_string(),
                });
                return Ok(pending);
            }
        };
        if self.retention_pending.take().is_some() {
            self.retention_after = self.now();
        }
        self.clear(true, true);
        self.candidate = None;
        self.resident = None;
        let ticket = match self.worker.release() {
            Ok(ticket) => ticket,
            Err(e) => {
                let _ = reply.try_send(ConfigApplyReceipt::Failed {
                    stage: ConfigApplyStage::Release,
                    message: e.to_string(),
                });
                return Ok(pending);
            }
        };
        self.config_retiring = None;
        self.config_release_failed = false;
        self.config_apply = Some(Operation {
            id,
            draft,
            persist,
            reply: Some(reply),
            candidate,
            targets,
            target: 0,
            phase: Phase::Release(ticket),
            cancel: models::DownloadCancellation::new(),
            stage: ConfigApplyStage::Release,
            deadline: self.now() + Duration::from_secs(120),
        });
        self.refresh_arming();
        self.wake.signal();
        Ok(pending)
    }
    pub(super) fn retire_config_apply(&mut self, outcome: ConfigApplyReceipt) {
        if let Some(mut op) = self.config_apply.take() {
            op.cancel.set();
            op.finish(outcome);
            if let Phase::Integrity { thread, .. } =
                std::mem::replace(&mut op.phase, Phase::Retired)
            {
                self.config_integrity_cleanup.push(thread);
            }
            match self.worker.release() {
                Ok(ticket) => self.config_retiring = Some(ticket),
                Err(_) => {
                    self.config_release_failed = true;
                    self.error =
                        "Model owner retirement failed; generation remains suspended.".into();
                }
            }
            self.refresh_arming();
        }
    }
    pub(super) fn advance_config_apply(&mut self) {
        let mut index = 0;
        while index < self.config_integrity_cleanup.len() {
            if self.config_integrity_cleanup[index].is_finished() {
                let _ = self.config_integrity_cleanup.swap_remove(index).join();
            } else {
                index += 1;
            }
        }
        if self
            .config_retiring
            .is_some_and(|ticket| self.worker.release_complete(ticket))
        {
            self.config_retiring = None;
            if !self.stopping && self.config_apply.is_none() {
                self.configure_native();
                self.capture_after = self.now();
            }
        }
        let Some(mut op) = self.config_apply.take() else {
            return;
        };
        let result: Result<bool> = (|| {
            ensure!(
                self.now() < op.deadline,
                "Configuration validation timed out."
            );
            match &mut op.phase {
                Phase::Release(ticket) => {
                    if !self.worker.release_complete(*ticket) {
                        return Ok(false);
                    }
                    // Loopback settings are validated locally, not by contacting the server.
                    // Retirement still precedes commit, but only GGUF owns a cold model.
                    if op.targets[op.target].backend != "gguf" {
                        return self.commit_config_apply(&mut op, false);
                    }
                    if op.targets[op.target].model_id.is_empty() {
                        op.stage = ConfigApplyStage::Preparation;
                        let id = self
                            .worker
                            .prepare_for_validation(&op.targets[op.target], *ticket)?;
                        op.phase = Phase::Prepare {
                            id,
                            prepared: false,
                        };
                        return Ok(false);
                    }
                    let config = op.targets[op.target].clone();
                    let cancel = op.cancel.clone();
                    let ticket = *ticket;
                    op.stage = ConfigApplyStage::Integrity;
                    let wake = self.wake.clone();
                    op.phase = Phase::Integrity {
                        ticket,
                        thread: thread::Builder::new()
                            .name("brotypist-config-integrity".into())
                            .spawn(move || {
                                let result = (|| {
                                    if config.backend == "gguf" && !config.model_id.is_empty() {
                                        let preset = models::get_preset(&config.model_id)?;
                                        let path = crate::paths::expand_user(
                                            std::ffi::OsStr::new(&config.model_path),
                                        )?;
                                        models::verify_catalogue_file(preset, &path, &cancel)?;
                                    }
                                    Ok(())
                                })();
                                wake.signal();
                                result
                            })?,
                    };
                }
                Phase::Retired => return Err(anyhow!("Configuration operation retired.")),
                Phase::Integrity { thread, .. } => {
                    if !thread.is_finished() {
                        return Ok(false);
                    }
                    let old = std::mem::replace(&mut op.phase, Phase::Retired);
                    let Phase::Integrity { thread, ticket } = old else {
                        unreachable!()
                    };
                    thread
                        .join()
                        .map_err(|_| anyhow!("Model integrity worker stopped."))??;
                    ensure!(
                        self.worker.release_complete(ticket),
                        "Model ownership changed during integrity validation."
                    );
                    op.stage = ConfigApplyStage::Preparation;
                    let id = self
                        .worker
                        .prepare_for_validation(&op.targets[op.target], ticket)?;
                    op.phase = Phase::Prepare {
                        id,
                        prepared: false,
                    };
                }
                Phase::Prepare { id, prepared } => {
                    let mut done = false;
                    for event in self.worker.poll() {
                        if event.id != id.get() {
                            continue;
                        }
                        match event.kind.as_str() {
                            "prepared" => *prepared = true,
                            "error" => return Err(anyhow!("{}", event.message)),
                            "done" => done = true,
                            _ => {}
                        }
                    }
                    if !done {
                        return Ok(false);
                    }
                    ensure!(
                        *prepared,
                        "Model preparation ended without verified preparation."
                    );
                    if op.target + 1 < op.targets.len() {
                        op.target += 1;
                        op.stage = ConfigApplyStage::Release;
                        op.phase = Phase::Release(self.worker.release()?);
                        return Ok(false);
                    }
                    return self.commit_config_apply(&mut op, true);
                }
            }
            Ok(false)
        })();
        match result {
            Ok(true) => {}
            Ok(false) => self.config_apply = Some(op),
            Err(e) => {
                let stage = op.stage;
                self.config_apply = Some(op);
                self.retire_config_apply(ConfigApplyReceipt::Failed {
                    stage,
                    message: e.to_string(),
                });
            }
        }
    }
    fn commit_config_apply(&mut self, op: &mut Operation, prepared: bool) -> Result<bool> {
        op.stage = ConfigApplyStage::Validation;
        let candidate = op.draft.apply_to(&self.config)?;
        ensure!(
            preparation_identity(&candidate) == preparation_identity(&op.candidate),
            "Model preparation inputs changed before commit."
        );
        if history_access_configured(&candidate) {
            locked(&self.storage).unlock()?;
        }
        op.stage = ConfigApplyStage::Persistence;
        self.config_promoting = true;
        let applied = self.apply_config(candidate, op.persist);
        self.config_promoting = false;
        applied?;
        op.finish(if op.persist {
            ConfigApplyReceipt::Saved {
                config: self.config.clone(),
            }
        } else {
            ConfigApplyReceipt::Applied {
                config: self.config.clone(),
            }
        });
        self.resident = prepared.then(|| op.targets[op.target].provider_identity());
        Ok(true)
    }
}
