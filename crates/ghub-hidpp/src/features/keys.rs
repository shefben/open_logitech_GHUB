//! Keyboard-side features: 0x8010 G-keys, 0x8020 M-keys, 0x8030 MR key, 0x8040 brightness and
//! 0x4522 DisableKeysByUsage. All volatile.

use crate::channel::RequestOptions;
use crate::device::{HidppDevice, feature};
use crate::error::{HidppError, Result};

/// 0x8010 G-keys: software control diverts G1-Gn to HID++ events instead of the firmware's
/// stored macros.
pub struct GKeyFeature<'d> {
    pub(crate) device: &'d HidppDevice,
    pub count: usize,
}

impl<'d> GKeyFeature<'d> {
    pub fn create(device: &'d HidppDevice) -> Result<Option<Self>> {
        if !device.has_feature(feature::GKEY) {
            return Ok(None);
        }
        let r = device.call(feature::GKEY, 0, &[], RequestOptions::default())?;
        // 32 is the width of the event's bitmask, so a larger count could not be reported anyway.
        Ok(Some(Self { device, count: (r.param(0) as usize).min(32) }))
    }

    pub fn set_software_control(&self, enabled: bool) -> Result<()> {
        self.device.call(feature::GKEY, 2, &[u8::from(enabled)], RequestOptions::default())?;
        Ok(())
    }
}

/// Event 0: little-endian bitmask of the G-keys currently held, bit 0 being G1.
pub fn parse_pressed_gkey_mask(p: &[u8]) -> u32 {
    let at = |i: usize| p.get(i).copied().unwrap_or(0) as u32;
    at(0) | (at(1) << 8) | (at(2) << 16) | (at(3) << 24)
}

/// 0x8020 M-keys (the bank indicator LEDs) and 0x8030 MR key.
pub struct ModeKeyFeature<'d> {
    pub(crate) device: &'d HidppDevice,
    pub count: usize,
    pub has_macro_record_key: bool,
}

impl<'d> ModeKeyFeature<'d> {
    pub fn create(device: &'d HidppDevice) -> Result<Option<Self>> {
        if !device.has_feature(feature::MKEY) {
            return Ok(None);
        }
        let r = device.call(feature::MKEY, 0, &[], RequestOptions::default())?;
        Ok(Some(Self {
            device,
            count: (r.param(0) as usize).min(8),
            has_macro_record_key: device.has_feature(feature::MR_KEY),
        }))
    }

    /// Lights one bank LED, or none when `mode` is 0. The mask is one-hot: lighting two at once
    /// would tell the user they are in two banks.
    pub fn set_mode_indicator(&self, mode: usize) -> Result<()> {
        if mode > self.count {
            return Err(HidppError::Unsupported(format!(
                "M{mode} is beyond the {} banks this keyboard reports",
                self.count
            )));
        }
        let mask = if mode == 0 { 0u8 } else { 1u8 << (mode - 1) };
        self.device.call(feature::MKEY, 1, &[mask], RequestOptions::default())?;
        Ok(())
    }

    pub fn set_macro_record_indicator(&self, on: bool) -> Result<()> {
        if !self.has_macro_record_key {
            return Err(HidppError::Unsupported("device has no MR key".into()));
        }
        // 0x8030 has only a write function; there is no way to read the LED back, so this is one
        // of the few setters that cannot verify itself.
        self.device.call(feature::MR_KEY, 0, &[u8::from(on)], RequestOptions::default())?;
        Ok(())
    }
}

/// M-key event: bitmask of pressed M-keys, bit 0 being M1. Returns the 1-based key, or 0 for none.
pub fn parse_pressed_mode(p: &[u8]) -> usize {
    let mask = p.first().copied().unwrap_or(0);
    (0..8).find(|i| mask & (1 << i) != 0).map(|i| i + 1).unwrap_or(0)
}

/// 0x8040 brightness control.
pub struct BrightnessFeature<'d> {
    pub(crate) device: &'d HidppDevice,
    pub maximum: u16,
    pub steps: u8,
}

impl<'d> BrightnessFeature<'d> {
    pub fn create(device: &'d HidppDevice) -> Result<Option<Self>> {
        if !device.has_feature(feature::BRIGHTNESS_CONTROL) {
            return Ok(None);
        }
        let info = device.call(feature::BRIGHTNESS_CONTROL, 0, &[], RequestOptions::default())?;
        let max = info.param_u16(0);
        Ok(Some(Self {
            device,
            // A device reporting 0 as its maximum would make every slider position invalid;
            // treating that as a percentage scale matches what the firmware accepts.
            maximum: if max == 0 { 100 } else { max },
            steps: info.param(2),
        }))
    }

    pub fn get(&self) -> Result<u16> {
        let r = self.device.call(feature::BRIGHTNESS_CONTROL, 1, &[], RequestOptions::default())?;
        Ok(r.param_u16(0))
    }

    pub fn set(&self, value: u16) -> Result<u16> {
        let value = value.min(self.maximum);
        self.device.call(
            feature::BRIGHTNESS_CONTROL,
            2,
            &[(value >> 8) as u8, value as u8],
            RequestOptions::default(),
        )?;
        self.get()
    }
}

/// 0x4522 DisableKeysByUsage: the keyboard's own game-mode key list.
///
/// Function layout recovered from G HUB's `devio::Feature4522DisableKeysByUsage` in
/// `lghub_agent.exe`: fn0 capability, fn1 disable keys, fn2 enable keys, fn3 enable all. Lists are
/// HID keyboard usages, one byte each, zero-terminated, at most 15 per call. The firmware applies
/// the list while its own game-mode key is engaged, and the list is volatile.
pub struct DisableKeysByUsageFeature<'d> {
    pub(crate) device: &'d HidppDevice,
    pub capability_byte: u8,
}

impl<'d> DisableKeysByUsageFeature<'d> {
    pub const MAX_KEYS_PER_CALL: usize = 15;

    pub fn create(device: &'d HidppDevice) -> Result<Option<Self>> {
        if !device.has_feature(feature::DISABLE_KEYS_BY_USAGE) {
            return Ok(None);
        }
        let caps =
            device.call(feature::DISABLE_KEYS_BY_USAGE, 0, &[], RequestOptions::default())?;
        Ok(Some(Self { device, capability_byte: caps.param(0) }))
    }

    pub fn enable_all(&self) -> Result<()> {
        self.device.call(feature::DISABLE_KEYS_BY_USAGE, 3, &[], RequestOptions::default())?;
        Ok(())
    }

    /// Replaces the firmware list: enable everything, then disable the given usages.
    pub fn set_disabled_keys(&self, usages: &[u8]) -> Result<()> {
        self.enable_all()?;
        for list in build_key_lists(usages) {
            let options = if list.len() > 3 {
                RequestOptions::long()
            } else {
                RequestOptions::default()
            };
            self.device.call(feature::DISABLE_KEYS_BY_USAGE, 1, &list, options)?;
        }
        Ok(())
    }
}

/// Splits a usage list into zero-terminated chunks of at most 15 usages, dropping zeros and
/// duplicates. Zero is the terminator, so a zero in the middle would truncate the list.
pub fn build_key_lists(usages: &[u8]) -> Vec<Vec<u8>> {
    let mut unique: Vec<u8> = Vec::new();
    for u in usages {
        if *u != 0 && !unique.contains(u) {
            unique.push(*u);
        }
    }
    unique
        .chunks(DisableKeysByUsageFeature::MAX_KEYS_PER_CALL)
        .map(|chunk| {
            let mut list = chunk.to_vec();
            list.push(0);
            list
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::device_over;

    #[test]
    fn the_gkey_event_mask_is_little_endian() {
        // G1 and G9: bit 0 of byte 0 and bit 0 of byte 1.
        assert_eq!(parse_pressed_gkey_mask(&[0x01, 0x01, 0x00, 0x00]), 0x0000_0101);
        assert_eq!(parse_pressed_gkey_mask(&[0, 0, 0, 0x80]), 0x8000_0000);
        // A short payload reads as zero rather than panicking.
        assert_eq!(parse_pressed_gkey_mask(&[]), 0);
    }

    #[test]
    fn the_mkey_event_gives_the_lowest_pressed_bank() {
        assert_eq!(parse_pressed_mode(&[0x01]), 1);
        assert_eq!(parse_pressed_mode(&[0x04]), 3);
        assert_eq!(parse_pressed_mode(&[0x00]), 0, "no bank held");
        // Two bits set is physically impossible on a one-hot indicator; taking the lowest is
        // deterministic rather than arbitrary.
        assert_eq!(parse_pressed_mode(&[0x06]), 2);
    }

    #[test]
    fn the_bank_indicator_mask_is_one_hot() {
        let (device, backend) = device_over(
            &[(feature::MKEY, 0x0C)],
            &[
                ("11FF0C0A", "11FF0C0A03"),
                // M2 -> mask 0x02
                ("11FF0C1A02", "11FF0C1A02"),
                // M0 (no bank) -> mask 0x00
                ("11FF0C1A00", "11FF0C1A00"),
            ],
        );
        let keys = ModeKeyFeature::create(&device).unwrap().unwrap();
        assert_eq!(keys.count, 3);
        keys.set_mode_indicator(2).unwrap();
        keys.set_mode_indicator(0).unwrap();
        assert!(backend.unanswered_requests().is_empty());
    }

    #[test]
    fn a_bank_beyond_the_reported_count_never_reaches_the_device() {
        let (device, backend) = device_over(&[(feature::MKEY, 0x0C)], &[("11FF0C0A", "11FF0C0A03")]);
        let keys = ModeKeyFeature::create(&device).unwrap().unwrap();
        // Shifting by 8 would wrap and light the wrong LED, or panic in debug.
        assert!(matches!(keys.set_mode_indicator(9), Err(HidppError::Unsupported(_))));
        assert_eq!(backend.all_requests().len(), 1);
    }

    #[test]
    fn the_mr_indicator_needs_the_mr_feature() {
        let (device, backend) = device_over(&[(feature::MKEY, 0x0C)], &[("11FF0C0A", "11FF0C0A03")]);
        let keys = ModeKeyFeature::create(&device).unwrap().unwrap();
        assert!(!keys.has_macro_record_key);
        assert!(matches!(keys.set_macro_record_indicator(true), Err(HidppError::Unsupported(_))));
        assert_eq!(backend.all_requests().len(), 1);
    }

    #[test]
    fn key_lists_are_zero_terminated_and_chunked_at_fifteen() {
        let usages: Vec<u8> = (1..=20).collect();
        let lists = build_key_lists(&usages);
        assert_eq!(lists.len(), 2);
        assert_eq!(lists[0].len(), 16, "15 usages plus the terminator");
        assert_eq!(lists[0][15], 0);
        assert_eq!(lists[1], vec![16, 17, 18, 19, 20, 0]);
    }

    #[test]
    fn a_zero_usage_is_dropped_rather_than_truncating_the_list() {
        // Zero is the terminator, so passing it through would silently drop every later key.
        let lists = build_key_lists(&[0x29, 0x00, 0x2B, 0x29]);
        assert_eq!(lists, vec![vec![0x29, 0x2B, 0]], "zeros and duplicates removed");
        assert!(build_key_lists(&[]).is_empty());
        assert!(build_key_lists(&[0, 0]).is_empty());
    }

    #[test]
    fn disabling_keys_enables_everything_first() {
        // Without the enable-all the new list would union with whatever was already disabled, and
        // a key the user just re-enabled would stay dead.
        let (device, backend) = device_over(
            &[(feature::DISABLE_KEYS_BY_USAGE, 0x0E)],
            &[
                ("11FF0E0A", "11FF0E0A01"),
                // fn3 enable all
                ("11FF0E3A", "11FF0E3A"),
                // fn1 disable [0x29 Esc, 0x2B Tab], zero-terminated
                ("11FF0E1A292B00", "11FF0E1A"),
            ],
        );
        let keys = DisableKeysByUsageFeature::create(&device).unwrap().unwrap();
        keys.set_disabled_keys(&[0x29, 0x2B]).unwrap();
        assert!(backend.unanswered_requests().is_empty());
        assert_eq!(backend.all_requests().len(), 3);
    }

    #[test]
    fn brightness_clamps_to_the_reported_maximum_and_reads_back() {
        let (device, backend) = device_over(
            &[(feature::BRIGHTNESS_CONTROL, 0x0D)],
            &[
                // getInfo -> max 100, 5 steps
                ("11FF0D0A", "11FF0D0A006405"),
                // setBrightness(100) after clamping 250 down
                ("11FF0D2A0064", "11FF0D2A"),
                ("11FF0D1A", "11FF0D1A0064"),
            ],
        );
        let brightness = BrightnessFeature::create(&device).unwrap().unwrap();
        assert_eq!(brightness.maximum, 100);
        assert_eq!(brightness.steps, 5);
        assert_eq!(brightness.set(250).unwrap(), 100);
        assert!(backend.unanswered_requests().is_empty());
    }

    #[test]
    fn a_zero_maximum_becomes_a_percentage_scale() {
        let (device, _) = device_over(
            &[(feature::BRIGHTNESS_CONTROL, 0x0D)],
            &[("11FF0D0A", "11FF0D0A000005")],
        );
        // Leaving the maximum at 0 would make every slider position clamp to 0.
        assert_eq!(BrightnessFeature::create(&device).unwrap().unwrap().maximum, 100);
    }

    #[test]
    fn gkey_count_is_capped_at_the_event_mask_width() {
        let (device, _) = device_over(&[(feature::GKEY, 0x0B)], &[("11FF0B0A", "11FF0B0AFF")]);
        // The event is a 32-bit mask, so 255 G-keys cannot be reported even if claimed.
        assert_eq!(GKeyFeature::create(&device).unwrap().unwrap().count, 32);
    }
}
