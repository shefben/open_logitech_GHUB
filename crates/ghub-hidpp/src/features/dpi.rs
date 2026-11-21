//! 0x2201 Adjustable DPI. Writes land in sensor RAM and are lost on power cycle.

use crate::capability::{CapabilityMeta, CapabilityStatus, DpiCapability, PersistenceKind};
use crate::channel::RequestOptions;
use crate::device::{HidppDevice, feature};
use crate::error::{HidppError, Result};

/// Current and default DPI of one sensor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DpiState {
    pub current: u32,
    pub default: u32,
}

pub struct DpiFeature<'d> {
    pub(crate) device: &'d HidppDevice,
    pub capability: DpiCapability,
}

impl<'d> DpiFeature<'d> {
    /// `Ok(None)` when the device has no adjustable sensor.
    pub fn create(device: &'d HidppDevice) -> Result<Option<Self>> {
        if !device.has_feature(feature::ADJUSTABLE_DPI) {
            return Ok(None);
        }
        let count = device.call(feature::ADJUSTABLE_DPI, 0, &[], RequestOptions::default())?;
        // The list needs the whole 16-byte payload, so it is forced long even on a device whose
        // short collection would otherwise be preferred.
        let list = device.call(feature::ADJUSTABLE_DPI, 1, &[0], RequestOptions::long())?;
        let sensors = (count.param(0) as usize).max(1);
        // Skip the echoed sensor index.
        let capability = parse_dpi_list(&list.parameters()[1..], sensors);
        Ok(Some(Self { device, capability }))
    }

    pub fn get(&self) -> Result<DpiState> {
        let r = self.device.call(feature::ADJUSTABLE_DPI, 2, &[0], RequestOptions::default())?;
        let current = r.param_u16(1) as u32;
        let default = r.param_u16(3) as u32;
        // Firmware that does not track a default answers zero; reporting 0 DPI as the default
        // would make the "reset" button set the mouse to nothing.
        Ok(DpiState { current, default: if default == 0 { current } else { default } })
    }

    /// Sets DPI and returns what the sensor reports afterwards, which is not always what was
    /// asked for.
    pub fn set(&self, dpi: u32) -> Result<u32> {
        if !self.capability.is_valid(dpi) {
            return Err(HidppError::Unsupported(format!(
                "{dpi} DPI is outside the device-reported range {}-{} (step {})",
                self.capability.minimum, self.capability.maximum, self.capability.step
            )));
        }
        let response = self.device.call(
            feature::ADJUSTABLE_DPI,
            3,
            &[0, (dpi >> 8) as u8, dpi as u8],
            RequestOptions::default(),
        )?;
        let echoed = response.param_u16(1) as u32;
        if echoed != 0 && echoed != dpi {
            return Err(HidppError::Malformed(format!(
                "device echoed {echoed} DPI after setting {dpi}"
            )));
        }
        Ok(self.get()?.current)
    }
}

/// Parses the `getSensorDpiList` payload, after the sensor index byte.
///
/// Big-endian u16 values terminated by zero. A value above 0xE000 is a step marker meaning "the
/// range from the previous value to the next, in (value - 0xE000) increments", which is how the
/// G903 HERO reports 100-25600 in steps of 50 in six bytes.
pub fn parse_dpi_list(data: &[u8], sensor_count: usize) -> DpiCapability {
    let mut values: Vec<u32> = Vec::new();
    let mut step = 0u32;
    let mut i = 0;
    while i + 1 < data.len() {
        let value = ((data[i] as u32) << 8) | data[i + 1] as u32;
        if value == 0 {
            break;
        }
        if value > 0xE000 {
            step = value - 0xE000;
        } else {
            values.push(value);
        }
        i += 2;
    }

    if values.is_empty() {
        return DpiCapability {
            meta: CapabilityMeta {
                status: CapabilityStatus::Unavailable,
                reason: Some("Sensor reported an empty DPI list".into()),
                persistence: PersistenceKind::None,
                feature_id: Some(feature::ADJUSTABLE_DPI),
            },
            ..Default::default()
        };
    }

    let minimum = values.iter().copied().min().unwrap_or(0);
    let maximum = values.iter().copied().max().unwrap_or(0);
    let meta = CapabilityMeta::supported(feature::ADJUSTABLE_DPI, PersistenceKind::Volatile);
    if step > 0 {
        DpiCapability { meta, sensor_count, minimum, maximum, step, ..Default::default() }
    } else {
        DpiCapability {
            meta,
            sensor_count,
            minimum,
            maximum,
            discrete_values: values,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::device_over;
    use crate::report::from_hex;

    /// Ported from `HidppParsingTests.DpiList_WithStepMarker_IsRange`. Observed on a G903 HERO:
    /// 0x0064 (100), 0xE032 (step 50), 0x6400 (25600).
    #[test]
    fn a_step_marker_becomes_a_range() {
        let data = from_hex("0064E032640000000000000000000000").unwrap();
        let c = parse_dpi_list(&data, 1);
        assert_eq!((c.minimum, c.maximum, c.step), (100, 25600, 50));
        assert!(c.is_valid(1650));
        assert!(!c.is_valid(1625), "off the 50 step");
        assert!(!c.is_valid(50), "below the minimum");
        assert_eq!(c.clamp(1640), 1650);
        assert_eq!(c.clamp(99_999), 25600);
    }

    /// Ported from `HidppParsingTests.DpiList_Discrete`.
    #[test]
    fn a_list_without_a_step_marker_is_discrete() {
        let data = from_hex("0190032006400C80000000").unwrap();
        let c = parse_dpi_list(&data, 1);
        assert_eq!(c.discrete_values, vec![400, 800, 1600, 3200]);
        assert!(c.is_valid(800));
        assert!(!c.is_valid(1000));
        assert_eq!(c.clamp(900), 800);
        assert_eq!(c.step, 0);
    }

    /// Ported from `HidppParsingTests.DpiList_Empty_IsUnavailable`. An all-zero list means the
    /// sensor could not answer, and the slider must be disabled rather than offer 0 DPI.
    #[test]
    fn an_empty_list_is_unavailable() {
        let c = parse_dpi_list(&[0u8; 16], 1);
        assert_eq!(c.meta.status, CapabilityStatus::Unavailable);
        assert!(!c.meta.is_usable());
        assert!(!c.is_valid(800));
    }

    #[test]
    fn a_trailing_odd_byte_is_ignored_rather_than_read_past() {
        // An odd-length payload must not read one byte past the end.
        let c = parse_dpi_list(&from_hex("019003200640").unwrap()[..5], 1);
        assert_eq!(c.discrete_values, vec![400, 800]);
    }

    #[test]
    fn setting_dpi_reads_back_what_the_sensor_actually_took() {
        let (device, backend) = device_over(
            &[(feature::ADJUSTABLE_DPI, 7)],
            &[
                // getSensorCount -> 1
                ("11FF070A", "11FF070A01"),
                // getSensorDpiList(0) -> 100, step 50, 25600
                ("11FF071A00", "11FF071A000064E0326400"),
                // setSensorDpi(0, 1650) -> echoes the value
                ("11FF073A000672", "11FF073A000672"),
                // getSensorDpi(0) -> current 1650, default 800
                ("11FF072A00", "11FF072A0006720320"),
            ],
        );
        let dpi = DpiFeature::create(&device).unwrap().unwrap();
        assert_eq!(dpi.capability.step, 50);
        assert_eq!(dpi.set(1650).unwrap(), 1650);
        assert_eq!(dpi.get().unwrap(), DpiState { current: 1650, default: 800 });
        assert!(backend.unanswered_requests().is_empty());
    }

    #[test]
    fn an_out_of_range_dpi_never_reaches_the_device() {
        let (device, backend) = device_over(
            &[(feature::ADJUSTABLE_DPI, 7)],
            &[("11FF070A", "11FF070A01"), ("11FF071A00", "11FF071A000064E0326400")],
        );
        let dpi = DpiFeature::create(&device).unwrap().unwrap();
        let before = backend.all_requests().len();
        // 1625 is on no step the sensor reported.
        assert!(matches!(dpi.set(1625), Err(HidppError::Unsupported(_))));
        assert_eq!(backend.all_requests().len(), before, "nothing may be sent");
    }

    #[test]
    fn a_mismatched_echo_is_reported_rather_than_believed() {
        // If the sensor echoes a different value, the write went somewhere we did not intend.
        let (device, _) = device_over(
            &[(feature::ADJUSTABLE_DPI, 7)],
            &[
                ("11FF070A", "11FF070A01"),
                ("11FF071A00", "11FF071A000064E0326400"),
                ("11FF073A000672", "11FF073A000320"),
            ],
        );
        let dpi = DpiFeature::create(&device).unwrap().unwrap();
        match dpi.set(1650) {
            Err(HidppError::Malformed(m)) => assert!(m.contains("800"), "{m}"),
            other => panic!("expected the echo mismatch to be reported, got {other:?}"),
        }
    }

    #[test]
    fn a_zero_default_falls_back_to_the_current_value() {
        // Firmware that does not track a default answers 0, and offering "reset to 0 DPI" would
        // leave the mouse unusable.
        let (device, _) = device_over(
            &[(feature::ADJUSTABLE_DPI, 7)],
            &[
                ("11FF070A", "11FF070A01"),
                ("11FF071A00", "11FF071A000064E0326400"),
                ("11FF072A00", "11FF072A0006720000"),
            ],
        );
        let dpi = DpiFeature::create(&device).unwrap().unwrap();
        assert_eq!(dpi.get().unwrap(), DpiState { current: 1650, default: 1650 });
    }
}
