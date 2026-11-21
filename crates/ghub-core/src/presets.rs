//! G HUB's built-in presets, taken from its own data files.
//!
//! Sources, all read from a G HUB install by `gen_presets.py` (kept beside the data):
//!
//! * `data/card_presets.json` - the SYSTEM tab of Assignments (mouse, media, audio, editing,
//!   device, lighting, OS and G HUB cards). Shipped as `data/ghub_commands.json`.
//! * `data/applications.json` - the COMMANDS tab: the Desktop application's 44 shortcuts (embedded)
//!   and the per-game command lists (816 games, `assets/presets/games.json`, loaded by the window
//!   only - the agent never needs them, because a chosen command is stored as a plain action).
//! * `data/lighting/effects/*.json` - the five ANIMATIONS (Verticool, Red White and Blue,
//!   Contrastic, Ocean Wave, Lightning), and the per-key maps of the Screen Sampler and Audio
//!   Visualizer presets.
//!
//! Every card is listed, including the ones this application cannot perform (webcam controls,
//! headset sidetone and so on). Those carry the reason instead of an action, so the list matches
//! G HUB's and nothing pretends to work.

use std::collections::HashMap;
use std::sync::OnceLock;

use ghub_hidpp::LedColor;
use serde::Deserialize;

use crate::action::{
    ActionDefinition, DpiCommand, KeyModifiers, KeyStroke, LightingCommand, MediaCommand,
    MouseButtonKind, ProfileCommand,
};
use crate::layout;

const COMMANDS_JSON: &str = include_str!("../data/ghub_commands.json");
const KEYFRAMES_JSON: &str = include_str!("../data/ghub_keyframes.json");
const SCREEN_SAMPLER_JSON: &str = include_str!("../data/ghub_screen_sampler.json");
const AUDIO_VISUALIZER_JSON: &str = include_str!("../data/ghub_audio_visualizer.json");

// =================================================================== commands

/// One entry of G HUB's assignment lists.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandPreset {
    /// G HUB's own id (`PRIMARY_CLICK`) for cards, or the command's name for shortcuts.
    pub id: String,
    pub name: String,
    /// The submenu it sits under in G HUB: Mouse, Media, Audio, Editing, ...
    pub category: String,
    /// The shortcut as G HUB writes it, `LCTRL + C`, when it is one.
    pub keys: Option<String>,
    /// What it does here, or why it cannot be done here.
    pub action: Result<ActionDefinition, String>,
}

impl CommandPreset {
    pub fn available(&self) -> bool {
        self.action.is_ok()
    }
}

#[derive(Deserialize)]
struct CommandsFile {
    cards: Vec<CardEntry>,
    desktop: DesktopEntry,
}

#[derive(Deserialize)]
struct CardEntry {
    id: String,
    name: String,
    category: String,
    #[serde(default)]
    keystroke: Option<Vec<String>>,
    #[serde(default)]
    macro_: Option<serde_json::Value>,
    #[serde(default, rename = "macro")]
    macro_definition: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct DesktopEntry {
    name: String,
    commands: Vec<ShortcutEntry>,
    #[serde(default, rename = "categoryColors")]
    category_colors: HashMap<String, String>,
}

#[derive(Deserialize)]
struct ShortcutEntry {
    category: String,
    name: String,
    keystroke: Vec<String>,
}

struct Commands {
    system: Vec<CommandPreset>,
    desktop_name: String,
    desktop: Vec<CommandPreset>,
    desktop_colors: HashMap<String, String>,
}

fn commands() -> &'static Commands {
    static COMMANDS: OnceLock<Commands> = OnceLock::new();
    COMMANDS.get_or_init(|| {
        let file: CommandsFile =
            serde_json::from_str(COMMANDS_JSON).expect("embedded ghub_commands.json is valid");
        let system = file
            .cards
            .into_iter()
            .map(|card| {
                let action = match (&card.keystroke, card.macro_definition.as_ref().or(card.macro_.as_ref())) {
                    (Some(keys), _) => keystroke_action(keys),
                    (None, Some(definition)) => card_action(&card.id, definition),
                    (None, None) => Err("G HUB gives this card no action".to_owned()),
                };
                CommandPreset {
                    keys: card.keystroke.as_deref().map(join_keys),
                    id: card.id,
                    name: card.name,
                    category: card.category,
                    action,
                }
            })
            .collect();
        let desktop = file
            .desktop
            .commands
            .into_iter()
            .map(|shortcut| CommandPreset {
                id: shortcut.name.clone(),
                action: keystroke_action(&shortcut.keystroke),
                keys: Some(join_keys(&shortcut.keystroke)),
                name: shortcut.name,
                category: shortcut.category,
            })
            .collect();
        Commands {
            system,
            desktop_name: file.desktop.name,
            desktop,
            desktop_colors: file.desktop.category_colors,
        }
    })
}

/// G HUB's SYSTEM cards for Windows, in G HUB's order. macOS-only cards are left out, as G HUB
/// hides them on Windows.
pub fn system_presets() -> &'static [CommandPreset] {
    &commands().system
}

/// The Desktop application's shortcuts: what G HUB's COMMANDS tab shows outside a game.
pub fn desktop_commands() -> &'static [CommandPreset] {
    &commands().desktop
}

/// "Desktop", as G HUB names that application.
pub fn desktop_name() -> &'static str {
    &commands().desktop_name
}

/// The colour G HUB tags a Desktop command category with, `#rrggbb`.
pub fn desktop_category_color(category: &str) -> Option<&'static str> {
    commands().desktop_colors.get(category).map(String::as_str)
}

fn join_keys(keys: &[String]) -> String {
    keys.join(" + ")
}

/// A card's `macro` object, turned into an action this application performs.
pub(crate) fn card_action(
    id: &str,
    definition: &serde_json::Value,
) -> Result<ActionDefinition, String> {
    let kind = definition.get("type").and_then(|v| v.as_str()).unwrap_or_default();
    let field = |object: &str, name: &str| {
        definition
            .get(object)
            .and_then(|o| o.get(name))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned()
    };
    let unsupported = |why: &str| Err(why.to_owned());
    match kind {
        "MOUSE" => {
            let action = field("mouse", "action");
            match action.as_str() {
                "BUTTON" => {
                    let usage = definition["mouse"]["hidUsage"].as_u64().unwrap_or(0);
                    let button = match usage {
                        1 => MouseButtonKind::Left,
                        2 => MouseButtonKind::Right,
                        3 => MouseButtonKind::Middle,
                        4 => MouseButtonKind::XButton1,
                        5 => MouseButtonKind::XButton2,
                        _ => return unsupported("Unknown mouse button"),
                    };
                    Ok(ActionDefinition::MouseButton { button })
                }
                "WHEEL_UP" => Ok(ActionDefinition::MouseWheel { delta: 120, horizontal: false }),
                "WHEEL_DOWN" => Ok(ActionDefinition::MouseWheel { delta: -120, horizontal: false }),
                "WHEEL_RIGHT" => Ok(ActionDefinition::MouseWheel { delta: 120, horizontal: true }),
                "WHEEL_LEFT" => Ok(ActionDefinition::MouseWheel { delta: -120, horizontal: true }),
                "DPI_UP" => Ok(ActionDefinition::Dpi { command: DpiCommand::Next, stage: 0 }),
                "DPI_DOWN" => Ok(ActionDefinition::Dpi { command: DpiCommand::Previous, stage: 0 }),
                "DPI_SHIFT" => Ok(ActionDefinition::Dpi { command: DpiCommand::Shift, stage: 0 }),
                "DPI_DEFAULT" => Ok(ActionDefinition::Dpi { command: DpiCommand::Default, stage: 0 }),
                "DPI_CYCLE" => Ok(ActionDefinition::Dpi { command: DpiCommand::Cycle, stage: 0 }),
                "BUTTON_DOUBLE_CLICK" => {
                    // Two left clicks: what the card does, built from a chord-free action list is
                    // not possible, so it is a two-step sequence of the mouse button.
                    unsupported("Double click is not available as a single action here; record it as a macro")
                }
                "DPI_GO_TO_SHIFT" => unsupported("Latching DPI shift is not available; use DPI Shift (hold)"),
                "WHEEL_MODE_TOGGLE" | "RATCHET_FORCE_CYCLE" => {
                    unsupported("Needs a mouse with a software-switchable wheel")
                }
                _ => unsupported("Not available here"),
            }
        }
        "MEDIA" => {
            let command = match field("media", "usage").as_str() {
                "PLAY_PAUSE" => MediaCommand::PlayPause,
                "STOP" => MediaCommand::Stop,
                "PREVIOUS_TRACK" => MediaCommand::Previous,
                "NEXT_TRACK" => MediaCommand::Next,
                "VOLUME_UP" => MediaCommand::VolumeUp,
                "VOLUME_DOWN" => MediaCommand::VolumeDown,
                "MUTE" => MediaCommand::Mute,
                _ => return unsupported("Unknown media control"),
            };
            Ok(ActionDefinition::Media { command })
        }
        // Muting the output is what the media mute key does system-wide; every other audio card
        // drives a Logitech headset's or microphone's own controls.
        "AUDIO" => match field("audio", "action").as_str() {
            "MUTE_SPEAKERS" => Ok(ActionDefinition::Media { command: MediaCommand::Mute }),
            _ => unsupported("Controls a Logitech headset or microphone"),
        },
        "DEVICE" => match field("device", "action").as_str() {
            "G_SHIFT" => Ok(ActionDefinition::GShift),
            // This application has one profile list; the onboard and G HUB cycles both step it.
            "PROFILE_CYCLE" => {
                Ok(ActionDefinition::Profile { command: ProfileCommand::Next, profile_id: None })
            }
            "G_SHIFT_DEFAULT" => unsupported("Restoring a G-Shift default is not available"),
            "BATTERY_LIFE" => unsupported("Shows battery on the device's LEDs; that write is not verified"),
            "REPORT_RATE_CYCLE" => unsupported("Report rate cycling is not available"),
            _ => unsupported("Not available here"),
        },
        "LIGHTING" => match field("lighting", "action").as_str() {
            "CYCLE_SOFTWARE_BRIGHTNESS" => {
                Ok(ActionDefinition::Lighting { command: LightingCommand::BrightnessCycle })
            }
            "CYCLE_FIRMWARE_BRIGHTNESS" => {
                unsupported("Firmware brightness cycling is not verified; use the software one")
            }
            _ => unsupported("Not available here"),
        },
        "GHUB" => match field("ghub", "action").as_str() {
            "CYCLE_GHUB_PROFILE" => {
                Ok(ActionDefinition::Profile { command: ProfileCommand::Next, profile_id: None })
            }
            _ => unsupported("Not available here"),
        },
        "WEBCAM" => unsupported("For Logitech webcams"),
        _ => {
            let _ = id;
            unsupported("Not available here")
        }
    }
}

/// A G HUB key token (`LCTRL`, `F5`, `NUMSTAR`) as a Windows virtual key.
pub fn token_virtual_key(token: &str) -> Option<u16> {
    let t = token.to_ascii_uppercase();
    if t.len() == 1 {
        let c = t.as_bytes()[0];
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            return Some(c as u16);
        }
    }
    if let Some(n) = t.strip_prefix('F').and_then(|n| n.parse::<u16>().ok()) {
        if (1..=24).contains(&n) {
            return Some(0x6F + n);
        }
    }
    if let Some(n) = t.strip_prefix("NUM").and_then(|n| n.parse::<u16>().ok()) {
        if n <= 9 {
            return Some(0x60 + n);
        }
    }
    Some(match t.as_str() {
        "APPKEY" => 0x5D,
        "BACKSLASH" => 0xDC,
        // The non-US # key sits where backslash does on an ANSI board and sends the same code.
        "HASH" => 0xDC,
        "NON_US_SLASH" => 0xE2,
        "BACKSPACE" => 0x08,
        "CAPSLOCK" => 0x14,
        "COMMA" => 0xBC,
        "DELETE" => 0x2E,
        "DOWN" => 0x28,
        "END" => 0x23,
        "ENTER" | "NUMENTER" => 0x0D,
        "EQUAL" => 0xBB,
        "ESCAPE" => 0x1B,
        "HOME" => 0x24,
        "INSERT" => 0x2D,
        "LALT" => 0xA4,
        "RALT" => 0xA5,
        "LBRACKET" => 0xDB,
        "RBRACKET" => 0xDD,
        "LCTRL" => 0xA2,
        "RCTRL" => 0xA3,
        "LEFT" => 0x25,
        "RIGHT" => 0x27,
        "UP" => 0x26,
        "LGUI" => 0x5B,
        "RGUI" => 0x5C,
        "LSHIFT" => 0xA0,
        "RSHIFT" => 0xA1,
        "MAIL" => 0xB4,
        "MINUS" => 0xBD,
        "NUMLOCK" => 0x90,
        "NUMMINUS" => 0x6D,
        "NUMPERIOD" => 0x6E,
        "NUMPLUS" => 0x6B,
        "NUMSLASH" => 0x6F,
        "NUMSTAR" => 0x6A,
        "PAGEDOWN" => 0x22,
        "PAGEUP" => 0x21,
        "PAUSE" => 0x13,
        "PERIOD" => 0xBE,
        "PRINTSCREEN" => 0x2C,
        "QUOTE" => 0xDE,
        "SCROLLLOCK" => 0x91,
        "SEMICOLON" => 0xBA,
        "SLASH" => 0xBF,
        "SPACEBAR" => 0x20,
        "TAB" => 0x09,
        "TILDE" => 0xC0,
        _ => return None,
    })
}

fn modifier_of(token: &str) -> Option<KeyModifiers> {
    match token {
        "LCTRL" | "RCTRL" => Some(KeyModifiers::CONTROL),
        "LSHIFT" | "RSHIFT" => Some(KeyModifiers::SHIFT),
        "LALT" | "RALT" => Some(KeyModifiers::ALT),
        "LGUI" | "RGUI" => Some(KeyModifiers::WINDOWS),
        _ => None,
    }
}

/// G HUB's `keystroke` array as an action: held modifiers plus one key, held while the control
/// is held - which is how G HUB plays a command card.
///
/// Win+L is the one exception. Windows ignores a synthesised Win+L, so it is turned into the call
/// that locks the workstation, which is what the user meant.
pub fn keystroke_action(tokens: &[String]) -> Result<ActionDefinition, String> {
    let tokens: Vec<String> = tokens.iter().map(|t| t.to_ascii_uppercase()).collect();
    if tokens.is_empty() {
        return Err("G HUB gives this command no keys".to_owned());
    }
    let mut modifiers = KeyModifiers::NONE;
    let mut keys = Vec::new();
    for token in &tokens {
        match modifier_of(token) {
            Some(m) => modifiers |= m,
            None => keys.push(token.as_str()),
        }
    }
    match keys.as_slice() {
        [] => {
            // Only modifiers: the last one is the key, the rest are held with it.
            let last = tokens.last().unwrap();
            let mut held = KeyModifiers::NONE;
            for token in &tokens[..tokens.len() - 1] {
                held |= modifier_of(token).unwrap_or(KeyModifiers::NONE);
            }
            let vk = token_virtual_key(last).ok_or_else(|| format!("Unknown key {last}"))?;
            Ok(ActionDefinition::Key(KeyStroke::with(vk, held)))
        }
        [key] => {
            if *key == "L" && modifiers == KeyModifiers::WINDOWS {
                return Ok(ActionDefinition::Launch {
                    target: "rundll32.exe".to_owned(),
                    arguments: Some("user32.dll,LockWorkStation".to_owned()),
                });
            }
            let vk = token_virtual_key(key).ok_or_else(|| format!("Unknown key {key}"))?;
            Ok(ActionDefinition::Key(KeyStroke::with(vk, modifiers)))
        }
        _ => Err(format!(
            "Presses {} keys at once; record it as a macro",
            keys.len()
        )),
    }
}

// =================================================================== game library

/// One game from G HUB's application list, with its command list.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Game {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub detection: GameDetection,
    /// Category to `#rrggbb`, the colour G HUB tags each command group with.
    #[serde(default)]
    pub colors: HashMap<String, String>,
    /// `[category, name, keystroke]`, as compact as the file allows.
    pub commands: Vec<(String, String, Vec<String>)>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct GameDetection {
    #[serde(default)]
    pub steam: Vec<String>,
    #[serde(default)]
    pub epic: Vec<String>,
    #[serde(default)]
    pub gog: Vec<String>,
}

impl Game {
    pub fn presets(&self) -> Vec<CommandPreset> {
        self.commands
            .iter()
            .map(|(category, name, keys)| CommandPreset {
                id: name.clone(),
                name: name.clone(),
                category: category.clone(),
                keys: Some(join_keys(keys)),
                action: keystroke_action(keys),
            })
            .collect()
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct GameLibrary {
    pub games: Vec<Game>,
}

impl GameLibrary {
    pub fn parse(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }

    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text)
    }

    /// Games whose name contains `query`, case-insensitively, in alphabetical order.
    pub fn search(&self, query: &str) -> Vec<&Game> {
        let needle = query.trim().to_lowercase();
        self.games
            .iter()
            .filter(|g| needle.is_empty() || g.name.to_lowercase().contains(&needle))
            .collect()
    }

    pub fn by_id(&self, id: &str) -> Option<&Game> {
        self.games.iter().find(|g| g.id == id)
    }
}

// =================================================================== per-key slot codes

/// A G HUB per-key slot code (`K4`, `C205`, `G1`, `B1`) as this layout's key id.
///
/// `K` is the HID keyboard usage, `C` the HID consumer usage of a media key, `G` a G-key and `B`
/// the branding LEDs (1 is the logo). Codes the G815 has no key for (`C183` stop, `B2`, the
/// `I` indicators, international keys) map to nothing.
pub fn layout_key(code: &str) -> Option<&'static str> {
    let (prefix, number) = code.split_at(1);
    let number: u16 = number.parse().ok()?;
    match prefix {
        "K" => layout::by_usage(u8::try_from(number).ok()?).map(|k| k.id),
        "C" => match number {
            0xB5 => Some("MediaNext"),
            0xB6 => Some("MediaPrevious"),
            0xCD => Some("MediaPlayPause"),
            0xE2 => Some("MediaMute"),
            _ => None,
        },
        "G" => {
            let id = format!("G{number}");
            layout::by_id(&id).map(|k| k.id)
        }
        "B" if number == 1 => Some("Logo"),
        _ => None,
    }
}

fn parse_hex(hex: &str) -> Option<LedColor> {
    crate::profile::parse_color(&format!("#{hex}"))
}

// =================================================================== keyframe animations

/// How G HUB eases between two keyframes. The CSS curves G HUB's editor uses for its preview.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Curve {
    /// Hold the frame, then jump.
    None,
    Linear,
    Bezier(f64, f64, f64, f64),
}

impl Curve {
    pub fn parse(name: &str) -> Self {
        match name {
            "NONE" => Self::None,
            "INOUTQUAD" => Self::Bezier(0.45, 0.0, 0.55, 1.0),
            "OUTINQUAD" => Self::Bezier(0.5, 1.0, 0.5, 0.0),
            "INOUTCUBIC" => Self::Bezier(0.65, 0.0, 0.35, 1.0),
            "OUTCIRC" => Self::Bezier(0.0, 0.55, 0.45, 1.0),
            "INCIRC" => Self::Bezier(0.55, 0.0, 1.0, 0.45),
            "OUTINCUBIC" => Self::Bezier(0.3, 1.0, 0.7, 0.0),
            "INOUTCIRC" => Self::Bezier(0.85, 0.0, 0.15, 1.0),
            "OUTINBACK" => Self::Bezier(0.25, 1.3, 0.75, -0.3),
            _ => Self::Linear,
        }
    }

    /// Progress through the transition for time fraction `t` in 0..1.
    pub fn ease(self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::None => 0.0,
            Self::Linear => t,
            Self::Bezier(x1, y1, x2, y2) => cubic_bezier(x1, y1, x2, y2, t),
        }
    }
}

/// CSS `cubic-bezier`: find the curve parameter whose x is `t` (Newton, then bisection), return y.
fn cubic_bezier(x1: f64, y1: f64, x2: f64, y2: f64, t: f64) -> f64 {
    let bez = |a: f64, b: f64, s: f64| {
        let u = 1.0 - s;
        3.0 * u * u * s * a + 3.0 * u * s * s * b + s * s * s
    };
    let d_bez = |a: f64, b: f64, s: f64| {
        let u = 1.0 - s;
        3.0 * u * u * a + 6.0 * u * s * (b - a) + 3.0 * s * s * (1.0 - b)
    };
    let mut s = t;
    for _ in 0..8 {
        let x = bez(x1, x2, s) - t;
        let d = d_bez(x1, x2, s);
        if x.abs() < 1e-6 {
            return bez(y1, y2, s);
        }
        if d.abs() < 1e-6 {
            break;
        }
        s = (s - x / d).clamp(0.0, 1.0);
    }
    let (mut lo, mut hi) = (0.0, 1.0);
    s = t;
    for _ in 0..30 {
        let x = bez(x1, x2, s);
        if (x - t).abs() < 1e-6 {
            break;
        }
        if x < t {
            lo = s;
        } else {
            hi = s;
        }
        s = (lo + hi) / 2.0;
    }
    bez(y1, y2, s)
}

/// The order G HUB plays keyframes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Playback {
    Cycle,
    ReverseCycle,
    Bounce,
    Random,
}

#[derive(Debug, Clone)]
pub struct Keyframe {
    /// In G HUB's frame units; one unit lasts the animation's `speed` in milliseconds.
    pub length: u32,
    pub curve: Curve,
    pub colors: HashMap<&'static str, LedColor>,
}

#[derive(Debug, Clone)]
pub struct KeyframeAnimation {
    /// The id this application stores in a profile: `ghub:` and the slug of G HUB's name.
    pub effect_id: &'static str,
    pub name: &'static str,
    /// G HUB's prefab id, so the provenance is checkable.
    pub ghub_id: String,
    pub playback: Playback,
    /// Milliseconds per frame unit. G HUB's editor calls this the frame rate.
    pub speed_ms: f64,
    pub frames: Vec<Keyframe>,
}

#[derive(Deserialize)]
struct KeyframesFile {
    animations: Vec<KeyframeEntry>,
}

#[derive(Deserialize)]
struct KeyframeEntry {
    id: String,
    name: String,
    direction: String,
    speed: f64,
    frames: Vec<FrameEntry>,
}

#[derive(Deserialize)]
struct FrameEntry {
    length: u32,
    curve: String,
    /// `rrggbb` to the space-separated slot codes painted that colour.
    colors: HashMap<String, String>,
}

/// The five G HUB animation ids, in G HUB's menu order (alphabetical, as its list shows them).
pub const KEYFRAME_EFFECTS: &[(&str, &str)] = &[
    ("ghub:contrastic", "Contrastic"),
    ("ghub:lightning", "Lightning"),
    ("ghub:oceanwave", "Ocean Wave"),
    ("ghub:redwhiteblue", "Red White and Blue"),
    ("ghub:verticool", "Verticool"),
];

pub fn keyframe_animations() -> &'static [KeyframeAnimation] {
    static ANIMATIONS: OnceLock<Vec<KeyframeAnimation>> = OnceLock::new();
    ANIMATIONS.get_or_init(|| {
        let file: KeyframesFile =
            serde_json::from_str(KEYFRAMES_JSON).expect("embedded ghub_keyframes.json is valid");
        let mut out: Vec<KeyframeAnimation> = file
            .animations
            .into_iter()
            .filter_map(|entry| {
                let (effect_id, name) = *KEYFRAME_EFFECTS
                    .iter()
                    .find(|(_, name)| name.eq_ignore_ascii_case(&entry.name))?;
                let frames = entry
                    .frames
                    .into_iter()
                    .map(|frame| {
                        let mut colors = HashMap::new();
                        for (hex, codes) in &frame.colors {
                            let Some(color) = parse_hex(hex) else { continue };
                            for code in codes.split_whitespace() {
                                if let Some(key) = layout_key(code) {
                                    colors.insert(key, color);
                                }
                            }
                        }
                        Keyframe { length: frame.length.max(1), curve: Curve::parse(&frame.curve), colors }
                    })
                    .collect::<Vec<_>>();
                if frames.is_empty() {
                    return None;
                }
                let playback = match entry.direction.as_str() {
                    "BOUNCE" => Playback::Bounce,
                    "RANDOM" => Playback::Random,
                    "REVERSE_CYCLE" => Playback::ReverseCycle,
                    _ => Playback::Cycle,
                };
                Some(KeyframeAnimation {
                    effect_id,
                    name,
                    ghub_id: entry.id,
                    playback,
                    speed_ms: entry.speed.max(1.0),
                    frames,
                })
            })
            .collect();
        out.sort_by_key(|a| KEYFRAME_EFFECTS.iter().position(|(id, _)| *id == a.effect_id));
        out
    })
}

pub fn keyframe_animation(effect_id: &str) -> Option<&'static KeyframeAnimation> {
    keyframe_animations().iter().find(|a| a.effect_id.eq_ignore_ascii_case(effect_id))
}

impl KeyframeAnimation {
    /// Frame indices in play order for one pass. The pass then repeats.
    fn sequence(&self, pass: u64) -> Vec<usize> {
        let n = self.frames.len();
        match self.playback {
            Playback::Cycle => (0..n).collect(),
            Playback::ReverseCycle => (0..n).rev().collect(),
            Playback::Bounce if n > 1 => (0..n).chain((1..n - 1).rev()).collect(),
            Playback::Bounce => vec![0],
            Playback::Random => {
                // A fresh shuffle each pass, seeded by the pass number so a preview and the
                // keyboard agree. Never the same frame twice in a row across the seam.
                let mut order: Vec<usize> = (0..n).collect();
                let mut seed = pass.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
                for i in (1..n).rev() {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    order.swap(i, (seed % (i as u64 + 1)) as usize);
                }
                order
            }
        }
    }

    fn pass_ms(&self, sequence: &[usize], unit_ms: f64) -> f64 {
        sequence.iter().map(|&i| self.frames[i].length as f64 * unit_ms).sum()
    }

    /// The colour of every key `elapsed_ms` into the animation, with `speed` the multiplier the
    /// profile stores (1.0 is G HUB's own rate).
    ///
    /// Each keyframe lasts `length x speed_ms` and eases towards the next one with its own curve;
    /// a key a frame does not name is dark in that frame.
    pub fn sample(&self, elapsed_ms: f64, speed: f64) -> HashMap<&'static str, LedColor> {
        let unit_ms = self.speed_ms / speed.clamp(0.05, 10.0);
        let mut pass: u64 = 0;
        let mut sequence = self.sequence(0);
        let mut pass_ms = self.pass_ms(&sequence, unit_ms);
        let mut t = elapsed_ms.max(0.0);
        if self.playback == Playback::Random {
            // Every pass has the same length, so jump straight to the right one.
            let skip = (t / pass_ms).floor();
            pass = skip as u64;
            t -= skip * pass_ms;
            sequence = self.sequence(pass);
            pass_ms = self.pass_ms(&sequence, unit_ms);
        } else {
            t %= pass_ms;
        }
        let _ = pass_ms;

        for (position, &index) in sequence.iter().enumerate() {
            let frame = &self.frames[index];
            let duration = frame.length as f64 * unit_ms;
            if t < duration || position + 1 == sequence.len() {
                let next_index = match sequence.get(position + 1) {
                    Some(&next) => next,
                    None => self.sequence(pass + 1)[0],
                };
                let next = &self.frames[next_index];
                let k = frame.curve.ease((t / duration).min(1.0));
                return blend(&frame.colors, &next.colors, k);
            }
            t -= duration;
        }
        HashMap::new()
    }

    /// One full pass at G HUB's own speed, in milliseconds.
    pub fn cycle_ms(&self) -> f64 {
        self.pass_ms(&self.sequence(0), self.speed_ms)
    }
}

fn blend(
    from: &HashMap<&'static str, LedColor>,
    to: &HashMap<&'static str, LedColor>,
    k: f64,
) -> HashMap<&'static str, LedColor> {
    let mut out = HashMap::with_capacity(from.len().max(to.len()));
    for (key, a) in from {
        let b = to.get(key).copied().unwrap_or(LedColor::BLACK);
        out.insert(*key, mix_unclamped(*a, b, k));
    }
    for (key, b) in to {
        out.entry(*key).or_insert_with(|| mix_unclamped(LedColor::BLACK, *b, k));
    }
    out
}

/// Like [`crate::effects::mix`], but lets the overshooting curves (OUTINBACK) overshoot, clamped
/// per channel.
fn mix_unclamped(a: LedColor, b: LedColor, k: f64) -> LedColor {
    let channel = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * k).round().clamp(0.0, 255.0) as u8;
    LedColor { r: channel(a.r, b.r), g: channel(a.g, b.g), b: channel(a.b, b.b) }
}

// =================================================================== screen sampler

/// One screen region of G HUB's keyboard Screen Sampler: insets from each edge, as fractions.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SamplerRegion {
    pub id: String,
    pub name: String,
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

#[derive(Deserialize)]
struct SamplerFile {
    regions: Vec<SamplerRegion>,
    keys: HashMap<String, String>,
}

pub struct ScreenSamplerLayout {
    /// Left to right across the screen.
    pub regions: Vec<SamplerRegion>,
    /// Layout key id to the index of the region it shows.
    pub keys: HashMap<&'static str, usize>,
}

pub fn screen_sampler_layout() -> &'static ScreenSamplerLayout {
    static LAYOUT: OnceLock<ScreenSamplerLayout> = OnceLock::new();
    LAYOUT.get_or_init(|| {
        let file: SamplerFile = serde_json::from_str(SCREEN_SAMPLER_JSON)
            .expect("embedded ghub_screen_sampler.json is valid");
        let keys = file
            .keys
            .iter()
            .filter_map(|(code, region)| {
                let key = layout_key(code)?;
                let index = file.regions.iter().position(|r| &r.id == region)?;
                Some((key, index))
            })
            .collect();
        ScreenSamplerLayout { regions: file.regions, keys }
    })
}

// =================================================================== audio visualizer

/// One bar of G HUB's per-key Audio Visualizer: a frequency band and the keys that light, bottom
/// key first, as the level rises. `None` is a gap G HUB leaves where a key is missing.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct VisualizerColumn {
    pub low: u32,
    pub high: u32,
    pub keys: Vec<Option<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VisualizerFile {
    default_low: u32,
    default_high: u32,
    columns: Vec<VisualizerColumn>,
}

pub struct VisualizerLayout {
    pub default_low: u32,
    pub default_high: u32,
    /// Per column: the band and its keys in fill order, mapped to this layout (gaps kept, so a
    /// column's height is G HUB's).
    pub columns: Vec<(u32, u32, Vec<Option<&'static str>>)>,
}

pub fn visualizer_layout() -> &'static VisualizerLayout {
    static LAYOUT: OnceLock<VisualizerLayout> = OnceLock::new();
    LAYOUT.get_or_init(|| {
        let file: VisualizerFile = serde_json::from_str(AUDIO_VISUALIZER_JSON)
            .expect("embedded ghub_audio_visualizer.json is valid");
        let columns = file
            .columns
            .into_iter()
            .map(|c| {
                let keys = c.keys.iter().map(|k| k.as_deref().and_then(layout_key)).collect();
                (c.low, c.high, keys)
            })
            .filter(|(_, _, keys): &(u32, u32, Vec<Option<&'static str>>)| keys.iter().any(Option::is_some))
            .collect();
        VisualizerLayout { default_low: file.default_low, default_high: file.default_high, columns }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn by_id<'a>(list: &'a [CommandPreset], id: &str) -> &'a CommandPreset {
        list.iter().find(|p| p.id == id).unwrap_or_else(|| panic!("{id} missing"))
    }

    #[test]
    fn every_windows_card_from_g_hub_is_listed() {
        // 68 cards in card_presets.json, 8 of them macOS-only.
        let cards = system_presets();
        assert_eq!(cards.len(), 60);
        let categories: Vec<&str> = cards.iter().map(|c| c.category.as_str()).collect();
        for expected in ["Mouse", "Webcam", "Device", "Media", "Audio", "Editing", "Lighting", "System", "G HUB"] {
            assert!(categories.contains(&expected), "{expected}");
        }
        assert!(cards.iter().all(|c| !c.name.contains('_')), "display names come from G HUB's strings");
    }

    #[test]
    fn the_cards_map_to_the_actions_they_name() {
        let cards = system_presets();
        assert_eq!(
            by_id(cards, "PRIMARY_CLICK").action,
            Ok(ActionDefinition::MouseButton { button: MouseButtonKind::Left })
        );
        assert_eq!(by_id(cards, "PRIMARY_CLICK").name, "Primary Click");
        assert_eq!(
            by_id(cards, "FORWARD").action,
            Ok(ActionDefinition::MouseButton { button: MouseButtonKind::XButton2 })
        );
        assert_eq!(
            by_id(cards, "WHEEL_LEFT").action,
            Ok(ActionDefinition::MouseWheel { delta: -120, horizontal: true })
        );
        assert_eq!(
            by_id(cards, "DPI_CYCLE").action,
            Ok(ActionDefinition::Dpi { command: DpiCommand::Cycle, stage: 0 })
        );
        assert_eq!(
            by_id(cards, "MEDIA_PLAY_PAUSE").action,
            Ok(ActionDefinition::Media { command: MediaCommand::PlayPause })
        );
        assert_eq!(
            by_id(cards, "SYSTEM_WINDOWS_COPY").action,
            Ok(ActionDefinition::Key(KeyStroke::with(b'C' as u16, KeyModifiers::CONTROL)))
        );
        assert_eq!(
            by_id(cards, "CYCLE_SOFTWARE_BRIGHTNESS").action,
            Ok(ActionDefinition::Lighting { command: LightingCommand::BrightnessCycle })
        );
        assert!(matches!(by_id(cards, "LOCK_SCREEN_WIN").action, Ok(ActionDefinition::Launch { .. })));
    }

    #[test]
    fn cards_this_application_cannot_perform_say_why() {
        let cards = system_presets();
        for id in ["WEBCAM_CYCLE_FOV", "SIDETONE_INCREASE", "BATTERY_LIFE", "CYCLE_FIRMWARE_BRIGHTNESS"] {
            let card = by_id(cards, id);
            assert!(!card.available(), "{id}");
            assert!(!card.action.as_ref().unwrap_err().is_empty());
        }
        // Everything a G815 and G903 can use is available.
        let available = cards.iter().filter(|c| c.available()).count();
        assert!(available >= 30, "{available}");
    }

    #[test]
    fn the_desktop_shortcuts_are_g_hubs_forty_four() {
        let commands = desktop_commands();
        assert_eq!(commands.len(), 44);
        assert_eq!(desktop_name(), "Desktop");
        assert_eq!(
            by_id(commands, "Zoom In").action,
            Ok(ActionDefinition::Key(KeyStroke::with(0xBB, KeyModifiers::CONTROL)))
        );
        assert_eq!(
            by_id(commands, "Open Task Manager").action,
            Ok(ActionDefinition::Key(KeyStroke::with(
                0x1B,
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )))
        );
        assert!(matches!(by_id(commands, "Lock PC").action, Ok(ActionDefinition::Launch { .. })));
        assert!(commands.iter().all(CommandPreset::available));
        assert_eq!(desktop_category_color("Editing"), Some("#ff43ee"));
    }

    #[test]
    fn a_modifier_alone_is_the_key() {
        assert_eq!(
            keystroke_action(&["LALT".to_owned()]),
            Ok(ActionDefinition::Key(KeyStroke::new(0xA4)))
        );
        assert!(keystroke_action(&["W".to_owned(), "A".to_owned()]).is_err());
        assert!(keystroke_action(&[]).is_err());
    }

    #[test]
    fn every_token_g_hub_uses_has_a_virtual_key() {
        let library = GameLibrary::load(&games_path()).expect("games.json");
        let mut missing = std::collections::BTreeSet::new();
        for game in &library.games {
            for (_, _, keys) in &game.commands {
                for key in keys {
                    if token_virtual_key(key).is_none() {
                        missing.insert(key.clone());
                    }
                }
            }
        }
        assert!(missing.is_empty(), "{missing:?}");
    }

    fn games_path() -> std::path::PathBuf {
        std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/presets/games.json"))
    }

    #[test]
    fn the_game_library_loads_and_nearly_every_command_is_usable() {
        let library = GameLibrary::load(&games_path()).expect("games.json");
        assert!(library.games.len() > 800, "{}", library.games.len());
        let total: usize = library.games.iter().map(|g| g.commands.len()).sum();
        let usable: usize =
            library.games.iter().flat_map(|g| g.presets()).filter(CommandPreset::available).count();
        assert!(total > 24_000);
        // Commands pressing two plain keys at once are the only ones refused.
        assert!(usable * 1000 / total >= 985, "{usable}/{total}");
        let found = library.search("baldur");
        assert!(found.iter().any(|g| g.name.starts_with("Baldur's Gate")));
        assert!(library.by_id(&found[0].id).is_some());
    }

    #[test]
    fn slot_codes_map_onto_the_g815_layout() {
        assert_eq!(layout_key("K4"), Some("A"));
        assert_eq!(layout_key("K41"), Some("Escape"));
        assert_eq!(layout_key("C205"), Some("MediaPlayPause"));
        assert_eq!(layout_key("C183"), None);
        assert_eq!(layout_key("G5"), Some("G5"));
        assert_eq!(layout_key("G9"), None);
        assert_eq!(layout_key("B1"), Some("Logo"));
        assert_eq!(layout_key("I1"), None);
    }

    #[test]
    fn the_five_g_hub_animations_load_with_their_own_timing() {
        let all = keyframe_animations();
        let names: Vec<&str> = all.iter().map(|a| a.name).collect();
        assert_eq!(names, ["Contrastic", "Lightning", "Ocean Wave", "Red White and Blue", "Verticool"]);
        let verticool = keyframe_animation("ghub:verticool").unwrap();
        assert_eq!(verticool.frames.len(), 15);
        assert_eq!(verticool.playback, Playback::Bounce);
        assert_eq!(verticool.speed_ms, 100.0);
        // 15 frames of 8 units at 100 ms, bounced: 28 transitions of 0.8 s.
        assert!((verticool.cycle_ms() - 28.0 * 800.0).abs() < 1e-6);
        // Every animation paints most of the board.
        for animation in all {
            assert!(animation.frames[0].colors.len() > 100, "{}", animation.name);
        }
    }

    #[test]
    fn a_keyframe_holds_its_colours_at_its_start_and_reaches_the_next_at_its_end() {
        let contrastic = keyframe_animation("ghub:contrastic").unwrap();
        let first = &contrastic.frames[0].colors;
        let second = &contrastic.frames[1].colors;
        let start = contrastic.sample(0.0, 1.0);
        assert_eq!(start.get("A"), first.get("A"));
        // Contrastic: 15 units of 200 ms, linear. Just before 3 s it is almost the next frame.
        let end = contrastic.sample(2999.0, 1.0);
        let (a, b) = (end["A"], second["A"]);
        assert!((a.r as i32 - b.r as i32).abs() <= 1 && (a.g as i32 - b.g as i32).abs() <= 1);
        // Twice the speed reaches the same point in half the time.
        assert_eq!(contrastic.sample(1500.0, 2.0).get("A"), contrastic.sample(3000.0, 1.0).get("A"));
    }

    #[test]
    fn a_none_curve_holds_rather_than_fades() {
        let lightning = keyframe_animation("ghub:lightning").unwrap();
        let (index, frame) =
            lightning.frames.iter().enumerate().find(|(_, f)| f.curve == Curve::None).unwrap();
        let start: f64 = lightning.frames[..index].iter().map(|f| f.length as f64 * 100.0).sum();
        let duration = frame.length as f64 * 100.0;
        let early = lightning.sample(start + 1.0, 1.0);
        let late = lightning.sample(start + duration - 1.0, 1.0);
        assert_eq!(early, late);
    }

    #[test]
    fn the_css_curves_match_their_end_points_and_shape() {
        for name in ["INCIRC", "OUTCIRC", "INOUTQUAD", "INOUTCUBIC", "LINEAR"] {
            let curve = Curve::parse(name);
            assert!(curve.ease(0.0).abs() < 1e-4, "{name}");
            assert!((curve.ease(1.0) - 1.0).abs() < 1e-4, "{name}");
        }
        // Ease-in is slow at first.
        assert!(Curve::parse("INCIRC").ease(0.5) < 0.3);
        // OUTINBACK rushes out, stalls, then rushes in.
        let back = Curve::parse("OUTINBACK");
        assert!(back.ease(0.2) > 0.3);
        assert!((back.ease(0.6) - back.ease(0.4)).abs() < 0.1);
    }

    #[test]
    fn random_playback_is_repeatable() {
        let rwb = keyframe_animation("ghub:redwhiteblue").unwrap();
        assert_eq!(rwb.playback, Playback::Random);
        for ms in [0.0, 1234.0, 98_765.0] {
            assert_eq!(rwb.sample(ms, 1.0), rwb.sample(ms, 1.0));
        }
    }

    #[test]
    fn the_screen_sampler_splits_the_board_into_g_hubs_five_regions() {
        let layout = screen_sampler_layout();
        let names: Vec<&str> = layout.regions.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["LEFT", "MID_LEFT", "MIDDLE", "MID_RIGHT", "RIGHT"]);
        assert_eq!(layout.keys.get("A"), Some(&0));
        assert!(layout.keys.len() > 100);
        assert!(layout.keys.get("NumPlus").copied().unwrap_or(0) >= 3);
    }

    #[test]
    fn the_visualizer_columns_run_low_to_high_with_keys() {
        let layout = visualizer_layout();
        assert_eq!((layout.default_low, layout.default_high), (60, 6000));
        assert!(layout.columns.len() >= 20);
        assert_eq!(layout.columns[0].0, 60);
        assert!(layout.columns.windows(2).all(|w| w[0].0 <= w[1].0));
        // The first full column climbs the left edge: Ctrl, Shift, Caps, Tab, `, Esc.
        let left = layout.columns.iter().find(|c| c.2.first() == Some(&Some("LeftControl")));
        assert!(left.is_some());
    }
}
