use super::*;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
};
use zbus::zvariant::OwnedObjectPath;
fn object(path: &str) -> Object {
    Object {
        bus: ":1.123".into(),
        path: OwnedObjectPath::try_from(format!("/fixture/{path}")).unwrap(),
    }
}
fn metadata(role: u32, states: &[u32]) -> Metadata {
    let mut bits = vec![0u32; 2];
    for s in states {
        bits[(s / 32) as usize] |= 1 << (s % 32);
    }
    Metadata {
        role,
        states: bits,
        attrs: Attributes::new(),
    }
}
struct Fixture {
    nodes: RefCell<BTreeMap<Object, Metadata>>,
    read: RefCell<TextRead>,
    reads: Cell<usize>,
    static_reads: Cell<usize>,
    insertions: Cell<usize>,
    caret_sets: Cell<usize>,
    fail_insert: Cell<bool>,
    reject_insert: Cell<bool>,
    fail_caret: Cell<bool>,
    active: Cell<bool>,
    geometry_reads: Cell<usize>,
    identity_reads: Cell<usize>,
}
impl Fixture {
    fn new() -> Self {
        let visible = [VISIBLE, SHOWING];
        Self {
            nodes: RefCell::new(BTreeMap::from([
                (object("app"), metadata(75, &[])),
                (object("window"), metadata(23, &[ACTIVE, VISIBLE, SHOWING])),
                (
                    object("field"),
                    metadata(
                        ENTRY,
                        &[FOCUSED, EDITABLE, ENABLED, SENSITIVE, VISIBLE, SHOWING],
                    ),
                ),
                (object("label"), metadata(LABEL, &visible)),
                (object("password"), metadata(PASSWORD, &visible)),
            ])),
            read: RefCell::new(TextRead {
                count: 3,
                caret: 3,
                selections: vec![],
                text: "a😀é".into(),
            }),
            reads: Cell::new(0),
            static_reads: Cell::new(0),
            insertions: Cell::new(0),
            caret_sets: Cell::new(0),
            fail_insert: Cell::new(false),
            reject_insert: Cell::new(false),
            fail_caret: Cell::new(false),
            active: Cell::new(true),
            geometry_reads: Cell::new(0),
            identity_reads: Cell::new(0),
        }
    }
    fn focus(&self) -> Focus {
        focused(self, &policy(), false).unwrap().unwrap()
    }
    fn snapshot(&self) -> Snapshot {
        snapshot_for(self, &self.focus(), false, false)
            .unwrap()
            .unwrap()
    }
}
fn policy() -> ObservationPolicy {
    ObservationPolicy::new(&Config::default()).unwrap()
}
impl Accessibility for Fixture {
    fn root(&self) -> Object {
        object("root")
    }
    fn children(&self, o: &Object, limit: usize) -> Result<Vec<Object>> {
        let children = match o.path.as_str() {
            "/fixture/root" => vec![object("app")],
            "/fixture/app" => vec![object("window")],
            "/fixture/window" => vec![object("field"), object("label"), object("password")],
            _ => vec![],
        };
        Ok(children.into_iter().take(limit).collect())
    }
    fn metadata(&self, o: &Object) -> Result<Metadata> {
        let mut m = self
            .nodes
            .borrow()
            .get(o)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown object"))?;
        if o == &object("window") && !self.active.get() {
            m.states[0] &= !(1 << ACTIVE);
        }
        Ok(m)
    }
    fn parent(&self, o: &Object) -> Result<Option<Object>> {
        Ok(match o.path.as_str() {
            "/fixture/field" | "/fixture/label" => Some(object("window")),
            "/fixture/window" => Some(object("app")),
            _ => None,
        })
    }
    fn app_identity(&self, _: &Object) -> Result<Option<(i32, String, String)>> {
        self.identity_reads.set(self.identity_reads.get() + 1);
        Ok(Some((
            std::process::id() as i32 + 1,
            "Editor".into(),
            "/fixture/editor".into(),
        )))
    }
    fn text(&self, _: &Object, reject_selection: bool) -> Result<Option<TextRead>> {
        self.reads.set(self.reads.get() + 1);
        let read = self.read.borrow().clone();
        Ok((!reject_selection || read.selections.is_empty()).then_some(read))
    }
    fn static_text(&self, o: &Object, max: usize) -> Result<String> {
        self.static_reads.set(self.static_reads.get() + 1);
        assert_eq!(o, &object("label"));
        Ok("Static public heading".chars().take(max).collect())
    }
    fn extents(&self, _: &Object) -> Result<Rect> {
        self.geometry_reads.set(self.geometry_reads.get() + 1);
        Ok([0.0, 0.0, 200.0, 100.0])
    }
    fn character_rect(&self, _: &Object, index: usize) -> Result<Rect> {
        self.geometry_reads.set(self.geometry_reads.get() + 1);
        Ok([index as f64 * 8.0, 10.0, 8.0, 20.0])
    }
    fn attribute_run(&self, _: &Object, _: usize) -> Result<(Attributes, i32, i32)> {
        self.geometry_reads.set(self.geometry_reads.get() + 1);
        bail!("No font run advertised")
    }
    fn insert(&self, _: &Object, caret: i32, text: &str, bytes: i32) -> Result<bool> {
        self.insertions.set(self.insertions.get() + 1);
        if self.fail_insert.get() {
            bail!("Provider disconnected after sending mutation");
        }
        if self.reject_insert.get() {
            return Ok(false);
        }
        assert_eq!(bytes as usize, text.len());
        let mut read = self.read.borrow_mut();
        let index = crate::core::TextIndex::new(&read.text);
        let byte = index
            .scalar_to_utf8(crate::core::ScalarIndex(caret as usize))
            .unwrap()
            .0;
        read.text.insert_str(byte, text);
        read.count = read.text.chars().count() as i32;
        Ok(true)
    }
    fn set_caret(&self, _: &Object, caret: i32) -> Result<bool> {
        self.caret_sets.set(self.caret_sets.get() + 1);
        if self.fail_caret.get() {
            bail!("Caret setter failed after insertion");
        }
        self.read.borrow_mut().caret = caret;
        Ok(true)
    }
}
#[test]
fn scalar_count_and_reversed_selection_are_checked_without_unit_guessing() {
    let api = Fixture::new();
    let snapshot = api.snapshot();
    assert_eq!(snapshot.caret, 3);
    assert_eq!(snapshot.text.as_ref(), "a😀é");
    api.read.borrow_mut().selections = vec![(3, 1)];
    let selected = api.snapshot();
    assert_eq!((selected.caret, selected.selection_length), (1, 2));
    assert_eq!(selected.selected_text(), "😀é");
    assert!(
        snapshot_for(&api, &api.focus(), false, true)
            .unwrap()
            .is_none()
    );
    api.read.borrow_mut().count = 4;
    assert!(
        snapshot_for(&api, &api.focus(), false, false)
            .unwrap()
            .is_none()
    );
    api.read.borrow_mut().count = 3;
    api.read.borrow_mut().selections = vec![(0, 4)];
    assert!(
        snapshot_for(&api, &api.focus(), false, false)
            .unwrap()
            .is_none()
    );
}
#[test]
fn privacy_roles_attributes_and_composition_precede_every_text_read() {
    for (role, attribute) in [
        (PASSWORD, None),
        (23, None),
        (ENTRY, Some(("Protected", "yes"))),
        (ENTRY, Some(("type", "password"))),
        (ENTRY, Some(("COMPOSING", "true"))),
    ] {
        let api = Fixture::new();
        let mut focus = api.focus();
        focus.metadata.role = role;
        if let Some((key, value)) = attribute {
            focus.metadata.attrs.insert(key.into(), value.into());
        }
        assert!(snapshot_for(&api, &focus, true, false).unwrap().is_none());
        assert_eq!(api.reads.get(), 0);
        assert_eq!(api.geometry_reads.get(), 0);
    }
}
#[test]
fn excluded_application_and_background_window_are_not_traversed_for_text() {
    let api = Fixture::new();
    let mut config = Config::default();
    config.excluded_apps.push("editor".into());
    assert!(
        focused(&api, &ObservationPolicy::new(&config).unwrap(), false)
            .unwrap()
            .is_none()
    );
    assert_eq!(api.reads.get(), 0);
    api.active.set(false);
    assert!(focused(&api, &policy(), false).unwrap().is_none());
    assert_eq!(api.reads.get(), 0);
}
#[test]
fn unknown_ime_state_never_queries_font_or_component_geometry() {
    let api = Fixture::new();
    let focus = api.focus();
    assert!(inline_metrics(&api, &focus, "Hello", 5, Some([40.0, 10.0, 1.0, 20.0])).is_none());
    assert_eq!(api.geometry_reads.get(), 0);
    let mut focus = focus;
    focus
        .metadata
        .attrs
        .insert("preedit".into(), "false".into());
    assert!(inline_metrics(&api, &focus, "HelloX", 5, Some([40.0, 10.0, 1.0, 20.0])).is_none());
    assert_eq!(api.geometry_reads.get(), 0);
}
#[test]
fn window_identity_check_does_not_read_text_and_rejects_hidden_or_rebound_window() {
    let api = Fixture::new();
    let mut snapshot = api.snapshot();
    let reads = api.reads.get();
    assert!(identity_matches(&api.focus(), &snapshot));
    assert_eq!(api.reads.get(), reads);
    snapshot.window_id.push('x');
    assert!(!identity_matches(&api.focus(), &snapshot));
    snapshot = api.snapshot();
    let mut focus = api.focus();
    focus.metadata.states[0] &= !(1 << VISIBLE);
    assert!(!identity_matches(&focus, &snapshot));
}
#[test]
fn static_context_skips_editable_and_password_values_and_obeys_bounds() {
    let api = Fixture::new();
    let snapshot = api.snapshot();
    let reads = api.reads.get();
    let context = read_context(&api, &policy(), &snapshot, &|| true, 6).unwrap();
    assert_eq!(context.text, "Static");
    assert_eq!(context.status, "ready");
    assert_eq!(api.reads.get(), reads);
    assert_eq!(api.static_reads.get(), 1);
}
#[test]
fn revoked_context_authorization_discards_already_collected_labels() {
    let api = Fixture::new();
    let snapshot = api.snapshot();
    let checks = Cell::new(0);
    let result = read_context(
        &api,
        &policy(),
        &snapshot,
        &|| {
            checks.set(checks.get() + 1);
            api.static_reads.get() == 0
        },
        12000,
    )
    .unwrap();
    assert!(checks.get() > 1);
    assert_eq!(result.status, "blocked");
    assert!(result.text.is_empty());
}
#[test]
fn insertion_uses_fresh_scalar_caret_and_utf8_byte_length_then_never_retries_caret_failure() {
    let api = Fixture::new();
    let snapshot = api.snapshot();
    api.fail_caret.set(true);
    assert!(matches!(
        insert_text(&api, &policy(), &snapshot, "😀é").unwrap(),
        EditOutcome::Dispatched
    ));
    assert_eq!(api.read.borrow().text, "a😀é😀é");
    assert_eq!(api.insertions.get(), 1);
    assert_eq!(api.caret_sets.get(), 1);
    assert!(matches!(
        insert_text(&api, &policy(), &snapshot, "😀é").unwrap(),
        EditOutcome::Rejected
    ));
    assert_eq!(api.insertions.get(), 1);
}
#[test]
fn stale_selection_protected_and_uncertain_mutations_keep_distinct_outcomes() {
    let api = Fixture::new();
    let mut snapshot = api.snapshot();
    snapshot.caret = 2;
    assert!(matches!(
        insert_text(&api, &policy(), &snapshot, "x").unwrap(),
        EditOutcome::Rejected
    ));
    assert_eq!(api.insertions.get(), 0);
    snapshot = api.snapshot();
    snapshot.selection_length = 1;
    assert!(matches!(
        insert_text(&api, &policy(), &snapshot, "x").unwrap(),
        EditOutcome::Rejected
    ));
    snapshot = api.snapshot();
    api.fail_insert.set(true);
    assert!(matches!(
        insert_text(&api, &policy(), &snapshot, "x").unwrap(),
        EditOutcome::Indeterminate
    ));
    assert_eq!(api.insertions.get(), 1);
    assert_eq!(api.caret_sets.get(), 0);
    api.fail_insert.set(false);
    api.reject_insert.set(true);
    assert!(matches!(
        insert_text(&api, &policy(), &snapshot, "x").unwrap(),
        EditOutcome::Rejected
    ));
}
#[test]
fn fresh_text_revision_rebuilds_scalar_boundaries() {
    let api = Fixture::new();
    let first = api.snapshot();
    assert_eq!(
        first
            .text
            .index()
            .scalar_to_utf8(crate::core::ScalarIndex(2))
            .unwrap()
            .0,
        5
    );
    *api.read.borrow_mut() = TextRead {
        count: 3,
        caret: 3,
        selections: vec![],
        text: "éab".into(),
    };
    let next = api.snapshot();
    assert_eq!(
        next.text
            .index()
            .scalar_to_utf8(crate::core::ScalarIndex(2))
            .unwrap()
            .0,
        3
    );
    assert!(!first.same_key(&next));
}
#[test]
fn clipping_rejects_disjoint_and_nonfinite_native_rectangles() {
    assert_eq!(
        intersect([0.0, 0.0, 20.0, 20.0], [10.0, 5.0, 20.0, 20.0]),
        Some([10.0, 5.0, 10.0, 15.0])
    );
    assert_eq!(
        intersect([0.0, 0.0, 20.0, 20.0], [20.0, 0.0, 10.0, 10.0]),
        None
    );
    assert_eq!(
        intersect([f64::NAN, 0.0, 20.0, 20.0], [0.0, 0.0, 10.0, 10.0]),
        None
    );
}
#[test]
fn host_notifications_do_not_advance_user_revision() {
    let input = InputState::default();
    input.note(true);
    assert_eq!(input.revision.load(Ordering::Acquire), 1);
    assert_eq!(input.user_revision.load(Ordering::Acquire), 1);
    let timestamp = *input.time.lock().unwrap();
    input.note(false);
    assert_eq!(input.revision.load(Ordering::Acquire), 2);
    assert_eq!(input.user_revision.load(Ordering::Acquire), 1);
    assert_eq!(*input.time.lock().unwrap(), timestamp);
}
#[test]
fn atomic_replacement_gate_refuses_selection_suffix_and_out_of_bounds_without_edits() {
    let api = Fixture::new();
    let mut snapshot = api.snapshot();
    assert!(insertion_range(&snapshot, 3, 0));
    for (start, length) in [(0, 1), (1, 2), (2, 0), (3, 1), (usize::MAX, 1)] {
        assert!(!insertion_range(&snapshot, start, length));
    }
    snapshot.selection_length = 1;
    assert!(!insertion_range(&snapshot, 3, 0));
    snapshot.selection_length = 0;
    snapshot.caret = 4;
    assert!(!insertion_range(&snapshot, 4, 0));
    assert_eq!(api.insertions.get(), 0);
}
#[test]
fn detached_context_read_refuses_revoked_authorization_before_connecting_to_native_bus() {
    let api = Fixture::new();
    let ticket = LinuxContextRead {
        snapshot: api.snapshot(),
        policy: policy(),
    };
    // This path is valid without DISPLAY or a session bus: no native handle is created.
    let result = ticket.read(&|| false, 2400).unwrap();
    assert_eq!(result.status, "blocked");
    assert!(result.text.is_empty());
    assert_eq!(result.source, "accessibility");
}
#[test]
fn detached_context_snapshot_binding_rejects_text_field_and_window_changes() {
    let api = Fixture::new();
    let snapshot = api.snapshot();
    let ticket: Box<dyn DetachedContextRead> = Box::new(LinuxContextRead {
        snapshot: snapshot.clone(),
        policy: policy(),
    });
    assert!(ticket.matches_snapshot(&snapshot));
    let mut changed = snapshot.clone();
    changed.text = "changed".into();
    assert!(!ticket.matches_snapshot(&changed));
    changed = snapshot.clone();
    changed.field_id.push('x');
    assert!(!ticket.matches_snapshot(&changed));
    changed = snapshot.clone();
    changed.window_id.push('x');
    assert!(!ticket.matches_snapshot(&changed));
    changed = snapshot.clone();
    changed.window_revision.push('x');
    assert!(!ticket.matches_snapshot(&changed));
    // The worker receives only the ticket's immutable data, never the bridge's D-Bus/X11 handles.
    assert!(
        std::thread::spawn(move || ticket.read(&|| false, 2400).unwrap().status)
            .join()
            .unwrap()
            == "blocked"
    );
}
