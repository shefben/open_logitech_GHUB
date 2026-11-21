//! HID transport abstractions. Port of `Artemis.Peripherals/Hid/HidAbstractions.cs`.
//!
//! A Logitech device exposes several HID top-level collections on one USB interface, and on
//! Windows each collection gets its own device path. HID++ traffic goes to the vendor-defined
//! collections (usage page >= 0xFF00), one of which takes 7-byte short reports and another 20-byte
//! long reports. Grouping collections back into their interface is therefore the first job of
//! enumeration, and [`group_key`] is what does it.

use std::fmt::Debug;
use std::sync::Arc;

/// One HID top-level collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidInterfaceInfo {
    pub path: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub product_name: Option<String>,
    pub serial_number: Option<String>,
    pub max_input_report_length: usize,
    pub max_output_report_length: usize,
    pub usage_page: u16,
    pub usage: u16,
    /// USB interface number (`MI_xx`) when the platform exposes it.
    pub interface_number: Option<u8>,
    /// Key shared by all collections of the same USB interface instance.
    pub group_key: String,
}

impl HidInterfaceInfo {
    /// HID++ lives on the vendor-defined pages; everything below 0xFF00 is a standard
    /// keyboard/mouse/consumer collection we must not write to.
    pub fn is_vendor_defined(&self) -> bool {
        self.usage_page >= 0xFF00
    }

    /// Whether this collection actually speaks HID++.
    ///
    /// Being vendor-defined is necessary but not sufficient: a Logitech receiver also exposes
    /// small vendor collections for things like its connect button, and a real C539 receiver on
    /// this machine enumerates a 2-byte one on page 0xFFBC. Port of the filter in
    /// `LogitechHidppProvider.cs`: the report must be exactly short or long. Very long (64-byte)
    /// traffic is reached by re-framing on the long collection rather than by a collection of its
    /// own, so it is deliberately not listed here.
    pub fn is_hidpp(&self) -> bool {
        self.is_vendor_defined()
            && matches!(
                self.max_output_report_length,
                crate::report::constants::SHORT_LENGTH | crate::report::constants::LONG_LENGTH
            )
    }
}

/// An open HID collection.
///
/// Implementations must tolerate one reader thread and one writer thread at the same time, which
/// is how [`crate::channel::HidppChannel`] drives them.
pub trait HidConnection: Send + Debug {
    fn info(&self) -> &HidInterfaceInfo;

    fn write(&self, report: &[u8]) -> std::io::Result<()>;

    /// Reads one input report into `buffer`. `Ok(0)` on timeout; `Err` on disconnect.
    fn read(&self, buffer: &mut [u8], timeout_ms: u32) -> std::io::Result<usize>;
}

pub trait HidBackend: Send + Sync {
    fn enumerate(&self, vendor_id: u16) -> std::io::Result<Vec<HidInterfaceInfo>>;

    /// Opens a collection. `Arc` rather than `Box` because a channel shares each connection
    /// between its reader thread and the request path, and `Sync` is what makes that sound.
    fn open(&self, info: &HidInterfaceInfo) -> std::io::Result<Arc<dyn HidConnection + Sync>>;
}

/// Logitech's USB vendor id.
pub const LOGITECH_VID: u16 = 0x046D;

/// Builds a grouping key from a Windows HID path such as
/// `\\?\hid#vid_046d&pid_c539&mi_02&col01#8&38ddd24c&0&0000#{guid}`, collapsing the collection
/// suffix (`&colNN`) and the collection ordinal (last `&NNNN`) so every collection of one USB
/// interface instance maps to the same key.
pub fn group_key(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    let parts: Vec<&str> = lower.split('#').collect();
    if parts.len() < 3 || !parts[1].contains("vid_") {
        return lower;
    }

    let mut hardware = parts[1];
    if let Some(col) = hardware.find("&col") {
        hardware = &hardware[..col];
    }

    let mut instance = parts[2];
    if let Some(last_amp) = instance.rfind('&') {
        if last_amp > 0 {
            instance = &instance[..last_amp];
        }
    }
    format!("{hardware}#{instance}")
}

/// The USB interface number out of a Windows HID path (`&mi_02` -> 2), when present.
pub fn interface_number(path: &str) -> Option<u8> {
    let lower = path.to_ascii_lowercase();
    let mi = lower.find("&mi_")?;
    let digits = lower.get(mi + 4..mi + 6)?;
    u8::from_str_radix(digits, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const COL01: &str = r"\\?\hid#vid_046d&pid_c539&mi_02&col01#8&38ddd24c&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";
    const COL02: &str = r"\\?\hid#vid_046d&pid_c539&mi_02&col02#8&38ddd24c&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}";
    const OTHER_INTERFACE: &str = r"\\?\hid#vid_046d&pid_c539&mi_01&col01#8&38ddd24c&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";

    #[test]
    fn collections_of_one_interface_share_a_group_key() {
        assert_eq!(group_key(COL01), group_key(COL02));
        assert_ne!(group_key(COL01), group_key(OTHER_INTERFACE));
    }

    #[test]
    fn group_key_is_case_insensitive() {
        assert_eq!(group_key(&COL01.to_uppercase()), group_key(COL01));
    }

    #[test]
    fn unparseable_paths_group_by_themselves() {
        // A Linux hidraw path or anything else without vid_: must not collapse unrelated devices
        // into one group.
        assert_eq!(group_key("/dev/hidraw3"), "/dev/hidraw3");
        assert_ne!(group_key("/dev/hidraw3"), group_key("/dev/hidraw4"));
    }

    #[test]
    fn interface_number_is_read_as_hex() {
        assert_eq!(interface_number(COL01), Some(2));
        assert_eq!(interface_number(OTHER_INTERFACE), Some(1));
        assert_eq!(interface_number("/dev/hidraw3"), None);
        // mi_0a is interface 10, not 0.
        assert_eq!(
            interface_number(r"\\?\hid#vid_046d&pid_c539&mi_0a&col01#8&x&0&0000#{g}"),
            Some(10)
        );
    }

    #[test]
    fn vendor_pages_are_recognised() {
        let mut info = HidInterfaceInfo {
            path: COL01.into(),
            vendor_id: LOGITECH_VID,
            product_id: 0xC539,
            product_name: None,
            serial_number: None,
            max_input_report_length: 20,
            max_output_report_length: 20,
            usage_page: 0xFF43,
            usage: 0x0202,
            interface_number: Some(2),
            group_key: group_key(COL01),
        };
        assert!(info.is_vendor_defined(), "0xFF43 is the HID++ long page");
        // The standard mouse collection on the same device must not be written to.
        info.usage_page = 0x0001;
        assert!(!info.is_vendor_defined());
    }
}
