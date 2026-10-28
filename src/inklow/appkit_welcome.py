"""Native six-step welcome and an isolated, real local-inference try-it field.

The shell drives refreshes on the AppKit thread and owns shared settings and
consent dialogs. This window owns its preview session, stopping it on navigation,
loss of focus or close; preview insertion targets only its own text view.
"""
from __future__ import annotations

import importlib
from pathlib import Path
import time

import AppKit as A
import objc
from Foundation import NSMutableAttributedString, NSAttributedString, NSObject, NSURL

from . import system
from .appkit_overlay import ghost_color, _dark, _rgb
from .resources import APP_NAME, REPOSITORY_URL, asset_path
from .shortcuts import mac_key_name, shortcut_from_event


class WelcomeView(A.NSView):
    """Lay out welcome content using top-left AppKit coordinates."""

    def isFlipped(self):
        return True


def label(text, x, y, width, height, *, size=13, bold=False, secondary=False):
    """Create a wrapping native label with geometry and font size in points."""
    view = A.NSTextField.wrappingLabelWithString_(text)
    view.setFrame_(A.NSMakeRect(x, y, width, height))
    view.setFont_(A.NSFont.systemFontOfSize_weight_(size, A.NSFontWeightSemibold if bold else A.NSFontWeightRegular))
    view.setTextColor_(A.NSColor.secondaryLabelColor() if secondary else A.NSColor.labelColor())
    return view


class GhostWordmark(A.NSTextField):
    """Keep the branded text colors aligned with the view's effective appearance."""

    def viewDidChangeEffectiveAppearance(self):
        objc.super(GhostWordmark, self).viewDidChangeEffectiveAppearance()
        if hasattr(self, "mark_size"):
            self.update_wordmark()

    @objc.python_method
    def update_wordmark(self):
        size = self.mark_size
        text = NSMutableAttributedString.alloc().initWithString_(APP_NAME)
        text.addAttribute_value_range_(A.NSFontAttributeName, A.NSFont.systemFontOfSize_weight_(size, A.NSFontWeightSemibold), (0, len(APP_NAME)))
        text.addAttribute_value_range_(A.NSFontAttributeName, A.NSFont.systemFontOfSize_weight_(size, A.NSFontWeightBold), (0, 3))
        text.addAttribute_value_range_(A.NSForegroundColorAttributeName, _rgb("F2F2F3" if _dark(self) else "15171C"), (0, 3))
        ghost = ghost_color(self) if A.NSWorkspace.sharedWorkspace().accessibilityDisplayShouldIncreaseContrast() else _rgb("6E717A" if _dark(self) else "A7A9AF")
        text.addAttribute_value_range_(A.NSForegroundColorAttributeName, ghost, (3, len(APP_NAME) - 3))
        text.addAttribute_value_range_(A.NSKernAttributeName, -size * 0.025, (0, len(APP_NAME)))
        paragraph = A.NSMutableParagraphStyle.alloc().init()
        paragraph.setAlignment_(A.NSTextAlignmentCenter)
        text.addAttribute_value_range_(A.NSParagraphStyleAttributeName, paragraph, (0, len(APP_NAME)))
        self.setAttributedStringValue_(text)


def wordmark(width, height, size):
    """System-font lockup: the predicted half is ghost gray, never mint."""
    view = GhostWordmark.labelWithString_("")
    view.mark_size = size
    view.update_wordmark()
    view.setFrame_(A.NSMakeRect(0, 0, width, height))
    view.setAlignment_(A.NSTextAlignmentCenter)
    view.setAccessibilityLabel_(APP_NAME)
    return view


class PreviewTextView(A.NSTextView):
    """Draw a non-document suffix and route acceptance to the local preview owner."""

    @objc.python_method
    def set_ghost(self, text):
        """Update the drawn suggestion and accessibility help without editing text."""
        if getattr(self, "ghost_text", "") != text:
            self.ghost_text = text
            self.setNeedsDisplay_(True)
        self.setAccessibilityHelp_(f"Suggestion: {text}" if text else "Type to request a local suggestion.")

    def drawRect_(self, dirty):
        """Draw near the local caret, skipping selections and active IME composition."""
        objc.super(PreviewTextView, self).drawRect_(dirty)
        text = getattr(self, "ghost_text", "")
        selection = self.selectedRange()
        if not text or selection.length or self.hasMarkedText() or not self.window():
            return
        caret, actual = self.firstRectForCharacterRange_actualRange_(selection, None)
        local = self.convertRect_fromView_(self.window().convertRectFromScreen_(caret), None)
        x = A.NSMinX(local) + 1
        y = A.NSMinY(local)
        # Text after the caret remains visible: draw the preview below it.
        # Compare Cocoa's selection offset against UTF-16 length, not Python len.
        if int(selection.location) < len(str(self.string()).encode("utf-16-le")) // 2:
            y += A.NSHeight(local) + 4
        bounds = A.NSInsetRect(self.visibleRect(), 12, 0)
        width = A.NSMaxX(bounds) - x
        if width <= 0:
            return
        ghost = NSAttributedString.alloc().initWithString_attributes_(text, {A.NSFontAttributeName: self.font(), A.NSForegroundColorAttributeName: ghost_color(self)})
        A.NSGraphicsContext.saveGraphicsState()
        A.NSBezierPath.bezierPathWithRect_(bounds).addClip()
        ghost.drawInRect_(A.NSMakeRect(x, y, width, max(0, A.NSMaxY(bounds) - y)))
        A.NSGraphicsContext.restoreGraphicsState()

    def keyDown_(self, event):
        """Consume preview actions only when handled; forward other keys to AppKit."""
        owner = self.welcome_owner
        flags = event.modifierFlags()
        modifiers = [name for name, mask in (("Ctrl", A.NSEventModifierFlagControl), ("Alt", A.NSEventModifierFlagOption), ("Shift", A.NSEventModifierFlagShift), ("Super", A.NSEventModifierFlagCommand)) if flags & mask]
        try:
            chord = shortcut_from_event(mac_key_name(int(event.keyCode())) or str(event.charactersIgnoringModifiers() or ""), modifiers, platform="macos", keycode=int(event.keyCode()))
        except ValueError:
            chord = ""
        if chord == "Escape" and owner.session:
            owner.session.dismiss()
            owner.refresh_preview()
            return
        for action in ("word", "all"):
            if chord and chord == owner.shell.controller.config.shortcuts.get(action) and owner.accept(action):
                return
        objc.super(PreviewTextView, self).keyDown_(event)


class WelcomeWindow(NSObject):
    """Retain onboarding controls and a preview session limited to the focused editor."""

    @objc.python_method
    def configure(self, shell):
        """Create the reusable window without starting a preview or downloading a model."""
        self.shell = shell
        self.step = 0
        self.session = None
        self.preview_text = ""
        self.preview_caret = 0
        self.actions = {}
        self.next_tag = 0
        self.permission_buttons = {}
        self.context_buttons = {}
        self.last_inline_check = 0.0
        style = A.NSWindowStyleMaskTitled | A.NSWindowStyleMaskClosable | A.NSWindowStyleMaskFullSizeContentView
        self.window = A.NSWindow.alloc().initWithContentRect_styleMask_backing_defer_(A.NSMakeRect(0, 0, 760, 580), style, A.NSBackingStoreBuffered, False)
        self.window.setTitle_(f"Welcome to {APP_NAME}")
        self.window.setIdentifier_("inklow.welcome")
        self.window.setAccessibilityIdentifier_("inklow.welcome")
        self.window.setTitleVisibility_(A.NSWindowTitleHidden)
        self.window.setTitlebarAppearsTransparent_(True)
        self.window.setReleasedWhenClosed_(False)
        self.window.setDelegate_(self)
        self.window.center()
        return self

    @objc.python_method
    def button(self, title, x, y, width, callback, identifier):
        """Retain a step-local action and expose its stable accessibility identifier."""
        button = A.NSButton.buttonWithTitle_target_action_(title, self, "perform:")
        button.setFrame_(A.NSMakeRect(x, y, width, 30))
        self.next_tag += 1
        button.setTag_(self.next_tag)
        button.setIdentifier_(identifier)
        button.setAccessibilityIdentifier_(identifier)
        button.setAccessibilityLabel_(title)
        self.actions[self.next_tag] = callback
        self.root.addSubview_(button)
        return button

    def perform_(self, sender):
        try:
            callback = self.actions.get(int(sender.tag()))
            if callback:
                callback()
        except Exception as exc:
            self.shell._error(str(exc))

    @objc.python_method
    def show(self):
        """Restart the tour at its first step, retaining the previously typed preview."""
        self.stop_preview()
        self.step = 0
        self.render()
        self.window.makeKeyAndOrderFront_(None)
        A.NSApplication.sharedApplication().activateIgnoringOtherApps_(True)

    @objc.python_method
    def render(self):
        """Stop the old preview before replacing step controls and action bindings."""
        self.stop_preview()
        self.actions = {}
        self.permission_buttons = {}
        self.context_buttons = {}
        self.root = WelcomeView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 760, 580))
        self.window.setContentView_(self.root)
        self.root.addSubview_(label(f"Step {self.step + 1} of 6", 440, 18, 108, 18, size=11, secondary=True))
        for index in range(6):
            segment = A.NSBox.alloc().initWithFrame_(A.NSMakeRect(552 + index * 32, 25, 28, 4))
            segment.setBoxType_(A.NSBoxCustom)
            segment.setBorderType_(A.NSNoBorder)
            segment.setCornerRadius_(2)
            segment.setFillColor_(A.NSColor.labelColor() if index <= self.step else A.NSColor.separatorColor())
            self.root.addSubview_(segment)
        divider = A.NSBox.alloc().initWithFrame_(A.NSMakeRect(0, 516, 760, 1))
        divider.setBoxType_(A.NSBoxSeparator)
        self.root.addSubview_(divider)
        self.back = self.button("Back", 32, 534, 84, lambda: self.navigate(-1), "inklow.welcome.back")
        self.back.setHidden_(self.step == 0)
        titles = ("Get started", "Continue", "Continue", "Continue", "Finish", "Start typing")
        self.primary = self.button(titles[self.step], 580, 534, 148, self.advance, "inklow.welcome.next")
        self.primary.setKeyEquivalent_("\r")
        hints = ("Set up at your own pace", "Optional context is off by default", "Allow both permissions to continue", "Choose a ready local model to continue", "Use your configured acceptance keys", "Find BroTypist in the menu bar")
        self.hint = label(hints[self.step], 138, 541, 424, 22, size=12, secondary=True)
        self.hint.setAlignment_(A.NSTextAlignmentRight)
        self.root.addSubview_(self.hint)
        getattr(self, f"step_{self.step}")()
        self.tick()

    @objc.python_method
    def heading(self, title, subtitle):
        self.root.addSubview_(label(title, 56, 76, 648, 42, size=26, bold=True))
        self.root.addSubview_(label(subtitle, 56, 125, 648, 60, size=13, secondary=True))

    @objc.python_method
    def card(self, title, body, y, *, height=78):
        box = A.NSBox.alloc().initWithFrame_(A.NSMakeRect(56, y, 648, height))
        box.setBoxType_(A.NSBoxCustom)
        box.setBorderType_(A.NSNoBorder)
        box.setFillColor_(A.NSColor.controlBackgroundColor())
        box.setCornerRadius_(10)
        self.root.addSubview_(box)
        self.root.addSubview_(label(title, 72, y + 12, 610, 22, bold=True))
        self.root.addSubview_(label(body, 72, y + 36, 610, height - 40, size=12, secondary=True))

    @objc.python_method
    def step_0(self):
        """Present the local-first introduction and user-initiated source link."""
        icon = A.NSImageView.alloc().initWithFrame_(A.NSMakeRect(324, 65, 112, 112))
        icon.setImage_(A.NSImage.alloc().initWithContentsOfFile_(str(asset_path("app.icns"))))
        self.root.addSubview_(icon)
        self.root.addSubview_(label("Welcome to", 56, 183, 648, 30, size=26, bold=True))
        self.root.subviews()[-1].setAlignment_(A.NSTextAlignmentCenter)
        mark = wordmark(648, 40, 32)
        mark.setFrameOrigin_(A.NSMakePoint(56, 218))
        self.root.addSubview_(mark)
        tagline = label("A little help with the words that come next.", 56, 266, 648, 25, secondary=True)
        tagline.setAlignment_(A.NSTextAlignmentCenter)
        self.root.addSubview_(tagline)
        self.card("Runs locally", "Use a model on this Mac or a configured local server. No account required.", 304, height=62)
        self.card("Stays out of the way", "Faint inline suggestions in supported fields. Accept what you want, or keep typing to ignore them.", 374, height=62)
        self.card("Source on GitHub", "Public source code. No account needed to browse.", 444, height=62)
        self.button("View source", 558, 454, 130, lambda: A.NSWorkspace.sharedWorkspace().openURL_(NSURL.URLWithString_(REPOSITORY_URL)), "inklow.welcome.source")

    @objc.python_method
    def step_1(self):
        """Present optional context choices through the shell's shared consent path."""
        config = self.shell.controller.config
        self.heading("Your writing stays local", "Predictions run on this Mac. BroTypist does not send your writing to a remote inference service, and writing history stays off unless you enable it in Personalization.")
        self.card("Text before cursor  →  Local model  →  Suggestion", f"Up to {config.context_chars:,} preceding characters from a supported, focused field. Password fields and excluded apps are skipped before capture.", 192, height=72)
        self.root.addSubview_(label("Optional context — each choice asks for confirmation and can be changed later in Context.", 56, 278, 648, 20, size=12, bold=True))
        for index, (field, title) in enumerate((
            ("screen_context", "Use focused-window text (Accessibility)"),
            ("clipboard_context", "Use the clipboard"),
            ("screenshot_context", "Use screenshots for context (Screen Recording)"),
            ("screenshot_appearance", "Use screenshots to improve suggestion appearance (Screen Recording)"),
        )):
            button = self.button(title, 64, 306 + index * 34, 620, lambda f=field: self.set_context(f, self.context_buttons[f]), f"inklow.welcome.{field}")
            button.setButtonType_(A.NSButtonTypeSwitch)
            button.setState_(A.NSControlStateValueOn if getattr(config, field) else A.NSControlStateValueOff)
            self.context_buttons[field] = button
        self.root.addSubview_(label("Screenshots are processed in memory on this Mac and discarded immediately; they are never saved or uploaded. Accessibility text is always tried first.", 72, 446, 616, 46, size=11.5, secondary=True))

    @objc.python_method
    def set_context(self, field, button):
        """Request a context change, then reflect the saved choice even on failure."""
        try:
            self.shell._context_consent(field, bool(button.state()))
        finally:
            # The switch always shows the saved choice, including after Cancel or an error.
            button.setState_(A.NSControlStateValueOn if getattr(self.shell.controller.config, field) else A.NSControlStateValueOff)

    @objc.python_method
    def step_2(self):
        """Build live permission cards; only button actions request OS access."""
        self.heading("Two macOS permissions", "Approve BroTypist in System Settings. These cards show the actual permission state, not a saved preference.")
        for key, title, detail, y in (("accessibility", "Accessibility", "Reads supported text near the caret and inserts only after you accept.", 196), ("input_monitoring", "Input Monitoring", "Listens for typing and configured acceptance shortcuts. Secure fields remain protected.", 292)):
            self.card(title, detail, y, height=86)
            self.permission_buttons[key] = self.button("Open System Settings…", 488, y + 9, 200, lambda k=key: system.open_permissions(k), f"inklow.welcome.{key}")
        self.card("Screen & System Audio Recording (optional)", "Needed only if you turned on a screenshot option. Not required for typing.", 388, height=72)
        self.permission_buttons["screen_recording"] = self.button("Request Access…", 488, 397, 200, self.request_screen_recording, "inklow.welcome.screen_recording")
        self.root.addSubview_(label("Source runs may appear as Python or your terminal. macOS may require a restart after granting access.", 72, 470, 616, 40, size=11.5, secondary=True))

    @objc.python_method
    def request_screen_recording(self):
        self.shell._request_screen_recording()

    @objc.python_method
    def step_3(self):
        """Build model selection and download controls without starting inference."""
        from .models import recommended_preset
        self.heading("Choose your local model", "Choose a catalogue model or import a local file. Nothing downloads until you ask. Choosing a model does not run a prediction.")
        self.recommended_model = recommended_preset().id
        self.model_choice = ""
        self.model_config_key = None
        self.model_menu_items = None
        self.local_backend_error = None
        self.root.addSubview_(label("The recommendation is based on this Mac’s memory. You can change models later.", 56, 189, 648, 22, size=12, secondary=True))
        self.model_popup = A.NSPopUpButton.alloc().initWithFrame_pullsDown_(A.NSMakeRect(56, 219, 648, 30), False)
        self.model_popup.setTarget_(self)
        self.model_popup.setAction_("chooseCatalogueModel:")
        self.model_popup.setIdentifier_("inklow.welcome.modelChoice")
        self.model_popup.setAccessibilityIdentifier_("inklow.welcome.modelChoice")
        self.model_popup.setAccessibilityLabel_("Local model")
        self.root.addSubview_(self.model_popup)
        self.model_description = label("", 56, 260, 648, 44, size=12, secondary=True)
        self.root.addSubview_(self.model_description)
        self.model_request = self.button("Download Model", 56, 315, 224, self.request_model, "inklow.welcome.requestModel")
        self.model_import = self.button("Import Local File…", 292, 315, 184, self.import_model, "inklow.welcome.importModel")
        self.button("Advanced Settings…", 488, 315, 216, lambda: self.shell.show_settings("models"), "inklow.welcome.models")
        self.selected_model = label("", 56, 361, 648, 25, size=13, bold=True)
        self.selected_model.cell().setLineBreakMode_(A.NSLineBreakByTruncatingMiddle)
        self.root.addSubview_(self.selected_model)
        self.model_status = label("", 56, 396, 648, 48, size=12, secondary=True)
        self.root.addSubview_(self.model_status)
        self.model_progress = A.NSProgressIndicator.alloc().initWithFrame_(A.NSMakeRect(56, 454, 516, 16))
        self.model_progress.setStyle_(A.NSProgressIndicatorStyleBar)
        self.root.addSubview_(self.model_progress)
        self.model_cancel = self.button("Cancel", 588, 446, 116, self.cancel_model_download, "inklow.welcome.cancelDownload")
        self.root.addSubview_(label("Downloads are verified before use. Imported files stay where you choose them.", 56, 484, 648, 22, size=11.5, secondary=True))
        self.refresh_model()

    @objc.python_method
    def _model_config(self):
        config = self.shell.controller.config
        return config.backend, config.model_path, config.model

    @objc.python_method
    def _current_model_choice(self, inventory):
        """Prefer a pending catalogue choice, then the active file or recommendation."""
        config = self.shell.controller.config
        pending = self.shell.model_selection
        if pending is not None and any(str(item["id"]) == pending for item in inventory):
            return pending
        if config.backend != "gguf":
            return ""
        selected = next((str(item["id"]) for item in inventory
                         if config.model_path and Path(str(item["path"])).expanduser() == Path(config.model_path).expanduser()), None)
        return selected if selected is not None else ("" if config.model_path else self.recommended_model)

    @objc.python_method
    def _populate_models(self, inventory):
        """Rebuild menu items only when labels change, keeping selection by model ID."""
        ordered = sorted(inventory, key=lambda item: (str(item["id"]) != self.recommended_model, not item.get("installed")))
        options = []
        config = self.shell.controller.config
        if config.backend != "gguf" or (config.model_path and not any(
                Path(str(item["path"])).expanduser() == Path(config.model_path).expanduser() for item in inventory)):
            options.append(("", self.shell._model_name() + (" — local file" if config.backend == "gguf" else " — local server")))
        for item in ordered:
            size = float(item.get("size_bytes") or 0) / 1_000_000_000
            state = "Installed" if item.get("installed") else f"{size:.2f} GB download"
            recommended = " — Recommended" if str(item["id"]) == self.recommended_model else ""
            options.append((str(item["id"]), f"{item['label']}{recommended} · {state}"))
        signature = tuple(options)
        if signature != self.model_menu_items:
            self.model_popup.removeAllItems()
            self.model_popup.addItemsWithTitles_([title for _, title in options])
            self.model_menu_items = signature
        index = next((index for index, (model_id, _) in enumerate(options) if model_id == self.model_choice), -1)
        self.model_popup.selectItemAtIndex_(index)

    def chooseCatalogueModel_(self, sender):
        index = int(sender.indexOfSelectedItem())
        if not self.model_menu_items or not 0 <= index < len(self.model_menu_items):
            return
        self.model_choice = self.model_menu_items[index][0]
        try:
            if self.model_choice:
                self.shell._select_model(self.model_choice)
            else:
                self.shell.model_selection = None
        except Exception as exc:
            self.shell._error(str(exc))
        finally:
            self.refresh_model()

    @objc.python_method
    def request_model(self):
        try:
            if self.model_choice:
                self.shell._request_model(self.model_choice)
        finally:
            self.refresh_model()

    @objc.python_method
    def import_model(self):
        try:
            self.shell._choose_model()
        finally:
            self.refresh_model()

    @objc.python_method
    def cancel_model_download(self):
        self.shell.controller.cancel_download()
        self.refresh_model()

    @objc.python_method
    def _model_can_continue(self, model):
        """Check setup prerequisites, not whether a model or local server can infer."""
        config = self.shell.controller.config
        if not self.shell._model_ready():
            return False
        if self.model_choice:
            if not model or not model.get("installed") or config.backend != "gguf":
                return False
            if Path(str(model["path"])).expanduser() != Path(config.model_path).expanduser():
                return False
        if config.backend == "gguf":
            return self.local_backend_error == ""
        return bool(config.model.strip())

    @objc.python_method
    def refresh_model(self):
        """Refresh model readiness and download state without loading model weights."""
        inventory = self.shell.controller.models()
        key = self._model_config()
        if self.model_config_key != key:
            self.model_choice = self._current_model_choice(inventory)
            self.model_config_key = key
        self._populate_models(inventory)
        model = next((item for item in inventory if str(item["id"]) == self.model_choice), None)
        config = self.shell.controller.config
        if config.backend == "gguf" and self.local_backend_error is None:
            try:
                # Check the Python/native dependency, never allocate a model or generate text.
                importlib.import_module("llama_cpp")
                self.local_backend_error = ""
            except Exception as exc:
                self.local_backend_error = "Local inference support could not load. Repair the installation and reopen BroTypist."
                self.model_status.setToolTip_(str(exc))
        download = self.shell.controller.status().get("download") or {}
        active = download.get("state") == "downloading"
        ready = self._model_can_continue(model)
        self.primary.setEnabled_(ready)
        self.selected_model.setStringValue_(f"Current: {self.shell._model_name()}")
        self.selected_model.setToolTip_(self.shell._model_name())
        if model:
            details = str(model.get("description") or "")
            if model.get("availability"):
                details = str(model["availability"])
            elif model.get("requires_license_acceptance") and not model.get("installed"):
                details += f" You’ll be asked to accept {model['license_name']} before downloading."
        else:
            details = "Your imported file is used in place." if config.backend == "gguf" else "Your configured local server is kept. Try it checks inference; setup does not test the connection."
        self.model_description.setStringValue_(details)
        self.model_request.setTitle_("Use Model" if model and model.get("installed") else "Download Model")
        self.model_request.setEnabled_(bool(model) and not active and (bool(model.get("installed")) or not model.get("availability")))
        self.model_import.setEnabled_(not active)
        self.model_cancel.setHidden_(not active)
        self.model_cancel.setEnabled_(active)
        done = float(download.get("received") or 0)
        total = float(download.get("total") or 0)
        self.model_progress.setHidden_(not active)
        self.model_progress.setIndeterminate_(active and total <= 0)
        self.model_progress.setMaxValue_(max(1, total))
        self.model_progress.setDoubleValue_(min(done, max(1, total)))
        if active and total <= 0:
            self.model_progress.startAnimation_(None)
        else:
            self.model_progress.stopAnimation_(None)
        if active:
            downloading = next((str(item["label"]) for item in inventory if str(item["id"]) == str(download.get("id"))), "model")
            progress = f"{done / 1_000_000:.1f} / {total / 1_000_000:.1f} MB" if total > 0 else f"{done / 1_000_000:.1f} MB"
            message = f"Downloading {downloading}… {progress}. Verification follows the download."
        elif download.get("state") in {"error", "cancelled"}:
            message = str(download.get("error") or download.get("message") or "Download stopped. You can try again.")
        elif config.backend == "gguf" and self.local_backend_error:
            message = self.local_backend_error
        elif ready:
            message = "Local model file and inference support are ready. Try it next." if config.backend == "gguf" else "Local server configured. Try it next to check inference."
        elif model and model.get("installed"):
            message = "Download verified. Choose Use Model to make it current." if download.get("state") == "complete" and str(download.get("id")) == self.model_choice else "Choose Use Model to make this installed model current."
        elif model:
            message = str(model.get("availability") or "This model is not installed. Choose Download Model to begin.")
        else:
            message = "The current local model is not ready. Import an existing file or choose a catalogue model."
        self.model_status.setStringValue_(message)

    @objc.python_method
    def step_4(self):
        """Create the local try-it editor; the focused-window tick starts its session."""
        config = self.shell.controller.config
        word = config.shortcuts.get("word") or "Unassigned"
        all_keys = config.shortcuts.get("all") or "Unassigned"
        self.heading("Try it", "Type in this field to ask your actual local model for a suggestion. This preview does not read or insert into another application, and does not record writing history.")
        self.root.addSubview_(label("Try: I was thinking we could", 56, 196, 648, 24, secondary=True))
        scroll = A.NSScrollView.alloc().initWithFrame_(A.NSMakeRect(56, 230, 648, 130))
        scroll.setHasVerticalScroller_(True)
        scroll.setBorderType_(A.NSBezelBorder)
        self.editor = PreviewTextView.alloc().initWithFrame_(A.NSMakeRect(0, 0, 628, 128))
        self.editor.welcome_owner = self
        self.editor.ghost_text = ""
        self.editor.setFont_(A.NSFont.systemFontOfSize_(17))
        self.editor.setRichText_(False)
        self.editor.setAllowsUndo_(True)
        self.editor.setHorizontallyResizable_(False)
        self.editor.setVerticallyResizable_(True)
        self.editor.setTextContainerInset_(A.NSMakeSize(12, 12))
        self.editor.textContainer().setWidthTracksTextView_(True)
        self.editor.setString_(self.preview_text)
        self.editor.setSelectedRange_((min(self.preview_caret, len(self.preview_text.encode("utf-16-le")) // 2), 0))
        self.editor.setDelegate_(self)
        self.editor.setAccessibilityIdentifier_("inklow.welcome.preview")
        self.editor.setAccessibilityLabel_("Try local predictions. Type your own text.")
        scroll.setDocumentView_(self.editor)
        self.root.addSubview_(scroll)
        self.root.addSubview_(label("A faint suffix is drawn in this field; it becomes document text only when you accept. Continue typing to ignore it.", 56, 380, 648, 45, size=12, secondary=True))
        self.root.addSubview_(label(f"{word}: next word  ·  {all_keys}: full suggestion  ·  esc: dismiss", 56, 439, 648, 30, size=12, secondary=True))
        self.preview_status = label("Waiting for you to type…", 56, 476, 648, 26, size=12, secondary=True)
        self.root.addSubview_(self.preview_status)
        self.window.makeFirstResponder_(self.editor)

    @objc.python_method
    def step_5(self):
        """Summarize setup and expose explicit login and keyboard-settings actions."""
        self.heading("You’re all set", "BroTypist lives in your menu bar. Click its caret glyph to pause predictions, control the current app, or open Settings.")
        image = A.NSImageView.alloc().initWithFrame_(A.NSMakeRect(342, 186, 76, 64))
        glyph = A.NSImage.alloc().initWithContentsOfFile_(str(asset_path("tray/active-template-36.png")))
        glyph.setTemplate_(True)
        image.setImage_(glyph)
        image.setImageScaling_(A.NSImageScaleProportionallyUpOrDown)
        self.root.addSubview_(image)
        config = self.shell.controller.config
        self.card("Your typing setup", f"Model: {self.shell._model_name()}\nNext word: {config.shortcuts.get('word') or 'Unassigned'}   ·   Full suggestion: {config.shortcuts.get('all') or 'Unassigned'}", 262, height=86)
        self.card("macOS inline predictive text", "", 360, height=78)
        self.inline_status = label("", 72, 396, 400, 36, size=12, secondary=True)
        self.root.addSubview_(self.inline_status)
        self.button("Keyboard Settings…", 488, 369, 200, lambda: system.open_permissions("keyboard"), "inklow.welcome.keyboard")
        self.last_inline_check = 0.0
        self.login = self.button("Launch BroTypist at login", 56, 456, 270, self.toggle_login, "inklow.welcome.launchAtLogin")
        self.login.setButtonType_(A.NSButtonTypeSwitch)
        self.login.setState_(A.NSControlStateValueOn if system.launch_at_login_state() else A.NSControlStateValueOff)

    @objc.python_method
    def toggle_login(self):
        self.shell._autostart(bool(self.login.state()))

    @objc.python_method
    def navigate(self, delta):
        self.step = max(0, min(5, self.step + delta))
        self.render()

    @objc.python_method
    def advance(self):
        """Advance only when permitted; save completion when the final step is accepted."""
        if self.step == 3:
            self.refresh_model()
        if not self.primary.isEnabled():
            return
        if self.step == 5:
            self.shell._change(setup_completed=True)
            self.close()
            if self.shell.no_tray or self.shell.status_item is None:
                self.shell.show_settings("general")
        else:
            self.navigate(1)

    @objc.python_method
    def stop_preview(self):
        """Close the session before clearing its reference and retaining editor state.

        Close failures propagate so shutdown cannot treat an owned preview as
        released before its session reports successful cleanup.
        """
        if self.session:
            self.session.close()
            self.session = None
        if hasattr(self, "editor"):
            self.preview_text = str(self.editor.string())
            self.preview_caret = int(self.editor.selectedRange().location)
            self.editor.set_ghost("")

    @objc.python_method
    def preview_values(self):
        """Return text, caret index and selection length in Python character units."""
        text = str(self.editor.string())
        selection = self.editor.selectedRange()
        # Cocoa ranges count UTF-16 code units; PreviewSession uses Python indices.
        encoded = text.encode("utf-16-le")
        caret = len(encoded[:int(selection.location) * 2].decode("utf-16-le", errors="ignore"))
        end = len(encoded[:int(selection.location + selection.length) * 2].decode("utf-16-le", errors="ignore"))
        return text, caret, end - caret

    @objc.python_method
    def tick(self):
        """Refresh the visible step; preview inference requires the editor to own focus."""
        if not self.window.isVisible():
            self.stop_preview()
            return
        if self.step == 2:
            permissions = self.shell._permission_state()
            for key in ("accessibility", "input_monitoring"):
                button = self.permission_buttons[key]
                button.setTitle_("✓ Allowed" if permissions[key] else "Open System Settings…")
                button.setEnabled_(not permissions[key])
            config = self.shell.controller.config
            wanted = config.screenshot_context or config.screenshot_appearance
            button = self.permission_buttons["screen_recording"]
            button.setTitle_("✓ Allowed" if permissions["screen_recording"] else ("Request Access…" if wanted else "Not needed"))
            button.setEnabled_(wanted and not permissions["screen_recording"])
            self.primary.setEnabled_(permissions["accessibility"] and permissions["input_monitoring"])
        elif self.step == 3:
            self.refresh_model()
        elif self.step == 5:
            now = time.monotonic()
            if now - self.last_inline_check >= 1:
                self.last_inline_check = now
                enabled = system.macos_inline_prediction_enabled()
                self.inline_status.setStringValue_(
                    "Enabled — its grey text competes with BroTypist. Turn off Show inline predictive text in Keyboard → Text Input → Edit…." if enabled
                    else ("Disabled — no conflict." if enabled is False else "Couldn’t be detected. BroTypist only reads this setting and never changes it."))
                self.inline_status.setTextColor_(A.NSColor.systemOrangeColor() if enabled else A.NSColor.secondaryLabelColor())
        elif self.step == 4:
            # A visible welcome window alone is insufficient: another key
            # window or another responder must release the preview session.
            if not self.window.isKeyWindow() or self.window.firstResponder() != self.editor:
                self.stop_preview()
                self.preview_status.setStringValue_("Click the typing field to resume the local preview.")
                self.editor.set_ghost("")
                return
            if self.session is None:
                from .preview import PreviewSession
                self.session = PreviewSession(self.shell.controller.config, self.shell.controller)
                self.update_preview()
            self.refresh_preview()

    def textDidChange_(self, notification):
        self.update_preview()

    def textViewDidChangeSelection_(self, notification):
        self.update_preview()

    @objc.python_method
    def update_preview(self):
        """Clear stale presentation and submit current text unless IME text is marked."""
        if self.session:
            text, caret, selection = self.preview_values()
            self.editor.set_ghost("")
            if self.editor.hasMarkedText():
                self.session.dismiss()
                return
            self.session.update(text, caret, selection_length=selection)

    @objc.python_method
    def refresh_preview(self):
        """Poll the session into ghost text and status, without inserting its result."""
        if self.session:
            status = self.session.poll()
            self.editor.set_ghost(str(status.get("suggestion") or ""))
            message = str(status.get("message") or "Waiting for you to type…")
            accepted = int(status.get("accepted") or 0)
            if accepted and status.get("state") == "idle":
                message = f"Accepted {accepted} suggestion{'s' if accepted != 1 else ''} — that’s the whole trick."
            self.preview_status.setStringValue_(message)

    @objc.python_method
    def accept(self, action):
        """Insert an accepted word or suffix locally; return false when nothing is accepted."""
        if not self.session or int(self.editor.selectedRange().length) or self.editor.hasMarkedText():
            return False
        insertion = self.session.accept_word() if action == "word" else self.session.accept_all()
        if not insertion:
            return False
        self.editor.insertText_replacementRange_(insertion, self.editor.selectedRange())
        self.update_preview()
        self.refresh_preview()
        return True

    def windowDidResignKey_(self, notification):
        self.stop_preview()

    def windowShouldClose_(self, sender):
        self.stop_preview()
        if self.shell.no_tray or self.shell.status_item is None:
            self.shell.show_settings("general")
        return True

    @objc.python_method
    def close(self):
        """Stop preview ownership before hiding the retained, reusable window."""
        self.stop_preview()
        self.window.orderOut_(None)
