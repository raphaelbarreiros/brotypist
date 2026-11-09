//! Explicit diagnostic-only native session. Compiled only by native-smoke.
//! Reuses the ancestor's canonical helpers without changing production Config
//! bounds, providers, model selection or ordinary application flags.
//! Canonical publication observations describe synchronous callback ordering only,
//! not GPU completion, compositor presentation, input latency or native acceptance.
use super::{
    FramingCache, GenerationOptions, HealingReplay, MAX_SUGGESTION_CHARS, OutputStream,
    UTILITY_GRAMMAR, alternatives, healing_tail, native_prompt_tokens, native_stops,
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
/// Explicit native-smoke content trace; never populated by ordinary providers.
#[derive(Debug, serde::Serialize)]
pub struct PromptProjectionSpan {
    pub index: usize,
    pub start: usize,
    pub end: usize,
    pub special: bool,
    pub text: String,
}

#[derive(Debug, serde::Serialize)]
pub struct PromptProjection {
    pub tokens: Vec<i32>,
    pub replay: String,
    pub replay_tokens: usize,
    pub grammar: Option<String>,
    pub family: String,
    pub stops: Vec<String>,
    pub spans: Vec<PromptProjectionSpan>,
}

/// Diagnostic-only tokenizer projection of the last successful canonical fitting pass.
/// Includes caller-supplied content; use only explicit synthetic diagnostic inputs.
/// No decode, scheduling, latency or live-cache claim is made by this function.
/// Ordinary writing keeps authored bytes intact and uses the canonical NFC model
/// replay; utility requests keep their framing, JSON grammar and stops.
pub fn prompt_projection(
    model: &dyn Tokenizer,
    config: &Config,
    prefix: &str,
    suffix: &str,
    options: &GenerationOptions,
) -> Result<PromptProjection> {
    let utility = !options.utility.is_empty();
    ensure!(
        config.prompt_style == "writing" || utility,
        "Prompt projection requires native writing or utility"
    );
    let mut framing = FramingCache {
        projection_spans: Some(Vec::new()),
        ..FramingCache::default()
    };
    framing.validate_guided(model)?;
    let authored_replay = if utility {
        String::new()
    } else {
        healing_tail(prefix)
    };
    let (tokens, replay_tokens, replay) = native_prompt_tokens(
        model,
        config,
        prefix,
        suffix,
        options,
        &mut framing,
        &authored_replay,
    )?;
    let family = framing
        .validated_family
        .take()
        .context("Missing validated family")?;
    let grammar = if utility {
        Some(UTILITY_GRAMMAR.into())
    } else if replay.is_empty() {
        None
    } else {
        Some(format!(
            "root ::= {} [^\\n\\r]*",
            serde_json::to_string(&replay)?
        ))
    };
    let stops = native_stops(Some(&family), utility)
        .into_iter()
        .map(String::from)
        .collect();
    Ok(PromptProjection {
        tokens,
        replay: replay.into_owned(),
        replay_tokens,
        grammar,
        family,
        stops,
        spans: framing
            .projection_spans
            .take()
            .context("Missing projection spans")?,
    })
}

/// Counts offers before invoking the consumer, so callback errors retain evidence.
/// Successful emissions mean only that the synchronous consumer returned Ok.
#[derive(Default, serde::Serialize)]
struct PublicationCounts {
    offers: usize,
    nonempty_offers: usize,
    successful_emissions: usize,
    nonempty_successful_emissions: usize,
}

impl PublicationCounts {
    fn offer(&mut self, text: &str) {
        self.offers += 1;
        self.nonempty_offers += usize::from(!text.is_empty());
    }

    fn emitted(&mut self, text: &str) {
        self.successful_emissions += 1;
        self.nonempty_successful_emissions += usize::from(!text.is_empty());
    }
}

/// Diagnostic-only bounded snapshot, reused across native callbacks. Ordinary
/// canonical completions fit 512 Unicode scalars; larger utility publications
/// are counted without retaining their bytes or claiming an exact comparison.
struct NativePublication {
    inside_generate: PublicationCounts,
    terminal_callback: PublicationCounts,
    finish_after_return: PublicationCounts,
    last_native_emission: String,
    last_native_emission_retained: bool,
    nonempty_finish_offer_matches_last_native_emission: Option<bool>,
}

impl NativePublication {
    fn new() -> Self {
        Self {
            inside_generate: PublicationCounts::default(),
            terminal_callback: PublicationCounts::default(),
            finish_after_return: PublicationCounts::default(),
            last_native_emission: String::with_capacity(4 * MAX_SUGGESTION_CHARS),
            last_native_emission_retained: false,
            nonempty_finish_offer_matches_last_native_emission: None,
        }
    }

    fn native_emit(
        &mut self,
        text: &str,
        terminal: bool,
        emit: &mut dyn FnMut(&str) -> Result<()>,
    ) -> Result<()> {
        self.inside_generate.offer(text);
        if terminal {
            self.terminal_callback.offer(text);
        }
        emit(text)?;
        self.inside_generate.emitted(text);
        if terminal {
            self.terminal_callback.emitted(text);
        }
        self.last_native_emission.clear();
        self.last_native_emission_retained = text.len() <= 4 * MAX_SUGGESTION_CHARS;
        if self.last_native_emission_retained {
            self.last_native_emission.push_str(text);
        }
        Ok(())
    }

    fn finish_emit(&mut self, text: &str, emit: &mut dyn FnMut(&str) -> Result<()>) -> Result<()> {
        self.finish_after_return.offer(text);
        self.nonempty_finish_offer_matches_last_native_emission = if text.is_empty() {
            None
        } else if self.inside_generate.successful_emissions == 0 {
            Some(false)
        } else if self.last_native_emission_retained {
            Some(text == self.last_native_emission)
        } else {
            None
        };
        emit(text)?;
        self.finish_after_return.emitted(text);
        Ok(())
    }
}

pub struct DiagnosticSession {
    model: NativeModel,
    config: Config,
    framing: FramingCache,
    calls: Vec<Value>,
    cancel_on_first_chunk: bool,
    cancelled_at: Option<Instant>,
    next_seed: Option<u32>,
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
            next_seed: None,
        };
        let tokens = if direct.config.prompt_style == "writing" {
            direct.framing.validate_guided(&direct.model)?;
            let family = direct
                .framing
                .validated_family
                .clone()
                .expect("validated family");
            let first =
                writing::writing_prompt_spans("", "", "", "", &family, Some(""), "", "")?.remove(0);
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
    pub fn set_context_chars(&mut self, context_chars: u32) -> Result<()> {
        let mut config = self.config.clone();
        config.context_chars = context_chars;
        config.validate()?;
        self.config = config;
        Ok(())
    }

    pub fn projection(
        &self,
        prefix: &str,
        suffix: &str,
        options: &GenerationOptions,
        token_cap: u32,
    ) -> Result<PromptProjection> {
        ensure!(
            (1..=256).contains(&token_cap),
            "Diagnostic native request cap must be 1–256"
        );
        // Same diagnostic-only budget as stream_one; production Config is unchanged.
        let mut budget = self.config.clone();
        budget.max_tokens = token_cap;
        prompt_projection(&self.model, &budget, prefix, suffix, options)
    }

    /// Diagnostic-only first sampler override for the next stream. Alternatives
    /// retain the native recurrence after that first actual generation call.
    /// Rejects the native random-seed sentinel; rejection retires any prior override.
    pub fn set_next_seed(&mut self, seed: u32) -> Result<()> {
        self.next_seed = None;
        self.model.diagnostic_set_next_seed(seed)?;
        self.model.diagnostic_clear_next_seed();
        self.next_seed = Some(seed);
        Ok(())
    }
    /// Clear actual KV/logits without reopening the model or clearing OS caches.
    pub fn clear_cache(&mut self) {
        self.model.diagnostic_clear_cache();
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
        if self.config.prompt_style == "writing" || utility {
            self.framing.validate_guided(&self.model)?;
        }
        let authored_healing = if self.config.prompt_style == "writing" && !utility {
            healing_tail(prefix)
        } else {
            String::new()
        };
        // This copy is used ONLY as a pure prompt-budget input. The actual
        // provider Config remains validated at <=128, never passed to Runtime,
        // WorkerClient or GgufProvider with a loosened production token bound.
        let mut prompt_budget = self.config.clone();
        prompt_budget.max_tokens = token_cap;
        let (tokens, replay_tokens, healing) = native_prompt_tokens(
            &self.model,
            &prompt_budget,
            prefix,
            suffix,
            options,
            &mut self.framing,
            &authored_healing,
        )?;
        let family = if self.config.prompt_style == "writing" || utility {
            self.framing.validated_family.clone()
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
        let mut output = OutputStream::new(prefix, suffix, &self.config, options);
        let mut replay = HealingReplay::new(&healing);
        let mut publication = NativePublication::new();
        let mut raw = String::new();
        let mut finish = Vec::new();
        let mut first_chunk = true;
        let mut terminal_seen = false;
        let mut stopped_by_consumer = false;
        let mut cancelled_at = None;
        let cancel_on_first = self.cancel_on_first_chunk;
        let requested_seed = self.next_seed.take();
        if let Some(seed) = requested_seed {
            self.model.diagnostic_set_next_seed(seed)?;
        }
        observation["requested_seed_override"] = json!(requested_seed);
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
                    Some(delta) => output.push(&delta, reason, cancel, &mut |text| {
                        publication.native_emit(text, reason.is_some(), emit)
                    })?,
                    None => reason.is_none(),
                };
                stopped_by_consumer |= !keep;
                Ok(keep)
            },
        );
        self.model.diagnostic_clear_next_seed();
        // NativeModel::generate returns only after its request sampler cleanup.
        // A callback error also returns through that cleanup, never an invented
        // iterator-close acknowledgement or a reconstructed model output.
        observation["native_stream_closed"] = json!(true);
        observation["native_work"] = json!(self.model.work());
        observation["native_stream_exhausted"] = json!(terminal_seen && generation.is_ok());
        observation["stopped_by_consumer"] = json!(stopped_by_consumer);
        observation["raw_generated_text"] = json!(raw);
        observation["finish_reasons"] = json!(finish);
        observation["native_call_error"] =
            json!(generation.as_ref().err().map(ToString::to_string));
        let seed = self.model.diagnostic_sampler_seed();
        observation["native_seed_state_after_call"] = json!(seed);
        if self.model.work().sampled_tokens > 0 || !first_chunk {
            let actual_seed = self.model.diagnostic_last_sampler_seed();
            observation["sampler_seed"] = json!(actual_seed);
            observation["resolved_arguments"]["seed"] = json!(actual_seed);
        }
        let finish_result = if generation.is_ok() {
            Some(output.finish(cancel, &mut |text| publication.finish_emit(text, emit)))
        } else {
            None
        };
        observation["canonical_output_stream"] = json!({
            "inside_native_generate": publication.inside_generate,
            "known_terminal_callback": publication.terminal_callback,
            "known_terminal_callback_seen": terminal_seen,
            "known_terminal_callback_emitted": terminal_seen
                .then_some(publication.terminal_callback.successful_emissions > 0),
            "finish_after_native_return": publication.finish_after_return,
            "finish_called": finish_result.is_some(),
            "finish_error": finish_result.as_ref()
                .and_then(|result| result.as_ref().err()).map(ToString::to_string),
            "nonempty_finish_offer_matches_last_native_emission":
                publication.nonempty_finish_offer_matches_last_native_emission,
            "exact_comparison_byte_limit": 4 * MAX_SUGGESTION_CHARS,
            "comparison_scope": "Nonempty canonical finish offer versus last successful canonical emission inside NativeModel::generate; null if no nonempty finish offer or the prior emission exceeded the retained-byte bound.",
            "limitation": "Synchronous callback ordering and consumer return only; not GPU/compositor/input latency, worker delivery, or physical native acceptance."
        });
        self.calls.push(observation);
        if cancelled_at.is_some() {
            self.cancelled_at = cancelled_at;
        }
        generation?;
        match finish_result {
            Some(result) => result,
            None => Ok(()),
        }
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
        let result = (|| {
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
        })();
        // No-offer/pre-cancel paths must not transfer a case's seed to another stream.
        self.next_seed = None;
        self.model.diagnostic_clear_next_seed();
        result
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
