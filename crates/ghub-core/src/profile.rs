//! Profiles, app rules, per-device settings and the persisted document shape. Port of
//! `Artemis.Peripherals/Profiles/ProfileModel.cs` and the trigger rule from
//! `Automation/Triggers.cs`.
//!
//! Field names are camelCase and enum values are their verbatim PascalCase names, matching the
//! `JsonSerializerOptions` the C# store used, so a `peripheral-profiles.json` written by the
//! Avalonia build loads here unchanged.
//!
//! Two things are deliberately *not* in a profile:
//!
//! - **Device flash state.** An onboard profile lives in the device. A copy of it belongs in an
//!   [`OnboardSnapshot`], taken before a write and stored separately, so restoring a profile
//!   cannot silently reflash a mouse.
//! - **Anything derived from the device.** Capabilities are read from the hardware on every
//!   connect. A profile that cached them would drive a device by what it used to be.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::action::{ActionBinding, ActionDefinition, MacroDefinition};
use crate::id::Id;

/// How an [`AppRule`] decides whether it matches the foreground window.
///
/// The order of the variants is the priority order: an exact path beats an executable name, which
/// beats an executable plus a title, which beats a title alone. A broad title rule must never
/// outrank the specific path rule a user wrote for one program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
pub enum AppMatchKind {
    /// Full executable path, case-insensitive. Highest priority.
    ExactPath,
    /// Executable file name, such as `game.exe`, case-insensitive.
    #[default]
    ExecutableName,
    /// Executable name plus a window-title pattern.
    ExecutableAndTitle,
    /// Window-title pattern only. Broadest, so lowest priority.
    TitleOnly,
}

impl AppMatchKind {
    /// Lower is more specific. Used to order rules; ties break on [`AppRule::priority`].
    pub fn specificity(&self) -> u8 {
        match self {
            Self::ExactPath => 0,
            Self::ExecutableName => 1,
            Self::ExecutableAndTitle => 2,
            Self::TitleOnly => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppRule {
    pub id: Id,
    #[serde(default)]
    pub kind: AppMatchKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_pattern: Option<String>,
    #[serde(default = "crate::profile::yes")]
    pub enabled: bool,
    /// Tie-breaker within one rule kind; higher wins.
    #[serde(default)]
    pub priority: i32,
}

impl AppRule {
    pub fn by_executable(name: impl Into<String>) -> Self {
        Self {
            id: Id::new(),
            kind: AppMatchKind::ExecutableName,
            executable_path: None,
            executable_name: Some(name.into()),
            title_pattern: None,
            enabled: true,
            priority: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DpiStage {
    pub dpi: u32,
    #[serde(default = "yes")]
    pub enabled: bool,
}

impl DpiStage {
    pub fn new(dpi: u32) -> Self {
        Self { dpi, enabled: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MouseSettings {
    #[serde(default = "default_dpi_stages")]
    pub dpi_stages: Vec<DpiStage>,
    #[serde(default)]
    pub active_stage: usize,
    #[serde(default)]
    pub default_stage: usize,
    /// DPI applied while a DPI-shift (sniper) action is held, restored on release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shift_dpi: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_rate_hz: Option<u32>,
    #[serde(default = "yes")]
    pub show_dpi_notification: bool,
    /// `None` leaves the hi-res wheel's current setting alone rather than imposing one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wheel_high_resolution: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wheel_inverted: Option<bool>,
}

fn default_dpi_stages() -> Vec<DpiStage> {
    vec![DpiStage::new(800), DpiStage::new(1600), DpiStage::new(3200)]
}

impl Default for MouseSettings {
    fn default() -> Self {
        Self {
            dpi_stages: default_dpi_stages(),
            active_stage: 0,
            default_stage: 0,
            shift_dpi: Some(400),
            report_rate_hz: None,
            show_dpi_notification: true,
            wheel_high_resolution: None,
            wheel_inverted: None,
        }
    }
}

impl MouseSettings {
    /// The DPI of the active stage, skipping disabled ones.
    pub fn active_dpi(&self) -> Option<u32> {
        self.dpi_stages.get(self.active_stage).filter(|s| s.enabled).map(|s| s.dpi).or_else(|| {
            self.dpi_stages.iter().find(|s| s.enabled).map(|s| s.dpi)
        })
    }

    /// Index of the next enabled stage, wrapping. `None` when no stage is enabled.
    pub fn next_stage(&self, from: usize, backwards: bool) -> Option<usize> {
        let n = self.dpi_stages.len();
        if n == 0 || !self.dpi_stages.iter().any(|s| s.enabled) {
            return None;
        }
        for step in 1..=n {
            let index = if backwards {
                (from + n - step % n) % n
            } else {
                (from + step) % n
            };
            if self.dpi_stages[index].enabled {
                return Some(index);
            }
        }
        None
    }
}

/// How a [`KeyReaction`] behaves while the key is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum KeyReactionMode {
    /// The key shows the reaction colour while held, and returns to its own colour on release.
    #[default]
    Momentary,
    /// The key keeps the reaction colour until it is pressed again.
    Toggle,
}

/// One key's press reaction. Only keys the user adds have one; the list starts empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyReaction {
    /// Layout key id, such as `Escape`, `W` or `G3`.
    pub key_id: String,
    /// Reaction colour, `#RRGGBB`.
    pub color: String,
    #[serde(default)]
    pub mode: KeyReactionMode,
    #[serde(default = "yes")]
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyboardSettings {
    /// Active G-key bank, 1-based (M1..M3).
    #[serde(default = "one_usize")]
    pub active_bank: usize,
    #[serde(default)]
    pub key_reactions: Vec<KeyReaction>,
    #[serde(default)]
    pub game_mode_enabled: bool,
    /// HID usages disabled while game mode is on. Default is both Windows keys, which is what
    /// G HUB's own game mode disables.
    #[serde(default = "default_game_mode_usages")]
    pub game_mode_disabled_usages: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brightness_percent: Option<u8>,
    /// Take G-keys under software control. Needed for software macros on G1..G5.
    #[serde(default = "yes")]
    pub software_gkeys: bool,
}

fn default_game_mode_usages() -> Vec<u8> {
    vec![0xE3, 0xE7]
}

impl Default for KeyboardSettings {
    fn default() -> Self {
        Self {
            active_bank: 1,
            key_reactions: Vec::new(),
            game_mode_enabled: false,
            game_mode_disabled_usages: default_game_mode_usages(),
            brightness_percent: None,
            software_gkeys: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LightingMode {
    /// Host-streamed lighting from a layered profile.
    Artemis,
    /// Static colours written by this application. Volatile.
    Static,
    /// A firmware effect. Device-resident, and volatile unless saved onboard.
    DeviceEffect,
    /// Leave the device's own lighting alone. The default, so starting this application never
    /// blacks out a keyboard the user was happy with.
    #[default]
    Untouched,
    /// An animation rendered here and streamed frame by frame.
    Animated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AnimationDirection {
    #[default]
    Right,
    Left,
    Up,
    Down,
    /// Outward from the middle of the board.
    Out,
    /// Inward towards the middle of the board.
    In,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimationSettings {
    /// Effect id from the catalogue. An unknown id falls back to the first entry rather than
    /// failing to load, so a profile from a newer build still opens.
    #[serde(default = "default_effect")]
    pub effect: String,
    /// Speed multiplier; 1.0 is the effect's natural rate.
    #[serde(default = "one_f64")]
    pub speed: f64,
    #[serde(default)]
    pub direction: AnimationDirection,
    /// Colours the effect cycles through, `#RRGGBB`. Empty means the effect's own palette.
    #[serde(default)]
    pub colors: Vec<String>,
    #[serde(default = "default_fps")]
    pub frames_per_second: u32,
    /// For effects that answer key presses: how strongly a press shows.
    #[serde(default = "one_f64")]
    pub intensity: f64,
}

fn default_effect() -> String {
    "wave".to_owned()
}
fn one_f64() -> f64 {
    1.0
}
fn default_fps() -> u32 {
    25
}

impl Default for AnimationSettings {
    fn default() -> Self {
        Self {
            effect: default_effect(),
            speed: 1.0,
            direction: AnimationDirection::Right,
            colors: Vec::new(),
            frames_per_second: 25,
            intensity: 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LightingSettings {
    #[serde(default)]
    pub mode: LightingMode,
    #[serde(default = "one_f64")]
    pub brightness: f64,
    /// Static colours keyed by LED or zone id (`zone:0`, `key:Escape`), `#RRGGBB`.
    #[serde(default)]
    pub static_colors: HashMap<String, String>,
    #[serde(default = "default_white")]
    pub default_color: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_effect_id: Option<u16>,
    #[serde(default = "default_effect_period")]
    pub device_effect_period_ms: u16,
    /// Seconds of inactivity before the lights dim. `None` leaves the device's timers alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_save_idle_seconds: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_save_sleep_seconds: Option<u16>,
    #[serde(default)]
    pub animation: AnimationSettings,
    /// Per-zone firmware effects for [`LightingMode::DeviceEffect`], keyed by the 0x8071 cluster
    /// *location* (0x0001 primary, 0x0002 logo), not its index: indexes are per-firmware.
    ///
    /// A zone listed here runs its own effect, period and colour; any other zone falls back to
    /// [`Self::device_effect_id`]. G HUB stores one effect per zone, so this is what an imported
    /// G HUB firmware lighting card turns into.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub zone_effects: HashMap<u16, ZoneEffect>,
}

/// One zone's firmware effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneEffect {
    /// 0x8071 effect id.
    pub effect_id: u16,
    #[serde(default = "default_effect_period")]
    pub period_ms: u16,
    /// `#RRGGBB`; `None` uses the profile's zone or default colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

fn default_white() -> String {
    "#FFFFFF".to_owned()
}
fn default_effect_period() -> u16 {
    5000
}

impl Default for LightingSettings {
    fn default() -> Self {
        Self {
            mode: LightingMode::Untouched,
            brightness: 1.0,
            static_colors: HashMap::new(),
            default_color: default_white(),
            device_effect_id: None,
            device_effect_period_ms: 5000,
            power_save_idle_seconds: None,
            power_save_sleep_seconds: None,
            animation: AnimationSettings::default(),
            zone_effects: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceProfileSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mouse: Option<MouseSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyboard: Option<KeyboardSettings>,
    #[serde(default)]
    pub lighting: LightingSettings,
    /// Bindings keyed by slot id, such as `button:XButton1`, `gkey:G3:M2` or `gkey:G3:M2:shift`.
    #[serde(default)]
    pub bindings: HashMap<String, ActionBinding>,
}

/// Lighting that replaces the active profile's lighting for one device while a given program is
/// in the foreground.
///
/// Every field is optional on purpose: a rule that wants to change only the colours keeps the
/// profile's brightness and power saving rather than resetting them to a default.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppLightingOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<LightingMode>,
    /// 0-1; `None` keeps the profile's brightness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brightness: Option<f64>,
    #[serde(default)]
    pub static_colors: HashMap<String, String>,
    /// Colour of every key or zone the rule does not name; `None` keeps the profile's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_effect_id: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animation: Option<AnimationSettings>,
}

impl AppLightingOverride {
    /// Applies this override on top of a profile's lighting, leaving unset members alone.
    pub fn apply_to(&self, base: &LightingSettings) -> LightingSettings {
        let mut result = base.clone();
        if let Some(mode) = self.mode {
            result.mode = mode;
        }
        if let Some(brightness) = self.brightness {
            result.brightness = brightness;
        }
        if let Some(color) = &self.default_color {
            result.default_color = color.clone();
        }
        if let Some(effect) = self.device_effect_id {
            result.device_effect_id = Some(effect);
        }
        if let Some(animation) = &self.animation {
            result.animation = animation.clone();
        }
        // Named colours are merged, not replaced: a rule that recolours WASD must not blank the
        // rest of the board.
        for (key, color) in &self.static_colors {
            result.static_colors.insert(key.clone(), color.clone());
        }
        result
    }
}

/// Per-application lighting. Unlike an [`AppRule`] this never switches profile, so bindings, DPI
/// and macros are left exactly as they were.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppLightingRule {
    pub id: Id,
    #[serde(default)]
    pub name: String,
    /// Foreground executable file name, case-insensitive.
    #[serde(default)]
    pub executable_name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Tie-breaker when several rules match the same program; higher wins.
    #[serde(default)]
    pub priority: i32,
    /// Lighting per device key. A device absent here keeps the profile's lighting.
    #[serde(default)]
    pub devices: HashMap<String, AppLightingOverride>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TriggerEvent {
    #[default]
    DeviceConnected,
    DeviceDisconnected,
    /// Battery at or below [`TriggerRule::threshold`] percent while discharging.
    BatteryBelow,
    DpiChanged,
    BankChanged,
    /// A profile became active; the match filters by profile name.
    ProfileActivated,
    /// The foreground executable name equals the match, case-insensitive.
    ForegroundApp,
}

/// "When X happens, run action Y". Software actions only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriggerRule {
    pub id: Id,
    #[serde(default)]
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub event: TriggerEvent,
    /// Device name substring, profile name or executable name, depending on the event. Empty
    /// matches anything.
    #[serde(default)]
    pub r#match: String,
    /// Battery percent for `BatteryBelow`, DPI value or bank number otherwise. 0 means any.
    #[serde(default)]
    pub threshold: i32,
    #[serde(default)]
    pub action: ActionDefinition,
    /// Minimum seconds between two firings. Without it, a trigger whose action causes its own
    /// event loops.
    #[serde(default = "default_cooldown")]
    pub cooldown_seconds: i32,
}

fn default_cooldown() -> i32 {
    2
}

impl TriggerRule {
    pub fn new(name: impl Into<String>, event: TriggerEvent) -> Self {
        Self {
            id: Id::new(),
            name: name.into(),
            enabled: true,
            event,
            r#match: String::new(),
            threshold: 0,
            action: ActionDefinition::PassThrough,
            cooldown_seconds: 2,
        }
    }
}

/// One named user profile holding settings for every managed device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeripheralProfile {
    pub id: Id,
    #[serde(default = "default_profile_name")]
    pub name: String,
    #[serde(default)]
    pub is_default: bool,
    /// Order in the profile list.
    #[serde(default)]
    pub order: i32,
    #[serde(default)]
    pub rules: Vec<AppRule>,
    /// Per-device settings keyed by `DeviceIdentity::key()`.
    #[serde(default)]
    pub devices: HashMap<String, DeviceProfileSettings>,
    /// Unix seconds. A string timestamp would be nicer to read, but this has to survive a
    /// round-trip through a file a user may edit, and a number cannot be ambiguous about zone.
    #[serde(default)]
    pub modified: i64,
}

fn default_profile_name() -> String {
    "Default".to_owned()
}

impl PeripheralProfile {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Id::new(),
            name: name.into(),
            is_default: false,
            order: 0,
            rules: Vec::new(),
            devices: HashMap::new(),
            modified: now_unix(),
        }
    }

    /// Settings for a device, created empty on first use.
    pub fn for_device(&mut self, device_key: &str) -> &mut DeviceProfileSettings {
        self.devices.entry(device_key.to_owned()).or_default()
    }

    pub fn device(&self, device_key: &str) -> Option<&DeviceProfileSettings> {
        self.devices.get(device_key)
    }

    pub fn touch(&mut self) {
        self.modified = now_unix();
    }
}

pub use crate::time::now_unix;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    #[serde(default = "yes")]
    pub low_battery_warnings: bool,
    #[serde(default = "default_low_battery")]
    pub low_battery_threshold_percent: u8,
    #[serde(default)]
    pub open_rgb_enabled: bool,
    #[serde(default = "default_loopback")]
    pub open_rgb_host: String,
    #[serde(default = "default_open_rgb_port")]
    pub open_rgb_port: u16,
    /// A non-loopback OpenRGB host is refused unless this is explicitly turned on: the protocol
    /// is unauthenticated, so pointing it off-machine without being asked would be a surprise.
    #[serde(default)]
    pub open_rgb_allow_remote: bool,
    #[serde(default)]
    pub automation_api_enabled: bool,
    #[serde(default = "default_automation_port")]
    pub automation_api_port: u16,
    #[serde(default)]
    pub allow_onboard_writes: bool,
    #[serde(default)]
    pub log_protocol_transactions: bool,
    #[serde(default)]
    pub log_protocol_payloads: bool,
    /// User swatches for the freestyle colour picker, `#RRGGBB`.
    #[serde(default)]
    pub custom_colors: Vec<String>,
}

fn default_low_battery() -> u8 {
    15
}
fn default_loopback() -> String {
    "127.0.0.1".to_owned()
}
fn default_open_rgb_port() -> u16 {
    6742
}
fn default_automation_port() -> u16 {
    9696
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            low_battery_warnings: true,
            low_battery_threshold_percent: 15,
            open_rgb_enabled: false,
            open_rgb_host: default_loopback(),
            open_rgb_port: 6742,
            open_rgb_allow_remote: false,
            automation_api_enabled: false,
            automation_api_port: 9696,
            allow_onboard_writes: false,
            log_protocol_transactions: false,
            log_protocol_payloads: false,
            custom_colors: Vec::new(),
        }
    }
}

/// Backup of readable device flash state, taken before any onboard write.
///
/// Stored outside the profile document on purpose. A profile is something a user copies between
/// machines; a flash image is specific to one device and must never travel with one.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardSnapshot {
    #[serde(default)]
    pub device_key: String,
    #[serde(default)]
    pub device_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware: Option<String>,
    #[serde(default)]
    pub taken: i64,
    /// Sector id to hex contents.
    #[serde(default)]
    pub sectors_hex: HashMap<u16, String>,
}

/// Root document the store persists.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDocument {
    #[serde(default = "current_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub profiles: Vec<PeripheralProfile>,
    #[serde(default)]
    pub macros: Vec<MacroDefinition>,
    #[serde(default)]
    pub triggers: Vec<TriggerRule>,
    #[serde(default)]
    pub app_lighting: Vec<AppLightingRule>,
    /// Manually locked profile. Wins over every rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked_profile_id: Option<Id>,
    /// Last profile chosen by hand, used as the fallback instead of the default one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persistent_profile_id: Option<Id>,
    #[serde(default = "yes")]
    pub auto_switch_enabled: bool,
    #[serde(default)]
    pub app: AppSettings,
}

pub const CURRENT_SCHEMA_VERSION: u32 = 2;

fn current_schema_version() -> u32 {
    CURRENT_SCHEMA_VERSION
}

impl Default for ProfileDocument {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            profiles: Vec::new(),
            macros: Vec::new(),
            triggers: Vec::new(),
            app_lighting: Vec::new(),
            locked_profile_id: None,
            persistent_profile_id: None,
            auto_switch_enabled: true,
            app: AppSettings::default(),
        }
    }
}

impl ProfileDocument {
    /// A fresh document with one profile, so the application always has something to apply.
    pub fn create_default() -> Self {
        let mut profile = PeripheralProfile::new("Desktop (default)");
        profile.is_default = true;
        Self { profiles: vec![profile], ..Self::default() }
    }

    /// The profile marked default, or the first one. `None` only for an empty document, which the
    /// store refuses to save.
    pub fn default_profile(&self) -> Option<&PeripheralProfile> {
        self.profiles.iter().find(|p| p.is_default).or_else(|| self.profiles.first())
    }

    pub fn profile(&self, id: Id) -> Option<&PeripheralProfile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    pub fn profile_mut(&mut self, id: Id) -> Option<&mut PeripheralProfile> {
        self.profiles.iter_mut().find(|p| p.id == id)
    }

    pub fn macro_by_id(&self, id: Id) -> Option<&MacroDefinition> {
        self.macros.iter().find(|m| m.id == id)
    }

    /// Profiles that bind the given macro, for the "still in use" warning on delete.
    pub fn profiles_using_macro(&self, macro_id: Id) -> Vec<&PeripheralProfile> {
        self.profiles
            .iter()
            .filter(|p| {
                p.devices.values().any(|d| {
                    d.bindings.values().any(|b| b.action.macro_id() == Some(macro_id))
                })
            })
            .collect()
    }
}

fn yes() -> bool {
    true
}
fn one_usize() -> usize {
    1
}

/// `#RRGGBB`, the only colour form these files use.
pub fn is_hex_color(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Parses `#RRGGBB` into an LED colour.
pub fn parse_color(s: &str) -> Option<ghub_hidpp::LedColor> {
    if !is_hex_color(s) {
        return None;
    }
    let value = u32::from_str_radix(&s[1..], 16).ok()?;
    Some(ghub_hidpp::LedColor {
        r: (value >> 16) as u8,
        g: (value >> 8) as u8,
        b: value as u8,
    })
}

pub fn format_color(color: ghub_hidpp::LedColor) -> String {
    format!("#{:02X}{:02X}{:02X}", color.r, color.g, color.b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{ActionDefinition, KeyStroke};

    #[test]
    fn a_fresh_document_always_has_a_profile_to_apply() {
        let doc = ProfileDocument::create_default();
        assert_eq!(doc.profiles.len(), 1);
        assert!(doc.default_profile().is_some());
        assert!(doc.default_profile().unwrap().is_default);
        assert_eq!(doc.schema_version, CURRENT_SCHEMA_VERSION);
        assert!(doc.auto_switch_enabled);
    }

    #[test]
    fn the_default_profile_falls_back_to_the_first_when_none_is_marked() {
        let mut doc = ProfileDocument::default();
        doc.profiles.extend([PeripheralProfile::new("a"), PeripheralProfile::new("b")]);
        assert_eq!(doc.default_profile().unwrap().name, "a");
        doc.profiles[1].is_default = true;
        assert_eq!(doc.default_profile().unwrap().name, "b");
        // An empty document has no answer rather than a made-up one.
        doc.profiles.clear();
        assert!(doc.default_profile().is_none());
    }

    #[test]
    fn match_kinds_are_ordered_most_specific_first() {
        // A broad title rule must never outrank the exact path a user wrote for one program.
        assert!(AppMatchKind::ExactPath.specificity() < AppMatchKind::ExecutableName.specificity());
        assert!(
            AppMatchKind::ExecutableName.specificity()
                < AppMatchKind::ExecutableAndTitle.specificity()
        );
        assert!(
            AppMatchKind::ExecutableAndTitle.specificity() < AppMatchKind::TitleOnly.specificity()
        );
    }

    #[test]
    fn dpi_stage_cycling_skips_disabled_stages_and_wraps() {
        let mut mouse = MouseSettings {
            dpi_stages: vec![
                DpiStage::new(400),
                DpiStage { dpi: 800, enabled: false },
                DpiStage::new(1600),
            ],
            ..MouseSettings::default()
        };
        assert_eq!(mouse.next_stage(0, false), Some(2), "stage 1 is disabled");
        assert_eq!(mouse.next_stage(2, false), Some(0), "wraps to the start");
        assert_eq!(mouse.next_stage(0, true), Some(2), "backwards wraps too");
        assert_eq!(mouse.next_stage(2, true), Some(0));

        // Every stage disabled: there is nothing to switch to, and reporting stage 0 would set a
        // DPI the user turned off.
        for stage in &mut mouse.dpi_stages {
            stage.enabled = false;
        }
        assert_eq!(mouse.next_stage(0, false), None);
        assert_eq!(mouse.active_dpi(), None);
    }

    #[test]
    fn the_active_dpi_skips_a_disabled_active_stage() {
        let mouse = MouseSettings {
            dpi_stages: vec![DpiStage { dpi: 400, enabled: false }, DpiStage::new(1600)],
            active_stage: 0,
            ..MouseSettings::default()
        };
        assert_eq!(mouse.active_dpi(), Some(1600), "the first enabled stage, not the disabled one");
    }

    #[test]
    fn an_app_lighting_override_leaves_unset_members_alone() {
        let mut base = LightingSettings {
            mode: LightingMode::Static,
            brightness: 0.5,
            default_color: "#112233".into(),
            power_save_idle_seconds: Some(60),
            ..LightingSettings::default()
        };
        base.static_colors.insert("key:Escape".into(), "#FF0000".into());
        base.static_colors.insert("key:W".into(), "#00FF00".into());

        let mut over = AppLightingOverride::default();
        over.static_colors.insert("key:W".into(), "#0000FF".into());
        let result = over.apply_to(&base);

        assert_eq!(result.brightness, 0.5, "an unset brightness keeps the profile's");
        assert_eq!(result.mode, LightingMode::Static);
        assert_eq!(result.power_save_idle_seconds, Some(60));
        assert_eq!(result.default_color, "#112233");
        // Named colours merge, so recolouring W must not blank Escape.
        assert_eq!(result.static_colors.get("key:Escape").unwrap(), "#FF0000");
        assert_eq!(result.static_colors.get("key:W").unwrap(), "#0000FF");
    }

    #[test]
    fn colours_round_trip_through_hex() {
        assert!(is_hex_color("#AABBCC"));
        assert!(is_hex_color("#aabbcc"));
        assert!(!is_hex_color("AABBCC"), "the hash is required");
        assert!(!is_hex_color("#AABBC"), "six digits, not five");
        assert!(!is_hex_color("#AABBCG"), "G is not a hex digit");
        assert!(!is_hex_color(""));

        let color = parse_color("#102030").unwrap();
        assert_eq!((color.r, color.g, color.b), (0x10, 0x20, 0x30));
        assert_eq!(format_color(color), "#102030");
        assert!(parse_color("not a colour").is_none());
    }

    #[test]
    fn a_macro_still_bound_somewhere_is_reported_as_in_use() {
        let mut doc = ProfileDocument::create_default();
        let m = MacroDefinition::new("reload");
        let id = m.id;
        doc.macros.push(m);

        assert!(doc.profiles_using_macro(id).is_empty());

        let profile_id = doc.profiles[0].id;
        doc.profile_mut(profile_id)
            .unwrap()
            .for_device("logitech-hidpp:u1")
            .bindings
            .insert("gkey:G1:M1".into(), ActionBinding::new(ActionDefinition::MacroRef { macro_id: id }));

        let using = doc.profiles_using_macro(id);
        assert_eq!(using.len(), 1);
        assert_eq!(using[0].id, profile_id);
        // A different macro id must not be reported as in use.
        assert!(doc.profiles_using_macro(Id::new()).is_empty());
    }

    #[test]
    fn for_device_creates_settings_on_first_use_and_reuses_them_after() {
        let mut profile = PeripheralProfile::new("test");
        assert!(profile.device("k").is_none());
        profile
            .for_device("k")
            .bindings
            .insert("button:1".into(), ActionBinding::new(ActionDefinition::Disabled));
        assert_eq!(profile.for_device("k").bindings.len(), 1, "the same settings, not a new set");
        assert_eq!(profile.devices.len(), 1);
    }

    #[test]
    fn the_document_round_trips_through_json_in_the_c_sharp_shape() {
        let mut doc = ProfileDocument::create_default();
        let profile_id = doc.profiles[0].id;
        doc.profiles[0].rules.push(AppRule::by_executable("game.exe"));
        doc.profiles[0].modified = 1_700_000_000;
        {
            let device = doc.profile_mut(profile_id).unwrap().for_device("logitech-hidpp:u1");
            device.mouse = Some(MouseSettings::default());
            device.keyboard = Some(KeyboardSettings::default());
            device.bindings.insert(
                "gkey:G1:M1".into(),
                ActionBinding::new(ActionDefinition::Key(KeyStroke::new(0x41))),
            );
        }
        doc.triggers.push(TriggerRule::new("low battery", TriggerEvent::BatteryBelow));

        let json = serde_json::to_string_pretty(&doc).unwrap();
        // Field names are camelCase and enum values are their verbatim names, which is what the
        // C# JsonSerializerOptions produced.
        assert!(json.contains("\"schemaVersion\": 2"), "{json}");
        assert!(json.contains("\"isDefault\": true"));
        assert!(json.contains("\"executableName\": \"game.exe\""));
        assert!(json.contains("\"kind\": \"ExecutableName\""), "{json}");
        assert!(json.contains("\"mode\": \"Untouched\""));
        assert!(json.contains("\"event\": \"BatteryBelow\""));
        assert!(json.contains("\"dpiStages\""));

        let back: ProfileDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(back, doc);
    }

    #[test]
    fn a_sparse_document_loads_with_every_default_filled_in() {
        // The minimum a hand-written or partially-migrated file can be. Every absent field has to
        // take its default rather than failing the load, or one missing key costs the user their
        // whole profile set.
        let doc: ProfileDocument = serde_json::from_str(r#"{"profiles":[{"id":"01"}]}"#).unwrap();
        assert_eq!(doc.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(doc.profiles[0].name, "Default");
        assert!(doc.auto_switch_enabled);
        assert_eq!(doc.app.open_rgb_host, "127.0.0.1");
        assert_eq!(doc.app.low_battery_threshold_percent, 15);
        assert!(!doc.app.allow_onboard_writes, "writes stay off unless the file says otherwise");
        assert!(doc.profiles[0].devices.is_empty());
    }

    #[test]
    fn lighting_defaults_to_leaving_the_device_alone() {
        // Starting this application must not black out a keyboard that was lit the way the user
        // liked it.
        assert_eq!(LightingSettings::default().mode, LightingMode::Untouched);
        assert_eq!(LightingMode::default(), LightingMode::Untouched);
        assert_eq!(DeviceProfileSettings::default().lighting.mode, LightingMode::Untouched);
    }
}
