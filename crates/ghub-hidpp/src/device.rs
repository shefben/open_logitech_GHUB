//! A HID++ 2.0 device at one device index of a channel.
//! Port of `Artemis.Peripherals/Logitech/Hidpp/HidppDevice.cs`.
//!
//! Feature indexes are discovered per session through the FeatureSet feature and are never
//! hardcoded: the same feature sits at a different index on different firmware, so a hardcoded
//! index is a write aimed at whatever feature happens to live there.

use std::collections::HashMap;
use std::sync::Arc;

use crate::channel::{HidppChannel, RequestOptions};
use crate::error::{HidppError, Result};
use crate::report::HidppReport;

/// HID++ 2.0 feature ids used by this application.
pub mod feature {
    pub const ROOT: u16 = 0x0000;
    pub const FEATURE_SET: u16 = 0x0001;
    pub const DEVICE_INFORMATION: u16 = 0x0003;
    pub const DEVICE_NAME: u16 = 0x0005;
    pub const RESET: u16 = 0x0020;
    pub const BATTERY_LEVEL_STATUS: u16 = 0x1000;
    pub const BATTERY_VOLTAGE: u16 = 0x1001;
    pub const UNIFIED_BATTERY: u16 = 0x1004;
    pub const LED_SOFTWARE_CONTROL: u16 = 0x1300;
    pub const REPROG_CONTROLS: u16 = 0x1B00;
    pub const REPROG_CONTROLS_V4: u16 = 0x1B04;
    pub const WIRELESS_DEVICE_STATUS: u16 = 0x1D4B;
    pub const ADJUSTABLE_DPI: u16 = 0x2201;
    pub const HI_RES_WHEEL: u16 = 0x2121;
    pub const DISABLE_KEYS: u16 = 0x4521;
    pub const DISABLE_KEYS_BY_USAGE: u16 = 0x4522;
    pub const GKEY: u16 = 0x8010;
    pub const MKEY: u16 = 0x8020;
    pub const MR_KEY: u16 = 0x8030;
    pub const BRIGHTNESS_CONTROL: u16 = 0x8040;
    pub const ADJUSTABLE_REPORT_RATE: u16 = 0x8060;
    pub const COLOR_LED_EFFECTS: u16 = 0x8070;
    pub const RGB_EFFECTS: u16 = 0x8071;
    pub const PER_KEY_LIGHTING: u16 = 0x8080;
    pub const PER_KEY_LIGHTING_V2: u16 = 0x8081;
    pub const ONBOARD_PROFILES: u16 = 0x8100;
    pub const MOUSE_BUTTON_SPY: u16 = 0x8110;

    /// Human-readable name, for the diagnostics page and the protocol log. Covers more ids than
    /// the constants above because a device's feature table lists everything it implements, and an
    /// unnamed row in diagnostics is a row nobody can act on.
    pub fn name_of(feature: u16) -> &'static str {
        match feature {
            ROOT => "Root",
            FEATURE_SET => "FeatureSet",
            0x0002 => "FeatureInfo",
            DEVICE_INFORMATION => "DeviceInformation",
            0x0004 => "DeviceUnitId",
            DEVICE_NAME => "DeviceName",
            0x0007 => "DeviceFriendlyName",
            0x0008 => "KeepAlive",
            RESET => "ConfigChange",
            0x0021 => "UniqueRandomId",
            0x00C2 => "DfuControlSigned",
            0x00C3 => "DfuControl",
            0x00D0 => "Dfu",
            BATTERY_LEVEL_STATUS => "BatteryLevelStatus",
            BATTERY_VOLTAGE => "BatteryVoltage",
            UNIFIED_BATTERY => "UnifiedBattery",
            0x1814 => "ChangeHost",
            0x1815 => "HostsInfo",
            0x1802 => "DeviceReset",
            0x1803 => "GpioAccess",
            0x1806 => "ConfigurableDeviceProperties",
            0x1811 => "Oobstate",
            0x1830 => "PowerModes",
            0x1890 => "RfTest",
            0x18A1 => "LedTest",
            0x1E00 => "EnableHiddenFeatures",
            0x1E22 => "SpiDirectAccess",
            0x1EB0 => "TdeAccess",
            0x1F20 => "AdcMeasurement",
            LED_SOFTWARE_CONTROL => "LedSoftwareControl",
            REPROG_CONTROLS => "ReprogControls",
            REPROG_CONTROLS_V4 => "ReprogControlsV4",
            WIRELESS_DEVICE_STATUS => "WirelessDeviceStatus",
            0x2200 => "MousePointer",
            ADJUSTABLE_DPI => "AdjustableDpi",
            0x2205 => "PointerMotionScaling",
            0x40A3 => "FnInversionWithDefault",
            0x4100 => "Encryption",
            DISABLE_KEYS => "DisableKeys",
            DISABLE_KEYS_BY_USAGE => "DisableKeysByUsage",
            0x4530 => "DualPlatform",
            0x4540 => "KeyboardInternationalLayouts",
            GKEY => "GKey",
            MKEY => "MKey",
            MR_KEY => "MRKey",
            BRIGHTNESS_CONTROL => "BrightnessControl",
            ADJUSTABLE_REPORT_RATE => "AdjustableReportRate",
            COLOR_LED_EFFECTS => "ColorLedEffects",
            RGB_EFFECTS => "RgbEffects",
            PER_KEY_LIGHTING => "PerKeyLighting",
            PER_KEY_LIGHTING_V2 => "PerKeyLightingV2",
            0x8090 => "ModeStatus",
            ONBOARD_PROFILES => "OnboardProfiles",
            MOUSE_BUTTON_SPY => "MouseButtonSpy",
            0x8111 => "Latency",
            _ => "Unknown",
        }
    }

    /// Feature ids that flash firmware. We never call these: the implementation plan forbids
    /// firmware updating outright, so they are listed in order to be refused, not used.
    pub const FIRMWARE_UPDATE_FEATURES: [u16; 3] = [0x00C2, 0x00C3, 0x00D0];

    pub fn is_firmware_update(feature: u16) -> bool {
        FIRMWARE_UPDATE_FEATURES.contains(&feature)
    }
}

/// One row of a device's feature table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HidppFeatureInfo {
    pub feature_id: u16,
    pub index: u8,
    pub version: u8,
    pub flags: u8,
}

impl HidppFeatureInfo {
    pub fn is_obsolete(&self) -> bool {
        self.flags & 0x80 != 0
    }

    pub fn is_hidden(&self) -> bool {
        self.flags & 0x40 != 0
    }

    pub fn is_engineering(&self) -> bool {
        self.flags & 0x20 != 0
    }
}

impl std::fmt::Display for HidppFeatureInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "0x{:04X} {} v{} @ idx {}",
            self.feature_id,
            feature::name_of(self.feature_id),
            self.version,
            self.index
        )?;
        if self.is_hidden() {
            f.write_str(" (hidden)")?;
        }
        if self.is_engineering() {
            f.write_str(" (engineering)")?;
        }
        Ok(())
    }
}

/// A notification from this device, with its feature resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notification {
    /// HID++ 1.0 style notification (receiver link status 0x41 and friends), identified by a sub
    /// id in 0x40..0x80 rather than by a feature index.
    Legacy(HidppReport),
    Feature { feature_id: u16, report: HidppReport },
}

/// A HID++ 2.0 device at one device index of a channel.
pub struct HidppDevice {
    channel: Arc<HidppChannel>,
    device_index: u8,
    protocol_version: (u8, u8),
    features: HashMap<u16, HidppFeatureInfo>,
    by_index: HashMap<u8, u16>,
}

impl std::fmt::Debug for HidppDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "HidppDevice({} index 0x{:02X}, {} features)",
            self.channel.name(),
            self.device_index,
            self.features.len()
        )
    }
}

impl HidppDevice {
    pub fn new(channel: Arc<HidppChannel>, device_index: u8) -> Self {
        Self {
            channel,
            device_index,
            protocol_version: (0, 0),
            features: HashMap::new(),
            by_index: HashMap::new(),
        }
    }

    pub fn channel(&self) -> &Arc<HidppChannel> {
        &self.channel
    }

    pub fn device_index(&self) -> u8 {
        self.device_index
    }

    pub fn protocol_version(&self) -> (u8, u8) {
        self.protocol_version
    }

    pub fn has_feature(&self, feature_id: u16) -> bool {
        self.features.contains_key(&feature_id)
    }

    pub fn get_feature(&self, feature_id: u16) -> Option<HidppFeatureInfo> {
        self.features.get(&feature_id).copied()
    }

    /// The feature table, ordered by index so the diagnostics page reads like the device's own.
    pub fn features(&self) -> Vec<HidppFeatureInfo> {
        let mut all: Vec<_> = self.features.values().copied().collect();
        all.sort_unstable_by_key(|f| f.index);
        all
    }

    /// `Root.GetProtocolVersion`. A device index with nothing paired to it answers with a HID++
    /// 1.0 `UnknownDevice` error, so [`crate::error::ProtocolError::is_unknown_device`] on the
    /// returned error is how an empty receiver slot is told from a real failure.
    pub fn ping(&mut self, options: RequestOptions) -> Result<(u8, u8)> {
        const PING_DATA: u8 = 0x5A;
        let response =
            self.channel
                .request(self.device_index, 0x00, 0x1, &[0x00, 0x00, PING_DATA], options)?;
        if response.param(2) != PING_DATA {
            return Err(HidppError::Malformed(format!(
                "ping echo mismatch (got 0x{:02X})",
                response.param(2)
            )));
        }
        self.protocol_version = (response.param(0), response.param(1));
        Ok(self.protocol_version)
    }

    /// `Root.GetFeature` for one feature id. `Ok(None)` when the device does not implement it:
    /// index 0 is Root's own index, so any other feature answering with 0 means "absent".
    pub fn query_feature(&self, feature_id: u16) -> Result<Option<HidppFeatureInfo>> {
        let response = self.channel.request(
            self.device_index,
            0x00,
            0x0,
            &[(feature_id >> 8) as u8, feature_id as u8],
            RequestOptions::default(),
        )?;
        let index = response.param(0);
        if index == 0 && feature_id != feature::ROOT {
            return Ok(None);
        }
        // Note the byte order here differs from the FeatureSet table below: GetFeature answers
        // index, flags, version while GetFeatureId answers id, id, flags, version.
        Ok(Some(HidppFeatureInfo {
            feature_id,
            index,
            version: response.param(2),
            flags: response.param(1),
        }))
    }

    /// Enumerates the whole feature table through FeatureSet (0x0001).
    ///
    /// This is the only sanctioned way to learn what a device can do. Probing by sending unknown
    /// function ids is forbidden: an unrecognised write is still a write.
    pub fn discover_features(&mut self) -> Result<Vec<HidppFeatureInfo>> {
        self.features.clear();
        self.by_index.clear();
        self.register(HidppFeatureInfo { feature_id: feature::ROOT, index: 0, version: 0, flags: 0 });

        let feature_set = self.query_feature(feature::FEATURE_SET)?.ok_or_else(|| {
            HidppError::Malformed("device does not implement FeatureSet (0x0001)".into())
        })?;
        self.register(feature_set);

        let count_response = self.channel.request(
            self.device_index,
            feature_set.index,
            0x0,
            &[],
            RequestOptions::default(),
        )?;
        let count = count_response.param(0);
        if count > 0x7F {
            // Feature indexes above 0x7F collide with the notification range, so a count this
            // large means we are reading something that is not a feature table.
            return Err(HidppError::Malformed(format!("implausible feature count {count}")));
        }

        for i in 1..=count {
            let entry = self.channel.request(
                self.device_index,
                feature_set.index,
                0x1,
                &[i],
                RequestOptions::default(),
            )?;
            self.register(HidppFeatureInfo {
                feature_id: entry.param_u16(0),
                index: i,
                version: entry.param(3),
                flags: entry.param(2),
            });
        }

        Ok(self.features())
    }

    /// Installs a known feature table without querying. Used by replay fixtures and by the G HUB
    /// import path, where the table came from a capture.
    pub fn install_features(&mut self, features: impl IntoIterator<Item = HidppFeatureInfo>) {
        self.features.clear();
        self.by_index.clear();
        for feature in features {
            self.register(feature);
        }
    }

    fn register(&mut self, feature: HidppFeatureInfo) {
        self.features.insert(feature.feature_id, feature);
        self.by_index.insert(feature.index, feature.feature_id);
    }

    /// Calls a function of a feature by id, resolving the index from the discovered table.
    pub fn call(
        &self,
        feature_id: u16,
        function: u8,
        parameters: &[u8],
        options: RequestOptions,
    ) -> Result<HidppReport> {
        if feature::is_firmware_update(feature_id) {
            // Belt and braces. Nothing in this codebase calls a DFU feature, and this makes a
            // future caller that tries fail loudly rather than put the device in bootloader mode.
            return Err(HidppError::Unsupported(format!(
                "feature 0x{feature_id:04X} ({}) flashes firmware and is never called",
                feature::name_of(feature_id)
            )));
        }
        let feature = self.features.get(&feature_id).ok_or_else(|| {
            HidppError::Unsupported(format!(
                "feature 0x{feature_id:04X} ({}) not present on device index 0x{:02X}",
                feature::name_of(feature_id),
                self.device_index
            ))
        })?;
        self.channel
            .request(self.device_index, feature.index, function, parameters, options)
    }

    /// Resolves a notification from the channel against this device's feature table.
    ///
    /// `None` when the report belongs to another device index, or to a feature index this device
    /// does not know - which on a receiver means it is another paired device's traffic.
    pub fn classify(&self, report: &HidppReport) -> Option<Notification> {
        if report.device_index() != self.device_index {
            return None;
        }
        if (0x40..0x80).contains(&report.feature_index()) {
            return Some(Notification::Legacy(report.clone()));
        }
        match self.by_index.get(&report.feature_index()) {
            Some(&id) if id != feature::ROOT => {
                Some(Notification::Feature { feature_id: id, report: report.clone() })
            }
            _ => None,
        }
    }

    /// One line per feature, for the diagnostics page and bug reports.
    pub fn describe_features(&self) -> String {
        self.features().iter().map(|f| f.to_string()).collect::<Vec<_>>().join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hid::{HidBackend, LOGITECH_VID};
    use crate::replay::{HidTranscript, ReplayHidBackend, TranscriptFrame};
    use crate::report::constants::WIRED_DEVICE_INDEX;
    use crate::report::from_hex;

    /// Zero-pads hex out to a 20-byte long report, the way a device pads a short answer.
    fn long(hex: &str) -> Vec<u8> {
        let mut bytes = from_hex(hex).expect("bad hex");
        assert!(bytes.len() <= 20, "{hex} is longer than a long report");
        bytes.resize(20, 0);
        bytes
    }

    /// A device over a replay backend answering the given request/response pairs. The backend is
    /// given no short collection, matching the directly attached G815 observed on hardware.
    fn device_over(pairs: &[(&str, &str)]) -> (HidppDevice, ReplayHidBackend) {
        let frames = pairs
            .iter()
            .flat_map(|(out, reply)| {
                [
                    TranscriptFrame { outgoing: true, data: long(out) },
                    TranscriptFrame { outgoing: false, data: long(reply) },
                ]
            })
            .collect();
        let transcript = HidTranscript { device: "test".into(), frames };
        let backend = ReplayHidBackend::new(&transcript, 0xC33F, false);
        let connections = backend
            .enumerate(LOGITECH_VID)
            .expect("enumerate")
            .iter()
            .map(|i| backend.open(i).expect("open"))
            .collect();
        let (channel, _notifications) = HidppChannel::new(connections, "replay").expect("channel");
        (HidppDevice::new(Arc::new(channel), WIRED_DEVICE_INDEX), backend)
    }

    #[test]
    fn ping_reads_the_protocol_version_and_checks_the_echo() {
        // Root.GetProtocolVersion, function 1, ping byte 0x5A in param 2. The device answers
        // major, minor, echo.
        let (mut device, backend) =
            device_over(&[("11FF001A00005A", "11FF001A04055A")]);
        assert_eq!(device.ping(RequestOptions::default()).unwrap(), (4, 5));
        assert_eq!(device.protocol_version(), (4, 5));
        assert!(backend.unanswered_requests().is_empty());
    }

    #[test]
    fn a_wrong_ping_echo_is_malformed_not_a_version() {
        // A device answering someone else's ping would otherwise hand us a bogus version.
        let (mut device, _) = device_over(&[("11FF001A00005A", "11FF001A040599")]);
        match device.ping(RequestOptions::default()) {
            Err(HidppError::Malformed(m)) => assert!(m.contains("0x99"), "{m}"),
            other => panic!("expected a malformed-response error, got {other:?}"),
        }
        // The version must stay unset rather than be half-written from a bad answer.
        assert_eq!(device.protocol_version(), (0, 0));
    }

    #[test]
    fn an_absent_feature_answers_with_index_zero() {
        // Root.GetFeature for 0x2201 on a keyboard: index 0 means "not implemented".
        let (device, _) = device_over(&[("11FF000A2201", "11FF000A000000")]);
        assert_eq!(device.query_feature(feature::ADJUSTABLE_DPI).unwrap(), None);
    }

    #[test]
    fn query_feature_reads_flags_from_param_one_and_version_from_param_two() {
        // GetFeature answers index, flags, version - a different order from the FeatureSet table,
        // and swapping them would mark every feature engineering-only.
        let (device, _) = device_over(&[("11FF000A1B04", "11FF000A05002A")]);
        let info = device.query_feature(feature::REPROG_CONTROLS_V4).unwrap().unwrap();
        assert_eq!(info.index, 0x05);
        assert_eq!(info.flags, 0x00);
        assert_eq!(info.version, 0x2A);
        assert!(!info.is_engineering());
    }

    #[test]
    fn discovery_walks_the_feature_set_table() {
        let (mut device, backend) = device_over(&[
            // Root.GetFeature(0x0001) -> FeatureSet lives at index 1, version 2.
            ("11FF000A0001", "11FF000A010002"),
            // FeatureSet.GetCount -> 3 features past Root.
            ("11FF010A", "11FF010A03"),
            // FeatureSet.GetFeatureId(1..3): id, id, flags, version.
            ("11FF011A01", "11FF011A00010002"),
            ("11FF011A02", "11FF011A22010001"),
            ("11FF011A03", "11FF011A80810001"),
        ]);

        let features = device.discover_features().unwrap();
        // Root is registered locally at index 0 and is not in the table, so 3 rows become 4.
        assert_eq!(features.len(), 4);
        assert_eq!(features[0].feature_id, feature::ROOT);
        assert!(device.has_feature(feature::ADJUSTABLE_DPI));
        assert_eq!(device.get_feature(feature::ADJUSTABLE_DPI).unwrap().index, 2);
        assert_eq!(device.get_feature(feature::PER_KEY_LIGHTING_V2).unwrap().index, 3);
        assert!(!device.has_feature(feature::ONBOARD_PROFILES));
        assert!(backend.unanswered_requests().is_empty(), "discovery asked for nothing extra");
    }

    #[test]
    fn an_implausible_feature_count_is_rejected_before_the_walk() {
        // 0xFF features would run the index into the notification range (0x40..0x80), so this is
        // not a feature table. Rejecting it early also stops 255 pointless requests.
        let (mut device, backend) = device_over(&[
            ("11FF000A0001", "11FF000A010002"),
            ("11FF010A", "11FF010AFF"),
        ]);
        assert!(matches!(device.discover_features(), Err(HidppError::Malformed(_))));
        assert_eq!(backend.all_requests().len(), 2, "must not walk the bogus table");
    }

    #[test]
    fn a_device_without_feature_set_is_rejected() {
        let (mut device, _) = device_over(&[("11FF000A0001", "11FF000A000000")]);
        match device.discover_features() {
            Err(HidppError::Malformed(m)) => assert!(m.contains("FeatureSet"), "{m}"),
            other => panic!("expected FeatureSet to be required, got {other:?}"),
        }
    }

    #[test]
    fn calling_an_undiscovered_feature_does_not_reach_the_device() {
        // The point: no request is sent. Guessing an index would aim a write at whatever feature
        // happens to sit there on this firmware.
        let (device, backend) = device_over(&[]);
        let err = device
            .call(feature::PER_KEY_LIGHTING_V2, 0x1, &[], RequestOptions::default())
            .expect_err("feature was never discovered");
        assert!(matches!(err, HidppError::Unsupported(_)), "{err:?}");
        assert!(backend.all_requests().is_empty(), "nothing may be sent");
    }

    #[test]
    fn calling_a_firmware_update_feature_is_refused_even_when_present() {
        // Firmware flashing is out of scope by instruction. Having the feature in the table must
        // not make it callable.
        let (mut device, backend) = device_over(&[]);
        device.install_features([
            HidppFeatureInfo { feature_id: feature::ROOT, index: 0, version: 0, flags: 0 },
            HidppFeatureInfo { feature_id: 0x00D0, index: 9, version: 0, flags: 0 },
        ]);
        let err = device.call(0x00D0, 0x0, &[], RequestOptions::default()).expect_err("refused");
        assert!(matches!(err, HidppError::Unsupported(_)), "{err:?}");
        assert!(backend.all_requests().is_empty());
    }

    #[test]
    fn notifications_resolve_to_features_and_legacy_separately() {
        let (mut device, _) = device_over(&[]);
        device.install_features([
            HidppFeatureInfo { feature_id: feature::ROOT, index: 0, version: 0, flags: 0 },
            HidppFeatureInfo { feature_id: feature::GKEY, index: 0x0A, version: 1, flags: 0 },
        ]);

        // A GKey notification: feature index 0x0A, software id 0 because the device originated it.
        let gkey = HidppReport::try_parse(&long("11FF0A0001")).unwrap();
        assert_eq!(
            device.classify(&gkey),
            Some(Notification::Feature { feature_id: feature::GKEY, report: gkey.clone() })
        );

        // Receiver link status, HID++ 1.0 style: sub id 0x41 is in the legacy range and has no
        // feature index to resolve.
        let link = HidppReport::try_parse(&long("11FF410004")).unwrap();
        assert!(matches!(device.classify(&link), Some(Notification::Legacy(_))));

        // A feature index this device does not know: on a receiver that is another paired
        // device's traffic, and acting on it would attribute it to the wrong device.
        let unknown = HidppReport::try_parse(&long("11FF1F0001")).unwrap();
        assert_eq!(device.classify(&unknown), None);

        // Another device index entirely.
        let other_index = HidppReport::try_parse(&long("11020A0001")).unwrap();
        assert_eq!(device.classify(&other_index), None);

        // Root's own index is filtered: a Root notification carries nothing actionable.
        let root = HidppReport::try_parse(&long("11FF000001")).unwrap();
        assert_eq!(device.classify(&root), None);
    }

    #[test]
    fn feature_names_cover_the_ids_we_use() {
        // An id we drive must never show up as "Unknown" on the diagnostics page.
        for id in [
            feature::ADJUSTABLE_DPI,
            feature::ADJUSTABLE_REPORT_RATE,
            feature::PER_KEY_LIGHTING,
            feature::PER_KEY_LIGHTING_V2,
            feature::ONBOARD_PROFILES,
            feature::GKEY,
            feature::MKEY,
            feature::MR_KEY,
            feature::BRIGHTNESS_CONTROL,
            feature::UNIFIED_BATTERY,
            feature::REPROG_CONTROLS_V4,
            feature::DEVICE_NAME,
            feature::DEVICE_INFORMATION,
        ] {
            assert_ne!(feature::name_of(id), "Unknown", "0x{id:04X} has no name");
        }
    }

    #[test]
    fn flags_decode() {
        let hidden = HidppFeatureInfo { feature_id: 0x1E00, index: 7, version: 0, flags: 0x40 };
        assert!(hidden.is_hidden());
        assert!(!hidden.is_engineering());
        assert!(hidden.to_string().contains("(hidden)"));

        let engineering = HidppFeatureInfo { flags: 0x20, ..hidden };
        assert!(engineering.is_engineering());
        assert!(HidppFeatureInfo { flags: 0x80, ..hidden }.is_obsolete());
    }

    #[test]
    fn firmware_update_features_are_refused_by_name() {
        // The plan forbids firmware flashing. These three ids are the HID++ way to do it.
        for id in [0x00C2u16, 0x00C3, 0x00D0] {
            assert!(feature::is_firmware_update(id), "0x{id:04X} must be refused");
        }
        assert!(!feature::is_firmware_update(feature::DEVICE_INFORMATION));
    }
}
