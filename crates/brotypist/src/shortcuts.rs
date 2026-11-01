//! Canonical saved chords and physical key geometry, independent of key glyphs.
use anyhow::{Result, bail};
use std::collections::BTreeMap;

pub const MODIFIERS: [&str; 4] = ["Ctrl", "Alt", "Shift", "Super"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chord {
    pub key: String,
    pub modifiers: Vec<String>,
}

pub fn default_shortcuts() -> BTreeMap<String, String> {
    [
        ("word", "Tab"),
        ("all", "AboveTab"),
        ("force", "Ctrl+AboveTab"),
        ("app_toggle", "Ctrl+Alt+Super+AboveTab"),
        ("toggle", ""),
        ("alternatives", ""),
        ("synonyms", ""),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect()
}

fn canonical_part(part: &str) -> String {
    match part.trim().to_lowercase().as_str() {
        "control" | "ctrl" => "Ctrl".into(),
        "option" | "alt" => "Alt".into(),
        "shift" => "Shift".into(),
        "command" | "cmd" | "meta" | "super" | "win" | "windows" => "Super".into(),
        "esc" | "escape" => "Escape".into(),
        "tab" => "Tab".into(),
        "abovetab" => "AboveTab".into(),
        "space" => "Space".into(),
        "enter" | "return" => "Return".into(),
        _ => part.trim().to_uppercase(),
    }
}
fn supported_key(key: &str) -> bool {
    matches!(key, "Tab" | "AboveTab" | "Escape" | "Space" | "Return")
        || (key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric())
        || matches!(
            key,
            "F1" | "F2" | "F3" | "F4" | "F5" | "F6" | "F7" | "F8" | "F9" | "F10" | "F11" | "F12"
        )
}

pub fn normalize_shortcut(value: &str) -> Result<String> {
    if value.trim().is_empty() {
        return Ok(String::new());
    }
    let chord = parse_shortcut(value)?;
    let mut parts = chord.modifiers;
    parts.push(chord.key);
    Ok(parts.join("+"))
}

/// A disabled shortcut is not a chord; callers should skip empty saved values.
pub fn parse_shortcut(value: &str) -> Result<Chord> {
    let mut seen = [false; 4];
    let mut key = None;
    for part in value.split('+') {
        let part = canonical_part(part);
        if part.is_empty() {
            bail!("Shortcut contains an empty key");
        }
        if let Some(index) = MODIFIERS.iter().position(|&m| m == part) {
            if seen[index] {
                bail!("Shortcut repeats a modifier");
            }
            seen[index] = true;
        } else if !supported_key(&part) || key.replace(part).is_some() {
            bail!("Use one key with optional Ctrl, Alt, Shift, Super modifiers");
        }
    }
    let Some(key) = key else {
        bail!("Shortcut must specify one supported key");
    };
    Ok(Chord {
        key,
        modifiers: MODIFIERS
            .iter()
            .zip(seen)
            .filter_map(|(&m, present)| present.then(|| m.to_owned()))
            .collect(),
    })
}

pub fn shortcut_from_event(
    key: &str,
    modifiers: &[&str],
    platform: &str,
    keycode: Option<u32>,
) -> Result<String> {
    let physical = match platform {
        "macos" | "darwin" => mac_above_tab_keycode(None),
        "windows" | "win32" => Some(41),
        "linux" | "x11" => Some(49),
        _ => None,
    };
    let key = if keycode.is_some() && keycode == physical {
        "AboveTab"
    } else {
        key
    };
    let mut parts = modifiers.to_vec();
    parts.push(key);
    normalize_shortcut(&parts.join("+"))
}

pub fn mac_above_tab_keycode(layout_type: Option<u32>) -> Option<u32> {
    let layout = layout_type.or_else(current_mac_layout)?;
    match layout {
        0x49534f20 => Some(10),
        0x414e5349 => Some(50),
        _ => None,
    }
}
#[cfg(target_os = "macos")]
fn current_mac_layout() -> Option<u32> {
    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn LMGetKbdType() -> u8;
        fn KBGetLayoutType(kind: i16) -> u32;
    }
    Some(unsafe { KBGetLayoutType(i16::from(LMGetKbdType())) })
}
#[cfg(not(target_os = "macos"))]
fn current_mac_layout() -> Option<u32> {
    None
}

pub fn mac_key_name(code: u32) -> String {
    mac_key_name_for_layout(code, current_mac_layout())
}
pub fn mac_key_name_for_layout(code: u32, layout_type: Option<u32>) -> String {
    if Some(code) == mac_above_tab_keycode(layout_type) {
        return "AboveTab".into();
    }
    match code {
        0 => "A",
        1 => "S",
        2 => "D",
        3 => "F",
        4 => "H",
        5 => "G",
        6 => "Z",
        7 => "X",
        8 => "C",
        9 => "V",
        11 => "B",
        12 => "Q",
        13 => "W",
        14 => "E",
        15 => "R",
        16 => "Y",
        17 => "T",
        31 => "O",
        32 => "U",
        34 => "I",
        35 => "P",
        37 => "L",
        38 => "J",
        40 => "K",
        45 => "N",
        46 => "M",
        18 => "1",
        19 => "2",
        20 => "3",
        21 => "4",
        23 => "5",
        22 => "6",
        26 => "7",
        28 => "8",
        25 => "9",
        29 => "0",
        48 => "Tab",
        53 => "Escape",
        49 => "Space",
        36 => "Return",
        122 => "F1",
        120 => "F2",
        99 => "F3",
        118 => "F4",
        96 => "F5",
        97 => "F6",
        98 => "F7",
        100 => "F8",
        101 => "F9",
        109 => "F10",
        103 => "F11",
        111 => "F12",
        _ => "",
    }
    .into()
}

pub fn windows_key(vk: u32, scan: u32) -> String {
    if scan == 41 {
        return "AboveTab".into();
    }
    if (0x30..=0x39).contains(&vk) || (0x41..=0x5a).contains(&vk) {
        return char::from_u32(vk).unwrap().to_string();
    }
    if (0x70..=0x7b).contains(&vk) {
        return format!("F{}", vk - 0x6f);
    }
    match vk {
        9 => "Tab",
        27 => "Escape",
        32 => "Space",
        13 => "Return",
        _ => "",
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_chords_normalize_aliases_and_modifier_order() {
        for (raw, expected) in [
            ("ctrl+tab", "Ctrl+Tab"),
            ("Option+CMD+AboveTab", "Alt+Super+AboveTab"),
            ("shift+a", "Shift+A"),
            ("", ""),
            ("  ", ""),
            ("F12", "F12"),
            (
                "windows+CONTROL+shift+option+enter",
                "Ctrl+Alt+Shift+Super+Return",
            ),
            ("Escape", "Escape"),
            ("space", "Space"),
        ] {
            assert_eq!(normalize_shortcut(raw).unwrap(), expected);
        }
        let chord = parse_shortcut("cmd+alt+A").unwrap();
        assert_eq!(chord.key, "A");
        assert_eq!(chord.modifiers, ["Alt", "Super"]);
    }
    #[test]
    fn ambiguous_or_unsupported_chords_are_refused() {
        for value in [
            "Ctrl",
            "Ctrl++Tab",
            "Ctrl+Ctrl+A",
            "A+B",
            "Alt+Unknown",
            "F13",
            "+A",
            "A+",
            "Ctrl+Control+Tab",
            "é",
            "Alt+ ",
            "F01",
            "Fn+A",
        ] {
            assert!(normalize_shortcut(value).is_err(), "{value}");
        }
        assert!(parse_shortcut("").is_err());
    }
    #[test]
    fn all_supported_keys_round_trip() {
        for key in ('A'..='Z')
            .chain('0'..='9')
            .map(|c| c.to_string())
            .chain((1..=12).map(|n| format!("F{n}")))
            .chain(["Tab", "AboveTab", "Escape", "Space", "Return"].map(String::from))
        {
            for mask in 0..16 {
                let modifiers: Vec<_> = MODIFIERS
                    .iter()
                    .enumerate()
                    .filter_map(|(i, &m)| (mask & (1 << i) != 0).then_some(m))
                    .collect();
                let mut parts = modifiers;
                parts.push(&key);
                let value = parts.join("+");
                assert_eq!(normalize_shortcut(&value).unwrap(), value);
            }
        }
    }
    #[test]
    fn keyboard_geometry_does_not_hijack_the_other_key() {
        for (geometry, physical, other) in [(0x49534f20, 10, 50), (0x414e5349, 50, 10)] {
            assert_eq!(mac_above_tab_keycode(Some(geometry)), Some(physical));
            assert_eq!(
                mac_key_name_for_layout(physical, Some(geometry)),
                "AboveTab"
            );
            assert_eq!(mac_key_name_for_layout(other, Some(geometry)), "");
        }
        assert_eq!(mac_above_tab_keycode(Some(0)), None);
        for platform in ["windows", "win32", "linux", "x11"] {
            let code = if matches!(platform, "linux" | "x11") {
                49
            } else {
                41
            };
            assert_eq!(
                shortcut_from_event("§", &["Control"], platform, Some(code)).unwrap(),
                "Ctrl+AboveTab"
            );
        }
        assert!(shortcut_from_event("§", &[], "unknown", Some(10)).is_err());
    }
    #[test]
    fn native_key_maps_cover_letters_numbers_functions_and_specials() {
        for vk in (0x30..=0x39).chain(0x41..=0x5a) {
            assert_eq!(windows_key(vk, 0), char::from_u32(vk).unwrap().to_string());
        }
        for n in 1..=12 {
            assert_eq!(windows_key(0x6f + n, 0), format!("F{n}"));
        }
        assert_eq!(windows_key(0, 41), "AboveTab");
        assert_eq!(windows_key(0xff, 0), "");
        for (code, key) in [
            (0, "A"),
            (22, "6"),
            (48, "Tab"),
            (53, "Escape"),
            (49, "Space"),
            (36, "Return"),
            (111, "F12"),
        ] {
            assert_eq!(mac_key_name_for_layout(code, Some(0x414e5349)), key);
        }
    }
}
