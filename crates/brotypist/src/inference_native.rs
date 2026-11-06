//! Safe, thread-confined owner of the pinned llama.cpp C ABI.
//! No Rust layout assumptions about upstream structs and no Python runtime.
use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::{CString, c_char, c_int, c_void},
    marker::PhantomData,
    path::Path,
    ptr::NonNull,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
};

pub const LLAMA_REVISION: &str = "0c1e57098bba43ac29e6e3b677cdceebdd22334f";
// Shared with the opaque native owner at creation; cancellation remains bounded
// by one prefill batch and generated tokens still decode individually.
const PREFILL_BATCH_TOKENS: usize = 128;
unsafe extern "C" {
    fn bt_llama_open(
        path: *const c_char,
        auto_gpu: c_int,
        threads: c_int,
        batch_capacity: c_int,
        error: *mut c_char,
        cap: usize,
        #[cfg(feature = "native-smoke")] preparation: Option<extern "C" fn(c_int, c_int)>,
    ) -> *mut c_void;
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
    fn bt_llama_decode(
        owner: *mut c_void,
        tokens: *const i32,
        count: i32,
        position: i32,
        final_logits: c_int,
    ) -> c_int;
    #[cfg(feature = "native-smoke")]
    fn bt_llama_synchronize(owner: *mut c_void);
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

/// Opt-in native-smoke counters. Timings use synchronized monotonic wall clocks,
/// not CPU time: decode includes backend completion, sampling starts after it.
/// Counts include only successful decodes, and never contain prompt/token text.
#[cfg(feature = "native-smoke")]
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct NativeDiagnostics {
    pub prompt_tokens: usize,
    pub reused_prompt_tokens: usize,
    pub evaluated_prompt_tokens: usize,
    pub prefill_decode_batches: usize,
    pub prefill_output_rows: usize,
    pub generation_evaluated_tokens: usize,
    pub generation_decode_batches: usize,
    pub generation_output_rows: usize,
    pub sample_calls: usize,
    pub prefill_decode_wall_ms: f64,
    pub generation_decode_wall_ms: f64,
    pub sampling_wall_ms: f64,
}

pub struct NativeModel {
    owner: NonNull<c_void>,
    evaluated: Vec<i32>,
    // A cached prefix alone is insufficient: an interrupted no-output prefill
    // has KV entries but no logits from which it is safe to sample.
    current_logits_valid: bool,
    #[cfg(feature = "native-smoke")]
    diagnostics: Option<NativeDiagnostics>,
    #[cfg(all(test, feature = "native-smoke"))]
    cancel_after_decode_batches: Option<usize>,
    context_size: usize,
    seed: u64,
    // The foreign model/context/sampler must never move to another thread.
    _thread: PhantomData<Rc<()>>,
}
impl NativeModel {
    pub fn open(path: &Path, acceleration: &str) -> Result<Self> {
        #[cfg(feature = "native-smoke")]
        let _span = crate::runtime::stage_diagnostics::preparation_span(
            crate::runtime::stage_diagnostics::Stage::NativeModelOpen,
        );
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
            bt_llama_open(
                path.as_ptr(),
                (acceleration == "auto") as c_int,
                threads,
                PREFILL_BATCH_TOKENS as c_int,
                error.as_mut_ptr().cast(),
                error.len(),
                #[cfg(feature = "native-smoke")]
                crate::runtime::stage_diagnostics::preparation_active()
                    .then_some(crate::runtime::stage_diagnostics::native_open_phase),
            )
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
            current_logits_valid: false,
            #[cfg(feature = "native-smoke")]
            diagnostics: None,
            #[cfg(all(test, feature = "native-smoke"))]
            cancel_after_decode_batches: None,
            context_size: context_size as usize,
            seed: u32::MAX as u64,
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
    #[cfg(feature = "native-smoke")]
    pub fn diagnostic_reset_sampler_seed(&mut self, seed: u64) {
        self.seed = seed;
    }
    #[cfg(feature = "native-smoke")]
    pub fn enable_diagnostics(&mut self) {
        self.diagnostics = Some(NativeDiagnostics::default());
    }
    #[cfg(feature = "native-smoke")]
    pub fn diagnostics(&self) -> Option<NativeDiagnostics> {
        self.diagnostics
    }
    #[cfg(feature = "native-smoke")]
    fn diagnostic_clock(&self) -> Option<std::time::Instant> {
        self.diagnostics.as_ref().map(|_| {
            // Finish previously queued backend work before starting this clock.
            unsafe { bt_llama_synchronize(self.owner.as_ptr()) };
            std::time::Instant::now()
        })
    }
    pub fn evaluated_tokens(&self) -> usize {
        self.evaluated.len()
    }
    fn reset(&mut self) {
        unsafe {
            bt_llama_reset(self.owner.as_ptr());
        }
        self.evaluated.clear();
        self.current_logits_valid = false;
    }
    fn decode(&mut self, tokens: &[i32], cancel: &AtomicBool, _prefill: bool) -> Result<bool> {
        for (index, batch) in tokens.chunks(PREFILL_BATCH_TOKENS).enumerate() {
            if cancel.load(Ordering::Acquire) {
                return Ok(false);
            }
            let final_logits = index == (tokens.len() - 1) / PREFILL_BATCH_TOKENS;
            self.current_logits_valid = false;
            #[cfg(feature = "native-smoke")]
            let started = self.diagnostic_clock();
            let result = unsafe {
                bt_llama_decode(
                    self.owner.as_ptr(),
                    batch.as_ptr(),
                    batch.len() as i32,
                    self.evaluated.len() as i32,
                    final_logits as c_int,
                )
            };
            #[cfg(feature = "native-smoke")]
            let elapsed = started.map(|started| {
                unsafe { bt_llama_synchronize(self.owner.as_ptr()) };
                started.elapsed().as_secs_f64() * 1000.0
            });
            if result != 0 {
                self.reset();
                bail!("Native model evaluation failed ({result}).");
            }
            self.evaluated.extend_from_slice(batch);
            self.current_logits_valid = final_logits;
            #[cfg(feature = "native-smoke")]
            if let (Some(metrics), Some(elapsed)) = (&mut self.diagnostics, elapsed) {
                if _prefill {
                    metrics.evaluated_prompt_tokens += batch.len();
                    metrics.prefill_decode_batches += 1;
                    metrics.prefill_output_rows += usize::from(final_logits);
                    metrics.prefill_decode_wall_ms += elapsed;
                } else {
                    metrics.generation_evaluated_tokens += batch.len();
                    metrics.generation_decode_batches += 1;
                    metrics.generation_output_rows += usize::from(final_logits);
                    metrics.generation_decode_wall_ms += elapsed;
                }
            }
            // Deterministic native regression hook: cancellation happens at the
            // real post-decode boundary, not in a simulated cache implementation.
            #[cfg(all(test, feature = "native-smoke"))]
            if let Some(remaining) = &mut self.cancel_after_decode_batches {
                *remaining -= 1;
                if *remaining == 0 {
                    self.cancel_after_decode_batches = None;
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
        #[cfg(feature = "native-smoke")]
        if let Some(metrics) = &mut self.diagnostics {
            *metrics = NativeDiagnostics {
                prompt_tokens: tokens.len(),
                ..NativeDiagnostics::default()
            };
        }
        ensure!(
            !tokens.is_empty() && tokens.len() < self.context_size,
            "The model prompt exceeds its context window."
        );
        if cancel.load(Ordering::Acquire) {
            return Ok(false);
        }
        let exact = self.evaluated == tokens;
        if exact && self.current_logits_valid {
            #[cfg(feature = "native-smoke")]
            if let Some(metrics) = &mut self.diagnostics {
                metrics.reused_prompt_tokens = tokens.len();
            }
            return Ok(true);
        }
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
        self.current_logits_valid = false;
        if reuse > 0 && unsafe { bt_llama_trim(self.owner.as_ptr(), reuse as i32) } != 0 {
            self.evaluated.truncate(reuse);
        } else {
            self.reset();
        }
        let start = self.evaluated.len();
        #[cfg(feature = "native-smoke")]
        if let Some(metrics) = &mut self.diagnostics {
            metrics.reused_prompt_tokens = start;
        }
        let result = self.decode(&tokens[start..], cancel, true);
        #[cfg(feature = "native-smoke")]
        if crate::runtime::stage_diagnostics::preparation_active() {
            // Preparation attribution must include actual backend completion,
            // not merely queued work. Never synchronize normal generation here.
            unsafe { bt_llama_synchronize(self.owner.as_ptr()) };
        }
        result
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
        #[cfg(feature = "native-smoke")]
        if let Some(metrics) = &mut self.diagnostics {
            *metrics = NativeDiagnostics::default();
        }
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
        self.seed = next_python_seed(self.seed);
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
        let outcome = (|| {
            let mut fence = ByteStream::new(stops);
            let mut bytes = Vec::with_capacity(128);
            for index in 0..max_tokens {
                if cancel.load(Ordering::Acquire) {
                    return Ok(());
                }
                ensure!(
                    self.current_logits_valid,
                    "Native sampling requires current logits."
                );
                #[cfg(feature = "native-smoke")]
                let started = self.diagnostic_clock();
                let token = unsafe { bt_llama_sample(self.owner.as_ptr()) };
                #[cfg(feature = "native-smoke")]
                if let Some(started) = started {
                    unsafe { bt_llama_synchronize(self.owner.as_ptr()) };
                    if let Some(metrics) = &mut self.diagnostics {
                        metrics.sample_calls += 1;
                        metrics.sampling_wall_ms += started.elapsed().as_secs_f64() * 1000.0;
                    }
                }
                ensure!(token >= 0, "Native sampling did not select a token.");
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

    /// Explicitly opt in with BROTYPIST_NATIVE_SMOKE_MODEL=/existing/model.gguf.
    /// Uses the real pinned backend and canonical generation, without downloads
    /// or application/profile writes. Run serially; backend lifetime is global.
    #[cfg(feature = "native-smoke")]
    #[test]
    #[ignore = "requires an explicitly supplied existing GGUF model"]
    fn native_cache_logits_and_cancelled_prefill_recovery() -> Result<()> {
        let path = std::env::var_os("BROTYPIST_NATIVE_SMOKE_MODEL")
            .context("Set BROTYPIST_NATIVE_SMOKE_MODEL to an existing GGUF model")?;
        let acceleration =
            std::env::var("BROTYPIST_NATIVE_SMOKE_ACCELERATION").unwrap_or_else(|_| "cpu".into());
        ensure!(matches!(acceleration.as_str(), "cpu" | "auto"));
        let mut model = NativeModel::open(Path::new(&path), &acceleration)?;
        model.enable_diagnostics();
        let cancel = AtomicBool::new(false);
        let mut tokens = model.tokenize(
            &"The project report records progress and the next planned task. ".repeat(96),
            true,
            true,
        )?;
        let multi_batch_tokens = 3 * PREFILL_BATCH_TOKENS + 1;
        ensure!(
            tokens.len() >= multi_batch_tokens && model.context_size() > multi_batch_tokens + 1
        );
        tokens.truncate(multi_batch_tokens);

        // Multi-batch prefill produces exactly one output row, not one per
        // intermediate chunk. A live exact prefix can skip all evaluation.
        assert!(!model.current_logits_valid);
        assert!(model.evaluate(&tokens, &cancel)?);
        let metrics = model.diagnostics().unwrap();
        assert_eq!(metrics.reused_prompt_tokens, 0);
        assert_eq!(metrics.evaluated_prompt_tokens, tokens.len());
        assert_eq!(metrics.prefill_output_rows, 1);
        assert!(model.current_logits_valid);
        // Independent diagnostic cases must retain the resident prompt/logits.
        model.diagnostic_reset_sampler_seed(7);
        assert!(model.evaluate(&tokens, &cancel)?);
        let metrics = model.diagnostics().unwrap();
        assert_eq!(metrics.reused_prompt_tokens, tokens.len());
        assert_eq!(metrics.evaluated_prompt_tokens, 0);
        assert_eq!(metrics.prefill_output_rows, 0);

        // Shortening a live cache must refresh the last token's logits.
        assert!(model.evaluate(&tokens[..128], &cancel)?);
        let metrics = model.diagnostics().unwrap();
        assert!(matches!(metrics.reused_prompt_tokens, 0 | 127));
        assert_eq!(
            metrics.evaluated_prompt_tokens,
            128 - metrics.reused_prompt_tokens
        );
        assert_eq!(metrics.prefill_output_rows, 1);
        assert!(model.current_logits_valid);

        // Generated single-token decodes still produce usable logits.
        assert!(model.decode(&tokens[128..129], &cancel, false)?);
        let metrics = model.diagnostics().unwrap();
        assert_eq!(metrics.generation_evaluated_tokens, 1);
        assert_eq!(metrics.generation_decode_batches, 1);
        assert_eq!(metrics.generation_output_rows, 1);
        assert!(model.current_logits_valid);

        fn completion(model: &mut NativeModel, tokens: &[i32]) -> Result<String> {
            let mut text = String::new();
            let mut sampler = SamplerOptions::for_family(None, false);
            sampler.temperature = 0.0;
            model.generate(
                tokens,
                1,
                sampler,
                None,
                &[],
                &AtomicBool::new(false),
                &mut |delta, _| {
                    text.push_str(delta);
                    Ok(true)
                },
            )?;
            assert_eq!(model.diagnostics().unwrap().sample_calls, 1);
            Ok(text)
        }

        // Exercise both intermediate chunk boundaries. The successor is
        // exactly the cached KV prefix: equality must NOT skip final replay.
        for interrupted_batches in [1, 2] {
            model.reset();
            assert!(!model.current_logits_valid);
            model.cancel_after_decode_batches = Some(interrupted_batches);
            assert!(!model.evaluate(&tokens, &cancel)?);
            let prefix_len = interrupted_batches * PREFILL_BATCH_TOKENS;
            assert_eq!(model.evaluated_tokens(), prefix_len);
            assert!(!model.current_logits_valid);
            let metrics = model.diagnostics().unwrap();
            assert_eq!(metrics.evaluated_prompt_tokens, prefix_len);
            assert_eq!(metrics.prefill_decode_batches, interrupted_batches);
            assert_eq!(metrics.prefill_output_rows, 0);

            // An already cancelled request cannot repair or consume logits.
            assert!(!model.evaluate(&tokens[..prefix_len], &cancel)?);
            assert!(!model.current_logits_valid);
            assert_eq!(model.diagnostics().unwrap().prefill_decode_batches, 0);
            cancel.store(false, Ordering::Release);
            let recovered = completion(&mut model, &tokens[..prefix_len])?;
            let metrics = model.diagnostics().unwrap();
            // A hybrid model may refuse partial trim; full replay is safe too.
            assert!(
                metrics.reused_prompt_tokens == prefix_len - 1 || metrics.reused_prompt_tokens == 0
            );
            assert_eq!(
                metrics.evaluated_prompt_tokens,
                prefix_len - metrics.reused_prompt_tokens
            );
            assert_eq!(metrics.prefill_output_rows, 1);
            assert!(model.current_logits_valid);
            assert!(model.evaluate(&tokens[..prefix_len], &cancel)?);
            assert_eq!(model.diagnostics().unwrap().evaluated_prompt_tokens, 0);

            model.reset();
            let cold = completion(&mut model, &tokens[..prefix_len])?;
            assert_eq!(
                recovered, cold,
                "recovered logits must match cold generation"
            );
        }

        // Cancelling after the FINAL chunk leaves valid logits, unlike an
        // intermediate chunk, and the exact successor should reuse them.
        model.reset();
        model.cancel_after_decode_batches = Some(tokens.len().div_ceil(PREFILL_BATCH_TOKENS));
        assert!(!model.evaluate(&tokens, &cancel)?);
        assert!(model.current_logits_valid);
        cancel.store(false, Ordering::Release);
        assert!(model.evaluate(&tokens, &cancel)?);
        assert_eq!(model.diagnostics().unwrap().evaluated_prompt_tokens, 0);

        // Force the bridge's real position-boundary error without asking the
        // backend to evaluate an invalid token. Errors must clear BOTH states.
        model.evaluated.resize(model.context_size(), tokens[0]);
        assert!(model.decode(&tokens[..1], &cancel, true).is_err());
        assert_eq!(model.evaluated_tokens(), 0);
        assert!(!model.current_logits_valid);
        assert!(model.evaluate(&tokens[..64], &cancel)?);
        assert_eq!(model.diagnostics().unwrap().evaluated_prompt_tokens, 64);
        assert!(model.current_logits_valid);
        Ok(())
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
