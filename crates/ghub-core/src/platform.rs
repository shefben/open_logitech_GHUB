//! The contract the host operating system must satisfy.
//!
//! These three items live here, in `ghub-core`, rather than in `ghub-platform`, because the
//! orchestrator in [`crate::service`] both *calls* [`PlatformServices`] and *implements*
//! [`HookSink`]. `ghub-platform` depends on this crate for the injector trait and the profile
//! model, so the dependency can only run one way: core defines the contract, platform satisfies
//! it. `ghub-platform` re-exports all three, so a caller never has to know which crate they came
//! from.

use crate::resolver::ForegroundApp;
use crate::MouseButtonKind;

/// Host queries the action runtime and profile resolver need.
///
/// Separate from [`crate::injection::InputInjector`] because a test double usually wants to
/// record injection while answering queries for real, or the reverse.
pub trait PlatformServices: Send + Sync {
    /// The focused window as an executable path and title, or `None` when nothing has focus.
    fn foreground_app(&self) -> Option<ForegroundApp>;

    /// Whether the user is physically holding a key.
    fn is_key_held(&self, virtual_key: u16) -> bool;

    /// Starts a program, document or URL with the user's own token. Never elevates.
    fn launch(&self, target: &str, arguments: Option<&str>) -> Result<(), String>;
}

/// What the hook layer reports back to the orchestrator.
///
/// Every method is called from a low-level hook procedure and must return in well under
/// Windows' `LowLevelHooksTimeout`, or the OS quietly removes the hook. The two that cannot be
/// deferred are [`HookSink::is_game_mode_blocked`] and [`HookSink::on_mouse_button`], because
/// their answers decide whether the event is swallowed; both are pure lookups against the active
/// profile. Everything expensive is dispatched to a thread by the caller.
pub trait HookSink: Send + Sync {
    /// A physical key event, after our own injection has been filtered out.
    fn on_keyboard_key(&self, virtual_key: u16, down: bool, extended: bool);

    /// Whether game mode should swallow this key. Must be a lookup, not a device round trip.
    fn is_game_mode_blocked(&self, virtual_key: u16) -> bool;

    /// The device key that mouse buttons are attributed to, or `None` when no mouse is managed.
    fn primary_mouse_key(&self) -> Option<String>;

    /// A physical mouse button. Returns whether a binding handled it, which decides suppression.
    fn on_mouse_button(&self, device_key: &str, button: MouseButtonKind, down: bool) -> bool;

    /// A wheel notch, 120 per detent, positive away from the user. For macro recording.
    fn on_wheel(&self, delta: i16);

    /// Ctrl+Shift+Pause. Stops every running macro and releases everything it holds.
    fn on_emergency_stop(&self);

    /// The foreground window changed. Called on a worker thread, not the hook thread.
    fn on_foreground_changed(&self, app: Option<ForegroundApp>);

    /// Called once, after installation, with what actually succeeded.
    fn on_hooks_installed(&self, status: HookStatus) {
        let _ = status;
    }
}

/// Which hooks are installed. Shown on the diagnostics page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HookStatus {
    pub keyboard: bool,
    pub mouse: bool,
    pub foreground: bool,
    /// Whether the keyboard hook has ever been called. Distinguishes "the OS never calls us"
    /// from "we were called and filtered the event out".
    pub keyboard_events_seen: bool,
    /// `GetLastError` straight after installation. Zero when everything succeeded.
    pub last_error: u32,
}

impl HookStatus {
    pub fn all_installed(&self) -> bool {
        self.keyboard && self.mouse && self.foreground
    }

    /// One line for the diagnostics page.
    pub fn describe(&self) -> String {
        if self.all_installed() {
            let seen = if self.keyboard_events_seen { "events received" } else { "no events yet" };
            return format!("keyboard, mouse and foreground hooks installed ({seen})");
        }
        let mut missing = Vec::new();
        for (installed, name) in [
            (self.keyboard, "keyboard"),
            (self.mouse, "mouse"),
            (self.foreground, "foreground"),
        ] {
            if !installed {
                missing.push(name);
            }
        }
        if missing.len() == 3 {
            return format!(
                "no input hooks installed (last error {}); macros, game mode and press reactions \
                 will not work",
                self.last_error
            );
        }
        format!(
            "{} hook(s) failed to install (last error {}); the features that need them are off",
            missing.join(", "),
            self.last_error
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fully_installed_status_says_so_and_mentions_whether_events_arrived() {
        let status = HookStatus {
            keyboard: true,
            mouse: true,
            foreground: true,
            keyboard_events_seen: false,
            last_error: 0,
        };
        assert!(status.all_installed());
        assert!(status.describe().contains("no events yet"), "{}", status.describe());

        let live = HookStatus { keyboard_events_seen: true, ..status };
        assert!(live.describe().contains("events received"));
    }

    #[test]
    fn a_total_failure_says_which_features_are_lost() {
        let status = HookStatus { last_error: 5, ..Default::default() };
        let text = status.describe();
        assert!(!status.all_installed());
        assert!(text.contains("no input hooks installed"), "{text}");
        assert!(text.contains("last error 5"), "{text}");
        assert!(text.contains("macros"), "{text}");
    }

    #[test]
    fn a_partial_failure_names_the_missing_hooks() {
        let status = HookStatus {
            keyboard: true,
            mouse: false,
            foreground: false,
            keyboard_events_seen: true,
            last_error: 1400,
        };
        let text = status.describe();
        assert!(text.contains("mouse, foreground"), "{text}");
        assert!(text.contains("1400"), "{text}");
    }
}
