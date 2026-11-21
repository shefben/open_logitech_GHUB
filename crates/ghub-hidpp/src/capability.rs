//! What a device can do, as discovered at runtime.
//! Port of `Artemis.Peripherals/Abstractions/Capabilities.cs`.
//!
//! Capabilities are never assumed from a product id. Every field here was read back from the
//! device, and a capability that could not be read says so through [`CapabilityStatus`] rather
//! than defaulting to something plausible. The UI greys a control out on the strength of these
//! values, so a wrong optimistic default here becomes a write the firmware rejects.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CapabilityStatus {
    /// Readable and writable on this device and firmware.
    #[default]
    Supported,
    /// Exposed by the device but only safely readable: writes disabled by policy or protocol.
    ReadOnly,
    /// Device or firmware does not expose this feature.
    NotSupported,
    /// Feature exists but is currently unreachable: device asleep, owned by other software, or
    /// erroring.
    Unavailable,
}

/// Where a change made through a capability ends up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PersistenceKind {
    None,
    /// Device RAM; lost on power cycle. Safe to write.
    #[default]
    Volatile,
    /// Onboard memory (flash). Written only through an explicit "Save to device" action.
    Onboard,
    /// Executed or stored by this application only.
    Software,
}

/// Metadata every capability carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityMeta {
    pub status: CapabilityStatus,
    /// Human-readable reason when not fully supported, shown next to the disabled control.
    pub reason: Option<String>,
    pub persistence: PersistenceKind,
    /// HID++ feature id backing this capability, for the diagnostics page.
    pub feature_id: Option<u16>,
}

impl Default for CapabilityMeta {
    fn default() -> Self {
        Self {
            status: CapabilityStatus::Supported,
            reason: None,
            persistence: PersistenceKind::Volatile,
            feature_id: None,
        }
    }
}

impl CapabilityMeta {
    pub fn supported(feature_id: u16, persistence: PersistenceKind) -> Self {
        Self {
            status: CapabilityStatus::Supported,
            reason: None,
            persistence,
            feature_id: Some(feature_id),
        }
    }

    pub fn unavailable(feature_id: u16, reason: impl Into<String>) -> Self {
        Self {
            status: CapabilityStatus::Unavailable,
            reason: Some(reason.into()),
            persistence: PersistenceKind::None,
            feature_id: Some(feature_id),
        }
    }

    pub fn read_only(feature_id: u16, reason: impl Into<String>) -> Self {
        Self {
            status: CapabilityStatus::ReadOnly,
            reason: Some(reason.into()),
            persistence: PersistenceKind::Onboard,
            feature_id: Some(feature_id),
        }
    }

    /// Whether a control backed by this capability may be written.
    pub fn is_usable(&self) -> bool {
        self.status == CapabilityStatus::Supported
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DpiCapability {
    #[serde(flatten)]
    pub meta: CapabilityMeta,
    pub sensor_count: usize,
    pub minimum: u32,
    pub maximum: u32,
    /// Step for range-style lists; 0 when `discrete_values` is used.
    pub step: u32,
    pub discrete_values: Vec<u32>,
    /// DPI stages the app manages. Onboard profiles expose five on supported mice.
    pub max_stages: usize,
}

impl Default for DpiCapability {
    fn default() -> Self {
        Self {
            meta: CapabilityMeta::default(),
            sensor_count: 1,
            minimum: 0,
            maximum: 0,
            step: 0,
            discrete_values: Vec::new(),
            max_stages: 5,
        }
    }
}

impl DpiCapability {
    /// Whether the sensor accepts this exact value. Checked before every write: the device
    /// reported this range, so a value outside it is our bug, not the user's.
    pub fn is_valid(&self, dpi: u32) -> bool {
        if !self.discrete_values.is_empty() {
            return self.discrete_values.contains(&dpi);
        }
        if dpi < self.minimum || dpi > self.maximum {
            return false;
        }
        self.step == 0 || (dpi - self.minimum).is_multiple_of(self.step)
    }

    /// Snaps an arbitrary value to the nearest the sensor accepts, for a dragged slider.
    pub fn clamp(&self, dpi: u32) -> u32 {
        if !self.discrete_values.is_empty() {
            return self
                .discrete_values
                .iter()
                .copied()
                .min_by_key(|v| v.abs_diff(dpi))
                .unwrap_or(dpi);
        }
        let clamped = dpi.clamp(self.minimum, self.maximum);
        if self.step == 0 {
            return clamped;
        }
        // Round to the nearest step rather than truncating, so dragging a slider does not always
        // land below where the user let go.
        let steps = (clamped - self.minimum + self.step / 2) / self.step;
        (self.minimum + steps * self.step).clamp(self.minimum, self.maximum)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportRateCapability {
    #[serde(flatten)]
    pub meta: CapabilityMeta,
    /// Supported polling rates in Hz, ascending.
    pub rates_hz: Vec<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ButtonDescriptor {
    /// HID++ control id (CID).
    pub control_id: u16,
    pub task_id: u16,
    pub name: String,
    pub is_mouse_button: bool,
    /// Can be diverted to software, temporarily and in RAM only.
    pub divertable: bool,
    /// Can be persistently remapped in firmware.
    pub persistable: bool,
    pub reprogrammable: bool,
}

impl ButtonDescriptor {
    /// A button the assignment UI may offer. Only diversion is used, never persistent remapping,
    /// so divertable is the whole test.
    pub fn assignable(&self) -> bool {
        self.divertable
    }

    pub fn disabled_reason(&self) -> Option<&'static str> {
        if self.divertable {
            None
        } else {
            Some("Firmware does not allow diverting this control to software")
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ButtonsCapability {
    #[serde(flatten)]
    pub meta: CapabilityMeta,
    pub buttons: Vec<ButtonDescriptor>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardProfilesCapability {
    #[serde(flatten)]
    pub meta: CapabilityMeta,
    pub slot_count: usize,
    pub rom_slot_count: usize,
    pub button_count: usize,
    pub sector_size: usize,
    pub sector_count: usize,
    pub profile_format: u8,
    pub memory_model: u8,
    /// Writing onboard memory is off unless the user explicitly enables experimental writes, and
    /// even then it goes read, compare, write, verify.
    pub writes_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BatterySource {
    #[default]
    None,
    LevelStatus,
    Voltage,
    Unified,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatteryCapability {
    #[serde(flatten)]
    pub meta: CapabilityMeta,
    pub source: BatterySource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LightingZone {
    pub index: usize,
    pub name: String,
    pub led_count: usize,
}

impl Default for LightingZone {
    fn default() -> Self {
        Self { index: 0, name: String::new(), led_count: 1 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LightingKind {
    #[default]
    None,
    /// Per-key addressable (G815, feature 0x8081).
    PerKey,
    /// Zone colour effects (0x8070 / 0x8071).
    Zones,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LightingCapability {
    #[serde(flatten)]
    pub meta: CapabilityMeta,
    pub kind: LightingKind,
    pub zones: Vec<LightingZone>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GKeysCapability {
    #[serde(flatten)]
    pub meta: CapabilityMeta,
    pub count: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeKeysCapability {
    #[serde(flatten)]
    pub meta: CapabilityMeta,
    pub count: usize,
    pub has_macro_record_key: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameModeCapability {
    #[serde(flatten)]
    pub meta: CapabilityMeta,
}

/// Everything a device can do, discovered at runtime.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCapabilities {
    pub schema_version: u32,
    pub dpi: Option<DpiCapability>,
    pub report_rate: Option<ReportRateCapability>,
    pub buttons: Option<ButtonsCapability>,
    pub onboard_profiles: Option<OnboardProfilesCapability>,
    pub battery: Option<BatteryCapability>,
    pub lighting: Option<LightingCapability>,
    pub gkeys: Option<GKeysCapability>,
    pub mode_keys: Option<ModeKeysCapability>,
    pub game_mode: Option<GameModeCapability>,
    /// Raw feature list ("0x2201 AdjustableDpi v1 @ idx 7") for the diagnostics page.
    pub raw_features: Vec<String>,
}

impl DeviceCapabilities {
    pub const CURRENT_SCHEMA_VERSION: u32 = 1;

    pub fn new() -> Self {
        Self { schema_version: Self::CURRENT_SCHEMA_VERSION, ..Default::default() }
    }

    /// Name and status of each capability, in the order the diagnostics page lists them.
    pub fn enumerate(&self) -> Vec<(&'static str, Option<&CapabilityMeta>)> {
        vec![
            ("DPI", self.dpi.as_ref().map(|c| &c.meta)),
            ("Report rate", self.report_rate.as_ref().map(|c| &c.meta)),
            ("Buttons", self.buttons.as_ref().map(|c| &c.meta)),
            ("Onboard profiles", self.onboard_profiles.as_ref().map(|c| &c.meta)),
            ("Battery", self.battery.as_ref().map(|c| &c.meta)),
            ("Lighting", self.lighting.as_ref().map(|c| &c.meta)),
            ("G-keys", self.gkeys.as_ref().map(|c| &c.meta)),
            ("M-keys", self.mode_keys.as_ref().map(|c| &c.meta)),
            ("Game mode", self.game_mode.as_ref().map(|c| &c.meta)),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_discrete_dpi_list_accepts_only_its_own_values() {
        // A G903 sensor reporting a fixed list: 801 is not on it, so the slider must not send it.
        let dpi = DpiCapability {
            discrete_values: vec![400, 800, 1600, 3200],
            minimum: 400,
            maximum: 3200,
            ..Default::default()
        };
        assert!(dpi.is_valid(800));
        assert!(!dpi.is_valid(801));
        assert_eq!(dpi.clamp(900), 800, "snaps to the nearer value");
        assert_eq!(dpi.clamp(1300), 1600);
        assert_eq!(dpi.clamp(99_999), 3200);
    }

    #[test]
    fn a_stepped_dpi_range_accepts_only_values_on_the_step() {
        // 200-16000 in steps of 50, the shape the G903 HERO actually reports.
        let dpi = DpiCapability { minimum: 200, maximum: 16000, step: 50, ..Default::default() };
        assert!(dpi.is_valid(200));
        assert!(dpi.is_valid(1250));
        assert!(!dpi.is_valid(1275), "off the step");
        assert!(!dpi.is_valid(16050), "past the maximum");
        // Rounds to the nearest step, so a dragged slider does not always land low.
        assert_eq!(dpi.clamp(1274), 1250);
        assert_eq!(dpi.clamp(1276), 1300);
        assert_eq!(dpi.clamp(0), 200);
        assert_eq!(dpi.clamp(99_999), 16000);
        // Every clamped value must itself be valid, or the write we send gets rejected.
        for raw in [0, 1, 199, 201, 7777, 15_999, 16_001, 99_999] {
            assert!(dpi.is_valid(dpi.clamp(raw)), "clamp({raw}) produced an invalid value");
        }
    }

    #[test]
    fn an_unreadable_capability_is_not_usable() {
        let meta = CapabilityMeta::unavailable(0x2201, "device asleep");
        assert!(!meta.is_usable());
        assert_eq!(meta.persistence, PersistenceKind::None);
        // Read-only is also not writable: onboard writes stay behind the explicit opt-in.
        assert!(!CapabilityMeta::read_only(0x8100, "unknown format").is_usable());
        assert!(CapabilityMeta::supported(0x2201, PersistenceKind::Volatile).is_usable());
    }

    #[test]
    fn a_non_divertable_button_is_not_assignable_and_says_why() {
        let fixed = ButtonDescriptor { control_id: 0x0050, divertable: false, ..Default::default() };
        assert!(!fixed.assignable());
        assert!(fixed.disabled_reason().is_some());
        let free = ButtonDescriptor { divertable: true, ..fixed.clone() };
        assert!(free.assignable());
        assert_eq!(free.disabled_reason(), None);
    }

    #[test]
    fn capabilities_enumerate_in_the_diagnostics_order() {
        let caps = DeviceCapabilities::new();
        let names: Vec<_> = caps.enumerate().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names[0], "DPI");
        assert_eq!(names.len(), 9);
        // Nothing discovered yet, so every slot is absent rather than defaulted to Supported.
        assert!(caps.enumerate().iter().all(|(_, m)| m.is_none()));
    }
}
