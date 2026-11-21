//! Macro recorder: the port of `MacroRecorder` from `Actions/MacroRuntime.cs`.
//!
//! Turns raw hook events into [`MacroStep`]s. Two correctness properties come straight from the
//! C# and are what the tests below pin down:
//!
//! * **auto-repeat downs are collapsed.** Holding a key makes Windows deliver a down every
//!   repeat interval with no intervening up. Recording those would turn "hold A for a second"
//!   into thirty `KeyDown A` steps.
//! * **the recording is balanced on stop.** A key-up whose down happened *before* recording
//!   started is dropped (its down is not in the list, so replaying the up would release a key the
//!   macro never pressed), and a down still held when recording stops gets a synthetic up
//!   appended. Without this, playing back the macro leaves a key latched in Windows.
//!
//! Recorded content is never written to [`crate::diagnostics`] - see the privacy note there.

use std::time::Instant;

use crate::action::{MacroDefinition, MacroStep, MouseButtonKind};

/// Builds macro steps from recorded raw events, with optional delay capture.
#[derive(Debug)]
pub struct MacroRecorder {
    started: Option<Instant>,
    steps: Vec<MacroStep>,
    /// Milliseconds since `started` at which the previous step was recorded.
    last: u64,
    recording: bool,
    pub record_delays: bool,
    pub record_mouse_buttons: bool,
    pub record_mouse_movement: bool,
    /// Recording stops accepting events at this many steps.
    pub max_steps: usize,
}

impl Default for MacroRecorder {
    fn default() -> Self {
        Self {
            started: None,
            steps: Vec::new(),
            last: 0,
            recording: false,
            record_delays: true,
            record_mouse_buttons: true,
            // Off by default: pointer movement is relative and recording it makes almost every
            // macro feel broken, because the cursor drifts from wherever it happens to be.
            record_mouse_movement: false,
            max_steps: MacroDefinition::MAX_STEPS,
        }
    }
}

impl MacroRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_recording(&self) -> bool {
        self.recording
    }

    /// Steps captured so far, so the UI can show the list growing live.
    pub fn steps(&self) -> &[MacroStep] {
        &self.steps
    }

    pub fn start(&mut self) {
        self.steps.clear();
        self.started = Some(Instant::now());
        self.last = 0;
        self.recording = true;
    }

    /// Stops and returns the balanced step list. See the module note on what "balanced" means.
    pub fn stop(&mut self) -> Vec<MacroStep> {
        self.recording = false;
        self.started = None;
        let captured = std::mem::take(&mut self.steps);

        // `Vec` rather than a set: the order keys were pressed in is the order they must be
        // released in at the end, and a set would make the synthetic ups arbitrary.
        let mut held: Vec<u16> = Vec::new();
        let mut result: Vec<MacroStep> = Vec::with_capacity(captured.len());
        for step in captured {
            match step {
                MacroStep::KeyDown { virtual_key } => {
                    if !held.contains(&virtual_key) {
                        held.push(virtual_key);
                    }
                }
                MacroStep::KeyUp { virtual_key } => match held.iter().position(|k| *k == virtual_key) {
                    Some(index) => {
                        held.remove(index);
                    }
                    // An up with no recorded down: its down predates the recording.
                    None => continue,
                },
                _ => {}
            }
            result.push(step);
        }

        // Release in reverse press order, so a recording that ends mid-chord unwinds the way a
        // human would: Shift pressed first is released last.
        for virtual_key in held.into_iter().rev() {
            result.push(MacroStep::KeyUp { virtual_key });
        }
        result
    }

    /// Stops and throws the recording away.
    pub fn cancel(&mut self) {
        self.recording = false;
        self.started = None;
        self.steps.clear();
    }

    fn add(&mut self, step: MacroStep) {
        if !self.recording || self.steps.len() >= self.max_steps {
            return;
        }
        let now = self
            .started
            .map(|start| start.elapsed().as_millis() as u64)
            .unwrap_or(0);
        if self.record_delays && !self.steps.is_empty() && now > self.last {
            let gap = (now - self.last).min(MacroDefinition::MAX_DELAY_MILLISECONDS as u64);
            self.steps.push(MacroStep::Delay { milliseconds: gap as u32 });
        }
        self.last = now;
        self.steps.push(step);
    }

    pub fn on_key(&mut self, virtual_key: u16, down: bool) {
        if down && self.is_held(virtual_key) {
            return; // auto-repeat
        }
        self.add(if down {
            MacroStep::KeyDown { virtual_key }
        } else {
            MacroStep::KeyUp { virtual_key }
        });
    }

    /// Whether the last recorded event for this key was a down.
    ///
    /// The C# wrote this as a LINQ `Any` plus a `LastOrDefault` over the whole list for every
    /// keystroke - quadratic in the step count, which at the 5000-step limit is millions of
    /// comparisons inside a low-level hook. A reverse scan that stops at the first mention of
    /// the key gives the same answer.
    fn is_held(&self, virtual_key: u16) -> bool {
        self.steps.iter().rev().find_map(|step| match step {
            MacroStep::KeyDown { virtual_key: vk } if *vk == virtual_key => Some(true),
            MacroStep::KeyUp { virtual_key: vk } if *vk == virtual_key => Some(false),
            _ => None,
        }) == Some(true)
    }

    pub fn on_mouse_button(&mut self, button: MouseButtonKind, down: bool) {
        if self.record_mouse_buttons {
            self.add(if down {
                MacroStep::MouseDown { button }
            } else {
                MacroStep::MouseUp { button }
            });
        }
    }

    pub fn on_wheel(&mut self, delta: i32, horizontal: bool) {
        if self.record_mouse_buttons {
            self.add(MacroStep::Wheel { delta, horizontal });
        }
    }

    pub fn on_mouse_move(&mut self, dx: i32, dy: i32) {
        if self.record_mouse_movement && (dx != 0 || dy != 0) {
            self.add(MacroStep::MouseMove { dx, dy });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorder() -> MacroRecorder {
        let mut r = MacroRecorder::new();
        r.record_delays = false; // delays are timing-dependent; tested separately
        r.start();
        r
    }

    #[test]
    fn events_before_start_are_ignored() {
        let mut r = MacroRecorder::new();
        r.on_key(0x41, true);
        assert!(r.steps().is_empty());
        assert!(!r.is_recording());
    }

    #[test]
    fn a_simple_keystroke_records_down_then_up() {
        let mut r = recorder();
        r.on_key(0x41, true);
        r.on_key(0x41, false);
        assert_eq!(
            r.stop(),
            vec![
                MacroStep::KeyDown { virtual_key: 0x41 },
                MacroStep::KeyUp { virtual_key: 0x41 }
            ]
        );
    }

    #[test]
    fn auto_repeat_downs_are_collapsed() {
        let mut r = recorder();
        for _ in 0..30 {
            r.on_key(0x41, true);
        }
        r.on_key(0x41, false);
        r.on_key(0x41, true); // a genuine second press after the release
        assert_eq!(
            r.stop(),
            vec![
                MacroStep::KeyDown { virtual_key: 0x41 },
                MacroStep::KeyUp { virtual_key: 0x41 },
                MacroStep::KeyDown { virtual_key: 0x41 },
                // closed by stop()
                MacroStep::KeyUp { virtual_key: 0x41 },
            ]
        );
    }

    #[test]
    fn an_up_without_a_recorded_down_is_dropped() {
        // The user was already holding Shift when they hit record.
        let mut r = recorder();
        r.on_key(0xA0, false);
        r.on_key(0x41, true);
        r.on_key(0x41, false);
        assert_eq!(
            r.stop(),
            vec![
                MacroStep::KeyDown { virtual_key: 0x41 },
                MacroStep::KeyUp { virtual_key: 0x41 }
            ]
        );
    }

    #[test]
    fn keys_still_held_at_stop_are_released_in_reverse_press_order() {
        let mut r = recorder();
        r.on_key(0xA0, true); // Shift
        r.on_key(0x11, true); // Ctrl
        r.on_key(0x41, true); // A
        assert_eq!(
            r.stop(),
            vec![
                MacroStep::KeyDown { virtual_key: 0xA0 },
                MacroStep::KeyDown { virtual_key: 0x11 },
                MacroStep::KeyDown { virtual_key: 0x41 },
                MacroStep::KeyUp { virtual_key: 0x41 },
                MacroStep::KeyUp { virtual_key: 0x11 },
                MacroStep::KeyUp { virtual_key: 0xA0 },
            ]
        );
    }

    #[test]
    fn a_recording_never_leaves_an_unbalanced_key() {
        let mut r = recorder();
        // A deliberately nasty sequence: stray ups, repeats, interleaved chords.
        for (vk, down) in [
            (0x41u16, true),
            (0x42, false),
            (0x41, true),
            (0xA0, true),
            (0x41, false),
            (0x43, true),
            (0x43, true),
        ] {
            r.on_key(vk, down);
        }
        let mut macro_definition = MacroDefinition::new("recorded");
        macro_definition.steps = r.stop();
        assert!(
            macro_definition.unbalanced_keys().is_empty(),
            "{:?}",
            macro_definition.steps
        );
    }

    #[test]
    fn mouse_buttons_and_wheel_obey_their_switch() {
        let mut r = recorder();
        r.record_mouse_buttons = false;
        r.on_mouse_button(MouseButtonKind::Left, true);
        r.on_wheel(120, false);
        assert!(r.steps().is_empty());

        r.record_mouse_buttons = true;
        r.on_mouse_button(MouseButtonKind::Left, true);
        r.on_mouse_button(MouseButtonKind::Left, false);
        r.on_wheel(-120, true);
        assert_eq!(
            r.stop(),
            vec![
                MacroStep::MouseDown { button: MouseButtonKind::Left },
                MacroStep::MouseUp { button: MouseButtonKind::Left },
                MacroStep::Wheel { delta: -120, horizontal: true },
            ]
        );
    }

    #[test]
    fn movement_is_off_by_default_and_zero_deltas_never_record() {
        let mut r = recorder();
        r.on_mouse_move(5, 5);
        assert!(r.steps().is_empty(), "movement must be opt-in");
        r.record_mouse_movement = true;
        r.on_mouse_move(0, 0);
        assert!(r.steps().is_empty(), "a zero move is not an event");
        r.on_mouse_move(3, -4);
        assert_eq!(r.steps(), [MacroStep::MouseMove { dx: 3, dy: -4 }]);
    }

    #[test]
    fn delays_are_inserted_between_steps_but_never_before_the_first() {
        let mut r = MacroRecorder::new();
        r.start();
        r.on_key(0x41, true);
        std::thread::sleep(std::time::Duration::from_millis(12));
        r.on_key(0x41, false);
        let steps = r.stop();
        assert!(matches!(steps[0], MacroStep::KeyDown { .. }), "{steps:?}");
        match steps[1] {
            MacroStep::Delay { milliseconds } => assert!(
                milliseconds >= 10,
                "expected the measured gap, got {milliseconds}"
            ),
            ref other => panic!("expected a delay, got {other:?}"),
        }
        assert!(matches!(steps[2], MacroStep::KeyUp { .. }));
    }

    #[test]
    fn recording_stops_accepting_events_at_the_step_limit() {
        let mut r = recorder();
        r.max_steps = 4;
        for _ in 0..50 {
            r.on_mouse_button(MouseButtonKind::Left, true);
            r.on_mouse_button(MouseButtonKind::Left, false);
        }
        assert_eq!(r.steps().len(), 4);
    }

    #[test]
    fn cancel_discards_everything_and_start_resets() {
        let mut r = recorder();
        r.on_key(0x41, true);
        r.cancel();
        assert!(!r.is_recording());
        assert!(r.steps().is_empty());
        r.start();
        r.on_key(0x42, true);
        r.on_key(0x42, false);
        assert_eq!(r.stop().len(), 2);
    }

    #[test]
    fn stop_leaves_the_recorder_reusable() {
        let mut r = recorder();
        r.on_key(0x41, true);
        r.on_key(0x41, false);
        assert_eq!(r.stop().len(), 2);
        assert!(r.steps().is_empty(), "stop must hand over the steps, not copy them");
        r.start();
        r.on_key(0x42, true);
        assert_eq!(r.stop(), vec![
            MacroStep::KeyDown { virtual_key: 0x42 },
            MacroStep::KeyUp { virtual_key: 0x42 }
        ]);
    }
}
