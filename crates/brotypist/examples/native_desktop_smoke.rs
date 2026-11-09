//! Runs the canonical native desktop shell against an explicitly disposable profile.
//! Build with `--features native-smoke`; never a shipped application command.
//! `--owned-welcome <new-absolute-output>` separately exercises canonical Welcome
//! controls and PreviewSession with the explicit existing E4B GGUF and the owned
//! authorization/profile/observer fences below. Actual nonprompting Accessibility
//! and Input Monitoring status gates ordinary Next navigation; absent permission
//! or model readiness produces a retained blocked receipt before inference.
//! No step jump, permission grant, synthetic readiness, full Shell or global hook.
//! EN word, EN full and PT full use canonical language actions/directives once each.
//! Current-view draw, independent readback, immediate one-use refusal and native
//! focus/selection/marked-text revocation are retained, including bounded no-draw
//! and error cases. Positive bilingual coverage requires actual acceptance in both
//! languages; no-draw is not proof of a terminal model abstention. Each request is
//! bounded to 120s and cleanup to 10s. Owned view bitmaps are not compositor proof;
//! marked-text setters are not a physical IME test. This does not verify general
//! onboarding, installation, persistence or ordinary-writing model quality.
//! `--owned-model <new-absolute-output>` requires an explicit absolute existing GGUF
//! in BROTYPIST_SMOKE_MODEL plus the same owned profile/observer authorization.
//! It reuses the owned widget/bridge/controller lifecycle with one real WorkerClient;
//! there is no completion server, extra NativeModel, model fallback or download.
//! The predeclared `owned-model-en-pt-original-and-preview-v1` matrix runs all four
//! fixed inputs exactly once in one worker, with no lucky-offer retry or early success:
//! original EN "I reviewed the proposal and I think we should", original PT
//! "Eu revisei a proposta e acho que precisamos", then canonical welcome-preview EN
//! "I was thinking we could" and PT "Eu estava pensando que poderíamos".
//! These are input texts only, not preview language directives or prompt changes.
//! Only the GGUF scenario explicitly selects the ordinary writing prompt; controlled
//! server fixtures keep their existing configuration. The exact isolated Config and
//! request/draw (120s), acceptance (5s), pump (2ms), token and work limits are recorded.
//! Existing diagnostics correlate input notification/capture, preparation, ready/proceed,
//! native work, provider publication and actual native draw. The draw gate pumps
//! normal owned AppKit events/updateWindows until the same trace/inline lane/current
//! view's NativeDraw is at or after its latest ProviderPublication, never using
//! first-draw-ever or bitmap capture. Failure-time diagnostics precede error capture.
//! Callback, trace and client-submission origins remain separate; decode counters bracket
//! native calls, not synchronized GPU work. First/subsequent requests do not imply
//! a warm cache: actual cold/reuse counters are retained. Genuine no-offer requires
//! correlated terminal/work evidence, no publication/draw/edit/accepted claim,
//! unchanged independent widget readback and word/full stale-action refusal (plus
//! actual prior receipt replay when available). Verified abstention is retained and
//! continues the matrix; it never supplies positive acceptance coverage. Case errors,
//! cancellation and missing reports stay visible and fail the final matrix without
//! preventing the remaining fixed cases from being attempted. Success still requires
//! actual offered/current-view-drawn/accepted cases for BOTH English and Portuguese;
//! missing language coverage is an explicit failure, never replaced by another language.
//! The run checks displayed-prefix word/full acceptance, queued repeats, stale receipts,
//! independent NSTextView readback and worker shutdown. Each row has unique artifact
//! names and keeps its counters/diagnostics even when a later row succeeds. Owned view
//! bitmaps are not compositor screenshots; this is not the full64 semantic-quality gate.
//! Once output/profile guards succeed, receipt.json is written on success or error;
//! guard failures never write into an unauthorized output. Private INKLOW_HOME does
//! not confine OS frameworks or system daemons to the private filesystem.
//! On macOS, `--owned-choices <new-absolute-output>` runs controlled completion-server
//! fixtures through the real WorkerClient, Controller, OverlayOwner and two owned
//! NSTextViews. Includes ghost typed-through, unchanged captures, the exact generated
//! trailing-space payload progression (without duplicate same-authority publications),
//! partial/full acceptance, stale repeats and field/selection/suffix/
//! focus/scroll/host-close invalidation with independent widget readback.
//! Stream/choice fixtures preserve sanitizer-valid accents and non-ZWJ emoji through
//! provider parsing and native insertion; the choice parser removes trailing whitespace.
//! Exact ZWJ emoji use the separate local emoji tool and authored-suffix cases.
//! Stale-view coverage retains the actual typed native PresentationResult at first
//! publication (exact key, authority, revision, mode and visible text), then replays
//! that receipt against distinct, already acknowledged current rows: it must preserve
//! current acceptance, not disarm it. Stale revision/field and ghost lifecycle cases
//! replay actual captured typed receipts too, never receipts synthesized from suggestion
//! JSON; rehandling an acknowledged event is not a new native acknowledgment.
//! A retained old NSButton must be inert while the current chooser is live. Native
//! actions are bound to the installed control and publication; replacement revokes
//! its target and undelivered actions. The current exact displayed row must produce
//! one NSTextView edit and readback.
//! Clipped/hidden views and stale field/revision acknowledgments remain refusals.
//! Requires BROTYPIST_OWNED_DESKTOP_SMOKE=1 and an empty, private, absolute
//! INKLOW_HOME outside the normal profile. All owned entries resolve aliases once,
//! then use only canonical paths for profile, output, bundle and child launch.
//! All canonical ancestors must be root/current-UID-owned without extended ACLs;
//! group/world-writable ancestors must be sticky, with root/current-owned children.
//! This deliberately rejects even deny-only ACLs and non-sticky shared ancestors.
//! Root and other processes of the same UID remain trusted, not adversaries.
//! Prefer OS-created private directories under /private/tmp. `--layout-only` remains
//! rendering-only; the full-shell positional entry remains CI-only.
//! Layout-only keeps all twelve panes and adds standard/minimum/resized Advanced
//! Models plus App Settings size-extreme captures, native identities and scroll reachability.
//! Model rows use embedded models::presets metadata with explicit render-fixture
//! availability and installed=false, never model/profile filesystem inspection.
//! Resource/license details, the ordinary token cap and hard-exclusion controls/help
//! are rasterized without clicking license URLs or modal actions. This is not model
//! availability, save, consent, full-shell or runtime acceptance evidence.
//! The macOS full-shell entry uses the same owned acceptance helper with a controlled
//! provider, not a second real-model owner; receipt.json retains partial
//! owned_choice_evidence if that stage fails alongside the shared error/cleanup results.
//! On composited X11, full-shell chooser evidence requires Presentation to request
//! visibility, bounded native mapping and an after-paint frame before private capture.
//! Unsupported displays remain unexercised; hidden/refused or timed-out presentations
//! fail rather than being force-shown.
//! The owned entry refuses other running completion observers before
//! activation/output using process/bundle metadata only. Close them manually or use
//! an isolated disposable desktop. CI does not bypass this check. Native text
//! assistance is disabled; physical input or external focus loss aborts.
//! Recording, history, personalization and statistics remain disabled. No persisted
//! provenance/acceptance-statistics claim is made. Event counters observe events
//! delivered by WorkerClient::poll, not raw wire traffic; the owned lifecycle records
//! their actual result text/final flags and done events without rewriting or replay.
//! Terminal receipts remain separate. Actual draw callbacks are pinned to trace/lane/view identities.
//! Synthetic owned notification-to-barrier time is not physical-key latency.
//! Host destruction closes the real window and detaches its editor; retained
//! handles permit readback, so Objective-C object deallocation is not proved.
//! `--owned-ax <new-absolute-output>` uses a separately spawned private AppKit
//! editor bundle and the actual MacBridge with an immutable PID/incarnation/window
//! nonce fence. Existing AX trust only; no global hooks, keyboard posting, model,
//! permission prompt or application normal-profile access. Direct AX is not Controller/Tab proof.
//! It records unsupported native setters/notifications instead of forcing them.
//! The child protocol is inherited/private, bounded and accepts source fixtures only.
//! Its child environment is cleared, retaining only fixed PATH/LANG/authorization
//! and canonical private INKLOW_HOME, HOME, CFFIXED_USER_HOME, TMPDIR/TMP/TEMP,
//! XDG_CACHE_HOME and XDG_CONFIG_HOME. Working directory and Library preference,
//! cache, application-support and saved-state locations are private. Window
//! restoration is disabled with process-local volatile defaults and non-restorable
//! windows; text assistance is disabled before synthetic text is seeded.
//! These are not filesystem confinement: AppKit/Foundation, LaunchServices,
//! cfprefsd and text-service daemons may use the login account's normal preferences,
//! caches, registration and service state regardless of HOME. The receipt explicitly
//! makes no host-wide no-write claim. When normal-account OS side effects are
//! unacceptable, native execution requires a genuinely disposable macOS login
//! account; a temporary directory or CI flag is not a substitute. Validate the
//! filesystem-only owned_paths_* tests before considering a justified native run.
//! `--owned-emoji <new-absolute-output>` is a separate opt-in component proof:
//! it activates only the authenticated child and exercises real MacBridge emoji
//! replacement in a native selected-text-readonly field. Unlike `--owned-ax`,
//! it permits marked Unicode events targeted only to that child's PID, never
//! global keyboard posting. Independent widget text, UTF-16 selection and change
//! counts are retained; no real WhatsApp, chooser, Controller or physical-key proof.
//! It requires the same private profile, observer refusal and existing permissions;
//! permission denial is a failure, not a prompt or CI bypass.
//! `--owned-tab-row <new-absolute-output>` is a separate no-post source fixture.
//! It requires real direct-AX inline evidence on an ASCII baseline before probing
//! literal Tabs, native paragraph tab stops, and independently read widget caret
//! geometry. Unsupported font coverage/negative-case layout is reported, never
//! replaced with manufactured AX metadata. No model, hooks, typing or user files.
//! It is not WhatsApp or Controller/rapid-Tab acceptance. Partial receipts survive
//! failures; the owned child is closed on every exit.
//! Incomplete native coverage retains all case receipts, reports `passed=false`
//! with `outcome=partial_native_coverage`, and exits 2 after cleanup.
//! Explicit command (fresh private profile, existing AX trust, observers stopped):
//! ```sh
//! profile="$(mktemp -d /private/tmp/brotypist-tab-row.XXXXXX)"
//! BROTYPIST_OWNED_DESKTOP_SMOKE=1 INKLOW_HOME="$profile" \
//!   cargo run -p brotypist --features native-smoke --example native_desktop_smoke \
//!   -- --owned-tab-row "$profile/evidence"
//! ```
//!
//! Authorized manual run (never automatically closes observers or enables CI):
//! ```sh
//! profile="$(mktemp -d /private/tmp/brotypist-owned.XXXXXX)"
//! BROTYPIST_OWNED_DESKTOP_SMOKE=1 INKLOW_HOME="$profile" \
//!   cargo run -p brotypist --features native-smoke --example native_desktop_smoke \
//!   -- --owned-choices "$profile/evidence"
//! ```
//! For real inference, use a fresh profile with the same authorization, add
//! `BROTYPIST_SMOKE_MODEL=/absolute/path/to/already-verified.gguf`, and replace
//! `--owned-choices` with `--owned-model`. This command does not verify the model
//! checksum; the caller supplies the previously verified artifact. Model startup
//! may take longer than controlled fixtures. Never run in an active typing session.
use anyhow::{Context, Result, ensure};
use std::{path::PathBuf, process::ExitCode};

fn run() -> Result<i32> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    // WorkerClient intentionally relaunches its current executable. Preserve the
    // real startup guard and worker protocol instead of recursively opening UI.
    if arguments == [std::ffi::OsString::from("_worker")] {
        return brotypist::cli::run(vec!["_worker".into()]);
    }
    #[cfg(target_os = "macos")]
    if arguments == [std::ffi::OsString::from("--owned-ax-child")] {
        return brotypist::ui::macos::owned_ax_child();
    }
    #[cfg(target_os = "macos")]
    if arguments.len() == 2 && arguments[0] == "--owned-ax" {
        return brotypist::ui::macos::owned_ax(&PathBuf::from(&arguments[1]));
    }
    #[cfg(target_os = "macos")]
    if arguments.len() == 2 && arguments[0] == "--owned-emoji" {
        return brotypist::ui::macos::owned_emoji(&PathBuf::from(&arguments[1]));
    }
    #[cfg(target_os = "macos")]
    if arguments.len() == 2 && arguments[0] == "--owned-tab-row" {
        return brotypist::ui::macos::owned_tab_row(&PathBuf::from(&arguments[1]));
    }
    #[cfg(target_os = "macos")]
    if arguments.len() == 2 && arguments[0] == "--layout-only" {
        return brotypist::ui::macos::capture_layout(&PathBuf::from(&arguments[1]));
    }
    #[cfg(target_os = "macos")]
    if arguments.len() == 2 && arguments[0] == "--owned-choices" {
        return brotypist::ui::macos::owned_choices(&PathBuf::from(&arguments[1]));
    }
    #[cfg(target_os = "macos")]
    if arguments.len() == 2 && arguments[0] == "--owned-model" {
        return brotypist::ui::macos::owned_model(&PathBuf::from(&arguments[1]));
    }
    #[cfg(target_os = "macos")]
    if arguments.len() == 2 && arguments[0] == "--owned-welcome" {
        return brotypist::ui::macos::owned_welcome(&PathBuf::from(&arguments[1]));
    }
    ensure!(
        arguments.len() == 1,
        "Usage: native_desktop_smoke [--layout-only | --owned-choices | --owned-model | --owned-welcome | --owned-ax | --owned-emoji | --owned-tab-row] <absolute-output-directory>"
    );
    let output = PathBuf::from(
        arguments
            .into_iter()
            .next()
            .context("Missing output directory")?,
    );
    brotypist::ui::smoke(&output)
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            eprintln!("BroTypist native desktop smoke: {error:#}");
            ExitCode::FAILURE
        }
    }
}
