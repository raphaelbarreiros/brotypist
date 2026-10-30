//! Pure decisions shared with the Win32/UIA boundary; no operating-system calls.
use crate::core::{ScalarIndex, Snapshot};

pub(super) const MAX_TEXT: usize = 1_000_000;

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Modifiers { pub ctrl: bool, pub alt: bool, pub shift: bool, pub super_key: bool, pub right_alt: bool }
pub(super) fn shortcut_action(policy: &super::ObservationPolicy, key: PhysicalKey, flags: u32, modifiers: Modifiers, app: &str) -> Option<super::super::Action> {
    if flags & 0x12 != 0 || (modifiers.right_alt && modifiers.ctrl) || (key.vk == 9 && modifiers.alt) { return None; }
    let mut names = [""; 4]; let mut count = 0;
    for (active, name) in [(modifiers.ctrl, "Ctrl"), (modifiers.alt, "Alt"), (modifiers.shift, "Shift"), (modifiers.super_key, "Super")] {
        if active { names[count] = name; count += 1; }
    }
    policy.key_action(physical_name(key.vk, key.scan), &names[..count], "windows", app, "")
}

pub(super) fn readable_element(password: bool, enabled: bool, focused: bool, offscreen: bool, process: i32, expected_process: i32, control: i32) -> bool {
    !password && enabled && focused && !offscreen && process == expected_process && matches!(control, 50004 | 50030)
}

pub(super) fn replacement_units(snapshot: &Snapshot, start: usize, length: usize, text: &str) -> Option<(usize, usize)> {
    if snapshot.protected || !snapshot.valid_range() || text.is_empty() || text.contains('\0') { return None; }
    let end = start.checked_add(length)?;
    let index = snapshot.text_index();
    Some((index.scalar_to_utf16(ScalarIndex(start))?.0, index.scalar_to_utf16(ScalarIndex(end))?.0))
}

pub(super) fn provider_selection(text: &str, prefix: &str, selected_prefix: &str) -> Option<(usize, usize)> {
    if text.chars().count() > MAX_TEXT || !text.starts_with(prefix) || !text.starts_with(selected_prefix)
        || !selected_prefix.starts_with(prefix) { return None; }
    let start = prefix.chars().count();
    Some((start, selected_prefix.chars().count().checked_sub(start)?))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Readback { Applied, Unchanged, Uncertain }

/// The host may enforce a length limit. Confirm only the exact requested insertion
/// or a nonempty scalar prefix, with both surrounding slices and caret preserved.
pub(super) fn classify_readback(before: &Snapshot, after: &Snapshot, start: usize, length: usize, inserted: &str) -> Readback {
    if !before.same_authority(after) || after.protected || !after.valid_range() { return Readback::Uncertain; }
    if before.text == after.text { return Readback::Unchanged; }
    let Some(range) = before.text_index().scalar_range_to_utf8(start, length) else { return Readback::Uncertain };
    let prefix = &before.text[..range.start];
    let suffix = &before.text[range.end..];
    let Some(changed) = after.text.strip_prefix(prefix).and_then(|value| value.strip_suffix(suffix)) else { return Readback::Uncertain };
    if changed.is_empty() || !inserted.starts_with(changed) || after.selection_length != 0
        || after.caret != start + changed.chars().count() { return Readback::Uncertain; }
    Readback::Applied
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PhysicalKey { pub vk: u32, pub scan: u32, pub extended: bool }

/// A bounded set: repeats cannot allocate or orphan an edge when policy changes.
#[derive(Default)]
pub(super) struct ConsumedKeys { keys: [Option<PhysicalKey>; 32] }
impl ConsumedKeys {
    pub fn contains(&self, key: PhysicalKey) -> bool { self.keys.contains(&Some(key)) }
    pub fn press(&mut self, key: PhysicalKey) -> bool {
        if self.contains(key) { return true; }
        let Some(slot) = self.keys.iter_mut().find(|slot| slot.is_none()) else { return false };
        *slot = Some(key); true
    }
    pub fn release(&mut self, key: PhysicalKey) -> bool {
        let Some(slot) = self.keys.iter_mut().find(|slot| **slot == Some(key)) else { return false };
        *slot = None; true
    }
    pub fn has_capacity(&self) -> bool { self.keys.iter().any(Option::is_none) }
    pub fn is_empty(&self) -> bool { self.keys.iter().all(Option::is_none) }
}

pub(super) fn physical_name(vk: u32, scan: u32) -> &'static str {
    if scan == 41 { return "AboveTab"; }
    match vk {
        0x09 => "Tab", 0x1b => "Escape", 0x20 => "Space", 0x0d => "Return",
        0x08 => "Backspace", 0x2e => "Delete", 0x25 => "Left", 0x26 => "Up", 0x27 => "Right", 0x28 => "Down",
        0x24 => "Home", 0x23 => "End", 0x21 => "PageUp", 0x22 => "PageDown",
        0x30 => "0", 0x31 => "1", 0x32 => "2", 0x33 => "3", 0x34 => "4", 0x35 => "5", 0x36 => "6", 0x37 => "7", 0x38 => "8", 0x39 => "9",
        0x41 => "A", 0x42 => "B", 0x43 => "C", 0x44 => "D", 0x45 => "E", 0x46 => "F", 0x47 => "G", 0x48 => "H", 0x49 => "I", 0x4a => "J", 0x4b => "K", 0x4c => "L", 0x4d => "M", 0x4e => "N", 0x4f => "O", 0x50 => "P", 0x51 => "Q", 0x52 => "R", 0x53 => "S", 0x54 => "T", 0x55 => "U", 0x56 => "V", 0x57 => "W", 0x58 => "X", 0x59 => "Y", 0x5a => "Z",
        0x70 => "F1", 0x71 => "F2", 0x72 => "F3", 0x73 => "F4", 0x74 => "F5", 0x75 => "F6", 0x76 => "F7", 0x77 => "F8", 0x78 => "F9", 0x79 => "F10", 0x7a => "F11", 0x7b => "F12", _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(text: &str, caret: usize) -> Snapshot { Snapshot { app_id: "editor".into(), field_id: "42:17".into(), window_id: "42:10".into(), window_pid: 42, text: text.into(), caret, ..Default::default() } }
    #[test]
    fn actual_hook_decision_preserves_altgr_injection_os_switching_and_exact_modifiers() {
        use super::super::super::Action;
        let mut config = crate::config::Config::default();
        config.shortcuts.insert("force".into(), "Ctrl+AboveTab".into());
        config.shortcuts.insert("word".into(), "Alt+Tab".into());
        let policy = super::super::ObservationPolicy::new(&config).unwrap();
        let key = PhysicalKey { vk: 0xc0, scan: 41, extended: false };
        let ctrl = Modifiers { ctrl: true, ..Default::default() };
        assert_eq!(shortcut_action(&policy, key, 0, ctrl, "editor"), Some(Action::Force));
        assert_eq!(shortcut_action(&policy, key, 0x10, ctrl, "editor"), None);
        assert_eq!(shortcut_action(&policy, key, 0x02, ctrl, "editor"), None);
        assert_eq!(shortcut_action(&policy, key, 0, Modifiers { alt: true, ..ctrl }, "editor"), None);
        assert_eq!(shortcut_action(&policy, key, 0, Modifiers { alt: true, right_alt: true, ..ctrl }, "editor"), None);
        assert_eq!(shortcut_action(&policy, PhysicalKey { vk: 9, scan: 15, extended: false }, 0, Modifiers { alt: true, ..Default::default() }, "editor"), None);
    }
    #[test]
    fn native_metadata_gates_precede_any_text_capability() {
        assert!(readable_element(false, true, true, false, 42, 42, 50004));
        assert!(readable_element(false, true, true, false, 42, 42, 50030));
        assert!(!readable_element(true, true, true, false, 42, 42, 50004));
        assert!(!readable_element(false, false, true, false, 42, 42, 50004));
        assert!(!readable_element(false, true, false, false, 42, 42, 50004));
        assert!(!readable_element(false, true, true, true, 42, 42, 50004));
        assert!(!readable_element(false, true, true, false, 43, 42, 50004));
        assert!(!readable_element(false, true, true, false, 42, 42, 50020));
    }
    #[test]
    fn native_replacement_offsets_are_checked_utf16_not_bytes_or_scalars() {
        let before = snapshot("a😀:-)", 5);
        assert_eq!(replacement_units(&before, 2, 3, "🙂"), Some((3, 6)));
        assert_eq!(replacement_units(&before, 5, usize::MAX, "x"), None);
        assert_eq!(replacement_units(&before, 6, 0, "x"), None);
        assert_eq!(replacement_units(&before, 0, 0, "\0"), None);
        assert_eq!(before.text_index().utf16_to_scalar(crate::core::Utf16Offset(2)), None);
    }
    #[test]
    fn provider_ranges_measure_actual_prefixes_without_character_unit_assumptions() {
        assert_eq!(provider_selection("a😀e\u{301}z", "a😀", "a😀e\u{301}"), Some((2, 2)));
        assert_eq!(provider_selection("abc", "ab", "a"), None);
        assert_eq!(provider_selection("abc", "ax", "abc"), None);
    }
    #[test]
    fn constrained_host_applies_only_exact_nonempty_prefix_with_matching_authority_and_caret() {
        let before = snapshot("a😀:-)!", 5);
        let mut after = snapshot("a😀🙂!", 3);
        assert_eq!(classify_readback(&before, &after, 2, 3, "🙂 extra"), Readback::Applied);
        after.caret = 2;
        assert_eq!(classify_readback(&before, &after, 2, 3, "🙂 extra"), Readback::Uncertain);
        after.caret = 3; after.window_id = "42:other".into();
        assert_eq!(classify_readback(&before, &after, 2, 3, "🙂 extra"), Readback::Uncertain);
        assert_eq!(classify_readback(&before, &before, 2, 3, "🙂"), Readback::Unchanged);
        let changed = snapshot("a😀unrequested!", 13);
        assert_eq!(classify_readback(&before, &changed, 2, 3, "🙂"), Readback::Uncertain);
    }
    #[test]
    fn consumed_press_repeat_and_release_remain_paired_after_disable() {
        let key = PhysicalKey { vk: 9, scan: 15, extended: false };
        let mut pairs = ConsumedKeys::default();
        assert!(pairs.press(key)); assert!(pairs.press(key));
        assert!(pairs.contains(key));
        assert!(!pairs.release(PhysicalKey { extended: true, ..key }));
        assert!(!pairs.release(PhysicalKey { scan: 99, ..key }));
        assert!(!pairs.is_empty()); // stop cannot acknowledge while the paired press is owned
        assert!(pairs.release(key)); assert!(pairs.is_empty()); assert!(!pairs.release(key));
    }
    #[test]
    fn physical_above_tab_does_not_depend_on_keyboard_glyph() {
        assert_eq!(physical_name(0xc0, 41), "AboveTab");
        assert_eq!(physical_name(0x31, 2), "1");
    }
}
