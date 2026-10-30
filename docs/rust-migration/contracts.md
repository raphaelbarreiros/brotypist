# Implementation contract — approved native Rust migration

2026-10-30: user selected Rust-owned OS-native UI (not Tauri or Swift shell) and approved installing Rust 1.99.1/Cargo. Toolchain installation succeeded; binaries are `/Users/renanrocha/.cargo/bin/{cargo,rustc}`. Parent owns workspace/manifests/lockfile and integration checks.

Production workspace will use one application crate at `crates/brotypist`, organized into the boundaries in README.md rather than empty per-boundary crates. Root Cargo workspace and public module registration are integration-owned. Edition 2024, `anyhow::Result<T>` for service errors, serde JSON compatibility, standard platform cfgs. No Python sidecar in final runtime. Source baseline remains immutable while migration is in progress.

Derived caseless matching uses **Unicode 15.0.0 default-full, non-Turkic case folding**, matching the immutable CPython 3.12 baseline. The canonical `unicode_compat` iterator is shared by the app, independent updater and tools; no direct Unicode 9 folding remains in consumers. It does not normalize or rewrite authored text. Caller-owned NFC boundaries remain explicit. Exhaustive scalar folding equivalence does not certify normalization, category or regex-table equivalence.

## Independent first implementation packets

These do not require unproven UI/model bindings. Each owner writes complete owned modules plus relevant behavior tests, no builds/tests/formatters mid-flight. Dependency requirements go to parent; no Cargo edits.

### Configuration owner

Own `crates/brotypist/src/config.rs`, `paths.rs`, `shortcuts.rs` and module-local tests. Public API:

- `pub struct Config`: same public field names as Python Config at `config.py:121-177`; serde Serialize/Deserialize/Clone/Debug/PartialEq. String enums remain String at this boundary; numeric config fields use `u32`; booleans exact; `excluded_apps: Vec<String>`, `shortcuts: BTreeMap<String,String>`, `app_rules: BTreeMap<String,BTreeMap<String,serde_json::Value>>`. serde defaulting must preserve existing defaults, not silently accept wrong typed fields.
- `impl Default for Config`, `Config::validate(&mut self) -> Result<()>`, `Config::provider_identity(&self) -> Vec<String>`.
- `pub fn load_config() -> Result<Config>`, `save_config(&Config) -> Result<()>`, `config_path() -> Result<PathBuf>`, `default_instructions(locale: Option<&str>) -> String`, `normalize_domain(&str) -> Result<String>`, `local_endpoint(&str) -> Result<String>`.
- `paths::{config_dir, cache_dir, model_dir, update_dir}() -> Result<PathBuf>`: explicit platformdirs-compatible existing paths and INKLOW_HOME semantics. No new app-branded directories, normal-profile changes, or automatic OS registration.
- `shortcuts::normalize_shortcut(&str) -> Result<String>`, `default_shortcuts() -> BTreeMap<String,String>`, `parse_shortcut(&str) -> Result<Chord>`; `Chord {pub key:String,pub modifiers:Vec<String>}`. Physical key mapping helpers preserve source behavior.
- Initially validate catalog IDs against the exact baseline stable ID set; models owner later shares a single catalog source during integration, no circular config/models dependency needed.

### Storage owner

Own `crates/brotypist/src/storage.rs` and module-local tests. Depend on `config::Config`, `paths`, no core/native modules. Public API:

- `pub struct HistoryStore`; `HistoryStore::open() -> Result<Self>` uses baseline directory/db and credential identity, **does not unlock/create a key merely by opening**.
- `unlock(&mut self) -> Result<()>`, `lock(&mut self)`, `is_unlocked(&self)->bool`, `record(&mut self, app:&str,domain:&str,text:&str,authored_spans:&[(usize,usize)]) -> Result<bool>`, `records(&self, app:Option<&str>,domain:Option<&str>,limit:usize) -> Result<Vec<HistoryRecord>>`, `delete(&mut self,app:Option<&str>,domain:Option<&str>)->Result<usize>`.
- `HistoryRecord` public id/app_id/domain/text/created_at Strings and authored_spans Vec<(usize,usize)>; exact serialized payload naming/version must match Python, not these convenient Rust field names.
- Compatibility helpers `seal_record(key:&[u8;32],id:&str,payload:&[u8])->Result<Vec<u8>>` and `open_record(...)` support isolated cross-language smoke. Preserve AES-GCM nonce/AAD/keyring formats, authored scalar offsets, corruption/lost-key refusal and no plaintext fallback. Never access normal credentials/profile during implementation/checks.
- `record_statistics(&mut self,app:&str,text:&str,new_completion:bool)->Result<()>`, `statistics(&self,days:u32,grouping:&str)->Result<serde_json::Value>`, `clear_statistics(&mut self)->Result<()>`, `personalization(&self,app:&str,domain:&str,prefix:&str,strength:u32,source_allowed:&dyn Fn(&str,&str)->bool)->Result<String>`. Explicit identified-session/time inputs and temporary-unlock/count/info operations are approved additions; do not infer consent from source names.
- If source method semantics require additional input (language/time/consent), expose explicit typed request/additional methods; do not weaken gating to fit a signature. Notify parent of changes.

### Pure core/features owner

Own `crates/brotypist/src/core.rs`, `features.rs`, `spelling.rs` and module-local tests; shared snapshot/render types belong in `core.rs`, not a duplicate api module. Depend on config/shortcuts public contracts above.

- `pub type Rect = [f64;4]`; `InlineRender` and `Snapshot` have baseline public field names and types (Strings, usize scalar offsets, Option<Rect>, booleans, optional InlineRender, i32 window_pid). Derive Clone/Debug/Serialize/Deserialize/PartialEq where possible; defaults allow native constructors with `..Default::default()`.
- `Snapshot.text: TextBuffer`: immutable shared text (`Arc` ownership) with a lazy runtime-dependent checked `TextIndex` scalar/UTF-8/UTF-16 map. TextBuffer supports `From<String>`, `From<&str>`, `Deref<Target=str>`, `AsRef<str>`, Display, exact-text PartialEq and serde as a plain JSON string. Clones reuse text/maps; replacing the wrapper creates a fresh revision. No public mutable String plus cached-copy/fingerprint checks on every access. Native constructors use `text: observed.into()`.
- Snapshot methods `prefix()->&str`, `suffix()->&str`, `selected_text()->&str`, `line_suffix()->&str`, `inline_safe()->bool`, `has_anchor()->bool`, `anchor_rect()->Option<Rect>`, `eligible(min_chars:usize)->bool`, `same_key(&self,&Self)->bool`, `same_authority(&self,&Self)->bool`. Authority additionally compares window/document identities; geometry alone is not authority. Invalid scalar range yields safe ineligibility, never panic/native edit.
- `InlineRender::valid(caret:Option<Rect>)->bool` implements exact finite/clip/baseline/suffix-color rules.
- `SuggestionState` owns generation, snapshot and suggestion; expose `new/default`, `invalidate()->u64`, `offer(generation:u64,snapshot:Snapshot,text:String)->bool`, `advance(&Snapshot)->bool`, `accept(&Snapshot,full:bool,trailing_space:bool,trailing_punctuation:bool,visible:Option<&str>)->Option<String>`. Fields/accessors public for integration. Preserve pure update semantics; native confirmation is runtime responsibility.
- features preserve original named helpers: `length_profile(&str)->(u32,u32)`, `shorter_length(&str)->String`, `include_punctuation(&str,bool)->bool`, `current_word_only(prefix:&str,continuation:&str)->String`, `emoji_choices(prefix:&str,config:&Config)->Option<(String,Vec<String>)>`, `synonym_prompt(prefix,suffix,selection)->String`, `parse_choices(text:&str,max_choices:usize)->Vec<String>`; use embedded baseline emoji JSON, not generated substitutes. Reject decoded structural markers/line/control/format text and support safe utility abstention.
- Spelling module preserves complete extraction/language/range-mapping/planning/state contracts, with injected native dictionary backend; no fake spelling result when native backend unavailable. Additional result/backend types documented in handoff. This module does not run OS APIs or start app lifecycle.

## Parent integration obligations

Use language-server references when available before exported API changes (currently no project LSP configured). Parent adapts callsites to a single canonical API; no aliases/shims. Shared inferred signatures are a starting contract, not permission to reduce source behavior. An owner finding insufficient inputs must send concrete proposed contract changes before changing another owner's files. Full remaining controller/native/UI/provider/updater packets are dispatched only after their actual prerequisites stabilize.
