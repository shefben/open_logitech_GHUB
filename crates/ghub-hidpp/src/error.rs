//! HID++ error classification. Port of `Artemis.Peripherals/Logitech/Hidpp/HidppException.cs`.

use crate::report::HidppReport;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Hidpp20Error {
    NoError = 0x00,
    Unknown = 0x01,
    InvalidArgument = 0x02,
    OutOfRange = 0x03,
    HardwareError = 0x04,
    LogitechInternal = 0x05,
    InvalidFeatureIndex = 0x06,
    InvalidFunctionId = 0x07,
    Busy = 0x08,
    Unsupported = 0x09,
}

impl Hidpp20Error {
    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0x00 => Self::NoError,
            0x01 => Self::Unknown,
            0x02 => Self::InvalidArgument,
            0x03 => Self::OutOfRange,
            0x04 => Self::HardwareError,
            0x05 => Self::LogitechInternal,
            0x06 => Self::InvalidFeatureIndex,
            0x07 => Self::InvalidFunctionId,
            0x08 => Self::Busy,
            0x09 => Self::Unsupported,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Hidpp10Error {
    Success = 0x00,
    InvalidSubId = 0x01,
    InvalidAddress = 0x02,
    InvalidValue = 0x03,
    ConnectFail = 0x04,
    TooManyDevices = 0x05,
    AlreadyExists = 0x06,
    Busy = 0x07,
    UnknownDevice = 0x08,
    ResourceError = 0x09,
    RequestUnavailable = 0x0A,
    InvalidParamValue = 0x0B,
    WrongPinCode = 0x0C,
}

impl Hidpp10Error {
    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0x00 => Self::Success,
            0x01 => Self::InvalidSubId,
            0x02 => Self::InvalidAddress,
            0x03 => Self::InvalidValue,
            0x04 => Self::ConnectFail,
            0x05 => Self::TooManyDevices,
            0x06 => Self::AlreadyExists,
            0x07 => Self::Busy,
            0x08 => Self::UnknownDevice,
            0x09 => Self::ResourceError,
            0x0A => Self::RequestUnavailable,
            0x0B => Self::InvalidParamValue,
            0x0C => Self::WrongPinCode,
            _ => return None,
        })
    }
}

/// The device answered with a protocol error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolError {
    pub is_hidpp20: bool,
    pub code: u8,
    /// The feature index / sub id the error refers to.
    pub target_index: u8,
}

impl ProtocolError {
    /// Worth retrying: the device is busy, not wrong.
    pub fn is_transient(&self) -> bool {
        if self.is_hidpp20 {
            self.code == Hidpp20Error::Busy as u8
        } else {
            self.code == Hidpp10Error::Busy as u8
        }
    }

    /// Device index absent behind a receiver (paired device asleep or slot empty).
    pub fn is_unknown_device(&self) -> bool {
        !self.is_hidpp20
            && matches!(
                self.code,
                c if c == Hidpp10Error::UnknownDevice as u8 || c == Hidpp10Error::ResourceError as u8
            )
    }

    pub fn from_report(report: &HidppReport) -> Self {
        Self {
            is_hidpp20: report.is_hidpp20_error(),
            code: report.error_code(),
            target_index: report.error_target_index(),
        }
    }
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_hidpp20 {
            match Hidpp20Error::from_code(self.code) {
                Some(e) => write!(
                    f,
                    "HID++ 2.0 error {e:?} (0x{:02X}) for feature index 0x{:02X}",
                    self.code, self.target_index
                ),
                None => write!(
                    f,
                    "HID++ 2.0 error 0x{:02X} for feature index 0x{:02X}",
                    self.code, self.target_index
                ),
            }
        } else {
            match Hidpp10Error::from_code(self.code) {
                Some(e) => write!(
                    f,
                    "HID++ 1.0 error {e:?} (0x{:02X}) for sub id 0x{:02X}",
                    self.code, self.target_index
                ),
                None => write!(
                    f,
                    "HID++ 1.0 error 0x{:02X} for sub id 0x{:02X}",
                    self.code, self.target_index
                ),
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HidppError {
    #[error("{0}")]
    Protocol(ProtocolError),

    /// No answer within the timeout after all retries.
    #[error("{channel}: no response to {request} after {attempts} attempt(s)")]
    Timeout { channel: String, request: String, attempts: u32 },

    /// The transport went away (unplugged).
    #[error("{channel}: disconnected ({reason})")]
    Disconnected { channel: String, reason: String },

    /// A response arrived but failed validation (length / echo mismatch).
    #[error("malformed response: {0}")]
    Malformed(String),

    /// The request cannot be expressed on this device's collections.
    #[error("{0}")]
    Unsupported(String),

    #[error("HID error: {0}")]
    Hid(String),
}

impl HidppError {
    /// True when a caller should re-enumerate rather than retry.
    pub fn is_disconnected(&self) -> bool {
        matches!(self, Self::Disconnected { .. })
    }

    pub fn as_protocol(&self) -> Option<&ProtocolError> {
        match self {
            Self::Protocol(e) => Some(e),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, HidppError>;
