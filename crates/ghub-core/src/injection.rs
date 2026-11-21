//! Synthetic input, and the bookkeeping that guarantees nothing is left held down. Port of
//! `Artemis.Peripherals/Actions/InputInjection.cs`.
//!
//! [`TrackingInjector`] exists for one reason: a macro that presses Alt and is then interrupted -
//! emergency stop, device unplugged, profile switch, process exit - must not leave the user's
//! desktop stuck in Alt-Tab. Every press is recorded and [`TrackingInjector::release_all`]
//! undoes them, non-modifiers first so a chord comes apart in the order the OS expects.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::action::MouseButtonKind;

/// Somewhere synthetic input can go. The Windows implementation uses `SendInput`; tests use
/// [`RecordingInjector`].
pub trait InputInjector: Send + Sync {
    fn key_down(&self, virtual_key: u16);
    fn key_up(&self, virtual_key: u16);
    fn mouse_down(&self, button: MouseButtonKind);
    fn mouse_up(&self, button: MouseButtonKind);
    fn mouse_move(&self, dx: i32, dy: i32);
    fn wheel(&self, delta: i32, horizontal: bool);
    fn type_text(&self, text: &str);
}

/// Whether a virtual key is a modifier, which decides release order.
pub fn is_modifier(virtual_key: u16) -> bool {
    matches!(virtual_key, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C) || (0xA0..=0xA5).contains(&virtual_key)
}

thread_local! {
    /// Set while this thread is emitting input, so an input hook on the same thread can ignore
    /// our own synthetic events rather than treating them as a user keystroke and recursing.
    static INJECTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// True while the calling thread is inside an injection.
pub fn is_injecting() -> bool {
    INJECTING.with(|f| f.get())
}

/// Runs `f` with this thread marked as injecting, so [`is_injecting`] is true inside it.
///
/// Public because the hook routing has to be *tested* for the property that matters - that an
/// injected key is not routed back through the bindings that produced it - and there is no other
/// way to enter that state without a real device event.
pub fn during_injection<R>(f: impl FnOnce() -> R) -> R {
    marked(f)
}

fn marked<R>(f: impl FnOnce() -> R) -> R {
    INJECTING.with(|flag| flag.set(true));
    let result = f();
    INJECTING.with(|flag| flag.set(false));
    result
}

#[derive(Default)]
struct Held {
    keys: HashSet<u16>,
    buttons: HashSet<MouseButtonKind>,
}

/// Tracks everything currently held down so it can always be released.
pub struct TrackingInjector {
    inner: Arc<dyn InputInjector>,
    held: Mutex<Held>,
    last_release_failed: AtomicBool,
}

impl std::fmt::Debug for TrackingInjector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let held = self.held.lock().unwrap();
        write!(f, "TrackingInjector({} keys, {} buttons)", held.keys.len(), held.buttons.len())
    }
}

impl TrackingInjector {
    pub fn new(inner: Arc<dyn InputInjector>) -> Self {
        Self { inner, held: Mutex::new(Held::default()), last_release_failed: AtomicBool::new(false) }
    }

    /// Another tracker over the same sink.
    ///
    /// Each macro run gets its own, so stopping one run releases only what *it* pressed and
    /// leaves a key another binding is holding alone.
    pub fn child(&self) -> Self {
        Self::new(Arc::clone(&self.inner))
    }

    pub fn held_keys(&self) -> Vec<u16> {
        let mut keys: Vec<u16> = self.held.lock().unwrap().keys.iter().copied().collect();
        keys.sort_unstable();
        keys
    }

    pub fn held_buttons(&self) -> Vec<MouseButtonKind> {
        self.held.lock().unwrap().buttons.iter().copied().collect()
    }

    pub fn is_holding_anything(&self) -> bool {
        let held = self.held.lock().unwrap();
        !held.keys.is_empty() || !held.buttons.is_empty()
    }

    /// True when the last [`Self::release_all`] could not let go of everything.
    pub fn last_release_failed(&self) -> bool {
        self.last_release_failed.load(Ordering::SeqCst)
    }

    /// Releases every key and button this tracker pressed, non-modifiers first.
    ///
    /// Non-modifiers first because releasing Ctrl before C turns `Ctrl+C` into a bare `C`
    /// arriving in whatever has focus. A release the OS refuses stays tracked, so a later
    /// emergency stop retries it rather than forgetting the key is down.
    pub fn release_all(&self) {
        let (keys, buttons) = {
            let held = self.held.lock().unwrap();
            let mut keys: Vec<u16> = held.keys.iter().copied().collect();
            // Stable within each group so the order does not depend on hash iteration.
            keys.sort_unstable();
            keys.sort_by_key(|k| is_modifier(*k));
            (keys, held.buttons.iter().copied().collect::<Vec<_>>())
        };

        self.last_release_failed.store(false, Ordering::SeqCst);
        for key in keys {
            marked(|| self.inner.key_up(key));
            self.held.lock().unwrap().keys.remove(&key);
        }
        for button in buttons {
            marked(|| self.inner.mouse_up(button));
            self.held.lock().unwrap().buttons.remove(&button);
        }
    }
}

impl InputInjector for TrackingInjector {
    fn key_down(&self, virtual_key: u16) {
        self.held.lock().unwrap().keys.insert(virtual_key);
        marked(|| self.inner.key_down(virtual_key));
    }

    fn key_up(&self, virtual_key: u16) {
        // Forgotten only after the sink has taken the release, so a sink that panics or refuses
        // leaves the key tracked and a later release_all retries it.
        marked(|| self.inner.key_up(virtual_key));
        self.held.lock().unwrap().keys.remove(&virtual_key);
    }

    fn mouse_down(&self, button: MouseButtonKind) {
        self.held.lock().unwrap().buttons.insert(button);
        marked(|| self.inner.mouse_down(button));
    }

    fn mouse_up(&self, button: MouseButtonKind) {
        marked(|| self.inner.mouse_up(button));
        self.held.lock().unwrap().buttons.remove(&button);
    }

    fn mouse_move(&self, dx: i32, dy: i32) {
        marked(|| self.inner.mouse_move(dx, dy));
    }

    fn wheel(&self, delta: i32, horizontal: bool) {
        marked(|| self.inner.wheel(delta, horizontal));
    }

    fn type_text(&self, text: &str) {
        marked(|| self.inner.type_text(text));
    }
}

/// Records injected events instead of emitting them. Used by the tests and by the UI's
/// "try this binding" preview, which shows what a binding would do without doing it.
#[derive(Debug, Default)]
pub struct RecordingInjector {
    events: Mutex<Vec<String>>,
}

impl RecordingInjector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }

    pub fn take(&self) -> Vec<String> {
        std::mem::take(&mut self.events.lock().unwrap())
    }

    pub fn clear(&self) {
        self.events.lock().unwrap().clear();
    }

    fn add(&self, event: String) {
        self.events.lock().unwrap().push(event);
    }
}

impl InputInjector for RecordingInjector {
    fn key_down(&self, virtual_key: u16) {
        self.add(format!("down:{virtual_key:02X}"));
    }
    fn key_up(&self, virtual_key: u16) {
        self.add(format!("up:{virtual_key:02X}"));
    }
    fn mouse_down(&self, button: MouseButtonKind) {
        self.add(format!("mdown:{button:?}"));
    }
    fn mouse_up(&self, button: MouseButtonKind) {
        self.add(format!("mup:{button:?}"));
    }
    fn mouse_move(&self, dx: i32, dy: i32) {
        self.add(format!("move:{dx},{dy}"));
    }
    fn wheel(&self, delta: i32, horizontal: bool) {
        self.add(format!("wheel:{delta}{}", if horizontal { "h" } else { "" }));
    }
    fn type_text(&self, text: &str) {
        self.add(format!("text:{text}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> (Arc<RecordingInjector>, TrackingInjector) {
        let recorder = Arc::new(RecordingInjector::new());
        let tracking = TrackingInjector::new(Arc::clone(&recorder) as Arc<dyn InputInjector>);
        (recorder, tracking)
    }

    #[test]
    fn a_press_is_tracked_until_its_release_is_accepted() {
        let (recorder, tracking) = tracker();
        tracking.key_down(0x41);
        assert_eq!(tracking.held_keys(), vec![0x41]);
        assert!(tracking.is_holding_anything());

        tracking.key_up(0x41);
        assert!(tracking.held_keys().is_empty());
        assert!(!tracking.is_holding_anything());
        assert_eq!(recorder.events(), vec!["down:41", "up:41"]);
    }

    #[test]
    fn release_all_lets_go_of_non_modifiers_first() {
        // Releasing Ctrl before C turns Ctrl+C into a bare C arriving in whatever has focus.
        let (recorder, tracking) = tracker();
        tracking.key_down(0xA2); // Left Ctrl
        tracking.key_down(0xA0); // Left Shift
        tracking.key_down(0x43); // C
        recorder.clear();

        tracking.release_all();
        let events = recorder.events();
        assert_eq!(events[0], "up:43", "the letter goes first: {events:?}");
        assert!(events[1..].contains(&"up:A0".to_owned()));
        assert!(events[1..].contains(&"up:A2".to_owned()));
        assert!(!tracking.is_holding_anything());
    }

    #[test]
    fn release_all_also_releases_mouse_buttons() {
        let (recorder, tracking) = tracker();
        tracking.mouse_down(MouseButtonKind::Left);
        tracking.key_down(0x41);
        recorder.clear();
        tracking.release_all();

        let events = recorder.events();
        assert!(events.contains(&"up:41".to_owned()), "{events:?}");
        assert!(events.contains(&"mup:Left".to_owned()), "{events:?}");
        assert!(tracking.held_buttons().is_empty());
    }

    #[test]
    fn release_all_on_an_empty_tracker_does_nothing() {
        let (recorder, tracking) = tracker();
        tracking.release_all();
        assert!(recorder.events().is_empty());
        assert!(!tracking.last_release_failed());
    }

    #[test]
    fn a_child_tracker_releases_only_its_own_presses() {
        // One macro stopping must not release a key another binding is holding.
        let (recorder, parent) = tracker();
        let child = parent.child();
        parent.key_down(0xA4); // Alt, held by a binding
        child.key_down(0x09); // Tab, pressed by a macro
        recorder.clear();

        child.release_all();
        assert_eq!(recorder.events(), vec!["up:09"]);
        assert_eq!(parent.held_keys(), vec![0xA4], "the binding still holds Alt");
    }

    #[test]
    fn the_same_key_pressed_twice_is_tracked_once() {
        // Auto-repeat delivers many downs; one release must be enough to clear it.
        let (_, tracking) = tracker();
        tracking.key_down(0x41);
        tracking.key_down(0x41);
        assert_eq!(tracking.held_keys(), vec![0x41]);
        tracking.key_up(0x41);
        assert!(tracking.held_keys().is_empty());
    }

    #[test]
    fn the_injecting_flag_is_set_only_during_an_injection() {
        // An input hook on this thread uses the flag to ignore our own events; leaving it set
        // would make the hook ignore the user's real keystrokes too.
        struct Probe {
            seen: Mutex<Vec<bool>>,
        }
        impl InputInjector for Probe {
            fn key_down(&self, _: u16) {
                self.seen.lock().unwrap().push(is_injecting());
            }
            fn key_up(&self, _: u16) {}
            fn mouse_down(&self, _: MouseButtonKind) {}
            fn mouse_up(&self, _: MouseButtonKind) {}
            fn mouse_move(&self, _: i32, _: i32) {}
            fn wheel(&self, _: i32, _: bool) {}
            fn type_text(&self, _: &str) {}
        }

        assert!(!is_injecting());
        let probe = Arc::new(Probe { seen: Mutex::new(Vec::new()) });
        let tracking = TrackingInjector::new(Arc::clone(&probe) as Arc<dyn InputInjector>);
        tracking.key_down(0x41);
        assert_eq!(probe.seen.lock().unwrap().clone(), vec![true]);
        assert!(!is_injecting(), "the flag must be cleared afterwards");
    }

    #[test]
    fn the_modifier_table_covers_both_sides_and_nothing_else() {
        for vk in [0x10, 0x11, 0x12, 0x5B, 0x5C, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5] {
            assert!(is_modifier(vk), "0x{vk:02X} should be a modifier");
        }
        for vk in [0x41, 0x09, 0x0D, 0x9F, 0xA6, 0x5D] {
            assert!(!is_modifier(vk), "0x{vk:02X} should not be a modifier");
        }
    }

    #[test]
    fn every_event_kind_is_recorded_in_a_readable_form() {
        let recorder = RecordingInjector::new();
        recorder.key_down(0x41);
        recorder.key_up(0x41);
        recorder.mouse_down(MouseButtonKind::XButton1);
        recorder.mouse_up(MouseButtonKind::XButton1);
        recorder.mouse_move(-5, 7);
        recorder.wheel(120, false);
        recorder.wheel(-120, true);
        recorder.type_text("hi");
        assert_eq!(
            recorder.take(),
            vec![
                "down:41",
                "up:41",
                "mdown:XButton1",
                "mup:XButton1",
                "move:-5,7",
                "wheel:120",
                "wheel:-120h",
                "text:hi",
            ]
        );
        assert!(recorder.events().is_empty(), "take drains");
    }
}
