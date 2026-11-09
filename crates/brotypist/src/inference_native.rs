//! Safe, thread-confined owner of the pinned llama.cpp C ABI.
//! No Rust layout assumptions about upstream structs and no Python runtime.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
#[cfg(feature = "native-smoke")]
use sha2::{Digest, Sha256};
use std::{
    ffi::{CString, c_char, c_int, c_void},
    marker::PhantomData,
    path::Path,
    ptr::NonNull,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

pub const LLAMA_REVISION: &str = "0c1e57098bba43ac29e6e3b677cdceebdd22334f";
unsafe extern "C" {
    fn bt_llama_open(
        path: *const c_char,
        auto_gpu: c_int,
        threads: c_int,
        full_swa: c_int,
        error: *mut c_char,
        cap: usize,
    ) -> *mut c_void;
    #[cfg(feature = "native-smoke")]
    fn bt_llama_diagnostic_open(
        path: *const c_char,
        auto_gpu: c_int,
        threads: c_int,
        full_swa: c_int,
        flash: c_int,
        error: *mut c_char,
        cap: usize,
    ) -> *mut c_void;
    #[cfg(feature = "native-smoke")]
    fn bt_llama_diagnostic_drain(owner: *mut c_void);
    #[cfg(feature = "native-smoke")]
    fn bt_llama_diagnostic_full_vocabulary_grammar(owner: *mut c_void, enabled: c_int);
    #[cfg(feature = "native-smoke")]
    fn bt_llama_diagnostic_state_size(owner: *mut c_void) -> usize;
    #[cfg(feature = "native-smoke")]
    fn bt_llama_diagnostic_state_save(owner: *mut c_void, out: *mut u8, cap: usize) -> usize;
    #[cfg(feature = "native-smoke")]
    fn bt_llama_diagnostic_state_restore(owner: *mut c_void, data: *const u8, size: usize)
    -> usize;
    #[cfg(feature = "native-smoke")]
    fn bt_llama_diagnostic_bias(
        owner: *mut c_void,
        tokens: *const i32,
        bias: *const f32,
        count: i32,
    ) -> c_int;
    fn bt_llama_close(owner: *mut c_void);
    fn bt_llama_context_size(owner: *const c_void) -> c_int;
    fn bt_llama_gpu_layers(owner: *const c_void) -> c_int;
    fn bt_llama_meta(
        owner: *const c_void,
        key: *const c_char,
        bytes: *mut c_char,
        cap: usize,
    ) -> c_int;
    fn bt_llama_tokenize(
        owner: *const c_void,
        text: *const c_char,
        length: i32,
        tokens: *mut i32,
        cap: i32,
        add_bos: c_int,
        special: c_int,
    ) -> c_int;
    fn bt_llama_piece(owner: *const c_void, token: i32, bytes: *mut c_char, cap: i32) -> c_int;
    fn bt_llama_is_eog(owner: *const c_void, token: i32) -> c_int;
    fn bt_llama_trim(owner: *mut c_void, from: i32) -> c_int;
    fn bt_llama_reset(owner: *mut c_void);
    fn bt_llama_decode(owner: *mut c_void, tokens: *const i32, count: i32, position: i32) -> c_int;
    fn bt_llama_sampler(
        owner: *mut c_void,
        top_k: c_int,
        top_p: f32,
        min_p: f32,
        temperature: f32,
        repeat: f32,
        seed: u32,
        grammar: *const c_char,
    ) -> c_int;
    fn bt_llama_sample(owner: *mut c_void) -> i32;
    fn bt_llama_sampler_close(owner: *mut c_void);
    #[cfg(feature = "native-smoke")]
    fn bt_llama_diagnostic_logits(owner: *mut c_void, out: *mut f32, cap: i32) -> i32;
    #[cfg(feature = "native-smoke")]
    fn bt_llama_diagnostic_cache(owner: *const c_void, minimum: *mut i32, maximum: *mut i32)
    -> i32;
    #[cfg(target_os = "macos")]
    fn bt_draft_language_name(
        text: *const c_char,
        length: usize,
        out: *mut c_char,
        cap: usize,
    ) -> c_int;
}

pub fn draft_language_name(text: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let mut out = [0u8; 512];
        // SAFETY: input is a length-delimited UTF-8 span; output capacity is exact.
        let length = unsafe {
            bt_draft_language_name(
                text.as_ptr().cast(),
                text.len(),
                out.as_mut_ptr().cast(),
                out.len(),
            )
        };
        if length > 0 && (length as usize) < out.len() {
            return String::from_utf8(out[..length as usize].to_vec()).ok();
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = text;
    None
}

pub trait Tokenizer {
    fn tokenize(&self, text: &str, add_bos: bool, special: bool) -> Result<Vec<i32>>;
    fn metadata(&self, key: &str) -> Result<String>;
    fn context_size(&self) -> usize;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SamplerOptions {
    pub top_k: i32,
    pub top_p: f32,
    pub min_p: f32,
    pub temperature: f32,
    pub repeat_penalty: f32,
}
impl SamplerOptions {
    pub fn for_family(family: Option<&str>, diverse: bool) -> Self {
        let mut value = Self {
            top_k: 40,
            top_p: 0.9,
            min_p: 0.05,
            temperature: if diverse { 0.35 } else { 0.2 },
            repeat_penalty: if family.is_some() { 1.0 } else { 1.1 },
        };
        match family {
            Some(f) if f.starts_with("gemma") => {
                value.top_k = 64;
                value.top_p = 0.95;
                value.min_p = 0.0;
            }
            Some("qwen3") => {
                value.top_k = 20;
                value.top_p = 0.8;
                value.min_p = 0.0;
            }
            _ => {}
        }
        value
    }
}

/// Content-free work observed at the native decode boundary. Submitted tokens
/// are not counted as completed when llama_decode fails; neither count implies
/// that a suggestion was displayed or accepted.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DecodeWork {
    pub submitted_tokens: u64,
    pub completed_tokens: u64,
    pub submitted_batches: u64,
    pub completed_batches: u64,
    pub elapsed_us: u64,
}
impl std::ops::AddAssign for DecodeWork {
    fn add_assign(&mut self, other: Self) {
        self.submitted_tokens = self.submitted_tokens.saturating_add(other.submitted_tokens);
        self.completed_tokens = self.completed_tokens.saturating_add(other.completed_tokens);
        self.submitted_batches = self
            .submitted_batches
            .saturating_add(other.submitted_batches);
        self.completed_batches = self
            .completed_batches
            .saturating_add(other.completed_batches);
        self.elapsed_us = self.elapsed_us.saturating_add(other.elapsed_us);
    }
}

/// Per-evaluation counters, including work retained on cancellation/error.
/// Sampled tokens include EOG and tokens withheld by UTF-8/healing/display fences.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeWork {
    pub evaluations: u64,
    pub input_tokens: u64,
    pub reused_tokens: u64,
    pub prompt: DecodeWork,
    pub generation: DecodeWork,
    pub sampled_tokens: u64,
    pub cold_evaluations: u64,
    pub exact_reuses: u64,
    pub prefix_reuses: u64,
    pub prefix_misses: u64,
    pub trim_failures: u64,
    pub cache_resets: u64,
}
impl std::ops::AddAssign for NativeWork {
    fn add_assign(&mut self, other: Self) {
        self.evaluations = self.evaluations.saturating_add(other.evaluations);
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.reused_tokens = self.reused_tokens.saturating_add(other.reused_tokens);
        self.prompt += other.prompt;
        self.generation += other.generation;
        self.sampled_tokens = self.sampled_tokens.saturating_add(other.sampled_tokens);
        self.cold_evaluations = self.cold_evaluations.saturating_add(other.cold_evaluations);
        self.exact_reuses = self.exact_reuses.saturating_add(other.exact_reuses);
        self.prefix_reuses = self.prefix_reuses.saturating_add(other.prefix_reuses);
        self.prefix_misses = self.prefix_misses.saturating_add(other.prefix_misses);
        self.trim_failures = self.trim_failures.saturating_add(other.trim_failures);
        self.cache_resets = self.cache_resets.saturating_add(other.cache_resets);
    }
}

#[cfg(feature = "native-smoke")]
pub const DIAGNOSTIC_CHECKPOINT_SLOTS: usize = 2;
#[cfg(feature = "native-smoke")]
pub const DIAGNOSTIC_CHECKPOINT_BYTE_CAP: usize = 256 * 1024 * 1024;

/// Opaque in-memory capability tied to one model/context and immutable policy.
#[cfg(feature = "native-smoke")]
pub struct DiagnosticCheckpoint {
    owner: Rc<()>,
    slot: usize,
    serial: u64,
}

#[cfg(feature = "native-smoke")]
struct SavedCheckpoint {
    state: zeroize::Zeroizing<Vec<u8>>,
    tokens: zeroize::Zeroizing<Vec<i32>>,
    serial: u64,
    digest: [u8; 32],
}

pub struct NativeModel {
    owner: NonNull<c_void>,
    evaluated: Vec<i32>,
    logits_valid: bool,
    #[cfg(feature = "native-smoke")]
    cancel_after_trim: bool,
    #[cfg(feature = "native-smoke")]
    next_seed: Option<u32>,
    #[cfg(feature = "native-smoke")]
    last_sampler_seed: Option<u32>,
    #[cfg(feature = "native-smoke")]
    cancel_after_batches: Option<usize>,
    #[cfg(feature = "native-smoke")]
    checkpoint_owner: Rc<()>,
    #[cfg(feature = "native-smoke")]
    checkpoints: [Option<SavedCheckpoint>; DIAGNOSTIC_CHECKPOINT_SLOTS],
    #[cfg(feature = "native-smoke")]
    checkpoint_serial: u64,
    context_size: usize,
    seed: u64,
    work: NativeWork,
    // The foreign model/context/sampler must never move to another thread.
    _thread: PhantomData<Rc<()>>,
}
impl NativeModel {
    pub fn open(path: &Path, acceleration: &str) -> Result<Self> {
        Self::open_with_cache(path, acceleration, true)
    }
    #[cfg(feature = "native-smoke")]
    pub fn diagnostic_open_compact_cache(path: &Path, acceleration: &str) -> Result<Self> {
        Self::open_with_cache(path, acceleration, false)
    }
    fn open_with_cache(path: &Path, acceleration: &str, full_swa: bool) -> Result<Self> {
        Self::open_with_policy(path, acceleration, full_swa, false)
    }
    /// Experimental policy only; production open remains full-SWA/flash-off.
    #[cfg(feature = "native-smoke")]
    pub fn diagnostic_open_policy(
        path: &Path,
        acceleration: &str,
        full_swa: bool,
        flash_attention: bool,
    ) -> Result<Self> {
        Self::open_with_policy(path, acceleration, full_swa, flash_attention)
    }
    fn open_with_policy(
        path: &Path,
        acceleration: &str,
        full_swa: bool,
        flash_attention: bool,
    ) -> Result<Self> {
        ensure!(
            path.is_file(),
            "GGUF model file does not exist. Choose an existing model in Settings."
        );
        let path = CString::new(
            path.to_str()
                .context("The GGUF path is not valid Unicode.")?,
        )
        .context("The GGUF path contains a NUL byte.")?;
        let threads = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .min(i32::MAX as usize) as i32;
        let mut error = [0u8; 512];
        // SAFETY: all pointers are borrowed only during the call; success returns
        // a uniquely owned allocation released by this owner's Drop.
        let raw = unsafe {
            #[cfg(feature = "native-smoke")]
            if flash_attention {
                bt_llama_diagnostic_open(
                    path.as_ptr(),
                    (acceleration == "auto") as c_int,
                    threads,
                    full_swa as c_int,
                    1,
                    error.as_mut_ptr().cast(),
                    error.len(),
                )
            } else {
                bt_llama_open(
                    path.as_ptr(),
                    (acceleration == "auto") as c_int,
                    threads,
                    full_swa as c_int,
                    error.as_mut_ptr().cast(),
                    error.len(),
                )
            }
            #[cfg(not(feature = "native-smoke"))]
            {
                let _ = flash_attention;
                bt_llama_open(
                    path.as_ptr(),
                    (acceleration == "auto") as c_int,
                    threads,
                    full_swa as c_int,
                    error.as_mut_ptr().cast(),
                    error.len(),
                )
            }
        };
        let Some(owner) = NonNull::new(raw) else {
            let end = error.iter().position(|&b| b == 0).unwrap_or(error.len());
            bail!("{}", String::from_utf8_lossy(&error[..end]));
        };
        let context_size = unsafe { bt_llama_context_size(owner.as_ptr()) };
        if context_size <= 0 {
            unsafe {
                bt_llama_close(owner.as_ptr());
            }
            bail!("The model context is invalid.");
        }
        Ok(Self {
            owner,
            evaluated: Vec::new(),
            logits_valid: false,
            #[cfg(feature = "native-smoke")]
            cancel_after_trim: false,
            #[cfg(feature = "native-smoke")]
            next_seed: None,
            #[cfg(feature = "native-smoke")]
            last_sampler_seed: None,
            #[cfg(feature = "native-smoke")]
            cancel_after_batches: None,
            #[cfg(feature = "native-smoke")]
            checkpoint_owner: Rc::new(()),
            #[cfg(feature = "native-smoke")]
            checkpoints: std::array::from_fn(|_| None),
            #[cfg(feature = "native-smoke")]
            checkpoint_serial: 0,
            context_size: context_size as usize,
            seed: u32::MAX as u64,
            work: NativeWork::default(),
            _thread: PhantomData,
        })
    }
    pub fn gpu_layers_requested(&self) -> i32 {
        unsafe { bt_llama_gpu_layers(self.owner.as_ptr()) }
    }
    #[cfg(feature = "native-smoke")]
    pub fn diagnostic_sampler_seed(&self) -> u64 {
        self.seed
    }
    /// Known seed of the sampler constructed by the latest generation attempt.
    #[cfg(feature = "native-smoke")]
    pub fn diagnostic_last_sampler_seed(&self) -> Option<u32> {
        self.last_sampler_seed
    }
    /// Exact one-shot seed for the next generation attempt. MAX requests native
    /// entropy rather than an exact seed, so rejection also retires any override.
    #[cfg(feature = "native-smoke")]
    pub fn diagnostic_set_next_seed(&mut self, seed: u32) -> Result<()> {
        self.next_seed = None;
        ensure!(
            seed != u32::MAX,
            "The native random-seed sentinel is not an exact diagnostic seed."
        );
        self.next_seed = Some(seed);
        Ok(())
    }
    /// Retire an override left unconsumed by pre-sampler cancellation or failure.
    #[cfg(feature = "native-smoke")]
    pub fn diagnostic_clear_next_seed(&mut self) {
        self.next_seed = None;
    }
    /// Per-owner grammar reference path for paired diagnostics only. Switching
    /// retires the native sampler and clears KV/logits and outstanding seed state,
    /// so no previous trajectory can supply sampler or prompt history.
    /// `false` restores the ordinary shortlist wrapper; production has no toggle.
    #[cfg(feature = "native-smoke")]
    pub fn diagnostic_set_full_vocabulary_grammar(&mut self, enabled: bool) {
        unsafe {
            bt_llama_diagnostic_full_vocabulary_grammar(self.owner.as_ptr(), enabled as c_int);
        }
        self.diagnostic_clear_cache();
        self.diagnostic_clear_next_seed();
        self.last_sampler_seed = None;
    }
    #[cfg(feature = "native-smoke")]
    pub fn diagnostic_set_logit_bias(&mut self, biases: &[(i32, f32)]) -> Result<()> {
        ensure!(biases.len() <= 64, "Diagnostic bias count exceeds 64.");
        let mut tokens = [0i32; 64];
        let mut values = [0f32; 64];
        for (index, &(token, bias)) in biases.iter().enumerate() {
            ensure!(
                token >= 0 && bias.is_finite() && bias.abs() <= 2.0,
                "Diagnostic bias is outside its bounds."
            );
            ensure!(
                !tokens[..index].contains(&token),
                "Duplicate diagnostic bias token."
            );
            tokens[index] = token;
            values[index] = bias;
        }
        let result = unsafe {
            bt_llama_diagnostic_bias(
                self.owner.as_ptr(),
                tokens.as_ptr(),
                values.as_ptr(),
                biases.len() as i32,
            )
        };
        ensure!(
            result == 0,
            "Diagnostic bias token is outside the vocabulary."
        );
        self.diagnostic_discard_checkpoints();
        Ok(())
    }
    /// Synchronizes only this context, not the GPU/device or other processes.
    #[cfg(feature = "native-smoke")]
    pub fn diagnostic_drain(&mut self) -> u64 {
        let started = Instant::now();
        unsafe { bt_llama_diagnostic_drain(self.owner.as_ptr()) };
        started.elapsed().as_micros().min(u64::MAX as u128) as u64
    }
    #[cfg(feature = "native-smoke")]
    pub fn diagnostic_checkpoint_size(&mut self) -> usize {
        unsafe { bt_llama_diagnostic_state_size(self.owner.as_ptr()) }
    }
    #[cfg(feature = "native-smoke")]
    pub fn diagnostic_checkpoint_bytes(&self) -> usize {
        self.checkpoints
            .iter()
            .flatten()
            .map(|saved| saved.state.len() + saved.tokens.len() * std::mem::size_of::<i32>())
            .sum()
    }
    #[cfg(feature = "native-smoke")]
    pub fn diagnostic_discard_checkpoints(&mut self) {
        self.checkpoints = std::array::from_fn(|_| None);
    }
    /// Diagnostic whole-memory snapshot: preserves occupied pre-save history,
    /// not already evicted cells or logits. Both slots' payload/token storage
    /// count toward the cap, checked before allocation. No production adoption.
    #[cfg(feature = "native-smoke")]
    pub fn diagnostic_save_checkpoint(
        &mut self,
        slot: usize,
        byte_cap: usize,
    ) -> Result<Option<DiagnosticCheckpoint>> {
        ensure!(
            slot < DIAGNOSTIC_CHECKPOINT_SLOTS,
            "Invalid diagnostic checkpoint slot."
        );
        ensure!(
            byte_cap <= DIAGNOSTIC_CHECKPOINT_BYTE_CAP,
            "Diagnostic checkpoint cap exceeds 256 MiB."
        );
        ensure!(
            !self.evaluated.is_empty() && self.logits_valid,
            "No evaluated prefix to save."
        );
        let size = self.diagnostic_checkpoint_size();
        let token_bytes = self
            .evaluated
            .len()
            .checked_mul(std::mem::size_of::<i32>())
            .context("Diagnostic checkpoint token size overflow.")?;
        let required = size
            .checked_add(token_bytes)
            .context("Diagnostic checkpoint size overflow.")?;
        let previous = self.checkpoints[slot].as_ref().map_or(0, |saved| {
            saved.state.len() + saved.tokens.len() * std::mem::size_of::<i32>()
        });
        if size == 0
            || required > byte_cap.saturating_sub(self.diagnostic_checkpoint_bytes() - previous)
        {
            return Ok(None);
        }
        let serial = self
            .checkpoint_serial
            .checked_add(1)
            .context("Diagnostic checkpoint identity exhausted.")?;
        // Release the replaced slot before allocating, preserving the total cap.
        self.checkpoints[slot] = None;
        let mut bytes = zeroize::Zeroizing::new(Vec::new());
        bytes.try_reserve_exact(size)?;
        bytes.resize(size, 0);
        let written = unsafe {
            bt_llama_diagnostic_state_save(self.owner.as_ptr(), bytes.as_mut_ptr(), size)
        };
        ensure!(written == size, "Native full checkpoint save failed.");
        let digest = Sha256::digest(bytes.as_slice()).into();
        self.checkpoints[slot] = Some(SavedCheckpoint {
            state: bytes,
            tokens: zeroize::Zeroizing::new(self.evaluated.clone()),
            serial,
            digest,
        });
        self.checkpoint_serial = serial;
        Ok(Some(DiagnosticCheckpoint {
            owner: self.checkpoint_owner.clone(),
            slot,
            serial,
        }))
    }
    /// Restore only the exact saved prefix on its original model/policy owner.
    /// The pinned whole-state API does not serialize logits: evaluate must replay
    /// the final token, or fully reset if native suffix removal is unsupported.
    #[cfg(feature = "native-smoke")]
    pub fn diagnostic_restore_checkpoint(
        &mut self,
        checkpoint: &DiagnosticCheckpoint,
        tokens: &[i32],
    ) -> Result<bool> {
        let saved = self
            .checkpoints
            .get(checkpoint.slot)
            .and_then(Option::as_ref);
        if !Rc::ptr_eq(&self.checkpoint_owner, &checkpoint.owner)
            || !saved.is_some_and(|saved| {
                saved.serial == checkpoint.serial
                    && saved.tokens.as_slice() == tokens
                    && Sha256::digest(saved.state.as_slice())[..] == saved.digest[..]
            })
        {
            self.reset();
            self.diagnostic_discard_checkpoints();
            return Ok(false);
        }
        let saved = saved.expect("validated diagnostic checkpoint");
        self.logits_valid = false;
        self.evaluated.clear();
        let read = unsafe {
            bt_llama_diagnostic_state_restore(
                self.owner.as_ptr(),
                saved.state.as_ptr(),
                saved.state.len(),
            )
        };
        if read != saved.state.len() {
            self.reset();
            self.diagnostic_discard_checkpoints();
            return Ok(false);
        }
        self.evaluated.extend_from_slice(&saved.tokens);
        Ok(true)
    }
    /// Fault injection for the real harness: corrupt owned bytes without
    /// exposing payloads or passing unchecked corruption into native parsing.
    #[cfg(feature = "native-smoke")]
    pub fn diagnostic_corrupt_checkpoint(
        &mut self,
        checkpoint: &DiagnosticCheckpoint,
    ) -> Result<()> {
        ensure!(
            Rc::ptr_eq(&self.checkpoint_owner, &checkpoint.owner),
            "Foreign diagnostic checkpoint."
        );
        let saved = self
            .checkpoints
            .get_mut(checkpoint.slot)
            .and_then(Option::as_mut)
            .filter(|saved| saved.serial == checkpoint.serial)
            .context("Stale diagnostic checkpoint.")?;
        let first = saved
            .state
            .first_mut()
            .context("Empty diagnostic checkpoint.")?;
        *first ^= 1;
        Ok(())
    }
    #[cfg(feature = "native-smoke")]
    pub fn diagnostic_logits(&self) -> Result<Vec<f32>> {
        ensure!(self.logits_valid, "No evaluated logits are available.");
        let count =
            unsafe { bt_llama_diagnostic_logits(self.owner.as_ptr(), std::ptr::null_mut(), 0) };
        ensure!(count > 0, "Invalid native vocabulary size.");
        let mut logits = Vec::with_capacity(count as usize);
        let copied =
            unsafe { bt_llama_diagnostic_logits(self.owner.as_ptr(), logits.as_mut_ptr(), count) };
        ensure!(copied == count, "Native diagnostic logits are unavailable.");
        // The bridge initialized exactly count floats in caller-owned storage.
        unsafe { logits.set_len(count as usize) };
        Ok(logits)
    }
    #[cfg(feature = "native-smoke")]
    pub fn diagnostic_cache_state(&self) -> (i32, i32, i32) {
        let (mut minimum, mut maximum) = (-1, -1);
        let window =
            unsafe { bt_llama_diagnostic_cache(self.owner.as_ptr(), &mut minimum, &mut maximum) };
        (window, minimum, maximum)
    }
    #[cfg(feature = "native-smoke")]
    pub fn diagnostic_clear_cache(&mut self) {
        self.reset();
        self.diagnostic_discard_checkpoints();
    }
    #[cfg(feature = "native-smoke")]
    pub fn diagnostic_cancel_after_trim(&mut self) {
        self.cancel_after_trim = true;
    }
    /// Deterministic cancellation after real submitted/completed decode work.
    #[cfg(feature = "native-smoke")]
    pub fn diagnostic_cancel_after_decode_batches(&mut self, batches: usize) -> Result<()> {
        ensure!(
            batches > 0,
            "Diagnostic cancellation needs a positive batch count."
        );
        self.cancel_after_batches = Some(batches);
        Ok(())
    }
    pub fn evaluated_tokens(&self) -> usize {
        self.evaluated.len()
    }
    pub fn work(&self) -> NativeWork {
        self.work
    }
    fn reset(&mut self) {
        unsafe {
            bt_llama_reset(self.owner.as_ptr());
        }
        self.evaluated.clear();
        self.logits_valid = false;
        self.work.cache_resets += 1;
    }
    fn decode(&mut self, tokens: &[i32], cancel: &AtomicBool, prompt: bool) -> Result<bool> {
        for batch in tokens.chunks(64) {
            if cancel.load(Ordering::Acquire) {
                return Ok(false);
            }
            let work = if prompt {
                &mut self.work.prompt
            } else {
                &mut self.work.generation
            };
            work.submitted_tokens += batch.len() as u64;
            work.submitted_batches += 1;
            let started = Instant::now();
            let result = unsafe {
                bt_llama_decode(
                    self.owner.as_ptr(),
                    batch.as_ptr(),
                    batch.len() as i32,
                    self.evaluated.len() as i32,
                )
            };
            work.elapsed_us += started.elapsed().as_micros().min(u64::MAX as u128) as u64;
            if result != 0 {
                self.reset();
                bail!("Native model evaluation failed ({result}).");
            }
            work.completed_tokens += batch.len() as u64;
            work.completed_batches += 1;
            self.evaluated.extend_from_slice(batch);
            self.logits_valid = true;
            #[cfg(feature = "native-smoke")]
            if let Some(remaining) = self.cancel_after_batches.as_mut() {
                *remaining -= 1;
                if *remaining == 0 {
                    self.cancel_after_batches = None;
                    cancel.store(true, Ordering::Release);
                }
            }
            if cancel.load(Ordering::Acquire) {
                return Ok(false);
            }
        }
        Ok(true)
    }
    pub fn evaluate(&mut self, tokens: &[i32], cancel: &AtomicBool) -> Result<bool> {
        self.work = NativeWork {
            evaluations: 1,
            input_tokens: tokens.len() as u64,
            ..NativeWork::default()
        };
        ensure!(
            !tokens.is_empty() && tokens.len() < self.context_size,
            "The model prompt exceeds its context window."
        );
        if cancel.load(Ordering::Acquire) {
            return Ok(false);
        }
        let exact = self.logits_valid && self.evaluated == tokens;
        if exact {
            self.work.reused_tokens = tokens.len() as u64;
            self.work.exact_reuses = 1;
            return Ok(true);
        } // Current logits belong to a live decode.
        let common = self
            .evaluated
            .iter()
            .zip(tokens)
            .take_while(|(a, b)| a == b)
            .count();
        // When the new prompt is consumed, replay its final token to refresh
        // logits after truncation. Recurrent/hybrid models can refuse partial
        // removal; then perform a genuine full evaluation, never reuse stale KV.
        let reuse = if common == tokens.len() {
            common.saturating_sub(1)
        } else {
            common
        };
        if self.evaluated.is_empty() {
            self.work.cold_evaluations = 1;
        } else if reuse == 0 {
            self.work.prefix_misses = 1;
        }
        if reuse > 0 {
            if unsafe { bt_llama_trim(self.owner.as_ptr(), reuse as i32) } != 0 {
                self.logits_valid &= reuse == self.evaluated.len();
                self.evaluated.truncate(reuse);
                self.work.prefix_reuses = 1;
            } else {
                self.work.trim_failures = 1;
                self.reset();
            }
        } else {
            self.reset();
        }
        let start = self.evaluated.len();
        self.work.reused_tokens = start as u64;
        #[cfg(feature = "native-smoke")]
        if self.cancel_after_trim && start > 0 {
            self.cancel_after_trim = false;
            cancel.store(true, Ordering::Release);
        }
        self.decode(&tokens[start..], cancel, true)
    }
    fn piece(&self, token: i32, bytes: &mut Vec<u8>) -> Result<()> {
        bytes.resize(128, 0);
        let mut count = unsafe {
            bt_llama_piece(
                self.owner.as_ptr(),
                token,
                bytes.as_mut_ptr().cast(),
                bytes.len() as i32,
            )
        };
        if count < 0 {
            let size = count
                .checked_neg()
                .context("Native token piece overflow.")? as usize;
            ensure!(
                size <= 1024 * 1024,
                "Native token piece exceeds its byte limit."
            );
            bytes.resize(size, 0);
            count = unsafe {
                bt_llama_piece(
                    self.owner.as_ptr(),
                    token,
                    bytes.as_mut_ptr().cast(),
                    size as i32,
                )
            };
        }
        ensure!(
            count >= 0 && count as usize <= bytes.len(),
            "Native token decoding failed."
        );
        bytes.truncate(count as usize);
        Ok(())
    }
    /// Callback receives UTF-8 deltas and one terminal reason. Returning false
    /// stops native consumption immediately, releasing the request's sampler.
    pub fn generate(
        &mut self,
        tokens: &[i32],
        max_tokens: usize,
        sampler: SamplerOptions,
        grammar: Option<&str>,
        stops: &[&str],
        cancel: &AtomicBool,
        emit: &mut dyn FnMut(&str, Option<FinishReason>) -> Result<bool>,
    ) -> Result<()> {
        self.work = NativeWork::default();
        #[cfg(feature = "native-smoke")]
        let next_seed = {
            self.last_sampler_seed = None;
            self.next_seed.take()
        };
        if cancel.load(Ordering::Acquire) {
            return Ok(());
        }
        ensure!(
            max_tokens > 0
                && tokens
                    .len()
                    .checked_add(max_tokens)
                    .is_some_and(|n| n <= self.context_size),
            "The requested generation exceeds the model context window."
        );
        let grammar = grammar
            .map(CString::new)
            .transpose()
            .context("Native grammar contains a NUL byte.")?;
        if !self.evaluate(tokens, cancel)? {
            return Ok(());
        }
        #[cfg(feature = "native-smoke")]
        {
            self.seed = next_seed.map_or_else(|| next_python_seed(self.seed), u64::from);
        }
        #[cfg(not(feature = "native-smoke"))]
        {
            self.seed = next_python_seed(self.seed);
        }
        let result = unsafe {
            bt_llama_sampler(
                self.owner.as_ptr(),
                sampler.top_k,
                sampler.top_p,
                sampler.min_p,
                sampler.temperature,
                sampler.repeat_penalty,
                self.seed as u32,
                grammar.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            )
        };
        ensure!(
            result == 0,
            "The native engine could not construct the writing grammar or sampler."
        );
        #[cfg(feature = "native-smoke")]
        {
            let seed = self.seed as u32;
            self.last_sampler_seed = (seed != u32::MAX).then_some(seed);
        }
        let outcome = (|| {
            let mut fence = ByteStream::new(stops);
            let mut bytes = Vec::with_capacity(128);
            for index in 0..max_tokens {
                if cancel.load(Ordering::Acquire) {
                    return Ok(());
                }
                let token = unsafe { bt_llama_sample(self.owner.as_ptr()) };
                ensure!(token >= 0, "Native sampling did not select a token.");
                self.work.sampled_tokens += 1;
                if cancel.load(Ordering::Acquire) {
                    return Ok(());
                }
                if unsafe { bt_llama_is_eog(self.owner.as_ptr(), token) } != 0 {
                    let (text, _) = fence.push(&[], Some(FinishReason::Stop))?;
                    emit(&text, Some(FinishReason::Stop))?;
                    return Ok(());
                }
                self.piece(token, &mut bytes)?;
                let terminal = (index + 1 == max_tokens).then_some(FinishReason::Length);
                let (text, stopped) = fence.push(&bytes, terminal)?;
                let reason = if stopped {
                    Some(FinishReason::Stop)
                } else {
                    terminal
                };
                if (!text.is_empty() || reason.is_some()) && !emit(&text, reason)? {
                    return Ok(());
                }
                if stopped || terminal.is_some() {
                    return Ok(());
                }
                if !self.decode(&[token], cancel, false)? {
                    return Ok(());
                }
            }
            Ok(())
        })();
        unsafe {
            bt_llama_sampler_close(self.owner.as_ptr());
        }
        outcome
    }
}
impl Tokenizer for NativeModel {
    fn tokenize(&self, text: &str, add_bos: bool, special: bool) -> Result<Vec<i32>> {
        let length = i32::try_from(text.len())
            .context("Model input exceeds the native tokenization limit.")?;
        let capacity = text
            .len()
            .checked_add(8)
            .context("Model input length overflow.")?;
        let mut tokens = vec![0i32; capacity.min(i32::MAX as usize)];
        let call = |tokens: &mut Vec<i32>| unsafe {
            bt_llama_tokenize(
                self.owner.as_ptr(),
                text.as_ptr().cast(),
                length,
                tokens.as_mut_ptr(),
                tokens.len() as i32,
                add_bos as c_int,
                special as c_int,
            )
        };
        let mut count = call(&mut tokens);
        if count < 0 {
            let needed = count
                .checked_neg()
                .context("Native tokenization overflow.")? as usize;
            tokens.resize(needed, 0);
            count = call(&mut tokens);
        }
        ensure!(
            count >= 0 && count as usize <= tokens.len(),
            "The native engine could not tokenize the model input."
        );
        tokens.truncate(count as usize);
        Ok(tokens)
    }
    fn metadata(&self, key: &str) -> Result<String> {
        let key = CString::new(key)?;
        let mut bytes = vec![0u8; 1024];
        let mut count = unsafe {
            bt_llama_meta(
                self.owner.as_ptr(),
                key.as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if count < 0 {
            return Ok(String::new());
        }
        if count as usize >= bytes.len() {
            ensure!(
                count <= 1024 * 1024,
                "The model metadata exceeds its byte limit."
            );
            bytes.resize(count as usize + 1, 0);
            count = unsafe {
                bt_llama_meta(
                    self.owner.as_ptr(),
                    key.as_ptr(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
        }
        ensure!(
            count >= 0 && (count as usize) < bytes.len(),
            "Native model metadata is invalid."
        );
        String::from_utf8(bytes[..count as usize].to_vec())
            .context("The model metadata is not valid UTF-8.")
    }
    fn context_size(&self) -> usize {
        self.context_size
    }
}
impl Drop for NativeModel {
    fn drop(&mut self) {
        unsafe {
            bt_llama_close(self.owner.as_ptr());
        }
    }
}

/// Byte-level stops precede UTF-8 decoding: token pieces need not be scalars.
/// Hold proper prefixes across arbitrary boundaries; discard incomplete UTF-8
/// only at a token-limit cut, never invent replacement characters.
struct ByteStream {
    pending: Vec<u8>,
    utf8: Vec<u8>,
    stops: Vec<Vec<u8>>,
}
impl ByteStream {
    fn new(stops: &[&str]) -> Self {
        Self {
            pending: Vec::new(),
            utf8: Vec::new(),
            stops: stops.iter().map(|s| s.as_bytes().to_vec()).collect(),
        }
    }
    fn push(&mut self, bytes: &[u8], terminal: Option<FinishReason>) -> Result<(String, bool)> {
        self.pending.extend_from_slice(bytes);
        let end = self
            .stops
            .iter()
            .filter_map(|stop| self.pending.windows(stop.len()).position(|w| w == stop))
            .min();
        let stopped = end.is_some();
        let mut held = 0;
        if !stopped && terminal.is_none() {
            for stop in &self.stops {
                for length in 1..stop.len().min(self.pending.len() + 1) {
                    if self.pending.ends_with(&stop[..length]) {
                        held = held.max(length);
                    }
                }
            }
        }
        let visible = end.unwrap_or(self.pending.len() - held);
        self.utf8.extend_from_slice(&self.pending[..visible]);
        self.pending.drain(..visible);
        if stopped || terminal.is_some() {
            self.pending.clear();
        }
        let text = match std::str::from_utf8(&self.utf8) {
            Ok(text) => {
                let text = text.to_owned();
                self.utf8.clear();
                text
            }
            Err(error) => {
                ensure!(
                    error.error_len().is_none(),
                    "The native engine emitted invalid UTF-8."
                );
                ensure!(
                    terminal != Some(FinishReason::Stop) && !stopped,
                    "The native engine ended inside a UTF-8 character."
                );
                let valid = error.valid_up_to();
                let text = std::str::from_utf8(&self.utf8[..valid])?.to_owned();
                self.utf8.drain(..valid);
                if terminal == Some(FinishReason::Length) {
                    self.utf8.clear();
                }
                text
            }
        };
        Ok((text, stopped))
    }
}

// CPython Random(integer).randint(0, 2**32), used by the immutable oracle at
// each request. This preserves default seed evolution without Python or a new
// nondeterministic RNG, including the rare inclusive upper bound (2**32).
fn next_python_seed(seed: u64) -> u64 {
    let mut mt = [0u32; 624];
    mt[0] = 19650218;
    for i in 1..624 {
        mt[i] = 1812433253u32
            .wrapping_mul(mt[i - 1] ^ (mt[i - 1] >> 30))
            .wrapping_add(i as u32);
    }
    let key = [seed as u32, (seed >> 32) as u32];
    let count = if seed > u32::MAX as u64 { 2 } else { 1 };
    let (mut i, mut j) = (1usize, 0usize);
    for _ in 0..624 {
        mt[i] = (mt[i] ^ (mt[i - 1] ^ (mt[i - 1] >> 30)).wrapping_mul(1664525))
            .wrapping_add(key[j])
            .wrapping_add(j as u32);
        i += 1;
        j += 1;
        if i >= 624 {
            mt[0] = mt[623];
            i = 1;
        }
        if j >= count {
            j = 0;
        }
    }
    for _ in 0..623 {
        mt[i] = (mt[i] ^ (mt[i - 1] ^ (mt[i - 1] >> 30)).wrapping_mul(1566083941))
            .wrapping_sub(i as u32);
        i += 1;
        if i >= 624 {
            mt[0] = mt[623];
            i = 1;
        }
    }
    mt[0] = 0x80000000;
    let mut cursor = 624;
    let mut next = || {
        if cursor >= 624 {
            for index in 0..624 {
                let y = (mt[index] & 0x80000000) | (mt[(index + 1) % 624] & 0x7fffffff);
                mt[index] =
                    mt[(index + 397) % 624] ^ (y >> 1) ^ if y & 1 != 0 { 0x9908b0df } else { 0 };
            }
            cursor = 0;
        }
        let mut y = mt[cursor];
        cursor += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c5680;
        y ^= (y << 15) & 0xefc60000;
        y ^= y >> 18;
        y
    };
    loop {
        let low = next() as u64;
        let high = (next() >> 31) as u64;
        let value = low | (high << 32);
        if value <= 0x100000000 {
            return value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "native-smoke")]
    #[test]
    fn constrained_shortlist_preserves_sampling_distribution_and_state() {
        unsafe extern "C" {
            fn bt_grammar_sampler_regression() -> c_int;
        }
        let failed_line = unsafe { bt_grammar_sampler_regression() };
        assert_eq!(
            failed_line, 0,
            "native sampler regression at C line {failed_line}"
        );
    }

    #[test]
    fn bytes_assemble_multibyte_and_stop_at_every_boundary() {
        let wire = " ready 月🚀<turn|>private".as_bytes();
        for split in 0..=wire.len() {
            let mut state = ByteStream::new(&["<turn|>"]);
            let (a, stopped) = state.push(&wire[..split], None).unwrap();
            let b = if stopped {
                String::new()
            } else {
                state
                    .push(&wire[split..], Some(FinishReason::Stop))
                    .unwrap()
                    .0
            };
            assert_eq!(a + &b, " ready 月🚀");
        }
    }
    #[test]
    fn malformed_utf8_is_not_replaced_or_published() {
        assert!(ByteStream::new(&[]).push(&[0xff], None).is_err());
        assert_eq!(
            ByteStream::new(&[])
                .push(&[b'a', 0xe6], Some(FinishReason::Length))
                .unwrap()
                .0,
            "a"
        );
        assert!(
            ByteStream::new(&[])
                .push(&[0xe6], Some(FinishReason::Stop))
                .is_err()
        );
    }
    #[test]
    fn family_sampler_defaults_match_source() {
        assert_eq!(
            SamplerOptions::for_family(Some("gemma4-moe"), false),
            SamplerOptions {
                top_k: 64,
                top_p: 0.95,
                min_p: 0.0,
                temperature: 0.2,
                repeat_penalty: 1.0
            }
        );
        assert_eq!(SamplerOptions::for_family(Some("qwen3"), true).top_k, 20);
        assert_eq!(SamplerOptions::for_family(None, false).repeat_penalty, 1.1);
    }
    #[test]
    fn inclusive_integer_seed_matches_immutable_cpython_fixtures() {
        for (seed, expected) in [
            (0, 3626764237),
            (1, 3280387012),
            (4294967295, 872737089),
            (4294967296, 485306839),
        ] {
            assert_eq!(next_python_seed(seed), expected);
        }
    }
}
