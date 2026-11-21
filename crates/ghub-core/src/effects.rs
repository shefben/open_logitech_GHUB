//! Software per-key animations. Port of `Artemis.Peripherals/Lighting/KeyEffects.cs` and
//! `KeyEffectState.cs`.
//!
//! Rendering is a pure function of (settings, time, press history, brightness), so a frame can be
//! produced without a device present. That is what makes the effects testable and what lets the
//! same code paint both the keyboard and the UI's preview - the preview is not an approximation
//! of the effect, it *is* the effect.
//!
//! The first five effects mirror what G HUB ships for the G815 (Color Wave, Breathing, Color
//! Cycle, Ripple, Starlight). Echo Press, Screen Sampler and Audio Visualizer are G HUB's too; the
//! last two use G HUB's own per-key maps (see [`crate::presets`]) and a [`LiveInput`] the host
//! supplies. The `ghub:` entries are G HUB's five ANIMATIONS, played from its keyframe data. Rain,
//! Fire, Scan, Pinwheel and Heatmap are this application's own.

use std::collections::HashMap;
use std::f64::consts::PI;
use std::sync::{Arc, Mutex};

use ghub_hidpp::LedColor;

use crate::layout;
use crate::presets;
use crate::profile::{AnimationDirection, AnimationSettings};

/// Live data two of G HUB's presets need: what is on screen and what is playing.
///
/// Supplied by the host, which owns the capture code. Only Screen Sampler and Audio Visualizer
/// call it, so nothing is captured unless one of them is running.
pub trait LiveInput: Send + Sync {
    /// The average colour of each region, in order, or `None` when the screen cannot be read.
    fn screen(&self, regions: &[presets::SamplerRegion]) -> Option<Vec<LedColor>>;
    /// `count` log-spaced levels in 0..1 between `low_hz` and `high_hz`, or `None` when nothing
    /// can be captured.
    fn spectrum(&self, count: usize, low_hz: f64, high_hz: f64) -> Option<Vec<f32>>;
}

/// One entry in the animation catalogue: what it is called and what of it the user can set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEffectDefinition {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub uses_direction: bool,
    /// 0 means the effect has no colour of its own and paints a rainbow.
    pub max_colors: usize,
    /// Whether the effect answers key presses, which decides if the input hook has to stay on.
    pub reactive: bool,
}

impl KeyEffectDefinition {
    pub fn uses_colors(&self) -> bool {
        self.max_colors > 0
    }

    /// One of G HUB's keyframe ANIMATIONS rather than a computed effect.
    pub fn is_keyframe(&self) -> bool {
        self.id.starts_with("ghub:")
    }

    /// Needs the host's [`LiveInput`].
    pub fn is_live(&self) -> bool {
        matches!(self.id, "screen" | "audio")
    }
}

/// Echo Press: how long a pressed key takes to fade, at speed 1.
const ECHO_SECONDS: f64 = 1.0;

/// Audio Visualizer: bands computed before they are pooled into G HUB's columns.
const SPECTRUM_BANDS: usize = 48;

pub static CATALOG: &[KeyEffectDefinition] = &[
    KeyEffectDefinition {
        id: "wave",
        name: "Color Wave",
        description: "A band of colour travelling across the keyboard.",
        uses_direction: true,
        max_colors: 4,
        reactive: false,
    },
    KeyEffectDefinition {
        id: "breathing",
        name: "Breathing",
        description: "The whole keyboard fades in and out.",
        uses_direction: false,
        max_colors: 4,
        reactive: false,
    },
    KeyEffectDefinition {
        id: "cycle",
        name: "Color Cycle",
        description: "The whole keyboard moves through the spectrum together.",
        uses_direction: false,
        max_colors: 0,
        reactive: false,
    },
    KeyEffectDefinition {
        id: "ripple",
        name: "Ripple",
        description: "A ring spreads out from every key you press.",
        uses_direction: false,
        max_colors: 2,
        reactive: true,
    },
    KeyEffectDefinition {
        id: "starlight",
        name: "Starlight",
        description: "Keys light and fade one after another at random.",
        uses_direction: false,
        max_colors: 2,
        reactive: false,
    },
    KeyEffectDefinition {
        id: "echo",
        name: "Echo Press",
        description: "Each key you press lights up and fades back out.",
        uses_direction: false,
        max_colors: 2,
        reactive: true,
    },
    KeyEffectDefinition {
        id: "screen",
        name: "Screen Sampler",
        description: "The keyboard takes the colours of five strips across your screen.",
        uses_direction: false,
        max_colors: 0,
        reactive: false,
    },
    KeyEffectDefinition {
        id: "audio",
        name: "Audio Visualizer",
        description: "Columns of keys rise and fall with the sound your PC is playing.",
        uses_direction: false,
        max_colors: 2,
        reactive: false,
    },
    KeyEffectDefinition {
        id: "ghub:contrastic",
        name: "Contrastic",
        description: "G HUB's Contrastic animation, from its own keyframes.",
        uses_direction: false,
        max_colors: 0,
        reactive: false,
    },
    KeyEffectDefinition {
        id: "ghub:lightning",
        name: "Lightning",
        description: "G HUB's Lightning animation, from its own keyframes.",
        uses_direction: false,
        max_colors: 0,
        reactive: false,
    },
    KeyEffectDefinition {
        id: "ghub:oceanwave",
        name: "Ocean Wave",
        description: "G HUB's Ocean Wave animation, from its own keyframes.",
        uses_direction: false,
        max_colors: 0,
        reactive: false,
    },
    KeyEffectDefinition {
        id: "ghub:redwhiteblue",
        name: "Red White and Blue",
        description: "G HUB's Red White and Blue animation, from its own keyframes.",
        uses_direction: false,
        max_colors: 0,
        reactive: false,
    },
    KeyEffectDefinition {
        id: "ghub:verticool",
        name: "Verticool",
        description: "G HUB's Verticool animation, from its own keyframes.",
        uses_direction: false,
        max_colors: 0,
        reactive: false,
    },
    KeyEffectDefinition {
        id: "rain",
        name: "Rain",
        description: "Drops fall down the columns of the keyboard.",
        uses_direction: true,
        max_colors: 2,
        reactive: false,
    },
    KeyEffectDefinition {
        id: "fire",
        name: "Fire",
        description: "Embers flicker along the bottom and die out towards the top.",
        uses_direction: true,
        max_colors: 2,
        reactive: false,
    },
    KeyEffectDefinition {
        id: "scan",
        name: "Scan",
        description: "A bright bar sweeps over a dim board.",
        uses_direction: true,
        max_colors: 2,
        reactive: false,
    },
    KeyEffectDefinition {
        id: "pinwheel",
        name: "Pinwheel",
        description: "The spectrum turns around the middle of the keyboard.",
        uses_direction: true,
        max_colors: 0,
        reactive: false,
    },
    KeyEffectDefinition {
        id: "heatmap",
        name: "Heatmap",
        description: "Keys warm up as you use them and cool down when you stop.",
        uses_direction: false,
        max_colors: 2,
        reactive: true,
    },
];

/// The named effect, or the first in the catalogue.
///
/// Falling back rather than failing is deliberate: a profile written by a newer build naming an
/// effect this one does not have must still load and light the keyboard.
pub fn definition(id: &str) -> &'static KeyEffectDefinition {
    CATALOG
        .iter()
        .find(|e| e.id.eq_ignore_ascii_case(id))
        .unwrap_or(&CATALOG[0])
}

/// A lightable key: its LED id and where it sits, normalised so 0,0 is the top left of the board.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyPoint {
    pub led_id: u8,
    pub key_id: &'static str,
    pub x: f64,
    pub y: f64,
    /// A stable per-key number in 0..1, so the random-looking effects look the same every run.
    pub seed: f64,
}

/// Keys that can be lit, with their positions normalised across the board.
pub fn points() -> &'static [KeyPoint] {
    use std::sync::OnceLock;
    static POINTS: OnceLock<Vec<KeyPoint>> = OnceLock::new();
    POINTS.get_or_init(build_points)
}

fn build_points() -> Vec<KeyPoint> {
    let lit: Vec<&layout::KeyDefinition> =
        layout::keys().iter().filter(|k| k.led_id != 0).collect();
    let max_x = lit.iter().fold(0.0f64, |m, k| m.max(k.x + k.w));
    let max_y = lit.iter().fold(0.0f64, |m, k| m.max(k.y + k.h));
    lit.iter()
        .map(|k| KeyPoint {
            led_id: k.led_id,
            key_id: k.id,
            x: (k.x + k.w / 2.0) / max_x,
            y: (k.y + k.h / 2.0) / max_y,
            seed: stable_seed(k.id),
        })
        .collect()
}

/// FNV-1a over the key id, folded into 0..1.
///
/// The C# used `string.GetHashCode()`, which .NET randomises per process, so Starlight twinkled in
/// a different order on every launch. This is deterministic, which is both what the comment in the
/// C# claimed and what makes the effect tests able to assert anything.
fn stable_seed(id: &str) -> f64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    (hash % 1000) as f64 / 1000.0
}

pub fn point_for(key_id: &str) -> Option<&'static KeyPoint> {
    points().iter().find(|p| p.key_id == key_id)
}

/// Renders one frame.
///
/// `seconds` is wall time since the effect started. The renderer is stateless apart from `state`,
/// which carries everything that depends on history.
pub fn render(
    settings: &AnimationSettings,
    seconds: f64,
    state: &EffectState,
    brightness: f64,
) -> HashMap<u8, LedColor> {
    let speed = settings.speed.clamp(0.05, 10.0);
    let t = seconds * speed;
    let palette = palette(settings);
    let effect = definition(&settings.effect).id;
    let brightness = brightness.clamp(0.0, 1.0);

    // Per-frame inputs, computed once rather than per key.
    let keyframe = presets::keyframe_animation(effect).map(|a| a.sample(seconds * 1000.0, speed));
    let screen = if effect == "screen" { state.screen_colors() } else { None };
    let levels = if effect == "audio" { state.column_levels(&palette) } else { HashMap::new() };

    points()
        .iter()
        .map(|p| {
            let axis = axis(p, settings.direction);
            let color = match effect {
                _ if keyframe.is_some() => keyframe
                    .as_ref()
                    .and_then(|frame| frame.get(p.key_id).copied())
                    .unwrap_or(LedColor::BLACK),
                "echo" => state.echo_color(p, t, &palette, settings.intensity),
                "screen" => screen_color(p, screen.as_deref()),
                "audio" => levels.get(p.key_id).copied().unwrap_or(LedColor::BLACK),
                "wave" => sample(&palette, axis - t * 0.35),
                "breathing" => scale(sample(&palette, t * 0.08), pulse(t * 0.5)),
                "cycle" => hue(t * 0.1),
                "ripple" => state.ripple_color(p, t, &palette, settings.intensity),
                "starlight" => scale(sample(&palette, p.seed), state.twinkle(p, t)),
                "rain" => scale(sample(&palette, p.seed * 0.2), state.drop(p, t, settings.direction)),
                "fire" => fire(p, t, settings.direction),
                "scan" => scale(sample(&palette, axis), bar(axis, t)),
                "pinwheel" => hue(angle(p, settings.direction) + t * 0.12),
                "heatmap" => state.heat_color(p, &palette, settings.intensity),
                _ => sample(&palette, axis - t * 0.35),
            };
            (p.led_id, quantize(scale(color, brightness)))
        })
        .collect()
}

/// Rounds each channel down to a multiple of 8.
///
/// Two keys a hair apart in a gradient then share a colour, which lets 0x8081 pack them into one
/// 13-key packet, and a key whose colour barely moved between frames stops counting as changed at
/// all. Both matter: the link carries one packet per four distinct colours, so an unquantised
/// gradient across 110 keys is 28 packets a frame instead of 9.
pub fn quantize(c: LedColor) -> LedColor {
    LedColor { r: c.r & 0xF8, g: c.g & 0xF8, b: c.b & 0xF8 }
}

// ------------------------------------------------------------------ live presets

/// The colour of the screen region G HUB assigns a key. A key G HUB's map leaves out takes the
/// region above it, by position.
fn screen_color(p: &KeyPoint, colors: Option<&[LedColor]>) -> LedColor {
    let Some(colors) = colors.filter(|c| !c.is_empty()) else { return LedColor::BLACK };
    let index = presets::screen_sampler_layout()
        .keys
        .get(p.key_id)
        .copied()
        .unwrap_or_else(|| ((p.x * colors.len() as f64) as usize).min(colors.len() - 1));
    colors.get(index).copied().unwrap_or(LedColor::BLACK)
}

/// Lights one G HUB visualizer column to `level` (0..1): bottom key first, the top lit key partly
/// lit for a level between two keys. With no second colour each column takes its own hue.
fn light_column(
    keys: &[Option<&'static str>],
    level: f64,
    low: LedColor,
    high: Option<LedColor>,
    column_hue: f64,
    out: &mut HashMap<&'static str, LedColor>,
) {
    let height = keys.len().max(1) as f64;
    let filled = level.clamp(0.0, 1.0) * height;
    for (row, key) in keys.iter().enumerate() {
        let Some(key) = key else { continue };
        let lit = (filled - row as f64).clamp(0.0, 1.0);
        if lit <= 0.0 {
            continue;
        }
        let along = row as f64 / (height - 1.0).max(1.0);
        let colour = match high {
            Some(high) => mix(low, high, along),
            None => hue(column_hue),
        };
        out.insert(key, scale(colour, lit));
    }
}

// ------------------------------------------------------------------ geometry

/// Position along the direction of travel: 0 at the start edge, 1 at the far edge.
fn axis(p: &KeyPoint, direction: AnimationDirection) -> f64 {
    match direction {
        AnimationDirection::Right => p.x,
        AnimationDirection::Left => 1.0 - p.x,
        AnimationDirection::Down => p.y,
        AnimationDirection::Up => 1.0 - p.y,
        AnimationDirection::Out => distance(p),
        AnimationDirection::In => 1.0 - distance(p),
    }
}

fn distance(p: &KeyPoint) -> f64 {
    (((p.x - 0.5).powi(2) + (p.y - 0.5).powi(2)).sqrt() / 0.707).clamp(0.0, 1.0)
}

fn angle(p: &KeyPoint, direction: AnimationDirection) -> f64 {
    let a = ((p.y - 0.5).atan2(p.x - 0.5) + PI) / (2.0 * PI);
    if matches!(
        direction,
        AnimationDirection::Left | AnimationDirection::Up | AnimationDirection::In
    ) {
        1.0 - a
    } else {
        a
    }
}

fn flip(d: AnimationDirection) -> AnimationDirection {
    match d {
        AnimationDirection::Right => AnimationDirection::Left,
        AnimationDirection::Left => AnimationDirection::Right,
        AnimationDirection::Up => AnimationDirection::Down,
        AnimationDirection::Down => AnimationDirection::Up,
        AnimationDirection::Out => AnimationDirection::In,
        AnimationDirection::In => AnimationDirection::Out,
    }
}

// ------------------------------------------------------------------ colour

fn palette(settings: &AnimationSettings) -> Vec<LedColor> {
    settings.colors.iter().filter_map(|hex| crate::profile::parse_color(hex)).collect()
}

/// Samples the palette at `position`, wrapping, or the rainbow when it is empty.
fn sample(palette: &[LedColor], position: f64) -> LedColor {
    match palette.len() {
        0 => hue(position),
        1 => palette[0],
        n => {
            let wrapped = wrap(position) * n as f64;
            let i = wrapped as usize;
            mix(palette[i % n], palette[(i + 1) % n], wrapped - i as f64)
        }
    }
}

fn wrap(v: f64) -> f64 {
    v - v.floor()
}

pub fn mix(a: LedColor, b: LedColor, amount: f64) -> LedColor {
    let k = amount.clamp(0.0, 1.0);
    let channel = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * k) as u8;
    LedColor { r: channel(a.r, b.r), g: channel(a.g, b.g), b: channel(a.b, b.b) }
}

pub fn scale(c: LedColor, amount: f64) -> LedColor {
    let k = amount.clamp(0.0, 1.0);
    LedColor {
        r: (c.r as f64 * k) as u8,
        g: (c.g as f64 * k) as u8,
        b: (c.b as f64 * k) as u8,
    }
}

/// Full-saturation colour at `position` around the colour wheel.
pub fn hue(position: f64) -> LedColor {
    let h = wrap(position) * 6.0;
    let f = h - h.floor();
    let up = (255.0 * f) as u8;
    let down = (255.0 * (1.0 - f)) as u8;
    match h as u32 {
        0 => LedColor { r: 255, g: up, b: 0 },
        1 => LedColor { r: down, g: 255, b: 0 },
        2 => LedColor { r: 0, g: 255, b: up },
        3 => LedColor { r: 0, g: down, b: 255 },
        4 => LedColor { r: up, g: 0, b: 255 },
        _ => LedColor { r: 255, g: 0, b: down },
    }
}

/// A smooth 0..1..0 over one cycle, which reads as a breath rather than a triangle fade.
fn pulse(t: f64) -> f64 {
    (1.0 - (wrap(t) * 2.0 * PI).cos()) / 2.0
}

/// A bright band travelling along the axis, dim everywhere else.
fn bar(axis: f64, t: f64) -> f64 {
    let d = (axis - wrap(t * 0.3)).abs();
    // The bar wraps round the edge rather than vanishing and reappearing.
    let d = d.min(1.0 - d);
    (1.0 - d * 7.0).max(0.06)
}

fn fire(p: &KeyPoint, t: f64, direction: AnimationDirection) -> LedColor {
    // Hottest at the start edge, cooling towards the far edge, with per-key flicker so no two
    // keys ever match.
    let towards = if matches!(direction, AnimationDirection::Up | AnimationDirection::Left) {
        direction
    } else {
        flip(direction)
    };
    let height = 1.0 - axis(p, towards);
    let flicker = 0.65 + 0.35 * noise(p.seed, t * 1.7);
    let heat = (height * flicker).clamp(0.0, 1.0);
    const EMBER: LedColor = LedColor { r: 140, g: 20, b: 0 };
    const FLAME: LedColor = LedColor { r: 255, g: 110, b: 0 };
    const TIP: LedColor = LedColor { r: 255, g: 220, b: 120 };
    if heat < 0.35 {
        mix(LedColor::BLACK, EMBER, heat / 0.35)
    } else if heat < 0.75 {
        mix(EMBER, FLAME, (heat - 0.35) / 0.4)
    } else {
        mix(FLAME, TIP, (heat - 0.75) / 0.25)
    }
}

/// Cheap deterministic noise in 0..1. Good enough for flicker and drop offsets, and repeatable.
pub fn noise(a: f64, b: f64) -> f64 {
    let v = (a * 127.1 + b * 311.7).sin() * 43758.5453;
    v - v.floor()
}

// ------------------------------------------------------------------ history

/// Rings older than this have faded past the far corner of the board.
const RIPPLE_LIFE_SECONDS: f64 = 1.4;

/// Rings kept at once. A key held on auto-repeat would otherwise grow this without bound.
const MAX_RIPPLES: usize = 24;

#[derive(Debug, Clone, Copy)]
struct Ripple {
    x: f64,
    y: f64,
    started: f64,
}

#[derive(Debug, Default)]
struct History {
    ripples: Vec<Ripple>,
    /// Echo Press: when each key was last pressed.
    echoes: HashMap<&'static str, f64>,
    heat: HashMap<&'static str, f64>,
    last_heat_time: f64,
}

/// The part of an animation that depends on what happened before: twinkles, falling drops, the
/// rings left by key presses and how warm each key is. One instance per running effect per device.
///
/// [`Self::press`] is called from the input thread and everything else from the animation thread,
/// which is why the history sits behind a mutex. The lock is never held across a render: each
/// accessor copies out what it needs first.
#[derive(Default)]
pub struct EffectState {
    history: Mutex<History>,
    live: Option<Arc<dyn LiveInput>>,
}

impl std::fmt::Debug for EffectState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectState").field("live", &self.live.is_some()).finish_non_exhaustive()
    }
}

impl EffectState {
    pub fn new() -> Self {
        Self::default()
    }

    /// A state that can read the screen and the audio, for Screen Sampler and Audio Visualizer.
    pub fn with_live(live: Option<Arc<dyn LiveInput>>) -> Self {
        Self { live, ..Self::default() }
    }

    fn screen_colors(&self) -> Option<Vec<LedColor>> {
        self.live.as_ref()?.screen(&presets::screen_sampler_layout().regions)
    }

    /// Every visualizer key's colour this frame. Each G HUB column shows the loudest band in its
    /// frequency range.
    fn column_levels(&self, palette: &[LedColor]) -> HashMap<&'static str, LedColor> {
        let mut out = HashMap::new();
        let layout = presets::visualizer_layout();
        let low = layout.default_low as f64;
        let high = layout.default_high as f64;
        let Some(bands) = self.live.as_ref().and_then(|l| l.spectrum(SPECTRUM_BANDS, low, high))
        else {
            return out;
        };
        if bands.is_empty() {
            return out;
        }
        let ratio = (high / low).ln();
        let band_of =
            |hz: f64| (((hz.clamp(low, high) / low).ln() / ratio) * bands.len() as f64) as usize;
        let first = palette.first().copied().unwrap_or(LedColor { r: 0, g: 255, b: 0 });
        let second = match palette.len() {
            0 => Some(LedColor { r: 255, g: 0, b: 0 }),
            1 => Some(first),
            _ => Some(palette[1]),
        };
        let count = layout.columns.len().max(1);
        for (index, (from, to, keys)) in layout.columns.iter().enumerate() {
            let a = band_of(*from as f64).min(bands.len() - 1);
            let b = band_of(*to as f64).clamp(a + 1, bands.len());
            let level = bands[a..b].iter().copied().fold(0.0f32, f32::max) as f64;
            light_column(keys, level, first, second, index as f64 / count as f64, &mut out);
        }
        out
    }

    /// Echo Press: a pressed key at the first colour, fading to the second (or dark).
    pub fn echo_color(
        &self,
        point: &KeyPoint,
        seconds: f64,
        palette: &[LedColor],
        intensity: f64,
    ) -> LedColor {
        let pressed = self.history.lock().unwrap().echoes.get(point.key_id).copied();
        let lit = palette.first().copied().unwrap_or(LedColor::WHITE);
        let rest = palette.get(1).copied().unwrap_or(LedColor::BLACK);
        let Some(pressed) = pressed else { return rest };
        let fade = 1.0 - ((seconds - pressed) / ECHO_SECONDS).clamp(0.0, 1.0);
        mix(rest, lit, (fade * intensity.clamp(0.0, 2.0)).clamp(0.0, 1.0))
    }

    /// Records a key press for the effects that answer them. Safe to call from the input thread.
    ///
    /// A key id the layout does not have is ignored rather than stored: it could never be
    /// rendered, and keeping it would let an unknown id grow the heat map without bound.
    pub fn press(&self, key_id: &str, seconds: f64) {
        let Some(point) = point_for(key_id) else { return };
        let mut history = self.history.lock().unwrap();
        history.ripples.push(Ripple { x: point.x, y: point.y, started: seconds });
        if history.ripples.len() > MAX_RIPPLES {
            let excess = history.ripples.len() - MAX_RIPPLES;
            history.ripples.drain(..excess);
        }
        let heat = history.heat.entry(point.key_id).or_insert(0.0);
        *heat = (*heat + 0.45).min(1.0);
        history.echoes.insert(point.key_id, seconds);
    }

    /// Drops finished rings and cools the heat map. Called once per frame, so neither can grow
    /// without bound.
    pub fn advance(&self, seconds: f64) {
        let mut history = self.history.lock().unwrap();
        history.ripples.retain(|r| seconds - r.started <= RIPPLE_LIFE_SECONDS);
        history.echoes.retain(|_, pressed| seconds - *pressed <= ECHO_SECONDS * 20.0);

        // Cool at a steady rate in wall time, so the effect looks the same at 10 and at 60 fps.
        // Clamped to a second so a long pause does not blank the board in one step.
        let elapsed = (seconds - history.last_heat_time).clamp(0.0, 1.0);
        history.last_heat_time = seconds;
        history.heat.retain(|_, value| {
            *value -= elapsed * 0.35;
            *value > 0.01
        });
    }

    pub fn ripple_count(&self) -> usize {
        self.history.lock().unwrap().ripples.len()
    }

    pub fn heat_of(&self, key_id: &str) -> f64 {
        self.history.lock().unwrap().heat.get(key_id).copied().unwrap_or(0.0)
    }

    /// Ripple: the brightest point of each ring is at its current radius, fading as it ages.
    pub fn ripple_color(
        &self,
        point: &KeyPoint,
        seconds: f64,
        palette: &[LedColor],
        intensity: f64,
    ) -> LedColor {
        let mut best = 0.0f64;
        {
            let history = self.history.lock().unwrap();
            for r in &history.ripples {
                let age = seconds - r.started;
                if age < 0.0 {
                    continue;
                }
                let radius = age / RIPPLE_LIFE_SECONDS;
                let distance = ((point.x - r.x).powi(2) + (point.y - r.y).powi(2)).sqrt();
                let on_ring = 1.0 - (distance - radius).abs() * 9.0;
                if on_ring > 0.0 {
                    best = best.max(on_ring * (1.0 - radius));
                }
            }
        }
        let lit = palette
            .first()
            .copied()
            .unwrap_or_else(|| hue(point.x * 0.5 + seconds * 0.1));
        let rest = palette.get(1).copied().unwrap_or(LedColor::BLACK);
        mix(rest, lit, (best * intensity.clamp(0.0, 2.0)).clamp(0.0, 1.0))
    }

    /// Heatmap: a key is at its colour right after a press and cools to the resting colour.
    pub fn heat_color(
        &self,
        point: &KeyPoint,
        palette: &[LedColor],
        intensity: f64,
    ) -> LedColor {
        let heat = self.heat_of(point.key_id);
        let hot = palette.first().copied().unwrap_or(LedColor { r: 255, g: 40, b: 0 });
        let cold = palette.get(1).copied().unwrap_or(LedColor { r: 0, g: 10, b: 40 });
        mix(cold, hot, (heat * intensity.clamp(0.0, 2.0)).clamp(0.0, 1.0))
    }

    /// Starlight: each key lights on its own schedule, derived from its seed so the pattern is
    /// stable from run to run.
    pub fn twinkle(&self, point: &KeyPoint, seconds: f64) -> f64 {
        let period = 2.2 + point.seed * 2.5;
        let phase = (seconds * 0.5 + point.seed * period) % period / period;
        if phase < 0.5 {
            (1.0 - (phase - 0.12).abs() * 9.0).max(0.0)
        } else {
            0.0
        }
    }

    /// Rain: each column runs a drop whose head is bright and whose tail fades behind it.
    pub fn drop(&self, point: &KeyPoint, seconds: f64, direction: AnimationDirection) -> f64 {
        let column = (point.x * 22.0).floor() / 22.0;
        let along = if matches!(direction, AnimationDirection::Up | AnimationDirection::Left) {
            1.0 - point.y
        } else {
            point.y
        };
        // Modulo 1.6 rather than 1.0 leaves a gap between one drop and the next.
        let head = (seconds * 0.45 + noise(column, 0.0) * 3.0) % 1.6;
        let behind = head - along;
        if (0.0..0.45).contains(&behind) {
            1.0 - behind / 0.45
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(effect: &str) -> AnimationSettings {
        AnimationSettings { effect: effect.to_owned(), ..AnimationSettings::default() }
    }

    #[test]
    fn an_unknown_effect_falls_back_rather_than_failing() {
        // A profile from a newer build naming an effect we do not have must still light up.
        assert_eq!(definition("not-an-effect").id, "wave");
        assert_eq!(definition("WAVE").id, "wave", "lookup is case-insensitive");
        assert_eq!(definition("heatmap").id, "heatmap");
    }

    #[test]
    fn every_catalogue_entry_is_distinct_and_described() {
        let mut ids: Vec<&str> = CATALOG.iter().map(|e| e.id).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate effect ids");
        for e in CATALOG {
            assert!(!e.name.is_empty(), "{} has no name", e.id);
            assert!(e.description.ends_with('.'), "{} description reads oddly", e.id);
            assert!(e.max_colors <= 8, "{} exceeds the colour limit the store allows", e.id);
        }
        // Exactly the two reactive effects need the input hook. Getting this wrong means either
        // a dead effect or a keyboard hook left on for nothing.
        let reactive: Vec<&str> =
            CATALOG.iter().filter(|e| e.reactive).map(|e| e.id).collect();
        assert_eq!(reactive, vec!["ripple", "echo", "heatmap"]);
    }

    #[test]
    fn a_frame_covers_every_lightable_key_and_no_others() {
        let frame = render(&settings("wave"), 1.0, &EffectState::new(), 1.0);
        assert_eq!(frame.len(), layout::led_ids().len());
        assert!(!frame.contains_key(&0), "a zero id would blank the whole board");
        for id in layout::led_ids() {
            assert!(frame.contains_key(&id), "LED 0x{id:02X} is missing from the frame");
        }
    }

    #[test]
    fn every_effect_renders_without_panicking_in_every_direction() {
        let state = EffectState::new();
        state.press("W", 0.0);
        for effect in CATALOG {
            for direction in [
                AnimationDirection::Right,
                AnimationDirection::Left,
                AnimationDirection::Up,
                AnimationDirection::Down,
                AnimationDirection::Out,
                AnimationDirection::In,
            ] {
                let s = AnimationSettings {
                    effect: effect.id.to_owned(),
                    direction,
                    ..AnimationSettings::default()
                };
                for t in [0.0, 0.5, 7.3, 1_000.0] {
                    let frame = render(&s, t, &state, 1.0);
                    assert_eq!(
                        frame.len(),
                        layout::led_ids().len(),
                        "{} at t={t} going {direction:?}",
                        effect.id
                    );
                }
            }
        }
    }

    #[test]
    fn quantising_lets_a_gradient_pack_into_far_fewer_packets() {
        // The whole point: 0x8081 sends one packet per four distinct colours, so the number of
        // distinct colours in a frame is the frame's cost.
        let frame = render(&settings("wave"), 2.0, &EffectState::new(), 1.0);
        let distinct: std::collections::HashSet<LedColor> = frame.values().copied().collect();
        assert!(
            distinct.len() < frame.len(),
            "a gradient across {} keys produced {} distinct colours",
            frame.len(),
            distinct.len()
        );
        for color in frame.values() {
            assert_eq!(color.r & 0x07, 0, "unquantised red {color:?}");
            assert_eq!(color.g & 0x07, 0);
            assert_eq!(color.b & 0x07, 0);
        }
    }

    #[test]
    fn brightness_zero_is_a_black_frame_and_brightness_is_clamped() {
        let dark = render(&settings("cycle"), 1.0, &EffectState::new(), 0.0);
        assert!(dark.values().all(|c| *c == LedColor::BLACK), "brightness 0 must be black");
        // An out-of-range brightness from a hand-edited profile must not wrap a channel round.
        let over = render(&settings("cycle"), 1.0, &EffectState::new(), 5.0);
        let normal = render(&settings("cycle"), 1.0, &EffectState::new(), 1.0);
        assert_eq!(over, normal);
    }

    #[test]
    fn the_rainbow_covers_the_wheel_and_wraps() {
        assert_eq!(hue(0.0), LedColor { r: 255, g: 0, b: 0 });
        assert_eq!(hue(1.0), hue(0.0), "the wheel wraps");
        assert_eq!(hue(-0.5), hue(0.5), "and wraps backwards too");
        // A third of the way round is green, two thirds blue.
        let green = hue(1.0 / 3.0);
        assert!(green.g > 200 && green.r < 60, "{green:?}");
        let blue = hue(2.0 / 3.0);
        assert!(blue.b > 200 && blue.g < 60, "{blue:?}");
    }

    #[test]
    fn a_palette_is_honoured_and_an_empty_one_means_rainbow() {
        let red_blue = AnimationSettings {
            effect: "wave".into(),
            colors: vec!["#FF0000".into(), "#0000FF".into()],
            ..AnimationSettings::default()
        };
        let frame = render(&red_blue, 0.0, &EffectState::new(), 1.0);
        // Every colour must be on the line between red and blue, i.e. no green at all.
        assert!(frame.values().all(|c| c.g == 0), "a red-to-blue wave must have no green");

        // An invalid colour is skipped rather than parsed as black, which would put a dead band
        // in the gradient.
        let with_junk = AnimationSettings {
            effect: "wave".into(),
            colors: vec!["#FF0000".into(), "not a colour".into()],
            ..AnimationSettings::default()
        };
        let single = render(&with_junk, 0.0, &EffectState::new(), 1.0);
        assert!(
            single.values().all(|c| *c == LedColor { r: 248, g: 0, b: 0 }),
            "one valid colour means a solid board"
        );
    }

    #[test]
    fn a_single_colour_palette_is_solid_rather_than_a_gradient_to_itself() {
        let one = AnimationSettings {
            effect: "wave".into(),
            colors: vec!["#20FF80".into()],
            ..AnimationSettings::default()
        };
        let frame = render(&one, 3.0, &EffectState::new(), 1.0);
        let distinct: std::collections::HashSet<LedColor> = frame.values().copied().collect();
        assert_eq!(distinct.len(), 1, "{distinct:?}");
    }

    #[test]
    fn the_seed_is_stable_across_runs() {
        // The C# used GetHashCode, which .NET randomises per process, so Starlight twinkled in a
        // different order on every launch. These values are fixed.
        let a = stable_seed("Escape");
        let b = stable_seed("Escape");
        assert_eq!(a, b);
        assert_ne!(stable_seed("Escape"), stable_seed("Space"));
        assert!((0.0..1.0).contains(&a));
        // Two different runs of the whole point list must agree key for key.
        let first: Vec<f64> = points().iter().map(|p| p.seed).collect();
        let second: Vec<f64> = build_points().iter().map(|p| p.seed).collect();
        assert_eq!(first, second);
    }

    #[test]
    fn a_press_leaves_a_ripple_that_expires() {
        let state = EffectState::new();
        state.press("W", 0.0);
        assert_eq!(state.ripple_count(), 1);

        // Still alive just inside its lifetime, gone just outside it.
        state.advance(RIPPLE_LIFE_SECONDS - 0.01);
        assert_eq!(state.ripple_count(), 1);
        state.advance(RIPPLE_LIFE_SECONDS + 0.01);
        assert_eq!(state.ripple_count(), 0);
    }

    #[test]
    fn a_held_key_cannot_grow_the_ripple_list_without_bound() {
        let state = EffectState::new();
        for i in 0..500 {
            state.press("W", i as f64 * 0.001);
        }
        assert_eq!(state.ripple_count(), MAX_RIPPLES);
    }

    #[test]
    fn an_unknown_key_press_is_ignored() {
        let state = EffectState::new();
        state.press("NotAKey", 0.0);
        state.press("GameMode", 0.0);
        assert_eq!(state.ripple_count(), 0, "the game-mode key has no LED and no point");
        assert_eq!(state.heat_of("NotAKey"), 0.0);
    }

    #[test]
    fn heat_rises_with_presses_and_cools_at_a_frame_rate_independent_rate() {
        let state = EffectState::new();
        state.advance(0.0);
        state.press("W", 0.0);
        assert!((state.heat_of("W") - 0.45).abs() < 1e-9);
        state.press("W", 0.1);
        assert!((state.heat_of("W") - 0.9).abs() < 1e-9);
        // Heat saturates rather than climbing past full.
        state.press("W", 0.2);
        assert_eq!(state.heat_of("W"), 1.0);

        // One second of cooling at 0.35/s, reached in one step or in ten, must land the same.
        let coarse = EffectState::new();
        coarse.advance(0.0);
        coarse.press("W", 0.0);
        coarse.advance(1.0);
        let fine = EffectState::new();
        fine.advance(0.0);
        fine.press("W", 0.0);
        for i in 1..=10 {
            fine.advance(i as f64 * 0.1);
        }
        assert!(
            (coarse.heat_of("W") - fine.heat_of("W")).abs() < 1e-9,
            "{} vs {}",
            coarse.heat_of("W"),
            fine.heat_of("W")
        );
        assert!((coarse.heat_of("W") - 0.1).abs() < 1e-9, "0.45 - 0.35");
    }

    #[test]
    fn heat_is_dropped_once_it_is_effectively_zero() {
        let state = EffectState::new();
        state.advance(0.0);
        state.press("W", 0.0);
        // Cooling is capped at one second's worth per call, so this takes a few frames.
        for i in 1..=10 {
            state.advance(i as f64);
        }
        assert_eq!(state.heat_of("W"), 0.0);
        // And the entry itself is gone, so a long session does not accumulate every key ever hit.
        assert_eq!(state.history.lock().unwrap().heat.len(), 0);
    }

    #[test]
    fn a_long_pause_does_not_blank_the_heatmap_in_one_step() {
        // Waking from sleep delivers a huge elapsed time. Clamping it means the board fades out
        // over the next second rather than snapping to black.
        let state = EffectState::new();
        state.advance(0.0);
        state.press("W", 0.0);
        state.advance(3600.0);
        assert!(
            (state.heat_of("W") - 0.1).abs() < 1e-9,
            "one hour is clamped to one second of cooling, so 0.45 - 0.35 is left, not 0: {}",
            state.heat_of("W")
        );
        let state = EffectState::new();
        state.advance(0.0);
        state.press("W", 0.0);
        state.press("W", 0.0);
        state.press("W", 0.0);
        state.advance(3600.0);
        assert!(state.heat_of("W") > 0.6, "a full key keeps most of its heat: {}", state.heat_of("W"));
    }

    #[test]
    fn a_ripple_is_brightest_on_its_ring_and_fades_with_age() {
        let state = EffectState::new();
        let w = *point_for("W").unwrap();
        state.press("W", 0.0);
        let palette = vec![LedColor { r: 255, g: 255, b: 255 }, LedColor::BLACK];

        // At t=0 the ring has radius 0, so the pressed key itself is lit.
        let at_press = state.ripple_color(&w, 0.0, &palette, 1.0);
        assert!(at_press.r > 200, "{at_press:?}");
        // A distant key is not lit yet.
        let far = *point_for("Num0").unwrap();
        let early = state.ripple_color(&far, 0.0, &palette, 1.0);
        assert_eq!(early, LedColor::BLACK);
        // And the pressed key is dark again once the ring has moved on.
        let later = state.ripple_color(&w, 1.0, &palette, 1.0);
        assert!(later.r < at_press.r, "{later:?} should be dimmer than {at_press:?}");
    }

    #[test]
    fn a_ripple_recorded_in_the_future_is_skipped_rather_than_producing_a_negative_radius() {
        // Clocks are not guaranteed monotonic across a press on the input thread and a render on
        // the animation thread.
        let state = EffectState::new();
        state.press("W", 10.0);
        let w = *point_for("W").unwrap();
        let color = state.ripple_color(&w, 0.0, &[LedColor { r: 255, g: 0, b: 0 }], 1.0);
        assert_eq!(color, LedColor::BLACK);
    }

    #[test]
    fn intensity_is_clamped_so_a_hand_edited_profile_cannot_wrap_a_channel() {
        let state = EffectState::new();
        state.press("W", 0.0);
        let w = *point_for("W").unwrap();
        let palette = [LedColor { r: 100, g: 100, b: 100 }, LedColor::BLACK];
        let sane = state.ripple_color(&w, 0.0, &palette, 2.0);
        let absurd = state.ripple_color(&w, 0.0, &palette, 1_000.0);
        assert_eq!(sane, absurd);
        assert!(sane.r <= 100);
    }

    #[test]
    fn the_scan_bar_wraps_round_the_edge_instead_of_vanishing() {
        // A key at the far edge must be lit when the bar is at the near edge, or the sweep looks
        // like it restarts rather than continues.
        let at_start = bar(0.0, 0.0);
        assert!(at_start > 0.9, "{at_start}");
        let at_end_when_bar_at_start = bar(0.99, 0.0);
        assert!(at_end_when_bar_at_start > 0.9, "{at_end_when_bar_at_start}");
        // And the rest of the board is dim, not black: a black board reads as broken.
        let middle = bar(0.5, 0.0);
        assert!((0.0..0.1).contains(&middle) && middle > 0.0, "{middle}");
    }

    #[test]
    fn breathing_reaches_both_full_and_dark() {
        // Over one cycle the pulse has to touch 0 and 1, or the effect reads as a flicker.
        let samples: Vec<f64> = (0..200).map(|i| pulse(i as f64 / 200.0)).collect();
        assert!(samples.iter().cloned().fold(f64::MAX, f64::min) < 0.01);
        assert!(samples.iter().cloned().fold(0.0, f64::max) > 0.99);
    }

    #[test]
    fn noise_is_deterministic_and_in_range() {
        for (a, b) in [(0.0, 0.0), (0.5, 1.5), (123.456, 7.89)] {
            let v = noise(a, b);
            assert!((0.0..1.0).contains(&v), "noise({a}, {b}) = {v}");
            assert_eq!(v, noise(a, b));
        }
        assert_ne!(noise(0.1, 0.0), noise(0.2, 0.0));
    }

    #[test]
    fn the_direction_axis_runs_from_zero_to_one_the_right_way_round() {
        let left_edge = KeyPoint { led_id: 1, key_id: "a", x: 0.0, y: 0.0, seed: 0.0 };
        let right_edge = KeyPoint { led_id: 2, key_id: "b", x: 1.0, y: 1.0, seed: 0.0 };
        assert_eq!(axis(&left_edge, AnimationDirection::Right), 0.0);
        assert_eq!(axis(&right_edge, AnimationDirection::Right), 1.0);
        assert_eq!(axis(&left_edge, AnimationDirection::Left), 1.0);
        assert_eq!(axis(&left_edge, AnimationDirection::Down), 0.0);
        assert_eq!(axis(&right_edge, AnimationDirection::Up), 0.0);

        // Out starts at the middle; In starts at the edges.
        let middle = KeyPoint { led_id: 3, key_id: "c", x: 0.5, y: 0.5, seed: 0.0 };
        assert_eq!(axis(&middle, AnimationDirection::Out), 0.0);
        assert_eq!(axis(&middle, AnimationDirection::In), 1.0);
        assert!(axis(&left_edge, AnimationDirection::Out) > 0.9);
    }

    #[test]
    fn mixing_is_exact_at_both_ends_and_clamped_outside() {
        let a = LedColor { r: 0, g: 0, b: 0 };
        let b = LedColor { r: 100, g: 200, b: 255 };
        assert_eq!(mix(a, b, 0.0), a);
        assert_eq!(mix(a, b, 1.0), b);
        assert_eq!(mix(a, b, -1.0), a, "clamped, not extrapolated");
        assert_eq!(mix(a, b, 2.0), b);
        assert_eq!(mix(a, b, 0.5), LedColor { r: 50, g: 100, b: 127 });
    }

    #[test]
    fn rain_drops_move_along_the_board_over_time() {
        let state = EffectState::new();
        let top = *point_for("Escape").unwrap();
        // Over a few seconds, the drop has to be bright at this key at some point - otherwise the
        // effect never reaches it and the column stays dark.
        let lit = (0..400)
            .map(|i| state.drop(&top, i as f64 * 0.02, AnimationDirection::Down))
            .any(|v| v > 0.5);
        assert!(lit, "no drop ever reached the top row");
    }
}
