//! What the speakers are playing, as a spectrum, for G HUB's AUDIO VISUALIZER effect.
//!
//! A thread captures the default output device in WASAPI shared-mode loopback, downmixes to mono
//! and keeps the last [`FFT_SIZE`] samples. [`LoopbackSpectrum::bands`] runs a Hann-windowed FFT
//! over them on demand and folds the bins into log-spaced bands in 0..1, where 0 is -60 dBFS or
//! quieter and 1 is a full-scale sine.
//!
//! Loopback only sees what goes through the shared-mode mixer: an exclusive-mode game or player is
//! invisible to it, the same as it is to G HUB.

use std::f64::consts::PI;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Samples per analysis. 2048 at 48 kHz is ~43 ms and 23 Hz per bin.
pub const FFT_SIZE: usize = 2048;
/// Below this the band reads 0.
pub const FLOOR_DB: f64 = -60.0;
/// How fast a band falls back, in full-scale per second. Rises are immediate.
pub const DECAY_PER_SECOND: f32 = 2.5;
/// No packet for this long means nothing is playing: loopback delivers nothing at all in silence
/// rather than zeros, so stale audio has to be timed out.
const SILENCE_AFTER: Duration = Duration::from_millis(150);

// ------------------------------------------------------------------ analysis (pure)

/// In-place iterative radix-2 FFT. `re.len()` must be a power of two.
pub fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    debug_assert!(n.is_power_of_two() && im.len() == n);
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -2.0 * PI / len as f64;
        let (w_re, w_im) = (angle.cos(), angle.sin());
        for start in (0..n).step_by(len) {
            let (mut cur_re, mut cur_im) = (1.0, 0.0);
            for k in 0..len / 2 {
                let a = start + k;
                let b = a + len / 2;
                let t_re = re[b] * cur_re - im[b] * cur_im;
                let t_im = re[b] * cur_im + im[b] * cur_re;
                re[b] = re[a] - t_re;
                im[b] = im[a] - t_im;
                re[a] += t_re;
                im[a] += t_im;
                let next = cur_re * w_re - cur_im * w_im;
                cur_im = cur_re * w_im + cur_im * w_re;
                cur_re = next;
            }
        }
        len <<= 1;
    }
}

/// Edges of `count` log-spaced bands from `low` to `high` Hz: `count + 1` values.
pub fn band_edges(count: usize, low: f64, high: f64) -> Vec<f64> {
    let low = low.max(1.0);
    let high = high.max(low * 1.0001);
    (0..=count).map(|i| low * (high / low).powf(i as f64 / count.max(1) as f64)).collect()
}

/// dBFS to 0..1 against [`FLOOR_DB`].
pub fn level_from_db(db: f64) -> f32 {
    ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0) as f32
}

/// Scratch space for one analysis, kept so a frame allocates nothing but its result.
struct Analysis {
    window: Vec<f64>,
    re: Vec<f64>,
    im: Vec<f64>,
    smoothed: Vec<f32>,
    last: Option<Instant>,
}

impl Analysis {
    fn new() -> Self {
        let window =
            (0..FFT_SIZE).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / FFT_SIZE as f64).cos()).collect();
        Self {
            window,
            re: vec![0.0; FFT_SIZE],
            im: vec![0.0; FFT_SIZE],
            smoothed: Vec::new(),
            last: None,
        }
    }

    /// Raw band levels for `samples` (oldest first, `FFT_SIZE` long), before smoothing.
    fn levels(&mut self, samples: &[f32], rate: f64, edges: &[f64], out: &mut Vec<f32>) {
        for i in 0..FFT_SIZE {
            self.re[i] = samples.get(i).copied().unwrap_or(0.0) as f64 * self.window[i];
            self.im[i] = 0.0;
        }
        fft(&mut self.re, &mut self.im);
        let bin_hz = rate / FFT_SIZE as f64;
        let last_bin = FFT_SIZE / 2 - 1;
        out.clear();
        for pair in edges.windows(2) {
            // Every bin whose centre is inside the band, or the nearest one when the band is
            // narrower than a bin (the low end at 60 Hz is).
            let mut first = (pair[0] / bin_hz).ceil() as usize;
            let mut last = (pair[1] / bin_hz).floor() as usize;
            if last < first {
                first = ((pair[0] + pair[1]) / 2.0 / bin_hz).round() as usize;
                last = first;
            }
            let first = first.clamp(1, last_bin);
            let last = last.clamp(first, last_bin);
            let peak = (first..=last)
                .map(|k| (self.re[k] * self.re[k] + self.im[k] * self.im[k]).sqrt())
                .fold(0.0, f64::max);
            // A Hann-windowed full-scale sine peaks at N/4.
            let amplitude = peak * 4.0 / FFT_SIZE as f64;
            let db = if amplitude > 0.0 { 20.0 * amplitude.log10() } else { FLOOR_DB };
            out.push(level_from_db(db));
        }
    }

    /// Fast attack, linear decay at [`DECAY_PER_SECOND`].
    fn smooth(&mut self, raw: &[f32], now: Instant) -> Vec<f32> {
        let dt = self.last.map_or(0.0, |t| now.duration_since(t).as_secs_f32().min(1.0));
        self.last = Some(now);
        if self.smoothed.len() != raw.len() {
            self.smoothed = raw.to_vec();
            return raw.to_vec();
        }
        for (held, new) in self.smoothed.iter_mut().zip(raw) {
            *held = if *new >= *held { *new } else { (*held - DECAY_PER_SECOND * dt).max(*new) };
        }
        self.smoothed.clone()
    }
}

// ------------------------------------------------------------------ shared ring

struct Ring {
    samples: Vec<f32>,
    /// Next write position.
    head: usize,
    rate: f64,
    last_packet: Option<Instant>,
}

impl Ring {
    fn new() -> Self {
        Self { samples: vec![0.0; FFT_SIZE], head: 0, rate: 48_000.0, last_packet: None }
    }

    fn push(&mut self, value: f32) {
        self.samples[self.head] = value;
        self.head = (self.head + 1) % FFT_SIZE;
    }

    /// Copies the ring oldest-first into `out`.
    fn copy_ordered(&self, out: &mut [f32]) {
        let (newer, older) = self.samples.split_at(self.head);
        out[..older.len()].copy_from_slice(older);
        out[older.len()..].copy_from_slice(newer);
    }
}

struct Shared {
    ring: Mutex<Ring>,
    stop: std::sync::atomic::AtomicBool,
}

/// Loopback capture of the default output device plus its spectrum.
pub struct LoopbackSpectrum {
    shared: Arc<Shared>,
    analysis: Mutex<(Analysis, Vec<f32>, Vec<f32>)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl LoopbackSpectrum {
    /// Starts capturing. `None` if there is no output device to capture, or off Windows.
    pub fn start() -> Option<Self> {
        let shared = Arc::new(Shared {
            ring: Mutex::new(Ring::new()),
            stop: std::sync::atomic::AtomicBool::new(false),
        });
        let thread = live::spawn(Arc::clone(&shared))?;
        Some(Self {
            shared,
            analysis: Mutex::new((Analysis::new(), vec![0.0; FFT_SIZE], Vec::new())),
            thread: Some(thread),
        })
    }

    /// `count` log-spaced band levels in 0..1 between `low_hz` and `high_hz`, smoothed.
    ///
    /// Falls to 0 at [`DECAY_PER_SECOND`] when nothing is playing or the device has gone.
    pub fn bands(&self, count: usize, low_hz: f64, high_hz: f64) -> Vec<f32> {
        let mut guard = self.analysis.lock().unwrap_or_else(|e| e.into_inner());
        let (analysis, samples, raw) = &mut *guard;
        let rate = {
            let ring = self.shared.ring.lock().unwrap_or_else(|e| e.into_inner());
            let live = ring.last_packet.is_some_and(|t| t.elapsed() < SILENCE_AFTER);
            if live {
                ring.copy_ordered(samples);
            } else {
                samples.iter_mut().for_each(|s| *s = 0.0);
            }
            ring.rate
        };
        let edges = band_edges(count, low_hz, high_hz.min(rate / 2.0));
        analysis.levels(samples, rate, &edges, raw);
        analysis.smooth(raw, Instant::now())
    }

    /// Sample rate of the device being captured.
    pub fn sample_rate(&self) -> f64 {
        self.shared.ring.lock().unwrap_or_else(|e| e.into_inner()).rate
    }
}

impl Drop for LoopbackSpectrum {
    fn drop(&mut self) {
        self.shared.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// ------------------------------------------------------------------ WASAPI

#[cfg(windows)]
mod live {
    use super::*;
    use crate::sys::*;
    use std::ffi::c_void;
    use std::ptr::null_mut;
    use std::sync::atomic::Ordering;

    /// A COM interface pointer, released on drop.
    struct Com(*mut c_void);

    impl Com {
        /// Entry `index` of the object's vtable.
        unsafe fn slot(&self, index: usize) -> *const c_void {
            let vtable = *(self.0 as *const *const *const c_void);
            *vtable.add(index)
        }
    }

    impl Drop for Com {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let release: unsafe extern "system" fn(*mut c_void) -> u32 =
                        std::mem::transmute(self.slot(2));
                    release(self.0);
                }
            }
        }
    }

    fn ok(hr: HRESULT) -> Option<()> {
        (hr >= 0).then_some(())
    }

    #[derive(Clone, Copy)]
    enum Sample {
        F32,
        I16,
    }

    /// The objects one capture session needs, held in drop order.
    struct Session {
        capture: Com,
        client: Com,
        _device: Com,
        channels: usize,
        format: Sample,
    }

    unsafe fn open(enumerator: &Com, ring: &Mutex<Ring>) -> Option<Session> {
        // IMMDeviceEnumerator::GetDefaultAudioEndpoint
        let get_default: unsafe extern "system" fn(*mut c_void, u32, u32, *mut *mut c_void) -> HRESULT =
            std::mem::transmute(enumerator.slot(4));
        let mut device = null_mut();
        ok(get_default(enumerator.0, E_RENDER, E_CONSOLE, &mut device))?;
        let device = Com(device);

        // IMMDevice::Activate
        let activate: unsafe extern "system" fn(
            *mut c_void,
            *const GUID,
            u32,
            *const c_void,
            *mut *mut c_void,
        ) -> HRESULT = std::mem::transmute(device.slot(3));
        let mut client = null_mut();
        ok(activate(device.0, &IID_IAUDIOCLIENT, CLSCTX_ALL, std::ptr::null(), &mut client))?;
        let client = Com(client);

        // IAudioClient::GetMixFormat
        let get_mix: unsafe extern "system" fn(*mut c_void, *mut *mut WAVEFORMATEX) -> HRESULT =
            std::mem::transmute(client.slot(8));
        let mut format_ptr = null_mut();
        ok(get_mix(client.0, &mut format_ptr))?;
        let format = std::ptr::read_unaligned(format_ptr);
        let tag = format.wFormatTag;
        let bits = format.wBitsPerSample;
        let sub = if tag == WAVE_FORMAT_EXTENSIBLE && format.cbSize >= 22 {
            Some(std::ptr::read_unaligned(format_ptr as *const WAVEFORMATEXTENSIBLE).SubFormat)
        } else {
            None
        };
        let is_float = tag == WAVE_FORMAT_IEEE_FLOAT || sub == Some(KSDATAFORMAT_SUBTYPE_IEEE_FLOAT);
        let is_pcm = tag == WAVE_FORMAT_PCM || sub == Some(KSDATAFORMAT_SUBTYPE_PCM);
        let sample = match (is_float, is_pcm, bits) {
            (true, _, 32) => Some(Sample::F32),
            (_, true, 16) => Some(Sample::I16),
            _ => None,
        };
        let channels = format.nChannels as usize;
        let rate = format.nSamplesPerSec as f64;

        // IAudioClient::Initialize, 100 ms buffer, in the mixer's own format.
        let initialize: unsafe extern "system" fn(
            *mut c_void,
            u32,
            u32,
            i64,
            i64,
            *const WAVEFORMATEX,
            *const GUID,
        ) -> HRESULT = std::mem::transmute(client.slot(3));
        let hr = initialize(
            client.0,
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            1_000_000,
            0,
            format_ptr,
            std::ptr::null(),
        );
        CoTaskMemFree(format_ptr as *mut c_void);
        let format = sample?;
        if channels == 0 {
            return None;
        }
        ok(hr)?;

        // IAudioClient::GetService(IAudioCaptureClient)
        let get_service: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT =
            std::mem::transmute(client.slot(14));
        let mut capture = null_mut();
        ok(get_service(client.0, &IID_IAUDIOCAPTURECLIENT, &mut capture))?;
        let capture = Com(capture);

        // IAudioClient::Start
        let start: unsafe extern "system" fn(*mut c_void) -> HRESULT =
            std::mem::transmute(client.slot(10));
        ok(start(client.0))?;

        ring.lock().unwrap_or_else(|e| e.into_inner()).rate = rate;
        Some(Session { capture, client, _device: device, channels, format })
    }

    /// Drains every waiting packet into the ring. `None` once the device has failed.
    unsafe fn drain(session: &Session, ring: &Mutex<Ring>) -> Option<()> {
        let next_size: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT =
            std::mem::transmute(session.capture.slot(5));
        let get_buffer: unsafe extern "system" fn(
            *mut c_void,
            *mut *mut u8,
            *mut u32,
            *mut u32,
            *mut u64,
            *mut u64,
        ) -> HRESULT = std::mem::transmute(session.capture.slot(3));
        let release: unsafe extern "system" fn(*mut c_void, u32) -> HRESULT =
            std::mem::transmute(session.capture.slot(4));
        loop {
            let mut pending = 0u32;
            ok(next_size(session.capture.0, &mut pending))?;
            if pending == 0 {
                return Some(());
            }
            let mut data = null_mut();
            let (mut frames, mut flags) = (0u32, 0u32);
            ok(get_buffer(
                session.capture.0,
                &mut data,
                &mut frames,
                &mut flags,
                null_mut(),
                null_mut(),
            ))?;
            {
                let mut ring = ring.lock().unwrap_or_else(|e| e.into_inner());
                let channels = session.channels;
                let scale = 1.0 / channels as f32;
                for frame in 0..frames as usize {
                    let value = if flags & AUDCLNT_BUFFERFLAGS_SILENT != 0 || data.is_null() {
                        0.0
                    } else {
                        let mut sum = 0.0f32;
                        for c in 0..channels {
                            let i = frame * channels + c;
                            sum += match session.format {
                                Sample::F32 => {
                                    std::ptr::read_unaligned((data as *const f32).add(i))
                                }
                                Sample::I16 => {
                                    std::ptr::read_unaligned((data as *const i16).add(i)) as f32
                                        / 32768.0
                                }
                            };
                        }
                        sum * scale
                    };
                    ring.push(value);
                }
                ring.last_packet = Some(Instant::now());
            }
            ok(release(session.capture.0, frames))?;
        }
    }

    fn run(shared: Arc<Shared>, ready: std::sync::mpsc::Sender<bool>) {
        unsafe {
            let init = CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED);
            let mut enumerator = null_mut();
            let created = CoCreateInstance(
                &CLSID_MMDEVICEENUMERATOR,
                null_mut(),
                CLSCTX_ALL,
                &IID_IMMDEVICEENUMERATOR,
                &mut enumerator,
            );
            if created < 0 {
                let _ = ready.send(false);
                if init >= 0 {
                    CoUninitialize();
                }
                return;
            }
            let enumerator = Com(enumerator);
            let mut session = open(&enumerator, &shared.ring);
            let _ = ready.send(session.is_some());
            if session.is_none() {
                drop(enumerator);
                if init >= 0 {
                    CoUninitialize();
                }
                return;
            }
            let mut retry_at = Instant::now();
            while !shared.stop.load(Ordering::SeqCst) {
                match &session {
                    Some(active) => {
                        if drain(active, &shared.ring).is_none() {
                            // Device unplugged or the default changed: drop it, the bands decay
                            // through the silence timeout, and try the (new) default again.
                            log::info!("audio loopback lost its device; retrying");
                            session = None;
                            retry_at = Instant::now() + Duration::from_secs(1);
                        }
                    }
                    None if Instant::now() >= retry_at => {
                        session = open(&enumerator, &shared.ring);
                        retry_at = Instant::now() + Duration::from_secs(1);
                    }
                    None => {}
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            if let Some(active) = session.take() {
                let stop: unsafe extern "system" fn(*mut c_void) -> HRESULT =
                    std::mem::transmute(active.client.slot(11));
                stop(active.client.0);
            }
            drop(enumerator);
            if init >= 0 {
                CoUninitialize();
            }
        }
    }

    pub(super) fn spawn(shared: Arc<Shared>) -> Option<std::thread::JoinHandle<()>> {
        let (ready, started) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("ghub audio loopback".to_owned())
            .spawn(move || run(shared, ready))
            .ok()?;
        if started.recv().unwrap_or(false) {
            Some(thread)
        } else {
            let _ = thread.join();
            None
        }
    }
}

#[cfg(not(windows))]
mod live {
    use super::*;

    pub(super) fn spawn(_shared: Arc<Shared>) -> Option<std::thread::JoinHandle<()>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: f64, amplitude: f64) -> Vec<f32> {
        (0..FFT_SIZE).map(|i| (amplitude * (2.0 * PI * freq * i as f64 / rate).sin()) as f32).collect()
    }

    #[test]
    fn the_fft_puts_a_bin_centred_sine_in_its_bin() {
        let n = 64;
        let mut re: Vec<f64> = (0..n).map(|i| (2.0 * PI * 5.0 * i as f64 / n as f64).cos()).collect();
        let mut im = vec![0.0; n];
        fft(&mut re, &mut im);
        let magnitude = |k: usize| (re[k] * re[k] + im[k] * im[k]).sqrt();
        assert!((magnitude(5) - n as f64 / 2.0).abs() < 1e-9);
        assert!((magnitude(n - 5) - n as f64 / 2.0).abs() < 1e-9);
        for k in (0..n).filter(|k| *k != 5 && *k != n - 5) {
            assert!(magnitude(k) < 1e-9, "leak into bin {k}");
        }
    }

    #[test]
    fn the_fft_of_an_impulse_is_flat() {
        let mut re = vec![0.0; 16];
        re[0] = 1.0;
        let mut im = vec![0.0; 16];
        fft(&mut re, &mut im);
        assert!(re.iter().all(|v| (v - 1.0).abs() < 1e-12) && im.iter().all(|v| v.abs() < 1e-12));
    }

    #[test]
    fn band_edges_are_log_spaced_and_hit_both_ends() {
        let edges = band_edges(4, 60.0, 6000.0);
        assert_eq!(edges.len(), 5);
        assert!((edges[0] - 60.0).abs() < 1e-9 && (edges[4] - 6000.0).abs() < 1e-6);
        let ratio = edges[1] / edges[0];
        for pair in edges.windows(2) {
            assert!((pair[1] / pair[0] - ratio).abs() < 1e-9);
        }
    }

    #[test]
    fn levels_are_floored_at_minus_sixty_and_capped_at_full_scale() {
        assert_eq!(level_from_db(-80.0), 0.0);
        assert_eq!(level_from_db(FLOOR_DB), 0.0);
        assert!((level_from_db(-30.0) - 0.5).abs() < 1e-6);
        assert_eq!(level_from_db(0.0), 1.0);
        assert_eq!(level_from_db(6.0), 1.0);
    }

    #[test]
    fn a_full_scale_tone_lights_its_own_band_and_not_the_others() {
        let rate = 48_000.0;
        let mut analysis = Analysis::new();
        let edges = band_edges(8, 60.0, 6000.0);
        // 1 kHz falls in band 4 of 8 (60 * 100^(4/8) = 600 .. 60 * 100^(5/8) ~ 1067).
        let mut out = Vec::new();
        analysis.levels(&sine(1000.0, rate, 1.0), rate, &edges, &mut out);
        assert!(out[4] > 0.95, "own band reads {}", out[4]);
        assert!(out[0] < 0.2 && out[7] < 0.2, "far bands read {:?}", out);
    }

    #[test]
    fn a_quieter_tone_reads_lower_on_the_db_scale() {
        let rate = 48_000.0;
        let mut analysis = Analysis::new();
        let edges = band_edges(8, 60.0, 6000.0);
        let mut out = Vec::new();
        // -30 dBFS should read about half.
        analysis.levels(&sine(1000.0, rate, 10f64.powf(-30.0 / 20.0)), rate, &edges, &mut out);
        assert!((out[4] - 0.5).abs() < 0.05, "reads {}", out[4]);
    }

    #[test]
    fn a_band_narrower_than_a_bin_still_sees_its_tone() {
        let rate = 48_000.0;
        let mut analysis = Analysis::new();
        // 60..70 Hz is under half a bin wide at 2048 points.
        let mut out = Vec::new();
        analysis.levels(&sine(65.0, rate, 1.0), rate, &[60.0, 70.0], &mut out);
        assert!(out[0] > 0.8, "reads {}", out[0]);
    }

    #[test]
    fn silence_reads_zero() {
        let mut analysis = Analysis::new();
        let mut out = Vec::new();
        analysis.levels(&[0.0; FFT_SIZE], 48_000.0, &band_edges(4, 60.0, 6000.0), &mut out);
        assert!(out.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn smoothing_rises_at_once_and_falls_at_the_decay_rate() {
        let mut analysis = Analysis::new();
        let start = Instant::now();
        assert_eq!(analysis.smooth(&[0.0], start), [0.0]);
        assert_eq!(analysis.smooth(&[1.0], start + Duration::from_millis(10)), [1.0]);
        let after = analysis.smooth(&[0.0], start + Duration::from_millis(210));
        assert!((after[0] - (1.0 - DECAY_PER_SECOND * 0.2)).abs() < 1e-4, "got {after:?}");
        let later = analysis.smooth(&[0.0], start + Duration::from_secs(2));
        assert_eq!(later, [0.0]);
    }

    #[test]
    fn the_ring_reads_back_oldest_first() {
        let mut ring = Ring::new();
        for i in 0..FFT_SIZE + 3 {
            ring.push(i as f32);
        }
        let mut out = vec![0.0; FFT_SIZE];
        ring.copy_ordered(&mut out);
        assert_eq!(out[0], 3.0);
        assert_eq!(out[FFT_SIZE - 1], (FFT_SIZE + 2) as f32);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "captures the live output device"]
    fn live_loopback_bands() {
        let spectrum = LoopbackSpectrum::start().expect("a default output device");
        println!("rate: {}", spectrum.sample_rate());
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(100));
            let started = Instant::now();
            let bands = spectrum.bands(16, 60.0, 6000.0);
            let cost = started.elapsed();
            let bars: String = bands.iter().map(|v| (b'0' + (v * 9.0).round() as u8) as char).collect();
            println!("{bars}  ({cost:?})");
        }
        drop(spectrum);
        println!("stopped cleanly");
    }
}
