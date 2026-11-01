//! Explicit diagnostic-only native session. Compiled only by native-smoke.
//! Reuses the ancestor's canonical helpers without changing production Config
//! bounds, providers, model selection or ordinary application flags.
use super::{
    FramingCache, GenerationOptions, HealingReplay, OutputStream, UTILITY_GRAMMAR, alternatives,
    guided_family, healing_tail, native_stops, prompt_tokens,
};
use crate::{
    config::Config,
    inference_native::{FinishReason, NativeModel, SamplerOptions, Tokenizer},
    writing,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};
pub struct DiagnosticSession {
    model: NativeModel,
    config: Config,
    framing: FramingCache,
    calls: Vec<Value>,
    cancel_on_first_chunk: bool,
    cancelled_at: Option<Instant>,
}
impl DiagnosticSession {
    pub fn open(path: &Path, prompt_style: &str, acceleration: &str) -> Result<Self> {
        let mut config = Config {
            backend: "gguf".into(),
            model_path: path
                .to_str()
                .context("Non-Unicode diagnostic model path")?
                .into(),
            prompt_style: prompt_style.into(),
            acceleration: acceleration.into(),
            launch_at_login: false,
            check_updates_automatically: false,
            clipboard_context: false,
            screen_context: false,
            screenshot_context: false,
            record_history: false,
            record_unaccepted: false,
            statistics_enabled: false,
            personalization_strength: 0,
            idle_release_seconds: 0,
            ..Config::default()
        };
        config.validate()?;
        let model = NativeModel::open(path, &config.acceleration)?;
        let mut direct = Self {
            model,
            config,
            framing: FramingCache::default(),
            calls: Vec::new(),
            cancel_on_first_chunk: false,
            cancelled_at: None,
        };
        let tokens = if direct.config.prompt_style == "writing" {
            let family = guided_family(&direct.model)?;
            let first =
                writing::writing_prompt_spans("", "", "", "", &family, Some(""), "")?.remove(0);
            direct
                .framing
                .tokenize(&direct.model, vec![first], Some(&family))?
        } else {
            direct.model.tokenize(" ", true, true)?
        };
        ensure!(
            direct.model.evaluate(&tokens, &AtomicBool::new(false))?,
            "Native warmup was unexpectedly cancelled"
        );
        Ok(direct)
    }
    fn stream_one(
        &mut self,
        prefix: &str,
        suffix: &str,
        options: &GenerationOptions,
        diverse: bool,
        token_cap: u32,
        cancel: &AtomicBool,
        emit: &mut dyn FnMut(&str) -> Result<()>,
    ) -> Result<()> {
        if cancel.load(Ordering::Acquire) {
            return Ok(());
        }
        let utility = !options.utility.is_empty();
        let healing = if self.config.prompt_style == "writing" && !utility {
            healing_tail(prefix)
        } else {
            String::new()
        };
        let draft = if healing.is_empty() {
            prefix
        } else {
            &prefix[..prefix.len() - healing.len()]
        };
        // This copy is used ONLY as a pure prompt-budget input. The actual
        // provider Config remains validated at <=128, never passed to Runtime,
        // WorkerClient or GgufProvider with a loosened production token bound.
        let mut prompt_budget = self.config.clone();
        prompt_budget.max_tokens = token_cap;
        let tokens = prompt_tokens(
            &self.model,
            &prompt_budget,
            draft,
            suffix,
            options,
            &mut self.framing,
            Some(prefix),
        )?;
        let family = if self.config.prompt_style == "writing" || utility {
            Some(guided_family(&self.model)?)
        } else {
            None
        };
        let grammar = if utility {
            Some(UTILITY_GRAMMAR.into())
        } else if !healing.is_empty() {
            Some(format!(
                "root ::= {} [^\\n\\r]*",
                serde_json::to_string(&healing)?
            ))
        } else {
            None
        };
        let replay_tokens = if healing.is_empty() {
            0
        } else {
            self.model.tokenize(&healing, false, false)?.len()
        };
        let limit = (token_cap as usize)
            .checked_add(replay_tokens)
            .context("Native diagnostic token allowance overflow")?
            .min(self.model.context_size().saturating_sub(tokens.len() + 1));
        let sampler = SamplerOptions::for_family(family.as_deref(), diverse);
        let stops = native_stops(family.as_deref(), utility);
        let bos = self
            .model
            .metadata("tokenizer.ggml.bos_token_id")?
            .parse::<i32>()
            .ok();
        let architecture = self.model.metadata("general.architecture")?;
        let settings = json!({"max_tokens":limit, "stream":true, "temperature":sampler.temperature,
            "top_k":sampler.top_k, "top_p":sampler.top_p, "min_p":sampler.min_p,
            "repeat_penalty":sampler.repeat_penalty, "grammar":grammar, "stop":stops});
        let mut observation = json!({"settings":settings, "resolved_arguments":settings,
            "prompt_token_count":tokens.len(), "prompt_type":"native_token_ids", "architecture":architecture,
            "bos_token_id":bos, "bos_token_occurrences":bos.map(|bos| tokens.iter().filter(|&&id| id == bos).count()),
            "raw_generated_text":"", "finish_reasons":[], "native_stream_exhausted":false,
            "native_stream_closed":false, "context_size":self.model.context_size(),
            "sampler_seed":null,
            "native_api":"NativeModel::generate; raw UTF-8 after native stop fence and before canonical replay/display filters"});
        let mut output = OutputStream::new(suffix, &self.config, options);
        let mut replay = HealingReplay::new(&healing);
        let mut raw = String::new();
        let mut finish = Vec::new();
        let mut first_chunk = true;
        let mut terminal_seen = false;
        let mut stopped_by_consumer = false;
        let mut cancelled_at = None;
        let cancel_on_first = self.cancel_on_first_chunk;
        let generation = self.model.generate(
            &tokens,
            limit,
            sampler,
            grammar.as_deref(),
            &stops,
            cancel,
            &mut |delta, reason| {
                raw.push_str(delta);
                if let Some(reason) = reason {
                    terminal_seen = true;
                    finish.push(if reason == FinishReason::Length {
                        "length"
                    } else {
                        "stop"
                    });
                }
                if first_chunk {
                    first_chunk = false;
                    if cancel_on_first {
                        cancelled_at = Some(Instant::now());
                        cancel.store(true, Ordering::Release);
                    }
                }
                if cancel.load(Ordering::Acquire) {
                    stopped_by_consumer = true;
                    return Ok(false);
                }
                let keep = match replay.push(delta, reason)? {
                    Some(delta) => output.push(&delta, reason, cancel, emit)?,
                    None => reason.is_none(),
                };
                stopped_by_consumer |= !keep;
                Ok(keep)
            },
        );
        // NativeModel::generate returns only after its request sampler cleanup.
        // A callback error also returns through that cleanup, never an invented
        // iterator-close acknowledgement or a reconstructed model output.
        observation["native_stream_closed"] = json!(true);
        observation["native_stream_exhausted"] = json!(terminal_seen && generation.is_ok());
        observation["stopped_by_consumer"] = json!(stopped_by_consumer);
        observation["raw_generated_text"] = json!(raw);
        observation["finish_reasons"] = json!(finish);
        observation["native_call_error"] =
            json!(generation.as_ref().err().map(ToString::to_string));
        let seed = self.model.diagnostic_sampler_seed();
        observation["native_seed_state_after_call"] = json!(seed);
        if generation.is_ok() || !first_chunk {
            observation["sampler_seed"] = json!(seed as u32);
            observation["resolved_arguments"]["seed"] = json!(seed as u32);
        }
        self.calls.push(observation);
        if cancelled_at.is_some() {
            self.cancelled_at = cancelled_at;
        }
        generation?;
        output.finish(cancel, emit)
    }
    pub fn stream(
        &mut self,
        prefix: &str,
        suffix: &str,
        options: &GenerationOptions,
        token_cap: u32,
        cancel: &AtomicBool,
        emit: &mut dyn FnMut(&str) -> Result<()>,
    ) -> Result<()> {
        ensure!(
            (1..=256).contains(&token_cap),
            "Diagnostic native request cap must be 1–256"
        );
        if options.alternatives {
            alternatives(
                prefix,
                suffix,
                options,
                cancel,
                emit,
                &mut |p, s, o, diverse, callback| {
                    self.stream_one(p, s, o, diverse, token_cap, cancel, callback)
                },
            )
        } else {
            self.stream_one(prefix, suffix, options, false, token_cap, cancel, emit)
        }
    }
    pub fn observations(&self) -> &[Value] {
        &self.calls
    }
    pub fn reset_observations(&mut self) {
        self.calls.clear();
        self.cancelled_at = None;
    }
    pub fn cancel_on_first_chunk(&mut self, enabled: bool) {
        self.cancel_on_first_chunk = enabled;
    }
    pub fn cancellation_time(&self) -> Option<Instant> {
        self.cancelled_at
    }
    pub fn metadata(&self, key: &str) -> Result<String> {
        self.model.metadata(key)
    }
    pub fn context_size(&self) -> usize {
        self.model.context_size()
    }
    pub fn gpu_layers_requested(&self) -> i32 {
        self.model.gpu_layers_requested()
    }
}
