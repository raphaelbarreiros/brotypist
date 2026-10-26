"""Native Docs helper scenarios, using synthetic AX peers, never real accounts.

Requirements/risks/coverage (defined before implementation):
R1 exact native HTTPS edit URL + document-local focus -> positive hint; spoofed
   product/path/host, chrome focus, nested web area, missing bounds -> no hint.
R2 content-free capability -> no title/value reads from user-editable controls;
   hint/diagnostics/token contain no URL, document ID or document text.
R3 revalidate before actions -> changed document/window/field/PID, policy denial,
   Secure Input, expired/foreign/reused token -> no setting mutation.
R4 native observed checkbox transitions -> unchecked becomes checked once;
   already checked remains unchanged; unknown/mixed/ambiguous/localized metadata
   and action/readback failure never become a successful enable result.
R5 commit and verify -> unique scoped native OK action, reopen settings and read
   checked state; a reverted checkbox or missing commit control is unknown.
Scenarios above exercise identity, authorization and state transitions (P0).
Oracles: inspect final checkbox state and action journal, forbid user values,
assert exact result status, and scan exported metadata for private URL/ID/text.
No live compatibility claim: parent owns execution and human review.
"""
from threading import Event
from types import SimpleNamespace

import pytest

from inklow.native.base import BridgeBase
from inklow.native.macos_docs import DocsHelper


URL = "https://docs.google.com/document/d/private-disposable-id/edit"


class Peer:
    def __init__(self, role, parent=None, **attributes):
        self.attributes = {"AXRole": role, "AXEnabled": True, **attributes}
        if parent is not None:
            self.attributes["AXParent"] = parent
            parent.attributes.setdefault("AXChildren", []).append(self)
        self.pid = 1234


class NativeDocs(BridgeBase):
    def __init__(self):
        super().__init__()
        self._stopped = Event()
        self.pid = 1234
        self.secure = False
        self._ime_kind = "layout"
        self.actions = []
        self.reads = []
        self.commit_reverts = False
        self.fail_checkbox_press = False
        self.fail_action = False
        self.after_action = None
        self.app = Peer("AXApplication")
        self.window = Peer("AXWindow", self.app, AXDocument=URL)
        self.web = Peer("AXWebArea", self.window)
        self.editor = Peer("AXGroup", self.web, AXDOMClassList=["kix-appview-editor"],
                           bounds=(10, 20, 500, 300))
        self.page = Peer("AXGroup", self.editor,
                         AXDOMClassList=["kix-page-paginated", "canvas-first-page"],
                         bounds=(100, 40, 300, 250))
        self.field = Peer("AXTextArea", self.web, AXFocused=True)
        self.tools = Peer("AXMenuBarItem", self.web, AXTitle="Tools")
        self.menu = Peer("AXMenu", self.tools)
        self.item = Peer("AXMenuItem", self.menu, AXTitle="Accessibility settings")
        self.dialog = Peer("AXDialog", AXTitle="Accessibility settings")
        self.checkbox = Peer("AXCheckBox", self.dialog,
                             AXTitle="Turn on screen reader support", AXValue=0)
        self.braille = Peer("AXCheckBox", self.dialog,
                            AXTitle="Turn on braille support", AXValue=0)
        self.ok = Peer("AXButton", self.dialog, AXTitle="OK")
        self.dialog.attributes["AXDefaultButton"] = self.ok
        self.focus = self.field
        self.app.attributes["AXFocusedWindow"] = self.window
        self.ax = SimpleNamespace(
            AXIsProcessTrusted=lambda: True,
            AXUIElementCreateApplication=lambda pid: self.app,
            AXUIElementGetPid=lambda element, out: (0, element.pid),
            AXUIElementCopyActionNames=lambda element, out: (0, ["AXPress"]),
            AXUIElementPerformAction=self.press,
        )

    def _foreground(self):
        return self.pid, "/Applications/Browser", {"app_id": "com.google.Chrome",
                "browser": True, "domain": "docs.google.com", "name": "Chrome"}

    def secure_input_active(self):
        return self.secure

    def _attribute(self, element, name):
        self.reads.append((element, name))
        if name in {"AXValue", "AXSelectedText", "AXTitle", "AXDescription"}:
            if element is self.field or element is self.window or element is self.web:
                pytest.fail("User content/name was read")
            if name == "AXValue" and element.attributes["AXRole"] != "AXCheckBox":
                pytest.fail("An unrelated editable value was read")
        if element is self.app and name == "AXFocusedUIElement":
            return self.focus
        return element.attributes.get(name)

    def _field_bounds(self, element):
        return element.attributes.get("bounds", (10.0, 20.0, 500.0, 300.0))

    def press(self, element, action):
        self.actions.append(element)
        if self.fail_action:
            return 1
        if element is self.tools:
            self.focus = self.item
        elif element is self.item:
            if self.dialog not in self.web.attributes.get("AXChildren", []):
                self.web.attributes.setdefault("AXChildren", []).append(self.dialog)
            self.dialog.attributes["AXParent"] = self.web
            self.focus = self.checkbox
        elif element in (self.checkbox, self.braille):
            if not self.fail_checkbox_press:
                element.attributes["AXValue"] = 1
        elif element is self.ok:
            self.web.attributes["AXChildren"].remove(self.dialog)
            self.dialog.attributes.pop("AXParent", None)
            if self.commit_reverts:
                self.checkbox.attributes["AXValue"] = 0
            self.focus = self.field
        if self.after_action:
            self.after_action(element)
        return 0


@pytest.fixture
def native():
    return NativeDocs()


def test_hint_is_document_local_and_content_free_without_setting_probe(native):
    helper = DocsHelper(native)
    hint = helper.hint()
    assert hint["pid"] == 1234
    assert hint["app_id"] == "com.google.Chrome"
    assert hint["support"] == "unknown"
    assert hint["anchor"] == (100.0, 80.0, 1.0, 1.0)
    assert URL not in repr(hint) and "private-disposable-id" not in repr(hint)
    assert native.actions == []
    assert not any(name == "AXValue" for _, name in native.reads)
    assert helper.hint()["token"] == hint["token"]


@pytest.mark.parametrize("url", [
    "http://docs.google.com/document/d/id/edit",
    "https://docs.google.com.evil.example/document/d/id/edit",
    "https://docs.google.com./document/d/id/edit",
    "https://user@docs.google.com/document/d/id/edit",
    "https://docs.google.com:444/document/d/id/edit",
    "https://docs.google.com/spreadsheets/d/id/edit",
    "https://docs.google.com/presentation/d/id/edit",
    "https://docs.google.com/document/d/id/preview",
    "https://docs.google.com/document/d//edit",
    "https://docs.google.com/document/d/id/edit/extra",
    "https://docs.google.com/document/d/id%2Fspoof/edit",
    "https://docs.new", "Document - Google Docs", None,
])
def test_non_document_urls_never_offer_or_mutate(native, url):
    native.window.attributes["AXDocument"] = url
    helper = DocsHelper(native)
    assert helper.hint() is None
    assert helper.enable("unissued")["status"] == "unknown"
    assert native.actions == []


def test_address_bar_focus_never_qualifies(native):
    native.focus = Peer("AXTextField", native.window, AXFocused=True)
    assert DocsHelper(native).hint() is None
    assert native.actions == []


def test_nested_web_area_and_unowned_focus_are_not_document_proof(native):
    frame = Peer("AXWebArea", native.web)
    native.field.attributes["AXParent"] = frame
    assert DocsHelper(native).hint() is None
    native.field.attributes["AXParent"] = native.web
    native.field.pid = 999
    assert DocsHelper(native).hint() is None


@pytest.mark.parametrize("change", ["secure", "policy", "protected", "disabled", "unfocused", "bounds", "hidden"])
def test_unsafe_hint_has_no_value_reads_or_actions(native, change):
    if change == "secure":
        native.secure = True
    elif change == "policy":
        native._global_enabled = False
    elif change == "protected":
        native.field.attributes["AXProtectedContent"] = True
    elif change == "disabled":
        native.field.attributes["AXEnabled"] = 0
    elif change == "unfocused":
        native.field.attributes["AXFocused"] = 0
    elif change == "hidden":
        native.web.attributes["AXHidden"] = True
    else:
        native.field.attributes["bounds"] = (0, 0, 0, 1)
    assert DocsHelper(native).hint() is None
    assert native.actions == []
    assert not any(name == "AXValue" for _, name in native.reads)


@pytest.mark.parametrize("change", ["document", "window", "field", "pid", "secure", "policy"])
def test_stale_or_denied_token_cannot_navigate(native, change):
    helper = DocsHelper(native)
    token = helper.hint()["token"]
    if change == "document":
        native.window.attributes["AXDocument"] = URL.replace("private-disposable-id", "other")
    elif change == "window":
        native.app.attributes["AXFocusedWindow"] = Peer("AXWindow", AXDocument=URL)
    elif change == "field":
        native.focus = Peer("AXTextArea", native.web)
    elif change == "pid":
        native.pid = 999
    elif change == "secure":
        native.secure = True
    else:
        native._app_rules = {"domain:docs.google.com": {"enabled": False}}
    assert helper.enable(token)["status"] == "unknown"
    assert native.actions == []


def test_unchecked_is_enabled_committed_reopened_and_verified_once(native):
    helper = DocsHelper(native)
    result = helper.enable(helper.hint()["token"])
    assert result["status"] == "enabled"
    assert native.checkbox.attributes["AXValue"] == 1
    assert native.braille.attributes["AXValue"] == 1
    assert native.actions.count(native.checkbox) == 1
    assert native.actions.count(native.braille) == 1
    assert native.actions.count(native.ok) == 2
    assert "private-disposable-id" not in repr(result)


def test_already_checked_is_idempotent_and_single_use(native):
    native.checkbox.attributes["AXValue"] = 1
    native.braille.attributes["AXValue"] = 1
    helper = DocsHelper(native)
    token = helper.hint()["token"]
    assert helper.enable(token)["status"] == "already_enabled"
    assert native.checkbox not in native.actions
    assert native.braille not in native.actions
    assert native.actions.count(native.ok) == 2
    actions = list(native.actions)
    assert helper.enable(token)["status"] == "unknown"
    assert native.actions == actions


@pytest.mark.parametrize("value", [None, "false", "true", "0", 2, -1, 0.5])
def test_unknown_checkbox_state_is_never_toggled(native, value):
    native.checkbox.attributes["AXValue"] = value
    helper = DocsHelper(native)
    assert helper.enable(helper.hint()["token"])["status"] == "unknown"
    assert native.checkbox not in native.actions
    assert native.ok not in native.actions


@pytest.mark.parametrize("change", ["localized", "duplicate", "missing", "disabled", "no_commit"])
def test_missing_or_ambiguous_native_controls_never_claim_enabled(native, change):
    if change == "localized":
        native.checkbox.attributes["AXTitle"] = "Ativar suporte ao leitor de tela"
    elif change == "duplicate":
        Peer("AXCheckBox", native.dialog, AXTitle="Turn on screen reader support", AXValue=0)
    elif change == "missing":
        native.tools.attributes["AXTitle"] = "Unrelated menu"
    elif change == "disabled":
        native.checkbox.attributes["AXEnabled"] = False
    else:
        native.dialog.attributes["AXChildren"].remove(native.ok)
    helper = DocsHelper(native)
    assert helper.enable(helper.hint()["token"])["status"] == "unknown"
    assert native.checkbox not in native.actions


@pytest.mark.parametrize("change", ["failed_press", "unchanged", "reverted"])
def test_failed_setting_or_commit_readback_is_unknown(native, change):
    native.fail_action = change == "failed_press"
    native.fail_checkbox_press = change == "unchanged"
    native.commit_reverts = change == "reverted"
    helper = DocsHelper(native)
    assert helper.enable(helper.hint()["token"])["status"] == "unknown"


@pytest.mark.parametrize("change", ["document", "focus", "secure", "policy"])
def test_navigation_revalidates_before_checkbox_mutation(native, change):
    def interrupt(element):
        if element is not native.item:
            return
        if change == "document":
            native.window.attributes["AXDocument"] = URL.replace("private-disposable-id", "other")
        elif change == "focus":
            native.focus = Peer("AXTextField", native.window)
        elif change == "secure":
            native.secure = True
        else:
            native._global_enabled = False
    native.after_action = interrupt
    helper = DocsHelper(native)
    assert helper.enable(helper.hint()["token"])["status"] == "unknown"
    assert native.checkbox not in native.actions


def test_expired_and_foreign_tokens_are_rejected(native, monkeypatch):
    import inklow.native.macos_docs as docs
    clock = [100.0]
    monkeypatch.setattr(docs.time, "monotonic", lambda: clock[0])
    helper = DocsHelper(native)
    token = helper.hint()["token"]
    assert DocsHelper(native).enable(token)["status"] == "unknown"
    clock[0] += 31
    assert helper.enable(token)["status"] == "unknown"
    assert native.actions == []


def test_checkbox_changed_during_action_lookup_is_not_toggled(native):
    def actions(element, out):
        if element is native.checkbox:
            native.checkbox.attributes["AXValue"] = 1
        return 0, ["AXPress"]
    native.ax.AXUIElementCopyActionNames = actions
    helper = DocsHelper(native)
    result = helper.enable(helper.hint()["token"])
    assert result["status"] == "unknown"
    assert result["diagnostic"] == "docs_checkbox_changed"
    assert native.checkbox not in native.actions
    assert native.checkbox.attributes["AXValue"] == 1


def observed_portuguese(native):
    native.web.attributes["AXURL"] = URL + "?tab=t.0"
    group = Peer("AXGroup", native.web, AXSubrole="AXWebApplication")
    frame_group = Peer("AXGroup", group, AXDOMClassList=[
        "docs-texteventtarget-iframe", "docs-offscreen-z-index",
        "docs-texteventtarget-iframe-negative-top"])
    frame = Peer("AXWebArea", frame_group, AXURL="about:blank",
                 bounds=(0, 146, 625, 1))
    native.field.attributes.update(AXParent=frame, bounds=(0, 146, 625, 1))
    native.web.attributes["AXChildren"].remove(native.field)
    frame.attributes["AXChildren"] = [native.field]
    native.web.attributes["bounds"] = (0, 146, 1728, 971)
    native.window.attributes["bounds"] = (0, 0, 1728, 1117)
    native.editor.attributes["bounds"] = (0, 275, 1672, 842)
    native.page.attributes["bounds"] = (420, 291, 794, 826)
    native.tools.attributes.update(AXRole="AXMenuItem", AXTitle="Ferramentas",
                                   AXDOMIdentifier="docs-tools-menu")
    native.tools.attributes["AXChildren"].remove(native.menu)
    native.menu.attributes["AXParent"] = native.web
    native.web.attributes["AXChildren"].append(native.menu)
    native.menu.attributes["AXHidden"] = True
    native.item.attributes["AXTitle"] = "Acessibilidade c"
    native.dialog.attributes.update(AXRole="AXGroup", AXSubrole="AXApplicationDialog",
        AXTitle="Configurações de acessibilidade",
        AXDescription="Configurações de acessibilidade")
    native.dialog.attributes.pop("AXDefaultButton")
    native.checkbox.attributes["AXTitle"] = "Ativar a compatibilidade com o leitor de tela"
    native.braille.attributes["AXTitle"] = "Ativar suporte a braille"
    Peer("AXButton", native.web, AXTitle="Ferramentas")
    Peer("AXCheckBox", native.dialog, AXTitle="Ativar braille", AXValue=0)
    def reveal(element):
        if element is native.tools:
            native.menu.attributes["AXHidden"] = False
    native.after_action = reveal
    return frame_group, frame


def test_observed_input_iframe_portuguese_portal_and_commit(native):
    observed_portuguese(native)
    helper = DocsHelper(native)
    hint = helper.hint()
    assert hint["anchor"] == (420, 331, 1, 1)
    assert helper.enable(hint["token"])["status"] == "enabled"
    assert native.actions.count(native.checkbox) == 1
    assert native.actions.count(native.braille) == 1
    assert native.actions.count(native.ok) == 2
    assert all(name != "AXValue" or element in (native.checkbox, native.braille)
               for element, name in native.reads)


@pytest.mark.parametrize("change", ["class", "frame_url", "outer_url", "extra_frame"])
def test_observed_iframe_exception_is_narrow(native, change):
    group, frame = observed_portuguese(native)
    if change == "class":
        group.attributes["AXDOMClassList"] = ["unrelated-frame"]
    elif change == "frame_url":
        frame.attributes["AXURL"] = "https://evil.example/"
    elif change == "outer_url":
        native.web.attributes["AXURL"] = URL.replace("private-disposable-id", "other")
    else:
        nested = Peer("AXWebArea", frame, AXURL="about:blank")
        native.field.attributes["AXParent"] = nested
    assert DocsHelper(native).hint() is None
    assert native.actions == []


def test_account_scoped_url_has_canonical_identity(native):
    helper = DocsHelper(native)
    first = helper.hint()
    native.window.attributes["AXDocument"] = URL.replace("/document/d/", "/document/u/0/d/")
    second = helper.hint()
    assert first["document_id"] == second["document_id"]
    assert first["token"] == second["token"]


@pytest.mark.parametrize("moment", ["before", "after_commit"])
def test_editor_rebuild_is_accepted_only_after_native_commit(native, moment):
    helper = DocsHelper(native)
    token = helper.hint()["token"]
    def rebuild():
        native.field = Peer("AXTextArea", native.web, AXFocused=True)
        native.focus = native.field
    if moment == "before":
        rebuild()
    else:
        native.after_action = lambda element: rebuild() if element is native.ok else None
    result = helper.enable(token)
    assert result["status"] == ("unknown" if moment == "before" else "enabled")
    if moment == "before":
        assert native.actions == []


@pytest.mark.parametrize("kind", ["ime", "unknown", "dead_key"])
def test_composition_unavailable_never_issues_capability(native, kind):
    native._ime_kind = "layout" if kind == "dead_key" else kind
    native._dead_key = kind == "dead_key"
    assert DocsHelper(native).hint() is None
    assert native.actions == []


def test_marked_text_blocks_hint_without_reading_text(native):
    native.field.attributes["AXMarkedTextRange"] = object()
    native.ax.kAXValueCFRangeType = 4
    native.ax.AXValueGetValue = lambda *args: (True, (0, 2))
    assert DocsHelper(native).hint() is None
    assert native.actions == []
    assert not any(name == "AXValue" for _, name in native.reads)


def test_control_detached_during_action_lookup_cannot_mutate(native):
    def actions(element, out):
        if element is native.checkbox:
            native.checkbox.attributes["AXParent"] = Peer("AXGroup")
        return 0, ["AXPress"]
    native.ax.AXUIElementCopyActionNames = actions
    helper = DocsHelper(native)
    assert helper.enable(helper.hint()["token"])["status"] == "unknown"
    assert native.checkbox not in native.actions


def test_portal_in_unowned_menu_is_not_actionable(native):
    observed_portuguese(native)
    native.menu.attributes["AXParent"] = Peer("AXWindow")
    helper = DocsHelper(native)
    assert helper.enable(helper.hint()["token"])["status"] == "unknown"
    assert native.item not in native.actions


def test_native_ok_does_not_need_default_but_must_be_unique(native):
    native.dialog.attributes.pop("AXDefaultButton")
    Peer("AXButton", native.dialog, AXTitle="OK")
    helper = DocsHelper(native)
    assert helper.enable(helper.hint()["token"])["status"] == "unknown"
    assert native.checkbox not in native.actions


@pytest.mark.parametrize("change", ["no_margin", "hidden", "missing", "offscreen"])
def test_accessory_requires_observed_visible_page_margin(native, change):
    if change == "no_margin":
        native.page.attributes["bounds"] = (20, 40, 300, 250)
    elif change == "hidden":
        native.page.attributes["AXHidden"] = True
    elif change == "missing":
        native.editor.attributes["AXDOMClassList"] = ["unknown-editor"]
    else:
        native.page.attributes["bounds"] = (100, 900, 300, 250)
    assert DocsHelper(native).hint() is None
    assert native.actions == []


@pytest.mark.parametrize("which", ["menu", "dialog"])
def test_visibility_catches_up_after_native_press(native, monkeypatch, which):
    helper = DocsHelper(native)
    token = helper.hint()["token"]
    clock = [0.0]
    waits = []
    monkeypatch.setattr("inklow.native.macos_docs.time.monotonic", lambda: clock[0])
    def sleep(delay):
        waits.append(delay)
        clock[0] += delay
        getattr(native, which).attributes["AXHidden"] = False
    monkeypatch.setattr("inklow.native.macos_docs.time.sleep", sleep)
    def delayed(element):
        if element is (native.tools if which == "menu" else native.item):
            getattr(native, which).attributes["AXHidden"] = True
    native.after_action = delayed
    assert helper.enable(token)["status"] == "enabled"
    assert waits
    assert native.actions.count(native.checkbox) == 1


def test_hidden_menu_wait_revalidates_security_without_mutation(native, monkeypatch):
    helper = DocsHelper(native)
    token = helper.hint()["token"]
    def delayed(element):
        if element is native.tools:
            native.menu.attributes["AXHidden"] = True
    native.after_action = delayed
    def sleep(delay):
        native.secure = True
    monkeypatch.setattr("inklow.native.macos_docs.time.sleep", sleep)
    result = helper.enable(token)
    assert result["diagnostic"] == "docs_security_unavailable"
    assert native.actions == [native.tools]
    assert native.checkbox.attributes["AXValue"] == 0


def test_cached_page_reuses_handles_but_not_geometry(native):
    helper = DocsHelper(native)
    first = helper.hint()
    native.reads.clear()
    native.page.attributes["bounds"] = (150, 70, 300, 200)
    second = helper.hint()
    assert second["token"] == first["token"]
    assert second["anchor"] == (150, 110, 1.0, 1.0)
    assert not any(name == "AXChildren" for _, name in native.reads)


@pytest.mark.parametrize("change", ["owner", "detached", "hidden", "margin", "class"])
def test_cached_page_failure_revokes_capability_and_handles(native, change):
    helper = DocsHelper(native)
    token = helper.hint()["token"]
    if change == "owner":
        native.page.pid = 999
    elif change == "detached":
        native.page.attributes.pop("AXParent")
    elif change == "hidden":
        native.page.attributes["AXHidden"] = True
    elif change == "margin":
        native.page.attributes["bounds"] = (20, 40, 300, 250)
    else:
        native.page.attributes["AXDOMClassList"] = []
    assert helper.hint() is None
    assert helper.enable(token)["status"] == "unknown"
    assert native.actions == []
    native.page.pid = native.pid
    native.page.attributes.update(
        AXParent=native.editor, AXHidden=False,
        AXDOMClassList=["kix-page-paginated", "canvas-first-page"],
        bounds=(100, 40, 300, 250))
    native.reads.clear()
    assert helper.hint() is not None
    assert any(name == "AXChildren" for _, name in native.reads)


@pytest.mark.parametrize("failure", ["timeout", "owner", "ambiguous"])
def test_hidden_menu_never_retries_ownership_or_ambiguity(native, monkeypatch, failure):
    helper = DocsHelper(native)
    token = helper.hint()["token"]
    clock = [0.0]
    sleeps = []
    monkeypatch.setattr("inklow.native.macos_docs.time.monotonic", lambda: clock[0])
    def sleep(delay):
        sleeps.append(delay)
        clock[0] += delay
    monkeypatch.setattr("inklow.native.macos_docs.time.sleep", sleep)
    def delayed(element):
        if element is native.tools:
            native.menu.attributes["AXHidden"] = True
            if failure == "owner":
                native.menu.pid = 999
            elif failure == "ambiguous":
                Peer("AXMenuItem", native.menu, AXTitle="Accessibility settings")
    native.after_action = delayed
    result = helper.enable(token)
    assert result["status"] == "unknown"
    assert native.actions == [native.tools]
    if failure == "timeout":
        assert 0.6 <= clock[0] < 0.64
        assert result["diagnostic"] == "native_control_hidden"
    else:
        assert sleeps == []
        assert result["diagnostic"] == (
            "native_owner_unverified" if failure == "owner" else "docs_controls_ambiguous")


class NativeURL:
    def __init__(self, value):
        self.value = value

    def absoluteString(self):
        return self.value


@pytest.mark.parametrize("frame_url", ["about:blank", "https://evil.example/"])
def test_native_url_objects_preserve_iframe_origin_guard(native, frame_url):
    _, frame = observed_portuguese(native)
    native.window.attributes["AXDocument"] = NativeURL(URL)
    native.web.attributes["AXURL"] = NativeURL(URL)
    frame.attributes["AXURL"] = NativeURL(frame_url)
    helper = DocsHelper(native)
    hint = helper.hint()
    if frame_url != "about:blank":
        assert hint is None
        assert native.actions == []
    else:
        assert hint is not None
        assert helper.enable(hint["token"])["status"] == "enabled"


@pytest.mark.parametrize("reader,braille", [(0, 0), (0, 1), (1, 0), (1, 1)])
def test_both_preferences_are_independently_idempotent(native, reader, braille):
    native.checkbox.attributes["AXValue"] = reader
    native.braille.attributes["AXValue"] = braille
    helper = DocsHelper(native)
    result = helper.enable(helper.hint()["token"])
    assert result["status"] == ("already_enabled" if reader and braille else "enabled")
    assert native.actions.count(native.checkbox) == 1 - reader
    assert native.actions.count(native.braille) == 1 - braille
    assert native.checkbox.attributes["AXValue"] == native.braille.attributes["AXValue"] == 1
    assert native.actions.count(native.item) == native.actions.count(native.ok) == 2


@pytest.mark.parametrize("moment", ["before", "dialog", "commit", "both"])
@pytest.mark.parametrize("focus_kind", ["tools", "toolbar"])
def test_only_owned_post_action_tools_focus_is_retained(native, moment, focus_kind):
    observed_portuguese(native)
    toolbar = Peer("AXButton", native.web, AXTitle="Unrelated toolbar")
    helper = DocsHelper(native)
    token = helper.hint()["token"]
    def change(element):
        if element is native.tools:
            native.menu.attributes["AXHidden"] = False
        if ((moment in ("dialog", "both") and element is native.item)
                or (moment in ("commit", "both") and element is native.ok)):
            native.focus = native.tools if focus_kind == "tools" else toolbar
    native.after_action = change
    if moment == "before":
        native.focus = native.tools if focus_kind == "tools" else toolbar
    result = helper.enable(token)
    expected = moment != "before" and focus_kind == "tools"
    assert result["status"] == ("enabled" if expected else "unknown")
    if moment == "before":
        assert native.actions == []
    if moment == "dialog" and not expected:
        assert native.checkbox not in native.actions
        assert native.braille not in native.actions


@pytest.mark.parametrize("value", [None, "1", 2, -1, 0.5])
def test_braille_unknown_state_is_not_toggled_or_committed(native, value):
    native.checkbox.attributes["AXValue"] = 1
    native.braille.attributes["AXValue"] = value
    helper = DocsHelper(native)
    assert helper.enable(helper.hint()["token"])["status"] == "unknown"
    assert native.braille not in native.actions
    assert native.ok not in native.actions


def test_braille_enablement_wait_follows_only_confirmed_reader_transition(native, monkeypatch):
    native.braille.attributes["AXEnabled"] = False
    waits = []
    def sleep(delay):
        waits.append(delay)
        native.braille.attributes["AXEnabled"] = True
    monkeypatch.setattr("inklow.native.macos_docs.time.sleep", sleep)
    helper = DocsHelper(native)
    assert helper.enable(helper.hint()["token"])["status"] == "enabled"
    assert waits
    assert native.actions.count(native.checkbox) == native.actions.count(native.braille) == 1


@pytest.mark.parametrize("enabled", [False, None, "true"])
def test_braille_unavailable_without_reader_transition_is_not_retried(native, monkeypatch, enabled):
    native.checkbox.attributes["AXValue"] = 1
    native.braille.attributes["AXEnabled"] = enabled
    def forbidden_wait(delay):
        pytest.fail("No dependent transition authorized a wait")
    monkeypatch.setattr("inklow.native.macos_docs.time.sleep", forbidden_wait)
    helper = DocsHelper(native)
    assert helper.enable(helper.hint()["token"])["status"] == "unknown"
    assert native.braille not in native.actions
    assert native.ok not in native.actions


@pytest.mark.parametrize("change", ["rollback", "secure", "owner", "focus"])
def test_braille_commit_readback_rejects_rollback_and_security_changes(native, change):
    def interrupt(element):
        if element is native.ok:
            if change == "rollback":
                native.braille.attributes["AXValue"] = 0
            elif change == "secure":
                native.secure = True
            elif change == "owner":
                native.braille.pid = 999
            else:
                native.focus = Peer("AXButton", native.web)
    native.after_action = interrupt
    helper = DocsHelper(native)
    assert helper.enable(helper.hint()["token"])["status"] == "unknown"
    assert native.actions.count(native.braille) == 1
    assert native.actions.count(native.ok) == 1


def test_braille_transition_wait_aborts_on_security_change(native, monkeypatch):
    native.braille.attributes["AXEnabled"] = False
    def sleep(delay):
        native.secure = True
    monkeypatch.setattr("inklow.native.macos_docs.time.sleep", sleep)
    helper = DocsHelper(native)
    result = helper.enable(helper.hint()["token"])
    assert result["diagnostic"] == "docs_security_unavailable"
    assert native.actions.count(native.checkbox) == 1
    assert native.braille not in native.actions
    assert native.ok not in native.actions


def test_disabled_braille_transition_wait_is_bounded_without_second_click(native, monkeypatch):
    native.braille.attributes["AXEnabled"] = False
    clock = [0.0]
    monkeypatch.setattr("inklow.native.macos_docs.time.monotonic", lambda: clock[0])
    def sleep(delay):
        clock[0] += delay
    monkeypatch.setattr("inklow.native.macos_docs.time.sleep", sleep)
    helper = DocsHelper(native)
    assert helper.enable(helper.hint()["token"])["status"] == "unknown"
    assert 0.6 <= clock[0] < 0.64
    assert native.actions.count(native.checkbox) == 1
    assert native.braille not in native.actions
    assert native.ok not in native.actions


@pytest.mark.parametrize("change", ["checked", "disabled", "owner", "detached"])
def test_braille_action_metadata_races_never_toggle(native, change):
    native.checkbox.attributes["AXValue"] = 1
    def actions(element, out):
        if element is native.braille:
            if change == "checked":
                element.attributes["AXValue"] = 1
            elif change == "disabled":
                element.attributes["AXEnabled"] = False
            elif change == "owner":
                element.pid = 999
            else:
                element.attributes["AXParent"] = Peer("AXGroup")
        return 0, ["AXPress"]
    native.ax.AXUIElementCopyActionNames = actions
    helper = DocsHelper(native)
    assert helper.enable(helper.hint()["token"])["status"] == "unknown"
    assert native.braille not in native.actions
    assert native.ok not in native.actions


@pytest.mark.parametrize("identifier", ["docs-texteventtarget-descendant", "unrelated-group"])
def test_braille_editor_focus_remains_document_scoped_after_commit(native, identifier):
    observed_portuguese(native)
    native.field.attributes.update(AXRole="AXGroup", AXDOMIdentifier=identifier)
    helper = DocsHelper(native)
    hint = helper.hint()
    if identifier != "docs-texteventtarget-descendant":
        assert hint is None and native.actions == []
        return
    assert hint is not None
    native.field.attributes["AXRole"] = "AXTextArea"
    before = native.after_action

    def rebuild(element):
        before(element)
        if element is native.ok:
            native.field.attributes["AXRole"] = "AXGroup"
    native.after_action = rebuild
    assert helper.enable(hint["token"])["status"] == "enabled"
    assert native.checkbox.attributes["AXValue"] == native.braille.attributes["AXValue"] == 1


def test_unverified_native_action_provider_never_mutates_docs_preferences(native):
    original = native._foreground

    def foreground():
        pid, executable, metadata = original()
        return pid, executable, {**metadata, "app_id": "at.studio.AsideBrowser"}

    native._foreground = foreground
    helper = DocsHelper(native)
    hint = helper.hint()
    assert hint is not None
    result = helper.enable(hint["token"])
    assert result["status"] == "unknown" and result["diagnostic"] == "docs_native_actions_unverified"
    assert native.actions == []
    assert native.checkbox.attributes["AXValue"] == native.braille.attributes["AXValue"] == 0


def test_focus_window_attribute_cannot_disagree_with_ancestry(native):
    other_window = Peer("AXWindow", native.app, AXDocument=URL)
    native.field.attributes["AXWindow"] = other_window
    helper = DocsHelper(native)
    assert helper.hint() is None
    assert helper.diagnostic == "docs_focus_outside_document"
    assert native.actions == []
    assert not any(name == "AXValue" for _, name in native.reads)


def test_outer_scroll_clip_cannot_expose_an_invisible_page_margin(native):
    native.window.attributes["AXChildren"].remove(native.web)
    clip = Peer("AXScrollArea", native.window, bounds=(400, 20, 100, 300))
    clip.attributes["AXChildren"] = [native.web]
    native.web.attributes["AXParent"] = clip
    helper = DocsHelper(native)
    assert helper.hint() is None
    assert helper.diagnostic == "docs_anchor_unverified"
    assert native.actions == []
    assert not any(name == "AXValue" for _, name in native.reads)
