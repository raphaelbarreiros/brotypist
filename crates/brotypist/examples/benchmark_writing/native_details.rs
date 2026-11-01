//! Actual native metadata lookups and Darwin process high-water RSS.
use anyhow::{Context, Result, bail, ensure};
use brotypist::{inference_native::LLAMA_REVISION, providers::diagnostics::DiagnosticSession};
use serde_json::{Map, Value, json};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

pub fn peak_rss_bytes() -> Result<u64> {
    #[cfg(target_os = "macos")]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        // SAFETY: getrusage writes this exact ABI struct on success. Darwin
        // ru_maxrss is bytes; this is the direct child's process-lifetime peak.
        ensure!(
            unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } == 0,
            "Cannot measure direct child lifetime peak RSS"
        );
        let usage = unsafe { usage.assume_init() };
        return u64::try_from(usage.ru_maxrss).context("Darwin reported a negative RSS peak");
    }
    #[cfg(not(target_os = "macos"))]
    bail!("Direct benchmark RSS semantics require Darwin")
}
fn u32_le(reader: &mut File) -> Result<u32> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}
fn u64_le(reader: &mut File) -> Result<u64> {
    let mut bytes = [0; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}
fn skip(reader: &mut File, length: u64, end: u64) -> Result<()> {
    let target = reader
        .stream_position()?
        .checked_add(length)
        .context("GGUF metadata offset overflow")?;
    ensure!(target <= end, "Truncated GGUF metadata");
    reader.seek(SeekFrom::Start(target))?;
    Ok(())
}
fn width(kind: u32) -> Option<u64> {
    match kind {
        0 | 1 | 7 => Some(1),
        2 | 3 => Some(2),
        4 | 5 | 6 => Some(4),
        10 | 11 | 12 => Some(8),
        _ => None,
    }
}
fn skip_value(reader: &mut File, kind: u32, end: u64, depth: u8) -> Result<()> {
    ensure!(
        depth <= 8,
        "GGUF metadata nesting exceeds safe inspection bound"
    );
    if let Some(width) = width(kind) {
        return skip(reader, width, end);
    }
    match kind {
        8 => {
            let length = u64_le(reader)?;
            skip(reader, length, end)
        }
        9 => {
            let element = u32_le(reader)?;
            let count = u64_le(reader)?;
            if let Some(width) = width(element) {
                skip(
                    reader,
                    count
                        .checked_mul(width)
                        .context("GGUF metadata array overflow")?,
                    end,
                )
            } else {
                ensure!(
                    count <= 10_000_000,
                    "GGUF metadata array count exceeds safe inspection bound"
                );
                for _ in 0..count {
                    skip_value(reader, element, end, depth + 1)?;
                }
                Ok(())
            }
        }
        _ => bail!("Unknown GGUF metadata value type"),
    }
}
/// Enumerate only GGUF keys without loading tokenizer arrays or tensor bytes;
/// all returned values are read through the actual loaded NativeModel API.
pub fn model_details(path: &Path, model: &DiagnosticSession) -> Result<Value> {
    let mut file = File::open(path)?;
    let end = file.metadata()?.len();
    let mut magic = [0; 4];
    file.read_exact(&mut magic)?;
    ensure!(magic == *b"GGUF", "Native model metadata input is not GGUF");
    let version = u32_le(&mut file)?;
    ensure!(
        matches!(version, 2 | 3),
        "Unsupported GGUF metadata header version"
    );
    let _tensors = u64_le(&mut file)?;
    let count = u64_le(&mut file)?;
    ensure!(
        count <= 1_000_000,
        "GGUF metadata key count exceeds safe inspection bound"
    );
    let mut metadata = Map::new();
    for _ in 0..count {
        let length = u64_le(&mut file)?;
        ensure!(length <= 65536, "GGUF metadata key is oversized");
        let mut bytes = vec![0; length as usize];
        file.read_exact(&mut bytes)?;
        let key = String::from_utf8(bytes).context("GGUF metadata key is not UTF-8")?;
        let kind = u32_le(&mut file)?;
        skip_value(&mut file, kind, end, 0)?;
        // GGUF string metadata may legitimately be empty; do not confuse that
        // with an array value unavailable from the native scalar lookup.
        let value = model.metadata(&key)?;
        metadata.insert(
            key,
            if value.is_empty() && kind == 9 {
                Value::Null
            } else {
                json!(value)
            },
        );
    }
    Ok(
        json!({"metadata":metadata, "n_ctx":model.context_size(), "n_batch":null, "n_threads":null, "n_threads_batch":null,
        "model_params":{"n_gpu_layers":model.gpu_layers_requested(), "use_mmap":null, "use_mlock":null},
        "context_params":{"n_ctx":model.context_size(), "n_batch":null, "n_ubatch":null, "n_threads":null, "n_threads_batch":null},
        "process_peak_rss_bytes":peak_rss_bytes()?, "native_engine_revision":LLAMA_REVISION,
        "unavailable_fields_reason":"Current public NativeModel ABI exposes context size and GPU-layer request, but no resolved batch/thread/mmap/mlock getters. Null metadata entries are values (typically arrays) not exposed by native metadata lookup. GPU-layer request is not a measured GPU-memory allocation."}),
    )
}
