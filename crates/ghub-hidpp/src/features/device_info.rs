//! 0x0005 DeviceName and 0x0003 DeviceInformation. Read-only.
//!
//! Firmware version is **read and displayed only**. Nothing in this crate flashes firmware, and
//! the DFU feature ids are refused outright in [`crate::device::HidppDevice::call`].

use crate::channel::RequestOptions;
use crate::device::{HidppDevice, feature};
use crate::error::Result;

/// `0x0005` DeviceName: a length, then the name in 16-byte chunks.
pub fn read_name(device: &HidppDevice) -> Result<Option<String>> {
    if !device.has_feature(feature::DEVICE_NAME) {
        return Ok(None);
    }
    let count = device.call(feature::DEVICE_NAME, 0, &[], RequestOptions::default())?;
    let length = count.param(0) as usize;
    // A device claiming a name longer than 64 bytes is answering something else.
    if length == 0 || length > 64 {
        return Ok(None);
    }

    let mut bytes: Vec<u8> = Vec::with_capacity(length);
    while bytes.len() < length {
        let chunk =
            device.call(feature::DEVICE_NAME, 1, &[bytes.len() as u8], RequestOptions::default())?;
        let before = bytes.len();
        for b in chunk.parameters() {
            if bytes.len() >= length {
                break;
            }
            bytes.push(*b);
        }
        // A chunk that added nothing would loop forever; firmware that pads with nothing is
        // simply done.
        if bytes.len() == before {
            break;
        }
    }

    // Names are ASCII. Anything else is a misread, and a lossy conversion would put replacement
    // characters in the window title.
    let name: String = bytes.iter().map(|b| *b as char).collect();
    Ok(Some(name.trim_end_matches(['\0', ' ']).to_owned()))
}

/// One firmware entity, as reported by `0x0003` function 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareEntity {
    pub index: u8,
    pub kind: u8,
    pub prefix: String,
    pub major: u8,
    pub minor: u8,
    pub build: u16,
}

impl FirmwareEntity {
    pub fn kind_name(&self) -> &'static str {
        match self.kind {
            0 => "Main application",
            1 => "Bootloader",
            2 => "Hardware",
            5 => "Sensor",
            8 => "Other",
            9 => "Reserved",
            _ => "Unknown type",
        }
    }
}

impl std::fmt::Display for FirmwareEntity {
    /// `U1 31.02.0018`, matching the string G HUB shows. The numbers are BCD-ish on the wire and
    /// are printed as hex, which is how Logitech's own versions read.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = format!(
            "{} {:02X}.{:02X}.{:04X}",
            self.prefix.trim(),
            self.major,
            self.minor,
            self.build
        );
        f.write_str(text.trim())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceInformation {
    pub entity_count: usize,
    pub transport: u16,
    pub model_ids: Vec<u16>,
    pub firmware: Vec<FirmwareEntity>,
}

impl DeviceInformation {
    /// Main application firmware, the one worth showing.
    pub fn main_firmware(&self) -> Option<&FirmwareEntity> {
        self.firmware.iter().find(|f| f.kind == 0)
    }

    pub fn transport_usb(&self) -> bool {
        self.transport & 0x0008 != 0
    }

    pub fn transport_lightspeed(&self) -> bool {
        self.transport & 0x0004 != 0
    }

    pub fn transport_bluetooth(&self) -> bool {
        self.transport & 0x0001 != 0
    }
}

pub fn read_information(device: &HidppDevice) -> Result<Option<DeviceInformation>> {
    if !device.has_feature(feature::DEVICE_INFORMATION) {
        return Ok(None);
    }
    let info = device.call(feature::DEVICE_INFORMATION, 0, &[], RequestOptions::default())?;
    // Cap at 8: the answer has room for no more, and a larger count is a misread.
    let entities = (info.param(0) as usize).min(8);
    let transport = info.param_u16(5);
    let model_ids: Vec<u16> =
        [info.param_u16(7), info.param_u16(9), info.param_u16(11)]
            .into_iter()
            .filter(|m| *m != 0)
            .collect();

    let mut firmware = Vec::with_capacity(entities);
    for e in 0..entities as u8 {
        let fw = device.call(feature::DEVICE_INFORMATION, 1, &[e], RequestOptions::default())?;
        let prefix: String = fw.parameters()[1..4]
            .iter()
            .map(|b| if *b == 0 { ' ' } else { *b as char })
            .collect();
        firmware.push(FirmwareEntity {
            index: e,
            kind: fw.param(0),
            prefix,
            major: fw.param(4),
            minor: fw.param(5),
            build: fw.param_u16(6),
        });
    }

    Ok(Some(DeviceInformation { entity_count: entities, transport, model_ids, firmware }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::device_over;

    #[test]
    fn a_name_is_read_in_chunks_and_trimmed() {
        // DeviceName.GetCount -> 24, then two 16-byte chunks. "G815 RGB MECHANICAL" padded.
        let (device, backend) = device_over(
            &[(feature::DEVICE_NAME, 3)],
            &[
                ("11FF030A", "11FF030A13"),
                // offset 0: "G815 RGB MECHANI", a full 16-byte chunk
                ("11FF031A00", "11FF031A4738313520524742204D454348414E49"),
                // offset 16: "CAL", then the firmware's zero padding
                ("11FF031A10", "11FF031A43414C"),
            ],
        );
        assert_eq!(read_name(&device).unwrap().as_deref(), Some("G815 RGB MECHANICAL"));
        assert!(backend.unanswered_requests().is_empty());
    }

    #[test]
    fn an_absent_name_feature_is_none_without_asking() {
        let (device, backend) = device_over(&[], &[]);
        assert_eq!(read_name(&device).unwrap(), None);
        assert!(backend.all_requests().is_empty());
    }

    #[test]
    fn an_implausible_name_length_is_refused() {
        // 0xFF would have us issue 16 chunk reads for something that is not a name.
        let (device, backend) = device_over(
            &[(feature::DEVICE_NAME, 3)],
            &[("11FF030A", "11FF030AFF")],
        );
        assert_eq!(read_name(&device).unwrap(), None);
        assert_eq!(backend.all_requests().len(), 1, "must not walk chunks");
    }

    #[test]
    fn firmware_is_read_and_formatted_the_way_g_hub_shows_it() {
        let (device, _) = device_over(
            &[(feature::DEVICE_INFORMATION, 2)],
            &[
                // 2 entities, transport 0x000C (USB + Lightspeed), model ids.
                ("11FF020A", "11FF020A0200000000000CC33F00000000"),
                // entity 0: main application, prefix "U1", version 31.02 build 0x0018
                ("11FF021A00", "11FF021A0055310031020018"),
                // entity 1: bootloader
                ("11FF021A01", "11FF021A01424C310102000A"),
            ],
        );
        let info = read_information(&device).unwrap().unwrap();
        assert_eq!(info.entity_count, 2);
        assert!(info.transport_usb() && info.transport_lightspeed());
        assert!(!info.transport_bluetooth());
        assert_eq!(info.model_ids, vec![0xC33F]);

        let main = info.main_firmware().unwrap();
        assert_eq!(main.kind_name(), "Main application");
        assert_eq!(main.to_string(), "U1 31.02.0018");
        assert_eq!(info.firmware[1].kind_name(), "Bootloader");
    }
}
