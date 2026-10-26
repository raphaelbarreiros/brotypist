"""Conservative, native-AX-only Google Docs accessibility settings helper.

Official English control names and menu sequence:
https://support.google.com/docs/answer/6282736?hl=en#Turn_on_screen_reader_support
Reference requires BOTH screen reader and braille support:
Official document edit URL shape:
https://developers.google.com/workspace/docs/api/how-tos/documents

These sources establish names, not a browser's macOS AX projection. Missing,
unobserved or ambiguous controls remain unknown. No keyboard shortcut,
DOM, browser scripting, address-bar value, document text or title is used.
Callers must apply their runtime pause gate before hint/enable; the adapter also
rechecks the bridge's app/domain policy and Secure Input before every action.
Successful enabling proves the observed preference only, not inline capability.
"""
from __future__ import annotations

from dataclasses import dataclass
import hashlib
import hmac
import math
import re
import secrets
import time
from urllib.parse import urlsplit

from .base import BROWSER_BUNDLES

HELP_URL = "https://support.google.com/docs/answer/6282736?hl=en#Turn_on_screen_reader_support"
TOKEN_SECONDS = 30.0
UI_TIMEOUT = 0.6
MAX_NODES = 2048
MAX_ANCESTORS = 32
_EDIT_PATH = re.compile(r"/document/(?:u/[0-9]+/)?d/([A-Za-z0-9_-]+)/edit")


class _Unknown(Exception):
    """Only fixed, content-free reason codes cross the native boundary."""


class _Hidden(_Unknown):
    """Transient visibility only; never authorizes a native action."""


@dataclass
class _Target:
    token: str
    created: float
    app_id: str
    pid: int
    document_id: str
    window_id: str
    window: object
    web: object
    focus: object
    anchor: tuple[float, float, float, float]
    tools: object = None


class DocsHelper:
    """One short-lived document/focus capability; never persist hints or tokens.

    ``hint()`` returns a metadata dictionary or None; it does not inspect the
    preference or mutate AX. ``enable(token)`` consumes a previously issued
    token and returns status enabled/already_enabled/unknown, diagnostic and
    official help_url. ``diagnostic`` also explains the last rejected hint.
    All native operations use the existing bridge lock and AX primitives.
    """

    def __init__(self, bridge):
        self.bridge = bridge
        self._salt = secrets.token_bytes(32)
        self._target: _Target | None = None
        self._geometry_scope = None
        self._geometry_handles = None
        self.diagnostic = "docs_not_observed"

    def _get(self, element, name):
        return self.bridge._attribute(element, name)

    def _pid(self, element, expected):
        error, actual = self.bridge.ax.AXUIElementGetPid(element, None)
        if error or actual != expected:
            raise _Unknown("native_owner_unverified")

    def _safe(self, element, *, allow_hidden=False, allow_disabled=False):
        enabled = self._get(element, "AXEnabled")
        if (self._get(element, "AXProtectedContent")
                or self._get(element, "AXSubrole") == "AXSecureTextField"
                or (not allow_disabled and enabled is not None and not enabled)):
            raise _Unknown("native_control_unavailable")
        secure_description = getattr(self.bridge, "_secure_description", None)
        if secure_description is not None and self._get(element, "AXRoleDescription") == secure_description:
            raise _Unknown("native_control_protected")
        if self._get(element, "AXHidden") and not allow_hidden:
            raise _Hidden("native_control_hidden")

    def _ancestry(self, element, *, allow_hidden=False):
        result = []
        for _ in range(MAX_ANCESTORS):
            if element is None:
                return result
            if any(element == seen for seen in result):
                raise _Unknown("native_ancestry_ambiguous")
            self._safe(element, allow_hidden=allow_hidden)
            result.append(element)
            element = self._get(element, "AXParent")
        raise _Unknown("native_ancestry_unbounded")

    def _document_id(self, window, attribute="AXDocument"):
        value = self._get(window, attribute)
        if value is not None and hasattr(value, "absoluteString"):
            value = str(value.absoluteString())
        if not isinstance(value, str) or any(ord(char) <= 32 for char in value):
            raise _Unknown("docs_url_unverified")
        try:
            url = urlsplit(value)
            if (url.scheme != "https" or url.netloc not in ("docs.google.com", "docs.google.com:443")
                    or not _EDIT_PATH.fullmatch(url.path)):
                raise _Unknown("docs_url_unverified")
            # The URL and literal document identifier remain transient. A keyed
            # digest cannot be joined against IDs outside this helper instance.
            return hmac.new(self._salt, _EDIT_PATH.fullmatch(url.path)[1].encode("utf-8"),
                            hashlib.sha256).hexdigest()
        except ValueError:
            raise _Unknown("docs_url_unverified") from None

    def _session(self, target=None, scopes=(), *, committed=False, allow_hidden=False):
        bridge = self.bridge
        if (bridge._stopped.is_set() or not bridge.ax.AXIsProcessTrusted()
                or bridge.secure_input_active() or getattr(bridge, "_dead_key", False)
                or getattr(bridge, "_ime_kind", "unknown") != "layout"):
            raise _Unknown("docs_security_unavailable")
        foreground = bridge._foreground()
        if foreground is None:
            raise _Unknown("docs_foreground_unavailable")
        pid, executable, metadata = foreground
        app_id = metadata.get("app_id", "")
        if app_id.casefold() not in BROWSER_BUNDLES:
            raise _Unknown("docs_browser_unverified")
        if not bridge._allowed(app_id, metadata.get("domain", ""), executable, metadata.get("name", "")):
            raise _Unknown("docs_policy_denied")
        app = bridge.ax.AXUIElementCreateApplication(pid)
        window = self._get(app, "AXFocusedWindow")
        if window is None:
            raise _Unknown("docs_window_unverified")
        self._pid(window, pid)
        self._safe(window)
        document_id = self._document_id(window)
        if not bridge._allowed(app_id, "docs.google.com", executable, metadata.get("name", "")):
            raise _Unknown("docs_policy_denied")
        if target is not None and (target.pid != pid or target.app_id != app_id
                or target.window != window or target.document_id != document_id):
            raise _Unknown("docs_target_changed")
        focus = self._get(app, "AXFocusedUIElement")
        focused = self._get(focus, "AXFocused") if focus is not None else None
        if focus is None or (focused is not None and not focused):
            raise _Unknown("docs_focus_unverified")
        self._pid(focus, pid)
        editor_focus = self._get(focus, "AXRole") == "AXTextArea" or (
            self._get(focus, "AXRole") == "AXGroup"
            and self._get(focus, "AXDOMIdentifier") == "docs-texteventtarget-descendant")
        if target is None and not editor_focus:
            raise _Unknown("docs_focus_unverified")
        if self._get(focus, "AXRole") == "AXTextArea":
            marked = self._get(focus, "AXMarkedTextRange")
            if marked is not None:
                valid, value = bridge.ax.AXValueGetValue(marked, bridge.ax.kAXValueCFRangeType, None)
                if not valid or value[1] != 0:
                    raise _Unknown("docs_security_unavailable")
        ancestors = self._ancestry(focus, allow_hidden=allow_hidden)
        for node in ancestors:
            self._pid(node, pid)
        if not any(node == window for node in ancestors):
            raise _Unknown("docs_focus_outside_document")
        web_areas = [node for node in ancestors if self._get(node, "AXRole") == "AXWebArea"]
        if not web_areas:
            raise _Unknown("docs_focus_outside_document")
        web = web_areas[-1]
        self._pid(web, pid)
        if len(web_areas) != 1:
            frame = web_areas[0]
            parent = self._get(frame, "AXParent")
            classes = self._get(parent, "AXDOMClassList") or ()
            frame_url = self._get(frame, "AXURL")
            if frame_url is not None and hasattr(frame_url, "absoluteString"):
                frame_url = str(frame_url.absoluteString())
            if (len(web_areas) != 2 or not editor_focus
                    or frame_url != "about:blank"
                    or self._get(parent, "AXRole") != "AXGroup"
                    or "docs-texteventtarget-iframe" not in classes
                    or self._document_id(web, "AXURL") != document_id):
                raise _Unknown("docs_focus_outside_document")
        for attribute in ("AXDocument", "AXURL"):
            if self._get(web, attribute) is not None and self._document_id(web, attribute) != document_id:
                raise _Unknown("docs_document_ambiguous")
        if target is not None:
            if target.web != web:
                raise _Unknown("docs_target_changed")
            if focus != target.focus and not any(
                    any(node == scope for node in ancestors) for scope in scopes):
                if not committed or (not editor_focus and focus != target.tools):
                    raise _Unknown("docs_focus_changed")
                target.focus = focus
        return app_id, pid, document_id, window, web, focus, ancestors

    def _anchor(self, ancestors, identity):
        # This is an accessory viewport-margin anchor, never a text caret.
        bounds = self.bridge._field_bounds(ancestors[0])
        if (bounds is None or not all(math.isfinite(v) for v in bounds)
                or bounds[2] <= 0 or bounds[3] <= 0):
            raise _Unknown("docs_anchor_unverified")
        viewport = next(node for node in reversed(ancestors)
                        if self._get(node, "AXRole") == "AXWebArea")
        clip = None
        for node in ancestors[ancestors.index(viewport):]:
            if self._get(node, "AXRole") not in ("AXWebArea", "AXWindow"):
                continue
            bounds = self.bridge._field_bounds(node)
            if bounds is None or not all(math.isfinite(v) for v in bounds):
                raise _Unknown("docs_anchor_unverified")
            x, y, width, height = bounds
            if width < 80 or height < 80:
                raise _Unknown("docs_anchor_unverified")
            if clip is None:
                clip = bounds
            else:
                left, top = max(x, clip[0]), max(y, clip[1])
                right = min(x + width, clip[0] + clip[2])
                bottom = min(y + height, clip[1] + clip[3])
                clip = (left, top, right - left, bottom - top)
        if clip is None or clip[2] < 80 or clip[3] < 80:
            raise _Unknown("docs_anchor_unverified")
        scope = (identity, tuple(ancestors))
        if self._geometry_scope != scope:
            self._geometry_scope = None
            self._geometry_handles = None
        if self._geometry_handles is None:
            editors, pages = [], []
            for node in self._nodes(viewport):
                if self._get(node, "AXRole") != "AXGroup" or self._get(node, "AXHidden"):
                    continue
                classes = self._get(node, "AXDOMClassList") or ()
                if "kix-appview-editor" in classes:
                    editors.append(node)
                if "kix-page-paginated" in classes and "canvas-first-page" in classes:
                    pages.append(node)
            if len(editors) != 1 or len(pages) != 1:
                raise _Unknown("docs_anchor_unverified")
            self._geometry_scope = scope
            self._geometry_handles = editors[0], pages[0]
        editor, page = self._geometry_handles
        expected_pid = identity[1]
        for node, required in (
                (editor, {"kix-appview-editor"}),
                (page, {"kix-page-paginated", "canvas-first-page"})):
            chain = self._ancestry(node)
            for parent in chain:
                self._pid(parent, expected_pid)
            if (self._get(node, "AXRole") != "AXGroup"
                    or not required.issubset(self._get(node, "AXDOMClassList") or ())
                    or not any(parent == viewport for parent in chain)
                    or not any(parent == ancestors[-1] for parent in chain)
                    or (node == page and not any(parent == editor for parent in chain))):
                raise _Unknown("docs_anchor_unverified")
        editor_bounds = self.bridge._field_bounds(editor)
        page_bounds = self.bridge._field_bounds(page)
        for bounds in (editor_bounds, page_bounds):
            if (bounds is None or not all(math.isfinite(v) for v in bounds)
                    or bounds[2] <= 0 or bounds[3] <= 0):
                raise _Unknown("docs_anchor_unverified")
        left = max(clip[0], editor_bounds[0])
        top = max(clip[1], editor_bounds[1], page_bounds[1])
        right = min(clip[0] + clip[2], editor_bounds[0] + editor_bounds[2])
        bottom = min(clip[1] + clip[3], editor_bounds[1] + editor_bounds[3],
                     page_bounds[1] + page_bounds[3])
        # Existing docs_help accessory is left/above this metadata anchor.
        # Require actual free editor margin; never move it onto paper.
        if page_bounds[0] - left < 34 or page_bounds[0] > right or top + 40 > bottom:
            raise _Unknown("docs_anchor_unverified")
        return (page_bounds[0], top + 40, 1.0, 1.0)

    def hint(self):
        with self.bridge._lock:
            try:
                app_id, pid, document_id, window, web, focus, ancestors = self._session()
                anchor = self._anchor(ancestors, (app_id, pid, document_id, window, web, focus))
                previous = self._target
                if (previous is None or time.monotonic() - previous.created >= TOKEN_SECONDS
                        or (previous.app_id, previous.pid, previous.document_id, previous.window,
                            previous.web, previous.focus) != (app_id, pid, document_id, window, web, focus)):
                    previous = _Target(secrets.token_urlsafe(24), time.monotonic(), app_id, pid,
                                       document_id, secrets.token_hex(12), window, web, focus, anchor)
                    self._target = previous
                previous.anchor = anchor
                # Resolve focus/document again after potentially blocking AX geometry.
                self._session(previous)
                self.diagnostic = "docs_preference_unknown"
                return {"token": previous.token, "app_id": app_id, "pid": pid,
                        "window_id": previous.window_id, "document_id": document_id,
                        "anchor": anchor, "support": "unknown", "help_url": HELP_URL}
            except _Unknown as exc:
                self.diagnostic = str(exc)
            except Exception:
                self.diagnostic = "docs_native_metadata_unavailable"
            self._target = None
            self._geometry_scope = None
            self._geometry_handles = None
            return None

    def _nodes(self, root):
        queue, result, seen = [root], [], set()
        while queue:
            node = queue.pop()
            if node in seen:
                raise _Unknown("docs_controls_ambiguous")
            seen.add(node)
            if len(result) >= MAX_NODES:
                raise _Unknown("docs_controls_unbounded")
            result.append(node)
            # Only structure is read while traversing. Never AXValue or names
            # of unrelated editable fields, static text, document or window.
            queue.extend(self._get(node, "AXChildren") or ())
        return result

    def _named(self, root, roles, label, *, allow_hidden=False, allow_disabled=False):
        found = []
        for node in self._nodes(root):
            if self._get(node, "AXRole") not in roles:
                continue
            if (self._get(node, "AXRole") == "AXGroup"
                    and self._get(node, "AXSubrole") != "AXApplicationDialog"):
                continue  # Braille editor group names are document content, not dialog labels.
            names = [self._get(node, name) for name in ("AXTitle", "AXDescription")]
            if label in names:
                # A conflicting nonempty name is not a positive exact match.
                if any(name and name != label for name in names):
                    raise _Unknown("docs_control_name_ambiguous")
                found.append(node)
        if len(found) > 1:
            raise _Unknown("docs_controls_ambiguous")
        if not found:
            raise _Unknown("docs_control_unverified")
        self._safe(found[0], allow_hidden=allow_hidden, allow_disabled=allow_disabled)
        return found[0]

    def _wait_named(self, target, root, roles, label, scopes):
        deadline = time.monotonic() + UI_TIMEOUT
        while True:
            try:
                control = self._named(root, roles, label, allow_hidden=True)
                owned_scopes = tuple(scopes) + (control,)
                if roles == ("AXMenuItem",):
                    menus = tuple(node for node in self._ancestry(control, allow_hidden=True)
                                  if self._get(node, "AXRole") == "AXMenu")
                    if len(menus) != 1:
                        raise _Unknown("docs_menu_unverified")
                    owned_scopes = tuple(scopes) + menus
                # Validate ownership/security even while native UI visibility
                # catches up; only the visibility check is retryable.
                self._session(target, owned_scopes, allow_hidden=True)
                ancestors = self._ancestry(control, allow_hidden=True)
                for node in ancestors:
                    self._pid(node, target.pid)
                if not any(node == target.web for node in ancestors):
                    raise _Unknown("docs_control_outside_document")
                self._ancestry(control)
                self._session(target, owned_scopes)
                return control
            except _Hidden:
                if time.monotonic() >= deadline:
                    raise
            except _Unknown as exc:
                if str(exc) != "docs_control_unverified" or time.monotonic() >= deadline:
                    raise
                self._session(target, scopes)
            time.sleep(0.03)

    def _press(self, target, control, scopes, *, unchecked=False):
        self._session(target, scopes)
        self._pid(control, target.pid)
        ancestors = self._ancestry(control)
        if not any(node == target.web for node in ancestors):
            raise _Unknown("docs_control_outside_document")
        error, actions = self.bridge.ax.AXUIElementCopyActionNames(control, None)
        if error or "AXPress" not in (actions or ()):
            raise _Unknown("docs_native_action_unavailable")
        # Revalidate after reading action metadata, directly before mutation.
        self._session(target, scopes)
        self._safe(control)
        self._pid(control, target.pid)
        if not any(node == target.web for node in self._ancestry(control)):
            raise _Unknown("docs_control_outside_document")
        if unchecked and self._checked(target, scopes[0], control):
            raise _Unknown("docs_checkbox_changed")
        if self.bridge.ax.AXUIElementPerformAction(control, "AXPress"):
            raise _Unknown("docs_native_action_failed")

    def _open_settings(self, target):
        deadline = time.monotonic() + UI_TIMEOUT
        while True:
            self._session(target)
            candidates = []
            for label in ("Tools", "Ferramentas"):
                try:
                    candidates.append((label, self._named(target.web, ("AXMenuBarItem", "AXMenuItem"), label)))
                except _Unknown as exc:
                    if str(exc) != "docs_control_unverified":
                        raise
            if len(candidates) == 1:
                break
            if len(candidates) > 1 or time.monotonic() >= deadline:
                raise _Unknown("docs_control_unverified")
            time.sleep(0.03)
        language, tools = candidates[0]
        if (language == "Ferramentas"
                and self._get(tools, "AXDOMIdentifier") != "docs-tools-menu"):
            raise _Unknown("docs_control_unverified")
        portuguese = language == "Ferramentas"
        self._press(target, tools, ())
        item = self._wait_named(target, target.web, ("AXMenuItem",),
                                "Acessibilidade c" if portuguese else "Accessibility settings",
                                (tools,))
        menus = [node for node in self._ancestry(item)
                 if self._get(node, "AXRole") == "AXMenu"]
        if len(menus) != 1 or not any(node == target.web for node in self._ancestry(menus[0])):
            raise _Unknown("docs_menu_unverified")
        menu = menus[0]
        self._session(target, (tools, menu))
        self._press(target, item, (tools, menu))
        dialog = self._wait_named(target, target.web, ("AXDialog", "AXGroup"),
                                  "Configurações de acessibilidade" if portuguese else "Accessibility settings",
                                  (menu, tools))
        if (self._get(dialog, "AXRole") == "AXGroup"
                and self._get(dialog, "AXSubrole") != "AXApplicationDialog"):
            raise _Unknown("docs_dialog_unverified")
        # Native AXPress can leave focus on this exact Tools trigger. Only
        # adopt it after our own menu action exposed the verified owned dialog.
        session = self._session(target, (dialog, tools))
        target.tools = tools
        if session[5] == tools:
            target.focus = tools
        self._session(target, (dialog,))
        checkbox = self._named(dialog, ("AXCheckBox",),
                               "Ativar a compatibilidade com o leitor de tela" if portuguese
                               else "Turn on screen reader support")
        braille = self._named(dialog, ("AXCheckBox",),
                              "Ativar suporte a braille" if portuguese
                              else "Turn on braille support", allow_disabled=True)
        default = self._named(dialog, ("AXButton",), "OK")
        declared = self._get(dialog, "AXDefaultButton")
        if declared is not None and declared != default:
            raise _Unknown("docs_commit_control_unverified")
        return dialog, checkbox, braille, default

    def _checked(self, target, dialog, checkbox):
        self._session(target, (dialog,))
        self._pid(checkbox, target.pid)
        self._safe(checkbox)
        enabled = self._get(checkbox, "AXEnabled")
        if enabled is not True and not (type(enabled) is int and enabled == 1):
            raise _Unknown("native_control_unavailable")
        if not any(node == dialog for node in self._ancestry(checkbox)):
            raise _Unknown("docs_checkbox_detached")
        value = self._get(checkbox, "AXValue")
        # AX checkbox mixed state is 2. Strings/floats/coercible unknowns are
        # deliberately not interpreted as a state or permission to toggle.
        if not isinstance(value, (bool, int)) or value not in (0, 1):
            raise _Unknown("docs_checkbox_state_unknown")
        return bool(value)

    def _enable_checkbox(self, target, dialog, checkbox):
        if self._checked(target, dialog, checkbox):
            return False
        self._press(target, checkbox, (dialog,), unchecked=True)
        deadline = time.monotonic() + UI_TIMEOUT
        while not self._checked(target, dialog, checkbox):
            if time.monotonic() >= deadline:
                raise _Unknown("docs_checkbox_readback_failed")
            time.sleep(0.03)
        return True

    def _wait_braille(self, target, dialog, braille):
        deadline = time.monotonic() + UI_TIMEOUT
        while True:
            self._session(target, (dialog,))
            self._pid(braille, target.pid)
            self._safe(braille, allow_disabled=True)
            if not any(node == dialog for node in self._ancestry(
                    self._get(braille, "AXParent"))):
                raise _Unknown("docs_checkbox_detached")
            enabled = self._get(braille, "AXEnabled")
            if enabled is True or type(enabled) is int and enabled == 1:
                return
            if enabled is not False and not (type(enabled) is int and enabled == 0):
                raise _Unknown("native_control_unavailable")
            if time.monotonic() >= deadline:
                raise _Unknown("native_control_unavailable")
            time.sleep(0.03)

    def enable(self, token):
        with self.bridge._lock:
            target, self._target = self._target, None
            self._geometry_scope = None
            self._geometry_handles = None
            try:
                if (target is None or not isinstance(token, str)
                        or not secrets.compare_digest(token, target.token)
                        or time.monotonic() - target.created >= TOKEN_SECONDS):
                    raise _Unknown("docs_token_invalid")
                self._session(target)
                if target.app_id.casefold() == "at.studio.asidebrowser":
                    # This provider's live AXPress/readback did not agree with
                    # persisted Docs preferences. Metadata hints remain safe;
                    # do not mutate account preferences through that provider.
                    raise _Unknown("docs_native_actions_unverified")
                dialog, checkbox, braille, default = self._open_settings(target)
                changed_reader = self._enable_checkbox(target, dialog, checkbox)
                # Only our confirmed reader transition permits waiting for the
                # dependent checkbox to become enabled; never retry actions.
                if changed_reader:
                    self._wait_braille(target, dialog, braille)
                changed_braille = self._enable_checkbox(target, dialog, braille)
                self._press(target, default, (dialog,))
                # Verify both committed preferences even for an already-enabled
                # dialog. Native OK can rebuild the editor or retain Tools.
                deadline = time.monotonic() + UI_TIMEOUT
                while True:
                    try:
                        self._session(target, committed=True)
                        break
                    except _Unknown as exc:
                        if str(exc) != "docs_focus_changed" or time.monotonic() >= deadline:
                            raise
                    time.sleep(0.03)
                dialog, checkbox, braille, default = self._open_settings(target)
                if (not self._checked(target, dialog, checkbox)
                        or not self._checked(target, dialog, braille)):
                    raise _Unknown("docs_committed_readback_failed")
                self._press(target, default, (dialog,))
                status = "enabled" if changed_reader or changed_braille else "already_enabled"
                self.diagnostic = "docs_checkbox_verified_checked"
                return {"status": status, "diagnostic": self.diagnostic, "help_url": HELP_URL}
            except _Unknown as exc:
                self.diagnostic = str(exc)
            except Exception:
                # Never log native exceptions: they can carry user metadata.
                self.diagnostic = "docs_native_controls_unavailable"
            return {"status": "unknown", "diagnostic": self.diagnostic, "help_url": HELP_URL}
