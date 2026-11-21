//! HID++ 2.0 feature wrappers. Port of
//! `Artemis.Peripherals/Logitech/Hidpp/Features/HidppFeatures.cs`.
//!
//! One module per feature, each built by probing the device's discovered feature table and
//! returning `Ok(None)` when the device does not implement it. Two rules hold throughout:
//!
//! - **Read before write.** Every setter reads the value back and returns what the device now
//!   reports, rather than the value we asked for. The G903's sensor silently rounds, and a UI
//!   showing the requested value would be lying.
//! - **Writes are volatile unless stated.** Nothing here touches onboard flash; that is
//!   [`super::onboard`]'s job and is gated behind an explicit opt-in.
//!
//! The C# wrapped each of these in an interface (`IDpiFeature` and friends) so Artemis's DI
//! container could inject them. The container is gone, so these are plain structs.

pub mod battery;
pub mod controls;
pub mod device_info;
pub mod dpi;
pub mod keys;
pub mod lighting;
pub mod mouse;
pub mod onboard;
pub mod onboard_writer;
pub mod report_rate;

/// Replay harnesses shared by this crate's tests and by `ghub-core`'s.
///
/// Public and ungated rather than `#[cfg(test)]`: the device manager in `ghub-core` has to be
/// driven against a recorded device, and a `dev-dependency` back onto this crate's test code is
/// not expressible in Cargo. The cost is a few hundred bytes of dead code in a debug build and
/// nothing at all in release, where it is eliminated.
pub mod test_support;

pub use battery::{BatteryFeature, BatteryStatus, ChargingState};
pub use controls::{ControlNames, ReprogControlsFeature};
pub use device_info::{DeviceInformation, FirmwareEntity, read_information, read_name};
pub use dpi::{DpiFeature, DpiState};
pub use keys::{BrightnessFeature, DisableKeysByUsageFeature, GKeyFeature, ModeKeyFeature};
pub use lighting::{
    PerKeyLightingFeature, RgbClusterInfo, RgbEffectInfo, RgbEffectsFeature, RgbPowerTimers,
};
pub use mouse::{HiResWheelFeature, HiResWheelInfo, MouseButtonSpyFeature};
pub use onboard::{
    OnboardButtonBinding, OnboardMode, OnboardProfile, OnboardProfilesFeature, crc,
};
pub use onboard_writer::{
    CommitRefusal, OnboardDiff, OnboardEdit, OnboardProfileWriter, OnboardSnapshot, OnboardWritePlan,
    OnboardWriteError, WriteFailure, check_commit_preconditions,
};
pub use report_rate::ReportRateFeature;
