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
#
# Native capabilities: GtkEntry/GtkTextView public families, including normal
# subclasses; bounded committed scalar text, native IM commit range replacement,
# native nonmodal candidate buttons, and independently leased inline/chooser
# receipts. Present is display-only; Arm authorizes exact Core word/all strings
# and shown choice indices. Original keys flow for unarmed/stale receipts.
# Entry preedit is checked against its actual Pango layout; TextView uses its
# public preedit-adjusted cursor rectangles, refusing wrapped/bidi-ambiguous
# inline geometry. Choosers preserve the fully visible original-index prefix.
# Protected/hidden text, embedded TextView objects, noneditable ranges,
# overwrite insertions, custom/Gecko/GTK4 targets and unavailable geometry
# refuse rather than substituting whole-document edits or inferred capability.
# GtkEntry has no built-in undo; existing app handlers remain authoritative.
# GtkTextView's existing user-action contract surrounds one native IM commit.
#
# Verification reserved to integration owner: private build/patch review,
# actual normal/dead/Compose/AltGr/hex input and partial-commit continuation,
# lease/Arm/receipt loss, concurrent ghost+alternatives, word/all/choice,
# selected/reversed selection + native undo, protected fields, focus/reset,
# peer disconnect/reconnect (including retained user-preedit caches), accelerator
# consumption of Fcitx-rejected keys, explicit forwarded native keys, absent
# batch negotiation, reentrant destruction during dispatch, and expiry.
# Also require delayed broker auth/ACK with concurrent Capture, original native
# timestamp under delayed GUI callback, lost/error ACK quarantine, scope change
# while pending and real later user input. Sending Event first is not proof of
# Core enqueue; only its authenticated final successful ACK releases Capture.
# Refocus/reparent/buffer replacement must leave all owned shortcuts inert until
# Core re-queries Metadata, disarms its base consumer and echoes a fresh scope.
# These are required live checks, not source/runtime proof.
if (NOT TARGET im-fcitx5-gtk3)
    message(FATAL_ERROR "BroTypist GTK client requires the upstream GTK3 module target")
endif()
if (NOT BUILD_ONLY_PLUGIN)
    message(FATAL_ERROR "The private BroTypist trial requires BUILD_ONLY_PLUGIN=ON for source-owned static GClient")
endif()
target_sources(im-fcitx5-gtk3 PRIVATE "${CMAKE_CURRENT_LIST_DIR}/brotypist-gtk-client.cpp")
target_include_directories(im-fcitx5-gtk3 PRIVATE "${CMAKE_CURRENT_LIST_DIR}")
target_compile_features(im-fcitx5-gtk3 PRIVATE cxx_std_17)
