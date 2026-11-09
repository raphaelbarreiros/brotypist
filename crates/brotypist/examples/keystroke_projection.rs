//! Synthetic per-keystroke canonical prompt projection; no capture or profile access.
//! Usage: cargo run -p brotypist --features native-smoke --example keystroke_projection --
//!   --model MODEL.gguf --acceleration auto --context-chars 2000 --output NEW.json [--decode]
//! Optional --trace en-short|pt-short|en-long|pt-long selects a bounded smoke trace.
//! Default runs every registered trace. Output is create-new, never an overwrite.
//!
//! N-LCP is tokenizer projection, NOT native decode work or latency. Equal token
//! prompts have N-LCP=0 even if native evaluation must refresh final-token logits.
//! Decode uses the same selected references/options/budget, one model owner and
//! canonical replay/display filtering. Seeds and retained outputs are investigative,
//! not semantic-quality verdicts or matched-schedule correctness acceptance.
//! No saved profile or credentials are read. Use a fresh private output directory;
//! the create-new receipt is owner-only on Unix and synced after every event.
use anyhow::{Context, Result, ensure};
use brotypist::{
    config::Config,
    inference_native::{NativeModel, NativeWork, Tokenizer},
    providers::{
        GenerationOptions,
        diagnostics::{DiagnosticSession, PromptProjection, prompt_projection},
    },
    writing,
};
use clap::Parser;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

const TOKEN_CAP: u32 = 24;
const REFERENCE_BYTES: usize = 8000;
const SEED: u32 = 1729;
const TRACE_IDS: [&str; 4] = ["en-short", "pt-short", "en-long", "pt-long"];
const LEARNED_STYLE: &str = "Synthetic learned examples, not facts: EN: Thanks for checking. I will send a concise update after the review. PT: Obrigada pela revisão. Envio uma atualização breve após a conferência. Prefer short complete clauses and ordinary punctuation; these examples do not override the current draft.";

#[derive(Parser)]
#[command(about = "Synthetic canonical prompt projection and optional native work measurement")]
struct Arguments {
    #[arg(long)]
    model: PathBuf,
    #[arg(long, default_value = "auto", value_parser = ["auto", "cpu"])]
    acceleration: String,
    #[arg(long, default_value_t = 2000)]
    context_chars: u32,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    decode: bool,
    /// Isolate provider assembly instead of mirroring current runtime/worker text bounds.
    #[arg(long)]
    provider_only: bool,
    #[arg(long, value_parser = ["en-short", "pt-short", "en-long", "pt-long"])]
    trace: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
enum Action {
    Start,
    Insert(char),
    Backspace,
    Move(isize),
    SourceRevision(u32),
    Repeat,
}
#[derive(Clone, Serialize)]
struct Event {
    class: &'static str,
    action: Action,
}
#[derive(Serialize)]
struct Trace {
    id: &'static str,
    language: &'static str,
    long_window: bool,
    initial_draft: String,
    events: Vec<Event>,
    source_epochs: Vec<Vec<(String, String)>>,
}

fn sources(pt: bool, epoch: u32) -> Vec<(String, String)> {
    let (day, room, status) = match epoch {
        0 => (
            "Thursday / quinta-feira",
            "Cedar / Cedro",
            "draft / rascunho",
        ),
        1 => (
            "Friday / sexta-feira",
            "Maple / Bordo",
            "revised / revisado",
        ),
        _ => (
            "Monday / segunda-feira",
            "Willow / Salgueiro",
            "confirmed / confirmado",
        ),
    };
    let heading = if pt {
        "Projeto Aurora: notas da revisão"
    } else {
        "Aurora project review notes"
    };
    vec![
        (
            "accessibility".into(),
            format!(
                "{heading}\nAurora review revisão: {day}, 14:30, room sala {room}; status {status}.\nAurora budget orçamento: 420 synthetic credits; owner responsável Lia Teste.\nQuartz launch lançamento: 09:15; review revisão pending approval, not a commitment.\nQuartz budget orçamento: 310 synthetic credits; owner responsável Tom Example.\nCafé café accessibility ação: keyboard review with 👩🏽‍💻; 東京 prototype remains exploratory.\nAurora review revisão: {day}, 14:30, room sala {room}; status {status}.\nNavigation: Home Settings Search Help\nGarden watering schedule: succulents need dry soil.\nArchive revision {epoch}: these are invented demonstration records."
            ),
        ),
        (
            "screenshot".into(),
            format!(
                "Synthetic project board — OCR fixture\nAurora review revisão: {day}, 14:30, room sala {room}; status {status}.\nQuartz launch lançamento checklist: prototype protótipo ready, invoice fatura still pending.\nAurora review budget orçamento agenda: verify 420 credits before approval.\nRelease lançamento delivery entrega for Quartz is provisional; no customer deadline approved.\nWeather: scattered clouds, 18 degrees\nSidebar: archive, favourites, notifications\nOCR source revision {epoch}; row labels may repeat."
            ),
        ),
        (
            "clipboard".into(),
            format!(
                "Synthetic copied planning excerpt\nQuartz launch lançamento budget orçamento: compare 310 credits with prototype protótipo estimate.\nAurora review revisão action ação: Lia Teste will circulate notes after the meeting.\nQuartz launch lançamento: 09:15; review revisão pending approval, not a commitment.\nRecipe: mix flour and water, rest for twenty minutes.\nAurora café accessibility: keep the accent and the emoji 👩🏽‍💻 in the demo label.\nClipboard revision {epoch}: retain uncertainty; {status}."
            ),
        ),
    ]
}

fn typed(events: &mut Vec<Event>, text: &str, midline: bool) {
    let mut previous: Option<char> = None;
    for c in text.chars() {
        let class = if midline {
            "mid_line_typing"
        } else if c == ' ' {
            if previous.is_some_and(|c| c.is_alphanumeric() || matches!(c, '\u{301}' | '\u{327}')) {
                "word_completing_space"
            } else {
                "space_after_punctuation_or_unicode"
            }
        } else if c.is_alphanumeric() || matches!(c, '\u{301}' | '\u{327}') {
            "in_word_letter"
        } else {
            "punctuation_or_unicode"
        };
        events.push(Event {
            class,
            action: Action::Insert(c),
        });
        previous = Some(c);
    }
}

fn trace(id: &'static str, context_chars: u32) -> Trace {
    let pt = id.starts_with("pt");
    let long = id.ends_with("long");
    let mut draft = String::new();
    if long {
        // Distinct numbered multiline histories, then an active line near 800 scalars:
        // both the authored context window and reference query window can slide.
        let minimum = context_chars as usize + 1000;
        let mut count = 0;
        for index in 0.. {
            if count > minimum {
                break;
            }
            let line = if pt {
                format!(
                    "Registro sintético {index:04}: revisão Aurora, orçamento aberto, café e ação; a demonstração 東京 não define um prazo.\n"
                )
            } else {
                format!(
                    "Synthetic entry {index:04}: Aurora review, budget pending, café and 👩🏽‍💻 notes; the 東京 demonstration does not promise delivery.\n"
                )
            };
            count += line.chars().count();
            draft.push_str(&line);
        }
        // The initial relevant term is deliberately at the edge of the 800-scalar
        // reference query, so a few individual keys can drop a competing topic.
        let lead = if pt {
            "Aurora revisão orçamento "
        } else {
            "Aurora review budget "
        };
        draft.push_str(lead);
        let neutral = if pt {
            "nota comum sem decisão "
        } else {
            "ordinary note without decision "
        };
        let active = neutral.repeat(720_usize.div_ceil(neutral.chars().count()));
        draft.extend(active.chars().take(720));
        draft.push(' ');
    }
    draft.push_str(if pt {
        "Olá, equipe. A revisão Aurora precisa de uma confirma"
    } else {
        "Hello team. The Aurora review needs a confirma"
    });
    let mut events = vec![Event {
        class: "cold_start",
        action: Action::Start,
    }];
    typed(&mut events, if pt { "ção " } else { "tion " }, false);
    typed(
        &mut events,
        if pt {
            "do orçamento, "
        } else {
            "of the budget, "
        },
        false,
    );
    events.push(Event {
        class: "source_revision",
        action: Action::SourceRevision(1),
    });
    typed(&mut events, "Quartz ", false);
    typed(
        &mut events,
        if pt {
            "lançamento protótipo café "
        } else {
            "launch prototype cafe\u{301} "
        },
        false,
    );
    typed(&mut events, "👩🏽‍💻 東京. ", false);
    events.push(Event {
        class: "backspace_across_word_boundary",
        action: Action::Backspace,
    });
    events.push(Event {
        class: "backspace_punctuation",
        action: Action::Backspace,
    });
    events.push(Event {
        class: "backspace_within_word",
        action: Action::Backspace,
    });
    typed(&mut events, "京! ", false);
    // Delete a separator then a preceding letter, crossing an actual word boundary.
    typed(&mut events, if pt { "nota " } else { "note " }, false);
    events.push(Event {
        class: "backspace_across_word_boundary",
        action: Action::Backspace,
    });
    events.push(Event {
        class: "backspace_within_word",
        action: Action::Backspace,
    });
    typed(&mut events, if pt { "a " } else { "e " }, false);
    events.push(Event {
        class: "caret_move",
        action: Action::Move(-5),
    });
    typed(&mut events, if pt { "breve " } else { "brief " }, true);
    events.push(Event {
        class: "caret_move",
        action: Action::Move(-3),
    });
    events.push(Event {
        class: "backspace_within_word",
        action: Action::Backspace,
    });
    typed(&mut events, "x", true);
    events.push(Event {
        class: "source_revision",
        action: Action::SourceRevision(2),
    });
    events.push(Event {
        class: "exact_repeat",
        action: Action::Repeat,
    });
    // Move back into the active sentence, then return to the draft's end.
    events.push(Event {
        class: "caret_move",
        action: Action::Move(-24),
    });
    typed(&mut events, if pt { "ação " } else { "check " }, true);
    events.push(Event {
        class: "caret_move",
        action: Action::Move(1000),
    });
    if long {
        // These individual punctuation/space keys have no unfinished-word replay;
        // each advances the long scalar window rather than only extending a word.
        typed(&mut events, "; : , ; : , ; : , ; : , ", false);
        typed(
            &mut events,
            if pt {
                "Quartz entrega fatura "
            } else {
                "Quartz delivery invoice "
            },
            false,
        );
        for index in 0..8 {
            events.push(Event {
                class: if index == 0 || (pt && index == 7) {
                    "backspace_across_word_boundary"
                } else {
                    "backspace_within_word"
                },
                action: Action::Backspace,
            });
        }
        typed(
            &mut events,
            if pt {
                "Aurora revisão "
            } else {
                "Aurora review "
            },
            false,
        );
    }
    Trace {
        id,
        language: if pt { "pt-BR" } else { "en" },
        long_window: long,
        initial_draft: draft,
        events,
        source_epochs: (0..=2).map(|epoch| sources(pt, epoch)).collect(),
    }
}

fn hash(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}
fn file_hash(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
fn source_hashes() -> Value {
    let mut hashes = BTreeMap::new();
    for (name, bytes) in [
        (
            "examples/keystroke_projection.rs",
            include_bytes!("keystroke_projection.rs").as_slice(),
        ),
        (
            "src/providers.rs",
            include_bytes!("../src/providers.rs").as_slice(),
        ),
        (
            "src/providers_diagnostics.rs",
            include_bytes!("../src/providers_diagnostics.rs").as_slice(),
        ),
        (
            "src/writing.rs",
            include_bytes!("../src/writing.rs").as_slice(),
        ),
        (
            "src/inference_native.rs",
            include_bytes!("../src/inference_native.rs").as_slice(),
        ),
        (
            "src/config.rs",
            include_bytes!("../src/config.rs").as_slice(),
        ),
        (
            "native/llama_bridge.c",
            include_bytes!("../native/llama_bridge.c").as_slice(),
        ),
        (
            "src/runtime.rs",
            include_bytes!("../src/runtime.rs").as_slice(),
        ),
        (
            "src/process_client.rs",
            include_bytes!("../src/process_client.rs").as_slice(),
        ),
    ] {
        hashes.insert(name, hash(bytes));
    }
    json!(hashes)
}

// Exactly one of these owns weights. Projection-only never evaluates tokens.
enum Owner {
    Projection { model: NativeModel, config: Config },
    Decode(DiagnosticSession),
}
impl Owner {
    fn projection(
        &self,
        prefix: &str,
        suffix: &str,
        options: &GenerationOptions,
    ) -> Result<PromptProjection> {
        match self {
            Self::Projection { model, config } => {
                prompt_projection(model, config, prefix, suffix, options)
            }
            Self::Decode(session) => session.projection(prefix, suffix, options, TOKEN_CAP),
        }
    }
    fn context_size(&self) -> usize {
        match self {
            Self::Projection { model, .. } => model.context_size(),
            Self::Decode(session) => session.context_size(),
        }
    }
}

fn byte_at(text: &str, scalar: usize) -> usize {
    text.char_indices()
        .nth(scalar)
        .map_or(text.len(), |(index, _)| index)
}
fn apply(draft: &mut String, caret: &mut usize, epoch: &mut usize, action: &Action) {
    match action {
        Action::Insert(c) => {
            draft.insert(byte_at(draft, *caret), *c);
            *caret += 1;
        }
        Action::Backspace if *caret > 0 => {
            let start = byte_at(draft, *caret - 1);
            let end = byte_at(draft, *caret);
            draft.replace_range(start..end, "");
            *caret -= 1;
        }
        Action::Move(delta) => {
            *caret = caret
                .saturating_add_signed(*delta)
                .min(draft.chars().count());
        }
        Action::SourceRevision(revision) => *epoch = *revision as usize,
        Action::Start | Action::Repeat | Action::Backspace => {}
    }
}

fn span_at(projection: &PromptProjection, token: usize) -> Value {
    match projection
        .spans
        .iter()
        .find(|span| span.start <= token && token < span.end)
    {
        Some(span) => json!({
            "index":span.index, "start":span.start, "end":span.end,
            "trust":if span.special { "trusted_framing" } else { "untrusted_data" },
            "special_token_parsing":span.special,
            // Full synthetic text is unambiguous even when a short excerpt repeats.
            "synthetic_text":span.text, "text_sha256":hash(&span.text),
            "semantic_label":if projection.spans.last().is_some_and(|last| last.index == span.index) {
                "effective_authored_assistant_prefill"
            } else if span.special { "canonical_framing" } else { "canonical_data_see_synthetic_text" },
        }),
        None => {
            json!({"index":null,"trust":null,"semantic_label":"end_of_prompt", "token_offset":token})
        }
    }
}

fn window_identity(
    prefix: &str,
    suffix: &str,
    provider_input: (&str, &str),
    projection: &PromptProjection,
    context_chars: u32,
    options: &GenerationOptions,
) -> Result<Value> {
    let (provider_prefix, provider_suffix) = provider_input;
    ensure!(
        prefix.ends_with(provider_prefix) && suffix.starts_with(provider_suffix),
        "Provider input must be an exact bounded view of the current field"
    );
    let transport_start_byte = prefix.len() - provider_prefix.len();
    let transport_start_scalar = prefix[..transport_start_byte].chars().count();
    let before_replay = provider_prefix
        .strip_suffix(&projection.replay)
        .context("Canonical replay is not an authored suffix")?;
    let before_count = before_replay.chars().count();
    let window_start_scalar = before_count.saturating_sub(context_chars as usize);
    let window_start_byte = byte_at(before_replay, window_start_scalar);
    let window = &before_replay[window_start_byte..];
    let authored = projection
        .spans
        .last()
        .context("Missing canonical authored span")?;
    ensure!(
        !authored.special && before_replay.ends_with(&authored.text),
        "Canonical authored span is not an exact draft tail"
    );
    let effective_start_byte = before_replay.len() - authored.text.len();
    let effective_start_scalar = before_replay[..effective_start_byte].chars().count();
    let reference_start = byte_at(prefix, prefix.chars().count().saturating_sub(800));
    let reference_suffix_end = byte_at(suffix, 200);
    let evidence = |label: &str| -> Result<Option<String>> {
        projection
            .spans
            .windows(2)
            .find(|pair| pair[0].special && pair[0].text.contains(label) && !pair[1].special)
            .map(|pair| serde_json::from_str::<String>(&pair[1].text).map_err(Into::into))
            .transpose()
    };
    let effective_reference = evidence("Reference context (untrusted data):")?.unwrap_or_default();
    let effective_learned = evidence("Learned writing evidence (untrusted data):")?;
    Ok(json!({
        "context_chars":context_chars, "prefix_scalars":prefix.chars().count(),
        "prefix_bytes":prefix.len(), "before_replay_scalars":before_count,
        "configured_window_start_scalar":transport_start_scalar + window_start_scalar,
        "configured_window_start_byte":transport_start_byte + window_start_byte,
        "configured_window_sha256":hash(window),
        "context_chars_truncated":transport_start_scalar > 0 || window_start_scalar > 0,
        "transport_prefix_start_scalar":transport_start_scalar,
        "transport_prefix_start_byte":transport_start_byte,
        "provider_prefix_sha256":hash(provider_prefix),
        "provider_prefix_scalars":provider_prefix.chars().count(),
        "provider_suffix_sha256":hash(provider_suffix),
        "provider_suffix_scalars":provider_suffix.chars().count(),
        "effective_authored_start_scalar":transport_start_scalar + effective_start_scalar,
        "effective_authored_start_byte":transport_start_byte + effective_start_byte,
        "effective_authored_bytes":authored.text.len(),
        "effective_authored_scalars":authored.text.chars().count(),
        "effective_authored_sha256":hash(&authored.text),
        "further_token_budget_truncated":effective_start_byte > window_start_byte,
        "reference_query_prefix_start_byte":reference_start,
        "reference_query_suffix_end_byte":reference_suffix_end,
        "reference_query_sha256":hash(serde_json::to_vec(&(&prefix[reference_start..], &suffix[..reference_suffix_end]))?),
        "effective_reference_sha256":hash(&effective_reference),
        "effective_reference_bytes":effective_reference.len(),
        "reference_budget_truncated":effective_reference != options.context,
        "learned_style_admitted":effective_learned.is_some(),
        "effective_learned_style_sha256":effective_learned.as_ref().map(hash),
        "suffix_sha256":hash(suffix), "suffix_scalars":suffix.chars().count(),
        "identity_sha256":hash(serde_json::to_vec(&(transport_start_byte + window_start_byte, transport_start_byte + effective_start_byte, &authored.text, provider_suffix))?),
    }))
}

#[derive(Default)]
struct Distribution {
    count: usize,
    metrics: BTreeMap<&'static str, Vec<u64>>,
}
impl Distribution {
    fn add(&mut self, name: &'static str, value: u64) {
        self.metrics.entry(name).or_default().push(value);
    }
    fn finish(mut self) -> Value {
        let mut metrics = BTreeMap::new();
        for (name, values) in &mut self.metrics {
            values.sort_unstable();
            let n = values.len();
            let median = if n % 2 == 0 {
                (values[n / 2 - 1] as f64 + values[n / 2] as f64) / 2.0
            } else {
                values[n / 2] as f64
            };
            metrics.insert(
                *name,
                json!({"count":n, "minimum":values[0], "median":median,
                "p90":values[(n * 9).div_ceil(10) - 1], "maximum":values[n - 1],
                "sum":values.iter().map(|&value| value as u128).sum::<u128>()}),
            );
        }
        json!({"rows":self.count,"metrics":metrics})
    }
}

fn save_receipt(file: &mut File, value: &impl Serialize) -> Result<()> {
    file.seek(SeekFrom::Start(0))?;
    serde_json::to_writer_pretty(&mut *file, value)?;
    file.write_all(b"\n")?;
    let length = file.stream_position()?;
    file.set_len(length)?;
    file.sync_data()?;
    Ok(())
}

fn save_progress(file: &mut File, rows: &[Value]) -> Result<()> {
    #[derive(Serialize)]
    struct Progress<'a> {
        schema: &'static str,
        status: &'static str,
        scope: &'static str,
        rows: &'a [Value],
    }
    save_receipt(
        file,
        &Progress {
            schema: "brotypist-keystroke-projection-v3",
            status: "running",
            scope: "Incomplete synthetic-only event receipt; no acceptance claim. Synced after each event, not atomic across abrupt power loss.",
            rows,
        },
    )
}

fn main() -> Result<()> {
    let args = Arguments::parse();
    // Reserve before model loading; existing measurement files are never replaced.
    let mut output = OpenOptions::new();
    output.write(true).create_new(true);
    #[cfg(unix)]
    output.mode(0o600);
    let mut file = output.open(&args.output).with_context(|| {
        format!(
            "Output must be a new private file: {}",
            args.output.display()
        )
    })?;
    save_progress(&mut file, &[])?;
    let mut config = Config {
        backend: "gguf".into(),
        model_path: args
            .model
            .to_str()
            .context("Non-Unicode model path")?
            .into(),
        prompt_style: "writing".into(),
        acceleration: args.acceleration.clone(),
        context_chars: args.context_chars,
        max_tokens: TOKEN_CAP,
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
    // Config disables learned-history access; explicit synthetic options still
    // exercise the unchanged learned-evidence admission/fitting policy.
    let config_identity = json!({
        "config":config, "token_cap":TOKEN_CAP, "reference_max_bytes":REFERENCE_BYTES,
        "seed":SEED, "synthetic_learned_style":LEARNED_STYLE,
        "batch":64, "microbatch":32, "attention":"disabled", "swa":"full",
        "input_boundary":if args.provider_only { "provider_only" } else { "runtime_and_worker_scalar_bounds" },
        "runtime_side_scalar_limit":args.context_chars,"worker_side_scalar_limit":8000,
        "batch_policy_note":"fixed baseline; no backend overrides made by this harness"
    });
    let model_sha256 = file_hash(&args.model)?;
    let mut owner = if args.decode {
        let mut session = DiagnosticSession::open(&args.model, "writing", &args.acceleration)?;
        session.set_context_chars(args.context_chars)?;
        Owner::Decode(session)
    } else {
        Owner::Projection {
            model: NativeModel::open(&args.model, &args.acceleration)?,
            config,
        }
    };
    let traces: Vec<_> = TRACE_IDS
        .into_iter()
        .filter(|id| args.trace.as_deref().is_none_or(|selected| selected == *id))
        .map(|id| trace(id, args.context_chars))
        .collect();
    let traces_value = serde_json::to_value(&traces)?;
    let mut rows = Vec::new();
    let mut groups: BTreeMap<String, Distribution> = BTreeMap::new();
    let (mut failures, mut no_offers, mut cancellations, mut resets, mut trim_failures) =
        (0, 0, 0, 0_u64, 0_u64);
    for trace in &traces {
        if let Owner::Decode(session) = &mut owner {
            // Retire DiagnosticSession's framing warmup and the prior trace's KV.
            session.clear_cache();
            session.reset_observations();
        }
        let mut draft = trace.initial_draft.clone();
        let mut caret = draft.chars().count();
        let mut epoch = 0;
        let mut previous: Option<PromptProjection> = None;
        let mut previous_context: Option<String> = None;
        for (event_index, event) in trace.events.iter().enumerate() {
            apply(&mut draft, &mut caret, &mut epoch, &event.action);
            let split = byte_at(&draft, caret);
            let (prefix, suffix) = draft.split_at(split);
            let sources = &trace.source_epochs[epoch];
            // Deliberately called for EVERY event, including repeats and revisions.
            let context = writing::reference_context(prefix, suffix, sources, REFERENCE_BYTES);
            // Source-derived scalar bounds, not an executed Runtime/IPC path.
            // Reference selection above deliberately retains the full current field.
            let side_limit = (args.context_chars as usize).min(8000);
            let (provider_prefix, provider_suffix) = if args.provider_only {
                (prefix, suffix)
            } else {
                (
                    &prefix[byte_at(prefix, caret.saturating_sub(side_limit))..],
                    &suffix[..byte_at(suffix, side_limit)],
                )
            };
            let context_sha256 = hash(&context);
            let context_changed = previous_context.as_ref().map(|old| old != &context_sha256);
            let options = GenerationOptions {
                context,
                personalization: LEARNED_STYLE.into(),
                ..GenerationOptions::default()
            };
            let group_key = format!(
                "{}/{}/{}",
                trace.language,
                if trace.long_window {
                    "long_window"
                } else {
                    "short_window"
                },
                event.class
            );
            let group = groups.entry(group_key).or_default();
            group.count += 1;
            let projection = match owner.projection(provider_prefix, provider_suffix, &options) {
                Ok(projection) => projection,
                Err(error) => {
                    failures += 1;
                    rows.push(json!({"trace":trace.id,"event_index":event_index,"class":event.class,
                        "action":event.action,"source_epoch":epoch,"prefix":prefix,"suffix":suffix,
                        "provider_prefix_sha256":hash(provider_prefix),"provider_suffix_sha256":hash(provider_suffix),
                        "selected_context":options.context,"context_sha256":context_sha256,
                        "context_selection_changed":context_changed,"projection_error":format!("{error:#}"),
                        "n":null,"previous_n":previous.as_ref().map(|p| p.tokens.len()),"lcp":null,"n_minus_lcp":null,
                        "decode":null,"status":"projection_failed"}));
                    save_progress(&mut file, &rows)?;
                    // Never compare against a nonadjacent event after an error.
                    previous = None;
                    previous_context = Some(context_sha256);
                    continue;
                }
            };
            let n = projection.tokens.len();
            let lcp = previous
                .as_ref()
                .map(|old| {
                    old.tokens
                        .iter()
                        .zip(&projection.tokens)
                        .take_while(|(a, b)| a == b)
                        .count()
                })
                .or_else(|| (event_index == 0).then_some(0));
            let equal = previous.as_ref().map(|old| old.tokens == projection.tokens);
            let first_divergence = match lcp {
                Some(offset) if equal != Some(true) => json!({
                    "token_offset":offset,"current":span_at(&projection, offset),
                    "previous":previous.as_ref().map(|old| span_at(old, offset)),
                }),
                _ => Value::Null,
            };
            group.add("n", n as u64);
            if let Some(lcp) = lcp {
                group.add("lcp", lcp as u64);
                group.add("n_minus_lcp", (n - lcp) as u64);
            }
            group.add("replay_bytes", projection.replay.len() as u64);
            group.add("replay_tokens", projection.replay_tokens as u64);
            group.add(
                "context_selection_changed",
                u64::from(context_changed == Some(true)),
            );
            let window = window_identity(
                prefix,
                suffix,
                (provider_prefix, provider_suffix),
                &projection,
                args.context_chars,
                &options,
            )?;
            let decode = if let Owner::Decode(session) = &mut owner {
                session.reset_observations();
                session.set_next_seed(SEED)?;
                let cancel = AtomicBool::new(false);
                let mut offered = String::new();
                let result = session.stream(
                    provider_prefix,
                    provider_suffix,
                    &options,
                    TOKEN_CAP,
                    &cancel,
                    &mut |text| {
                        offered.clear();
                        offered.push_str(text);
                        Ok(())
                    },
                );
                let cancelled = cancel.load(Ordering::Acquire);
                let no_offer = offered.is_empty();
                no_offers += usize::from(no_offer);
                cancellations += usize::from(cancelled);
                let observations = session.observations();
                let mut work = NativeWork::default();
                let mut accounting_errors = Vec::new();
                if observations.len() != 1 {
                    accounting_errors.push(format!(
                        "Expected one native observation, found {}",
                        observations.len()
                    ));
                }
                for (index, observation) in observations.iter().enumerate() {
                    match serde_json::from_value::<NativeWork>(observation["native_work"].clone()) {
                        Ok(actual) => {
                            // Deferred assertion lets every failing row survive in output.
                            if actual
                                .reused_tokens
                                .checked_add(actual.prompt.submitted_tokens)
                                != Some(actual.input_tokens)
                            {
                                accounting_errors.push(format!(
                                    "Call {index}: reused + submitted prompt decode != input tokens"
                                ));
                            }
                            if actual.input_tokens != n as u64
                                || observation["prompt_token_count"].as_u64() != Some(n as u64)
                            {
                                accounting_errors.push(format!(
                                    "Call {index}: decode input differs from canonical projection"
                                ));
                            }
                            work += actual;
                        }
                        Err(error) => accounting_errors
                            .push(format!("Call {index}: invalid native work: {error}")),
                    }
                }
                resets += work.cache_resets;
                trim_failures += work.trim_failures;
                let failed = result.is_err() || !accounting_errors.is_empty();
                failures += usize::from(failed);
                group.add(
                    "native_prompt_submitted_tokens",
                    work.prompt.submitted_tokens,
                );
                group.add(
                    "native_prompt_completed_tokens",
                    work.prompt.completed_tokens,
                );
                group.add("native_reused_tokens", work.reused_tokens);
                group.add(
                    "native_generation_submitted_tokens",
                    work.generation.submitted_tokens,
                );
                group.add("native_sampled_tokens", work.sampled_tokens);
                group.add("no_offer", u64::from(no_offer));
                group.add("cancelled", u64::from(cancelled));
                group.add("cache_resets", work.cache_resets);
                group.add("trim_failures", work.trim_failures);
                json!({"seed":SEED,"offered_text":offered,"no_offer":no_offer,"cancelled":cancelled,
                    "stream_error":result.err().map(|error| format!("{error:#}")),
                    "accounting_ok":accounting_errors.is_empty(),"accounting_errors":accounting_errors,
                    "work":work,"observations":observations})
            } else {
                Value::Null
            };
            rows.push(json!({
                "trace":trace.id,"language":trace.language,"long_window":trace.long_window,
                "event_index":event_index,"class":event.class,"action":event.action,
                "cold_start":event_index == 0,"projection_predecessor_available":previous.is_some(),
                "source_epoch":epoch,"sources_sha256":hash(serde_json::to_vec(sources)?),
                "prefix":prefix,"suffix":suffix,"caret_scalar":caret,"caret_byte":split,
                "options":options,"context_sha256":context_sha256,
                "context_selection_changed":context_changed,
                "window":window,"n":n,"previous_n":previous.as_ref().map(|old| old.tokens.len()),
                "lcp":lcp,"n_minus_lcp":lcp.map(|lcp| n-lcp),"exact_equal_prompt":equal,
                "first_divergent_span":first_divergence,
                "replay_bytes":projection.replay.len(),"replay_tokens":projection.replay_tokens,
                "projection":projection,"decode":decode,
            }));
            save_progress(&mut file, &rows)?;
            previous = Some(projection);
            previous_context = Some(context_sha256);
        }
    }
    let (cold, warm): (BTreeMap<_, _>, BTreeMap<_, _>) = groups
        .into_iter()
        .map(|(key, distribution)| (key, distribution.finish()))
        .partition(|(key, _)| key.ends_with("/cold_start"));
    let report = json!({
        "schema":"brotypist-keystroke-projection-v3",
        "status":if failures == 0 { "passed" } else { "failed" },
        "receipt_durability":"Synced after each event and before the final assertion; abrupt power loss during a rewrite is not claimed atomic",
        "measurement":"tokenizer projection; optional real decode work, never inferred latency",
        "decode_enabled":args.decode,
        "quality_acceptance":null,"matched_schedule_correctness_acceptance":null,
        "cross_schedule_drift_acceptance":null,
        "notes":[
            "N-LCP is adjacent canonical prompt token divergence, not actual native work.",
            "Default inputs mirror Engine's context_chars tail/head and WorkerClient's 8000-scalar bounds before native assembly; this does not execute Runtime, IPC or desktop authority.",
            "The provider-only switch removes those caller bounds for component isolation; reference selection always sees the full synthetic field.",
            "Exact-equal prompts have N-LCP=0; a native logits refresh belongs only to observed decode work.",
            "Cold-start rows have no previous prompt, LCP=0, and are excluded from warm summaries.",
            "After a failed projection, adjacent-transition divergence is unavailable; per-metric counts exclude it rather than treating it as a warm reset.",
            "Decode retains KV within each trace and explicitly clears it between traces; no per-event model reload.",
            "DiagnosticSession opening performs framing warmup; it is cleared before measurements and not included in event work.",
            "Per-call elapsed counters, if present in raw observations, are not latency acceptance.",
            "All drafts, sources, learned style and outputs are synthetic or generated from synthetic input.",
            "Reference selection is rerun on every event using the current full prefix and suffix.",
            "No matched-schedule exact correctness, drift calibration or semantic quality verdict is made here."
        ],
        "model":{"path":args.model,"sha256":model_sha256,"context_tokens":owner.context_size()},
        "source_sha256":source_hashes(),"config_identity":config_identity,
        "config_sha256":hash(serde_json::to_vec(&config_identity)?),
        "fixture_sha256":hash(serde_json::to_vec(&traces_value)?),"traces":traces_value,
        "summary":{"rows":rows.len(),"failures":failures,
            "no_offers":if args.decode { Some(no_offers) } else { None },
            "cancellations":if args.decode { Some(cancellations) } else { None },
            "cache_resets":if args.decode { Some(resets) } else { None },
            "trim_failures":if args.decode { Some(trim_failures) } else { None },
            "intentional_trace_cache_clears":if args.decode { traces.len() } else { 0 },
            "percentile_method":"p90 nearest rank; median mean of middle pair for even counts",
            "cold_start_distributions":cold,"warm_transition_distributions":warm},
        "rows":rows,
    });
    save_receipt(&mut file, &report)?;
    ensure!(
        failures == 0,
        "Measurement contains {failures} failed rows; complete report retained at {}",
        args.output.display()
    );
    Ok(())
}
