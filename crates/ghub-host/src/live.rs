//! Screen and audio capture for G HUB's Screen Sampler and Audio Visualizer presets.
//!
//! Lazy and self-closing: the screen reader and the audio loopback stream are opened the first
//! time an effect asks, and closed again after [`IDLE`] without a request. No effect playing
//! means no capture, no thread and no audio stream.
//!
//! The screen is read on its own thread, because one GDI read-back of the desktop takes around
//! 12 ms - a third of a 25 fps frame. The animation thread takes the latest colours without
//! waiting.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ghub_core::effects::LiveInput;
use ghub_core::presets::SamplerRegion;
use ghub_hidpp::LedColor;
use ghub_platform::audio_capture::LoopbackSpectrum;
use ghub_platform::screen_capture::{Region, ScreenSampler};

/// Unused this long, a capture is closed.
const IDLE: Duration = Duration::from_secs(3);
/// Screen reads per second. More would cost CPU for no visible gain on a keyboard.
const SCREEN_RATE: Duration = Duration::from_millis(66);

#[derive(Default)]
struct ScreenShared {
    regions: Vec<Region>,
    colors: Option<Vec<LedColor>>,
    last_request: Option<Instant>,
}

#[derive(Default)]
pub struct LiveCapture {
    screen: Arc<Mutex<ScreenShared>>,
    screen_running: Arc<AtomicBool>,
    audio: Arc<Mutex<Option<(LoopbackSpectrum, Instant)>>>,
}

impl LiveCapture {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn start_screen_thread(&self) {
        if self.screen_running.swap(true, Ordering::SeqCst) {
            return;
        }
        let shared = Arc::clone(&self.screen);
        let running = Arc::clone(&self.screen_running);
        let spawned = std::thread::Builder::new().name("ghub screen sampler".into()).spawn(move || {
            let mut sampler = ScreenSampler::new();
            loop {
                let regions = {
                    let state = shared.lock().unwrap();
                    let idle = state.last_request.is_none_or(|t| t.elapsed() > IDLE);
                    if idle {
                        break;
                    }
                    state.regions.clone()
                };
                let colors = sampler.as_mut().map(|s| {
                    s.sample(&regions).into_iter().map(|[r, g, b]| LedColor { r, g, b }).collect()
                });
                shared.lock().unwrap().colors = colors;
                std::thread::sleep(SCREEN_RATE);
            }
            shared.lock().unwrap().colors = None;
            running.store(false, Ordering::SeqCst);
            log::debug!("Screen sampler closed: no effect is using it");
        });
        if spawned.is_err() {
            self.screen_running.store(false, Ordering::SeqCst);
        }
    }
}

impl LiveInput for LiveCapture {
    fn screen(&self, regions: &[SamplerRegion]) -> Option<Vec<LedColor>> {
        let colors = {
            let mut state = self.screen.lock().unwrap();
            state.last_request = Some(Instant::now());
            if state.regions.len() != regions.len() {
                state.regions = regions
                    .iter()
                    .map(|r| Region { left: r.left, top: r.top, right: r.right, bottom: r.bottom })
                    .collect();
            }
            state.colors.clone()
        };
        self.start_screen_thread();
        colors
    }

    fn spectrum(&self, count: usize, low_hz: f64, high_hz: f64) -> Option<Vec<f32>> {
        let mut audio = self.audio.lock().unwrap();
        if audio.is_none() {
            *audio = LoopbackSpectrum::start().map(|s| (s, Instant::now()));
            if audio.is_some() {
                // Closes the stream once no effect has asked for a while.
                let shared = Arc::clone(&self.audio);
                let _ = std::thread::Builder::new().name("ghub audio reaper".into()).spawn(move || {
                    loop {
                        std::thread::sleep(Duration::from_secs(1));
                        let mut audio = shared.lock().unwrap();
                        if audio.as_ref().is_none_or(|(_, last)| last.elapsed() > IDLE) {
                            *audio = None;
                            log::debug!("Audio loopback closed: no effect is using it");
                            return;
                        }
                    }
                });
            }
        }
        let (stream, last) = audio.as_mut()?;
        *last = Instant::now();
        Some(stream.bands(count, low_hz, high_hz))
    }
}
