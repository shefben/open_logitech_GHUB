//! 0x1000 BatteryLevelStatus, 0x1001 BatteryVoltage and 0x1004 UnifiedBattery. All read-only.

use crate::capability::BatterySource;
use crate::channel::RequestOptions;
use crate::device::{HidppDevice, feature};
use crate::error::{HidppError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChargingState {
    #[default]
    Discharging,
    Charging,
    /// Charging at a reduced rate, which firmware reports separately.
    ChargingSlow,
    Full,
    Error,
}

// `Copy`: plain data, every field already `Copy`, and it is read out of a device snapshot in
// several places at once. Cloning it by hand everywhere added noise and no safety.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BatteryStatus {
    /// `None` when the device reports a state but no usable percentage.
    pub level_percent: Option<u8>,
    pub voltage_millivolts: Option<u16>,
    pub charging: ChargingState,
    pub is_low: bool,
    /// Which feature this came from, shown in the UI so an estimated figure is never mistaken for
    /// a measured one.
    pub source: &'static str,
}

impl BatteryStatus {
    /// Whether the device is running off its battery, so a low level is worth warning about.
    ///
    /// Not `charging != Charging`: `Full` and `Error` are both "not discharging", and a device
    /// reporting `Error` at 5% must not raise a low-battery notification every poll.
    pub fn is_discharging(&self) -> bool {
        self.charging == ChargingState::Discharging
    }
}

pub struct BatteryFeature<'d> {
    pub(crate) device: &'d HidppDevice,
    pub source: BatterySource,
}

impl<'d> BatteryFeature<'d> {
    /// Picks the best battery feature the device implements. Unified (0x1004) first: it reports a
    /// real percentage, where 0x1001 only gives a voltage we have to interpolate.
    pub fn create(device: &'d HidppDevice) -> Option<Self> {
        let source = if device.has_feature(feature::UNIFIED_BATTERY) {
            BatterySource::Unified
        } else if device.has_feature(feature::BATTERY_LEVEL_STATUS) {
            BatterySource::LevelStatus
        } else if device.has_feature(feature::BATTERY_VOLTAGE) {
            BatterySource::Voltage
        } else {
            return None;
        };
        Some(Self { device, source })
    }

    pub fn read(&self) -> Result<BatteryStatus> {
        match self.source {
            BatterySource::Unified => {
                let r =
                    self.device.call(feature::UNIFIED_BATTERY, 1, &[], RequestOptions::default())?;
                Ok(parse_unified(r.parameters()))
            }
            BatterySource::LevelStatus => {
                let r = self.device.call(
                    feature::BATTERY_LEVEL_STATUS,
                    0,
                    &[],
                    RequestOptions::default(),
                )?;
                Ok(parse_level_status(r.parameters()))
            }
            BatterySource::Voltage => {
                let r =
                    self.device.call(feature::BATTERY_VOLTAGE, 0, &[], RequestOptions::default())?;
                Ok(parse_voltage(r.parameters()))
            }
            BatterySource::None => {
                Err(HidppError::Unsupported("device reports no battery feature".into()))
            }
        }
    }
}

/// `0x1000`: percentage, next level, charging status.
pub fn parse_level_status(p: &[u8]) -> BatteryStatus {
    let at = |i: usize| p.get(i).copied().unwrap_or(0);
    let charging = match at(2) {
        0 => ChargingState::Discharging,
        1 | 2 => ChargingState::Charging,
        3 => ChargingState::Full,
        4 => ChargingState::ChargingSlow,
        _ => ChargingState::Error,
    };
    BatteryStatus {
        level_percent: Some(at(0)),
        voltage_millivolts: None,
        is_low: at(0) <= 15 && charging == ChargingState::Discharging,
        charging,
        source: "0x1000 BatteryLevelStatus",
    }
}

/// `0x1001`: voltage in big-endian millivolts plus a flags byte. Bit 7 means external power is
/// attached; bits 0-2 then carry the charge status.
pub fn parse_voltage(p: &[u8]) -> BatteryStatus {
    let at = |i: usize| p.get(i).copied().unwrap_or(0);
    let mv = ((at(0) as u16) << 8) | at(1) as u16;
    let flags = at(2);
    let charging = if flags & 0x80 != 0 {
        match flags & 0x07 {
            0 | 1 => ChargingState::Charging,
            2 => ChargingState::Full,
            3 => ChargingState::ChargingSlow,
            _ => ChargingState::Error,
        }
    } else {
        ChargingState::Discharging
    };

    BatteryStatus {
        level_percent: Some(estimate_percent_from_voltage(mv)),
        voltage_millivolts: Some(mv),
        is_low: charging == ChargingState::Discharging && mv < 3650,
        charging,
        // Said out loud in the UI: this number is interpolated off a generic curve, not measured.
        source: "0x1001 BatteryVoltage (percentage estimated from voltage)",
    }
}

/// `0x1004`: percentage, flags, charging status.
pub fn parse_unified(p: &[u8]) -> BatteryStatus {
    let at = |i: usize| p.get(i).copied().unwrap_or(0);
    let charging = match at(2) {
        0 => ChargingState::Discharging,
        1 => ChargingState::Charging,
        2 => ChargingState::ChargingSlow,
        3 => ChargingState::Full,
        _ => ChargingState::Error,
    };
    let critical = at(1) & 0x01 != 0;
    let low = at(1) & 0x02 != 0;
    BatteryStatus {
        // Zero here means "not reported", not "flat".
        level_percent: if at(0) == 0 { None } else { Some(at(0)) },
        voltage_millivolts: None,
        is_low: critical || low,
        charging,
        source: "0x1004 UnifiedBattery",
    }
}

/// Typical single-cell Li-ion discharge curve, on the breakpoints Solaar uses. Only ever shown
/// labelled as an estimate.
pub fn estimate_percent_from_voltage(mv: u16) -> u8 {
    const TABLE: [(u16, u8); 13] = [
        (4186, 100),
        (4067, 90),
        (3989, 80),
        (3922, 70),
        (3859, 60),
        (3811, 50),
        (3778, 40),
        (3751, 30),
        (3717, 20),
        (3671, 10),
        (3646, 5),
        (3579, 2),
        (3500, 0),
    ];
    if mv >= TABLE[0].0 {
        return 100;
    }
    for i in 1..TABLE.len() {
        if mv >= TABLE[i].0 {
            let (hi_mv, hi_pct) = TABLE[i - 1];
            let (lo_mv, lo_pct) = TABLE[i];
            let span = (hi_mv - lo_mv) as i32;
            let pct_span = (hi_pct - lo_pct) as i32;
            let offset = (mv - lo_mv) as i32;
            // Round rather than truncate, matching the C#'s Math.Round.
            let interpolated = (offset * pct_span * 2 + span) / (span * 2);
            return (lo_pct as i32 + interpolated).clamp(0, 100) as u8;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::device_over;
    use crate::report::from_hex;

    /// Ported from `HidppParsingTests.BatteryVoltage_Observed`. 0x0F3A is a real reading.
    #[test]
    fn the_observed_voltage_reading_parses() {
        let s = parse_voltage(&from_hex("0F3A00").unwrap());
        assert_eq!(s.voltage_millivolts, Some(3898));
        assert_eq!(s.charging, ChargingState::Discharging);
        let pct = s.level_percent.unwrap();
        assert!((60..=75).contains(&pct), "estimated {pct}%");
        assert!(s.source.contains("estimated"), "the UI must not present this as measured");
        assert!(!s.is_low);

        // Bit 7 set means external power; the low bits then give the charge state.
        assert_eq!(parse_voltage(&[0x10, 0x00, 0x80]).charging, ChargingState::Charging);
        assert_eq!(parse_voltage(&[0x10, 0x00, 0x82]).charging, ChargingState::Full);
        assert_eq!(parse_voltage(&[0x10, 0x00, 0x83]).charging, ChargingState::ChargingSlow);
    }

    #[test]
    fn the_voltage_curve_is_monotonic_and_bounded() {
        // A non-monotonic curve would make the indicator jump backwards as the battery drains.
        let mut last = 0;
        for mv in (3400..4300).step_by(7) {
            let pct = estimate_percent_from_voltage(mv);
            assert!(pct >= last, "{mv} mV gave {pct}% after {last}%");
            assert!(pct <= 100);
            last = pct;
        }
        assert_eq!(estimate_percent_from_voltage(4500), 100);
        assert_eq!(estimate_percent_from_voltage(3000), 0);
    }

    #[test]
    fn a_low_voltage_is_flagged_only_while_discharging() {
        assert!(parse_voltage(&[0x0E, 0x10, 0x00]).is_low, "3600 mV on battery is low");
        assert!(!parse_voltage(&[0x0E, 0x10, 0x80]).is_low, "not low while charging");
    }

    #[test]
    fn unified_zero_percent_means_unreported_not_flat() {
        let unreported = parse_unified(&[0x00, 0x00, 0x00]);
        assert_eq!(unreported.level_percent, None, "0 must not display as a flat battery");
        let real = parse_unified(&[0x55, 0x00, 0x01]);
        assert_eq!(real.level_percent, Some(0x55));
        assert_eq!(real.charging, ChargingState::Charging);
        // Critical and low come from the flags byte, not from the percentage.
        assert!(parse_unified(&[0x55, 0x01, 0x00]).is_low);
        assert!(parse_unified(&[0x55, 0x02, 0x00]).is_low);
    }

    #[test]
    fn level_status_flags_low_at_fifteen_percent() {
        assert!(parse_level_status(&[15, 0, 0]).is_low);
        assert!(!parse_level_status(&[16, 0, 0]).is_low);
        assert!(!parse_level_status(&[15, 0, 1]).is_low, "charging is not low");
        assert_eq!(parse_level_status(&[50, 0, 3]).charging, ChargingState::Full);
        assert_eq!(parse_level_status(&[50, 0, 9]).charging, ChargingState::Error);
    }

    #[test]
    fn a_short_payload_reads_as_zero_rather_than_panicking() {
        // Firmware answering with fewer bytes than documented must not take the process down.
        assert_eq!(parse_voltage(&[]).voltage_millivolts, Some(0));
        assert_eq!(parse_unified(&[]).level_percent, None);
        assert_eq!(parse_level_status(&[]).level_percent, Some(0));
    }

    #[test]
    fn unified_is_preferred_over_voltage() {
        // 0x1004 gives a real percentage; 0x1001 only a voltage we interpolate. Picking the
        // wrong one would show an estimate when a measurement was available.
        let (device, _) = device_over(
            &[(feature::UNIFIED_BATTERY, 4), (feature::BATTERY_VOLTAGE, 5)],
            &[],
        );
        assert_eq!(BatteryFeature::create(&device).unwrap().source, BatterySource::Unified);

        let (voltage_only, _) = device_over(&[(feature::BATTERY_VOLTAGE, 5)], &[]);
        assert_eq!(
            BatteryFeature::create(&voltage_only).unwrap().source,
            BatterySource::Voltage
        );

        let (none, _) = device_over(&[], &[]);
        assert!(BatteryFeature::create(&none).is_none());
    }

    #[test]
    fn reading_unified_battery_goes_to_function_one() {
        let (device, backend) = device_over(
            &[(feature::UNIFIED_BATTERY, 4)],
            &[("11FF041A", "11FF041A50000100")],
        );
        let status = BatteryFeature::create(&device).unwrap().read().unwrap();
        assert_eq!(status.level_percent, Some(80));
        assert_eq!(status.charging, ChargingState::Charging);
        assert!(backend.unanswered_requests().is_empty());
    }
}
