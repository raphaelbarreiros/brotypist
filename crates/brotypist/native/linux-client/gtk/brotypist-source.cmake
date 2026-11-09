# SPDX-License-Identifier: MIT
# Included by the reviewed fcitx5-gtk 5.1.7 source patch only.
# Inputs: GTK 3.24, Gio/GObject/Pango from Gtk3/GioUnix2, xkbcommon,
# and Fcitx5::GClient already required by the upstream GTK3 module.
# Configure upstream with -DBROTYPIST_GTK_CLIENT_DIR=<this absolute directory>.
# Set -DBUILD_ONLY_PLUGIN=ON: upstream builds its reviewed GClient source STATIC.
# The patch adds an explicit negotiated-batch getter and definite IC-disconnect
# signal there; do not link against or modify the verified installed GClient SO.
# Private GTK3-only proposal: ENABLE_GTK2_IM_MODULE=OFF,
# ENABLE_GTK3_IM_MODULE=ON, ENABLE_GTK4_IM_MODULE=OFF, ENABLE_GIR=OFF.
# The integration owner supplies relocated *owned* SDK metadata, a private
# CMAKE_INSTALL_PREFIX, and private GTK_IM_MODULE_FILE/GTK_PATH for the trial.
# This fragment neither installs nor activates a desktop input method.
#
# Source inputs/review boundary:
# fcitx5-gtk 5.1.7, commit 3b18d2ab7401d4233daf38bbc5896f1703685a43;
# gtkimcontextsimple.c from GTK 3.24.52 supplies the known-empty ordinary-key
# branch proof. Other GTK versions remain conservative for ambiguous Simple
# fallback frames. No installed private GTK struct or Fcitx ABI is accessed.
# Opted-in key processing takes the existing upstream synchronous path and
# requests its negotiated batch replies. Missing negotiation remains unknown,
# not presumed ordered. Rejection continues through the ORIGINAL GTK event;
# no async rejected-key replay counter, idle completion heuristic, timer-based
# retirement, global GDK event handler replacement or blind barrier reset.
# Server-forwarded events use scoped gtk_main_do_event dispatch with IgnoredMask;
# actual return completes accelerator/widget dispatch. Strong context holds span
# the originating sync frame and nested forward callback. Without opt-in the
# upstream asynchronous/requeued paths remain unchanged.
# Definite watcher/IC disconnect withdraws remote UI activity only; retained
# original user-preedit/commit caches and local xkb/Simple remain authoritative.
# Native focus/window/field/buffer transitions expire local policy, not the
# registration. Metadata exports the actual tokens and focus_generation.
# Live Configure must echo the re-queried scope/generation; stale activation
# withdraws authorization with a fixed Scope error. Inert expires_ns=0 withdrawal
# always succeeds. Ordinary text/caret notifications do not advance generation.
# Action events freeze the real key-handling token tuple/generation/epoch before
# owned UI cleanup, and re-query the same scope before emission. Metadata-only
# re-enable never reads committed text and cannot retarget a later field.
#
# Native commit dispatch sets process-owned pending_edit and freezes the exact
# receipt at the ORIGINAL accepted_ns. Public Capture/Present/new acceptance
# remain blocked until the final asynchronous Event RPC ACK confirms broker
# authentication + bounded Core enqueue. Only private owner readback bypasses
# that barrier. Changed notifications defer/coalesce until actual ACK success.
# The broker enforces dispatch+2s delivery; a successful ACK stays conclusive
# even if the GUI receives its callback later. Timeout/error/no-owner retains
# the frozen receipt and quarantine across focus/owner changes, without edit
# redispatch, transport retry, automatic learning or authored-output exposure.
# Acceptance stages only the exact replacement selection, rechecks unchanged
# target/text/revision/policy/lease and selection, then emits ONE native IM commit.
# No SetText, destructive delete/insert, redispatch or compensating post-edit
# restoration. Exact applied readback requires the same field/document and user
# revision, expected full text, and collapsed caret/anchor at the insertion end.
# GTK's synchronous readback distinguishes unchanged-text rejection from an
# indeterminate/divergent outcome; neither authorizes another edit. Qt separately
# permits deferred native readback, so unchanged immediate Qt text is not rejection.
#
# Native capabilities: GtkEntry/GtkTextView public families, including normal
# subclasses; bounded committed scalar text, native IM commit range replacement,
# native nonmodal candidate buttons, and independently leased inline/chooser
# receipts. Present is display-only and always unarmed. Arm binds the exact
# authenticated owner, offer/capability/policy epoch, original lease, shown_text
# and shown_values to a fresh unchanged witness and current native visibility.
# Core supplies exact word_text/all_text, including configured punctuation/space;
# the client never reconstructs acceptance from ghost display text. Arm and
# policy renewal never extend an offer lease. Original keys flow when unarmed
# or stale. Inline and chooser Clear/expiry/Arm remain independent; Clear(0)
# removes both. Completion and anchored alternatives can coexist.
# Entry preedit is checked against its actual Pango layout; TextView uses its
# public preedit-adjusted cursor rectangles, refusing wrapped/bidi-ambiguous
# inline geometry. Choosers compact eligible fully fitting rows into an exact
# ordered subsequence, at most nine; a non-fitting middle row does not suppress
# later fitting rows. No truncation/elision/rewriting or hidden-row authority.
# Labels 1..n, arrows, Enter and clicks use displayed ordinals 0..n-1, not original
# offer indices. Core validates the entire nonempty exact shown_values sequence
# with match_choice_subsequence and freezes ChoiceMap(displayed -> original);
# duplicate values match successive offered occurrences in order.
# Choice commit is exactly shown_values[ordinal], without added trailing space;
# action=choice carries that displayed ordinal in choice_index. Core alone maps
# it back to the original offer identity. With no live ghost, emoji/synonyms
# word/all choose displayed ordinal 0; alternatives never steal word/all.
# GTK delivery/layout/mapped-row receipts are not owner-paint or compositor-pixel
# proof. Qt's separate chooser contract delays the original D-Bus Present reply
# until an actual owner-loop exposed QBackingStore paint and fresh witness pass.
# Exposure alone is not paint; withheld paint is not a genuinely unexposed window.
# That pending Qt reply must preserve independent lanes and refuse on withdrawal
# or expiry; bounded Qt reentrant-query lifetime/lane native checks now passed.
# Protected/hidden text, embedded TextView objects, noneditable ranges,
# overwrite insertions, custom/Gecko/GTK4 targets and unavailable geometry
# refuse rather than substituting whole-document edits or inferred capability.
# GtkEntry has no built-in undo; existing app handlers remain authoritative.
# GtkTextView's existing user-action contract surrounds one native IM commit.
#
# Observed private acceptance checkpoint: reviewed fcitx5-gtk 5.1.7 with
# source-owned static GClient built on GTK 3.24.41, and fcitx5-qt 5.1.16 built
# with the official exact Qt 6.12.0 SDK, on Ubuntu 24.04 Linux ARM64. Both native
# fixtures built and canonical both-client acceptance passed under Xvfb/Openbox:
# result=passed, canonical_both=true, gtk3-patched-plugin + qt6.12-patched-plugin.
# The observed 19.51-second run duration is not a performance claim. Content-free
# receipt: evidence/rust-migration/target-owned-client-acceptance.json (repo root).
# This does not validate the original CachyOS payload, x86-64 release or installer.
# Retain the reviewed source pins and all upstream license notices.
# GTK 3.24.41 build success does not extend the GTK 3.24.52 Simple branch proof.
# The earlier combined attempt failed GTK Present after registration/capture/
# geometry and did not reach Qt. A later concurrent-lanes run identified
# ime-connected withdrawal while the first chooser awaited final allocation:
# registration/capture had raced the actual entry's asynchronous Fcitx connection.
# This is a fixture startup precondition, not permission to suppress revocation.
# The GTK fixture now creates only its real first entry until that GClient emits
# connected and its initial current-im. Existing native handlers run unchanged.
# Only then is the second entry created; its resulting owner after-paint completes
# one bounded harness ready command before bind/capture/presentation. The existing
# GObject signals are observed only in the fixture; no spare IM context, polling,
# presentation retry, Metadata field, production readiness protocol or timer is
# introduced. Missing connection/IM/focus/layout evidence fails the fixture.
# Readiness is initial only: later IM/readiness/focus transitions still revoke,
# including deferred chooser replies. The GTK diagnostic subset and subsequent
# canonical both-client run passed. GTK exercised compact ordered-subsequence
# mapping with one native edit/report, pre-paint focus/layout/row-destroy
# cancellation, and expiry/supersession/hidden/focus/policy/target-destroy refusal.
# No normal IME activation, model inference or production installation occurred.
# Linux remains alpha and explicitly opt-in. Wayland placement, physical keyboard
# input, full adapter coverage, standalone Linux popup and full Settings acceptance
# remain unestablished; client acceptance must not be substituted for those gates.
#
# Diagnostic --client gtk / --client qt runs cover only the selected mandatory
# client. Receipts identify requested/covered clients and canonical_both=false;
# a subset cannot establish both-client acceptance. Default / --client both
# still requires both private plugins/fixtures and both successful client runs.
# Missing prerequisites or presentation refusals fail rather than skip.
#
# Remaining verification is reserved to the integration owner. The bounded
# exercised cases above do not cover actual normal/dead/Compose/AltGr/hex input
# or partial-commit continuation. Preserve this broader integration checklist:
# lease/Arm/receipt loss, concurrent ghost+alternatives, word/all/choice, compact
# subsequences with skipped middle rows and exact displayed-ordinal/Core mapping,
# selected/reversed/non-BMP selection and replacement + native undo, protected fields, focus/reset,
# peer disconnect/reconnect (including retained user-preedit caches), accelerator
# consumption of Fcitx-rejected keys, explicit forwarded native keys, absent
# batch negotiation, reentrant destruction during dispatch, and expiry.
# Also require delayed broker auth/ACK with concurrent Capture, original native
# timestamp under delayed GUI callback, lost/error ACK quarantine, scope change
# while pending and real later user input. Sending Event first is not proof of
# Core enqueue; only its authenticated final successful ACK releases Capture.
# Refocus/reparent/buffer replacement must leave all owned shortcuts inert until
# Core re-queries Metadata, disarms its base consumer and echoes a fresh scope.
# Cases outside the bounded receipt remain required live checks, not runtime proof.
if (NOT TARGET im-fcitx5-gtk3)
    message(FATAL_ERROR "BroTypist GTK client requires the upstream GTK3 module target")
endif()
if (NOT BUILD_ONLY_PLUGIN)
    message(FATAL_ERROR "The private BroTypist trial requires BUILD_ONLY_PLUGIN=ON for source-owned static GClient")
endif()
target_sources(im-fcitx5-gtk3 PRIVATE "${CMAKE_CURRENT_LIST_DIR}/brotypist-gtk-client.cpp")
target_include_directories(im-fcitx5-gtk3 PRIVATE "${CMAKE_CURRENT_LIST_DIR}")
target_compile_features(im-fcitx5-gtk3 PRIVATE cxx_std_17)
