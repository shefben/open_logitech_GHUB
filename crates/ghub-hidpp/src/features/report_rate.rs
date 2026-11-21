//! 0x8060 Adjustable report rate. Volatile.

use crate::capability::{CapabilityMeta, PersistenceKind, ReportRateCapability};
use crate::channel::RequestOptions;
use crate::device::{HidppDevice, feature};
use crate::error::{HidppError, Result};

pub struct ReportRateFeature<'d> {
    pub(crate) device: &'d HidppDevice,
    pub capability: ReportRateCapability,
}

impl<'d> ReportRateFeature<'d> {
    pub fn create(device: &'d HidppDevice) -> Result<Option<Self>> {
        if !device.has_feature(feature::ADJUSTABLE_REPORT_RATE) {
            return Ok(None);
        }
        let list =
            device.call(feature::ADJUSTABLE_REPORT_RATE, 0, &[], RequestOptions::default())?;
        Ok(Some(Self {
            device,
            capability: ReportRateCapability {
                meta: CapabilityMeta::supported(
                    feature::ADJUSTABLE_REPORT_RATE,
                    PersistenceKind::Volatile,
                ),
                rates_hz: rates_from_mask(list.param(0)),
            },
        }))
    }

    pub fn get_hz(&self) -> Result<u32> {
        let r =
            self.device.call(feature::ADJUSTABLE_REPORT_RATE, 1, &[0], RequestOptions::default())?;
        // The wire value is a period in milliseconds. A zero would divide by zero, and treating it
        // as 1 ms matches what the firmware means by "fastest".
        Ok(1000 / (r.param(0) as u32).max(1))
    }

    pub fn set_hz(&self, hz: u32) -> Result<u32> {
        if !self.capability.rates_hz.contains(&hz) {
            return Err(HidppError::Unsupported(format!(
                "{hz} Hz is not reported as supported ({})",
                self.capability
                    .rates_hz
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let ms = (1000 / hz) as u8;
        self.device.call(feature::ADJUSTABLE_REPORT_RATE, 2, &[ms], RequestOptions::default())?;
        self.get_hz()
    }
}

/// Bit n set means a period of (n + 1) ms is supported, so bit 0 is 1000 Hz.
pub fn rates_from_mask(mask: u8) -> Vec<u32> {
    let mut rates: Vec<u32> = (0..8)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| 1000 / (bit as u32 + 1))
        .collect();
    rates.sort_unstable();
    // Integer division collides: 3 ms and 4 ms both give 250 Hz once truncated, so the mask can
    // produce the same rate twice.
    rates.dedup();
    rates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::device_over;

    /// Ported from `HidppParsingTests.ReportRateMask_Observed0x8B`.
    #[test]
    fn the_observed_mask_decodes_to_four_rates() {
        // 0x8B = bits 0, 1, 3, 7 -> 1, 2, 4, 8 ms -> 1000, 500, 250, 125 Hz.
        assert_eq!(rates_from_mask(0x8B), vec![125, 250, 500, 1000]);
    }

    #[test]
    fn every_bit_of_the_mask_decodes_to_a_distinct_rate() {
        // 1 to 8 ms give 1000, 500, 333, 250, 200, 166, 142, 125 Hz: no two collide after
        // integer division, so the `dedup` in `rates_from_mask` is defensive rather than load
        // bearing. Asserting that here means a future change to the arithmetic that does start
        // colliding gets noticed instead of quietly dropping a rate from the dropdown.
        let all = rates_from_mask(0xFF);
        assert_eq!(all, vec![125, 142, 166, 200, 250, 333, 500, 1000]);
        assert_eq!(all.len(), 8, "one rate per bit");
        // A device reporting no rates leaves the dropdown empty rather than offering a default.
        assert!(rates_from_mask(0).is_empty());
    }

    #[test]
    fn setting_a_rate_reads_it_back() {
        let (device, backend) = device_over(
            &[(feature::ADJUSTABLE_REPORT_RATE, 9)],
            &[
                // getReportRateList -> 0x8B
                ("11FF090A", "11FF090A8B"),
                // setReportRate(1 ms)
                ("11FF092A01", "11FF092A"),
                // getReportRate -> 1 ms
                ("11FF091A00", "11FF091A01"),
            ],
        );
        let rate = ReportRateFeature::create(&device).unwrap().unwrap();
        assert_eq!(rate.capability.rates_hz, vec![125, 250, 500, 1000]);
        assert_eq!(rate.set_hz(1000).unwrap(), 1000);
        assert!(backend.unanswered_requests().is_empty());
    }

    #[test]
    fn an_unsupported_rate_never_reaches_the_device() {
        let (device, backend) = device_over(
            &[(feature::ADJUSTABLE_REPORT_RATE, 9)],
            &[("11FF090A", "11FF090A8B")],
        );
        let rate = ReportRateFeature::create(&device).unwrap().unwrap();
        // 8000 Hz is a real Logitech rate, but not one this mask reported.
        assert!(matches!(rate.set_hz(8000), Err(HidppError::Unsupported(_))));
        assert_eq!(backend.all_requests().len(), 1);
    }
}
