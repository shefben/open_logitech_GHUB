//! HID++ report ids, lengths and the parsed report.
//! Port of `Artemis.Peripherals/Logitech/Hidpp/HidppReport.cs`.

/// HID++ report ids and lengths.
pub mod constants {
    pub const SHORT_REPORT_ID: u8 = 0x10;
    pub const LONG_REPORT_ID: u8 = 0x11;
    pub const VERY_LONG_REPORT_ID: u8 = 0x12;
    pub const SHORT_LENGTH: usize = 7;
    pub const LONG_LENGTH: usize = 20;
    pub const VERY_LONG_LENGTH: usize = 64;

    /// Device index of a directly connected (wired) HID++ 2.0 device.
    pub const WIRED_DEVICE_INDEX: u8 = 0xFF;

    /// HID++ 2.0 error sub id.
    pub const HIDPP20_ERROR_SUB_ID: u8 = 0xFF;

    /// HID++ 1.0 error sub id.
    pub const HIDPP10_ERROR_SUB_ID: u8 = 0x8F;

    /// Software id tagging our requests, so responses meant for other HID++ clients (G HUB) are
    /// ignored rather than mistaken for ours.
    pub const SOFTWARE_ID: u8 = 0x0A;

    /// Report length for an id, or `None` if the id is not a HID++ report.
    pub fn length_of(report_id: u8) -> Option<usize> {
        match report_id {
            SHORT_REPORT_ID => Some(SHORT_LENGTH),
            LONG_REPORT_ID => Some(LONG_LENGTH),
            VERY_LONG_REPORT_ID => Some(VERY_LONG_LENGTH),
            _ => None,
        }
    }
}

use constants::*;

/// Immutable parsed HID++ report. Only constructed through a path that has validated the report id
/// and length, so every accessor below can index without bounds checks.
#[derive(Clone, PartialEq, Eq)]
pub struct HidppReport {
    data: Vec<u8>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BuildError {
    #[error("HID++ function ids are 4 bits, got 0x{0:02X}")]
    FunctionTooLarge(u8),
    #[error("HID++ software ids are 4 bits, got 0x{0:02X}")]
    SoftwareIdTooLarge(u8),
    #[error("0x{0:02X} is not a HID++ report id")]
    BadReportId(u8),
    #[error("too many parameters ({count}) for report 0x{report_id:02X}")]
    TooManyParameters { count: usize, report_id: u8 },
}

impl HidppReport {
    pub fn report_id(&self) -> u8 {
        self.data[0]
    }

    pub fn device_index(&self) -> u8 {
        self.data[1]
    }

    /// Feature index (HID++ 2.0) or sub id (HID++ 1.0).
    pub fn feature_index(&self) -> u8 {
        self.data[2]
    }

    /// Function id (upper nibble of byte 3).
    pub fn function(&self) -> u8 {
        self.data[3] >> 4
    }

    /// Software id (lower nibble of byte 3). 0 for device-originated notifications.
    pub fn software_id(&self) -> u8 {
        self.data[3] & 0x0F
    }

    /// Byte 3 whole: `function | software_id` for HID++ 2.0, the register address for 1.0.
    pub fn address(&self) -> u8 {
        self.data[3]
    }

    pub fn parameters(&self) -> &[u8] {
        &self.data[4..]
    }

    pub fn raw(&self) -> &[u8] {
        &self.data
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn is_hidpp20_error(&self) -> bool {
        self.feature_index() == HIDPP20_ERROR_SUB_ID && self.data.len() >= 6
    }

    pub fn is_hidpp10_error(&self) -> bool {
        self.feature_index() == HIDPP10_ERROR_SUB_ID && self.data.len() >= 6
    }

    pub fn is_error(&self) -> bool {
        self.is_hidpp20_error() || self.is_hidpp10_error()
    }

    /// For error reports: the feature index / sub id the error refers to.
    pub fn error_target_index(&self) -> u8 {
        self.data[3]
    }

    /// For error reports: the `function|swid` / address byte the error refers to.
    pub fn error_target_address(&self) -> u8 {
        self.data[4]
    }

    pub fn error_code(&self) -> u8 {
        self.data[5]
    }

    /// Parameter byte by index, 0 past the end. HID++ pads short answers with zeros, so an
    /// out-of-range read is normal rather than a bug worth panicking over.
    pub fn param(&self, index: usize) -> u8 {
        self.data.get(index + 4).copied().unwrap_or(0)
    }

    /// HID++ is big-endian on the wire throughout.
    pub fn param_u16(&self, index: usize) -> u16 {
        ((self.param(index) as u16) << 8) | self.param(index + 1) as u16
    }

    /// Parses an input report. `None` if it is not a valid HID++ report (wrong id or truncated).
    /// Overlong input is truncated to the id's declared length: a 64-byte OS read buffer holding a
    /// 7-byte short report is the normal case, not an error.
    pub fn try_parse(data: &[u8]) -> Option<Self> {
        if data.len() < SHORT_LENGTH {
            return None;
        }
        let expected = constants::length_of(data[0])?;
        if data.len() < expected {
            return None;
        }
        Some(Self { data: data[..expected].to_vec() })
    }

    /// Builds a HID++ 2.0 feature request.
    pub fn create(
        device_index: u8,
        feature_index: u8,
        function: u8,
        software_id: u8,
        parameters: &[u8],
        report_id: u8,
    ) -> std::result::Result<Self, BuildError> {
        if function > 0x0F {
            return Err(BuildError::FunctionTooLarge(function));
        }
        if software_id > 0x0F {
            return Err(BuildError::SoftwareIdTooLarge(software_id));
        }
        let length = constants::length_of(report_id).ok_or(BuildError::BadReportId(report_id))?;
        if parameters.len() > length - 4 {
            return Err(BuildError::TooManyParameters { count: parameters.len(), report_id });
        }

        let mut data = vec![0u8; length];
        data[0] = report_id;
        data[1] = device_index;
        data[2] = feature_index;
        data[3] = (function << 4) | software_id;
        data[4..4 + parameters.len()].copy_from_slice(parameters);
        Ok(Self { data })
    }

    /// Builds a HID++ 1.0 register request (sub id + address), used for receiver registers.
    pub fn create_register(
        device_index: u8,
        sub_id: u8,
        address: u8,
        parameters: &[u8],
        report_id: u8,
    ) -> std::result::Result<Self, BuildError> {
        let length = constants::length_of(report_id).ok_or(BuildError::BadReportId(report_id))?;
        if parameters.len() > length - 4 {
            return Err(BuildError::TooManyParameters { count: parameters.len(), report_id });
        }
        let mut data = vec![0u8; length];
        data[0] = report_id;
        data[1] = device_index;
        data[2] = sub_id;
        data[3] = address;
        data[4..4 + parameters.len()].copy_from_slice(parameters);
        Ok(Self { data })
    }

    /// Re-frames this report under a different report id, padding or truncating the body.
    pub fn with_report_id(&self, report_id: u8) -> std::result::Result<Self, BuildError> {
        let length = constants::length_of(report_id).ok_or(BuildError::BadReportId(report_id))?;
        let mut data = vec![0u8; length];
        data[0] = report_id;
        let copy = self.data.len().min(length) - 1;
        data[1..1 + copy].copy_from_slice(&self.data[1..1 + copy]);
        Ok(Self { data })
    }
}

impl std::fmt::Display for HidppReport {
    /// Uppercase hex, matching the C# `Convert.ToHexString` the protocol log and fixtures use.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in &self.data {
            write!(f, "{b:02X}")?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for HidppReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HidppReport({self})")
    }
}

/// Parses the uppercase hex the fixtures and protocol logs are written in.
pub fn from_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ProtocolError;

    fn report(hex: &str) -> HidppReport {
        HidppReport::try_parse(&from_hex(hex).expect("bad hex")).expect("not a HID++ report")
    }

    /// Ported from `HidppParsingTests.Report_RejectsUnknownIdAndTruncatedData`.
    #[test]
    fn rejects_unknown_id_and_truncated_data() {
        assert!(HidppReport::try_parse(&[0x20, 0, 0, 0, 0, 0, 0]).is_none(), "0x20 is not a HID++ id");
        // Long id but only 8 bytes.
        assert!(HidppReport::try_parse(&[0x11, 0xFF, 0, 0, 0, 0, 0, 0]).is_none());
        assert!(HidppReport::try_parse(&[]).is_none());
        assert!(HidppReport::try_parse(&[0x10, 0x01, 0x00, 0x1A, 0x04, 0x02, 0x5A]).is_some());
    }

    /// Ported from `HidppParsingTests.Report_TruncatesOverlongInput_ToDeclaredLength`.
    #[test]
    fn truncates_overlong_input_to_declared_length() {
        let mut data = vec![0u8; 64];
        data[0] = SHORT_REPORT_ID;
        assert_eq!(HidppReport::try_parse(&data).unwrap().len(), 7);
    }

    /// Ported from `HidppParsingTests.Create_EncodesFunctionAndSoftwareId`.
    #[test]
    fn create_encodes_function_and_software_id() {
        let r = HidppReport::create(0xFF, 0x0B, 0x3, 0x0A, &[1, 2, 3], SHORT_REPORT_ID).unwrap();
        assert_eq!(r.to_string(), "10FF0B3A010203");
        assert_eq!(r.function(), 3);
        assert_eq!(r.software_id(), 0x0A);

        assert_eq!(
            HidppReport::create(1, 1, 0x10, 0, &[], SHORT_REPORT_ID),
            Err(BuildError::FunctionTooLarge(0x10))
        );
        assert_eq!(
            HidppReport::create(1, 1, 1, 0, &[0; 4], SHORT_REPORT_ID),
            Err(BuildError::TooManyParameters { count: 4, report_id: SHORT_REPORT_ID })
        );
    }

    /// Ported from `HidppParsingTests.ErrorReports_AreClassified`. Both vectors are as observed.
    #[test]
    fn error_reports_are_classified() {
        let h20 = report("11FFFF0B3A08000000000000000000000000000000");
        assert!(h20.is_hidpp20_error());
        assert!(ProtocolError::from_report(&h20).is_transient(), "0x08 is Busy");

        let h10 = report("10028F001A0800");
        let e10 = ProtocolError::from_report(&h10);
        assert!(e10.is_unknown_device(), "0x08 is UnknownDevice on HID++ 1.0");
        assert!(!e10.is_hidpp20);
    }

    #[test]
    fn big_endian_parameters() {
        // A DPI answer: sensor 1, 0x0320 = 800. 20 bytes, as a long report must be.
        let r = report("11FF0B3A01032000000000000000000000000000");
        assert_eq!(r.param(0), 0x01);
        assert_eq!(r.param_u16(1), 800);
        // Past the end reads as zero rather than panicking.
        assert_eq!(r.param(200), 0);
    }

    #[test]
    fn with_report_id_reframes_without_losing_the_header() {
        let short = HidppReport::create(0xFF, 0x0B, 0x3, 0x0A, &[1, 2, 3], SHORT_REPORT_ID).unwrap();
        let long = short.with_report_id(LONG_REPORT_ID).unwrap();
        assert_eq!(long.len(), LONG_LENGTH);
        assert_eq!(long.report_id(), LONG_REPORT_ID);
        assert_eq!(long.device_index(), 0xFF);
        assert_eq!(long.feature_index(), 0x0B);
        assert_eq!(long.address(), short.address());
        assert_eq!(&long.parameters()[..3], &[1, 2, 3]);
    }
}
