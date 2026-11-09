use super::*;

fn peer() -> Peer {
    Peer {
        sender: ":1.42".into(),
        pid: 4242,
        uid: 1000,
        executable: "/usr/bin/native-writer".into(),
        app: "native-writer".into(),
    }
}
fn metadata() -> Dict {
    let mut d = Dict::new();
    put(&mut d, "protocol", 1u32);
    put(&mut d, "pid", 4242u32);
    text(&mut d, "backend", "qt6-fcitx-client/1");
    text(&mut d, "app_id", "/usr/bin/native-writer");
    put(&mut d, "focus", true);
    put(&mut d, "protected", false);
    put(&mut d, "editable", true);
    text(&mut d, "field_token", "field1");
    text(&mut d, "window_token", "window1");
    text(&mut d, "document_token", "block1");
    put(&mut d, "focus_generation", 1u64);
    put(&mut d, "pending_edit", false);
    text(&mut d, "purpose", "text");
    text(&mut d, "domain", "");
    d
}
fn witness() -> Witness {
    Witness {
        field: "field1".into(),
        window: "window1".into(),
        document: "block1".into(),
        revision: 10,
        user: 5,
        notification: 12,
        unit: "utf16".into(),
        text: "A😀bc".into(),
        caret: 3,
        anchor: 3,
        purpose: "text".into(),
        domain: String::new(),
    }
}
fn snapshot_wire(w: &Witness) -> Dict {
    let mut d = Dict::new();
    w.write_offer(&mut d);
    text(&mut d, "text", &w.text);
    put(&mut d, "notification_revision", w.notification);
    put(&mut d, "focus", true);
    put(&mut d, "protected", false);
    put(&mut d, "editable", true);
    text(&mut d, "purpose", &w.purpose);
    text(&mut d, "composing", "idle");
    text(&mut d, "domain", &w.domain);
    d
}
fn frozen() -> Frozen {
    let before = witness();
    let snapshot = before.snapshot(1, "native-writer", 4242).unwrap();
    let presentation = NativeHostPresentation::Preedit(NativePreedit {
        anchor: "client:1:offer:7:cap:9".into(),
        text: "xy".into(),
        revision: 11,
        expires_at: Instant::now() + Duration::from_secs(1),
    });
    Frozen {
        target: Target {
            id: 1,
            peer: peer(),
            path: "/org/brotypist/Client1/p4242".into(),
        },
        before,
        snapshot,
        presentation: Some(presentation),
        offer_id: 7,
        capability: 9,
        issued: 100,
        expires: 1000,
        epoch: 3,
        mode: "completion".into(),
        start: 2,
        length: 0,
        shown_text: "xy".into(),
        shown_values: Vec::new(),
        word: "x ".into(),
        all: "xy ".into(),
        armed_at: Some(110),
        revoked_at: None,
    }
}
fn accepted() -> Dict {
    let before = witness();
    let mut after = before.clone();
    after.text = "A😀xy bc".into();
    after.caret = 6;
    after.anchor = 6;
    after.revision += 1;
    after.notification += 1;
    let mut e = Dict::new();
    text(&mut e, "kind", "accepted");
    text(&mut e, "action", "all");
    text(&mut e, "outcome", "applied");
    text(&mut e, "text", "xy ");
    put(&mut e, "offer_id", 7u64);
    put(&mut e, "capability_id", 9u64);
    put(&mut e, "acceptance_id", 1u64);
    put(&mut e, "accepted_ns", 200u64);
    put(&mut e, "start", 3u32);
    put(&mut e, "length", 0u32);
    put(&mut e, "before", snapshot_wire(&before));
    put(&mut e, "after", snapshot_wire(&after));
    e
}

#[test]
fn offsets_roundtrip_without_splitting_surrogates_or_combining_scalars() {
    let text = "a😀e\u{301}🇧🇷";
    for scalar_index in 0..=text.chars().count() {
        let utf16 = native(text, "utf16", scalar_index).unwrap();
        assert_eq!(scalar(text, "utf16", utf16).unwrap(), scalar_index);
        assert_eq!(native(text, "scalar", scalar_index).unwrap(), scalar_index);
    }
    assert!(scalar(text, "utf16", 2).is_err());
    assert!(scalar(text, "utf16", 8).is_err());
    assert!(scalar(text, "scalar", 7).is_err());
    assert!(native(text, "utf16", usize::MAX).is_err());
    assert!(scalar(text, "bytes", 0).is_err());
}
#[test]
fn bounded_native_qt_blocks_are_separate_document_authorities() {
    let first = witness();
    let mut next = first.clone();
    next.document = "block2".into();
    let a = first.snapshot(1, "native-writer", 4242).unwrap();
    let b = next.snapshot(1, "native-writer", 4242).unwrap();
    assert!(!a.same_authority(&b));
    assert!(!a.same_key(&b));
    let mut collision_a = first.clone();
    collision_a.field = "a:b".into();
    collision_a.document = "c".into();
    let mut collision_b = first;
    collision_b.field = "a".into();
    collision_b.document = "b:c".into();
    assert_ne!(
        collision_a.snapshot(1, "writer", 4242).unwrap().field_id,
        collision_b.snapshot(1, "writer", 4242).unwrap().field_id
    );
}
#[test]
fn capture_fails_closed_for_missing_privacy_composition_and_bad_tokens() {
    for key in [
        "protected",
        "editable",
        "focus",
        "composing",
        "purpose",
        "field_token",
        "window_token",
        "document_token",
    ] {
        let mut d = snapshot_wire(&witness());
        d.remove(key);
        assert!(Witness::parse(&d).is_err(), "{key}");
    }
    for composition in ["active", "unknown", ""] {
        let mut d = snapshot_wire(&witness());
        text(&mut d, "composing", composition);
        assert!(Witness::parse(&d).is_err());
    }
    let mut d = snapshot_wire(&witness());
    put(&mut d, "protected", true);
    assert!(Witness::parse(&d).is_err());
    let mut d = snapshot_wire(&witness());
    put(&mut d, "caret", 2u32);
    assert!(Witness::parse(&d).is_err());
    let mut d = snapshot_wire(&witness());
    text(&mut d, "document_token", "window title secret");
    assert!(Witness::parse(&d).is_err());
}
#[test]
fn register_checks_real_executable_pid_and_protocol_not_arbitrary_claims() {
    assert!(server::metadata_valid(&metadata(), &peer()).is_ok());
    for (key, value) in [("app_id", "/usr/bin/other"), ("backend", "unreviewed/1")] {
        let mut m = metadata();
        text(&mut m, key, value);
        assert!(server::metadata_valid(&m, &peer()).is_err());
    }
    let mut m = metadata();
    put(&mut m, "pid", 1u32);
    assert!(server::metadata_valid(&m, &peer()).is_err());
    let mut m = metadata();
    put(&mut m, "protocol", 2u32);
    assert!(server::metadata_valid(&m, &peer()).is_err());
    assert!(
        Peer::process(":1.8".into(), std::process::id(), unsafe {
            libc::geteuid()
        })
        .is_err()
    );
}
#[test]
fn consent_uses_existing_global_app_exclusion_and_unknown_browser_domain_rules() {
    let mut c = Config::default();
    let p = peer();
    let m = metadata();
    assert!(permitted(&c, &p, &m));
    c.global_enabled = false;
    assert!(!permitted(&c, &p, &m));
    c.global_enabled = true;
    c.excluded_apps.push("native-writer".into());
    assert!(!permitted(&c, &p, &m));
    c.excluded_apps.clear();
    c.app_rules.insert(
        "app:native-writer".into(),
        [("enabled".into(), serde_json::json!(false))].into(),
    );
    assert!(!permitted(&c, &p, &m));
    c.app_rules.clear();
    c.app_rules.insert(
        "domain:private.example".into(),
        [("enabled".into(), serde_json::json!(false))].into(),
    );
    let mut browser = p;
    browser.app = "firefox".into();
    browser.executable = "/usr/bin/firefox".into();
    assert!(!permitted(&c, &browser, &m));
    let mut safe = m;
    text(&mut safe, "domain", "public.example");
    assert!(permitted(&c, &browser, &safe));
    text(&mut safe, "domain", "private.example");
    assert!(!permitted(&c, &browser, &safe));
}
#[test]
fn same_epoch_policy_renewal_changes_only_expiry_not_features_or_bindings() {
    let c = Config::default();
    let m = metadata();
    let p = peer();
    let mut first = policy(&c, 4, &p, &m, 100).unwrap();
    let mut renewal = policy(&c, 4, &p, &m, 500).unwrap();
    assert!(number(&renewal, "expires_ns").unwrap() > number(&first, "expires_ns").unwrap());
    first.remove("expires_ns");
    renewal.remove("expires_ns");
    assert_eq!(first, renewal);
    let mut disabled = c;
    disabled.global_enabled = false;
    assert!(!flag(&policy(&disabled, 5, &p, &m, 600).unwrap(), "enabled").unwrap());
}
#[test]
fn capture_reuses_live_policy_but_renews_before_expiry_or_changed_authority() {
    let config = Config::default();
    let meta = metadata();
    let owner = FocusOwner {
        client: 1,
        scope: FocusScope::parse(&meta).unwrap(),
    };
    let issued = 100;
    let mut first = policy(&config, 4, &peer(), &meta, issued).unwrap();
    bind_focus_policy(&mut first, 1, Some(&owner.scope), Some(&owner));
    let mut next = policy(&config, 4, &peer(), &meta, issued + 1).unwrap();
    bind_focus_policy(&mut next, 1, Some(&owner.scope), Some(&owner));
    assert!(policy_update_required(None, &next, issued + 1));
    assert!(!policy_update_required(Some(&first), &next, issued + 1));
    assert!(!policy_update_required(
        Some(&first),
        &next,
        issued + POLICY_LEASE / 2 - 1
    ));
    assert!(policy_update_required(
        Some(&first),
        &next,
        issued + POLICY_LEASE / 2
    ));
    assert!(policy_update_required(
        Some(&first),
        &next,
        issued + POLICY_LEASE
    ));
    assert!(policy_update_required(Some(&first), &next, 0));
    for (key, value) in [
        ("field_token", "other-field"),
        ("window_token", "other-window"),
        ("document_token", "other-document"),
    ] {
        let mut changed = clone_dict(&next).unwrap();
        text(&mut changed, key, value);
        assert!(policy_update_required(Some(&first), &changed, issued + 1));
    }
    for key in ["focus_generation", "policy_epoch"] {
        let mut changed = clone_dict(&next).unwrap();
        put(&mut changed, key, number(&next, key).unwrap() + 1);
        assert!(policy_update_required(Some(&first), &changed, issued + 1));
    }
    let mut disabled = clone_dict(&next).unwrap();
    put(&mut disabled, "enabled", false);
    assert!(policy_update_required(Some(&first), &disabled, issued + 1));
    for feature in [
        "predictions",
        "emoji",
        "synonyms",
        "alternatives",
        "correction",
    ] {
        let mut changed = clone_dict(&next).unwrap();
        put(&mut changed, feature, !flag(&next, feature).unwrap());
        assert!(policy_update_required(Some(&first), &changed, issued + 1));
    }
    let mut changed = clone_dict(&next).unwrap();
    put(&mut changed, "shortcuts", HashMap::<String, String>::new());
    assert!(policy_update_required(Some(&first), &changed, issued + 1));
    let mut withdrawn = clone_dict(&first).unwrap();
    inert_policy(&mut withdrawn);
    assert!(policy_update_required(Some(&first), &withdrawn, issued + 1));
    assert!(!policy_update_required(
        Some(&withdrawn),
        &withdrawn,
        issued + 1
    ));
    assert!(policy_update_required(Some(&withdrawn), &first, issued + 1));
}
#[test]
fn metadata_only_privacy_readiness_does_not_require_a_capture_notification() {
    let mut meta = metadata();
    text(&mut meta, "composing", "idle");
    assert_eq!(metadata_security(&meta), Some(false));
    assert!(capture_permitted(&Config::default(), &peer(), &meta));
    let mut disabled = Config::default();
    disabled.global_enabled = false;
    assert!(!capture_permitted(&disabled, &peer(), &meta));
    assert!(action_allowed(&disabled, &peer(), &meta, &Action::Toggle));
    assert!(action_allowed(
        &disabled,
        &peer(),
        &meta,
        &Action::AppToggle
    ));
    for composition in ["active", "unknown", ""] {
        text(&mut meta, "composing", composition);
        assert_eq!(metadata_security(&meta), Some(false));
        assert!(!capture_permitted(&Config::default(), &peer(), &meta));
    }
    meta.remove("composing");
    assert!(!capture_permitted(&Config::default(), &peer(), &meta));
    put(&mut meta, "protected", true);
    assert_eq!(metadata_security(&meta), Some(true));
    put(&mut meta, "protected", false);
    put(&mut meta, "focus", false);
    assert_eq!(metadata_security(&meta), None);
    put(&mut meta, "focus", true);
    meta.remove("protected");
    assert_eq!(metadata_security(&meta), None);
}
#[test]
fn startup_privacy_readiness_requires_known_purpose_and_exact_focus_scope_metadata() {
    let meta = metadata();
    for key in [
        "focus",
        "protected",
        "purpose",
        "field_token",
        "window_token",
        "document_token",
        "focus_generation",
    ] {
        let mut missing = clone_dict(&meta).unwrap();
        missing.remove(key);
        assert_eq!(metadata_security(&missing), None, "{key}");
    }
    for purpose in ["password", "pin", "sensitive", "hidden", "unknown", ""] {
        let mut unknown = clone_dict(&meta).unwrap();
        text(&mut unknown, "purpose", purpose);
        assert_eq!(metadata_security(&unknown), None, "{purpose}");
    }
    for key in ["field_token", "window_token", "document_token"] {
        for value in ["", "not a native token"] {
            let mut invalid = clone_dict(&meta).unwrap();
            text(&mut invalid, key, value);
            assert_eq!(metadata_security(&invalid), None, "{key}");
        }
    }
    let mut invalid = clone_dict(&meta).unwrap();
    text(&mut invalid, "focus_generation", "unknown");
    assert_eq!(metadata_security(&invalid), None);
    let mut protected = clone_dict(&meta).unwrap();
    put(&mut protected, "protected", true);
    assert_eq!(metadata_security(&protected), Some(true));
    assert!(!capture_permitted(&Config::default(), &peer(), &protected));
}
#[test]
fn leases_reject_preissue_expiry_future_and_delayed_events() {
    assert!(lease_valid(150, 100, 200, 250));
    assert!(!lease_valid(99, 100, 200, 250));
    assert!(!lease_valid(200, 100, 200, 250));
    assert!(!lease_valid(150, 100, 200, 149));
    assert!(!lease_valid(150, 100, 200, 150 + LEASE + 1));
}
#[test]
fn exact_core_armed_insertion_is_confirmed_not_the_raw_visible_prefix() {
    let (edit, _) = validate_edit(frozen(), &accepted(), 250).unwrap();
    assert_eq!(edit.text, "xy ");
    assert_eq!(edit.start, 2);
    assert_eq!(edit.length, 0);
    assert_eq!(edit.action, Action::All);
    assert_eq!(&*edit.after.text, "A😀xy bc");
    let mut raw = accepted();
    text(&mut raw, "text", "xy");
    assert!(validate_edit(frozen(), &raw, 250).is_err());
}
#[test]
fn presentation_without_arm_never_authorizes_generated_edit() {
    let mut f = frozen();
    f.armed_at = None;
    assert!(validate_edit(f, &accepted(), 250).is_err());
    let mut f = frozen();
    f.armed_at = Some(201);
    assert!(validate_edit(f, &accepted(), 250).is_err());
}
#[test]
fn arm_timeout_keeps_true_earlier_confirmation_but_rejects_later_revoked_edit() {
    let mut f = frozen();
    f.revoked_at = Some(220);
    assert!(validate_edit(f, &accepted(), 250).is_ok());
    let mut f = frozen();
    f.revoked_at = Some(199);
    assert!(validate_edit(f, &accepted(), 250).is_err());
}
#[test]
fn dedupe_is_per_authenticated_client_and_burns_failed_acceptance_ids() {
    let mut ids = HashMap::new();
    assert!(take_acceptance(&mut ids, 1, 1).is_ok());
    assert!(take_acceptance(&mut ids, 1, 1).is_err());
    assert!(take_acceptance(&mut ids, 1, 0).is_err());
    assert!(take_acceptance(&mut ids, 2, 1).is_ok());
    assert!(take_acceptance(&mut ids, 1, 2).is_ok());
}
#[test]
fn confirmation_checks_frozen_identity_revision_exact_range_and_readback() {
    for key in ["field_token", "window_token", "document_token"] {
        let mut e = accepted();
        let mut b = nested(&e, "before").unwrap();
        text(&mut b, key, "other");
        put(&mut e, "before", b);
        assert!(validate_edit(frozen(), &e, 250).is_err());
    }
    let mut e = accepted();
    let mut b = nested(&e, "before").unwrap();
    put(&mut b, "revision", 11u64);
    put(&mut e, "before", b);
    assert!(validate_edit(frozen(), &e, 250).is_err());
    let mut e = accepted();
    put(&mut e, "start", 2u32);
    assert!(validate_edit(frozen(), &e, 250).is_err());
    let mut e = accepted();
    let mut a = nested(&e, "after").unwrap();
    text(&mut a, "text", "A😀xy bc!");
    put(&mut e, "after", a);
    assert!(validate_edit(frozen(), &e, 250).is_err());
    let mut e = accepted();
    let mut a = nested(&e, "after").unwrap();
    put(&mut a, "user_revision", 6u64);
    put(&mut e, "after", a);
    assert!(validate_edit(frozen(), &e, 250).is_err());
}
#[test]
fn rejected_and_indeterminate_dispatch_are_terminal_not_applied() {
    for outcome in ["rejected", "indeterminate", ""] {
        let mut e = accepted();
        text(&mut e, "outcome", outcome);
        assert!(validate_edit(frozen(), &e, 250).is_err());
    }
    let mut e = accepted();
    put(&mut e, "capability_id", 10u64);
    assert!(validate_edit(frozen(), &e, 250).is_err());
    let mut e = accepted();
    put(&mut e, "accepted_ns", 1000u64);
    assert!(validate_edit(frozen(), &e, 1001).is_err());
}
#[test]
fn choice_uses_dense_visible_ordinal_and_exact_unicode_payload_with_independent_readback() {
    for payload in ["xy ", "  café  café", " 👩🏽‍💻"] {
        let choice = || {
            let mut f = frozen();
            f.mode = "alternatives".into();
            f.shown_text.clear();
            f.shown_values = vec!["first".into(), payload.into()];
            f.word.clear();
            f.all.clear();
            f.presentation = Some(NativeHostPresentation::Choices(NativeChoices {
                anchor: "client:choice".into(),
                values: f.shown_values.clone(),
                revision: 11,
                expires_at: Instant::now() + Duration::from_secs(1),
            }));
            f
        };
        let mut e = accepted();
        text(&mut e, "action", "choice");
        put(&mut e, "choice_index", 1u32);
        text(&mut e, "text", payload);
        let mut after = witness();
        after.text = format!("A😀{payload}bc");
        after.caret = 3 + payload.encode_utf16().count();
        after.anchor = after.caret;
        after.revision += 1;
        after.notification += 1;
        put(&mut e, "after", snapshot_wire(&after));
        let mut wrong_readback = clone_dict(&e).unwrap();
        let mut wrong_after = snapshot_wire(&after);
        text(&mut wrong_after, "text", "A😀firstbc");
        put(&mut wrong_readback, "after", wrong_after);
        assert!(validate_edit(choice(), &wrong_readback, 250).is_err());
        let mut hidden = clone_dict(&e).unwrap();
        put(&mut hidden, "choice_index", 2u32);
        assert!(validate_edit(choice(), &hidden, 250).is_err());
        let (edit, _) = validate_edit(choice(), &e, 250).unwrap();
        assert_eq!(edit.action, Action::Choice(1));
        assert_eq!(edit.text, payload);
        assert_eq!(edit.after.text.as_ref(), after.text);
        assert_eq!(edit.start, 2);
    }
}
#[test]
fn wire_bounds_reject_oversized_text_and_nested_resource_payloads() {
    assert!(!bounded_text(&"x".repeat(16_001)));
    assert!(!bounded_text("a\0b"));
    let mut d = Dict::new();
    text(&mut d, "text", &"x".repeat(MAX_TEXT + 1));
    assert!(!bounded(&d));
    let mut d = Dict::new();
    for n in 0..65 {
        put(&mut d, &format!("key{n}"), true);
    }
    assert!(!bounded(&d));
    let mut d = Dict::new();
    put(&mut d, "float", 1.0f64);
    assert!(!bounded(&d));
}
#[test]
fn authenticated_event_peer_cannot_substitute_sender_uid_pid_or_executable() {
    let p = peer();
    let clients = [(
        1,
        server::Client {
            peer: p.clone(),
            path: "/org/brotypist/Client1/p4242".into(),
            metadata: metadata(),
            policy_epoch: 3,
            policy_expires: 1000,
            sent_policy: None,
        },
    )]
    .into();
    assert!(server::registered_peer(&clients, 1, &p));
    assert!(!server::registered_peer(&clients, 2, &p));
    let mut other = p.clone();
    other.sender = ":1.43".into();
    assert!(!server::registered_peer(&clients, 1, &other));
    let mut other = p.clone();
    other.uid += 1;
    assert!(!server::registered_peer(&clients, 1, &other));
    let mut other = p.clone();
    other.pid += 1;
    assert!(!server::registered_peer(&clients, 1, &other));
    let mut other = p;
    other.executable = "/usr/bin/other".into();
    assert!(!server::registered_peer(&clients, 1, &other));
    let mut content = metadata();
    text(&mut content, "text", "must not be metadata");
    assert!(server::metadata_valid(&content, &peer()).is_err());
}
#[test]
fn fresh_consent_revocation_blocks_recording_but_later_focus_epoch_is_not_consent() {
    let c = Config::default();
    let p = peer();
    let mut m = metadata();
    assert!(confirmation_consent(&c, 3, 3, &p, &m));
    put(&mut m, "focus", false);
    assert!(confirmation_consent(&c, 3, 3, &p, &m));
    assert!(!confirmation_consent(&c, 4, 3, &p, &m));
    put(&mut m, "protected", true);
    assert!(!confirmation_consent(&c, 3, 3, &p, &m));
    put(&mut m, "protected", false);
    let mut disabled = c;
    disabled.global_enabled = false;
    assert!(!confirmation_consent(&disabled, 3, 3, &p, &m));
}
#[test]
fn only_live_policy_toggles_bypass_disabled_capture_permission() {
    let mut c = Config::default();
    c.global_enabled = false;
    let p = peer();
    let mut m = metadata();
    assert!(action_allowed(&c, &p, &m, &Action::Toggle));
    assert!(action_allowed(&c, &p, &m, &Action::AppToggle));
    for action in [
        Action::Force,
        Action::Alternatives,
        Action::Synonyms,
        Action::Settings,
    ] {
        assert!(!action_allowed(&c, &p, &m, &action));
    }
    assert!(policy_live(3, 3, 200, 100));
    assert!(!policy_live(3, 3, 200, 200));
    assert!(!policy_live(4, 3, 200, 100));
    assert!(!policy_live(3, 3, 200, 0));
    c.global_enabled = true;
    assert!(!action_allowed(&c, &p, &m, &Action::Force));
    text(&mut m, "composing", "idle");
    assert!(action_allowed(&c, &p, &m, &Action::Force));
    text(&mut m, "composing", "active");
    assert!(!action_allowed(&c, &p, &m, &Action::Force));
}
#[test]
fn harmless_native_notifications_preserve_receipts_but_real_input_and_focus_revoke() {
    let w = witness();
    let mut event = snapshot_wire(&w);
    put(&mut event, "notification_revision", w.notification + 1);
    assert!(!changed_revokes(&w, &event));
    put(&mut event, "user_revision", w.user + 1);
    assert!(changed_revokes(&w, &event));
    put(&mut event, "user_revision", w.user);
    put(&mut event, "revision", w.revision + 1);
    assert!(changed_revokes(&w, &event));
    put(&mut event, "revision", w.revision);
    put(&mut event, "focus", false);
    assert!(changed_revokes(&w, &event));
    put(&mut event, "focus", true);
    text(&mut event, "document_token", "another-block");
    assert!(changed_revokes(&w, &event));
}
#[test]
fn same_live_policy_notifications_do_not_change_controller_visible_input_revision() {
    let w = witness();
    let mut event = snapshot_wire(&w);
    let mut input = UserInputFence::default();
    let at = Instant::now();
    input.seed(1, w.user);
    let mut revision = 17;
    for notification in w.notification + 1..w.notification + 6_051 {
        put(&mut event, "notification_revision", notification);
        let user_changed = input.observe(1, number(&event, "user_revision").unwrap(), at);
        assert!(!reconcile_notification(
            &mut revision,
            Some(&w),
            &event,
            user_changed,
        ));
    }
    assert_eq!(revision, 17);
    assert_eq!(input.revision, 0);
    assert_eq!(input.timestamp, None);
    // Even unchanged content cannot hide an observed real user input.
    assert!(reconcile_notification(
        &mut revision,
        Some(&w),
        &event,
        true
    ));
    assert_eq!(revision, 18);
    assert!(reconcile_notification(&mut revision, None, &event, false));
    assert_eq!(revision, 19);
}
#[test]
fn semantic_native_notifications_still_invalidate_controller_visible_revision() {
    let w = witness();
    for key in ["revision", "user_revision"] {
        let mut event = snapshot_wire(&w);
        let value = number(&event, key).unwrap() + 1;
        put(&mut event, key, value);
        let mut revision = 17;
        assert!(reconcile_notification(
            &mut revision,
            Some(&w),
            &event,
            false
        ));
        assert_eq!(revision, 18, "{key}");
    }
    for (key, value) in [
        ("field_token", "another-field"),
        ("window_token", "another-window"),
        ("document_token", "another-block"),
        ("composing", "active"),
        ("composing", "unknown"),
    ] {
        let mut event = snapshot_wire(&w);
        text(&mut event, key, value);
        let mut revision = 17;
        assert!(reconcile_notification(
            &mut revision,
            Some(&w),
            &event,
            false
        ));
        assert_eq!(revision, 18, "{key}");
    }
    for (key, value) in [("focus", false), ("protected", true)] {
        let mut event = snapshot_wire(&w);
        put(&mut event, key, value);
        let mut revision = 17;
        assert!(reconcile_notification(
            &mut revision,
            Some(&w),
            &event,
            false
        ));
        assert_eq!(revision, 18, "{key}");
    }
}
#[test]
fn registration_policy_cannot_consume_keys_before_owner_disarms_fallback() {
    let mut p = policy(&Config::default(), 3, &peer(), &metadata(), 100).unwrap();
    assert!(policy_live(
        3,
        number(&p, "policy_epoch").unwrap(),
        number(&p, "expires_ns").unwrap(),
        101
    ));
    inert_policy(&mut p);
    assert!(!flag(&p, "enabled").unwrap());
    assert!(!policy_live(
        3,
        number(&p, "policy_epoch").unwrap(),
        number(&p, "expires_ns").unwrap(),
        101
    ));
    for feature in [
        "predictions",
        "emoji",
        "synonyms",
        "alternatives",
        "correction",
    ] {
        assert!(!flag(&p, feature).unwrap());
    }
}
#[test]
fn candidate_lists_have_an_aggregate_wire_limit_not_just_per_string_bounds() {
    let values = vec!["x".repeat(16_000); 40];
    assert!(values.iter().all(|value| bounded_text(value)));
    let mut wire = Dict::new();
    list(&mut wire, "values", &values).unwrap();
    assert!(!bounded(&wire));
}
#[test]
fn effective_tab_policy_suppresses_modified_tab_but_not_above_tab() {
    let mut c = Config::default();
    c.shortcuts.insert("word".into(), "Tab".into());
    c.shortcuts.insert("all".into(), "Shift+Tab".into());
    c.shortcuts.insert("force".into(), "Ctrl+Alt+Tab".into());
    c.shortcuts.insert("toggle".into(), "Ctrl+AboveTab".into());
    c.app_rules.insert(
        "app:native-writer".into(),
        [("tab_enabled".into(), serde_json::json!(false))].into(),
    );
    let p = policy(&c, 3, &peer(), &metadata(), 100).unwrap();
    let shortcuts =
        HashMap::<String, String>::try_from(p["shortcuts"].try_clone().unwrap()).unwrap();
    for action in ["word", "all", "force"] {
        assert_eq!(shortcuts.get(action).map(String::as_str), Some(""));
    }
    assert_eq!(shortcuts["toggle"], "Ctrl+AboveTab");
    let mut meta = metadata();
    text(&mut meta, "domain", "permitted.example");
    c.app_rules.insert(
        "domain:permitted.example".into(),
        [("tab_enabled".into(), serde_json::json!(true))].into(),
    );
    let p = policy(&c, 3, &peer(), &meta, 100).unwrap();
    let shortcuts =
        HashMap::<String, String>::try_from(p["shortcuts"].try_clone().unwrap()).unwrap();
    assert_eq!(shortcuts["word"], "Tab");
    assert_eq!(shortcuts["all"], "Shift+Tab");
}
#[test]
fn suspended_shortcuts_withdraw_policy_and_all_actions_unlike_disabled_config() {
    let mut c = Config::default();
    c.global_enabled = false;
    let mut p = policy(&c, 3, &peer(), &metadata(), 100).unwrap();
    let expiry = number(&p, "expires_ns").unwrap();
    assert!(!flag(&p, "enabled").unwrap());
    assert!(client_actions_live(true, 3, 3, expiry, 101));
    assert!(action_allowed(&c, &peer(), &metadata(), &Action::Toggle));
    gate_shortcut_policy(&mut p, false);
    assert_eq!(number(&p, "expires_ns").unwrap(), 0);
    assert!(!client_actions_live(false, 3, 3, expiry, 101));
    assert!(!client_actions_live(
        true,
        3,
        3,
        number(&p, "expires_ns").unwrap(),
        101
    ));
    assert!(!receipt_live(&frozen(), false, 3, Some(1), 200));
    assert!(receipt_live(&frozen(), true, 3, Some(1), 200));
    let mut revoked = frozen();
    revoked.revoked_at = Some(190);
    assert!(!receipt_live(&revoked, true, 3, Some(1), 200));
    assert!(validate_edit(revoked, &accepted(), 250).is_err());
}
#[test]
fn capture_reconciles_intervening_input_once_before_matching_notification() {
    let mut input = UserInputFence::default();
    let first = Instant::now();
    let second = first + Duration::from_millis(1);
    assert!(!input.observe(1, 5, first));
    assert_eq!(input.revision, 0);
    assert!(input.observe(1, 6, second));
    assert_eq!(input.revision, 1);
    assert_eq!(input.timestamp, Some(second));
    assert!(!input.observe(1, 6, second + Duration::from_millis(1)));
    assert_eq!(input.revision, 1);
    assert_eq!(input.timestamp, Some(second));
    // An earlier real confirmation must not regress an already observed later key.
    input.seed(1, 5);
    assert!(!input.observe(1, 6, second + Duration::from_millis(2)));
    assert_eq!(input.revision, 1);
    assert!(input.observe(1, 7, second + Duration::from_millis(3)));
    assert_eq!(input.revision, 2);
}
#[test]
fn queued_precapture_notifications_cannot_regress_a_new_capture_baseline() {
    let mut current = witness();
    current.user = 6;
    current.revision = 12;
    current.notification = 14;
    let mut earlier = snapshot_wire(&current);
    put(&mut earlier, "user_revision", 5u64);
    put(&mut earlier, "revision", 11u64);
    put(&mut earlier, "notification_revision", 13u64);
    assert!(stale_notification(&current, &earlier));
    let matching = snapshot_wire(&current);
    assert!(!stale_notification(&current, &matching));
    let mut later = snapshot_wire(&current);
    put(&mut later, "user_revision", 7u64);
    put(&mut later, "revision", 13u64);
    put(&mut later, "notification_revision", 15u64);
    assert!(!stale_notification(&current, &later));
    text(&mut earlier, "document_token", "new-block");
    assert!(!stale_notification(&current, &earlier));
}
#[test]
fn live_configure_is_bound_to_exact_focus_scope_and_generation() {
    let meta = metadata();
    let scope = FocusScope::parse(&meta).unwrap();
    let owner = FocusOwner {
        client: 1,
        scope: scope.clone(),
    };
    let mut live = policy(&Config::default(), 3, &peer(), &meta, 100).unwrap();
    bind_focus_policy(&mut live, 1, Some(&scope), Some(&owner));
    assert!(number(&live, "expires_ns").unwrap() > 100);
    for key in ["field_token", "window_token", "document_token"] {
        assert_eq!(string(&live, key).unwrap(), string(&meta, key).unwrap());
    }
    assert_eq!(number(&live, "focus_generation").unwrap(), 1);
    let mut refocused = scope.clone();
    refocused.generation += 1;
    let mut stale = policy(&Config::default(), 3, &peer(), &meta, 100).unwrap();
    bind_focus_policy(&mut stale, 1, Some(&refocused), Some(&owner));
    assert_eq!(number(&stale, "expires_ns").unwrap(), 0);
    let mut other_block = scope;
    other_block.document = "block2".into();
    assert!(!other_block.matches(&witness()));
    let mut stale = policy(&Config::default(), 3, &peer(), &meta, 100).unwrap();
    bind_focus_policy(&mut stale, 1, Some(&other_block), Some(&owner));
    assert_eq!(number(&stale, "expires_ns").unwrap(), 0);
}
#[test]
fn metadata_pending_edit_is_real_editing_state_not_composition_or_protection() {
    let mut meta = metadata();
    put(&mut meta, "pending_edit", true);
    assert!(pending_edit(&meta));
    assert!(!flag(&meta, "protected").unwrap());
    assert!(flag(&meta, "editable").unwrap());
    text(&mut meta, "composing", "idle");
    assert_eq!(string(&meta, "composing").unwrap(), "idle");
    assert!(server::metadata_valid(&meta, &peer()).is_ok());
    put(&mut meta, "pending_edit", false);
    assert!(!pending_edit(&meta));
    text(&mut meta, "pending_edit", "unknown");
    assert!(server::metadata_valid(&meta, &peer()).is_err());
}
#[test]
fn indeterminate_receipt_fences_provenance_without_claiming_or_requiring_after() {
    let mut event = accepted();
    text(&mut event, "outcome", "indeterminate");
    event.remove("after");
    let receipt = frozen();
    let expected = receipt.presentation.clone().unwrap();
    let uncertain = validate_uncertain(receipt, &event, 1500).unwrap();
    assert_eq!(uncertain.mode, "completion");
    assert_eq!(&*uncertain.before.text, "A😀bc");
    assert_eq!(uncertain.presentation, expected);
    assert!(validate_edit(frozen(), &event, 250).is_err());
    // An indeterminate native outcome explicitly has no authoritative readback.
    put(&mut event, "after", true);
    assert!(validate_uncertain(frozen(), &event, 250).is_ok());
    let mut ids = HashMap::new();
    assert!(take_acceptance(&mut ids, 1, number(&event, "acceptance_id").unwrap()).is_ok());
    assert!(take_acceptance(&mut ids, 1, number(&event, "acceptance_id").unwrap()).is_err());
}
#[test]
fn indeterminate_still_requires_exact_frozen_arm_before_range_text_and_lease() {
    let mut event = accepted();
    text(&mut event, "outcome", "indeterminate");
    event.remove("after");
    let mut unarmed = frozen();
    unarmed.armed_at = None;
    assert!(validate_uncertain(unarmed, &event, 250).is_err());
    let mut wrong = clone_dict(&event).unwrap();
    text(&mut wrong, "action", "choice");
    put(&mut wrong, "choice_index", 0u32);
    assert!(validate_uncertain(frozen(), &wrong, 250).is_err());
    let mut wrong = clone_dict(&event).unwrap();
    text(&mut wrong, "text", "xy");
    assert!(validate_uncertain(frozen(), &wrong, 250).is_err());
    let mut wrong = clone_dict(&event).unwrap();
    put(&mut wrong, "start", 4u32);
    assert!(validate_uncertain(frozen(), &wrong, 250).is_err());
    let mut wrong = clone_dict(&event).unwrap();
    put(&mut wrong, "accepted_ns", 1000u64);
    assert!(validate_uncertain(frozen(), &wrong, 1001).is_err());
    let mut wrong = clone_dict(&event).unwrap();
    let mut before = nested(&wrong, "before").unwrap();
    put(&mut before, "revision", 11u64);
    put(&mut wrong, "before", before);
    assert!(validate_uncertain(frozen(), &wrong, 250).is_err());
    let mut wrong = clone_dict(&event).unwrap();
    put(&mut wrong, "capability_id", 10u64);
    assert!(validate_uncertain(frozen(), &wrong, 250).is_err());
    assert!(validate_uncertain(frozen(), &event, LEASE + 201).is_err());
}
#[test]
fn metadata_action_origin_matches_capture_window_and_freezes_field_and_focus() {
    let target = frozen().target;
    let meta = metadata();
    let origin = metadata_origin(&target, &meta).unwrap();
    let snapshot = witness()
        .snapshot(target.id, &target.peer.app, target.peer.pid)
        .unwrap();
    assert_eq!(origin.app_id, snapshot.app_id);
    assert_eq!(origin.window_pid, snapshot.window_pid);
    assert_eq!(origin.window_id, snapshot.window_id);
    assert_eq!(origin.executable, target.peer.executable);
    let mut refocused = clone_dict(&meta).unwrap();
    put(&mut refocused, "focus_generation", 2u64);
    assert_ne!(
        origin.document_id,
        metadata_origin(&target, &refocused).unwrap().document_id
    );
    let mut other_field = clone_dict(&meta).unwrap();
    text(&mut other_field, "field_token", "field2");
    assert_ne!(
        origin.document_id,
        metadata_origin(&target, &other_field).unwrap().document_id
    );
    let mut claimed = meta;
    text(&mut claimed, "app_id", "claimed-app");
    put(&mut claimed, "pid", 1u32);
    let actual = metadata_origin(&target, &claimed).unwrap();
    assert_eq!(actual.app_id, target.peer.app);
    assert_eq!(actual.window_pid, target.peer.pid as i32);
}
#[test]
fn old_action_cannot_follow_a_renewed_scope_to_a_new_target() {
    let target = frozen().target;
    let mut meta = metadata();
    let scope = FocusScope::parse(&meta).unwrap();
    let owner = FocusOwner {
        client: target.id,
        scope: scope.clone(),
    };
    let mut event = Dict::new();
    scope.write(&mut event);
    put(&mut event, "policy_epoch", 3u64);
    assert!(action_origin(&target, &meta, &event, &owner, 3).is_ok());
    put(&mut meta, "focus_generation", 2u64);
    let renewed = FocusOwner {
        client: target.id,
        scope: FocusScope::parse(&meta).unwrap(),
    };
    assert!(action_origin(&target, &meta, &event, &renewed, 3).is_err());
    put(&mut event, "focus_generation", 2u64);
    assert!(action_origin(&target, &meta, &event, &renewed, 3).is_ok());
    assert!(action_origin(&target, &meta, &event, &renewed, 4).is_err());
}
#[test]
fn revoked_epoch_applied_receipt_becomes_provenance_without_reading_after() {
    let mut receipt = frozen();
    receipt.revoked_at = Some(190);
    let mut event = accepted();
    event.remove("after");
    assert!(validate_edit(frozen(), &event, 250).is_err());
    let uncertain = validate_uncertain(receipt, &event, 250).unwrap();
    assert_eq!(&*uncertain.before.text, "A😀bc");
    assert_eq!(uncertain.mode, "completion");
    let mut invalid = event;
    text(&mut invalid, "text", "not the issued Arm insertion");
    assert!(validate_uncertain(frozen(), &invalid, 250).is_err());
}
#[test]
fn consent_revocation_downgrades_queued_applied_and_keeps_existing_uncertainty() {
    let applied = validate_edit(frozen(), &accepted(), 250).unwrap().0;
    let mut event = accepted();
    text(&mut event, "outcome", "indeterminate");
    event.remove("after");
    let uncertain = validate_uncertain(frozen(), &event, 250).unwrap();
    let origin = metadata_origin(&frozen().target, &metadata()).unwrap();
    let mut events = VecDeque::from([
        NativeHostEvent::Applied(applied),
        NativeHostEvent::Indeterminate(uncertain),
        NativeHostEvent::Action {
            action: Action::Toggle,
            origin,
        },
    ]);
    fence_revoked_events(&mut events, &[], false);
    assert_eq!(events.len(), 2);
    assert!(
        events
            .iter()
            .all(|event| matches!(event, NativeHostEvent::Indeterminate(_)))
    );
    fence_revoked_events(&mut events, &[], false);
    assert_eq!(events.len(), 2);
}
#[test]
fn unavailable_broker_preserves_original_real_fallback_error() {
    let original = anyhow::Error::new(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "original native fallback failure",
    ));
    let result = compose_transports(
        Err(original),
        Err(anyhow::anyhow!("session bus unavailable")),
    );
    let error = match result {
        Ok(_) => panic!("Unexpected native transport"),
        Err(error) => error,
    };
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(error.to_string(), "original native fallback failure");
}
