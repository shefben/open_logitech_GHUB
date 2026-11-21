//! Mouse-side features: 0x8110 MouseButtonSpy and 0x2121 HiResWheel. Both volatile.

use crate::channel::RequestOptions;
use crate::device::{HidppDevice, feature};
use crate::error::{HidppError, Result};

/// Length of the 0x8110 button mapping: one byte per physical button.
///
/// A free constant rather than an associated one because the impl below carries a lifetime, and
/// Rust will not accept a generic `Self` in an array length.
pub const MAPPING_LENGTH: usize = 16;

/// The mapping the firmware holds: index N is physical button N + 1.
pub type ButtonMapping = [u8; MAPPING_LENGTH];

/// 0x8110 Mouse button spy, for G903-class mice that do not implement 0x1B04.
///
/// Function layout recovered from G HUB's `devio::Feature8110MouseButtonSpy` in `lghub_agent.exe`
/// and matching libhidpp: fn0 button count, fn1 start spy, fn2 stop spy, fn3 get mapping, fn4 set
/// mapping. The mapping is 16 bytes, one per physical button: code N+1 means button N+1 keeps its
/// native behaviour, 0 means muted so the button only reports through the spy event. The firmware
/// resets it on power cycle, sleep and receiver link-up, so callers re-apply it.
pub struct MouseButtonSpyFeature<'d> {
    pub(crate) device: &'d HidppDevice,
    pub button_count: usize,
}

impl<'d> MouseButtonSpyFeature<'d> {
    /// Kept for callers that reach for it through the type.
    pub const MAPPING_LENGTH: usize = MAPPING_LENGTH;

    pub fn create(device: &'d HidppDevice) -> Result<Option<Self>> {
        if !device.has_feature(feature::MOUSE_BUTTON_SPY) {
            return Ok(None);
        }
        let r = device.call(feature::MOUSE_BUTTON_SPY, 0, &[], RequestOptions::default())?;
        Ok(Some(Self {
            device,
            button_count: (r.param(0) as usize).min(MAPPING_LENGTH),
        }))
    }

    pub fn start(&self) -> Result<()> {
        self.device.call(feature::MOUSE_BUTTON_SPY, 1, &[], RequestOptions::default())?;
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
        self.device.call(feature::MOUSE_BUTTON_SPY, 2, &[], RequestOptions::default())?;
        Ok(())
    }

    pub fn get_mapping(&self) -> Result<ButtonMapping> {
        let r = self.device.call(feature::MOUSE_BUTTON_SPY, 3, &[], RequestOptions::default())?;
        let mut mapping = [0u8; MAPPING_LENGTH];
        for (i, slot) in mapping.iter_mut().enumerate() {
            *slot = r.param(i);
        }
        Ok(mapping)
    }

    /// Writes the mapping and returns what the device reports afterwards.
    ///
    /// `non_idempotent`: resending a mapping write after a lost reply could mute a button the
    /// firmware had already un-muted.
    pub fn set_mapping(&self, mapping: &ButtonMapping) -> Result<ButtonMapping> {
        self.device.call(
            feature::MOUSE_BUTTON_SPY,
            4,
            mapping,
            RequestOptions::non_idempotent().forcing_long(),
        )?;
        self.get_mapping()
    }
}

/// Identity mapping: every button keeps its native behaviour.
pub fn identity_mapping(button_count: usize) -> ButtonMapping {
    let mut m = [0u8; MAPPING_LENGTH];
    for (i, slot) in m.iter_mut().enumerate().take(button_count.min(MAPPING_LENGTH)) {
        *slot = i as u8 + 1;
    }
    m
}

/// Identity mapping with the given 1-based buttons muted.
///
/// Buttons 1-3 (left, right, middle) are never muted. If this application stops without restoring
/// the mapping, the user is left with a working pointer rather than a dead mouse.
pub fn muted_mapping(button_count: usize, muted_buttons: &[usize]) -> ButtonMapping {
    let mut m = identity_mapping(button_count);
    let last = button_count.min(MAPPING_LENGTH);
    for b in muted_buttons {
        if *b > 3 && *b <= last {
            m[*b - 1] = 0;
        }
    }
    m
}

/// Event 0 payload: big-endian 16-bit mask of pressed physical buttons, bit 0 being button 1.
pub fn parse_button_mask(p: &[u8]) -> u16 {
    let at = |i: usize| p.get(i).copied().unwrap_or(0) as u16;
    (at(0) << 8) | at(1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HiResWheelInfo {
    pub multiplier: u8,
    pub can_invert: bool,
    pub has_ratchet_switch: bool,
}

/// 0x2121 Hi-res wheel. Function layout as in Solaar and G HUB's `devio::Feature2121HiResWheel`:
/// fn0 capabilities, fn1 get mode, fn2 set mode. Mode bits: 0x01 diverted to HID++, 0x02 high
/// resolution, 0x04 inverted.
pub struct HiResWheelFeature<'d> {
    pub(crate) device: &'d HidppDevice,
    pub info: HiResWheelInfo,
}

impl<'d> HiResWheelFeature<'d> {
    pub fn create(device: &'d HidppDevice) -> Result<Option<Self>> {
        if !device.has_feature(feature::HI_RES_WHEEL) {
            return Ok(None);
        }
        let caps = device.call(feature::HI_RES_WHEEL, 0, &[], RequestOptions::default())?;
        Ok(Some(Self { device, info: parse_capabilities(&[caps.param(0), caps.param(1)]) }))
    }

    pub fn get_mode(&self) -> Result<u8> {
        Ok(self.device.call(feature::HI_RES_WHEEL, 1, &[], RequestOptions::default())?.param(0))
    }

    /// Sets resolution and inversion, keeping whatever the diversion bit currently is, and returns
    /// the mode read back.
    pub fn set(&self, high_resolution: bool, inverted: bool) -> Result<u8> {
        if inverted && !self.info.can_invert {
            return Err(HidppError::Unsupported(
                "this wheel does not report support for inversion".into(),
            ));
        }
        let current = self.get_mode()?;
        self.device.call(
            feature::HI_RES_WHEEL,
            2,
            &[build_mode(current, high_resolution, inverted)],
            RequestOptions::default(),
        )?;
        self.get_mode()
    }
}

pub fn parse_capabilities(p: &[u8]) -> HiResWheelInfo {
    let flags = p.get(1).copied().unwrap_or(0);
    HiResWheelInfo {
        multiplier: p.first().copied().unwrap_or(0),
        can_invert: flags & 0x08 != 0,
        has_ratchet_switch: flags & 0x04 != 0,
    }
}

/// Rewrites only the resolution and inversion bits of a mode byte.
///
/// Bit 0 is diversion to HID++, which the input layer owns. Clobbering it here would silently stop
/// scroll events reaching the application that diverted them.
pub fn build_mode(current: u8, high_resolution: bool, inverted: bool) -> u8 {
    let mut mode = current & !0x06;
    if high_resolution {
        mode |= 0x02;
    }
    if inverted {
        mode |= 0x04;
    }
    mode
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::device_over;

    #[test]
    fn the_identity_mapping_numbers_buttons_from_one() {
        let m = identity_mapping(5);
        assert_eq!(&m[..5], &[1, 2, 3, 4, 5]);
        assert_eq!(&m[5..], &[0u8; 11], "unreported buttons stay zero");
        // A count past the mapping length must not overflow the array.
        assert_eq!(identity_mapping(99)[15], 16);
    }

    #[test]
    fn the_first_three_buttons_can_never_be_muted() {
        // If this process dies without restoring the mapping, left, right and middle click must
        // still work. That is the whole reason for the guard.
        let m = muted_mapping(12, &[1, 2, 3, 4, 11]);
        assert_eq!(&m[..4], &[1, 2, 3, 0], "button 4 muted, 1-3 untouched");
        assert_eq!(m[10], 0, "button 11 muted");
        // A button the device never reported is ignored rather than written past the count.
        let narrow = muted_mapping(5, &[9]);
        assert_eq!(narrow, identity_mapping(5));
    }

    #[test]
    fn the_spy_event_mask_is_big_endian() {
        // Button 1 and button 9.
        assert_eq!(parse_button_mask(&[0x01, 0x01]), 0x0101);
        assert_eq!(parse_button_mask(&[0x80, 0x00]), 0x8000);
        assert_eq!(parse_button_mask(&[]), 0);
    }

    #[test]
    fn setting_a_mapping_reads_it_back() {
        let mapping = muted_mapping(12, &[4]);
        let hex: String = mapping.iter().map(|b| format!("{b:02X}")).collect();
        let (device, backend) = device_over(
            &[(feature::MOUSE_BUTTON_SPY, 0x10)],
            &[
                ("11FF100A", "11FF100A0C"),
                (&format!("11FF104A{hex}"), "11FF104A"),
                ("11FF103A", &format!("11FF103A{hex}")),
            ],
        );
        let spy = MouseButtonSpyFeature::create(&device).unwrap().unwrap();
        assert_eq!(spy.button_count, 12);
        assert_eq!(spy.set_mapping(&mapping).unwrap(), mapping);
        assert!(backend.unanswered_requests().is_empty());
    }

    #[test]
    fn the_wheel_mode_keeps_the_diversion_bit() {
        // Bit 0 belongs to the input layer. Turning high resolution on must not take diversion
        // away, or scroll events stop reaching whatever diverted them.
        assert_eq!(build_mode(0x01, true, false), 0x03);
        assert_eq!(build_mode(0x01, false, false), 0x01);
        assert_eq!(build_mode(0x07, false, false), 0x01, "both bits cleared, diversion kept");
        assert_eq!(build_mode(0x00, true, true), 0x06);
        // Unknown high bits are preserved rather than zeroed.
        assert_eq!(build_mode(0x80, true, false), 0x82);
    }

    #[test]
    fn wheel_capabilities_decode() {
        let both = parse_capabilities(&[8, 0x0C]);
        assert_eq!(both.multiplier, 8);
        assert!(both.can_invert && both.has_ratchet_switch);
        let neither = parse_capabilities(&[8, 0x00]);
        assert!(!neither.can_invert && !neither.has_ratchet_switch);
        assert_eq!(parse_capabilities(&[]).multiplier, 0);
    }

    #[test]
    fn inverting_a_wheel_that_cannot_invert_never_reaches_the_device() {
        let (device, backend) = device_over(
            &[(feature::HI_RES_WHEEL, 0x05)],
            &[("11FF050A", "11FF050A0800")],
        );
        let wheel = HiResWheelFeature::create(&device).unwrap().unwrap();
        assert!(!wheel.info.can_invert);
        assert!(matches!(wheel.set(true, true), Err(HidppError::Unsupported(_))));
        assert_eq!(backend.all_requests().len(), 1);
    }
}
