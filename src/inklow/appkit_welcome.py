"""Native six-step welcome and an isolated, real local-inference try-it field."""
from __future__ import annotations

from pathlib import Path

import AppKit as A
import objc
from Foundation import NSMutableAttributedString, NSAttributedString, NSObject, NSURL

from . import system
from .appkit_overlay import ghost_color, _dark, _rgb
from .resources import APP_NAME, REPOSITORY_URL, asset_path
from .shortcuts import mac_key_name, shortcut_from_event


class WelcomeView(A.NSView):
    def isFlipped(self):
        return True


def label(text, x, y, width, height, *, size=13, bold=False, secondary=False):
    view = A.NSTextField.wrappingLabelWithString_(text)
    view.setFrame_(A.NSMakeRect(x, y, width, height))
    view.setFont_(A.NSFont.systemFontOfSize_weight_(size, A.NSFontWeightSemibold if bold else A.NSFontWeightRegular))
    view.setTextColor_(A.NSColor.secondaryLabelColor() if secondary else A.NSColor.labelColor())
    return view


class GhostWordmark(A.NSTextField):
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
    @objc.python_method
    def set_ghost(self, text):
        if getattr(self, "ghost_text", "") != text:
            self.ghost_text = text
            self.setNeedsDisplay_(True)
        self.setAccessibilityHelp_(f"Suggestion: {text}" if text else "Type to request a local suggestion.")

    def drawRect_(self, dirty):
        objc.super(PreviewTextView, self).drawRect_(dirty)
        text = getattr(self, "ghost_text", "")
        selection = self.selectedRange()
        if not text or selection.length or self.hasMarkedText() or not self.window():
            return
        caret, actual = self.firstRectForCharacterRange_actualRange_(selection, None)
        local = self.convertRect_fromView_(self.window().convertRectFromScreen_(caret), None)
        x = A.NSMinX(local) + 1
        y = A.NSMinY(local)
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
    @objc.python_method
    def configure(self, shell):
        self.shell = shell
        self.step = 0
        self.session = None
        self.preview_text = ""
        self.preview_caret = 0
        self.actions = {}
        self.next_tag = 0
        self.permission_buttons = {}
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
        self.stop_preview()
        self.step = 0
        self.render()
        self.window.makeKeyAndOrderFront_(None)
        A.NSApplication.sharedApplication().activateIgnoringOtherApps_(True)

    @objc.python_method
    def render(self):
        self.stop_preview()
        self.actions = {}
        self.permission_buttons = {}
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
        hints = ("Takes about two minutes", "Local inference. Your choice of context.", "Allow both permissions to continue", "Choose a local model to continue", "Use your configured acceptance keys", "Find BroTypist in the menu bar")
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
        icon = A.NSImageView.alloc().initWithFrame_(A.NSMakeRect(324, 65, 112, 112))
        icon.setImage_(A.NSImage.alloc().initWithContentsOfFile_(str(asset_path("icons/app-128.png"))))
        self.root.addSubview_(icon)
        self.root.addSubview_(label("Welcome to", 56, 183, 648, 30, size=26, bold=True))
        self.root.subviews()[-1].setAlignment_(A.NSTextAlignmentCenter)
        mark = wordmark(648, 40, 32)
        mark.setFrameOrigin_(A.NSMakePoint(56, 218))
        self.root.addSubview_(mark)
        tagline = label("A little help with the words that come next.", 56, 266, 648, 25, secondary=True)
        tagline.setAlignment_(A.NSTextAlignmentCenter)
        self.root.addSubview_(tagline)
        self.card("Runs locally", "A model on this Mac, or your configured local loopback server. No account required.", 304, height=62)
        self.card("Stays out of the way", "Suggestions beside your caret. Accept the words you want, or keep typing to ignore them.", 374, height=62)
        self.card("Source on GitHub", "Browse the repository with an authorized GitHub account.", 444, height=62)
        self.button("View source", 558, 454, 130, lambda: A.NSWorkspace.sharedWorkspace().openURL_(NSURL.URLWithString_(REPOSITORY_URL)), "inklow.welcome.source")

    @objc.python_method
    def step_1(self):
        config = self.shell.controller.config
        self.heading("Your writing stays local", "Predictions run on this Mac. BroTypist does not send your writing to a remote inference service.")
        self.card("Text before cursor  →  Local model  →  Suggestion", f"Up to {config.context_chars:,} preceding characters are supplied to the model for an eligible request.", 202)
        self.card("What BroTypist reads", "Supported, focused text fields only. Password fields and excluded apps are skipped before capture. Clipboard and screen context are optional, explicit choices.", 292, height=94)
        self.card("What you control", "Writing history is opt-in and encrypted using OS secure storage. Aggregate statistics contain counts, not writing. Model downloads use the network.", 398, height=94)

    @objc.python_method
    def step_2(self):
        self.heading("Two macOS permissions", "Approve BroTypist in System Settings. These cards show the actual permission state, not a saved preference.")
        for key, title, detail, y in (("accessibility", "Accessibility", "Reads supported text near the caret and inserts only after you accept.", 210), ("input_monitoring", "Input Monitoring", "Listens for typing and configured acceptance shortcuts. Secure fields remain protected.", 330)):
            self.card(title, detail, y, height=104)
            self.permission_buttons[key] = self.button("Open System Settings…", 488, y + 9, 200, lambda k=key: system.open_permissions(k), f"inklow.welcome.{key}")
        self.root.addSubview_(label("Source runs may appear as Python or your terminal. macOS may require a restart after granting access. Screen Recording is optional and configured separately in Privacy.", 72, 455, 616, 46, size=11.5, secondary=True))

    @objc.python_method
    def step_3(self):
        self.heading("Choose your local model", "Use the existing verified GGUF catalog, choose a GGUF file, or configure a local loopback server in AI Models. Nothing downloads until you ask.")
        self.card("Selected model", self.shell._model_name(), 206, height=86)
        self.model_status = label("", 72, 310, 616, 54, secondary=True)
        self.root.addSubview_(self.model_status)
        self.button("Open Model Manager…", 56, 382, 210, lambda: self.shell.show_settings("models"), "inklow.welcome.models")
        self.root.addSubview_(label("Return here when your model is selected. The manager supports real download progress, cancellation and local files. This step never silently changes the backend or model.", 56, 436, 648, 53, size=12, secondary=True))

    @objc.python_method
    def step_4(self):
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
        self.heading("You’re all set", "BroTypist lives in your menu bar. Click its caret glyph to pause predictions, resume, manage models or open Settings.")
        image = A.NSImageView.alloc().initWithFrame_(A.NSMakeRect(336, 199, 88, 78))
        glyph = A.NSImage.alloc().initWithContentsOfFile_(str(asset_path("tray/active-template-36.png")))
        glyph.setTemplate_(True)
        image.setImage_(glyph)
        image.setImageScaling_(A.NSImageScaleProportionallyUpOrDown)
        self.root.addSubview_(image)
        config = self.shell.controller.config
        self.card("Your typing setup", f"Model: {self.shell._model_name()}\nNext word: {config.shortcuts.get('word') or 'Unassigned'}   ·   Full suggestion: {config.shortcuts.get('all') or 'Unassigned'}", 302, height=104)
        self.login = self.button("Launch BroTypist at login", 56, 435, 270, self.toggle_login, "inklow.welcome.launchAtLogin")
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
        if self.session:
            self.session.close()
            self.session = None
        if hasattr(self, "editor"):
            self.preview_text = str(self.editor.string())
            self.preview_caret = int(self.editor.selectedRange().location)
            self.editor.set_ghost("")

    @objc.python_method
    def preview_values(self):
        text = str(self.editor.string())
        selection = self.editor.selectedRange()
        # Cocoa ranges count UTF-16 code units; PreviewSession uses Python indices.
        encoded = text.encode("utf-16-le")
        caret = len(encoded[:int(selection.location) * 2].decode("utf-16-le", errors="ignore"))
        end = len(encoded[:int(selection.location + selection.length) * 2].decode("utf-16-le", errors="ignore"))
        return text, caret, end - caret

    @objc.python_method
    def tick(self):
        if not self.window.isVisible():
            self.stop_preview()
            return
        if self.step == 2:
            permissions = self.shell._permission_state()
            for key, button in self.permission_buttons.items():
                button.setTitle_("✓ Allowed" if permissions[key] else "Open System Settings…")
                button.setEnabled_(not permissions[key])
            self.primary.setEnabled_(permissions["accessibility"] and permissions["input_monitoring"])
        elif self.step == 3:
            config = self.shell.controller.config
            ready = config.backend != "gguf" or bool(config.model_path and Path(config.model_path).is_file())
            self.primary.setEnabled_(ready)
            status = self.shell.controller.status()
            download = status.get("download") or {}
            message = str(download.get("error") or download.get("message") or "")
            if not message:
                message = ("Local model file ready." if config.backend == "gguf" else "Local server configured. Try it next to check inference.") if ready else "No model file selected. Open Model Manager to download or choose one."
            self.model_status.setStringValue_(f"{self.shell._model_name()}\n{message}")
        elif self.step == 4:
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
        if self.session:
            text, caret, selection = self.preview_values()
            self.editor.set_ghost("")
            if self.editor.hasMarkedText():
                self.session.dismiss()
                return
            self.session.update(text, caret, selection_length=selection)

    @objc.python_method
    def refresh_preview(self):
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
        self.stop_preview()
        self.window.orderOut_(None)
