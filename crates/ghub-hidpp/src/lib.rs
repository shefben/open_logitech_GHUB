//! HID++ transport for Logitech devices. Port of `Artemis.Peripherals/{Hid,Logitech/Hidpp}`.
//!
//! Layering, bottom up:
//!
//! - [`hid`] - HID collection enumeration and the connection traits, platform-agnostic.
//! - [`descriptor`] - HID report descriptor parsing, for the report lengths `hidapi` omits.
//! - [`hidapi_backend`] - the real backend, opening collections in shared mode.
//! - [`report`] - HID++ report framing and parsing.
//! - [`channel`] - one serialised request/response channel over a device's vendor collections.
//! - [`error`] - protocol error classification, including which errors are worth a retry.
//!
//! Safety rule carried over from the C#: read before write, and only ever send write commands
//! whose format has been verified. Features are never discovered by sending unknown write ids.

//! - [`identity`] - how a device is named and keyed, stably across reconnects.
//! - [`model`] - the verified Logitech identifier table.
//! - [`session`] - one open device: capabilities, feature accessors, notification decoding.
//! - [`provider`] - enumeration, receiver-slot probing and channel sharing.

pub mod capability;
pub mod channel;
pub mod color;
pub mod descriptor;
pub mod device;
pub mod error;
pub mod features;
pub mod hid;
pub mod hidapi_backend;
pub mod identity;
pub mod model;
pub mod provider;
pub mod replay;
pub mod report;
pub mod session;
pub mod sha256;

pub use capability::{CapabilityMeta, CapabilityStatus, DeviceCapabilities, PersistenceKind};
pub use color::LedColor;
pub use channel::{HidppChannel, RequestOptions};
pub use device::{HidppDevice, HidppFeatureInfo, Notification, feature};
pub use error::{Hidpp10Error, Hidpp20Error, HidppError, ProtocolError, Result};
pub use hid::{HidBackend, HidConnection, HidInterfaceInfo, LOGITECH_VID};
pub use hidapi_backend::HidApiBackend;
pub use identity::{
    ConnectionType, DeviceDescriptor, DeviceEvent, DeviceHealth, DeviceIdentity, DeviceNotification,
    PeripheralKind,
};
pub use model::{LogitechModel, ModelFamily};
pub use provider::{DiscoveredDevice, LogitechHidppProvider, ProviderOptions};
pub use replay::{HidTranscript, ReplayHidBackend};
pub use report::{HidppReport, constants};
pub use session::{Candidate, LogitechHidppSession, SessionOptions};
