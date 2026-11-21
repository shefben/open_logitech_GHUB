//! 0x1B04 Reprogrammable controls v4: diverting a button to software, in RAM only.
//!
//! Persistent firmware remapping exists in the protocol and is deliberately never used. Diversion
//! is lost on power cycle, so the worst case of a crash mid-session is a mouse whose buttons work
//! normally again.

use crate::capability::ButtonDescriptor;
use crate::channel::RequestOptions;
use crate::device::{HidppDevice, feature};
use crate::error::{HidppError, Result};

/// What the firmware currently does with one control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonReporting {
    pub control_id: u16,
    pub diverted: bool,
    pub raw_xy_diverted: bool,
    /// The control id the firmware has this one mapped to, 0 when unmapped.
    pub remapped_to: u16,
}

pub struct ReprogControlsFeature<'d> {
    pub(crate) device: &'d HidppDevice,
    pub buttons: Vec<ButtonDescriptor>,
}

impl<'d> ReprogControlsFeature<'d> {
    pub fn create(device: &'d HidppDevice) -> Result<Option<Self>> {
        if !device.has_feature(feature::REPROG_CONTROLS_V4) {
            return Ok(None);
        }
        let count = device.call(feature::REPROG_CONTROLS_V4, 0, &[], RequestOptions::default())?;
        // 32 is a cap, not a count: no Logitech device has more controls, and a firmware answering
        // 200 would otherwise have us issue 200 requests.
        let total = (count.param(0) as usize).min(32);
        let mut buttons = Vec::with_capacity(total);
        for i in 0..total as u8 {
            let info =
                device.call(feature::REPROG_CONTROLS_V4, 1, &[i], RequestOptions::default())?;
            buttons.push(parse_control_info(info.parameters()));
        }
        Ok(Some(Self { device, buttons }))
    }

    pub fn get_reporting(&self, control_id: u16) -> Result<ButtonReporting> {
        let r = self.device.call(
            feature::REPROG_CONTROLS_V4,
            2,
            &[(control_id >> 8) as u8, control_id as u8],
            RequestOptions::default(),
        )?;
        Ok(ButtonReporting {
            control_id,
            diverted: r.param(2) & 0x01 != 0,
            raw_xy_diverted: r.param(2) & 0x04 != 0,
            remapped_to: r.param_u16(3),
        })
    }

    /// Diverts a control to software, or gives it back to the firmware.
    pub fn set_diverted(&self, control_id: u16, diverted: bool) -> Result<ButtonReporting> {
        let button = self.buttons.iter().find(|b| b.control_id == control_id);
        if !button.is_some_and(|b| b.divertable) {
            return Err(HidppError::Unsupported(format!(
                "control 0x{control_id:04X} cannot be diverted on this device"
            )));
        }
        // Bit 0 is divert, bit 1 is "divert valid". The persist bits are never set: this is a
        // volatile change by design.
        let flags = 0x02 | u8::from(diverted);
        self.device.call(
            feature::REPROG_CONTROLS_V4,
            3,
            &[(control_id >> 8) as u8, control_id as u8, flags, 0, 0],
            RequestOptions::default(),
        )?;
        self.get_reporting(control_id)
    }
}

pub fn parse_control_info(p: &[u8]) -> ButtonDescriptor {
    let at = |i: usize| p.get(i).copied().unwrap_or(0);
    let control_id = ((at(0) as u16) << 8) | at(1) as u16;
    let flags = at(4);
    ButtonDescriptor {
        control_id,
        task_id: ((at(2) as u16) << 8) | at(3) as u16,
        name: ControlNames::get(control_id),
        is_mouse_button: flags & 0x01 != 0,
        reprogrammable: flags & 0x10 != 0,
        divertable: flags & 0x20 != 0,
        persistable: flags & 0x40 != 0,
    }
}

/// Parses the diverted-buttons event: up to four pressed control ids, big-endian, zero-padded.
pub fn parse_diverted_buttons(p: &[u8]) -> Vec<u16> {
    let mut pressed = Vec::new();
    let mut i = 0;
    // Only the first 8 bytes carry control ids; the rest of a long report is padding.
    while i + 1 < 8 && i + 1 < p.len() {
        let cid = ((p[i] as u16) << 8) | p[i + 1] as u16;
        if cid != 0 {
            pressed.push(cid);
        }
        i += 2;
    }
    pressed
}

pub struct ControlNames;

impl ControlNames {
    pub fn get(cid: u16) -> String {
        match cid {
            0x0050 => "Left click".into(),
            0x0051 => "Right click".into(),
            0x0052 => "Middle click".into(),
            0x0053 => "Back".into(),
            0x0056 => "Forward".into(),
            0x005B => "Wheel tilt left".into(),
            0x005D => "Wheel tilt right".into(),
            0x00C3 => "Gesture button".into(),
            0x00C4 => "Smart shift".into(),
            0x00D7 => "Switch receiver".into(),
            0x00ED => "DPI up".into(),
            0x00EE => "DPI down".into(),
            0x00FD => "DPI switch".into(),
            // An unnamed control is still assignable, so it gets a stable label rather than
            // being hidden from the UI.
            other => format!("Control 0x{other:04X}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::device_over;
    use crate::report::from_hex;

    /// Ported from `HidppParsingTests.ControlInfo_Flags`. Both vectors are as observed.
    #[test]
    fn control_flags_decode() {
        // Gesture button: flags 0x30 = reprogrammable + divertable.
        let b = parse_control_info(&from_hex("00C300A930000000").unwrap());
        assert_eq!(b.control_id, 0x00C3);
        assert_eq!(b.task_id, 0x00A9);
        assert_eq!(b.name, "Gesture button");
        assert!(b.divertable && b.reprogrammable);
        assert!(!b.persistable);
        assert!(b.assignable());

        // Left click: flags 0x01 = mouse button, not divertable. The UI must grey it out.
        let locked = parse_control_info(&from_hex("0050003801000000").unwrap());
        assert!(locked.is_mouse_button);
        assert!(!locked.divertable);
        assert!(!locked.assignable());
        assert!(locked.disabled_reason().is_some());
    }

    #[test]
    fn an_unnamed_control_still_gets_a_label() {
        assert_eq!(ControlNames::get(0x0199), "Control 0x0199");
        assert_eq!(ControlNames::get(0x00FD), "DPI switch");
    }

    #[test]
    fn the_diverted_event_reads_up_to_four_ids() {
        let p = from_hex("00C300ED0000000000000000000000000000").unwrap();
        assert_eq!(parse_diverted_buttons(&p), vec![0x00C3, 0x00ED]);
        // All-zero means every diverted button was released.
        assert!(parse_diverted_buttons(&[0u8; 16]).is_empty());
        // Ids past the first four bytes pairs are padding and must be ignored.
        let five = from_hex("0001000200030004000500000000000000000000").unwrap();
        assert_eq!(parse_diverted_buttons(&five), vec![1, 2, 3, 4]);
    }

    #[test]
    fn diverting_a_locked_control_never_reaches_the_device() {
        let (device, backend) = device_over(
            &[(feature::REPROG_CONTROLS_V4, 6)],
            &[
                ("11FF060A", "11FF060A01"),
                // One control: left click, flags 0x01, not divertable.
                ("11FF061A00", "11FF061A0050003801"),
            ],
        );
        let controls = ReprogControlsFeature::create(&device).unwrap().unwrap();
        assert_eq!(controls.buttons.len(), 1);
        let before = backend.all_requests().len();
        assert!(matches!(
            controls.set_diverted(0x0050, true),
            Err(HidppError::Unsupported(_))
        ));
        // Also true for a control the device never reported at all.
        assert!(matches!(
            controls.set_diverted(0x00C3, true),
            Err(HidppError::Unsupported(_))
        ));
        assert_eq!(backend.all_requests().len(), before, "nothing may be sent");
    }

    #[test]
    fn diversion_sets_the_valid_bit_and_no_persist_bits() {
        // The flags byte is the whole safety story here: 0x03 is divert + valid, and bits 2 and 3
        // would make the change persistent in firmware.
        let (device, backend) = device_over(
            &[(feature::REPROG_CONTROLS_V4, 6)],
            &[
                ("11FF060A", "11FF060A01"),
                ("11FF061A00", "11FF061A00C300A930"),
                // setCidReporting(0x00C3, flags 0x03)
                ("11FF063A00C3030000", "11FF063A00C3030000"),
                // getCidReporting(0x00C3) -> flags byte 0x01: diverted, nothing remapped
                ("11FF062A00C3", "11FF062A00C301"),
            ],
        );
        let controls = ReprogControlsFeature::create(&device).unwrap().unwrap();
        let state = controls.set_diverted(0x00C3, true).unwrap();
        assert!(state.diverted);
        assert_eq!(state.remapped_to, 0, "we never remap, only divert");
        assert!(backend.unanswered_requests().is_empty(), "the flags byte must be exactly 0x03");
    }

    #[test]
    fn an_implausible_control_count_is_capped() {
        // 0xFF controls would mean 255 requests. The cap means 32, and the transcript proves it.
        let mut pairs: Vec<(String, String)> = vec![("11FF060A".into(), "11FF060AFF".into())];
        for i in 0..32u8 {
            pairs.push((format!("11FF061A{i:02X}"), format!("11FF061A00{i:02X}000030")));
        }
        let borrowed: Vec<(&str, &str)> =
            pairs.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let (device, backend) = device_over(&[(feature::REPROG_CONTROLS_V4, 6)], &borrowed);
        let controls = ReprogControlsFeature::create(&device).unwrap().unwrap();
        assert_eq!(controls.buttons.len(), 32);
        assert_eq!(backend.all_requests().len(), 33, "count plus 32 infos, not 256");
    }
}
