//! Pure input policy: the port of `Input/InputRules.cs`, plus the extended-key table that the C#
//! kept inline in `WindowsPeripheralPlatform.SendKey`.
//!
//! Everything here is arithmetic over virtual-key codes, so it is testable on any host - which
//! matters, because these are exactly the rules whose bugs leave a key latched down in Windows
//! with no way to release it except logging out.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;

/// Emergency stop is Ctrl+Shift+Pause.
///
/// With Ctrl held, Windows reports the Pause key as `VK_CANCEL` (0x03) instead of `VK_PAUSE`
/// (0x13), so both are accepted. This is not a guess about the hardware: it is documented
/// behaviour of the keyboard driver, which maps Ctrl+Pause to Break.
pub fn is_emergency_stop(virtual_key: u16, ctrl: bool, shift: bool) -> bool {
    ctrl && shift && matches!(virtual_key, 0x13 | 0x03)
}

/// The gesture, for the UI and the tray menu. One definition, so the label can never drift from
/// the code that implements it.
pub const EMERGENCY_STOP_GESTURE: &str = "Ctrl+Shift+Pause";

/// Whether a virtual key needs `KEYEVENTF_EXTENDEDKEY` when injected.
///
/// Without the flag the OS resolves the scan code to the numeric-pad twin of the key, so Right
/// Alt arrives as Left Alt, the arrow cluster arrives as numpad digits, and Delete arrives as
/// `.`. The set is: the navigation cluster, Insert/Delete, both Windows keys and Menu, numpad
/// divide, Num Lock, Right Ctrl, Right Alt, and the browser/media/volume keys.
pub fn is_extended_key(virtual_key: u16) -> bool {
    matches!(
        virtual_key,
        0x21 // PRIOR (Page Up)
            | 0x22 // NEXT (Page Down)
            | 0x23 // END
            | 0x24 // HOME
            | 0x25 // LEFT
            | 0x26 // UP
            | 0x27 // RIGHT
            | 0x28 // DOWN
            | 0x2D // INSERT
            | 0x2E // DELETE
            | 0x5B // LWIN
            | 0x5C // RWIN
            | 0x5D // APPS (Menu)
            | 0x6F // DIVIDE (numpad /)
            | 0x90 // NUMLOCK
            | 0xA3 // RCONTROL
            | 0xA5 // RMENU (Right Alt)
            | 0xAD..=0xB7 // volume, media transport and browser keys
    )
}

/// Pairs down and up events for low-level hooks.
///
/// An up event is swallowed exactly when its down event was swallowed, so a policy change between
/// the two - a profile switch, game mode toggled, a binding deleted - can never leave a key or
/// button stuck down in Windows. This is the single most important invariant in the hook layer:
/// swallowing a down and then passing its up through is harmless, but passing a down through and
/// then swallowing its up latches the key in every application on the desktop.
#[derive(Debug, Default)]
pub struct InputSuppressionTracker<T: Eq + Hash> {
    down: Mutex<HashMap<T, bool>>,
}

impl<T: Eq + Hash + Clone> InputSuppressionTracker<T> {
    pub fn new() -> Self {
        Self { down: Mutex::new(HashMap::new()) }
    }

    /// Records the decision for a down event and returns it. Auto-repeat downs keep the first
    /// decision, so a key held across a profile switch behaves consistently for its whole press.
    pub fn on_down(&self, key: T, should_swallow: bool) -> bool {
        let Ok(mut down) = self.down.lock() else {
            // A poisoned lock means a hook panicked. Passing input through is the safe failure:
            // the alternative swallows every key with no way back.
            return false;
        };
        *down.entry(key).or_insert(should_swallow)
    }

    /// Whether the matching down was swallowed, in which case this up must be swallowed too.
    pub fn on_up(&self, key: &T) -> bool {
        let Ok(mut down) = self.down.lock() else {
            return false;
        };
        down.remove(key).unwrap_or(false)
    }

    /// Keys currently believed to be down. For the diagnostics page.
    pub fn held(&self) -> Vec<T> {
        self.down
            .lock()
            .map(|down| down.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Forgets every pending decision. Called when the hooks are torn down, so a restart does not
    /// inherit the previous run's idea of what is held.
    pub fn clear(&self) {
        if let Ok(mut down) = self.down.lock() {
            down.clear();
        }
    }
}

/// Mouse hook message numbers, named so the dispatch table reads as the API does.
pub mod message {
    pub const LBUTTONDOWN: u32 = 0x0201;
    pub const LBUTTONUP: u32 = 0x0202;
    pub const RBUTTONDOWN: u32 = 0x0204;
    pub const RBUTTONUP: u32 = 0x0205;
    pub const MBUTTONDOWN: u32 = 0x0207;
    pub const MBUTTONUP: u32 = 0x0208;
    pub const MOUSEWHEEL: u32 = 0x020A;
    pub const XBUTTONDOWN: u32 = 0x020B;
    pub const XBUTTONUP: u32 = 0x020C;
    pub const KEYDOWN: u32 = 0x0100;
    pub const KEYUP: u32 = 0x0101;
    pub const SYSKEYDOWN: u32 = 0x0104;
    pub const SYSKEYUP: u32 = 0x0105;
}

/// Decoded mouse hook event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEvent {
    Button { button: ghub_core::MouseButtonKind, down: bool },
    /// Wheel notch. Positive is away from the user, 120 per notch.
    Wheel { delta: i16 },
}

/// Classifies a `WH_MOUSE_LL` message. `mouse_data` is the `MSLLHOOKSTRUCT` field.
///
/// Split out of the hook procedure so the whole decode - including the X-button index living in
/// the high word of `mouse_data` - is testable without installing a hook.
pub fn classify_mouse(message: u32, mouse_data: u32) -> Option<MouseEvent> {
    use ghub_core::MouseButtonKind as Button;
    let x_button = if (mouse_data >> 16) == 1 { Button::XButton1 } else { Button::XButton2 };
    Some(match message {
        message::LBUTTONDOWN => MouseEvent::Button { button: Button::Left, down: true },
        message::LBUTTONUP => MouseEvent::Button { button: Button::Left, down: false },
        message::RBUTTONDOWN => MouseEvent::Button { button: Button::Right, down: true },
        message::RBUTTONUP => MouseEvent::Button { button: Button::Right, down: false },
        message::MBUTTONDOWN => MouseEvent::Button { button: Button::Middle, down: true },
        message::MBUTTONUP => MouseEvent::Button { button: Button::Middle, down: false },
        message::XBUTTONDOWN => MouseEvent::Button { button: x_button, down: true },
        message::XBUTTONUP => MouseEvent::Button { button: x_button, down: false },
        message::MOUSEWHEEL => MouseEvent::Wheel { delta: (mouse_data >> 16) as i16 },
        _ => return None,
    })
}

/// Whether a `WH_KEYBOARD_LL` message is a key press (as opposed to a release).
pub fn is_key_down(message: u32) -> bool {
    matches!(message, message::KEYDOWN | message::SYSKEYDOWN)
}

/// Updates held-modifier state from a key event. Returns `(ctrl, shift)`.
///
/// Both the side-specific codes and the aggregate `VK_CONTROL` / `VK_SHIFT` are accepted, because
/// which one arrives depends on the keyboard driver.
pub fn track_modifiers(virtual_key: u16, down: bool, ctrl: bool, shift: bool) -> (bool, bool) {
    match virtual_key {
        0xA2 | 0xA3 | 0x11 => (down, shift),
        0xA0 | 0xA1 | 0x10 => (ctrl, down),
        _ => (ctrl, shift),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghub_core::MouseButtonKind as Button;

    #[test]
    fn emergency_stop_needs_both_modifiers() {
        assert!(is_emergency_stop(0x13, true, true));
        assert!(is_emergency_stop(0x03, true, true), "Ctrl+Pause reports VK_CANCEL");
        assert!(!is_emergency_stop(0x13, true, false));
        assert!(!is_emergency_stop(0x13, false, true));
        assert!(!is_emergency_stop(0x41, true, true));
    }

    #[test]
    fn the_extended_set_covers_the_keys_that_would_otherwise_arrive_as_numpad() {
        for vk in [0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x2D, 0x2E] {
            assert!(is_extended_key(vk), "navigation key 0x{vk:02X} must be extended");
        }
        assert!(is_extended_key(0xA5), "Right Alt would arrive as Left Alt");
        assert!(is_extended_key(0xA3), "Right Ctrl would arrive as Left Ctrl");
        assert!(is_extended_key(0xAF), "volume up is in the 0xAD..=0xB7 run");
        assert!(!is_extended_key(0xA2), "Left Ctrl is not extended");
        assert!(!is_extended_key(0x41), "a letter is never extended");
        assert!(!is_extended_key(0x6A), "numpad multiply is not extended, only divide is");
        assert!(!is_extended_key(0xB8), "past the media run");
        assert!(!is_extended_key(0xAC), "before the media run");
    }

    #[test]
    fn a_swallowed_down_swallows_its_up() {
        let tracker = InputSuppressionTracker::<u16>::new();
        assert!(tracker.on_down(0x41, true));
        assert!(tracker.on_up(&0x41));
    }

    #[test]
    fn a_passed_down_passes_its_up_even_if_policy_changed_in_between() {
        let tracker = InputSuppressionTracker::<u16>::new();
        assert!(!tracker.on_down(0x41, false));
        // Game mode was switched on while the key was held. The up must still pass through,
        // or the key stays down in every application on the desktop.
        assert!(!tracker.on_up(&0x41));
    }

    #[test]
    fn auto_repeat_downs_keep_the_first_decision() {
        let tracker = InputSuppressionTracker::<u16>::new();
        assert!(tracker.on_down(0x41, true));
        assert!(tracker.on_down(0x41, false), "the repeat must not change the verdict");
        assert!(tracker.on_down(0x41, false));
        assert!(tracker.on_up(&0x41));
        // After the release the next press decides afresh.
        assert!(!tracker.on_down(0x41, false));
    }

    #[test]
    fn an_up_with_no_recorded_down_passes_through() {
        let tracker = InputSuppressionTracker::<u16>::new();
        assert!(!tracker.on_up(&0x41), "the down happened before the hook was installed");
    }

    #[test]
    fn the_tracker_reports_and_clears_held_keys() {
        let tracker = InputSuppressionTracker::<u16>::new();
        tracker.on_down(0x41, true);
        tracker.on_down(0x42, false);
        let mut held = tracker.held();
        held.sort_unstable();
        assert_eq!(held, [0x41, 0x42]);
        tracker.clear();
        assert!(tracker.held().is_empty());
        assert!(!tracker.on_up(&0x41));
    }

    #[test]
    fn the_tracker_works_for_mouse_buttons_too() {
        let tracker = InputSuppressionTracker::<Button>::new();
        assert!(tracker.on_down(Button::XButton1, true));
        assert!(!tracker.on_down(Button::Left, false));
        assert!(tracker.on_up(&Button::XButton1));
        assert!(!tracker.on_up(&Button::Left));
    }

    #[test]
    fn mouse_messages_decode_to_the_right_button() {
        assert_eq!(
            classify_mouse(message::LBUTTONDOWN, 0),
            Some(MouseEvent::Button { button: Button::Left, down: true })
        );
        assert_eq!(
            classify_mouse(message::RBUTTONUP, 0),
            Some(MouseEvent::Button { button: Button::Right, down: false })
        );
        assert_eq!(
            classify_mouse(message::MBUTTONDOWN, 0),
            Some(MouseEvent::Button { button: Button::Middle, down: true })
        );
    }

    #[test]
    fn the_x_button_index_comes_from_the_high_word() {
        assert_eq!(
            classify_mouse(message::XBUTTONDOWN, 0x0001_0000),
            Some(MouseEvent::Button { button: Button::XButton1, down: true })
        );
        assert_eq!(
            classify_mouse(message::XBUTTONDOWN, 0x0002_0000),
            Some(MouseEvent::Button { button: Button::XButton2, down: true })
        );
        assert_eq!(
            classify_mouse(message::XBUTTONUP, 0x0002_0000),
            Some(MouseEvent::Button { button: Button::XButton2, down: false })
        );
    }

    #[test]
    fn wheel_deltas_are_signed() {
        // A scroll towards the user is 0xFF88 in the high word: unsigned maths would read it as
        // +65416 and scroll the wrong way by a screenful.
        assert_eq!(
            classify_mouse(message::MOUSEWHEEL, 0xFF88_0000),
            Some(MouseEvent::Wheel { delta: -120 })
        );
        assert_eq!(
            classify_mouse(message::MOUSEWHEEL, 0x0078_0000),
            Some(MouseEvent::Wheel { delta: 120 })
        );
    }

    #[test]
    fn unrelated_mouse_messages_are_ignored() {
        assert_eq!(classify_mouse(0x0200 /* MOUSEMOVE */, 0), None);
        assert_eq!(classify_mouse(message::KEYDOWN, 0), None);
    }

    #[test]
    fn system_keys_count_as_presses() {
        assert!(is_key_down(message::KEYDOWN));
        assert!(is_key_down(message::SYSKEYDOWN), "Alt+key arrives as WM_SYSKEYDOWN");
        assert!(!is_key_down(message::KEYUP));
        assert!(!is_key_down(message::SYSKEYUP));
    }

    #[test]
    fn modifier_tracking_accepts_both_the_sided_and_aggregate_codes() {
        let (ctrl, shift) = track_modifiers(0xA2, true, false, false);
        assert!(ctrl && !shift);
        let (ctrl, shift) = track_modifiers(0xA1, true, ctrl, shift);
        assert!(ctrl && shift);
        let (ctrl, shift) = track_modifiers(0x11, false, ctrl, shift);
        assert!(!ctrl && shift, "the aggregate VK_CONTROL releases Ctrl");
        let (ctrl, shift) = track_modifiers(0x41, true, ctrl, shift);
        assert!(!ctrl && shift, "a letter changes nothing");
    }
}
