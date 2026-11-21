//! G815 US (ANSI) physical layout. Port of `Artemis.Peripherals/Logitech/G815Layout.cs`.
//!
//! Three numbers matter per key: the HID usage (what the keyboard sends), the 0x8081 LED id (what
//! the lighting frame addresses) and the position in key units (what the UI paints). They are not
//! interchangeable and two of them are zero for some keys, so each is stated rather than derived
//! at the call site.
//!
//! LED ids: keyboard-zone ids are `usage - 3`, modifiers `usage - 0x78`, and the media keys use
//! OpenRGB's own G815 table. The G-key ids (`0xB4..0xB8`) and the logo id (`0xD2`) follow
//! OpenRGB's documented zone transform but do not appear in its key table, so they carry
//! [`LedEvidence::Derived`] until a device confirms them.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeyGroup {
    Main,
    Function,
    Navigation,
    Numpad,
    Media,
    GKey,
    ModeKey,
    Special,
    Logo,
}

/// How much is known about a key's LED id. Surfaced on the diagnostics page so a derived id is
/// never reported as a verified one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedEvidence {
    /// In OpenRGB's G815 key table.
    Reference,
    /// From OpenRGB's zone transform, cross-checked against G HUB's own `g815_us` geometry, but
    /// not in any key table.
    Derived,
    /// The key has no software-addressable LED at all.
    None,
}

impl LedEvidence {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Reference => "reference",
            Self::Derived => "derived",
            Self::None => "none",
        }
    }
}

/// One physical key.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyDefinition {
    /// Stable id used in profiles and key reactions, such as `Escape`, `W`, `G3`.
    pub id: &'static str,
    pub label: &'static str,
    /// HID keyboard usage. 0 for keys that are not HID keys, such as the G-keys.
    pub hid_usage: u8,
    /// 0x8081 per-key LED id. 0 means the key cannot be lit individually.
    pub led_id: u8,
    /// Position and size in key units.
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub group: KeyGroup,
}

impl KeyDefinition {
    pub fn led_evidence(&self) -> LedEvidence {
        if self.led_id == 0 {
            LedEvidence::None
        } else if matches!(self.group, KeyGroup::GKey | KeyGroup::Logo) {
            LedEvidence::Derived
        } else {
            LedEvidence::Reference
        }
    }

    /// Whether 0x4522's game mode can lock this key out. HID keys only, and not the media keys:
    /// locking out volume would be a surprise, not a feature.
    pub fn is_game_mode_candidate(&self) -> bool {
        self.hid_usage != 0 && self.group != KeyGroup::Media
    }
}

/// 0x4540 reported layout 1 (US) on the attached keyboard.
pub const LAYOUT_ID: u8 = 1;

/// Indicator-zone LED of the brightness key: OpenRGB's G815 table lists zone 0x40 id 0x99, and
/// G HUB's own `g815_us` geometry has exactly one `PERKEY_INDICATOR` component.
pub const BRIGHTNESS_LED_ID: u8 = 0x99;

/// The game-mode key has no software-addressable LED.
///
/// Neither OpenRGB's table nor G HUB's geometry lists one - G HUB treats the key as a click target
/// only. 0x98, the other half of the indicator pair, was tried on hardware and the firmware
/// answered `InvalidArgument`; because 0x8081 validates a whole frame, that one unknown id blanks
/// the entire keyboard. The key stays selectable in the UI; it simply cannot be lit.
pub const GAME_MODE_LED_ID: u8 = 0;

/// Horizontal offset of everything right of the G-key column.
const O: f64 = 1.25;

/// Keyboard-zone LED id for a HID usage.
const fn led(usage: u8) -> u8 {
    if usage >= 0xE0 && usage <= 0xE7 {
        usage - 0x78
    } else {
        usage - 3
    }
}

/// The whole layout, built once.
pub fn keys() -> &'static [KeyDefinition] {
    use std::sync::OnceLock;
    static KEYS: OnceLock<Vec<KeyDefinition>> = OnceLock::new();
    KEYS.get_or_init(build)
}

pub fn by_usage(usage: u8) -> Option<&'static KeyDefinition> {
    if usage == 0 {
        return None;
    }
    keys().iter().find(|k| k.hid_usage == usage)
}

pub fn by_id(id: &str) -> Option<&'static KeyDefinition> {
    keys().iter().find(|k| k.id == id)
}

pub fn by_led_id(led_id: u8) -> Option<&'static KeyDefinition> {
    if led_id == 0 {
        return None;
    }
    keys().iter().find(|k| k.led_id == led_id)
}

/// Every addressable LED id, which is what a per-key lighting frame may contain.
pub fn led_ids() -> Vec<u8> {
    let mut ids: Vec<u8> = keys().iter().map(|k| k.led_id).filter(|id| *id != 0).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

pub fn game_mode_candidates() -> Vec<&'static KeyDefinition> {
    keys().iter().filter(|k| k.is_game_mode_candidate()).collect()
}

/// Width and height of the whole board in key units, for laying the canvas out.
pub fn extent() -> (f64, f64) {
    keys().iter().fold((0.0f64, 0.0f64), |(w, h), k| {
        (w.max(k.x + k.w), h.max(k.y + k.h))
    })
}

fn build() -> Vec<KeyDefinition> {
    let mut k: Vec<KeyDefinition> = Vec::with_capacity(128);

    /// A HID key. `x` is relative to the right of the G-key column, and the LED id follows from
    /// the usage.
    macro_rules! key {
        ($id:literal, $label:literal, $usage:expr, $x:expr, $y:expr) => {
            key!($id, $label, $usage, $x, $y, 1.0, KeyGroup::Main, 1.0)
        };
        ($id:literal, $label:literal, $usage:expr, $x:expr, $y:expr, $w:expr) => {
            key!($id, $label, $usage, $x, $y, $w, KeyGroup::Main, 1.0)
        };
        ($id:literal, $label:literal, $usage:expr, $x:expr, $y:expr, $w:expr, $g:expr) => {
            key!($id, $label, $usage, $x, $y, $w, $g, 1.0)
        };
        ($id:literal, $label:literal, $usage:expr, $x:expr, $y:expr, $w:expr, $g:expr, $h:expr) => {
            k.push(KeyDefinition {
                id: $id,
                label: $label,
                hid_usage: $usage,
                led_id: led($usage),
                x: $x + O,
                y: $y,
                w: $w,
                h: $h,
                group: $g,
            })
        };
    }

    /// A key that is not a HID key: its LED id is given outright.
    macro_rules! raw {
        ($id:literal, $label:literal, $led:expr, $x:expr, $y:expr, $g:expr) => {
            k.push(KeyDefinition {
                id: $id,
                label: $label,
                hid_usage: 0,
                led_id: $led,
                x: $x,
                y: $y,
                w: 1.0,
                h: 1.0,
                group: $g,
            })
        };
    }

    // Top strip. The logo sits in the G-key column, so it takes no offset.
    raw!("Logo", "G", 0xD2, 0.0, 0.0, KeyGroup::Logo);
    raw!("M1", "M1", 0, O + 1.0, 0.0, KeyGroup::ModeKey);
    raw!("M2", "M2", 0, O + 2.0, 0.0, KeyGroup::ModeKey);
    raw!("M3", "M3", 0, O + 3.0, 0.0, KeyGroup::ModeKey);
    raw!("MR", "MR", 0, O + 4.0, 0.0, KeyGroup::ModeKey);
    raw!("GameMode", "Game", GAME_MODE_LED_ID, O + 15.25, 0.0, KeyGroup::Special);
    raw!("Brightness", "\u{2600}", BRIGHTNESS_LED_ID, O + 16.25, 0.0, KeyGroup::Special);
    raw!("MediaPrevious", "\u{23ee}", 0x9E, O + 18.5, 0.0, KeyGroup::Media);
    raw!("MediaPlayPause", "\u{23ef}", 0x9B, O + 19.5, 0.0, KeyGroup::Media);
    raw!("MediaNext", "\u{23ed}", 0x9D, O + 20.5, 0.0, KeyGroup::Media);
    raw!("MediaMute", "\u{1f507}", 0x9C, O + 21.5, 0.0, KeyGroup::Media);

    // G-key column. Not HID keys: they arrive through HID++ 0x8010 when software control is on.
    raw!("G1", "G1", 0xB4, 0.0, 1.25, KeyGroup::GKey);
    raw!("G2", "G2", 0xB5, 0.0, 2.25, KeyGroup::GKey);
    raw!("G3", "G3", 0xB6, 0.0, 3.25, KeyGroup::GKey);
    raw!("G4", "G4", 0xB7, 0.0, 4.25, KeyGroup::GKey);
    raw!("G5", "G5", 0xB8, 0.0, 5.25, KeyGroup::GKey);

    // Function row. The F-keys gain half a unit of gap after every fourth key.
    let y = 1.25;
    key!("Escape", "Esc", 0x29, 0.0, y, 1.0, KeyGroup::Function);
    const F_NAMES: [&str; 12] =
        ["F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12"];
    for (i, name) in F_NAMES.iter().enumerate() {
        let usage = 0x3A + i as u8;
        k.push(KeyDefinition {
            id: name,
            label: name,
            hid_usage: usage,
            led_id: led(usage),
            x: 2.0 + i as f64 + (i / 4) as f64 * 0.5 + O,
            y,
            w: 1.0,
            h: 1.0,
            group: KeyGroup::Function,
        });
    }
    key!("PrintScreen", "PrtSc", 0x46, 15.25, y, 1.0, KeyGroup::Function);
    key!("ScrollLock", "ScrLk", 0x47, 16.25, y, 1.0, KeyGroup::Function);
    key!("Pause", "Pause", 0x48, 17.25, y, 1.0, KeyGroup::Function);

    // Number row.
    let y = 2.5;
    key!("Grave", "`", 0x35, 0.0, y);
    const DIGIT_IDS: [&str; 10] =
        ["D1", "D2", "D3", "D4", "D5", "D6", "D7", "D8", "D9", "D0"];
    const DIGIT_LABELS: [&str; 10] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"];
    for i in 0..10 {
        let usage = 0x1E + i as u8;
        k.push(KeyDefinition {
            id: DIGIT_IDS[i],
            label: DIGIT_LABELS[i],
            hid_usage: usage,
            led_id: led(usage),
            x: 1.0 + i as f64 + O,
            y,
            w: 1.0,
            h: 1.0,
            group: KeyGroup::Main,
        });
    }
    key!("Minus", "-", 0x2D, 11.0, y);
    key!("Equals", "=", 0x2E, 12.0, y);
    key!("Backspace", "Backspace", 0x2A, 13.0, y, 2.0);
    key!("Insert", "Ins", 0x49, 15.25, y, 1.0, KeyGroup::Navigation);
    key!("Home", "Home", 0x4A, 16.25, y, 1.0, KeyGroup::Navigation);
    key!("PageUp", "PgUp", 0x4B, 17.25, y, 1.0, KeyGroup::Navigation);
    key!("NumLock", "Num", 0x53, 18.5, y, 1.0, KeyGroup::Numpad);
    key!("NumDivide", "/", 0x54, 19.5, y, 1.0, KeyGroup::Numpad);
    key!("NumMultiply", "*", 0x55, 20.5, y, 1.0, KeyGroup::Numpad);
    key!("NumMinus", "-", 0x56, 21.5, y, 1.0, KeyGroup::Numpad);

    // QWERTY row.
    let y = 3.5;
    key!("Tab", "Tab", 0x2B, 0.0, y, 1.5);
    const ROW1: [(&str, u8); 10] = [
        ("Q", 0x14),
        ("W", 0x1A),
        ("E", 0x08),
        ("R", 0x15),
        ("T", 0x17),
        ("Y", 0x1C),
        ("U", 0x18),
        ("I", 0x0C),
        ("O", 0x12),
        ("P", 0x13),
    ];
    for (i, (name, usage)) in ROW1.iter().enumerate() {
        k.push(KeyDefinition {
            id: name,
            label: name,
            hid_usage: *usage,
            led_id: led(*usage),
            x: 1.5 + i as f64 + O,
            y,
            w: 1.0,
            h: 1.0,
            group: KeyGroup::Main,
        });
    }
    key!("LeftBracket", "[", 0x2F, 11.5, y);
    key!("RightBracket", "]", 0x30, 12.5, y);
    key!("Backslash", "\\", 0x31, 13.5, y, 1.5);
    key!("Delete", "Del", 0x4C, 15.25, y, 1.0, KeyGroup::Navigation);
    key!("End", "End", 0x4D, 16.25, y, 1.0, KeyGroup::Navigation);
    key!("PageDown", "PgDn", 0x4E, 17.25, y, 1.0, KeyGroup::Navigation);
    key!("Num7", "7", 0x5F, 18.5, y, 1.0, KeyGroup::Numpad);
    key!("Num8", "8", 0x60, 19.5, y, 1.0, KeyGroup::Numpad);
    key!("Num9", "9", 0x61, 20.5, y, 1.0, KeyGroup::Numpad);
    key!("NumPlus", "+", 0x57, 21.5, y, 1.0, KeyGroup::Numpad, 2.0);

    // Home row.
    let y = 4.5;
    key!("CapsLock", "Caps", 0x39, 0.0, y, 1.75);
    const ROW2: [(&str, u8); 9] = [
        ("A", 0x04),
        ("S", 0x16),
        ("D", 0x07),
        ("F", 0x09),
        ("G", 0x0A),
        ("H", 0x0B),
        ("J", 0x0D),
        ("K", 0x0E),
        ("L", 0x0F),
    ];
    for (i, (name, usage)) in ROW2.iter().enumerate() {
        k.push(KeyDefinition {
            id: name,
            label: name,
            hid_usage: *usage,
            led_id: led(*usage),
            x: 1.75 + i as f64 + O,
            y,
            w: 1.0,
            h: 1.0,
            group: KeyGroup::Main,
        });
    }
    key!("Semicolon", ";", 0x33, 10.75, y);
    key!("Apostrophe", "'", 0x34, 11.75, y);
    key!("Enter", "Enter", 0x28, 12.75, y, 2.25);
    key!("Num4", "4", 0x5C, 18.5, y, 1.0, KeyGroup::Numpad);
    key!("Num5", "5", 0x5D, 19.5, y, 1.0, KeyGroup::Numpad);
    key!("Num6", "6", 0x5E, 20.5, y, 1.0, KeyGroup::Numpad);

    // Bottom letter row.
    let y = 5.5;
    key!("LeftShift", "Shift", 0xE1, 0.0, y, 2.25);
    const ROW3: [(&str, u8); 7] = [
        ("Z", 0x1D),
        ("X", 0x1B),
        ("C", 0x06),
        ("V", 0x19),
        ("B", 0x05),
        ("N", 0x11),
        ("M", 0x10),
    ];
    for (i, (name, usage)) in ROW3.iter().enumerate() {
        k.push(KeyDefinition {
            id: name,
            label: name,
            hid_usage: *usage,
            led_id: led(*usage),
            x: 2.25 + i as f64 + O,
            y,
            w: 1.0,
            h: 1.0,
            group: KeyGroup::Main,
        });
    }
    key!("Comma", ",", 0x36, 9.25, y);
    key!("Period", ".", 0x37, 10.25, y);
    key!("Slash", "/", 0x38, 11.25, y);
    key!("RightShift", "Shift", 0xE5, 12.25, y, 2.75);
    key!("Up", "\u{2191}", 0x52, 16.25, y, 1.0, KeyGroup::Navigation);
    key!("Num1", "1", 0x59, 18.5, y, 1.0, KeyGroup::Numpad);
    key!("Num2", "2", 0x5A, 19.5, y, 1.0, KeyGroup::Numpad);
    key!("Num3", "3", 0x5B, 20.5, y, 1.0, KeyGroup::Numpad);
    key!("NumEnter", "Enter", 0x58, 21.5, y, 1.0, KeyGroup::Numpad, 2.0);

    // Space row.
    let y = 6.5;
    key!("LeftControl", "Ctrl", 0xE0, 0.0, y, 1.25);
    key!("LeftWindows", "Win", 0xE3, 1.25, y, 1.25);
    key!("LeftAlt", "Alt", 0xE2, 2.5, y, 1.25);
    key!("Space", "", 0x2C, 3.75, y, 6.25);
    key!("RightAlt", "Alt", 0xE6, 10.0, y, 1.25);
    key!("RightWindows", "Win", 0xE7, 11.25, y, 1.25);
    key!("Menu", "Menu", 0x65, 12.5, y, 1.25);
    key!("RightControl", "Ctrl", 0xE4, 13.75, y, 1.25);
    key!("Left", "\u{2190}", 0x50, 15.25, y, 1.0, KeyGroup::Navigation);
    key!("Down", "\u{2193}", 0x51, 16.25, y, 1.0, KeyGroup::Navigation);
    key!("Right", "\u{2192}", 0x4F, 17.25, y, 1.0, KeyGroup::Navigation);
    key!("Num0", "0", 0x62, 18.5, y, 2.0, KeyGroup::Numpad);
    key!("NumDecimal", ".", 0x63, 20.5, y, 1.0, KeyGroup::Numpad);

    k
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn every_key_id_is_unique() {
        // Ids are what profiles and key reactions store; a duplicate would make one of them
        // unreachable and the other ambiguous.
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for key in keys() {
            *seen.entry(key.id).or_default() += 1;
        }
        let duplicates: Vec<&&str> = seen.iter().filter(|(_, n)| **n > 1).map(|(id, _)| id).collect();
        assert!(duplicates.is_empty(), "duplicate key ids: {duplicates:?}");
    }

    #[test]
    fn every_hid_usage_and_led_id_appears_at_most_once() {
        // A repeated LED id would be written twice in one frame; a repeated usage would make a
        // key press ambiguous.
        let mut usages: HashMap<u8, Vec<&str>> = HashMap::new();
        let mut leds: HashMap<u8, Vec<&str>> = HashMap::new();
        for key in keys() {
            if key.hid_usage != 0 {
                usages.entry(key.hid_usage).or_default().push(key.id);
            }
            if key.led_id != 0 {
                leds.entry(key.led_id).or_default().push(key.id);
            }
        }
        for (usage, ids) in &usages {
            assert_eq!(ids.len(), 1, "usage 0x{usage:02X} is on {ids:?}");
        }
        for (led, ids) in &leds {
            assert_eq!(ids.len(), 1, "LED 0x{led:02X} is on {ids:?}");
        }
    }

    #[test]
    fn keyboard_led_ids_follow_the_documented_transform() {
        // Letters and digits: usage - 3.
        assert_eq!(by_id("A").unwrap().led_id, 0x04 - 3);
        assert_eq!(by_id("Escape").unwrap().led_id, 0x29 - 3);
        assert_eq!(by_id("F12").unwrap().led_id, 0x45 - 3);
        // Modifiers: usage - 0x78, which is a different rule and the easy one to get wrong.
        assert_eq!(by_id("LeftControl").unwrap().led_id, 0xE0 - 0x78);
        assert_eq!(by_id("RightWindows").unwrap().led_id, 0xE7 - 0x78);
        // 0xE0..0xE7 is the modifier range; 0x65 (Menu) sits below it and takes the other rule.
        assert_eq!(by_id("Menu").unwrap().led_id, 0x65 - 3);
    }

    #[test]
    fn the_game_mode_key_is_present_but_cannot_be_lit() {
        // It has to exist, because the UI shows it as a click target. It must have LED id 0,
        // because 0x8081 validates the whole frame and one unknown id blanks the keyboard.
        let key = by_id("GameMode").expect("the game-mode key must be in the layout");
        assert_eq!(key.led_id, 0);
        assert_eq!(key.led_evidence(), LedEvidence::None);
        assert!(!led_ids().contains(&0), "a zero id must never reach a frame");
        assert!(by_led_id(0).is_none());
    }

    #[test]
    fn the_brightness_key_carries_the_one_indicator_led() {
        assert_eq!(by_id("Brightness").unwrap().led_id, BRIGHTNESS_LED_ID);
        assert_eq!(by_id("Brightness").unwrap().led_evidence(), LedEvidence::Reference);
    }

    #[test]
    fn g_keys_and_the_logo_are_marked_derived_not_verified() {
        // Their ids come from a zone transform, not from any key table. Claiming otherwise would
        // present a guess as evidence.
        for id in ["G1", "G2", "G3", "G4", "G5", "Logo"] {
            let key = by_id(id).unwrap_or_else(|| panic!("{id} is missing"));
            assert_eq!(key.led_evidence(), LedEvidence::Derived, "{id}");
            assert_ne!(key.led_id, 0, "{id} is addressable, just not from a table");
            assert_eq!(key.hid_usage, 0, "{id} is not a HID key");
        }
        assert_eq!(by_id("G1").unwrap().led_id, 0xB4);
        assert_eq!(by_id("G5").unwrap().led_id, 0xB8);
        assert_eq!(by_id("Logo").unwrap().led_id, 0xD2);
    }

    #[test]
    fn mode_keys_are_click_targets_with_no_led_and_no_usage() {
        for id in ["M1", "M2", "M3", "MR"] {
            let key = by_id(id).unwrap();
            assert_eq!(key.led_id, 0, "{id}");
            assert_eq!(key.hid_usage, 0, "{id}");
            assert_eq!(key.group, KeyGroup::ModeKey);
        }
    }

    #[test]
    fn a_usage_lookup_never_answers_for_a_non_hid_key() {
        // Every G-key, mode key and the logo has usage 0. A lookup for 0 must not return one of
        // them, or a stray zero byte in a report would be decoded as a key press.
        assert!(by_usage(0).is_none());
        assert_eq!(by_usage(0x04).unwrap().id, "A");
        assert!(by_usage(0xFF).is_none());
    }

    #[test]
    fn game_mode_candidates_exclude_the_media_keys_and_the_g_keys() {
        let candidates = game_mode_candidates();
        assert!(candidates.iter().all(|k| k.hid_usage != 0));
        assert!(
            !candidates.iter().any(|k| k.group == KeyGroup::Media),
            "locking out volume is not what a game mode is for"
        );
        assert!(!candidates.iter().any(|k| k.id.starts_with('G') && k.group == KeyGroup::GKey));
        // The Windows keys, which are the default game-mode exclusions, must be candidates.
        assert!(candidates.iter().any(|k| k.hid_usage == 0xE3));
        assert!(candidates.iter().any(|k| k.hid_usage == 0xE7));
    }

    #[test]
    fn the_layout_has_the_key_count_the_hardware_does() {
        // 110 lit components on the G815 per G HUB's own geometry, plus the five non-lit click
        // targets (M1-M3, MR and the game-mode key).
        let total = keys().len();
        let lit = led_ids().len();
        assert_eq!(lit, total - 5, "exactly five keys have no LED: {lit} lit of {total}");
        assert!((100..=120).contains(&total), "unexpected key count {total}");
    }

    #[test]
    fn no_two_keys_overlap_on_the_canvas() {
        // The UI hit-tests by rectangle, so an overlap would make one key unclickable.
        let all = keys();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                let overlap_x = a.x < b.x + b.w - 1e-9 && b.x < a.x + a.w - 1e-9;
                let overlap_y = a.y < b.y + b.h - 1e-9 && b.y < a.y + a.h - 1e-9;
                assert!(
                    !(overlap_x && overlap_y),
                    "{} at ({}, {}) {}x{} overlaps {} at ({}, {}) {}x{}",
                    a.id, a.x, a.y, a.w, a.h, b.id, b.x, b.y, b.w, b.h
                );
            }
        }
    }

    #[test]
    fn the_extent_covers_every_key() {
        let (w, h) = extent();
        for key in keys() {
            assert!(key.x + key.w <= w + 1e-9, "{} runs past the width", key.id);
            assert!(key.y + key.h <= h + 1e-9, "{} runs past the height", key.id);
        }
        // Sanity: a full-size board with a numpad and a G-key column.
        assert!(w > 20.0 && w < 26.0, "width {w}");
        assert!(h > 7.0 && h < 9.0, "height {h}");
    }

    #[test]
    fn the_g_key_column_is_left_of_everything_else() {
        let g1 = by_id("G1").unwrap();
        let escape = by_id("Escape").unwrap();
        assert!(g1.x + g1.w <= escape.x + 1e-9, "the G-key column must not sit under Escape");
        assert_eq!(g1.x, 0.0);
        assert_eq!(escape.x, O);
    }
}
