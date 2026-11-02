//! Target-owned native clients paint first, then receive exact acceptance authority.
//! Their confirmations account for a real edit; they never call insertion again.
use super::*;
use platform::{
    NativeClientAcceptance, NativeClientOffer, NativeHostEvent, NativeHostPresentation,
};

const MAX_OFFERS: usize = 256;
const CONFIRMATION_GRACE: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(super) struct State {
    actions: VecDeque<(Action, platform::Foreground)>,
    pub(super) confirmed: u64,
    offers: VecDeque<Offer>,
    consuming: bool,
}
struct Plan {
    text: String,
    remainder: Option<SuggestionState>,
}
enum Authorized {
    Completion {
        word: Option<Plan>,
        all: Option<Plan>,
    },
    Correction(String),
    Choices(Vec<String>),
}
impl Authorized {
    fn insertions(&self) -> (&str, &str) {
        match self {
            Self::Completion { word, all } => (
                word.as_ref().map_or("", |plan| plan.text.as_str()),
                all.as_ref().map_or("", |plan| plan.text.as_str()),
            ),
            Self::Correction(text) => (text, text),
            Self::Choices(_) => ("", ""),
        }
    }
    fn take(self, action: &Action) -> Option<Plan> {
        match (self, action) {
            (Self::Completion { word, .. }, Action::Word) => word,
            (Self::Completion { all, .. }, Action::All) => all,
            (Self::Correction(text), Action::Word | Action::All) => Some(Plan {
                text,
                remainder: None,
            }),
            (Self::Choices(mut values), Action::Choice(index)) if *index < values.len() => {
                Some(Plan {
                    text: values.swap_remove(*index),
                    remainder: None,
                })
            }
            _ => None,
        }
    }
}
struct Offer {
    presentation: NativeHostPresentation,
    before: Snapshot,
    revision: u64,
    epoch: u64,
    mode: String,
    start: usize,
    length: usize,
    authorized: Authorized,
    issued_at: Instant,
    completion_counted: bool,
}
fn expiry(presentation: &NativeHostPresentation) -> Instant {
    match presentation {
        NativeHostPresentation::Preedit(receipt) => receipt.expires_at,
        NativeHostPresentation::Choices(receipt) => receipt.expires_at,
    }
}
fn same_before(a: &Snapshot, b: &Snapshot) -> bool {
    !a.protected
        && !b.protected
        && a.valid_range()
        && b.valid_range()
        && a.same_key(b)
        && a.same_authority(b)
        && a.text == b.text
        && a.caret == b.caret
        && a.selection_length == b.selection_length
        && a.can_replace == b.can_replace
        && a.on_demand_only == b.on_demand_only
}
impl Offer {
    fn timely(&self, accepted_at: Instant, now: Instant) -> bool {
        accepted_at >= self.issued_at
            && accepted_at <= expiry(&self.presentation)
            && accepted_at <= now
            && now.duration_since(accepted_at) <= CONFIRMATION_GRACE
    }
}

impl Controller {
    /// None means this target has no cooperating client. Some(false) is a native
    /// refusal, not permission to fall back to a second renderer/key consumer.
    pub fn present_client(
        &self,
        key: &SnapshotKey,
        authority: &Authority,
        revision: u64,
        mode: &str,
    ) -> Option<bool> {
        self.engine
            .borrow_mut()
            .present_client(key, authority, revision, mode)
    }
    pub fn uses_native_presentation(&self) -> bool {
        self.engine
            .borrow()
            .bridge
            .as_ref()
            .is_some_and(|bridge| bridge.uses_native_presentation())
    }
    pub fn client_owns_actions(&self) -> bool {
        self.engine
            .borrow()
            .bridge
            .as_ref()
            .is_some_and(|bridge| bridge.client_owns_actions())
    }
}
impl Engine {
    fn prune_client_offers(&mut self) {
        let now = self.now();
        self.native_client.offers.retain(|offer| {
            expiry(&offer.presentation)
                .checked_add(CONFIRMATION_GRACE)
                .is_some_and(|end| now <= end)
        });
    }
    fn completion_plan(&self, snapshot: &Snapshot, visible: &str, all: bool) -> Option<Plan> {
        let mut remainder = self.suggestion.clone();
        let text = remainder.accept(
            snapshot,
            all,
            self.config.trailing_space,
            features::include_punctuation(&self.config.punctuation_mode, all),
            Some(visible),
        )?;
        Some(Plan {
            text,
            remainder: Some(remainder),
        })
    }
    pub(super) fn present_client(
        &mut self,
        key: &SnapshotKey,
        authority: &Authority,
        revision: u64,
        mode: &str,
    ) -> Option<bool> {
        if !self
            .bridge
            .as_ref()
            .is_some_and(|bridge| bridge.client_owns_actions())
        {
            return None;
        }
        Some(
            self.try_present_client(key, authority, revision, mode)
                .unwrap_or(false),
        )
    }
    fn try_present_client(
        &mut self,
        key: &SnapshotKey,
        authority: &Authority,
        revision: u64,
        mode: &str,
    ) -> Result<bool> {
        self.prune_client_offers();
        if !self.native_ready
            || !self.active()
            || self.shortcuts_suspended
            || !self.presentation_is_current(key, authority, revision, mode)
            || self.native_client.offers.len() >= MAX_OFFERS
        {
            return Ok(false);
        }
        let Some(snapshot) = self.candidate.clone() else {
            return Ok(false);
        };
        if snapshot.protected || !self.allowed(&snapshot.app_id, &snapshot.domain, "") {
            return Ok(false);
        }
        let (text, values, start, length) = match mode {
            "completion" if snapshot.inline_safe() && snapshot.eligible(1) => {
                (self.suggestion.text.clone(), Vec::new(), snapshot.caret, 0)
            }
            "correction" if self.can_replace(&snapshot) => {
                let Some((before, correction)) = self.correction.as_ref() else {
                    return Ok(false);
                };
                if !same_before(before, &snapshot) {
                    return Ok(false);
                }
                (
                    correction.replacement.clone(),
                    Vec::new(),
                    correction.start,
                    correction.length,
                )
            }
            "alternatives" | "emoji" | "synonyms" => {
                let Some(choices) = self.choices.as_ref().filter(|choices| {
                    choices.mode == mode && same_before(&choices.snapshot, &snapshot)
                }) else {
                    return Ok(false);
                };
                if (choices.length != 0
                    || choices.start != snapshot.caret
                    || mode != "alternatives")
                    && !self.can_replace(&snapshot)
                {
                    return Ok(false);
                }
                (
                    String::new(),
                    choices.values.clone(),
                    choices.start,
                    choices.length,
                )
            }
            _ => return Ok(false),
        };
        let preserves_inline = mode == "completion"
            || (mode == "alternatives"
                && self
                    .choices
                    .as_ref()
                    .is_some_and(|choices| choices.anchored));
        if !preserves_inline {
            self.claims.clear();
            self.bridge.as_mut().unwrap().hide_native()?;
        }
        let offered_at = self.now();
        let Some(presentation) =
            self.bridge
                .as_mut()
                .unwrap()
                .present_client_offer(&NativeClientOffer {
                    snapshot: &snapshot,
                    user_input_revision: self.candidate_revision,
                    mode,
                    text: &text,
                    values: &values,
                    start,
                    length,
                })?
        else {
            return Ok(false);
        };
        let now = self.now();
        let authorized = match &presentation {
            NativeHostPresentation::Preedit(receipt)
                if receipt.revision == self.candidate_revision
                    && receipt.expires_at > now
                    && receipt.expires_at <= now + Duration::from_secs(2) =>
            {
                match mode {
                    "completion"
                        if self
                            .presentation_count(key, authority, &receipt.text, mode)
                            .is_some() =>
                    {
                        let word = self.completion_plan(&snapshot, &receipt.text, false);
                        let all = self.completion_plan(&snapshot, &receipt.text, true);
                        if word.is_none() && all.is_none() {
                            None
                        } else {
                            Some(Authorized::Completion { word, all })
                        }
                    }
                    "correction" if receipt.text == text && !text.is_empty() => {
                        Some(Authorized::Correction(text))
                    }
                    _ => None,
                }
            }
            NativeHostPresentation::Choices(receipt)
                if matches!(mode, "alternatives" | "emoji" | "synonyms")
                    && receipt.revision == self.candidate_revision
                    && receipt.expires_at > now
                    && receipt.expires_at <= now + Duration::from_secs(2)
                    && !receipt.values.is_empty()
                    && receipt.values.len() <= values.len()
                    && receipt.values == values[..receipt.values.len()] =>
            {
                Some(Authorized::Choices(receipt.values.clone()))
            }
            _ => None,
        };
        let Some(authorized) =
            authorized.filter(|_| self.presentation_is_current(key, authority, revision, mode))
        else {
            self.disarm();
            let _ = self.bridge.as_mut().unwrap().hide_native();
            return Ok(false);
        };
        let claim = Arc::new(Claim {
            key: key.clone(),
            authority: authority.clone(),
            revision: self.candidate_revision,
            epoch: self.current_epoch(),
            mode: mode.into(),
            visible: match &presentation {
                NativeHostPresentation::Preedit(receipt) => receipt.text.clone(),
                NativeHostPresentation::Choices(receipt) => receipt.values.join("\n"),
            },
            count: match &presentation {
                NativeHostPresentation::Choices(receipt) => receipt.values.len(),
                _ => 0,
            },
            evidence: match &presentation {
                NativeHostPresentation::Preedit(receipt) => {
                    PresentationEvidence::NativePreedit(receipt.clone())
                }
                NativeHostPresentation::Choices(receipt) => {
                    PresentationEvidence::NativeChoices(receipt.clone())
                }
            },
        });
        // Install the frozen authorization BEFORE Arm: the native owner may apply
        // an edit before the RPC reply is delivered, including a timed-out reply.
        self.native_client.offers.push_back(Offer {
            presentation: presentation.clone(),
            before: snapshot,
            revision: self.candidate_revision,
            epoch: self.current_epoch(),
            mode: mode.into(),
            start,
            length,
            authorized,
            issued_at: offered_at,
            completion_counted: self.completion_counted,
        });
        let (word_text, all_text) = self
            .native_client
            .offers
            .back()
            .unwrap()
            .authorized
            .insertions();
        let armed = self
            .bridge
            .as_mut()
            .unwrap()
            .arm_client_offer(&NativeClientAcceptance {
                presentation: &presentation,
                word_text,
                all_text,
            });
        if !matches!(armed, Ok(true)) {
            self.disarm();
            let _ = self.bridge.as_mut().unwrap().hide_native();
            return Ok(false);
        }
        self.claims.retain(|current| current.mode != mode);
        self.claims.push(claim);
        self.refresh_arming();
        Ok(true)
    }
    pub(super) fn pump_client_confirmations(&mut self) {
        if self.native_client.consuming
            || (self.native_client.offers.is_empty()
                && !self
                    .bridge
                    .as_ref()
                    .is_some_and(|bridge| bridge.client_owns_actions()))
        {
            return;
        }
        if self
            .bridge
            .as_mut()
            .is_some_and(|bridge| bridge.pump_events().is_err())
        {
            self.fail("Native event transport");
            return;
        }
        self.consume_host_events();
    }

    /// Called before ordinary recapture. A later user-input notification must not
    /// cause an earlier real native edit to be forgotten or learned as authored.
    pub(super) fn consume_host_events(&mut self) {
        if self.native_client.consuming {
            return;
        }
        self.native_client.consuming = true;
        self.prune_client_offers();
        let events = self
            .bridge
            .as_mut()
            .map(|bridge| bridge.drain_host_events())
            .unwrap_or_default();
        if self.stopping {
            self.native_client.offers.clear();
            self.native_client.actions.clear();
            self.native_client.consuming = false;
            return;
        }
        for event in events {
            match event {
                NativeHostEvent::Applied(edit) => self.confirm_client_edit(edit),
                NativeHostEvent::Indeterminate(native) => {
                    if let Some(offer) = self.take_client_offer(&native.presentation, &native.mode)
                    {
                        if same_before(&offer.before, &native.before)
                            && offer.timely(native.accepted_at, self.now())
                        {
                            self.uncertain_provenance =
                                Some((native.before.authority(), offer.revision));
                        }
                    }
                }
                NativeHostEvent::Dismissed(dismissed) => self.confirm_client_dismissal(dismissed),
                NativeHostEvent::Action { action, origin }
                    if self.native_client.actions.len() < 64 =>
                {
                    self.native_client.actions.push_back((action, origin));
                }
                _ => {}
            }
        }
        self.native_client.consuming = false;
    }
    pub(super) fn run_host_actions(&mut self) {
        while let Some((action, origin)) = self.native_client.actions.pop_front() {
            if self.stopping || self.shortcuts_suspended || !self.input_claim_valid() {
                continue;
            }
            if matches!(action, Action::Toggle | Action::Settings) {
                self.action(action, None, None);
                continue;
            }
            if !matches!(
                action,
                Action::AppToggle | Action::Force | Action::Alternatives | Action::Synonyms
            ) {
                continue;
            }
            let current = self
                .bridge
                .as_mut()
                .and_then(|bridge| bridge.foreground().ok().flatten());
            if !current.is_some_and(|current| {
                current.app_id == origin.app_id
                    && current.domain == origin.domain
                    && current.window_id == origin.window_id
                    && current.window_pid == origin.window_pid
                    && current.document_id == origin.document_id
            }) {
                continue;
            }
            if action == Action::AppToggle {
                // Metadata-only toggling must also be able to re-enable this app.
                self.toggle_app(&origin.app_id, &origin.domain);
            } else {
                self.action(action, None, None);
            }
        }
    }

    fn take_client_offer(
        &mut self,
        presentation: &NativeHostPresentation,
        mode: &str,
    ) -> Option<Offer> {
        let index = self
            .native_client
            .offers
            .iter()
            .position(|offer| offer.presentation == *presentation && offer.mode == mode)?;
        self.native_client.offers.remove(index)
    }
    fn client_confirmation_allowed(&self, offer: &Offer, before: &Snapshot, at: Instant) -> bool {
        self.native_ready
            && self.active()
            && !self.shortcuts_suspended
            && self.allowed(&before.app_id, &before.domain, "")
            && same_before(&offer.before, before)
            && offer.timely(at, self.now())
    }
    fn client_view_current(&self, offer: &Offer) -> bool {
        self.current_epoch() == offer.epoch
            && self.user_revision() == Some(offer.revision)
            && self
                .candidate
                .as_ref()
                .is_some_and(|candidate| same_before(candidate, &offer.before))
    }
    fn confirm_client_edit(&mut self, native: platform::NativeHostEdit) {
        let Some(offer) = self.take_client_offer(&native.presentation, &native.mode) else {
            return;
        };
        if !self.client_confirmation_allowed(&offer, &native.before, native.accepted_at)
            || native.start != offer.start
            || native.length != offer.length
        {
            return;
        }
        let restore_view = self.client_view_current(&offer);
        let count_completion = !offer.completion_counted;
        let Some(mut plan) = offer.authorized.take(&native.action) else {
            return;
        };
        if plan.text != native.text {
            return;
        }
        let Some(expected) =
            edit_snapshot(&native.before, native.start, native.length, &native.text)
        else {
            return;
        };
        if !restore_view {
            plan.remainder = None
        }
        let edit = PendingEdit {
            before: native.before,
            expected,
            text: native.text,
            start: native.start,
            length: native.length,
            remainder: plan.remainder,
            at: native.accepted_at,
            revision: offer.revision,
            mode: native.mode,
        };
        if !confirmed_edit(&edit, &native.after) {
            self.uncertain_provenance = Some((edit.before.authority(), edit.revision));
            return;
        }
        self.commit_confirmed_edit(edit, native.after, restore_view, count_completion);
        self.native_client.confirmed = self.native_client.confirmed.wrapping_add(1);
    }
    fn confirm_client_dismissal(&mut self, native: platform::NativeHostDismiss) {
        let Some(offer) = self.take_client_offer(&native.presentation, &native.mode) else {
            return;
        };
        if !self.client_confirmation_allowed(&offer, &native.before, native.accepted_at) {
            return;
        }
        let clear_view = self.client_view_current(&offer);
        self.finish_dismissal(
            &native.before,
            &native.mode,
            native.bypass,
            native.accepted_at,
            clear_view,
        );
    }
    pub(super) fn finish_dismissal(
        &mut self,
        snapshot: &Snapshot,
        mode: &str,
        bypass: bool,
        at: Instant,
        clear_view: bool,
    ) -> bool {
        if clear_view {
            self.clear(false, true)
        }
        self.mark_attempt(snapshot, mode);
        if !bypass {
            match self.config.escape_behavior.as_str() {
                "pause" => {
                    self.field_pause = Some((
                        snapshot.authority(),
                        at + Duration::from_secs(self.config.escape_pause_seconds.into()),
                    ))
                }
                "app" => {
                    self.escape_app = snapshot.app_id.clone();
                    self.configure_native();
                }
                _ => {}
            }
        }
        bypass || self.config.escape_behavior != "passthrough"
    }
}
