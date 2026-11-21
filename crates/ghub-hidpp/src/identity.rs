//! Device identity, description and health. Port of
//! `Artemis.Peripherals/Abstractions/DeviceDescriptor.cs` plus the notification records from
//! `Abstractions/Contracts.cs`.
//!
//! The C# split these across a provider contract so Artemis could host third-party providers.
//! There is one provider here and there will only ever be one, so the interfaces are gone and the
//! data records remain - they are what the UI binds to and what the profile store keys on.

use crate::features::battery::BatteryStatus;

/// Kind of physical peripheral.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum PeripheralKind {
    #[default]
    Unknown,
    Mouse,
    Keyboard,
    Receiver,
    Headset,
    Other,
}

/// How the peripheral reaches the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ConnectionType {
    #[default]
    Unknown,
    Wired,
    LightspeedReceiver,
    Bluetooth,
    Network,
}

impl ConnectionType {
    /// Label as G HUB writes it in the device tile.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Wired => "Wired",
            Self::LightspeedReceiver => "LIGHTSPEED",
            Self::Bluetooth => "Bluetooth",
            Self::Network => "Network",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeviceConnectionState {
    #[default]
    Connected,
    Disconnected,
    Error,
}

/// Stable identity of a physical device.
///
/// Paths are deliberately not part of the identity: Windows HID paths change on reconnect, and a
/// mouse that moves from its cable to its receiver must keep the same profile. The provider builds
/// `stable_id` from the HID++ unit id, which is the same over both links.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct DeviceIdentity {
    pub provider_id: String,
    pub stable_id: String,
}

impl DeviceIdentity {
    pub fn new(provider_id: impl Into<String>, stable_id: impl Into<String>) -> Self {
        Self { provider_id: provider_id.into(), stable_id: stable_id.into() }
    }

    pub fn key(&self) -> String {
        format!("{}:{}", self.provider_id, self.stable_id)
    }
}

impl std::fmt::Display for DeviceIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.provider_id, self.stable_id)
    }
}

/// Immutable description of a discovered device.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeviceDescriptor {
    pub identity: DeviceIdentity,
    pub display_name: String,
    /// The name the device reports through 0x0005, which can differ from the marketing name.
    pub model_name: Option<String>,
    pub kind: PeripheralKind,
    pub connection_type: ConnectionType,
    pub vendor_id: u16,
    pub product_id: u16,
    /// Product id reported over the wireless link, for receiver-attached devices.
    pub wireless_product_id: Option<u16>,
    /// Receiver slot / HID++ device index.
    pub device_index: Option<u8>,
    /// Read and displayed only. Nothing in this application flashes firmware.
    pub firmware_version: Option<String>,
    pub serial_number: Option<String>,
    /// True when the model was matched against the verified identifier table rather than inferred.
    pub is_known_model: bool,
}

impl DeviceDescriptor {
    pub fn new(identity: DeviceIdentity, display_name: impl Into<String>) -> Self {
        Self {
            identity,
            display_name: display_name.into(),
            model_name: None,
            kind: PeripheralKind::Unknown,
            connection_type: ConnectionType::Unknown,
            vendor_id: 0,
            product_id: 0,
            wireless_product_id: None,
            device_index: None,
            firmware_version: None,
            serial_number: None,
            is_known_model: false,
        }
    }

    pub fn usb_id_string(&self) -> String {
        let base = format!("{:04X}:{:04X}", self.vendor_id, self.product_id);
        match self.wireless_product_id {
            Some(w) => format!("{base} (wireless {w:04X})"),
            None => base,
        }
    }
}

/// Device health snapshot. A `None` field means "not exposed by the protocol", never a guess.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceHealth {
    pub state: DeviceConnectionState,
    pub battery: Option<BatteryStatus>,
    pub last_error: Option<String>,
}

/// A device-originated event, delivered from the channel's reader thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceEvent {
    /// A G-key changed state. `gkey` is 1-based.
    GKey { gkey: usize, pressed: bool },
    /// An M-key was pressed. `mode` is 1-based.
    ModeKey { mode: usize },
    /// The MR (macro record) key changed state.
    MacroRecordKey { pressed: bool },
    /// A diverted 0x1B04 control changed state.
    Button { control_id: u16, pressed: bool },
    /// A physical mouse button seen through the 0x8110 spy. `button` is 1-based.
    SpyButton { button: usize, pressed: bool },
    /// The wireless link came up or went down. A link coming up means every volatile setting on
    /// the device was reset and has to be re-applied.
    Link { established: bool },
    Battery(BatteryStatus),
}

/// A `DeviceEvent` tagged with the device it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceNotification {
    pub device: DeviceIdentity,
    pub event: DeviceEvent,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identity_key_is_stable_and_printable() {
        let id = DeviceIdentity::new("logitech-hidpp", "u0123456789abcdef");
        assert_eq!(id.key(), "logitech-hidpp:u0123456789abcdef");
        assert_eq!(id.to_string(), id.key());
    }

    #[test]
    fn the_usb_id_string_names_the_wireless_pid_when_there_is_one() {
        let mut d = DeviceDescriptor::new(DeviceIdentity::new("p", "s"), "G903");
        d.vendor_id = 0x046D;
        d.product_id = 0xC539;
        assert_eq!(d.usb_id_string(), "046D:C539");
        d.wireless_product_id = Some(0x4087);
        assert_eq!(d.usb_id_string(), "046D:C539 (wireless 4087)");
    }

    #[test]
    fn connection_labels_match_g_hub() {
        assert_eq!(ConnectionType::LightspeedReceiver.label(), "LIGHTSPEED");
        assert_eq!(ConnectionType::Wired.label(), "Wired");
    }
}
