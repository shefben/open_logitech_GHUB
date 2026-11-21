//! An open HID++ 2.0 device. Port of `Artemis.Peripherals/Logitech/LogitechHidppSession.cs`.
//!
//! The session probes the device once, turns the answers into the [`DeviceCapabilities`] the UI
//! binds to, and then hands out feature wrappers on demand.
//!
//! The C# kept live feature objects in a `Dictionary<Type, IDeviceFeature>` and handed them back
//! from `GetFeature<T>()`. That shape does not translate: every feature here borrows the device it
//! talks to, so storing them next to the device would make the session self-referential. Instead
//! the session stores the *probe results* - the small, owned answers each feature's constructor
//! got from the device - and the accessors rebuild the wrapper against `&self.device` with no
//! further traffic. Same cost, no unsafe, and a feature can never outlive its device.
//!
//! One probe failure never takes the device down: [`tolerate`] logs it, records a warning the
//! diagnostics page shows, and leaves that capability absent. A *disconnect* during probing is
//! different and propagates, because nothing else we ask will work either.

use std::sync::Arc;

use crate::capability::*;
use crate::channel::{HidppChannel, RequestOptions};
use crate::device::{HidppDevice, Notification, feature};
use crate::error::{HidppError, Result};
use crate::features::battery::{self, BatteryFeature};
use crate::features::controls::{self, ReprogControlsFeature};
use crate::features::device_info::{self, DeviceInformation};
use crate::features::dpi::DpiFeature;
use crate::features::keys::{
    self, BrightnessFeature, DisableKeysByUsageFeature, GKeyFeature, ModeKeyFeature,
};
use crate::features::lighting::{
    PerKeyLightingFeature, RgbClusterInfo, RgbEffectsFeature,
};
use crate::features::mouse::{self, HiResWheelFeature, HiResWheelInfo, MouseButtonSpyFeature};
use crate::features::onboard::OnboardProfilesFeature;
use crate::features::report_rate::ReportRateFeature;
use crate::identity::{
    ConnectionType, DeviceEvent, DeviceHealth, DeviceConnectionState, DeviceDescriptor,
    DeviceIdentity, PeripheralKind,
};
use crate::model::{self, LogitechModel};
use crate::report::HidppReport;
use crate::report::constants::WIRED_DEVICE_INDEX;

/// Hardware safety policy. Port of `LogitechProviderOptions`.
#[derive(Debug, Clone, Default)]
pub struct SessionOptions {
    /// Onboard (flash) profile writes. Off by default; even when on, a write still needs an
    /// explicit Save-to-Device with a backup, a diff and a read-back.
    pub allow_onboard_writes: bool,
    /// Per-key LED ids for this device's layout.
    ///
    /// Injected rather than looked up here: the keyboard layouts live in `ghub-core`, which
    /// depends on this crate, so this crate cannot reach them. The C# read `G815Layout.Keys`
    /// directly because both lived in one assembly.
    pub per_key_led_ids: Vec<u8>,
}

/// What the provider learned about a device before opening a session on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub group_key: String,
    pub product_id: u16,
    /// HID++ device index: a receiver slot, or [`WIRED_DEVICE_INDEX`] for a directly attached one.
    pub device_index: u8,
}

/// Everything a feature constructor learned from the device, kept so the wrappers can be rebuilt
/// without re-probing.
#[derive(Debug, Clone, Default)]
struct Probe {
    dpi: Option<DpiCapability>,
    report_rate: Option<ReportRateCapability>,
    buttons: Option<Vec<ButtonDescriptor>>,
    onboard: Option<OnboardProfilesCapability>,
    rgb: Option<(Vec<RgbClusterInfo>, Vec<LightingZone>)>,
    battery: Option<BatterySource>,
    gkeys: Option<usize>,
    mode_keys: Option<(usize, bool)>,
    brightness: Option<(u16, u8)>,
    disable_keys: Option<u8>,
    spy_buttons: Option<usize>,
    wheel: Option<HiResWheelInfo>,
}

pub struct LogitechHidppSession {
    device: HidppDevice,
    candidate: Candidate,
    options: SessionOptions,
    descriptor: DeviceDescriptor,
    capabilities: DeviceCapabilities,
    probe: Probe,
    model: Option<&'static LogitechModel>,
    information: Option<DeviceInformation>,
    /// Owned rather than rebuilt: this one carries state (what each key currently shows, which
    /// key ids the firmware refused) that must survive across frames.
    per_key: Option<PerKeyLightingFeature>,
    /// Edge state for notification decoding: the device reports a mask, the UI wants transitions.
    gkey_mask: u32,
    spy_mask: u16,
    diverted_pressed: Vec<u16>,
    link_lost: bool,
    warnings: Vec<String>,
}

impl std::fmt::Debug for LogitechHidppSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "LogitechHidppSession({}, index 0x{:02X})",
            self.descriptor.display_name, self.candidate.device_index
        )
    }
}

impl LogitechHidppSession {
    /// Opens a session. Probing happens here, so a device that cannot answer never becomes a
    /// session at all.
    pub fn open(
        channel: Arc<HidppChannel>,
        candidate: Candidate,
        identity: DeviceIdentity,
        options: SessionOptions,
    ) -> Result<Self> {
        let device = HidppDevice::new(channel, candidate.device_index);
        let descriptor =
            DeviceDescriptor::new(identity, format!("Logitech {:04X}", candidate.product_id));
        let mut session = Self {
            device,
            candidate,
            options,
            descriptor,
            capabilities: DeviceCapabilities::new(),
            probe: Probe::default(),
            model: None,
            information: None,
            per_key: None,
            gkey_mask: 0,
            spy_mask: 0,
            diverted_pressed: Vec::new(),
            link_lost: false,
            warnings: Vec::new(),
        };
        session.refresh_capabilities()?;
        Ok(session)
    }

    pub fn descriptor(&self) -> &DeviceDescriptor {
        &self.descriptor
    }

    pub fn capabilities(&self) -> &DeviceCapabilities {
        &self.capabilities
    }

    pub fn identity(&self) -> &DeviceIdentity {
        &self.descriptor.identity
    }

    pub fn device(&self) -> &HidppDevice {
        &self.device
    }

    pub fn model(&self) -> Option<&'static LogitechModel> {
        self.model
    }

    pub fn information(&self) -> Option<&DeviceInformation> {
        self.information.as_ref()
    }

    pub fn options(&self) -> &SessionOptions {
        &self.options
    }

    /// Probe failures, one line each, for the diagnostics page. Empty is the healthy case.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn is_connected(&self) -> bool {
        self.device.channel().is_connected() && !self.link_lost
    }

    /// Re-probes the device and rebuilds its capabilities.
    ///
    /// Called on open, and again whenever the wireless link comes back: a device that was asleep
    /// has lost every volatile setting, and its feature table has to be read again rather than
    /// assumed to be the one from before.
    pub fn refresh_capabilities(&mut self) -> Result<()> {
        self.warnings.clear();
        self.device.ping(RequestOptions::default())?;
        self.device.discover_features()?;
        self.probe = Probe::default();

        let label = self.descriptor.display_name.clone();
        let (name, information) = {
            let mut warnings = Vec::new();
            let name =
                tolerate(&label, &mut warnings, "DeviceName", device_info::read_name(&self.device))?;
            let information = tolerate(
                &label,
                &mut warnings,
                "DeviceInformation",
                device_info::read_information(&self.device),
            )?;
            self.warnings.append(&mut warnings);
            (name, information)
        };
        self.information = information;

        let wireless = self.candidate.device_index != WIRED_DEVICE_INDEX;
        self.model = match &self.information {
            Some(info) => model::by_model_ids(&info.model_ids),
            None => model::by_wired_pid(self.candidate.product_id),
        };

        let kind = self.model.map(|m| m.kind).unwrap_or_else(|| guess_kind(&self.device));
        let wireless_pid = if wireless {
            self.information.as_ref().and_then(|info| {
                info.model_ids
                    .iter()
                    .copied()
                    .find(|id| self.model.is_some_and(|m| m.wireless_product_ids.contains(id)))
            })
        } else {
            None
        };

        self.descriptor = DeviceDescriptor {
            display_name: self
                .model
                .map(|m| m.name.to_owned())
                .or_else(|| name.clone())
                .unwrap_or_else(|| self.descriptor.display_name.clone()),
            model_name: name,
            kind,
            connection_type: if wireless {
                ConnectionType::LightspeedReceiver
            } else {
                ConnectionType::Wired
            },
            vendor_id: crate::hid::LOGITECH_VID,
            product_id: self.candidate.product_id,
            wireless_product_id: wireless_pid.filter(|id| *id != 0),
            device_index: Some(self.candidate.device_index),
            // Read and shown. Nothing in this application writes firmware.
            firmware_version: self
                .information
                .as_ref()
                .and_then(|i| i.main_firmware().map(|f| f.to_string())),
            is_known_model: self.model.is_some(),
            ..self.descriptor.clone()
        };

        self.probe_features()?;
        self.capabilities = self.build_capabilities(kind);
        Ok(())
    }

    fn probe_features(&mut self) -> Result<()> {
        let label = self.descriptor.display_name.clone();
        let mut warnings = Vec::new();
        let mut probe = Probe::default();
        let mut per_key = None;

        // Scoped so the immutable borrow of `self.device` ends before the results are stored.
        {
            let device = &self.device;
            let allow_writes = self.options.allow_onboard_writes;

            probe.dpi = tolerate(
                &label,
                &mut warnings,
                "AdjustableDpi",
                DpiFeature::create(device),
            )?
            .map(|f| f.capability);
            probe.report_rate = tolerate(
                &label,
                &mut warnings,
                "ReportRate",
                ReportRateFeature::create(device),
            )?
            .map(|f| f.capability);
            probe.buttons = tolerate(
                &label,
                &mut warnings,
                "ReprogControlsV4",
                ReprogControlsFeature::create(device),
            )?
            .map(|f| f.buttons);
            probe.onboard = tolerate(
                &label,
                &mut warnings,
                "OnboardProfiles",
                OnboardProfilesFeature::create(device, allow_writes),
            )?
            .map(|f| f.capability);
            probe.rgb = tolerate(
                &label,
                &mut warnings,
                "RgbEffects",
                RgbEffectsFeature::create(device),
            )?
            .map(|f| (f.clusters, f.zones));
            probe.gkeys =
                tolerate(&label, &mut warnings, "GKey", GKeyFeature::create(device))?
                    .map(|f| f.count);
            probe.mode_keys =
                tolerate(&label, &mut warnings, "MKey", ModeKeyFeature::create(device))?
                    .map(|f| (f.count, f.has_macro_record_key));
            probe.brightness = tolerate(
                &label,
                &mut warnings,
                "Brightness",
                BrightnessFeature::create(device),
            )?
            .map(|f| (f.maximum, f.steps));
            probe.disable_keys = tolerate(
                &label,
                &mut warnings,
                "DisableKeysByUsage",
                DisableKeysByUsageFeature::create(device),
            )?
            .map(|f| f.capability_byte);
            probe.spy_buttons = tolerate(
                &label,
                &mut warnings,
                "MouseButtonSpy",
                MouseButtonSpyFeature::create(device),
            )?
            .map(|f| f.button_count);
            probe.wheel = tolerate(
                &label,
                &mut warnings,
                "HiResWheel",
                HiResWheelFeature::create(device),
            )?
            .map(|f| f.info);

            // Battery sends nothing: which of 0x1000 / 0x1001 / 0x1004 the device implements is
            // the whole answer, and that is already in the feature table.
            probe.battery = BatteryFeature::create(device).map(|f| f.source);

            // Per-key lighting is keyed off the layout the caller supplied rather than off the
            // model name, so a device whose layout we do not have gets zone lighting instead of a
            // frame writer addressing key ids it cannot know.
            if !self.options.per_key_led_ids.is_empty() {
                let mut ids: Vec<u8> =
                    self.options.per_key_led_ids.iter().copied().filter(|id| *id != 0).collect();
                ids.sort_unstable();
                ids.dedup();
                per_key = PerKeyLightingFeature::create(device, ids);
            }
        }

        self.probe = probe;
        self.per_key = per_key;
        self.warnings.append(&mut warnings);
        Ok(())
    }

    fn build_capabilities(&self, kind: PeripheralKind) -> DeviceCapabilities {
        let is_mouse = kind == PeripheralKind::Mouse;
        DeviceCapabilities {
            dpi: self.probe.dpi.clone().or_else(|| {
                // A mouse with no 0x2201 is worth saying so about; a keyboard without one is not.
                is_mouse.then(|| DpiCapability {
                    meta: not_supported(feature::ADJUSTABLE_DPI),
                    ..DpiCapability::default()
                })
            }),
            report_rate: self.probe.report_rate.clone(),
            buttons: match &self.probe.buttons {
                Some(buttons) => Some(ButtonsCapability {
                    meta: CapabilityMeta::supported(
                        feature::REPROG_CONTROLS_V4,
                        PersistenceKind::Volatile,
                    ),
                    buttons: buttons.clone(),
                }),
                // No 0x1B04 on a mouse is the G903's case, and it is not a failure: the buttons
                // are still remappable, just not by diverting them. Say exactly how.
                None if is_mouse => Some(ButtonsCapability {
                    meta: CapabilityMeta::unavailable(
                        feature::MOUSE_BUTTON_SPY,
                        match self.probe.spy_buttons {
                            Some(count) => format!(
                                "No 0x1B04 divert support; {count} physical buttons are remappable \
                                 only through onboard profile memory (Save to Device) or software \
                                 interception of standard mouse input"
                            ),
                            None => "Device exposes no reprogrammable-controls feature".to_owned(),
                        },
                    ),
                    buttons: Vec::new(),
                }),
                None => None,
            },
            onboard_profiles: self.probe.onboard.clone(),
            battery: self.probe.battery.map(|source| BatteryCapability {
                meta: CapabilityMeta {
                    persistence: PersistenceKind::None,
                    ..CapabilityMeta::supported(
                        match source {
                            BatterySource::Unified => feature::UNIFIED_BATTERY,
                            BatterySource::LevelStatus => feature::BATTERY_LEVEL_STATUS,
                            _ => feature::BATTERY_VOLTAGE,
                        },
                        PersistenceKind::None,
                    )
                },
                source,
            }),
            lighting: match (&self.per_key, &self.probe.rgb) {
                (Some(_), rgb) => Some(LightingCapability {
                    meta: CapabilityMeta::supported(
                        feature::PER_KEY_LIGHTING_V2,
                        PersistenceKind::Volatile,
                    ),
                    kind: LightingKind::PerKey,
                    zones: rgb.as_ref().map(|(_, z)| z.clone()).unwrap_or_default(),
                }),
                (None, Some((_, zones))) => Some(LightingCapability {
                    meta: CapabilityMeta::supported(
                        feature::RGB_EFFECTS,
                        PersistenceKind::Volatile,
                    ),
                    kind: LightingKind::Zones,
                    zones: zones.clone(),
                }),
                (None, None) => None,
            },
            gkeys: self.probe.gkeys.map(|count| GKeysCapability {
                meta: CapabilityMeta::supported(feature::GKEY, PersistenceKind::Software),
                count,
            }),
            mode_keys: self.probe.mode_keys.map(|(count, has_mr)| ModeKeysCapability {
                meta: CapabilityMeta::supported(feature::MKEY, PersistenceKind::Software),
                count,
                has_macro_record_key: has_mr,
            }),
            game_mode: (self.probe.disable_keys.is_some() || self.probe.gkeys.is_some()).then(
                || GameModeCapability {
                    meta: CapabilityMeta {
                        reason: Some(
                            "Key exclusion runs in software; 0x4522 write semantics undocumented \
                             and the physical game-mode key state is not reported"
                                .to_owned(),
                        ),
                        ..CapabilityMeta::supported(
                            feature::DISABLE_KEYS_BY_USAGE,
                            PersistenceKind::Software,
                        )
                    },
                },
            ),
            raw_features: self.device.features().iter().map(|f| f.to_string()).collect(),
            ..DeviceCapabilities::new()
        }
    }

    // --- Feature accessors. Each rebuilds a wrapper from the probe; none sends a request. ---

    pub fn dpi(&self) -> Option<DpiFeature<'_>> {
        Some(DpiFeature { device: &self.device, capability: self.probe.dpi.clone()? })
    }

    pub fn report_rate(&self) -> Option<ReportRateFeature<'_>> {
        Some(ReportRateFeature {
            device: &self.device,
            capability: self.probe.report_rate.clone()?,
        })
    }

    pub fn battery(&self) -> Option<BatteryFeature<'_>> {
        Some(BatteryFeature { device: &self.device, source: self.probe.battery? })
    }

    pub fn controls(&self) -> Option<ReprogControlsFeature<'_>> {
        Some(ReprogControlsFeature { device: &self.device, buttons: self.probe.buttons.clone()? })
    }

    pub fn onboard(&self) -> Option<OnboardProfilesFeature<'_>> {
        Some(OnboardProfilesFeature {
            device: &self.device,
            capability: self.probe.onboard.clone()?,
        })
    }

    pub fn rgb(&self) -> Option<RgbEffectsFeature<'_>> {
        let (clusters, zones) = self.probe.rgb.clone()?;
        Some(RgbEffectsFeature { device: &self.device, clusters, zones })
    }

    pub fn gkeys(&self) -> Option<GKeyFeature<'_>> {
        Some(GKeyFeature { device: &self.device, count: self.probe.gkeys? })
    }

    pub fn mode_keys(&self) -> Option<ModeKeyFeature<'_>> {
        let (count, has_macro_record_key) = self.probe.mode_keys?;
        Some(ModeKeyFeature { device: &self.device, count, has_macro_record_key })
    }

    pub fn brightness(&self) -> Option<BrightnessFeature<'_>> {
        let (maximum, steps) = self.probe.brightness?;
        Some(BrightnessFeature { device: &self.device, maximum, steps })
    }

    pub fn disable_keys(&self) -> Option<DisableKeysByUsageFeature<'_>> {
        Some(DisableKeysByUsageFeature {
            device: &self.device,
            capability_byte: self.probe.disable_keys?,
        })
    }

    pub fn button_spy(&self) -> Option<MouseButtonSpyFeature<'_>> {
        Some(MouseButtonSpyFeature { device: &self.device, button_count: self.probe.spy_buttons? })
    }

    pub fn wheel(&self) -> Option<HiResWheelFeature<'_>> {
        Some(HiResWheelFeature { device: &self.device, info: self.probe.wheel? })
    }

    pub fn per_key_lighting(&self) -> Option<&PerKeyLightingFeature> {
        self.per_key.as_ref()
    }

    /// Writes a per-key frame. Split out rather than handed to the caller as a feature, because
    /// the frame writer needs `&mut` for its own cache and `&self.device` at the same time.
    pub fn write_per_key_frame(
        &mut self,
        colors: &std::collections::HashMap<u8, crate::color::LedColor>,
    ) -> Result<()> {
        let Some(per_key) = self.per_key.as_mut() else {
            return Err(HidppError::Unsupported(
                "device has no per-key lighting".into(),
            ));
        };
        per_key.write_frame(&self.device, colors)
    }

    /// Takes software control of per-key lighting.
    pub fn acquire_per_key_lighting(&mut self) -> Result<()> {
        let effects = self.probe.rgb.clone().map(|(clusters, zones)| RgbEffectsFeature {
            device: &self.device,
            clusters,
            zones,
        });
        let Some(per_key) = self.per_key.as_mut() else {
            return Err(HidppError::Unsupported("device has no per-key lighting".into()));
        };
        per_key.acquire(effects.as_ref())
    }

    /// Returns per-key lighting to the onboard effect. Called on shutdown, so the keyboard is
    /// never left dark when this application exits.
    pub fn release_per_key_lighting(&mut self) -> Result<()> {
        let effects = self.probe.rgb.clone().map(|(clusters, zones)| RgbEffectsFeature {
            device: &self.device,
            clusters,
            zones,
        });
        let Some(per_key) = self.per_key.as_mut() else {
            return Ok(());
        };
        per_key.release(effects.as_ref())
    }

    pub fn read_health(&self) -> DeviceHealth {
        let mut health = DeviceHealth {
            state: if self.is_connected() {
                DeviceConnectionState::Connected
            } else {
                DeviceConnectionState::Disconnected
            },
            ..DeviceHealth::default()
        };
        if let Some(battery) = self.battery() {
            match battery.read() {
                Ok(status) => health.battery = Some(status),
                Err(e) => health.last_error = Some(format!("Battery read failed: {e}")),
            }
        }
        health
    }

    /// Decodes one report from the channel's notification stream into the events it represents.
    ///
    /// Returns an empty vector for a report that belongs to another device index - which, on a
    /// receiver, is another paired device's traffic - or to a feature this device does not have.
    /// Masks are turned into per-key transitions here, which is why this needs `&mut self`.
    pub fn handle_report(&mut self, report: &HidppReport) -> Vec<DeviceEvent> {
        match self.device.classify(report) {
            Some(Notification::Feature { feature_id, report }) => {
                self.decode_feature(feature_id, &report)
            }
            Some(Notification::Legacy(report)) => self.decode_legacy(&report),
            None => Vec::new(),
        }
    }

    fn decode_feature(&mut self, feature_id: u16, report: &HidppReport) -> Vec<DeviceEvent> {
        if report.function() != 0 {
            return Vec::new();
        }
        let p = report.parameters();
        match feature_id {
            feature::GKEY => {
                let mask = keys::parse_pressed_gkey_mask(p);
                let changed = mask ^ self.gkey_mask;
                self.gkey_mask = mask;
                (0..32)
                    .filter(|i| changed & (1u32 << i) != 0)
                    .map(|i| DeviceEvent::GKey {
                        gkey: i + 1,
                        pressed: mask & (1u32 << i) != 0,
                    })
                    .collect()
            }
            feature::MKEY => match keys::parse_pressed_mode(p) {
                0 => Vec::new(),
                mode => vec![DeviceEvent::ModeKey { mode }],
            },
            feature::MR_KEY => {
                vec![DeviceEvent::MacroRecordKey { pressed: p.first().copied().unwrap_or(0) != 0 }]
            }
            feature::MOUSE_BUTTON_SPY => {
                let mask = mouse::parse_button_mask(p);
                let changed = mask ^ self.spy_mask;
                self.spy_mask = mask;
                (0..16)
                    .filter(|i| changed & (1u16 << i) != 0)
                    .map(|i| DeviceEvent::SpyButton {
                        button: i + 1,
                        pressed: mask & (1u16 << i) != 0,
                    })
                    .collect()
            }
            feature::REPROG_CONTROLS_V4 => {
                let pressed = controls::parse_diverted_buttons(p);
                let mut events: Vec<DeviceEvent> = pressed
                    .iter()
                    .filter(|cid| !self.diverted_pressed.contains(cid))
                    .map(|cid| DeviceEvent::Button { control_id: *cid, pressed: true })
                    .collect();
                events.extend(
                    self.diverted_pressed
                        .iter()
                        .filter(|cid| !pressed.contains(cid))
                        .map(|cid| DeviceEvent::Button { control_id: *cid, pressed: false }),
                );
                self.diverted_pressed = pressed;
                events
            }
            feature::BATTERY_VOLTAGE => {
                vec![DeviceEvent::Battery(battery::parse_voltage(p))]
            }
            feature::BATTERY_LEVEL_STATUS => {
                vec![DeviceEvent::Battery(battery::parse_level_status(p))]
            }
            feature::UNIFIED_BATTERY => {
                vec![DeviceEvent::Battery(battery::parse_unified(p))]
            }
            // The device came back from a power cycle or sleep: every volatile setting it held is
            // gone, so the owner must re-apply rather than assume.
            feature::WIRELESS_DEVICE_STATUS => {
                self.link_lost = false;
                vec![DeviceEvent::Link { established: true }]
            }
            _ => Vec::new(),
        }
    }

    /// Receiver notification 0x41 (device connection): bit 6 of the first parameter set means the
    /// link is *not* established.
    fn decode_legacy(&mut self, report: &HidppReport) -> Vec<DeviceEvent> {
        if report.feature_index() != 0x41 {
            return Vec::new();
        }
        let established = report.param(0) & 0x40 == 0;
        self.link_lost = !established;
        if established {
            // Volatile state on the far side is gone; the masks we were tracking are stale.
            self.gkey_mask = 0;
            self.spy_mask = 0;
            self.diverted_pressed.clear();
        }
        vec![DeviceEvent::Link { established }]
    }
}

/// Records a probe failure and carries on with that capability absent.
///
/// A disconnect is not a probe failure and propagates: nothing else we ask is going to work
/// either, and continuing would fill the warning list with the same cause twelve times over.
fn tolerate<T>(
    label: &str,
    warnings: &mut Vec<String>,
    what: &str,
    outcome: Result<Option<T>>,
) -> Result<Option<T>> {
    match outcome {
        Ok(value) => Ok(value),
        Err(e) if e.is_disconnected() => Err(e),
        Err(e) => {
            let line = format!("{what} query failed; feature marked unavailable: {e}");
            log::warn!("{label}: {line}");
            warnings.push(line);
            Ok(None)
        }
    }
}

fn not_supported(feature_id: u16) -> CapabilityMeta {
    CapabilityMeta {
        status: CapabilityStatus::NotSupported,
        reason: Some(format!("Feature 0x{feature_id:04X} not reported by device")),
        persistence: PersistenceKind::None,
        feature_id: Some(feature_id),
    }
}

/// Kind from the feature table, for a device that is not in the verified model list.
///
/// Deliberately coarse: it decides which page the device opens on, nothing that gets written.
fn guess_kind(device: &HidppDevice) -> PeripheralKind {
    if device.has_feature(feature::ADJUSTABLE_DPI) {
        return PeripheralKind::Mouse;
    }
    if device.has_feature(feature::PER_KEY_LIGHTING_V2) || device.has_feature(feature::GKEY) {
        return PeripheralKind::Keyboard;
    }
    PeripheralKind::Other
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::{long, session_over};
    use crate::identity::DeviceEvent;

    /// A device-originated report: software id 0, zero-padded to a long report the way firmware
    /// sends it.
    fn notification(hex: &str) -> HidppReport {
        HidppReport::try_parse(&long(hex)).expect("a valid long report")
    }

    /// Every request below is `11 <device index> <feature index> <function<<4 | 0x0A>> <params>`,
    /// which is what the channel builds: software id 0x0A, long reports because the replay backend
    /// is given no short collection.
    ///
    /// A G815-shaped device. Feature table: 1 FeatureSet, 2 DeviceInformation, 3 G-keys,
    /// 4 M-keys, 5 per-key lighting. No DPI, no battery.
    fn g815_frames() -> Vec<(&'static str, &'static str)> {
        vec![
            // Root.getProtocolVersion, echoing the 0x5A ping byte back.
            ("11FF001A00005A", "11FF001A04055A"),
            // Root.getFeature(0x0001) -> index 1, flags 0, version 1.
            ("11FF000A0001", "11FF000A010001"),
            // FeatureSet.getCount -> 5, then one getFeatureId per index.
            ("11FF010A", "11FF010A05"),
            ("11FF011A01", "11FF011A00010001"),
            ("11FF011A02", "11FF011A00030001"),
            ("11FF011A03", "11FF011A80100002"),
            ("11FF011A04", "11FF011A80200001"),
            ("11FF011A05", "11FF011A80810001"),
            // 0x0005 DeviceName is absent, so the name read answers None with no traffic at all.
            // 0x0003 getDeviceInfo: 2 entities, unit id, transport 0x0002, model id C33F.
            ("11FF020A", "11FF020A02112233440002C33F00000000"),
            // Firmware entities: main application "U1" 03.02.0018, then the bootloader.
            ("11FF021A00", "11FF021A0055310003020018"),
            ("11FF021A01", "11FF021A01424C3101020000"),
            // 0x8010 getCount -> 5 G-keys.
            ("11FF030A", "11FF030A05"),
            // 0x8020 getCount -> 3 banks.
            ("11FF040A", "11FF040A03"),
        ]
    }

    #[test]
    fn a_keyboard_session_describes_itself_from_the_feature_table() {
        let options = SessionOptions {
            // A zero and a duplicate, both of which must be dropped: zero is the frame
            // terminator, and a repeated id would be sent twice.
            per_key_led_ids: vec![0x01, 0x02, 0x00, 0x02],
            ..SessionOptions::default()
        };
        let (session, backend) =
            session_over(0xC33F, WIRED_DEVICE_INDEX, &g815_frames(), options);
        let session = session.expect("the session should open");

        assert_eq!(session.descriptor().kind, PeripheralKind::Keyboard);
        assert_eq!(session.descriptor().connection_type, ConnectionType::Wired);
        assert!(session.descriptor().is_known_model, "C33F is in the verified table");
        assert_eq!(session.descriptor().display_name, "G815 RGB Mechanical Gaming Keyboard");
        assert_eq!(session.descriptor().product_id, 0xC33F);
        assert_eq!(session.descriptor().vendor_id, 0x046D);
        assert_eq!(session.descriptor().wireless_product_id, None, "a wired device has none");
        assert_eq!(session.descriptor().device_index, Some(WIRED_DEVICE_INDEX));

        // Firmware is read and shown. Nothing here writes it.
        assert_eq!(session.descriptor().firmware_version.as_deref(), Some("U1 03.02.0018"));

        let caps = session.capabilities();
        assert!(caps.dpi.is_none(), "a keyboard with no 0x2201 claims nothing about DPI");
        assert_eq!(caps.gkeys.as_ref().unwrap().count, 5);
        assert_eq!(caps.mode_keys.as_ref().unwrap().count, 3);
        assert!(
            !caps.mode_keys.as_ref().unwrap().has_macro_record_key,
            "0x8030 is not in this table"
        );
        assert_eq!(
            caps.lighting.as_ref().unwrap().kind,
            LightingKind::PerKey,
            "0x8081 plus a supplied layout means per-key"
        );
        assert!(caps.game_mode.is_some(), "G-keys alone enable the software game mode");
        assert!(caps.battery.is_none(), "a wired keyboard reports no battery feature");
        assert!(
            session.warnings().is_empty(),
            "nothing should have failed: {:?}",
            session.warnings()
        );
        assert!(
            backend.unanswered_requests().is_empty(),
            "no request beyond the transcript: {:?}",
            backend.unanswered_requests()
        );
        assert_eq!(session.per_key_lighting().unwrap().key_ids, vec![0x01, 0x02]);
    }

    #[test]
    fn a_mouse_with_no_divert_feature_says_why_rather_than_going_quiet() {
        // The G903's case: 0x2201 and 0x8110 present, 0x1B04 absent. Device index 1 is a receiver
        // slot, so every request carries 01 rather than FF.
        let frames = vec![
            ("1101001A00005A", "1101001A04055A"),
            ("1101000A0001", "1101000A010001"),
            ("1101010A", "1101010A05"),
            ("1101011A01", "1101011A00010001"),
            ("1101011A02", "1101011A00030001"),
            ("1101011A03", "1101011A22010002"),
            ("1101011A04", "1101011A81100001"),
            ("1101011A05", "1101011A10040001"),
            // 0x0003: transport 0x0004 (LIGHTSPEED), model ids 4087 and C091.
            ("1101020A", "1101020A011122334400044087C0910000"),
            ("1101021A00", "1101021A0055310003020018"),
            // 0x2201 getSensorCount -> 1, then getSensorDpiList: 100, step marker 50, 25600.
            ("1101030A", "1101030A01"),
            ("1101031A00", "1101031A000064E0326400000000"),
            // 0x8110 getButtonCount -> 11.
            ("1101040A", "1101040A0B"),
            // 0x1004 is present, which is all the battery probe needs: it sends nothing.
        ];
        let (session, backend) = session_over(0xC091, 0x01, &frames, SessionOptions::default());
        let session = session.expect("the session should open");

        assert_eq!(session.descriptor().kind, PeripheralKind::Mouse);
        assert_eq!(
            session.descriptor().connection_type,
            ConnectionType::LightspeedReceiver,
            "device index 1 is a receiver slot"
        );
        assert_eq!(
            session.descriptor().wireless_product_id,
            Some(0x4087),
            "the wireless pid is the model id that appears in the wireless column"
        );
        assert_eq!(session.descriptor().display_name, "G903 LIGHTSPEED HERO");

        let buttons = session.capabilities().buttons.as_ref().unwrap();
        assert_eq!(buttons.meta.status, CapabilityStatus::Unavailable);
        assert!(buttons.buttons.is_empty());
        let reason = buttons.meta.reason.as_deref().unwrap();
        assert!(reason.contains("11 physical buttons"), "got: {reason}");
        assert!(
            reason.contains("Save to Device"),
            "the reason must tell the user what still works: {reason}"
        );

        // The list carried a step marker, so DPI is a range rather than a set of stages.
        let dpi = session.capabilities().dpi.as_ref().unwrap();
        assert_eq!((dpi.minimum, dpi.maximum, dpi.step), (100, 25600, 50));
        assert_eq!(session.capabilities().battery.as_ref().unwrap().source, BatterySource::Unified);
        assert!(session.capabilities().lighting.is_none(), "no 0x8071 in this table");
        assert!(session.warnings().is_empty(), "{:?}", session.warnings());
        assert!(
            backend.unanswered_requests().is_empty(),
            "{:?}",
            backend.unanswered_requests()
        );
    }

    #[test]
    fn a_mouse_without_dpi_is_marked_not_supported_rather_than_absent() {
        // No 0x2201 at all. The kind comes from the model table (C091 is a mouse), so the UI must
        // still be told that DPI is missing rather than left to infer it from an absent field.
        let frames = vec![
            ("11FF001A00005A", "11FF001A04055A"),
            ("11FF000A0001", "11FF000A010001"),
            ("11FF010A", "11FF010A03"),
            ("11FF011A01", "11FF011A00010001"),
            ("11FF011A02", "11FF011A00030001"),
            ("11FF011A03", "11FF011A1B040004"),
            ("11FF020A", "11FF020A01112233440008C09100000000"),
            ("11FF021A00", "11FF021A0055310003020018"),
            // 0x1B04 getControlCount -> 1, then that control: cid 0x0050, flags 0x31
            // (mouse button, reprogrammable, divertable).
            ("11FF030A", "11FF030A01"),
            ("11FF031A00", "11FF031A0050005031"),
        ];
        let (session, backend) =
            session_over(0xC091, WIRED_DEVICE_INDEX, &frames, SessionOptions::default());
        let session = session.expect("the session should open");

        assert_eq!(session.descriptor().kind, PeripheralKind::Mouse);
        let dpi = session.capabilities().dpi.as_ref().unwrap();
        assert_eq!(dpi.meta.status, CapabilityStatus::NotSupported);
        assert!(
            dpi.meta.reason.as_deref().unwrap().contains("0x2201"),
            "the reason should name the feature the device lacks"
        );
        assert!(!dpi.meta.is_usable(), "a NotSupported capability must never be written");

        let buttons = session.capabilities().buttons.as_ref().unwrap();
        assert_eq!(buttons.meta.status, CapabilityStatus::Supported);
        assert_eq!(buttons.buttons.len(), 1);
        assert_eq!(buttons.buttons[0].control_id, 0x0050);
        assert!(buttons.buttons[0].divertable);
        assert!(backend.unanswered_requests().is_empty(), "{:?}", backend.unanswered_requests());
    }

    #[test]
    fn a_feature_that_fails_to_probe_leaves_a_warning_not_a_dead_device() {
        // 0x8010 is in the table but answers getCount with a HID++ 2.0 error. The session must
        // still come up, with G-keys absent and one warning naming the feature.
        let frames = vec![
            ("11FF001A00005A", "11FF001A04055A"),
            ("11FF000A0001", "11FF000A010001"),
            ("11FF010A", "11FF010A03"),
            ("11FF011A01", "11FF011A00010001"),
            ("11FF011A02", "11FF011A80100002"),
            ("11FF011A03", "11FF011A80200001"),
            // Error report: sub id FF, then the feature index and address it refers to, then
            // ERR_INVALID_FUNCTION_ID (0x07).
            ("11FF020A", "11FFFF020A07"),
            ("11FF030A", "11FF030A03"),
        ];
        let (session, _) =
            session_over(0xC33F, WIRED_DEVICE_INDEX, &frames, SessionOptions::default());
        let session = session.expect("one bad feature must not stop the session opening");

        assert!(session.capabilities().gkeys.is_none());
        assert_eq!(session.capabilities().mode_keys.as_ref().unwrap().count, 3);
        assert_eq!(session.warnings().len(), 1, "{:?}", session.warnings());
        assert!(session.warnings()[0].starts_with("GKey query failed"), "{:?}", session.warnings());
        // The capability list still reports game mode, because M-keys came up fine. A failed
        // probe must subtract exactly one capability, not cascade.
        assert!(session.capabilities().mode_keys.is_some());
    }

    #[test]
    fn the_raw_feature_list_is_recorded_for_diagnostics() {
        let (session, _) =
            session_over(0xC33F, WIRED_DEVICE_INDEX, &g815_frames(), SessionOptions::default());
        let session = session.unwrap();
        let raw = &session.capabilities().raw_features;
        assert!(raw.iter().any(|f| f.contains("0x8081")), "{raw:?}");
        assert!(
            raw.iter().any(|f| f.contains("0x0001")),
            "FeatureSet itself belongs here: {raw:?}"
        );
    }

    #[test]
    fn a_gkey_mask_becomes_one_event_per_changed_key() {
        let (session, _) =
            session_over(0xC33F, WIRED_DEVICE_INDEX, &g815_frames(), SessionOptions::default());
        let mut session = session.unwrap();

        // 0x8010 sits at index 3 in this fixture. Software id 0 marks a notification.
        let g1_g3 = notification("11FF030005");
        assert_eq!(
            session.handle_report(&g1_g3),
            vec![
                DeviceEvent::GKey { gkey: 1, pressed: true },
                DeviceEvent::GKey { gkey: 3, pressed: true },
            ]
        );

        // G1 released, G3 still held: only G1 is reported, because only G1 changed.
        let g3_only = notification("11FF030004");
        assert_eq!(
            session.handle_report(&g3_only),
            vec![DeviceEvent::GKey { gkey: 1, pressed: false }]
        );
        // The same mask again is not an event.
        assert!(session.handle_report(&g3_only).is_empty());
    }

    #[test]
    fn another_paired_devices_traffic_is_ignored() {
        let (session, _) =
            session_over(0xC33F, WIRED_DEVICE_INDEX, &g815_frames(), SessionOptions::default());
        let mut session = session.unwrap();

        // Device index 2 on a shared receiver channel: another mouse's G-key event.
        assert!(session.handle_report(&notification("1102030005")).is_empty());

        // A feature index this device does not have is ignored too, rather than decoded as
        // whatever feature happens to sit at that index on some other device.
        assert!(session.handle_report(&notification("11FF3F0005")).is_empty());
    }

    #[test]
    fn a_dropped_link_marks_the_session_disconnected_and_clears_stale_masks() {
        let (session, _) =
            session_over(0xC33F, WIRED_DEVICE_INDEX, &g815_frames(), SessionOptions::default());
        let mut session = session.unwrap();

        // Hold G1 so there is edge state to go stale.
        let g1 = notification("11FF030001");
        assert_eq!(session.handle_report(&g1).len(), 1);

        // Receiver notification 0x41 with bit 6 set: link down.
        assert_eq!(
            session.handle_report(&notification("11FF410040")),
            vec![DeviceEvent::Link { established: false }]
        );
        assert!(!session.is_connected(), "a lost link is a disconnected session");

        // Link back up. The masks are cleared, so the first report after a reconnect is treated
        // as a fresh press rather than diffed against what the device showed before it slept.
        assert_eq!(
            session.handle_report(&notification("11FF410000")),
            vec![DeviceEvent::Link { established: true }]
        );
        assert!(session.is_connected());
        assert_eq!(
            session.handle_report(&g1),
            vec![DeviceEvent::GKey { gkey: 1, pressed: true }],
            "G1 is reported again because the stale mask was dropped"
        );
    }

    #[test]
    fn diverted_button_transitions_are_reported_in_both_directions() {
        // 0x1B04 at index 2, two controls. The event carries the set of control ids currently
        // held, so a release is "a cid that was in the last set and is not in this one".
        let frames = vec![
            ("11FF001A00005A", "11FF001A04055A"),
            ("11FF000A0001", "11FF000A010001"),
            ("11FF010A", "11FF010A02"),
            ("11FF011A01", "11FF011A00010001"),
            ("11FF011A02", "11FF011A1B040004"),
            ("11FF020A", "11FF020A02"),
            ("11FF021A00", "11FF021A0053005331"),
            ("11FF021A01", "11FF021A0056005631"),
        ];
        let (session, _) =
            session_over(0xC091, WIRED_DEVICE_INDEX, &frames, SessionOptions::default());
        let mut session = session.unwrap();
        assert_eq!(session.capabilities().buttons.as_ref().unwrap().buttons.len(), 2);

        // 0x0053 pressed.
        assert_eq!(
            session.handle_report(&notification("11FF02000053")),
            vec![DeviceEvent::Button { control_id: 0x0053, pressed: true }]
        );
        // 0x0056 pressed as well, in the same report. 0x0053 is still held, so it is not repeated.
        assert_eq!(
            session.handle_report(&notification("11FF020000530056")),
            vec![DeviceEvent::Button { control_id: 0x0056, pressed: true }]
        );
        // Both released: the empty set means two release events.
        let released = session.handle_report(&notification("11FF0200"));
        assert_eq!(released.len(), 2, "{released:?}");
        assert!(released.contains(&DeviceEvent::Button { control_id: 0x0053, pressed: false }));
        assert!(released.contains(&DeviceEvent::Button { control_id: 0x0056, pressed: false }));
        assert!(session.handle_report(&notification("11FF0200")).is_empty());
    }

    #[test]
    fn per_key_lighting_is_refused_rather_than_guessed_without_a_layout() {
        // 0x8081 is in the table, but no layout was supplied, so there are no key ids to address.
        // Writing a frame must fail loudly rather than address ids we made up.
        let (session, _) =
            session_over(0xC33F, WIRED_DEVICE_INDEX, &g815_frames(), SessionOptions::default());
        let mut session = session.unwrap();
        assert!(session.per_key_lighting().is_none());
        assert_eq!(
            session.capabilities().lighting,
            None,
            "without a layout and without 0x8071 there is nothing to claim"
        );
        assert!(matches!(
            session.write_per_key_frame(&std::collections::HashMap::new()),
            Err(HidppError::Unsupported(_))
        ));
        // Releasing is still safe to call, so shutdown does not need to know whether it acquired.
        assert!(session.release_per_key_lighting().is_ok());
    }
}
