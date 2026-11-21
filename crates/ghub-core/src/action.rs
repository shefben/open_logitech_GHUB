//! Actions and macros. Port of `Artemis.Peripherals/Actions/ActionModel.cs`.
//!
//! An action is what a control does when it is pressed. The C# used a polymorphic JSON hierarchy
//! with a `"type"` discriminator; serde's internally tagged enum produces byte-identical JSON, so
//! profiles written by the Avalonia build still load here.
//!
//! Everything in this module executes **in software**, by synthesising input. [`ExecutionSite`]
//! exists so the UI can say so plainly: a user who expects a binding to survive into another
//! machine needs to know it lives in this application and not in the device's own memory.

use serde::{Deserialize, Serialize};

use crate::id::Id;

/// Where an action runs. Shown in the UI so software execution is never mistaken for firmware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ExecutionSite {
    #[default]
    Software,
    Hardware,
}

impl ExecutionSite {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Software => "In software",
            Self::Hardware => "On device",
        }
    }
}

/// Held modifiers, as a bit set.
///
/// Serialised the way `System.Text.Json`'s `JsonStringEnumConverter` writes a `[Flags]` enum -
/// `"None"`, `"Control"`, `"Control, Shift"` - because that is what the Avalonia build's profile
/// files contain. A bare number is accepted too, for files written before that converter was
/// added and for anything hand-edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyModifiers(pub u8);

impl Serialize for KeyModifiers {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for KeyModifiers {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Text(String),
            Bits(u8),
        }
        match Wire::deserialize(deserializer)? {
            Wire::Bits(bits) => Ok(Self(bits & 0x0F)),
            Wire::Text(text) => Self::parse(&text)
                .ok_or_else(|| serde::de::Error::custom(format!("not a modifier set: {text}"))),
        }
    }
}

impl std::fmt::Display for KeyModifiers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0 == 0 {
            return f.write_str("None");
        }
        let names: Vec<&str> = [
            (Self::CONTROL, "Control"),
            (Self::SHIFT, "Shift"),
            (Self::ALT, "Alt"),
            (Self::WINDOWS, "Windows"),
        ]
        .into_iter()
        .filter(|(flag, _)| self.contains(*flag))
        .map(|(_, name)| name)
        .collect();
        f.write_str(&names.join(", "))
    }
}

impl KeyModifiers {
    /// Parses `"Control, Shift"`. Case-insensitive, and tolerant of extra whitespace, because
    /// these files get hand-edited.
    pub fn parse(text: &str) -> Option<Self> {
        let mut bits = Self::NONE;
        for part in text.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            bits |= match part.to_ascii_lowercase().as_str() {
                "none" => Self::NONE,
                "control" | "ctrl" => Self::CONTROL,
                "shift" => Self::SHIFT,
                "alt" => Self::ALT,
                "windows" | "win" => Self::WINDOWS,
                _ => return None,
            };
        }
        Some(bits)
    }
    pub const NONE: Self = Self(0);
    pub const CONTROL: Self = Self(1);
    pub const SHIFT: Self = Self(2);
    pub const ALT: Self = Self(4);
    pub const WINDOWS: Self = Self(8);

    pub fn contains(&self, other: Self) -> bool {
        self.0 & other.0 == other.0 && other.0 != 0
    }

    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for KeyModifiers {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for KeyModifiers {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseButtonKind {
    Left,
    Right,
    Middle,
    XButton1,
    XButton2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaCommand {
    PlayPause,
    Stop,
    Next,
    Previous,
    VolumeUp,
    VolumeDown,
    Mute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DpiCommand {
    Next,
    Previous,
    Cycle,
    Default,
    /// Hold for the profile's shift (sniper) DPI, restore on release.
    Shift,
    /// Select the stage named by [`ActionDefinition::Dpi::stage`].
    Stage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProfileCommand {
    Next,
    Previous,
    Select,
    ToggleLock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LightingCommand {
    BrightnessUp,
    BrightnessDown,
    ToggleOff,
    /// G HUB's "Cycle Maximum Brightness": steps 100, 75, 50, 25, 0 per cent and wraps.
    BrightnessCycle,
}

/// One keystroke with its modifiers held, used both as an action and as a chord step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyStroke {
    pub virtual_key: u16,
    #[serde(default)]
    pub modifiers: KeyModifiers,
}

impl KeyStroke {
    pub fn new(virtual_key: u16) -> Self {
        Self { virtual_key, modifiers: KeyModifiers::NONE }
    }

    pub fn with(virtual_key: u16, modifiers: KeyModifiers) -> Self {
        Self { virtual_key, modifiers }
    }

    pub fn describe(&self) -> String {
        describe_key(self.virtual_key, self.modifiers)
    }
}

/// What a control does when it is pressed.
///
/// The `type` discriminators match the C# `JsonDerivedType` names exactly, so a profile written by
/// the Avalonia build loads unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum ActionDefinition {
    #[serde(rename = "key")]
    Key(KeyStroke),
    /// A sequence of key combinations tapped in order, such as Ctrl+K then Ctrl+C.
    #[serde(rename = "chord")]
    Chord { steps: Vec<KeyStroke> },
    #[serde(rename = "mouseButton")]
    MouseButton { button: MouseButtonKind },
    #[serde(rename = "mouseWheel")]
    MouseWheel {
        delta: i32,
        #[serde(default)]
        horizontal: bool,
    },
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "media")]
    Media { command: MediaCommand },
    /// Launch a program, file or URL, with the user's own token. Never elevated.
    #[serde(rename = "launch")]
    Launch {
        target: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arguments: Option<String>,
    },
    #[serde(rename = "macro")]
    MacroRef {
        #[serde(rename = "macroId")]
        macro_id: Id,
    },
    #[serde(rename = "dpi")]
    Dpi {
        command: DpiCommand,
        #[serde(default)]
        stage: usize,
    },
    /// While held, bindings resolve against the `:shift` layer.
    #[serde(rename = "gshift")]
    GShift,
    #[serde(rename = "profile")]
    Profile {
        command: ProfileCommand,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile_id: Option<Id>,
    },
    /// `bank` 0 cycles; 1..n selects M1..Mn.
    #[serde(rename = "bank")]
    Bank { bank: usize },
    #[serde(rename = "lighting")]
    Lighting { command: LightingCommand },
    /// Intercepted and swallowed: the control does nothing at all.
    #[serde(rename = "disabled")]
    Disabled,
    /// Not intercepted: the device's or the OS's own behaviour applies.
    #[serde(rename = "default")]
    #[default]
    PassThrough,
}

impl ActionDefinition {
    /// One line for the binding row in the UI.
    pub fn describe(&self) -> String {
        match self {
            Self::Key(k) => k.describe(),
            Self::Chord { steps } => {
                steps.iter().map(KeyStroke::describe).collect::<Vec<_>>().join(", ")
            }
            Self::MouseButton { button } => match button {
                MouseButtonKind::Left => "Left click".into(),
                MouseButtonKind::Right => "Right click".into(),
                MouseButtonKind::Middle => "Middle click".into(),
                MouseButtonKind::XButton1 => "Back".into(),
                MouseButtonKind::XButton2 => "Forward".into(),
            },
            Self::MouseWheel { delta, horizontal } => match (horizontal, *delta > 0) {
                (true, true) => "Scroll right".into(),
                (true, false) => "Scroll left".into(),
                (false, true) => "Scroll up".into(),
                (false, false) => "Scroll down".into(),
            },
            // Truncated with an ellipsis the way the C# did, so a pasted paragraph does not
            // stretch the binding row.
            Self::Text { text } => {
                let shown: String = if text.chars().count() > 24 {
                    format!("{}\u{2026}", text.chars().take(24).collect::<String>())
                } else {
                    text.clone()
                };
                format!("Type \"{shown}\"")
            }
            Self::Media { command } => match command {
                MediaCommand::PlayPause => "Play/Pause".into(),
                MediaCommand::Stop => "Stop".into(),
                MediaCommand::Next => "Next".into(),
                MediaCommand::Previous => "Previous".into(),
                MediaCommand::VolumeUp => "Volume up".into(),
                MediaCommand::VolumeDown => "Volume down".into(),
                MediaCommand::Mute => "Mute".into(),
            },
            Self::Launch { target, .. } => format!("Open {}", file_name_of(target)),
            Self::MacroRef { .. } => "Run macro".into(),
            Self::Dpi { command, stage } => match command {
                DpiCommand::Stage => format!("DPI stage {}", stage + 1),
                DpiCommand::Shift => "DPI shift (hold)".into(),
                DpiCommand::Next => "DPI next".into(),
                DpiCommand::Previous => "DPI previous".into(),
                DpiCommand::Cycle => "DPI cycle".into(),
                DpiCommand::Default => "DPI default".into(),
            },
            Self::GShift => "G-Shift (hold)".into(),
            Self::Profile { command, .. } => match command {
                ProfileCommand::Select => "Switch profile".into(),
                ProfileCommand::Next => "Profile Next".into(),
                ProfileCommand::Previous => "Profile Previous".into(),
                ProfileCommand::ToggleLock => "Profile ToggleLock".into(),
            },
            Self::Bank { bank } => match bank {
                0 => "Cycle G-key bank".into(),
                n => format!("G-key bank M{n}"),
            },
            Self::Lighting { command } => match command {
                LightingCommand::BrightnessUp => "BrightnessUp".into(),
                LightingCommand::BrightnessDown => "BrightnessDown".into(),
                LightingCommand::ToggleOff => "ToggleOff".into(),
                LightingCommand::BrightnessCycle => "Cycle brightness".into(),
            },
            Self::Disabled => "Disabled".into(),
            Self::PassThrough => "Default".into(),
        }
    }

    /// The group this action appears under in the assignment panel, matching G HUB's own
    /// left-hand categories.
    pub fn category(&self) -> &'static str {
        match self {
            Self::Key(_) | Self::Chord { .. } => "Keys",
            Self::MouseButton { .. } | Self::MouseWheel { .. } => "Mouse",
            Self::Text { .. } => "Text",
            Self::Media { .. } => "Media",
            Self::Launch { .. } => "System",
            Self::MacroRef { .. } => "Macros",
            Self::Dpi { .. } | Self::GShift => "DPI",
            Self::Profile { .. } | Self::Bank { .. } => "Profiles",
            Self::Lighting { .. } => "Lighting",
            Self::Disabled | Self::PassThrough => "Other",
        }
    }

    /// Whether this action needs the control diverted to software.
    ///
    /// [`Self::PassThrough`] is the only one that does not, and getting this wrong is expensive in
    /// both directions: diverting unnecessarily makes a key stop working when this application
    /// exits, and not diverting makes the binding silently do nothing.
    pub fn needs_interception(&self) -> bool {
        !matches!(self, Self::PassThrough)
    }

    /// Macro this action runs, if any. Used to refuse deleting a macro that is still bound.
    pub fn macro_id(&self) -> Option<Id> {
        match self {
            Self::MacroRef { macro_id } => Some(*macro_id),
            _ => None,
        }
    }
}

/// The last path segment of a launch target, for the binding label.
fn file_name_of(target: &str) -> String {
    let trimmed = target.trim_end_matches(['/', '\\']);
    match trimmed.rsplit(['/', '\\']).next() {
        Some(name) if !name.is_empty() => name.to_owned(),
        _ => target.to_owned(),
    }
}

/// Binds an action to a physical control within a profile.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionBinding {
    pub action: ActionDefinition,
    #[serde(default)]
    pub site: ExecutionSite,
}

impl ActionBinding {
    pub fn new(action: ActionDefinition) -> Self {
        Self { action, site: ExecutionSite::Software }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MacroRepeatMode {
    /// Play once per press.
    #[default]
    Once,
    /// Play once; presses while playing are ignored.
    NoRepeat,
    /// Repeat while the trigger is held.
    WhileHeld,
    /// First press starts repeating, second press stops.
    Toggle,
    /// Repeat [`MacroDefinition::repeat_count`] times.
    RepeatN,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MacroCondition {
    /// Continue only while the given key is held by the user.
    KeyHeld,
    /// Continue only while the foreground executable name matches.
    ForegroundApp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum MacroStep {
    #[serde(rename = "keyDown")]
    KeyDown { virtual_key: u16 },
    #[serde(rename = "keyUp")]
    KeyUp { virtual_key: u16 },
    #[serde(rename = "mouseDown")]
    MouseDown { button: MouseButtonKind },
    #[serde(rename = "mouseUp")]
    MouseUp { button: MouseButtonKind },
    /// Relative cursor movement in pixels. Absolute positioning is deliberately not offered: a
    /// macro that jumps the pointer to a screen coordinate breaks on every other display layout.
    #[serde(rename = "mouseMove")]
    MouseMove { dx: i32, dy: i32 },
    #[serde(rename = "wheel")]
    Wheel {
        delta: i32,
        #[serde(default)]
        horizontal: bool,
    },
    #[serde(rename = "delay")]
    Delay { milliseconds: u32 },
    #[serde(rename = "text")]
    Text { text: String },
    /// Any library action, inlined into the macro.
    #[serde(rename = "action")]
    Action { action: ActionDefinition },
    #[serde(rename = "if")]
    Condition {
        condition: MacroCondition,
        value: String,
        then: Vec<MacroStep>,
        #[serde(default, skip_serializing_if = "Option::is_none", rename = "else")]
        otherwise: Option<Vec<MacroStep>>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MacroDefinition {
    pub id: Id,
    pub name: String,
    #[serde(default)]
    pub repeat_mode: MacroRepeatMode,
    #[serde(default = "one")]
    pub repeat_count: u32,
    /// Delay inserted between repetitions.
    #[serde(default = "default_repeat_delay")]
    pub repeat_delay_milliseconds: u32,
    /// When false, recorded delays are replaced by [`Self::fixed_delay_milliseconds`].
    #[serde(default = "yes")]
    pub use_recorded_delays: bool,
    #[serde(default = "default_fixed_delay")]
    pub fixed_delay_milliseconds: u32,
    /// Safety bound for `WhileHeld` and `Toggle`: stop after this many repetitions even if the
    /// release never arrives. Without it, a dropped release event means a macro that types
    /// forever and a user who has to pull the cable.
    #[serde(default = "default_max_loops")]
    pub max_loop_iterations: u32,
    #[serde(default)]
    pub steps: Vec<MacroStep>,
    #[serde(default)]
    pub tags: Vec<String>,
}

fn one() -> u32 {
    1
}
fn yes() -> bool {
    true
}
fn default_repeat_delay() -> u32 {
    50
}
fn default_fixed_delay() -> u32 {
    20
}
fn default_max_loops() -> u32 {
    500
}

impl MacroDefinition {
    pub const MAX_STEPS: usize = 5000;
    pub const MAX_REPEAT_COUNT: u32 = 1000;
    pub const MAX_DELAY_MILLISECONDS: u32 = 600_000;

    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Id::new(),
            name: name.into(),
            repeat_mode: MacroRepeatMode::Once,
            repeat_count: 1,
            repeat_delay_milliseconds: 50,
            use_recorded_delays: true,
            fixed_delay_milliseconds: 20,
            max_loop_iterations: 500,
            steps: Vec::new(),
            tags: Vec::new(),
        }
    }

    /// Every macro here runs in software. Kept explicit for the UI and for export.
    pub fn site(&self) -> ExecutionSite {
        ExecutionSite::Software
    }

    /// Clamps the macro to its documented bounds and says what it changed.
    ///
    /// Called before a macro is saved or run, rather than trusting a hand-edited profile file: an
    /// unbounded delay or step count here becomes an application that appears to hang while
    /// holding modifier keys down.
    pub fn clamp(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
        if self.steps.len() > Self::MAX_STEPS {
            notes.push(format!(
                "{} steps truncated to the {} step limit",
                self.steps.len(),
                Self::MAX_STEPS
            ));
            self.steps.truncate(Self::MAX_STEPS);
        }
        if self.repeat_count > Self::MAX_REPEAT_COUNT || self.repeat_count == 0 {
            notes.push(format!("repeat count {} clamped", self.repeat_count));
            self.repeat_count = self.repeat_count.clamp(1, Self::MAX_REPEAT_COUNT);
        }
        if self.max_loop_iterations == 0 {
            notes.push("a zero loop bound would never play; set to 1".into());
            self.max_loop_iterations = 1;
        }
        let total_before = notes.len();
        clamp_delays(&mut self.steps);
        for field in [&mut self.repeat_delay_milliseconds, &mut self.fixed_delay_milliseconds] {
            if *field > Self::MAX_DELAY_MILLISECONDS {
                *field = Self::MAX_DELAY_MILLISECONDS;
                notes.push("a delay longer than ten minutes was clamped".into());
            }
        }
        let _ = total_before;
        notes
    }

    /// Keys this macro presses and does not release.
    ///
    /// The runtime releases these on abort. A macro that holds Alt and is interrupted otherwise
    /// leaves the user's desktop in Alt-Tab.
    pub fn unbalanced_keys(&self) -> Vec<u16> {
        let mut held: Vec<u16> = Vec::new();
        collect_unbalanced(&self.steps, &mut held);
        held.sort_unstable();
        held.dedup();
        held
    }
}

fn clamp_delays(steps: &mut [MacroStep]) {
    for step in steps.iter_mut() {
        match step {
            MacroStep::Delay { milliseconds } => {
                *milliseconds = (*milliseconds).min(MacroDefinition::MAX_DELAY_MILLISECONDS);
            }
            MacroStep::Condition { then, otherwise, .. } => {
                clamp_delays(then);
                if let Some(otherwise) = otherwise {
                    clamp_delays(otherwise);
                }
            }
            _ => {}
        }
    }
}

/// Walks both branches of every condition: a key held in one branch and released in the other is
/// still unbalanced on the path that holds it.
fn collect_unbalanced(steps: &[MacroStep], held: &mut Vec<u16>) {
    for step in steps {
        match step {
            MacroStep::KeyDown { virtual_key } => held.push(*virtual_key),
            MacroStep::KeyUp { virtual_key } => {
                if let Some(pos) = held.iter().rposition(|k| k == virtual_key) {
                    held.remove(pos);
                }
            }
            MacroStep::Condition { then, otherwise, .. } => {
                collect_unbalanced(then, held);
                if let Some(otherwise) = otherwise {
                    collect_unbalanced(otherwise, held);
                }
            }
            _ => {}
        }
    }
}

/// `Ctrl + Shift + K`, the way the binding row reads.
pub fn describe_key(vk: u16, modifiers: KeyModifiers) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if modifiers.contains(KeyModifiers::CONTROL) {
        parts.push("Ctrl");
    }
    if modifiers.contains(KeyModifiers::SHIFT) {
        parts.push("Shift");
    }
    if modifiers.contains(KeyModifiers::ALT) {
        parts.push("Alt");
    }
    if modifiers.contains(KeyModifiers::WINDOWS) {
        parts.push("Win");
    }
    let name = key_name(vk);
    let mut text = parts.join(" + ");
    if !text.is_empty() {
        text.push_str(" + ");
    }
    text.push_str(&name);
    text
}

/// Windows virtual-key name, matching the C# table exactly.
pub fn key_name(vk: u16) -> String {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => char::from(vk as u8).to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        0x08 => "Backspace".into(),
        0x09 => "Tab".into(),
        0x0D => "Enter".into(),
        0x10 => "Shift".into(),
        0x11 => "Ctrl".into(),
        0x12 => "Alt".into(),
        0x13 => "Pause".into(),
        0x14 => "Caps Lock".into(),
        0x1B => "Esc".into(),
        0x20 => "Space".into(),
        0x21 => "Page Up".into(),
        0x22 => "Page Down".into(),
        0x23 => "End".into(),
        0x24 => "Home".into(),
        0x25 => "Left".into(),
        0x26 => "Up".into(),
        0x27 => "Right".into(),
        0x28 => "Down".into(),
        0x2C => "Print Screen".into(),
        0x2D => "Insert".into(),
        0x2E => "Delete".into(),
        0x5B => "Left Win".into(),
        0x5C => "Right Win".into(),
        0x5D => "Menu".into(),
        0x60..=0x69 => format!("Num {}", vk - 0x60),
        0x6A => "Num *".into(),
        0x6B => "Num +".into(),
        0x6D => "Num -".into(),
        0x6E => "Num .".into(),
        0x6F => "Num /".into(),
        0x90 => "Num Lock".into(),
        0x91 => "Scroll Lock".into(),
        0xA0 => "Left Shift".into(),
        0xA1 => "Right Shift".into(),
        0xA2 => "Left Ctrl".into(),
        0xA3 => "Right Ctrl".into(),
        0xA4 => "Left Alt".into(),
        0xA5 => "Right Alt".into(),
        0xBA => ";".into(),
        0xBB => "=".into(),
        0xBC => ",".into(),
        0xBD => "-".into(),
        0xBE => ".".into(),
        0xBF => "/".into(),
        0xC0 => "`".into(),
        0xDB => "[".into(),
        0xDC => "\\".into(),
        0xDD => "]".into(),
        0xDE => "'".into(),
        _ => format!("VK 0x{vk:02X}"),
    }
}

/// HID keyboard usage to Windows virtual key, US layout. For imported G HUB keystrokes.
pub fn virtual_key_from_hid_usage(usage: u8) -> Option<u16> {
    let vk: u16 = match usage {
        0x04..=0x1D => b'A' as u16 + usage as u16 - 0x04,
        0x1E..=0x26 => b'1' as u16 + usage as u16 - 0x1E,
        0x27 => '0' as u16,
        0x28 => 0x0D,
        0x29 => 0x1B,
        0x2A => 0x08,
        0x2B => 0x09,
        0x2C => 0x20,
        0x2D => 0xBD,
        0x2E => 0xBB,
        0x2F => 0xDB,
        0x30 => 0xDD,
        0x31 => 0xDC,
        0x33 => 0xBA,
        0x34 => 0xDE,
        0x35 => 0xC0,
        0x36 => 0xBC,
        0x37 => 0xBE,
        0x38 => 0xBF,
        0x39 => 0x14,
        0x3A..=0x45 => 0x70 + usage as u16 - 0x3A,
        0x46 => 0x2C,
        0x47 => 0x91,
        0x48 => 0x13,
        0x49 => 0x2D,
        0x4A => 0x24,
        0x4B => 0x21,
        0x4C => 0x2E,
        0x4D => 0x23,
        0x4E => 0x22,
        0x4F => 0x27,
        0x50 => 0x25,
        0x51 => 0x28,
        0x52 => 0x26,
        0x53 => 0x90,
        0x54 => 0x6F,
        0x55 => 0x6A,
        0x56 => 0x6D,
        0x57 => 0x6B,
        0x58 => 0x0D,
        0x59..=0x61 => 0x61 + usage as u16 - 0x59,
        0x62 => 0x60,
        0x63 => 0x6E,
        0x65 => 0x5D,
        0xE0 => 0xA2,
        0xE1 => 0xA0,
        0xE2 => 0xA4,
        0xE3 => 0x5B,
        0xE4 => 0xA3,
        0xE5 => 0xA1,
        0xE6 => 0xA5,
        0xE7 => 0x5C,
        _ => return None,
    };
    Some(vk)
}

pub fn modifiers_from_hid_usages(usages: &[u8]) -> KeyModifiers {
    let mut m = KeyModifiers::NONE;
    for u in usages {
        m |= match u {
            0xE0 | 0xE4 => KeyModifiers::CONTROL,
            0xE1 | 0xE5 => KeyModifiers::SHIFT,
            0xE2 | 0xE6 => KeyModifiers::ALT,
            0xE3 | 0xE7 => KeyModifiers::WINDOWS,
            _ => KeyModifiers::NONE,
        };
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_json_discriminators_match_the_avalonia_build() {
        // A profile written by the C# must load here unchanged, so the tag values are part of the
        // file format rather than an implementation detail.
        let cases: Vec<(ActionDefinition, &str)> = vec![
            (ActionDefinition::Key(KeyStroke::new(0x41)), "key"),
            (ActionDefinition::Chord { steps: vec![] }, "chord"),
            (ActionDefinition::MouseButton { button: MouseButtonKind::Left }, "mouseButton"),
            (ActionDefinition::MouseWheel { delta: 1, horizontal: false }, "mouseWheel"),
            (ActionDefinition::Text { text: "x".into() }, "text"),
            (ActionDefinition::Media { command: MediaCommand::Mute }, "media"),
            (ActionDefinition::Launch { target: "x".into(), arguments: None }, "launch"),
            (ActionDefinition::MacroRef { macro_id: Id::NIL }, "macro"),
            (ActionDefinition::Dpi { command: DpiCommand::Next, stage: 0 }, "dpi"),
            (ActionDefinition::GShift, "gshift"),
            (ActionDefinition::Profile { command: ProfileCommand::Next, profile_id: None }, "profile"),
            (ActionDefinition::Bank { bank: 1 }, "bank"),
            (ActionDefinition::Lighting { command: LightingCommand::ToggleOff }, "lighting"),
            (ActionDefinition::Disabled, "disabled"),
            (ActionDefinition::PassThrough, "default"),
        ];
        for (action, tag) in cases {
            let json = serde_json::to_string(&action).unwrap();
            assert!(
                json.contains(&format!("\"type\":\"{tag}\"")),
                "{action:?} serialised as {json}, expected tag {tag}"
            );
            let back: ActionDefinition = serde_json::from_str(&json).unwrap();
            assert_eq!(back, action);
        }
    }

    #[test]
    fn a_key_action_reads_the_way_the_binding_row_shows_it() {
        let ctrl_shift_k = ActionDefinition::Key(KeyStroke::with(
            0x4B,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert_eq!(ctrl_shift_k.describe(), "Ctrl + Shift + K");
        assert_eq!(ActionDefinition::Key(KeyStroke::new(0x70)).describe(), "F1");
        assert_eq!(ActionDefinition::Key(KeyStroke::new(0x87)).describe(), "F24");
        assert_eq!(ActionDefinition::Key(KeyStroke::new(0x0D)).describe(), "Enter");
        // An unnamed key must still render as something a bug report can quote.
        assert_eq!(key_name(0xF0), "VK 0xF0");
    }

    #[test]
    fn long_typed_text_is_truncated_rather_than_stretching_the_row() {
        let long = "a".repeat(100);
        let described = ActionDefinition::Text { text: long }.describe();
        assert_eq!(described.chars().count(), 24 + "Type \"\u{2026}\"".chars().count());
        // Multi-byte text must not be sliced mid-character.
        let emoji = ActionDefinition::Text { text: "\u{1F600}".repeat(40) }.describe();
        assert!(emoji.ends_with("\u{2026}\""));
    }

    #[test]
    fn a_launch_target_is_labelled_by_its_file_name() {
        assert_eq!(
            ActionDefinition::Launch { target: r"C:\Games\game.exe".into(), arguments: None }
                .describe(),
            "Open game.exe"
        );
        // A trailing separator must not produce "Open ".
        assert_eq!(
            ActionDefinition::Launch { target: r"C:\Games\".into(), arguments: None }.describe(),
            "Open Games"
        );
        // A URL has no file name; showing the whole thing beats showing nothing.
        assert_eq!(
            ActionDefinition::Launch { target: "https://example.com".into(), arguments: None }
                .describe(),
            "Open example.com"
        );
    }

    #[test]
    fn only_pass_through_leaves_a_control_alone() {
        assert!(!ActionDefinition::PassThrough.needs_interception());
        // Disabled is *not* pass-through: the control must still be diverted, or the key keeps
        // doing its default thing instead of nothing.
        assert!(ActionDefinition::Disabled.needs_interception());
        assert!(ActionDefinition::Key(KeyStroke::new(0x41)).needs_interception());
    }

    #[test]
    fn a_macro_holding_a_key_reports_it_as_unbalanced() {
        let mut m = MacroDefinition::new("hold alt");
        m.steps = vec![
            MacroStep::KeyDown { virtual_key: 0xA4 },
            MacroStep::KeyDown { virtual_key: 0x09 },
            MacroStep::KeyUp { virtual_key: 0x09 },
        ];
        assert_eq!(m.unbalanced_keys(), vec![0xA4], "Alt is still down at the end");

        m.steps.push(MacroStep::KeyUp { virtual_key: 0xA4 });
        assert!(m.unbalanced_keys().is_empty());
    }

    #[test]
    fn a_key_held_in_only_one_branch_of_a_condition_still_counts() {
        // The branch that holds it is a path the runtime can take, so abort has to release it.
        let mut m = MacroDefinition::new("conditional hold");
        m.steps = vec![MacroStep::Condition {
            condition: MacroCondition::ForegroundApp,
            value: "game.exe".into(),
            then: vec![MacroStep::KeyDown { virtual_key: 0x10 }],
            otherwise: Some(vec![MacroStep::KeyDown { virtual_key: 0x11 }]),
        }];
        assert_eq!(m.unbalanced_keys(), vec![0x10, 0x11]);
    }

    #[test]
    fn clamping_bounds_a_hand_edited_macro_and_says_what_it_changed() {
        let mut m = MacroDefinition::new("hostile");
        m.repeat_count = 999_999;
        m.max_loop_iterations = 0;
        m.repeat_delay_milliseconds = u32::MAX;
        m.steps = vec![MacroStep::Delay { milliseconds: u32::MAX }];
        let notes = m.clamp();

        assert_eq!(m.repeat_count, MacroDefinition::MAX_REPEAT_COUNT);
        assert_eq!(m.max_loop_iterations, 1, "a zero bound would never play at all");
        assert_eq!(m.repeat_delay_milliseconds, MacroDefinition::MAX_DELAY_MILLISECONDS);
        assert_eq!(
            m.steps[0],
            MacroStep::Delay { milliseconds: MacroDefinition::MAX_DELAY_MILLISECONDS }
        );
        assert!(notes.len() >= 3, "every change must be reported: {notes:?}");
    }

    #[test]
    fn a_nested_delay_is_clamped_too() {
        let mut m = MacroDefinition::new("nested");
        m.steps = vec![MacroStep::Condition {
            condition: MacroCondition::KeyHeld,
            value: "16".into(),
            then: vec![MacroStep::Delay { milliseconds: 10_000_000 }],
            otherwise: None,
        }];
        m.clamp();
        let MacroStep::Condition { then, .. } = &m.steps[0] else { panic!("shape changed") };
        assert_eq!(
            then[0],
            MacroStep::Delay { milliseconds: MacroDefinition::MAX_DELAY_MILLISECONDS }
        );
    }

    #[test]
    fn hid_usages_map_to_the_virtual_keys_the_c_sharp_table_had() {
        assert_eq!(virtual_key_from_hid_usage(0x04), Some(0x41), "usage 0x04 is A");
        assert_eq!(virtual_key_from_hid_usage(0x1D), Some(0x5A), "usage 0x1D is Z");
        assert_eq!(virtual_key_from_hid_usage(0x1E), Some(0x31), "usage 0x1E is 1");
        assert_eq!(virtual_key_from_hid_usage(0x27), Some(0x30), "usage 0x27 is 0, not 9+1");
        assert_eq!(virtual_key_from_hid_usage(0x3A), Some(0x70), "usage 0x3A is F1");
        assert_eq!(virtual_key_from_hid_usage(0x45), Some(0x7B), "usage 0x45 is F12");
        assert_eq!(virtual_key_from_hid_usage(0x59), Some(0x61), "keypad 1");
        assert_eq!(virtual_key_from_hid_usage(0x62), Some(0x60), "keypad 0 is not keypad 10");
        // 0x32 and 0x64 are layout-specific keys with no US virtual key; a guess here would type
        // the wrong character.
        assert_eq!(virtual_key_from_hid_usage(0x32), None);
        assert_eq!(virtual_key_from_hid_usage(0x00), None);
    }

    #[test]
    fn modifiers_serialise_as_the_flags_string_the_c_sharp_wrote() {
        assert_eq!(KeyModifiers::NONE.to_string(), "None");
        assert_eq!(
            (KeyModifiers::CONTROL | KeyModifiers::WINDOWS).to_string(),
            "Control, Windows"
        );
        // Order is fixed, not insertion order, so the file is stable across saves.
        assert_eq!(
            (KeyModifiers::WINDOWS | KeyModifiers::CONTROL).to_string(),
            "Control, Windows"
        );
        assert_eq!(KeyModifiers::parse("Shift,  alt ").unwrap().0, 2 | 4);
        assert_eq!(KeyModifiers::parse("None").unwrap(), KeyModifiers::NONE);
        assert!(KeyModifiers::parse("Meta").is_none(), "an unknown name must not be ignored");
        // A number, from a file written before the string converter.
        assert_eq!(serde_json::from_str::<KeyModifiers>("3").unwrap().0, 3);
        // Bits above the four defined flags are dropped rather than carried into a comparison
        // that would then never match.
        assert_eq!(serde_json::from_str::<KeyModifiers>("255").unwrap().0, 0x0F);
    }

    #[test]
    fn both_left_and_right_modifier_usages_set_the_same_flag() {
        let m = modifiers_from_hid_usages(&[0xE4, 0xE5]);
        assert!(m.contains(KeyModifiers::CONTROL) && m.contains(KeyModifiers::SHIFT));
        assert!(!m.contains(KeyModifiers::ALT));
        assert!(modifiers_from_hid_usages(&[0x04]).is_empty(), "A is not a modifier");
    }

    #[test]
    fn a_profile_file_written_by_the_c_sharp_still_loads() {
        // Shape taken from the Avalonia build's own output: camelCase fields, "type" tag,
        // absent optional fields.
        let json =
            r#"{"action":{"type":"key","virtualKey":65,"modifiers":"Control, Shift"},"site":"Software"}"#;
        let binding: ActionBinding = serde_json::from_str(json).unwrap();
        assert_eq!(
            binding.action,
            ActionDefinition::Key(KeyStroke::with(
                0x41,
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            ))
        );
        assert_eq!(binding.site, ExecutionSite::Software);
        // Round-trips back to the same shape the C# wrote.
        assert_eq!(serde_json::to_string(&binding).unwrap(), json);

        // A binding with no site at all defaults to software rather than failing to load.
        let minimal: ActionBinding =
            serde_json::from_str(r#"{"action":{"type":"default"}}"#).unwrap();
        assert_eq!(minimal.action, ActionDefinition::PassThrough);
        assert_eq!(minimal.site, ExecutionSite::Software);
    }
}
