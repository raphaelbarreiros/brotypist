//! Opt-in, disposable desktop evidence using the actual resident shell and inference worker.
//! Save/model assertions observe the shell's single receipt consumer by operation ID;
//! user-facing notices are not persistence or model-validation evidence.
//! Button signals enqueue actions; native assertions await their resulting state.
//! Modal/paint loops can dispatch timers while the scenario still borrows the shell.
//! Every fixture turn releases the shell until its normal owner drains queued
//! actions and refreshes native state, including actions that intentionally do
//! not change a page (access-review refusal and cancelled close/quit).
//! A real Save persistence failure temporarily replaces only this run's validated
//! private settings destination with an empty directory. Its original file is
//! moved intact into an owned backup and restored before explicit recovery Save;
//! cleanup also restores it on scenario failure. Receipts never include draft text.
//! Before constructing GTK, inspect public D-Bus providers through the live loader.
//! Executable interposition fails with retained evidence; hidden implementations and
//! shared-library installation paths are not constrained. No D-Bus function is called.
//! Catalogue detail evidence browses existing zero/positive-minimum entries in the
//! real Models dialog without saving a model, approving a license or downloading.
use super::*;
use anyhow::{bail, ensure};
use glib::translate::IntoGlib;
use std::cell::Cell;
use std::ffi::CStr;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// One armed operation, captured at submission before any polling. The shell remains
/// the sole receiver; its already-consumed receipt is moved here, never cloned.
#[derive(Default)]
pub(super) struct ReceiptObservation {
    armed: bool,
    operation: Option<crate::runtime::ConfigOperationId>,
    receipt: Option<ConfigApplyReceipt>,
    error: Option<&'static str>,
}
impl ReceiptObservation {
    fn arm(&mut self) -> Result<()> {
        ensure!(!self.armed, "Previous smoke receipt has not been consumed.");
        self.armed = true;
        Ok(())
    }
    pub(super) fn started(&mut self, id: crate::runtime::ConfigOperationId) {
        if self.armed {
            if self.operation.is_some() {
                self.error = Some("More than one operation submitted for an armed smoke receipt.");
            } else {
                self.operation = Some(id);
            }
        }
    }
    pub(super) fn received(
        &mut self,
        id: crate::runtime::ConfigOperationId,
        receipt: ConfigApplyReceipt,
    ) {
        if self.armed && self.operation == Some(id) {
            if self.receipt.is_some() {
                self.error = Some("The shell consumed a duplicate terminal receipt.");
            } else {
                self.receipt = Some(receipt);
            }
        }
    }
}

#[derive(Debug)]
enum TerminalReceipt {
    Saved,
    Failed,
    PersistenceFailed,
    Cancelled,
}

fn wait_for_native_map(window: &gtk::Window) -> Result<()> {
    let mapped = |window: &gtk::Window| {
        window.is_visible()
            && window.is_mapped()
            && window.window().is_some_and(|native| native.is_viewable())
    };
    if mapped(window) {
        return Ok(());
    }
    let loop_ = glib::MainLoop::new(None, false);
    let done = loop_.clone();
    let handler = window.connect_map_event(move |window, _| {
        if mapped(window) {
            done.quit();
        }
        glib::Propagation::Proceed
    });
    let expired = Rc::new(Cell::new(false));
    let timed_out = expired.clone();
    let done = loop_.clone();
    let timeout = glib::timeout_add_local_once(Duration::from_secs(5), move || {
        timed_out.set(true);
        done.quit();
    });
    // Dispatch the real native MapNotify. Never show a refused presentation or
    // capture an unmapped drawable; rendered_snapshot separately awaits paint.
    loop_.run();
    window.disconnect(handler);
    if !expired.get() {
        timeout.remove();
    }
    ensure!(
        mapped(window) && !expired.get(),
        "X11 chooser did not map before its deadline (visible={}, mapped={}, native_viewable={}).",
        window.is_visible(),
        window.is_mapped(),
        window.window().is_some_and(|native| native.is_viewable()),
    );
    Ok(())
}

fn rendered_snapshot(
    window: &gtk::Window,
    area: Option<&gtk::DrawingArea>,
) -> Result<gtk::gdk_pixbuf::Pixbuf> {
    use gdk::prelude::WindowExtManual;

    ensure!(
        window.is_visible() && window.is_mapped(),
        "Screenshot requires the mapped canonical GTK window."
    );
    if let Some(area) = area {
        ensure!(
            area.is_ancestor(window) && area.is_visible() && area.is_mapped(),
            "Screenshot drawing area must belong to the mapped owned window."
        );
    }
    let clock = window
        .frame_clock()
        .context("Mapped GTK window has no frame clock.")?;
    let loop_ = glib::MainLoop::new(None, false);
    let painted = Rc::new(Cell::new(false));
    let ready = painted.clone();
    let done = loop_.clone();
    let handler = clock.connect_after_paint(move |_| {
        ready.set(true);
        done.quit();
    });
    let expired = Rc::new(Cell::new(false));
    let timed_out = expired.clone();
    let done = loop_.clone();
    let timeout = glib::timeout_add_local_once(Duration::from_secs(5), move || {
        timed_out.set(true);
        done.quit();
    });
    // GTK allocates during the frame's layout phase. A positive allocated size alone
    // does not make gtk_widget_draw valid after a pane, label or control changed.
    window.queue_draw();
    clock.request_phase(gdk::FrameClockPhase::AFTER_PAINT);
    loop_.run();
    clock.disconnect(handler);
    if !expired.get() {
        timeout.remove();
    }
    ensure!(
        painted.get() && !expired.get(),
        "GTK did not render the requested frame."
    );
    // Read the widget that owns the draw callback, not its transparent popup
    // parent. Native child drawables need not be included when Cairo reads the
    // parent's X11 surface (especially under compositor redirection).
    // Never fall back to a root/desktop capture or a newly rendered surface.
    let widget: &gtk::Widget = match area {
        Some(area) => area.upcast_ref(),
        None => window.upcast_ref(),
    };
    let width = widget.allocated_width();
    let height = widget.allocated_height();
    ensure!(width > 0 && height > 0, "Native widget has no allocation.");
    let native = widget
        .window()
        .context("Mapped GTK widget has no native drawable.")?;
    ensure!(
        native.is_viewable()
            && widget.is_mapped()
            && window.is_mapped()
            && native.display() == gtk::prelude::WidgetExt::display(window),
        "Readback must remain within the mapped owned widget's display."
    );
    native.display().sync();
    let pixels = native
        .pixbuf(0, 0, width, height)
        .context("Cannot read the rendered native GTK drawable.")?;
    let evidence = pixel_evidence(&pixels);
    eprintln!("[native-linux-smoke] native-readback {evidence}");
    ensure!(
        image_has_content(
            pixels.read_pixel_bytes().as_ref(),
            pixels.width() as usize,
            pixels.height() as usize,
            pixels.rowstride() as usize,
            pixels.n_channels() as usize,
        ),
        "Native screenshot is blank or transparent rather than rendered controls: {evidence}"
    );
    Ok(pixels)
}

/// Scalar-only, bounded diagnostics: no pixels, labels or user text in receipts.
fn pixel_evidence(pixels: &gtk::gdk_pixbuf::Pixbuf) -> Value {
    let bytes = pixels.read_pixel_bytes();
    let channels = pixels.n_channels() as usize;
    let stride = pixels.rowstride() as usize;
    let width = pixels.width() as usize;
    let mut nontransparent = 0usize;
    let mut opaque = 0usize;
    let mut contrasting = 0usize;
    let mut background = None::<[u8; 3]>;
    for y in 0..pixels.height() as usize {
        let row = &bytes.as_ref()[y * stride..y * stride + width * channels];
        for pixel in row.chunks_exact(channels) {
            let alpha = if channels == 4 { pixel[3] } else { 255 };
            nontransparent += usize::from(alpha != 0);
            if alpha < 128 {
                continue;
            }
            opaque += 1;
            let base = *background.get_or_insert([pixel[0], pixel[1], pixel[2]]);
            contrasting +=
                usize::from((0..3).any(|channel| pixel[channel].abs_diff(base[channel]) >= 16));
        }
    }
    json!({
        "width": pixels.width(),
        "height": pixels.height(),
        "channels": channels,
        "rowstride": stride,
        "nontransparent_pixels": nontransparent,
        "alpha_at_least_128_pixels": opaque,
        "contrasting_pixels": contrasting,
    })
}

fn image_has_content(
    bytes: &[u8],
    width: usize,
    height: usize,
    stride: usize,
    channels: usize,
) -> bool {
    if width == 0 || height == 0 || !matches!(channels, 3 | 4) {
        return false;
    }
    let Some(row_bytes) = width.checked_mul(channels) else {
        return false;
    };
    if stride < row_bytes {
        return false;
    }
    let mut background = None::<[u8; 3]>;
    let mut contrasting = 0usize;
    for y in 0..height {
        let Some(start) = y.checked_mul(stride) else {
            return false;
        };
        let Some(end) = start.checked_add(row_bytes) else {
            return false;
        };
        let Some(row) = bytes.get(start..end) else {
            return false;
        };
        for pixel in row.chunks_exact(channels) {
            if channels == 4 && pixel[3] < 128 {
                continue;
            }
            let base = *background.get_or_insert([pixel[0], pixel[1], pixel[2]]);
            if (0..3).any(|channel| pixel[channel].abs_diff(base[channel]) >= 16) {
                contrasting += 1;
            }
        }
    }
    let Some(area) = width.checked_mul(height) else {
        return false;
    };
    contrasting >= (area / 1000).max(32)
}

fn cancel_owned_dialogs() {
    // This executable owns only the disposable fixture's GTK objects. In particular,
    // unwind Dialog::run even if the expected title/parent did not match the real modal.
    for dialog in gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|window| window.downcast::<gtk::Dialog>().ok())
    {
        dialog.response(gtk::ResponseType::Cancel);
    }
}

struct PrivateSettingsDestination {
    profile: PathBuf,
    device: u64,
    inode: u64,
}
impl PrivateSettingsDestination {
    fn new(profile: &Path) -> Result<Self> {
        let profile = profile.canonicalize()?;
        ensure!(
            crate::paths::config_dir()?.canonicalize()? == profile,
            "Persistence fixture must use the gated private profile."
        );
        let metadata = fs::symlink_metadata(&profile)?;
        let destination = Self {
            profile,
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        destination.validate()?;
        Ok(destination)
    }
    fn validate(&self) -> Result<()> {
        let metadata = fs::symlink_metadata(&self.profile)?;
        ensure!(
            metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && metadata.dev() == self.device
                && metadata.ino() == self.inode
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.permissions().mode() & 0o077 == 0,
            "Persistence fixture private profile identity changed."
        );
        Ok(())
    }
}

/// Keep the original inode, not a reconstructed Config or a truncating rewrite.
/// Backup directories deliberately survive any restoration error; Drop must not
/// delete the only remaining copy of the settings file.
struct SettingsObstruction {
    destination: PathBuf,
    backup: PathBuf,
    original: fs::Metadata,
    blocker: Option<fs::Metadata>,
    obstructed: bool,
    profile_device: u64,
    profile_inode: u64,
    moved: bool,
}
impl SettingsObstruction {
    fn install(profile: &PrivateSettingsDestination) -> Result<Self> {
        profile.validate()?;
        let destination = profile.profile.join("settings.json");
        ensure!(
            crate::config::config_path()?.canonicalize()? == destination,
            "Only the real private settings destination may be obstructed."
        );
        let original = fs::symlink_metadata(&destination)?;
        ensure!(
            original.is_file()
                && !original.file_type().is_symlink()
                && original.nlink() == 1
                && original.uid() == unsafe { libc::geteuid() }
                && original.permissions().mode() & 0o077 == 0,
            "Persistence fixture requires an owned private regular settings file."
        );
        let backup = tempfile::Builder::new()
            .prefix(".native-save-backup-")
            .tempdir_in(&profile.profile)?;
        let staged_blocker = backup.path().join("obstruction");
        fs::DirBuilder::new().mode(0o700).create(&staged_blocker)?;
        let blocker = fs::symlink_metadata(&staged_blocker)?;
        let mut guard = Self {
            destination,
            backup: backup.keep(),
            original,
            blocker: Some(blocker),
            obstructed: false,
            profile_device: profile.device,
            profile_inode: profile.inode,
            moved: false,
        };
        fs::rename(&guard.destination, guard.backup.join("settings.json"))?;
        guard.moved = true;
        fs::rename(staged_blocker, &guard.destination)?;
        guard.obstructed = true;
        Ok(guard)
    }
    fn restore(&mut self) -> Result<()> {
        let parent = self
            .destination
            .parent()
            .context("Missing private profile")?;
        let profile = fs::symlink_metadata(parent)?;
        ensure!(
            profile.is_dir()
                && profile.dev() == self.profile_device
                && profile.ino() == self.profile_inode
                && profile.uid() == unsafe { libc::geteuid() }
                && profile.permissions().mode() & 0o077 == 0,
            "Settings restoration requires the unchanged private profile."
        );
        if let Some(blocker) = &self.blocker {
            let staged;
            let path = if self.obstructed {
                &self.destination
            } else {
                staged = self.backup.join("obstruction");
                &staged
            };
            let metadata = fs::symlink_metadata(path)?;
            ensure!(
                metadata.is_dir()
                    && metadata.dev() == blocker.dev()
                    && metadata.ino() == blocker.ino(),
                "Settings obstruction identity changed; retain original backup."
            );
            fs::remove_dir(path)?;
            self.blocker = None;
        }
        if self.moved {
            let saved = self.backup.join("settings.json");
            let metadata = fs::symlink_metadata(&saved)?;
            ensure!(
                metadata.is_file()
                    && metadata.dev() == self.original.dev()
                    && metadata.ino() == self.original.ino(),
                "Preserved settings identity changed; retain backup without overwriting."
            );
            ensure!(
                matches!(
                    fs::symlink_metadata(&self.destination),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound
                ),
                "Settings destination is occupied; retain original backup."
            );
            fs::rename(saved, &self.destination)?;
            self.moved = false;
        }
        match fs::remove_dir(&self.backup) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }
}
impl Drop for SettingsObstruction {
    fn drop(&mut self) {
        if self.restore().is_err() {
            eprintln!("[native-linux-smoke] settings restoration failed; original backup retained");
        }
    }
}

const PERSISTENCE_INSTRUCTIONS: &str = "  recuperação e\u{301} 👩‍💻\n  保持原文  \n";

fn persistence_controls() -> [(&'static str, Value); 5] {
    [
        ("custom_instructions", json!(PERSISTENCE_INSTRUCTIONS)),
        ("debounce_ms", json!(900)),
        ("writing_language", json!("en-GB")),
        ("history_retention_days", json!(14)),
        ("complete_inside_text", json!(true)),
    ]
}

struct Scenario {
    step: u32,
    reported_step: Option<u32>,
    entered: Instant,
    started: Instant,
    awaiting_owner_since: Option<Instant>,
    output: PathBuf,
    checks: Vec<String>,
    images: Vec<String>,
    choice_result: Value,
    catalog_details: Vec<Value>,
    catalog_before: Option<Config>,
    before: String,
    completed: bool,
    failure: Option<String>,
    cleanup_at: Rc<Cell<Option<Instant>>>,
    prompt_error: Rc<RefCell<Option<String>>>,
    answered_prompts: Rc<Cell<u32>>,
    context_consents_staged: bool,
    concurrent_apply: Option<PendingConfigApply>,
    selection_before: Option<Config>,
    diagnostics_before: Option<Value>,
    persistence_destination: PrivateSettingsDestination,
    persistence_obstruction: Option<SettingsObstruction>,
    persistence_before: Option<Config>,
    persistence_receipts: Vec<Value>,
    persistence_restored: bool,
}
impl Scenario {
    fn next(&mut self) {
        self.step += 1;
        self.entered = Instant::now();
    }
    fn check(&mut self, condition: bool, name: &str) -> Result<()> {
        ensure!(condition, "{name}");
        self.checks.push(name.into());
        Ok(())
    }
    fn wait(&self, ready: bool, seconds: u64, what: &str) -> Result<bool> {
        if !ready {
            ensure!(
                self.entered.elapsed() < Duration::from_secs(seconds),
                "Timed out waiting for {what}."
            );
        }
        Ok(ready)
    }
    fn welcome_page_ready(&self, s: &Shell, step: usize) -> Result<bool> {
        // Read the real stack through the owned field's native ancestry rather
        // than treating the navigation index alone as presentation evidence.
        let stack = s
            .welcome
            .smoke_field()
            .ancestor(gtk::Stack::static_type())
            .and_then(|widget| widget.downcast::<gtk::Stack>().ok())
            .context("Owned Welcome field has no native page stack.")?;
        let page = step.to_string();
        Ok(s.welcome.step() == step
            && s.welcome.window.is_visible()
            && s.welcome.window.is_mapped()
            && stack.visible_child_name().as_deref() == Some(page.as_str())
            && stack
                .visible_child()
                .is_some_and(|child| child.is_visible() && child.is_mapped()))
    }
    fn wait_welcome_page(&self, s: &Shell, step: usize) -> Result<bool> {
        if self.welcome_page_ready(s, step)? {
            return Ok(true);
        }
        self.wait(
            false,
            5,
            &format!(
                "mapped native Welcome page {step} (actual index {})",
                s.welcome.step()
            ),
        )
    }
    fn terminal(
        &mut self,
        s: &mut Shell,
        expected: TerminalReceipt,
        seconds: u64,
        name: &str,
    ) -> Result<bool> {
        if let Some(error) = s.smoke_receipt.error {
            bail!(error);
        }
        if !self.wait(s.smoke_receipt.receipt.is_some(), seconds, name)? {
            return Ok(false);
        }
        let observation = std::mem::take(&mut s.smoke_receipt);
        let id = observation
            .operation
            .context("Missing submitted operation ID")?;
        ensure!(
            !s.operations
                .iter()
                .any(|op| op.pending.operation_id() == id),
            "Observed operation {id:?} was not retired by the shell."
        );
        let receipt = observation.receipt.context("Missing terminal receipt")?;
        let matches = matches!(
            (&expected, &receipt),
            (TerminalReceipt::Saved, ConfigApplyReceipt::Saved { .. })
                | (TerminalReceipt::Failed, ConfigApplyReceipt::Failed { .. })
                | (
                    TerminalReceipt::PersistenceFailed,
                    ConfigApplyReceipt::Failed {
                        stage: crate::runtime::ConfigApplyStage::Persistence,
                        ..
                    }
                )
                | (TerminalReceipt::Cancelled, ConfigApplyReceipt::Cancelled)
        );
        if !matches {
            let actual = match &receipt {
                ConfigApplyReceipt::Saved { .. } => "Saved".to_owned(),
                ConfigApplyReceipt::Applied { .. } => "Applied".to_owned(),
                ConfigApplyReceipt::Failed { stage, .. } => {
                    format!("Failed ({stage:?})")
                }
                ConfigApplyReceipt::Cancelled => "Cancelled".to_owned(),
                ConfigApplyReceipt::Superseded => "Superseded".to_owned(),
                ConfigApplyReceipt::Stopped => "Stopped".to_owned(),
            };
            bail!("Operation {id:?}: expected {expected:?}, received {actual} ({name}).");
        }
        self.check(matches, &format!("{name} ({id:?})"))?;
        if self.step == 40 || self.step == 41 {
            self.persistence_receipts.push(json!({
                "operation": format!("{id:?}"),
                "receipt": if matches!(receipt, ConfigApplyReceipt::Saved { .. }) {
                    "Saved"
                } else {
                    "Failed(Persistence)"
                },
            }));
        }
        if let ConfigApplyReceipt::Saved { config } = receipt {
            self.check(
                config == s.controller.config() && config == crate::config::load_config()?,
                "the exact Saved receipt matches current and independently reloaded configuration",
            )?;
        }
        Ok(true)
    }
    fn selection_retained(&mut self, s: &Shell, name: &str) -> Result<()> {
        let baseline = self
            .selection_before
            .as_ref()
            .context("Missing model-selection baseline")?;
        let unchanged = &s.controller.config() == baseline
            && &crate::config::load_config()? == baseline
            && s.settings.draft.borrow().is_dirty()
            && s.settings.draft.borrow().value("custom_instructions")
                == Some(&json!("  ação e\u{301} 👩‍💻\n"));
        self.check(unchanged, name)
    }
    fn persistence_controls_retained(&mut self, s: &Shell, dirty: bool) -> Result<()> {
        for (field, value) in persistence_controls() {
            self.check(
                s.settings.smoke_control_matches(field, &value)?
                    && s.settings.draft.borrow().value(field) == Some(&value),
                &format!("persistence recovery retains exact native control and draft: {field}"),
            )?;
        }
        self.check(
            if dirty {
                s.settings.smoke_unsaved_controls_ready()
            } else {
                s.settings.smoke_saved_controls_ready()
            },
            if dirty {
                "failed persistence retains editable dirty controls and enabled Save/Revert"
            } else {
                "only the recovery Saved receipt cleans the confirmed native draft"
            },
        )
    }
    fn restore_persistence_destination(&mut self) -> Result<()> {
        if let Some(obstruction) = self.persistence_obstruction.as_mut() {
            self.persistence_destination.validate()?;
            obstruction.restore()?;
            self.persistence_obstruction = None;
            self.persistence_restored = true;
        }
        Ok(())
    }
    fn capture_catalog_memory(&mut self, s: &Shell, specified: bool) -> Result<()> {
        let filename = if specified {
            "models-memory-specified.png"
        } else {
            "models-memory-unspecified.png"
        };
        self.capture(&s.models.window, filename)?;
        let mut detail = s.models.smoke_catalog_detail()?;
        let minimum = detail["catalog_entry"]["minimum_memory_bytes"]
            .as_u64()
            .context("Selected catalogue entry has no minimum-memory metadata")?;
        let text = detail["detail_text"]
            .as_str()
            .context("Native model description is unreadable")?;
        self.check(
            (minimum > 0) == specified
                && detail["mapped"] == true
                && detail["width"].as_i64().is_some_and(|width| width > 0)
                && detail["height"].as_i64().is_some_and(|height| height > 0)
                && text.contains("Catalog minimum physical memory: unavailable") == !specified
                && !text.contains("Catalog minimum physical memory: 0 "),
            if specified {
                "real Models dialog renders a specified positive catalogue memory threshold"
            } else {
                "real Models dialog does not present unspecified catalogue memory as numeric zero"
            },
        )?;
        detail["screenshot"] = json!(filename);
        detail["minimum_memory_specified"] = json!(specified);
        self.catalog_details.push(detail);
        Ok(())
    }
    fn capture(&mut self, window: &gtk::Window, name: &str) -> Result<()> {
        self.capture_drawable(window, None, name)
    }
    fn capture_drawable(
        &mut self,
        window: &gtk::Window,
        area: Option<&gtk::DrawingArea>,
        name: &str,
    ) -> Result<()> {
        let pixels = rendered_snapshot(window, area)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.output.join(name))?;
        file.write_all(&pixels.save_to_bufferv("png", &[])?)?;
        file.sync_all()?;
        self.images.push(name.into());
        eprintln!(
            "[native-linux-smoke] captured {name} ({}x{})",
            pixels.width(),
            pixels.height()
        );
        Ok(())
    }
    fn choice_subsequence(&mut self, s: &Shell) -> Result<()> {
        let (sender, receiver) = mpsc::channel();
        let presentation = overlay::Presentation::new(&sender);
        let Some((window, area)) = presentation.smoke_choice_surface() else {
            self.choice_result = json!({
                "outcome": "unexercised",
                "reason": "Missing prerequisite: composited X11 display with input-shape support"
            });
            return Ok(());
        };
        self.choice_result =
            json!({"outcome":"failed","reason":"Native chooser setup did not complete"});
        let native = s
            .settings
            .window
            .window()
            .context("Settings drawable unavailable")?;
        let monitor = native
            .display()
            .monitor_at_window(&native)
            .context("Settings monitor unavailable")?;
        let bounds = monitor.workarea();
        let scale = f64::from(monitor.scale_factor());
        let snapshot = crate::core::Snapshot::default();
        // Keep the exact whitespace/decomposed-scalar witnesses, but make the
        // rejected middle offer valid at the producer boundary. Its exclusion
        // must come from real native geometry, not an impossible offer size.
        let choices = [
            "  first  ".to_owned(),
            "W".repeat(160),
            " e\u{301} last ".to_owned(),
        ];
        let event = json!({
            "key": snapshot.key(), "authority": snapshot.authority(),
            "revision": 901, "mode": "alternatives",
            "rect": [
                f64::from(bounds.x() + 24) * scale,
                f64::from(bounds.y() + 24) * scale,
                scale, 20. * scale
            ],
            "choices": choices
        });
        let result = (|| -> Result<()> {
            ensure!(
                choices
                    .iter()
                    .all(|value| crate::features::safe_choice(value)),
                "X11 chooser fixture violates the producer choice envelope."
            );
            let layout = area.create_pango_layout(Some(&format!("2. {}", choices[1])));
            layout.set_single_paragraph_mode(false);
            layout.set_width(-1);
            layout.set_ellipsize(pango::EllipsizeMode::None);
            let (ink, logical) = layout.pixel_extents();
            let middle_width = (ink.x() + ink.width()).max(logical.x() + logical.width()) + 16;
            ensure!(
                layout.unknown_glyphs_count() == 0 && ink.x() >= -8 && ink.y() >= -6,
                "X11 chooser middle row must have supported, unclipped native glyphs."
            );
            ensure!(
                middle_width > bounds.width(),
                "X11 chooser geometry prerequisite: producer-valid middle row is {middle_width}px wide, but the real workarea permits {}px.",
                bounds.width()
            );
            // This isolated surface emits paint receipts into our own channel;
            // no synthetic offer is submitted for controller acceptance.
            presentation.present(event, &s.controller);
            ensure!(
                window.is_visible(),
                "X11 chooser presentation was refused: window remained hidden (native_presentation_owner={}, native_event_source={}).",
                s.controller.uses_native_presentation(),
                s.controller.native_event_source().is_some(),
            );
            wait_for_native_map(&window)?;
            self.capture_drawable(&window, Some(&area), "choices-eligible-subsequence.png")?;
            let expected = "  first  \n e\u{301} last ";
            let mut acknowledgments = 0;
            for action in receiver.try_iter() {
                if let UiAction::Command(Command::PresentationResult {
                    key,
                    authority,
                    revision,
                    visible_text,
                    mode,
                }) = action
                {
                    ensure!(
                        key == snapshot.key()
                            && authority == snapshot.authority()
                            && revision == 901
                            && mode == "alternatives"
                            && visible_text == expected,
                        "X11 draw acknowledged an altered, clipped or noneligible choice."
                    );
                    acknowledgments += 1;
                }
            }
            self.check(
                acknowledgments != 0,
                "actual X11 draw acknowledges exact offers 0 and 2 after skipping an overlong middle row",
            )?;
            self.check(
                !window.is_active()
                    && !window.has_toplevel_focus()
                    && window
                        .window()
                        .and_then(|native| overlay::keyboard_focus_within(&native))
                        == Some(false)
                    && presentation.smoke_choice_draw_evidence()["at_draw"]["native"]
                        ["keyboard_focus_within"]
                        == false,
                "compacted mouse-through X11 chooser and its native children have no server keyboard focus at draw or readback",
            )?;
            Ok(())
        })();
        // Capture the real draw's guard operands and map/configure ordering.
        // The later GTK getters in this receipt are not rejection-point proof.
        let draw_evidence = presentation.smoke_choice_draw_evidence();
        presentation.clear();
        window.close();
        self.choice_result = match &result {
            Ok(()) => json!({
                "outcome":"passed",
                "screenshot":"choices-eligible-subsequence.png",
                "readback":"owned-drawing-area-native-drawable",
                "draw":draw_evidence,
            }),
            Err(error) => json!({
                "outcome":"failed",
                "reason":error.to_string(),
                "readback":"owned-drawing-area-native-drawable",
                "draw":draw_evidence,
            }),
        };
        result
    }
    fn answer(&self, parent: &gtk::Window, title: &str, response: gtk::ResponseType) {
        eprintln!(
            "[native-linux-smoke] stage={} awaiting modal {title:?}, parent={:?}",
            self.step,
            parent.title()
        );
        let parent = parent.clone();
        let title = title.to_owned();
        let failure = self.prompt_error.clone();
        let answered = self.answered_prompts.clone();
        let deadline = Instant::now() + Duration::from_secs(10);
        // Dialog::run does not hide its window, and Window::close queues an idle
        // delete event. Consecutive same-title consents can briefly coexist.
        let existing = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|window| window.downcast::<gtk::Dialog>().ok())
            .collect::<Vec<_>>();
        glib::timeout_add_local(Duration::from_millis(10), move || {
            let dialog = gtk::Window::list_toplevels()
                .into_iter()
                .filter_map(|window| window.downcast::<gtk::Dialog>().ok())
                .find(|dialog| {
                    !existing.contains(dialog) && dialog.is_visible() && dialog.is_mapped()
                });
            if let Some(dialog) = dialog {
                let actual_parent = dialog.transient_for();
                if dialog.title().as_deref() == Some(title.as_str())
                    && actual_parent.as_ref() == Some(&parent)
                {
                    let button = dialog
                        .widget_for_response(response)
                        .and_then(|widget| widget.downcast::<gtk::Button>().ok())
                        .filter(|button| button.is_visible() && button.is_sensitive());
                    if let Some(button) = button {
                        eprintln!(
                            "[native-linux-smoke] answering actual modal {title:?}: {response:?}"
                        );
                        button.clicked();
                        answered.set(answered.get() + 1);
                    } else {
                        *failure.borrow_mut() = Some(format!(
                            "Expected actual GTK modal response button is unavailable: {title}, {response:?}"
                        ));
                        cancel_owned_dialogs();
                    }
                } else {
                    *failure.borrow_mut() = Some(format!(
                        "Unexpected owned GTK modal: title={:?}, parent={:?}; expected {title:?}, parent={:?}",
                        dialog.title(),
                        actual_parent.and_then(|window| window.title()),
                        parent.title(),
                    ));
                    cancel_owned_dialogs();
                }
                glib::ControlFlow::Break
            } else if Instant::now() >= deadline {
                *failure.borrow_mut() =
                    Some(format!("Expected actual GTK modal was not shown: {title}"));
                cancel_owned_dialogs();
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }
    fn tick(&mut self, s: &mut Shell) -> Result<()> {
        if let Some(since) = self.awaiting_owner_since {
            if !s.smoke_owner_turn_completed {
                ensure!(
                    since.elapsed() < Duration::from_secs(5),
                    "Timed out waiting for the shell owner to process queued native actions before stage {}.",
                    self.step
                );
                return Ok(());
            }
        }
        self.advance(s)?;
        // Paint/modal loops can run the fixture timer before the shell's timer.
        // Only the normal owner may process actions; no nested pumping, second
        // queue consumer or repeated click can establish this boundary.
        s.smoke_owner_turn_completed = false;
        self.awaiting_owner_since = Some(Instant::now());
        Ok(())
    }
    fn advance(&mut self, s: &mut Shell) -> Result<()> {
        if self.reported_step != Some(self.step) {
            self.reported_step = Some(self.step);
            eprintln!("[native-linux-smoke] entering stage={}", self.step);
        }
        if let Some(error) = self.prompt_error.borrow_mut().take() {
            bail!(error);
        }
        match self.step {
            0 => {
                if !self.wait(s.settings.window.is_mapped(), 10, "Settings mapping")? {
                    return Ok(());
                }
                self.check(
                    !s.tray_available && s.settings.window.is_visible(),
                    "no-tray launch retains visible Settings and Quit recovery",
                )?;
                self.check(
                    !s.controller.config().global_enabled,
                    "normal global capture is disabled throughout the owned-widget fixture",
                )?;
                self.check(s.settings.smoke_inventory(),"every Config field has its canonical native control or explicit service editor")?;
                self.choice_subsequence(s)?;
                s.settings.show_pane(PANES[0]);
                self.next();
            }
            1..=6 => {
                let index = (self.step - 1) as usize;
                if !self.wait(
                    s.settings.smoke_pane_ready(PANES[index]),
                    5,
                    "the selected native Settings pane to map",
                )? {
                    return Ok(());
                }
                self.check(
                    s.settings.smoke_pane() == PANES[index],
                    &format!("six-pane navigation: {}", PANES[index]),
                )?;
                self.capture(
                    &s.settings.window,
                    &format!(
                        "settings-{}.png",
                        PANES[index].to_lowercase().replace(' ', "-")
                    ),
                )?;
                s.settings.show_pane(if index < 5 {
                    PANES[index + 1]
                } else {
                    "General"
                });
                self.next();
            }
            7 => {
                s.settings.smoke_control("debounce_ms", json!(900))?;
                self.check(
                    s.settings.draft.borrow().is_dirty()
                        && s.controller.config().debounce_ms == 100,
                    "native draft edits do not modify the controller before Save",
                )?;
                s.settings.smoke_revert();
                self.next();
            }
            8 => {
                if !self.wait(
                    !s.settings.draft.borrow().is_dirty(),
                    5,
                    "queued Revert to reset the native draft",
                )? {
                    return Ok(());
                }
                self.check(
                    !s.settings.draft.borrow().is_dirty()
                        && s.controller.config().debounce_ms == 100,
                    "Revert discards the native draft without persistence",
                )?;
                s.settings.smoke_control("debounce_ms", json!(500))?;
                s.smoke_receipt.arm()?;
                s.settings.smoke_save();
                self.next();
            }
            9 => {
                if !self.terminal(
                    s,
                    TerminalReceipt::Saved,
                    30,
                    "Save consumes its exact Saved receipt",
                )? {
                    return Ok(());
                }
                self.check(
                    s.controller.config().debounce_ms == 500
                        && !s.settings.draft.borrow().is_dirty(),
                    "Save applies only after controller-confirmed persistence",
                )?;
                self.check(
                    crate::config::load_config()?.debounce_ms == 500,
                    "Save is readable from the isolated settings file",
                )?;
                s.settings.show_pane("Languages");
                s.settings.smoke_control(
                    "custom_instructions",
                    json!("Use concise sentences in this synthetic test."),
                )?;
                let mut concurrent = s.controller.config();
                concurrent.complete_inside_text = false;
                self.concurrent_apply = Some(s.controller.apply_settings(
                    SettingsDraft::from_configs(&s.controller.config(), &concurrent)?,
                    false,
                )?);
                self.next();
            }
            10 => {
                let receipt = self
                    .concurrent_apply
                    .as_ref()
                    .context("Missing concurrent operation")?
                    .try_recv()?;
                if !self.wait(receipt.is_some(), 30, "concurrent Applied receipt")? {
                    return Ok(());
                }
                self.check(
                    matches!(receipt, Some(ConfigApplyReceipt::Applied { .. })),
                    "nonpersisting operation returns its own Applied receipt",
                )?;
                self.concurrent_apply.take();
                let candidate = s.settings.draft.borrow().apply_to(&s.controller.config())?;
                self.check(!candidate.complete_inside_text&&candidate.custom_instructions=="Use concise sentences in this synthetic test.","live configuration rebases without losing the dirty native instructions editor")?;
                s.settings.show_pane("Autocomplete");
                s.settings.window.resize(1020, 740);
                s.settings.show_pane("Languages");
                s.smoke_receipt.arm()?;
                s.settings.smoke_save();
                self.next();
            }
            11 => {
                if !self.terminal(
                    s,
                    TerminalReceipt::Saved,
                    30,
                    "instruction Save consumes its exact Saved receipt",
                )? {
                    return Ok(());
                }
                let saved = crate::config::load_config()?;
                self.check(
                    !s.settings.draft.borrow().is_dirty(),
                    "the instruction Saved receipt acknowledges the submitted draft",
                )?;
                self.check(
                    !saved.complete_inside_text
                        && saved.custom_instructions
                            == "Use concise sentences in this synthetic test.",
                    "Save preserves concurrent changes across pane navigation and resizing",
                )?;
                s.settings.show_pane("Privacy");
                self.answer(
                    &s.parent(),
                    "Enable encrypted local learning?",
                    gtk::ResponseType::Cancel,
                );
                s.settings.smoke_history(true);
                self.next();
            }
            12 => {
                self.check(!s.controller.config().record_history&&!s.settings.smoke_history_checked(),"Cancel on the actual history consent dialog leaves recording disabled and its native switch unchecked")?;
                self.answer(
                    &s.settings.window,
                    "Allow optional local context?",
                    gtk::ResponseType::Cancel,
                );
                s.settings.smoke_control("clipboard_context", json!(true))?;
                self.check(
                    s.settings.draft.borrow().value("clipboard_context") == Some(&json!(false)),
                    "declining the actual clipboard consent dialog leaves the draft disabled",
                )?;
                self.answer(
                    &s.settings.window,
                    "Allow optional local context?",
                    gtk::ResponseType::Accept,
                );
                s.settings.smoke_control("clipboard_context", json!(true))?;
                self.check(
                    s.settings.draft.borrow().value("clipboard_context") == Some(&json!(true))
                        && s.settings.draft.borrow().value("screen_context") == Some(&json!(false)),
                    "clipboard consent is independent of Accessibility context",
                )?;
                self.check(
                    self.answered_prompts.get() == 3,
                    "consecutive same-title clipboard dialogs receive distinct Cancel and Continue button responses",
                )?;
                self.next();
            }
            13 => {
                if !self.context_consents_staged {
                    self.answer(
                        &s.settings.window,
                        "Allow optional local context?",
                        gtk::ResponseType::Accept,
                    );
                    s.settings.smoke_control("screen_context", json!(true))?;
                    s.settings
                        .smoke_control("clipboard_context", json!(false))?;
                    self.check(s.settings.draft.borrow().value("screen_context")==Some(&json!(true))&&s.settings.draft.borrow().value("clipboard_context")==Some(&json!(false)),"revoking clipboard consent does not revoke the independent Accessibility draft")?;
                    self.context_consents_staged = true;
                    self.entered = Instant::now();
                    // Release the shell borrow before observing its status
                    // reflection and native frame. Consent actions run once.
                    return Ok(());
                }
                let draft = s.settings.draft.borrow();
                let dirty = draft.is_dirty();
                let draft_consents_staged = draft.value("screen_context") == Some(&json!(true))
                    && draft.value("clipboard_context") == Some(&json!(false));
                drop(draft);
                let clipboard_checked = s.settings.smoke_check_active("clipboard_context")?;
                let screen_checked = s.settings.smoke_check_active("screen_context")?;
                let unsaved_controls_ready = s.settings.smoke_unsaved_controls_ready();
                let staged = dirty
                    && draft_consents_staged
                    && !clipboard_checked
                    && screen_checked
                    && unsaved_controls_ready;
                if !self
                    .wait(
                        staged,
                        5,
                        "native context checkboxes and unsaved controls to reflect staged consents",
                    )
                    .map_err(|error| {
                        anyhow::anyhow!(
                            "{error} (stage={}, dirty={dirty}, draft_consents_staged={draft_consents_staged}, clipboard_checked={clipboard_checked}, screen_checked={screen_checked}, unsaved_controls_ready={unsaved_controls_ready})",
                            self.step,
                        )
                    })?
                {
                    return Ok(());
                }
                self.check(
                    staged,
                    "native clipboard is unchecked, Accessibility is checked and unsaved controls reflect the independent consent draft before capture",
                )?;
                self.check(
                    self.answered_prompts.get() == 4,
                    "staged context evidence follows exactly the four expected consent responses",
                )?;
                self.capture(&s.settings.window, "privacy-independent-consents.png")?;
                self.check(!s.controller.config().clipboard_context&&!s.controller.config().screen_context&&!s.controller.config().record_history&&!s.controller.config().global_enabled,"controller context, recording and normal global capture remain disabled while native consent drafts are edited")?;
                s.settings.smoke_revert();
                self.next();
            }
            14 => {
                let dirty = s.settings.draft.borrow().is_dirty();
                let clipboard_checked = s.settings.smoke_check_active("clipboard_context")?;
                let screen_checked = s.settings.smoke_check_active("screen_context")?;
                // The preceding screenshot dispatched a nested GTK loop while
                // holding the shell borrow. Its timers may have run without
                // draining the action queued by the one real Revert click.
                // Release ownership until reset and widget reflection are both
                // observed; never repeat the action or manufacture a reset.
                if !self
                    .wait(
                        !dirty && !clipboard_checked && !screen_checked,
                        5,
                        "queued Revert to reset the draft and native context checkboxes",
                    )
                    .map_err(|error| {
                        anyhow::anyhow!(
                            "{error} (stage={}, dirty={dirty}, clipboard_checked={clipboard_checked}, screen_checked={screen_checked}, pending_operations={})",
                            self.step,
                            s.operations.len(),
                        )
                    })?
                {
                    return Ok(());
                }
                self.check(
                    !dirty
                        && s.settings.draft.borrow().value("clipboard_context")
                            == Some(&json!(false))
                        && s.settings.draft.borrow().value("screen_context") == Some(&json!(false)),
                    "Revert revokes unsaved independent context consents",
                )?;
                self.check(
                    !clipboard_checked
                        && !screen_checked
                        && !s.settings.smoke_history_checked(),
                    "Revert restores unchecked native clipboard, Accessibility and recording controls",
                )?;
                let current = s.controller.config();
                self.check(
                    !current.clipboard_context
                        && !current.screen_context
                        && !current.record_history
                        && !current.global_enabled,
                    "Revert leaves controller context, recording and normal global capture disabled",
                )?;
                s.settings.show_pane("General");
                s.settings.smoke_control("debounce_ms", json!(800))?;
                self.answer(
                    &s.parent(),
                    "Save before closing?",
                    gtk::ResponseType::Cancel,
                );
                s.settings.window.close();
                self.next();
            }
            15 => {
                self.check(
                    s.settings.window.is_visible()
                        && s.settings.draft.borrow().is_dirty()
                        && !s.quitting,
                    "Cancel on dirty-window close keeps the resident controls and draft",
                )?;
                self.answer(
                    &s.parent(),
                    "Save before quitting?",
                    gtk::ResponseType::Cancel,
                );
                click_button(&s.settings.window, "action_quit")?;
                self.next();
            }
            16 => {
                self.check(
                    s.settings.window.is_visible()
                        && s.settings.draft.borrow().is_dirty()
                        && !s.quitting,
                    "guarded Quit Cancel does not discard edits or begin shutdown",
                )?;
                s.settings.smoke_revert();
                s.sender.send(UiAction::Welcome)?;
                self.next();
            }
            17 => {
                if !self.wait_welcome_page(s, 0)? {
                    return Ok(());
                }
                self.check(
                    s.welcome.window.is_visible() && s.welcome.step() == 0,
                    "Welcome opens its canonical first step",
                )?;
                self.capture(&s.welcome.window, "welcome-1.png")?;
                s.welcome.smoke_next();
                self.next();
            }
            18 => {
                if !self.wait_welcome_page(s, 1)? {
                    return Ok(());
                }
                self.check(s.welcome.step() == 1, "Welcome local-privacy step")?;
                self.capture(&s.welcome.window, "welcome-2.png")?;
                s.welcome.smoke_back();
                self.step = 180;
                self.entered = Instant::now();
            }
            180 => {
                if !self.wait_welcome_page(s, 0)? {
                    return Ok(());
                }
                self.check(
                    s.welcome.step() == 0,
                    "Welcome Back navigates through the canonical native button",
                )?;
                s.welcome.smoke_next();
                self.step = 181;
                self.entered = Instant::now();
            }
            181 => {
                if !self.wait_welcome_page(s, 1)? {
                    return Ok(());
                }
                self.check(
                    s.welcome.step() == 1,
                    "Welcome returns forward after native Back",
                )?;
                s.welcome.smoke_next();
                self.step = 19;
                self.entered = Instant::now();
            }
            19 => {
                if !self.wait_welcome_page(s, 2)? {
                    return Ok(());
                }
                self.check(s.welcome.step() == 2, "Welcome system-access step")?;
                s.welcome.smoke_next();
                self.next();
            }
            20 => {
                if !self.wait_welcome_page(s, 2)? {
                    return Ok(());
                }
                self.check(
                    s.welcome.step() == 2 && !s.welcome.can_continue(&s.controller.config()),
                    "Welcome cannot bypass explicit access review",
                )?;
                s.welcome.smoke_review(true);
                self.step = 200;
                self.entered = Instant::now();
            }
            200 => {
                if !self.wait(
                    self.welcome_page_ready(s, 2)?
                        && s.welcome.can_continue(&s.controller.config()),
                    5,
                    "native access review and Continue control reflection",
                )? {
                    return Ok(());
                }
                self.capture(&s.welcome.window, "welcome-3.png")?;
                s.welcome.smoke_next();
                self.step = 21;
                self.entered = Instant::now();
            }
            21 => {
                if !self.wait_welcome_page(s, 3)? {
                    return Ok(());
                }
                self.check(
                    s.welcome.step() == 3 && model_selected(&s.controller.config()),
                    "Welcome model step references the explicitly supplied existing GGUF",
                )?;
                self.check(
                    !s.welcome.preview_active(),
                    "selecting a model alone does not create a ready preview session",
                )?;
                self.capture(&s.welcome.window, "welcome-4.png")?;
                s.welcome.smoke_next();
                self.next();
            }
            22 => {
                if !self.wait(
                    self.welcome_page_ready(s, 4)? && s.welcome.smoke_field().has_focus(),
                    10,
                    "owned Welcome field focus",
                )? {
                    return Ok(());
                }
                let field = s.welcome.smoke_field();
                let buffer = field.buffer().context("Missing owned preview document")?;
                buffer.set_text("I would like to");
                buffer.place_cursor(&buffer.end_iter());
                self.next();
            }
            23 => {
                let result = s.welcome.smoke_result();
                if !self.wait(
                    s.welcome.smoke_drawn() && !text(&result, "suggestion").is_empty(),
                    75,
                    "actual English inference and native ghost drawing",
                )? {
                    return Ok(());
                }
                self.check(
                    s.controller.status()["preview_suspended"] == true,
                    "owned inference waits for the normal controller worker to release",
                )?;
                self.capture(&s.welcome.window, "preview-english-visible.png")?;
                let field = s.welcome.smoke_field();
                self.before = field_text(&field)?;
                self.check(
                    key(&field, false)?,
                    "native GTK acceptance key dispatch was handled by the actual Welcome field",
                )?;
                self.next();
            }
            24 => {
                let field = s.welcome.smoke_field();
                let after = field_text(&field)?;
                let result = s.welcome.smoke_result();
                if !self.wait(
                    after != self.before && result["accepted"].as_u64().unwrap_or(0) >= 1,
                    10,
                    "confirmed English widget readback",
                )? {
                    return Ok(());
                }
                self.check(after.starts_with(&self.before),"English acceptance preserves authored prefix and is counted only after actual GTK document readback")?;
                self.capture(&s.welcome.window, "preview-english-accepted.png")?;
                let buffer = field.buffer().unwrap();
                buffer.set_text("Amanhã vamos");
                buffer.place_cursor(&buffer.end_iter());
                self.next();
            }
            25 => {
                let result = s.welcome.smoke_result();
                if !self.wait(
                    s.welcome.smoke_drawn() && !text(&result, "suggestion").is_empty(),
                    75,
                    "actual Portuguese inference and native ghost drawing",
                )? {
                    return Ok(());
                }
                self.capture(&s.welcome.window, "preview-portuguese-visible.png")?;
                let field = s.welcome.smoke_field();
                self.before = field_text(&field)?;
                self.check(key(&field,true)?,"configured Above-Tab scan identity reaches the canonical full-accept GTK handler")?;
                self.next();
            }
            26 => {
                let field = s.welcome.smoke_field();
                let after = field_text(&field)?;
                let result = s.welcome.smoke_result();
                if !self.wait(
                    after != self.before && result["accepted"].as_u64().unwrap_or(0) >= 2,
                    10,
                    "confirmed Portuguese widget readback",
                )? {
                    return Ok(());
                }
                self.check(after.starts_with(&self.before),"Portuguese full acceptance preserves the document and receives native readback confirmation")?;
                self.capture(&s.welcome.window, "preview-portuguese-accepted.png")?;
                let buffer = field.buffer().unwrap();
                buffer.select_range(&buffer.start_iter(), &buffer.iter_at_offset(3));
                self.next();
            }
            27 => {
                if !self.wait(
                    !s.welcome.smoke_drawn()
                        && text(&s.welcome.smoke_result(), "suggestion").is_empty(),
                    10,
                    "native selection to invalidate the owned preview",
                )? {
                    return Ok(());
                }
                self.check(
                    !s.welcome.smoke_drawn()
                        && text(&s.welcome.smoke_result(), "suggestion").is_empty(),
                    "selection invalidates the actual owned-field preview",
                )?;
                let field = s.welcome.smoke_field();
                let buffer = field.buffer().unwrap();
                buffer.place_cursor(&buffer.end_iter());
                field.emit_by_name::<()>("preedit-changed", &[&"synthetic-preedit"]);
                self.next();
            }
            28 => {
                if !self.wait(
                    s.welcome.smoke_composing() && !s.welcome.smoke_drawn(),
                    5,
                    "native composition to invalidate the owned preview",
                )? {
                    return Ok(());
                }
                self.check(s.welcome.smoke_composing()&&!s.welcome.smoke_drawn(),"native GTK preedit-changed composition signal cancels and refuses preview acceptance")?;
                let field = s.welcome.smoke_field();
                field.emit_by_name::<()>("preedit-changed", &[&""]);
                s.settings.show_pane("General");
                self.next();
            }
            29 => {
                if !self.wait(
                    s.welcome.preview_stopped() && s.controller.status()["preview"] == false,
                    15,
                    "preview process retirement after focus loss",
                )? {
                    return Ok(());
                }
                self.check(!s.welcome.smoke_drawn(),"focus loss retires the isolated preview before restoring normal controller ownership")?;
                s.sender.send(UiAction::Welcome)?;
                self.next();
            }
            30 => {
                if !self.wait(
                    self.welcome_page_ready(s, 4)? && s.welcome.smoke_field().has_focus(),
                    10,
                    "reopened native Try it page and owned field focus",
                )? {
                    return Ok(());
                }
                s.welcome.smoke_next();
                self.next();
            }
            31 => {
                if !self.wait(
                    self.welcome_page_ready(s, 5)? && s.welcome.preview_stopped(),
                    15,
                    "final Welcome step and preview shutdown",
                )? {
                    return Ok(());
                }
                self.check(
                    s.welcome.step() == 5,
                    "Welcome completes all six native steps",
                )?;
                self.capture(&s.welcome.window, "welcome-6.png")?;
                s.welcome.smoke_next();
                self.next();
            }
            32 => {
                if !self.wait(
                    !s.welcome.window.is_visible()
                        && !s.welcome.window.is_mapped()
                        && s.settings.window.is_visible()
                        && s.settings.window.is_mapped(),
                    5,
                    "Welcome Finish to unmap Welcome and restore mapped Settings",
                )? {
                    return Ok(());
                }
                self.check(
                    !s.welcome.window.is_visible() && s.settings.window.is_visible() && !s.quitting,
                    "Welcome Finish without a tray returns to visible Settings, not a hidden loop",
                )?;
                self.answer(&s.parent(), "Quit BroTypist?", gtk::ResponseType::Cancel);
                s.settings.window.close();
                self.next();
            }
            33 => {
                self.check(
                    s.settings.window.is_visible() && !s.quitting,
                    "no-tray window-close Cancel retains visible recovery controls",
                )?;
                self.check(
                    self.answered_prompts.get() == 7,
                    "all seven consent, dirty-draft and no-tray prompts were answered through their actual native buttons",
                )?;
                self.selection_before = Some(s.controller.config());
                s.settings
                    .smoke_control("custom_instructions", json!("  ação e\u{301} 👩‍💻\n"))?;
                s.smoke_receipt.arm()?;
                s.action(UiAction::SelectModel {
                    path: self
                        .output
                        .join("nonexistent-selection.gguf")
                        .to_string_lossy()
                        .into_owned(),
                    prompt_style: "continuation".into(),
                    model_id: None,
                })?;
                self.next();
            }
            34 => {
                if !self.terminal(
                    s,
                    TerminalReceipt::Failed,
                    60,
                    "invalid model selection consumes its exact Failed receipt",
                )? {
                    return Ok(());
                }
                self.selection_retained(
                    s,
                    "failed model selection keeps current and persisted configuration and exact dirty Unicode instructions",
                )?;
                let config = s.controller.config();
                s.smoke_receipt.arm()?;
                s.action(UiAction::SelectModel {
                    path: config.model_path,
                    prompt_style: config.prompt_style,
                    model_id: (!config.model_id.is_empty()).then_some(config.model_id),
                })?;
                self.next();
            }
            35 => {
                if !self.terminal(
                    s,
                    TerminalReceipt::Saved,
                    90,
                    "model selection consumes its exact Saved receipt",
                )? {
                    return Ok(());
                }
                self.selection_retained(
                    s,
                    "successful same-model selection preserves current and persisted identity and exact dirty instructions",
                )?;
                let config = s.controller.config();
                s.smoke_receipt.arm()?;
                s.action(UiAction::SelectModel {
                    path: config.model_path,
                    prompt_style: config.prompt_style,
                    model_id: (!config.model_id.is_empty()).then_some(config.model_id),
                })?;
                s.action(UiAction::CancelModelSelection)?;
                self.next();
            }
            36 => {
                if !self.terminal(
                    s,
                    TerminalReceipt::Cancelled,
                    30,
                    "cancel model selection consumes its exact Cancelled receipt",
                )? {
                    return Ok(());
                }
                self.selection_retained(
                    s,
                    "cancelled selection retains current and persisted configuration and exact dirty Unicode instructions",
                )?;
                self.answer(
                    &s.settings.window,
                    "Allow local personalization?",
                    gtk::ResponseType::Cancel,
                );
                s.settings.smoke_control("use_history", json!(true))?;
                self.check(
                    s.settings.draft.borrow().value("use_history") == Some(&json!(false)),
                    "cancelled use consent does not enable personalization or recording",
                )?;
                s.settings
                    .smoke_control("writing_language", json!("pt-BR"))?;
                s.settings
                    .smoke_control("spelling_language", json!("en-GB"))?;
                s.settings
                    .smoke_control("history_retention_days", json!(7))?;
                s.smoke_receipt.arm()?;
                s.settings.smoke_save();
                self.next();
            }
            37 => {
                if !self.terminal(
                    s,
                    TerminalReceipt::Saved,
                    30,
                    "language and retention Save consumes its exact Saved receipt",
                )? {
                    return Ok(());
                }
                let saved = crate::config::load_config()?;
                self.check(
                    !s.settings.draft.borrow().is_dirty(),
                    "language and retention Saved receipt acknowledges the submitted draft",
                )?;
                self.check(saved.writing_language == "pt-BR" && saved.spelling_language == "en-GB" && saved.history_retention_days == 7 && !saved.use_history && !saved.record_history && saved.custom_instructions == "  ação e\u{301} 👩‍💻\n", "new language, regional spelling, independent use and retention controls persist exact draft through receipt")?;
                ensure!(
                    !crate::paths::config_dir()?
                        .join("diagnostics/requests.json")
                        .exists(),
                    "Disposable diagnostic export must not reuse an earlier file."
                );
                self.diagnostics_before = Some(serde_json::to_value(s.controller.diagnostics())?);
                s.action(UiAction::ExportDiagnostics)?;
                self.next();
            }
            38 => {
                let exported: Value = serde_json::from_slice(&fs::read(
                    crate::paths::config_dir()?.join("diagnostics/requests.json"),
                )?)?;
                let expected = self.diagnostics_before.take();
                self.check(
                    expected.as_ref() == Some(&exported),
                    "explicit native action writes the actual typed content-free diagnostic snapshot",
                )?;
                self.check(self.answered_prompts.get() == 8, "independent personalization-use consent adds one real native cancellation without weakening prior prompt coverage")?;
                self.persistence_before = Some(s.controller.config());
                self.check(
                    self.persistence_before.as_ref() == Some(&crate::config::load_config()?),
                    "persistence failure starts from identical current and independently loaded saved configuration",
                )?;
                for (field, value) in persistence_controls() {
                    s.settings.smoke_control(field, value)?;
                }
                s.settings.show_pane("Languages");
                self.next();
            }
            39 => {
                if !self.wait(
                    s.settings.smoke_pane_ready("Languages")
                        && s.settings.smoke_unsaved_controls_ready(),
                    5,
                    "mapped editable persistence-failure draft",
                )? {
                    return Ok(());
                }
                self.persistence_controls_retained(s, true)?;
                self.persistence_obstruction =
                    Some(SettingsObstruction::install(&self.persistence_destination)?);
                s.smoke_receipt.arm()?;
                s.settings.smoke_save();
                self.next();
            }
            40 => {
                if !self.terminal(
                    s,
                    TerminalReceipt::PersistenceFailed,
                    30,
                    "native Save consumes its own exact Failed(Persistence) receipt",
                )? {
                    return Ok(());
                }
                self.restore_persistence_destination()?;
                let baseline = self
                    .persistence_before
                    .as_ref()
                    .context("Missing persistence-failure baseline")?;
                self.check(
                    &s.controller.config() == baseline
                        && &crate::config::load_config()? == baseline,
                    "failed native Save changes neither controller nor independently reloaded saved configuration",
                )?;
                self.persistence_controls_retained(s, true)?;
                // One explicit recovery click, after restoration and all failure
                // assertions. Never retry Save from the failure handler.
                s.smoke_receipt.arm()?;
                s.settings.smoke_save();
                self.next();
            }
            41 => {
                if !self.terminal(
                    s,
                    TerminalReceipt::Saved,
                    30,
                    "explicit recovery Save consumes its own exact Saved receipt",
                )? {
                    return Ok(());
                }
                self.check(
                    self.persistence_receipts.len() == 2
                        && self.persistence_receipts[0]["operation"]
                            != self.persistence_receipts[1]["operation"],
                    "failed Save and explicit recovery consume distinct submitted operation IDs",
                )?;
                let mut expected = self
                    .persistence_before
                    .take()
                    .context("Missing persistence recovery baseline")?;
                expected.custom_instructions = PERSISTENCE_INSTRUCTIONS.into();
                expected.debounce_ms = 900;
                expected.writing_language = "en-GB".into();
                expected.history_retention_days = 14;
                expected.complete_inside_text = true;
                self.check(
                    s.controller.config() == expected
                        && crate::config::load_config()? == expected,
                    "recovery Save persists exactly the retained draft and preserves every unrelated setting",
                )?;
                self.persistence_controls_retained(s, false)?;
                self.catalog_before = Some(s.controller.config());
                s.sender.send(UiAction::Models)?;
                self.next();
            }
            42 => {
                if !self.wait(
                    s.models.window.is_mapped() && s.models.smoke_catalog_ready(),
                    5,
                    "Models dialog mapping and initial native catalogue population",
                )? {
                    return Ok(());
                }
                s.models.smoke_select_catalog_memory(false)?;
                self.next();
            }
            43 => {
                self.capture_catalog_memory(s, false)?;
                s.models.smoke_select_catalog_memory(true)?;
                self.next();
            }
            44 => {
                self.capture_catalog_memory(s, true)?;
                let baseline = self
                    .catalog_before
                    .as_ref()
                    .context("Missing catalogue-browsing baseline")?;
                self.check(
                    &s.controller.config() == baseline
                        && &crate::config::load_config()? == baseline
                        && s.operations.is_empty()
                        && !s.settings.draft.borrow().is_dirty(),
                    "catalogue browsing changes neither runtime model, saved configuration nor settings draft",
                )?;
                s.models.window.hide();
                self.completed = true;
                click_button(&s.settings.window, "action_quit")?;
                self.cleanup_at.set(Some(Instant::now()));
                self.next();
            }
            _ => {}
        }
        Ok(())
    }
}
fn field_text(field: &gtk::TextView) -> Result<String> {
    let b = field
        .buffer()
        .context("Owned preview buffer is unavailable")?;
    Ok(b.text(&b.start_iter(), &b.end_iter(), false)
        .unwrap_or_default()
        .into())
}
fn click_button(root: &impl IsA<gtk::Widget>, id: &str) -> Result<()> {
    fn find(widget: &gtk::Widget, id: &str) -> Option<gtk::Button> {
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            if button.widget_name() == id && button.is_visible() && button.is_sensitive() {
                return Some(button.clone());
            }
        }
        widget
            .downcast_ref::<gtk::Container>()
            .and_then(|c| c.children().iter().find_map(|child| find(child, id)))
    }
    find(root.as_ref(), id)
        .with_context(|| format!("Missing actual native action button: {id}"))?
        .clicked();
    Ok(())
}
/// Dispatch only into the owned GTK widget. This does not inject global X11 input and is not physical-key evidence.
fn key(field: &gtk::TextView, full: bool) -> Result<bool> {
    ensure!(
        field.has_focus(),
        "Preview acceptance must retain owned-field focus."
    );
    let mut event = gdk::Event::new(gdk::EventType::KeyPress);
    let key = event
        .downcast_mut::<gdk::EventKey>()
        .context("Cannot construct an owned GTK key event")?;
    let raw = key.as_mut();
    raw.keyval = (if full {
        gdk::keys::constants::grave
    } else {
        gdk::keys::constants::Tab
    })
    .into_glib();
    raw.hardware_keycode = if full { 49 } else { 23 };
    raw.state = 0;
    raw.send_event = 1;
    if let Some(window) = gtk::prelude::TextViewExt::window(field, gtk::TextWindowType::Text) {
        use glib::translate::ToGlibPtr;
        raw.window = window.to_glib_full();
    }
    Ok(field.event(&event))
}
fn gate() -> Result<PathBuf> {
    let opted = std::env::var("BROTYPIST_OWNED_DESKTOP_SMOKE").as_deref() == Ok("1")
        || matches!(std::env::var("CI").as_deref(), Ok("true" | "1"));
    ensure!(
        opted,
        "Native UI smoke requires CI or BROTYPIST_OWNED_DESKTOP_SMOKE=1."
    );
    let profile = PathBuf::from(
        std::env::var_os("INKLOW_HOME")
            .context("Native UI smoke requires an explicit empty private INKLOW_HOME.")?,
    );
    ensure!(profile.is_absolute(), "The smoke profile must be absolute.");
    let metadata = fs::symlink_metadata(&profile)?;
    ensure!(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.permissions().mode() & 0o077 == 0,
        "Native smoke requires an owned private profile directory."
    );
    for entry in fs::read_dir(&profile)? {
        ensure!(
            entry?.file_name() == "instance.lock",
            "The smoke profile must be empty except for the common single-owner instance lock."
        );
    }
    ensure!(
        !crate::platform::wayland::is_prepared(),
        "This X11 GTK fixture does not claim a private KWin compositor smoke."
    );
    Ok(profile)
}

fn native_provider_evidence(
    address: *mut libc::c_void,
    executable_base: *mut libc::c_void,
) -> Value {
    let mut provider = std::mem::MaybeUninit::<libc::Dl_info>::uninit();
    if address.is_null() || unsafe { libc::dladdr(address, provider.as_mut_ptr()) } == 0 {
        return json!({"passed":false,"provider":null,"error":"No loader provider for symbol address"});
    }
    // dladdr initialized Dl_info; its filename remains owned by the loaded object.
    let provider = unsafe { provider.assume_init() };
    let name = if provider.dli_fname.is_null() {
        None
    } else {
        Some(
            unsafe { CStr::from_ptr(provider.dli_fname) }
                .to_string_lossy()
                .into_owned(),
        )
    };
    let interposed = provider.dli_fbase == executable_base;
    json!({
        "passed": !interposed && !provider.dli_fbase.is_null() && name.is_some(),
        "provider": name,
        "provider_base": format!("{:p}", provider.dli_fbase),
        "executable_interposition": interposed,
    })
}

fn dbus_linkage_evidence() -> Value {
    let mut evidence = json!({
        "passed": false,
        "pid": std::process::id(),
        "observed_unix_ns": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_nanos().to_string()),
        "executable": std::env::current_exe().ok(),
        "lookup": "dlsym(RTLD_DEFAULT) and dladdr; no D-Bus calls",
        "providers": [],
    });
    let mut executable = std::mem::MaybeUninit::<libc::Dl_info>::uninit();
    // A local function identifies the executable by its loader base, including
    // PIE builds and invocation through symlinks, without assuming any pathname.
    if unsafe {
        libc::dladdr(
            dbus_linkage_evidence as *const () as *const libc::c_void,
            executable.as_mut_ptr(),
        )
    } == 0
    {
        evidence["error"] = json!("Unable to identify the smoke executable's loader base");
        return evidence;
    }
    let executable = unsafe { executable.assume_init() };
    if executable.dli_fbase.is_null() {
        evidence["error"] = json!("The smoke executable has no loader base");
        return evidence;
    }
    evidence["executable_base"] = json!(format!("{:p}", executable.dli_fbase));
    let mut providers = Vec::new();
    for symbol in [
        c"dbus_get_version",
        c"dbus_connection_open_private",
        c"dbus_threads_init_default",
    ] {
        // Clear and inspect dlerror around each lookup; never invoke the address.
        let (address, error) = unsafe {
            libc::dlerror();
            let address = libc::dlsym(libc::RTLD_DEFAULT, symbol.as_ptr());
            let error = libc::dlerror();
            let error = if error.is_null() {
                None
            } else {
                Some(CStr::from_ptr(error).to_string_lossy().into_owned())
            };
            (address, error)
        };
        let mut provider = if let Some(error) = error {
            json!({"passed":false,"provider":null,"error":error})
        } else {
            native_provider_evidence(address, executable.dli_fbase)
        };
        provider["symbol"] = json!(symbol.to_string_lossy());
        providers.push(provider);
    }
    evidence["passed"] = json!(providers.iter().all(|provider| provider["passed"] == true));
    evidence["providers"] = json!(providers);
    evidence
}

pub(super) fn run(output: &Path) -> Result<i32> {
    let profile = gate()?;
    let persistence_destination = PrivateSettingsDestination::new(&profile)?;
    ensure!(output.is_absolute(), "Evidence output must be absolute.");
    if !output.exists() {
        fs::create_dir_all(output)?;
    }
    let metadata = fs::symlink_metadata(output)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Evidence destination must be a directory, not a symlink."
    );
    ensure!(
        !output.join("receipt.json").exists(),
        "Preserve previous evidence; choose a fresh output directory."
    );
    let dbus_linkage = dbus_linkage_evidence();
    if dbus_linkage["passed"] != true {
        write_receipt(
            output,
            &json!({
                "platform":"linux","passed":false,"outcome":"failed",
                "error":"Native D-Bus provider inspection failed before GTK construction",
                "dbus_linkage":dbus_linkage,"assertions":[],"screenshots":[],
            }),
        )?;
        bail!("Native D-Bus provider inspection failed; inspect receipt.json.");
    }
    let prerequisite = (|| -> Result<PathBuf> {
        let model = crate::ui::smoke_model_path()?;
        super::models::validate_import(&model)?;
        Ok(model.canonicalize()?)
    })();
    let model = match prerequisite {
        Ok(model) => model,
        Err(error) => {
            write_receipt(
                output,
                &json!({"platform":"linux","passed":false,"outcome":"unexercised","error":error.to_string(),"dbus_linkage":dbus_linkage,"assertions":[],"screenshots":[]}),
            )?;
            return Err(error);
        }
    };
    let mut config = Config::default();
    config.model_path = model
        .to_str()
        .context("Smoke model filename must be UTF-8")?
        .into();
    config.prompt_style =
        std::env::var("BROTYPIST_SMOKE_PROMPT_STYLE").unwrap_or_else(|_| "continuation".into());
    config.setup_completed = true;
    config.check_updates_automatically = false;
    config.global_enabled = false;
    config.debounce_ms = 100;
    config.min_chars = 1;
    config.max_tokens = 16;
    config.completion_length = "short".into();
    config.acceleration = "cpu".into();
    config.validate()?;
    let shell = match create_shell(
        config,
        LaunchOptions {
            no_tray: true,
            pane: Some("General".into()),
            ..Default::default()
        },
    ) {
        Ok(shell) => shell,
        Err(error) => {
            write_receipt(
                output,
                &json!({"platform":"linux","passed":false,"outcome":"failed","error":error.to_string(),"dbus_linkage":dbus_linkage,"assertions":[],"screenshots":[]}),
            )?;
            return Err(error);
        }
    };
    let display = gdk::Display::default().context("GTK display unavailable")?;
    ensure!(
        display.type_().name().contains("X11"),
        "This smoke requires the explicitly owned X11 GTK display."
    );
    let started = Instant::now();
    let cleanup_at = Rc::new(Cell::new(None::<Instant>));
    let prompt_error = Rc::new(RefCell::new(None::<String>));
    let shutdown_started = cleanup_at.clone();
    let failure = prompt_error.clone();
    // Unlike the scenario and resident timers, this callback does not borrow Shell.
    // Dialog::run holds that borrow while dispatching a nested GLib loop.
    let watchdog = glib::timeout_add_local(Duration::from_millis(50), move || {
        if started.elapsed() >= Duration::from_secs(210) && shutdown_started.get().is_none() {
            *failure.borrow_mut() =
                Some("Native UI smoke exceeded its bounded scenario deadline.".into());
            shutdown_started.set(Some(Instant::now()));
        }
        if let Some(at) = shutdown_started.get() {
            cancel_owned_dialogs();
            if at.elapsed() >= Duration::from_secs(20) {
                let mut failure = failure.borrow_mut();
                if failure.is_none() {
                    *failure = Some(
                        "Canonical Quit did not complete its bounded ownership cleanup.".into(),
                    );
                }
                eprintln!("[native-linux-smoke] cleanup deadline expired");
                gtk::main_quit();
            }
        }
        glib::ControlFlow::Continue
    });
    let state = Rc::new(RefCell::new(Scenario {
        step: 0,
        reported_step: None,
        entered: Instant::now(),
        started,
        awaiting_owner_since: None,
        output: output.into(),
        checks: Vec::new(),
        images: Vec::new(),
        choice_result: json!({"outcome":"unexercised","reason":"Scenario not reached"}),
        catalog_details: Vec::new(),
        catalog_before: None,
        before: String::new(),
        completed: false,
        failure: None,
        cleanup_at,
        prompt_error,
        concurrent_apply: None,
        selection_before: None,
        diagnostics_before: None,
        persistence_destination,
        persistence_obstruction: None,
        persistence_before: None,
        persistence_receipts: Vec::new(),
        persistence_restored: false,
        answered_prompts: Rc::new(Cell::new(0)),
        context_consents_staged: false,
    }));
    let scenario = state.clone();
    let weak = Rc::downgrade(&shell);
    let alive = Rc::new(std::cell::Cell::new(true));
    let running = alive.clone();
    let source = glib::timeout_add_local(Duration::from_millis(120), move || {
        let Some(shell) = weak.upgrade() else {
            running.set(false);
            return glib::ControlFlow::Break;
        };
        let Ok(mut s) = shell.try_borrow_mut() else {
            return glib::ControlFlow::Continue;
        };
        let Ok(mut scenario) = scenario.try_borrow_mut() else {
            return glib::ControlFlow::Continue;
        };
        if s.stopped {
            running.set(false);
            return glib::ControlFlow::Break;
        }
        if scenario.failure.is_none() {
            let result = scenario.tick(&mut s);
            if let Err(error) = result {
                scenario.failure = Some(error.to_string());
                if let Err(error) = scenario.restore_persistence_destination() {
                    scenario.failure = Some(format!(
                        "{}; settings restoration failed: {error}",
                        scenario
                            .failure
                            .as_deref()
                            .unwrap_or("Native scenario failed")
                    ));
                }
                if scenario.cleanup_at.get().is_none() {
                    scenario.cleanup_at.set(Some(Instant::now()));
                }
                eprintln!(
                    "[native-linux-smoke] stage={} failed: {error}",
                    scenario.step
                );
                s.settings.smoke_revert();
                let _ = s.sender.send(UiAction::Quit);
            }
        }
        glib::ControlFlow::Continue
    });
    let result = run_shell(shell.clone());
    watchdog.remove();
    if alive.get() {
        source.remove();
    }
    let mut scenario = state.borrow_mut();
    if let Err(error) = scenario.restore_persistence_destination() {
        scenario.failure = Some(format!(
            "{}; settings restoration failed: {error}",
            scenario
                .failure
                .as_deref()
                .unwrap_or("Native scenario ended")
        ));
    }
    if scenario.failure.is_none() {
        let error = scenario.prompt_error.borrow_mut().take();
        scenario.failure = error;
    }
    let s = shell.borrow();
    let clean = s.stopped
        && s.controller.status()["shutdown_complete"] == true
        && s.welcome.preview_stopped()
        && s.clipboard.is_none()
        && s.power.is_none();
    if clean {
        scenario.checks.push("actual canonical Quit completed controller, preview, clipboard and power ownership cleanup and exited GTK".into());
    }
    let passed = result.is_ok() && scenario.completed && scenario.failure.is_none() && clean;
    let receipt = json!({"schema":1,"platform":"linux","ui":"GTK3 native canonical resident shell","passed":passed,"outcome":if passed{"passed"}else{"failed"},"error":scenario.failure,"last_stage":scenario.step,"modal_responses":scenario.answered_prompts.get(),"assertions":scenario.checks,"screenshots":scenario.images,"clean_exit":clean,"elapsed_ms":scenario.started.elapsed().as_millis(),"profile":profile,"model":model.file_name().map(|s|s.to_string_lossy().into_owned()),"native_status":s.controller.status()["native_status"],"input":"GTK owned-widget event dispatch; no global synthetic input","limits":["Not physical keyboard or external host-edit proof","Not a real KWin/Wayland compositor scenario","Does not register login, change permissions, read history/credentials, download models or install updates"]});
    let mut receipt = receipt;
    receipt["dbus_linkage"] = dbus_linkage;
    receipt["choice_subsequence"] = scenario.choice_result.clone();
    receipt["catalog_memory_details"] = json!({
        "source":"real Models dialog and runtime catalogue inventory",
        "entries":scenario.catalog_details,
        "model_selection_saved":false,
        "download_requested":false,
        "license_consent_granted":false,
        "installation_fixture_created":false,
    });
    receipt["persistence_failure"] = json!({
        "scope": "real native Save and sole shell receipt consumer; only the fresh private settings destination obstructed",
        "obstruction": "empty owned directory; original settings inode preserved by rename",
        "restored": scenario.persistence_restored,
        "receipts": scenario.persistence_receipts,
        "outcome": if passed { "passed" } else { "not-completed" },
    });
    write_receipt(output, &receipt)?;
    result?;
    ensure!(
        passed,
        "Native Linux UI smoke failed; inspect receipt.json."
    );
    Ok(0)
}
fn write_receipt(output: &Path, receipt: &Value) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output.join("receipt.json"))?;
    file.write_all(&serde_json::to_vec_pretty(receipt)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loaded_public_dbus_symbols_do_not_interpose_from_the_executable() {
        let evidence = dbus_linkage_evidence();
        assert_eq!(evidence["passed"], true, "{evidence:#}");
    }

    #[test]
    fn loader_provider_check_rejects_an_executable_address() {
        let address = dbus_linkage_evidence as *const () as *mut libc::c_void;
        let mut executable = std::mem::MaybeUninit::<libc::Dl_info>::uninit();
        assert_ne!(unsafe { libc::dladdr(address, executable.as_mut_ptr()) }, 0,);
        let executable = unsafe { executable.assume_init() };
        let evidence = native_provider_evidence(address, executable.dli_fbase);
        assert_eq!(evidence["executable_interposition"], true, "{evidence:#}");
        assert_eq!(evidence["passed"], false, "{evidence:#}");
    }

    #[test]
    fn native_screenshot_requires_visible_rendered_content() {
        let mut pixels = vec![0; 32 * 32 * 3];
        assert!(!image_has_content(&pixels, 32, 32, 96, 3));
        pixels.fill(255);
        assert!(!image_has_content(&pixels, 32, 32, 96, 3));
        pixels[96..192].fill(0);
        assert!(image_has_content(&pixels, 32, 32, 96, 3));
        assert!(!image_has_content(&pixels[..100], 32, 32, 96, 3));
    }

    #[test]
    fn transparent_or_padding_bytes_do_not_count_as_controls() {
        let mut transparent = vec![0; 32 * 32 * 4];
        transparent[..256].fill(255);
        for alpha in transparent.iter_mut().skip(3).step_by(4) {
            *alpha = 0;
        }
        assert!(!image_has_content(&transparent, 32, 32, 128, 4));
        let mut padded = vec![0; 100 * 32];
        for padding in padded.chunks_exact_mut(100) {
            padding[96..].fill(255);
        }
        assert!(!image_has_content(&padded, 32, 32, 100, 3));
    }
}
