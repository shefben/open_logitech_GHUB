//! A recording platform for tests.
//!
//! The orchestrator in `ghub-core` needs a [`PlatformServices`] and an
//! [`InputInjector`](ghub_core::injection::InputInjector) to do anything at all. Driving it
//! through the real one would move the user's cursor, type into whatever window has focus, and
//! make the outcome depend on which application is open - so the service tests drive this
//! instead and assert on the recorded calls.
//!
//! It is in the shipped crate rather than behind `#[cfg(test)]` because `ghub-core`'s tests need
//! it too, and a `dev-dependency` cycle between the two crates is not expressible.

use std::sync::Mutex;

use ghub_core::injection::InputInjector;
use ghub_core::resolver::ForegroundApp;
use ghub_core::MouseButtonKind;

use crate::PlatformServices;

/// One recorded host interaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformCall {
    KeyDown(u16),
    KeyUp(u16),
    MouseDown(MouseButtonKind),
    MouseUp(MouseButtonKind),
    MouseMove { dx: i32, dy: i32 },
    Wheel { delta: i32, horizontal: bool },
    TypeText(String),
    Launch { target: String, arguments: Option<String> },
}

/// Records everything, answers queries from fields the test sets.
#[derive(Debug, Default)]
pub struct StubPlatform {
    calls: Mutex<Vec<PlatformCall>>,
    foreground: Mutex<Option<ForegroundApp>>,
    held: Mutex<Vec<u16>>,
    /// When set, [`PlatformServices::launch`] fails with this message instead of recording.
    launch_error: Mutex<Option<String>>,
}

impl StubPlatform {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn calls(&self) -> Vec<PlatformCall> {
        self.calls.lock().map(|calls| calls.clone()).unwrap_or_default()
    }

    pub fn take_calls(&self) -> Vec<PlatformCall> {
        self.calls.lock().map(|mut calls| std::mem::take(&mut *calls)).unwrap_or_default()
    }

    pub fn clear(&self) {
        if let Ok(mut calls) = self.calls.lock() {
            calls.clear();
        }
    }

    pub fn set_foreground(&self, app: Option<ForegroundApp>) {
        if let Ok(mut slot) = self.foreground.lock() {
            *slot = app;
        }
    }

    pub fn set_held_keys(&self, keys: Vec<u16>) {
        if let Ok(mut slot) = self.held.lock() {
            *slot = keys;
        }
    }

    pub fn fail_launch_with(&self, message: impl Into<String>) {
        if let Ok(mut slot) = self.launch_error.lock() {
            *slot = Some(message.into());
        }
    }

    /// Keys this stub believes are still pressed, derived from the recorded calls.
    ///
    /// The point of the whole stub: a test can assert that a macro left nothing held, which is
    /// the property that matters and the one that is invisible when driving the real injector.
    pub fn still_pressed(&self) -> Vec<u16> {
        let mut held: Vec<u16> = Vec::new();
        for call in self.calls() {
            match call {
                PlatformCall::KeyDown(vk) => {
                    if !held.contains(&vk) {
                        held.push(vk);
                    }
                }
                PlatformCall::KeyUp(vk) => held.retain(|k| *k != vk),
                _ => {}
            }
        }
        held
    }

    /// Mouse buttons still down, by the same argument as [`Self::still_pressed`].
    pub fn still_down(&self) -> Vec<MouseButtonKind> {
        let mut held: Vec<MouseButtonKind> = Vec::new();
        for call in self.calls() {
            match call {
                PlatformCall::MouseDown(button) => {
                    if !held.contains(&button) {
                        held.push(button);
                    }
                }
                PlatformCall::MouseUp(button) => held.retain(|b| *b != button),
                _ => {}
            }
        }
        held
    }

    fn record(&self, call: PlatformCall) {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(call);
        }
    }
}

impl InputInjector for StubPlatform {
    fn key_down(&self, virtual_key: u16) {
        self.record(PlatformCall::KeyDown(virtual_key));
    }

    fn key_up(&self, virtual_key: u16) {
        self.record(PlatformCall::KeyUp(virtual_key));
    }

    fn mouse_down(&self, button: MouseButtonKind) {
        self.record(PlatformCall::MouseDown(button));
    }

    fn mouse_up(&self, button: MouseButtonKind) {
        self.record(PlatformCall::MouseUp(button));
    }

    fn mouse_move(&self, dx: i32, dy: i32) {
        self.record(PlatformCall::MouseMove { dx, dy });
    }

    fn wheel(&self, delta: i32, horizontal: bool) {
        self.record(PlatformCall::Wheel { delta, horizontal });
    }

    fn type_text(&self, text: &str) {
        self.record(PlatformCall::TypeText(text.to_owned()));
    }
}

impl PlatformServices for StubPlatform {
    fn foreground_app(&self) -> Option<ForegroundApp> {
        self.foreground.lock().ok().and_then(|app| app.clone())
    }

    fn is_key_held(&self, virtual_key: u16) -> bool {
        self.held.lock().map(|held| held.contains(&virtual_key)).unwrap_or(false)
    }

    fn launch(&self, target: &str, arguments: Option<&str>) -> Result<(), String> {
        if let Some(error) = self.launch_error.lock().ok().and_then(|e| e.clone()) {
            return Err(error);
        }
        self.record(PlatformCall::Launch {
            target: target.to_owned(),
            arguments: arguments.map(str::to_owned),
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injection_is_recorded_in_order() {
        let platform = StubPlatform::new();
        platform.key_down(0xA0);
        platform.key_down(0x41);
        platform.key_up(0x41);
        platform.type_text("hi");
        platform.wheel(-120, true);
        assert_eq!(
            platform.calls(),
            vec![
                PlatformCall::KeyDown(0xA0),
                PlatformCall::KeyDown(0x41),
                PlatformCall::KeyUp(0x41),
                PlatformCall::TypeText("hi".to_owned()),
                PlatformCall::Wheel { delta: -120, horizontal: true },
            ]
        );
    }

    #[test]
    fn leftover_presses_are_visible() {
        let platform = StubPlatform::new();
        platform.key_down(0xA0);
        platform.key_down(0x41);
        platform.key_up(0x41);
        platform.mouse_down(MouseButtonKind::Left);
        assert_eq!(platform.still_pressed(), [0xA0]);
        assert_eq!(platform.still_down(), [MouseButtonKind::Left]);
        platform.key_up(0xA0);
        platform.mouse_up(MouseButtonKind::Left);
        assert!(platform.still_pressed().is_empty());
        assert!(platform.still_down().is_empty());
    }

    #[test]
    fn queries_come_from_what_the_test_set() {
        let platform = StubPlatform::new();
        assert_eq!(platform.foreground_app(), None);
        assert!(!platform.is_key_held(0xA0));

        platform.set_foreground(Some(ForegroundApp::new("C:\\games\\game.exe", "Game")));
        platform.set_held_keys(vec![0xA0]);
        assert_eq!(platform.foreground_app().unwrap().executable_name(), Some("game.exe"));
        assert!(platform.is_key_held(0xA0));
        assert!(!platform.is_key_held(0x41));
    }

    #[test]
    fn launching_records_or_fails_on_demand() {
        let platform = StubPlatform::new();
        assert!(platform.launch("notepad.exe", Some("x.txt")).is_ok());
        assert_eq!(
            platform.take_calls(),
            vec![PlatformCall::Launch {
                target: "notepad.exe".to_owned(),
                arguments: Some("x.txt".to_owned())
            }]
        );
        platform.fail_launch_with("access was denied");
        assert_eq!(platform.launch("notepad.exe", None), Err("access was denied".to_owned()));
        assert!(platform.calls().is_empty(), "a failed launch records nothing");
    }

    #[test]
    fn take_calls_drains_and_clear_empties() {
        let platform = StubPlatform::new();
        platform.key_down(0x41);
        assert_eq!(platform.take_calls().len(), 1);
        assert!(platform.calls().is_empty());
        platform.key_down(0x42);
        platform.clear();
        assert!(platform.calls().is_empty());
    }
}
