//! Runs the software effects against a device. Port of
//! `Artemis.Peripherals/Lighting/KeyAnimationEngine.cs`.
//!
//! One thread per animated device renders frames and hands them to a [`FrameSink`], which in the
//! real application is the device's 0x8081 per-key feature. That feature diffs frames, so a
//! still-looking effect costs almost nothing on the wire.
//!
//! The loop paces itself: a frame that takes longer to write than its slot makes the *next* frame
//! later rather than queueing behind it, so a slow link degrades to a lower frame rate instead of
//! lagging further and further behind.
//!
//! The sink is a trait rather than a device handle so the engine can be driven and asserted on
//! without hardware. The C# took a `Func<string, IPerKeyLightingFeature?>` for the same reason,
//! but then also owned the thread pool task; here the only thing the engine owns is the thread.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::profile::AnimationSettings;
use crate::reactions::{Frame, ReactionEngine};
use crate::{effects, profile};

/// Where rendered frames go.
pub trait FrameSink: Send + Sync {
    /// Writes one frame.
    ///
    /// Returning `false` stops the loop. That is the normal way an animation ends: lighting was
    /// handed back to the firmware, or the device went away. An error the sink can recover from
    /// should be logged and `true` returned, so one dropped packet does not kill the effect.
    fn write_frame(&self, device_key: &str, frame: &Frame) -> bool;
}

/// A sink that records frames, for tests and for the UI preview.
#[derive(Debug, Default)]
pub struct RecordingSink {
    frames: Mutex<Vec<(String, Frame)>>,
    /// When set, the sink stops accepting frames, which ends the loop.
    closed: AtomicBool,
}

impl RecordingSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    pub fn frame_count(&self) -> usize {
        self.frames.lock().unwrap().len()
    }

    pub fn last_frame(&self) -> Option<(String, Frame)> {
        self.frames.lock().unwrap().last().cloned()
    }

    pub fn take(&self) -> Vec<(String, Frame)> {
        std::mem::take(&mut self.frames.lock().unwrap())
    }
}

impl FrameSink for RecordingSink {
    fn write_frame(&self, device_key: &str, frame: &Frame) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        self.frames.lock().unwrap().push((device_key.to_owned(), frame.clone()));
        true
    }
}

struct Running {
    /// Settings and brightness, flattened. Equal signatures mean nothing has to restart.
    signature: String,
    state: Arc<effects::EffectState>,
    clock: Instant,
    stop: Arc<AtomicBool>,
    frames: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
}

impl Running {
    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            // The loop checks the flag once per frame slot, so this returns within one frame.
            let _ = handle.join();
        }
    }
}

pub struct AnimationEngine {
    devices: Mutex<HashMap<String, Running>>,
    sink: Arc<dyn FrameSink>,
    reactions: Arc<Mutex<ReactionEngine>>,
    frames_rendered: Arc<AtomicU64>,
    /// Screen and audio for the two live presets, set by the host.
    live: Mutex<Option<Arc<dyn effects::LiveInput>>>,
}

impl AnimationEngine {
    /// Gives the live presets (Screen Sampler, Audio Visualizer) their input. Effects already
    /// running keep what they started with; the next configure picks this up.
    pub fn set_live_input(&self, live: Option<Arc<dyn effects::LiveInput>>) {
        *self.live.lock().unwrap() = live;
    }

    pub fn new(sink: Arc<dyn FrameSink>, reactions: Arc<Mutex<ReactionEngine>>) -> Self {
        Self {
            devices: Mutex::new(HashMap::new()),
            sink,
            reactions,
            frames_rendered: Arc::new(AtomicU64::new(0)),
            live: Mutex::new(None),
        }
    }

    /// True while at least one device is being animated.
    pub fn is_active(&self) -> bool {
        !self.devices.lock().unwrap().is_empty()
    }

    /// Frames rendered since the engine started, for the UI's "it is running" readout.
    pub fn frames_rendered(&self) -> u64 {
        self.frames_rendered.load(Ordering::Relaxed)
    }

    pub fn frames_for(&self, device_key: &str) -> u64 {
        self.devices
            .lock()
            .unwrap()
            .get(device_key)
            .map(|r| r.frames.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Starts or updates the animation on a device.
    ///
    /// Settings that did not change leave the running loop alone, so saving an unrelated profile
    /// setting does not restart the effect from frame zero - which on a slow effect looks like a
    /// visible stutter every time the user touches anything.
    pub fn configure(&self, device_key: &str, settings: &AnimationSettings, brightness: f64) {
        let signature = signature_of(settings, brightness);
        let mut devices = self.devices.lock().unwrap();
        if let Some(running) = devices.get_mut(device_key) {
            if running.signature == signature {
                return;
            }
            running.stop();
            devices.remove(device_key);
        }

        // Only the live presets get the input, so no other effect can start a capture.
        let live = if effects::definition(&settings.effect).is_live() {
            self.live.lock().unwrap().clone()
        } else {
            None
        };
        let state = Arc::new(effects::EffectState::with_live(live));
        let stop = Arc::new(AtomicBool::new(false));
        let frames = Arc::new(AtomicU64::new(0));

        let handle = {
            let device_key = device_key.to_owned();
            let settings = settings.clone();
            let state = Arc::clone(&state);
            let stop = Arc::clone(&stop);
            let frames = Arc::clone(&frames);
            let total = Arc::clone(&self.frames_rendered);
            let sink = Arc::clone(&self.sink);
            let reactions = Arc::clone(&self.reactions);
            std::thread::Builder::new()
                .name(format!("ghub-anim-{device_key}"))
                .spawn(move || {
                    run_loop(
                        &device_key,
                        &settings,
                        brightness,
                        &state,
                        &stop,
                        &frames,
                        &total,
                        sink.as_ref(),
                        &reactions,
                    )
                })
                .expect("spawn animation thread")
        };

        devices.insert(
            device_key.to_owned(),
            Running { signature, state, clock: Instant::now(), stop, frames, handle: Some(handle) },
        );
    }

    /// Stops the animation on a device: mode changed, lighting handed back, or device gone.
    pub fn forget(&self, device_key: &str) {
        let mut running = self.devices.lock().unwrap().remove(device_key);
        // Joined outside the map lock, so a loop finishing its frame cannot deadlock against a
        // configure on another device.
        if let Some(running) = running.as_mut() {
            running.stop();
        }
    }

    pub fn stop_all(&self) {
        let mut all: Vec<Running> = self.devices.lock().unwrap().drain().map(|(_, r)| r).collect();
        for running in &mut all {
            running.stop();
        }
    }

    /// Feeds a key press to the effects that answer them. Called from the input thread; takes
    /// only the device map lock and never blocks on a frame.
    pub fn on_key(&self, key_id: &str, down: bool) {
        if !down {
            return;
        }
        for running in self.devices.lock().unwrap().values() {
            running.state.press(key_id, running.clock.elapsed().as_secs_f64());
        }
    }
}

impl Drop for AnimationEngine {
    fn drop(&mut self) {
        self.stop_all();
    }
}

/// Settings plus brightness as one comparable string.
///
/// Serialising rather than deriving `PartialEq` on the settings: brightness is a float that
/// arrives from a slider, and comparing it exactly would restart the effect on a change far too
/// small to see. Three decimals is finer than the eye and finer than the 5-bit quantiser.
fn signature_of(settings: &AnimationSettings, brightness: f64) -> String {
    let json = serde_json::to_string(settings).unwrap_or_default();
    format!("{json}|{brightness:.3}")
}

#[allow(clippy::too_many_arguments)]
fn run_loop(
    device_key: &str,
    settings: &AnimationSettings,
    brightness: f64,
    state: &effects::EffectState,
    stop: &AtomicBool,
    frames: &AtomicU64,
    total: &AtomicU64,
    sink: &dyn FrameSink,
    reactions: &Mutex<ReactionEngine>,
) {
    let fps = settings.frames_per_second.clamp(1, 60);
    let slot = Duration::from_secs_f64(1.0 / fps as f64);
    let clock = Instant::now();

    while !stop.load(Ordering::SeqCst) {
        let started_at = Instant::now();
        let seconds = clock.elapsed().as_secs_f64();

        state.advance(seconds);
        let mut frame = effects::render(settings, seconds, state, brightness);

        // Reactions win over the animation, and this frame becomes what a released key returns
        // to, so a key let go mid-effect rejoins the animation instead of showing a stale colour.
        reactions.lock().unwrap().overlay(device_key, &mut frame);

        if !sink.write_frame(device_key, &frame) {
            return;
        }
        frames.fetch_add(1, Ordering::Relaxed);
        total.fetch_add(1, Ordering::Relaxed);

        // Sleep only what is left of the slot. An over-running frame yields briefly rather than
        // spinning, so a device that cannot keep up still lets other threads run.
        match slot.checked_sub(started_at.elapsed()) {
            Some(left) if !left.is_zero() => std::thread::sleep(left),
            _ => std::thread::yield_now(),
        }
    }
}

/// Renders one frame without starting a loop, for the UI preview and for a one-shot write.
pub fn render_once(
    settings: &AnimationSettings,
    seconds: f64,
    brightness: f64,
) -> Frame {
    let state = effects::EffectState::new();
    state.advance(seconds);
    effects::render(settings, seconds, &state, brightness)
}

/// A static per-key frame from a profile's named colours.
///
/// Keys the profile does not name take `default_color`. Painting only the named keys would leave
/// the rest showing whatever was there before, which looks like a half-applied profile.
pub fn static_frame(
    static_colors: &HashMap<String, String>,
    default_color: &str,
    brightness: f64,
) -> Frame {
    let fallback = profile::parse_color(default_color).unwrap_or(ghub_hidpp::LedColor::BLACK);
    let mut frame: Frame = crate::layout::keys()
        .iter()
        .filter(|k| k.led_id != 0)
        .map(|k| (k.led_id, effects::quantize(effects::scale(fallback, brightness))))
        .collect();

    for (key, hex) in static_colors {
        let Some(color) = profile::parse_color(hex) else { continue };
        let color = effects::quantize(effects::scale(color, brightness));
        // Two key forms, matching what the profile file uses: `key:Escape` for a keyboard and
        // `zone:0` for a mouse. A zone id is not a per-key LED, so it is not in this frame.
        if let Some(id) = key.strip_prefix("key:") {
            if let Some(definition) = crate::layout::by_id(id) {
                if definition.led_id != 0 {
                    frame.insert(definition.led_id, color);
                }
            }
        }
    }
    frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{AnimationDirection, KeyReaction, KeyReactionMode};
    use ghub_hidpp::LedColor;

    const DEVICE: &str = "logitech-hidpp:u1";

    fn fast(effect: &str) -> AnimationSettings {
        AnimationSettings {
            effect: effect.to_owned(),
            frames_per_second: 60,
            ..AnimationSettings::default()
        }
    }

    /// Waits for a condition, failing rather than hanging if it never happens.
    fn wait_for(what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if condition() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for {what}");
    }

    fn engine(sink: Arc<RecordingSink>) -> AnimationEngine {
        AnimationEngine::new(sink, Arc::new(Mutex::new(ReactionEngine::new())))
    }

    #[test]
    fn configuring_a_device_starts_rendering_frames() {
        let sink = Arc::new(RecordingSink::new());
        let engine = engine(Arc::clone(&sink));
        assert!(!engine.is_active());

        engine.configure(DEVICE, &fast("wave"), 1.0);
        assert!(engine.is_active());
        wait_for("the first frames", || sink.frame_count() >= 3);

        let (device, frame) = sink.last_frame().unwrap();
        assert_eq!(device, DEVICE);
        assert_eq!(frame.len(), crate::layout::led_ids().len());
        assert!(engine.frames_rendered() >= 3);
        assert!(engine.frames_for(DEVICE) >= 3);

        engine.forget(DEVICE);
        assert!(!engine.is_active());
        let after = sink.frame_count();
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(sink.frame_count(), after, "no frames after forget");
    }

    #[test]
    fn reconfiguring_with_the_same_settings_does_not_restart_the_effect() {
        let sink = Arc::new(RecordingSink::new());
        let engine = engine(Arc::clone(&sink));
        engine.configure(DEVICE, &fast("wave"), 1.0);
        wait_for("frames", || sink.frame_count() >= 2);
        let frames_before = engine.frames_for(DEVICE);

        // Saving an unrelated setting re-applies the same animation. The counter must keep
        // climbing from where it was, not reset.
        engine.configure(DEVICE, &fast("wave"), 1.0);
        wait_for("more frames", || engine.frames_for(DEVICE) > frames_before);
        engine.stop_all();
    }

    #[test]
    fn changing_any_setting_restarts_the_loop() {
        let sink = Arc::new(RecordingSink::new());
        let engine = engine(Arc::clone(&sink));
        engine.configure(DEVICE, &fast("wave"), 1.0);
        wait_for("frames", || engine.frames_for(DEVICE) >= 5);

        engine.configure(DEVICE, &fast("cycle"), 1.0);
        // A restart resets the per-device counter, so it must now be small again.
        wait_for("the new loop", || engine.frames_for(DEVICE) >= 1);
        assert!(engine.frames_for(DEVICE) < 1000);
        // The total keeps counting across restarts.
        assert!(engine.frames_rendered() >= 6);
        engine.stop_all();
    }

    #[test]
    fn a_brightness_change_too_small_to_see_does_not_restart() {
        let sink = Arc::new(RecordingSink::new());
        let engine = engine(Arc::clone(&sink));
        let settings = fast("cycle");
        assert_eq!(signature_of(&settings, 0.5), signature_of(&settings, 0.5004));
        assert_ne!(signature_of(&settings, 0.5), signature_of(&settings, 0.51));

        engine.configure(DEVICE, &settings, 0.5);
        wait_for("frames", || engine.frames_for(DEVICE) >= 3);
        let before = engine.frames_for(DEVICE);
        engine.configure(DEVICE, &settings, 0.5004);
        assert!(engine.frames_for(DEVICE) >= before, "the loop kept running");
        engine.stop_all();
    }

    #[test]
    fn a_sink_that_refuses_a_frame_ends_the_loop() {
        // This is how an animation stops when lighting is handed back to the firmware.
        let sink = Arc::new(RecordingSink::new());
        let engine = engine(Arc::clone(&sink));
        engine.configure(DEVICE, &fast("wave"), 1.0);
        wait_for("frames", || sink.frame_count() >= 2);

        sink.close();
        std::thread::sleep(Duration::from_millis(100));
        let settled = sink.frame_count();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(sink.frame_count(), settled, "the loop must have stopped");
        engine.stop_all();
    }

    #[test]
    fn reactions_are_overlaid_on_every_animation_frame() {
        let sink = Arc::new(RecordingSink::new());
        let reactions = Arc::new(Mutex::new(ReactionEngine::new()));
        let engine = AnimationEngine::new(Arc::clone(&sink) as Arc<dyn FrameSink>, Arc::clone(&reactions));

        let w = crate::layout::by_id("W").unwrap().led_id;
        reactions.lock().unwrap().configure(
            DEVICE,
            &Frame::new(),
            &[KeyReaction {
                key_id: "W".into(),
                color: "#FF0000".into(),
                mode: KeyReactionMode::Momentary,
                enabled: true,
            }],
            1.0,
        );
        // 'W' is virtual key 0x57.
        reactions.lock().unwrap().on_key(0x57, true, false);

        engine.configure(DEVICE, &fast("cycle"), 1.0);
        wait_for("a frame with the reaction on it", || {
            sink.last_frame().is_some_and(|(_, f)| f.get(&w) == Some(&LedColor { r: 255, g: 0, b: 0 }))
        });

        // Releasing hands the key back to the animation, whose colour is whatever cycle is
        // painting - not red, and not the static base.
        reactions.lock().unwrap().on_key(0x57, false, false);
        wait_for("the key back under the animation", || {
            sink.last_frame().is_some_and(|(_, f)| f.get(&w) != Some(&LedColor { r: 255, g: 0, b: 0 }))
        });
        engine.stop_all();
    }

    #[test]
    fn a_press_reaches_a_reactive_effect() {
        let sink = Arc::new(RecordingSink::new());
        let engine = engine(Arc::clone(&sink));
        let settings = AnimationSettings {
            effect: "heatmap".into(),
            frames_per_second: 60,
            colors: vec!["#FFFFFF".into(), "#000000".into()],
            ..AnimationSettings::default()
        };
        engine.configure(DEVICE, &settings, 1.0);
        wait_for("frames", || sink.frame_count() >= 2);

        let w = crate::layout::by_id("W").unwrap().led_id;
        // Before the press, W is at the cold colour.
        assert_eq!(sink.last_frame().unwrap().1[&w], LedColor::BLACK);
        engine.on_key("W", true);
        wait_for("W to warm up", || {
            sink.last_frame().is_some_and(|(_, f)| f[&w] != LedColor::BLACK)
        });
        engine.stop_all();
    }

    #[test]
    fn a_key_release_is_not_fed_to_the_effects() {
        // Only presses leave heat or a ripple; a release must not count as a second press.
        let sink = Arc::new(RecordingSink::new());
        let engine = engine(Arc::clone(&sink));
        engine.configure(DEVICE, &fast("heatmap"), 1.0);
        wait_for("frames", || sink.frame_count() >= 1);
        engine.on_key("W", false);
        let w = crate::layout::by_id("W").unwrap().led_id;
        std::thread::sleep(Duration::from_millis(60));
        let cold = sink.last_frame().unwrap().1[&w];
        assert_eq!(cold, LedColor { r: 0, g: 8, b: 40 }, "the resting heatmap colour, quantised");
        engine.stop_all();
    }

    #[test]
    fn dropping_the_engine_stops_every_thread() {
        let sink = Arc::new(RecordingSink::new());
        {
            let engine = engine(Arc::clone(&sink));
            engine.configure("a", &fast("wave"), 1.0);
            engine.configure("b", &fast("cycle"), 1.0);
            wait_for("both loops", || sink.frame_count() >= 4);
        }
        let settled = sink.frame_count();
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(sink.frame_count(), settled, "a dropped engine must leave no threads running");
    }

    #[test]
    fn the_frame_rate_is_clamped_to_something_the_link_can_carry() {
        // A hand-edited profile asking for 10 000 fps must not spin a core; the store rejects it,
        // but the loop clamps it anyway.
        let sink = Arc::new(RecordingSink::new());
        let engine = engine(Arc::clone(&sink));
        let settings =
            AnimationSettings { frames_per_second: 10_000, ..AnimationSettings::default() };
        engine.configure(DEVICE, &settings, 1.0);
        std::thread::sleep(Duration::from_millis(200));
        let count = sink.frame_count();
        engine.stop_all();
        // 60 fps over ~200 ms is about 12 frames. Allow generous slack for a loaded machine, but
        // not the thousands an unclamped loop would produce.
        assert!(count < 100, "rendered {count} frames in 200 ms");
    }

    #[test]
    fn render_once_matches_what_the_loop_would_paint() {
        let settings = fast("wave");
        let a = render_once(&settings, 1.25, 1.0);
        let b = render_once(&settings, 1.25, 1.0);
        assert_eq!(a, b, "the preview must be the effect, not an approximation of it");
        assert_eq!(a.len(), crate::layout::led_ids().len());
    }

    #[test]
    fn a_static_frame_paints_named_keys_and_fills_the_rest() {
        let mut colors = HashMap::new();
        colors.insert("key:W".to_owned(), "#FF0000".to_owned());
        colors.insert("key:NotAKey".to_owned(), "#00FF00".to_owned());
        colors.insert("zone:0".to_owned(), "#0000FF".to_owned());
        colors.insert("key:GameMode".to_owned(), "#00FF00".to_owned());

        let frame = static_frame(&colors, "#101010", 1.0);
        let w = crate::layout::by_id("W").unwrap().led_id;
        let a = crate::layout::by_id("A").unwrap().led_id;
        assert_eq!(frame[&w], LedColor { r: 248, g: 0, b: 0 });
        assert_eq!(frame[&a], LedColor { r: 16, g: 16, b: 16 }, "an unnamed key takes the default");
        assert_eq!(frame.len(), crate::layout::led_ids().len());
        // A zone id and a key with no LED must not add an entry; a zero id would blank the board.
        assert!(!frame.contains_key(&0));
    }

    #[test]
    fn a_static_frame_with_an_unparseable_default_is_black_not_a_guess() {
        let frame = static_frame(&HashMap::new(), "not a colour", 1.0);
        assert!(frame.values().all(|c| *c == LedColor::BLACK));
    }

    #[test]
    fn directions_are_carried_into_the_signature() {
        // Changing only the direction has to restart the loop, or the user sees no change.
        let right = AnimationSettings {
            direction: AnimationDirection::Right,
            ..AnimationSettings::default()
        };
        let left =
            AnimationSettings { direction: AnimationDirection::Left, ..AnimationSettings::default() };
        assert_ne!(signature_of(&right, 1.0), signature_of(&left, 1.0));
    }
}
