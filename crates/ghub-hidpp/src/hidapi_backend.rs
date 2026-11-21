//! Real HID backend over `hidapi`. Port of `Artemis.Peripherals/Hid/HidSharpBackend.cs`.
//!
//! Three properties carry over from the C# and are the reason this file is more than a thin
//! wrapper:
//!
//! 1. **Shared, never exclusive.** The C# sets `OpenOption.Exclusive, false` so G HUB keeps
//!    working while we are attached. `hidapi`'s `windows-native` backend opens with
//!    `FILE_SHARE_READ | FILE_SHARE_WRITE`, which is the same thing.
//! 2. **Vendor collections only.** A Logitech device exposes its boot keyboard and mouse
//!    collections on the same interface. Writing to those would inject input, so enumeration
//!    marks them and [`crate::channel`] only ever opens vendor-defined ones.
//! 3. **Report lengths come from the descriptor.** `hidapi` does not expose
//!    `caps.InputReportByteLength`, which the channel needs to tell the short-report collection
//!    from the long-report one. It does expose the raw report descriptor, so [`crate::descriptor`]
//!    recovers the lengths from that instead of guessing.

use std::ffi::CString;
use std::sync::{Arc, Mutex};

use hidapi::{DeviceInfo, HidApi, HidDevice};

use crate::descriptor;
use crate::hid::{HidBackend, HidConnection, HidInterfaceInfo, group_key, interface_number};
use crate::report::constants;

/// HID backend over `hidapi`.
pub struct HidApiBackend {
    /// `hidapi::HidApi` is `Send` but not `Sync`, and its device-list cache is `&mut`-refreshed,
    /// so the context is owned behind a lock. It is held only across enumeration and open, never
    /// across a read or a write.
    api: Mutex<HidApi>,
}

impl std::fmt::Debug for HidApiBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HidApiBackend")
    }
}

impl HidApiBackend {
    pub fn new() -> std::io::Result<Self> {
        let api = HidApi::new().map_err(to_io)?;
        Ok(Self { api: Mutex::new(api) })
    }

    /// Re-scans the bus. Call this on a device-arrival notification; `enumerate` reads the cache
    /// that this fills.
    pub fn refresh(&self) -> std::io::Result<()> {
        let mut api = self.api.lock().expect("hidapi lock poisoned");
        api.refresh_devices().map_err(to_io)
    }
}

impl HidBackend for HidApiBackend {
    fn enumerate(&self, vendor_id: u16) -> std::io::Result<Vec<HidInterfaceInfo>> {
        let api = self.api.lock().expect("hidapi lock poisoned");
        let mut result = Vec::new();
        for device in api.device_list().filter(|d| d.vendor_id() == vendor_id) {
            // A collection we cannot describe is skipped rather than fatal, matching the C#'s
            // `catch { }`: one unreadable collection must not hide the rest of the device.
            match describe(&api, device) {
                Some(info) => result.push(info),
                None => log::debug!(
                    "skipping HID collection {} (could not be described)",
                    device.path().to_string_lossy()
                ),
            }
        }
        Ok(result)
    }

    fn open(&self, info: &HidInterfaceInfo) -> std::io::Result<Arc<dyn HidConnection + Sync>> {
        if !info.is_vendor_defined() {
            // Guard rail, not politeness: writing a report to the boot keyboard collection would
            // inject keystrokes into whatever has focus.
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "refusing to open non-vendor HID collection (usage page 0x{:04X}) at {}",
                    info.usage_page, info.path
                ),
            ));
        }

        let path = c_path(&info.path)?;
        let api = self.api.lock().expect("hidapi lock poisoned");

        // Two handles to the same path, one per direction. The reader thread parks in a 100 ms
        // `read_timeout` and the Windows backend leaves that overlapped read pending between
        // calls, so sharing one handle would both serialise writes behind the poll and interleave
        // two users of the same pending read. Shared-mode opens make a second handle legal.
        let reader = api.open_path(&path).map_err(to_io)?;
        let handles = match api.open_path(&path) {
            Ok(writer) => Handles::Split { reader: Mutex::new(reader), writer: Mutex::new(writer) },
            Err(e) => {
                log::warn!(
                    "{}: only one handle available ({e}); writes will wait on the read poll",
                    info.path
                );
                Handles::Shared(Mutex::new(reader))
            }
        };
        drop(api);

        Ok(Arc::new(HidApiConnection { info: info.clone(), handles }))
    }
}

/// Builds the interface description for one collection.
///
/// `usage_page` and `usage` come free from enumeration. The report lengths do not, so a
/// vendor-defined collection is opened briefly to read its descriptor; standard collections are
/// left alone, since we never open them and so never need their sizes.
fn describe(api: &HidApi, device: &DeviceInfo) -> Option<HidInterfaceInfo> {
    let path = device.path().to_string_lossy().into_owned();
    let mut info = HidInterfaceInfo {
        vendor_id: device.vendor_id(),
        product_id: device.product_id(),
        product_name: device.product_string().map(str::to_owned),
        serial_number: device.serial_number().map(str::to_owned),
        max_input_report_length: 0,
        max_output_report_length: 0,
        usage_page: device.usage_page(),
        usage: device.usage(),
        interface_number: interface_number(&path)
            .or_else(|| u8::try_from(device.interface_number()).ok()),
        group_key: group_key(&path),
        path,
    };

    if info.is_vendor_defined() {
        let (input, output) = report_lengths(api, device, &info);
        info.max_input_report_length = input;
        info.max_output_report_length = output;
    }

    Some(info)
}

/// Max input/output report length for a vendor collection, from its report descriptor where that
/// can be read and from the Logitech usage convention where it cannot.
fn report_lengths(api: &HidApi, device: &DeviceInfo, info: &HidInterfaceInfo) -> (usize, usize) {
    if let Some(parsed) = read_descriptor(api, device) {
        if parsed.max_input_report_length > 0 {
            return (
                parsed.max_input_report_length,
                // A collection with no output report declared is still written to via the control
                // endpoint, so fall back to the input length rather than to zero.
                if parsed.max_output_report_length > 0 {
                    parsed.max_output_report_length
                } else {
                    parsed.max_input_report_length
                },
            );
        }
    }

    // Fallback, used only when the descriptor cannot be read at all.
    //
    // On page 0xFF00 - a receiver - Logitech's convention is usage 1 = short, 2 = long, and this
    // machine's C539 receiver enumerates exactly that. The convention does NOT generalise: the
    // directly attached G815 on this machine puts its 20-byte collection on page 0xFF43 usage
    // 0x0602 and a 64-byte one on usage 0x0604, so guessing from usage alone would mis-size both.
    //
    // Anything the convention does not cover therefore returns zero, which makes
    // `HidInterfaceInfo::is_hidpp` false and leaves the collection unused. Declining to drive a
    // device is recoverable; framing a write wrongly and sending it to the device is not.
    let guess = match (info.usage_page, info.usage) {
        (0xFF00, 0x0001) => constants::SHORT_LENGTH,
        (0xFF00, 0x0002) => constants::LONG_LENGTH,
        _ => 0,
    };
    if guess == 0 {
        log::warn!(
            "{}: report descriptor unavailable and usage 0x{:04X}:0x{:04X} is not a known HID++ \
             collection shape; leaving it unused",
            info.path,
            info.usage_page,
            info.usage
        );
    } else {
        log::warn!(
            "{}: report descriptor unavailable; assuming {guess}-byte reports from the receiver \
             usage convention (not hardware-verified)",
            info.path
        );
    }
    (guess, guess)
}

fn read_descriptor(api: &HidApi, device: &DeviceInfo) -> Option<descriptor::ParsedDescriptor> {
    let handle = device.open_device(api).ok()?;
    // 4 KiB is well past any real HID descriptor; HID++ ones are a few dozen bytes.
    let mut buffer = [0u8; 4096];
    let read = handle.get_report_descriptor(&mut buffer).ok()?;
    match descriptor::parse(&buffer[..read]) {
        Ok(parsed) => Some(parsed),
        Err(e) => {
            log::warn!("{}: unparseable report descriptor: {e}", device.path().to_string_lossy());
            None
        }
    }
}

enum Handles {
    Split { reader: Mutex<HidDevice>, writer: Mutex<HidDevice> },
    /// One handle for both directions, used only when a second open fails.
    Shared(Mutex<HidDevice>),
}

struct HidApiConnection {
    info: HidInterfaceInfo,
    handles: Handles,
}

impl std::fmt::Debug for HidApiConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "HidApiConnection({:04X}:{:04X} {})",
            self.info.vendor_id, self.info.product_id, self.info.path
        )
    }
}

impl HidConnection for HidApiConnection {
    fn info(&self) -> &HidInterfaceInfo {
        &self.info
    }

    fn write(&self, report: &[u8]) -> std::io::Result<()> {
        let guard = match &self.handles {
            Handles::Split { writer, .. } => writer,
            Handles::Shared(shared) => shared,
        };
        let device = guard.lock().expect("HID write lock poisoned");
        // `hidapi` pads a short buffer up to the collection's output report length itself, so the
        // caller passes exactly the HID++ report and nothing more.
        device.write(report).map(|_| ()).map_err(to_io)
    }

    fn read(&self, buffer: &mut [u8], timeout_ms: u32) -> std::io::Result<usize> {
        let guard = match &self.handles {
            Handles::Split { reader, .. } => reader,
            Handles::Shared(shared) => shared,
        };
        let device = guard.lock().expect("HID read lock poisoned");

        // Read into a buffer at least as long as the collection's longest input report. Handing
        // `hidapi` a shorter one on Windows would leave the tail of a long report in the pending
        // overlapped read and desynchronise every later answer.
        let wanted = self.info.max_input_report_length.max(buffer.len()).max(constants::SHORT_LENGTH);
        let timeout = i32::try_from(timeout_ms).unwrap_or(i32::MAX);

        if wanted <= buffer.len() {
            return device.read_timeout(buffer, timeout).map_err(to_io);
        }

        let mut scratch = vec![0u8; wanted];
        let count = device.read_timeout(&mut scratch, timeout).map_err(to_io)?;
        let copy = count.min(buffer.len());
        buffer[..copy].copy_from_slice(&scratch[..copy]);
        Ok(copy)
    }
}

fn c_path(path: &str) -> std::io::Result<CString> {
    CString::new(path).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "HID path contains a NUL byte")
    })
}

/// `hidapi` errors carry no `io::ErrorKind`, so a disconnect and a permission failure arrive
/// looking the same. [`crate::channel`] only needs to know "the device stopped answering", which
/// `Other` conveys; the message is preserved for the log.
fn to_io(e: hidapi::HidError) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hid::LOGITECH_VID;

    /// Enumeration against whatever is actually plugged in. This is the one test in the crate that
    /// touches real hardware, so it asserts only what must hold on any machine, and reports what
    /// it found for the hardware-verification record.
    #[test]
    fn enumeration_describes_logitech_collections() {
        let backend = match HidApiBackend::new() {
            Ok(b) => b,
            Err(e) => {
                eprintln!("hidapi unavailable on this machine ({e}); skipping");
                return;
            }
        };

        let found = backend.enumerate(LOGITECH_VID).expect("enumeration failed");
        // Print the whole listing before asserting anything, so a failure arrives with the full
        // picture of the machine rather than the first few rows.
        eprintln!("{} Logitech HID collections", found.len());
        for info in &found {
            eprintln!(
                "  {:04X}:{:04X} page {:04X} usage {:04X} in {} out {} mi {:?} hidpp {} {:?}",
                info.vendor_id,
                info.product_id,
                info.usage_page,
                info.usage,
                info.max_input_report_length,
                info.max_output_report_length,
                info.interface_number,
                info.is_hidpp(),
                info.product_name
            );
        }

        for info in &found {
            assert_eq!(info.vendor_id, LOGITECH_VID);
            assert!(!info.group_key.is_empty());
            if info.is_hidpp() {
                // A HID++ collection's input and output lengths must agree, since the channel
                // sizes its read buffer from one and frames its writes to the other.
                assert_eq!(
                    info.max_input_report_length, info.max_output_report_length,
                    "asymmetric HID++ collection {}",
                    info.path
                );
                assert!(matches!(
                    info.max_input_report_length,
                    constants::SHORT_LENGTH | constants::LONG_LENGTH
                ));
            }
        }

        // Collections of one interface must share a group key.
        for info in &found {
            let siblings = found.iter().filter(|o| o.group_key == info.group_key).count();
            assert!(siblings >= 1);
        }
    }

    #[test]
    fn opening_a_standard_collection_is_refused() {
        let backend = match HidApiBackend::new() {
            Ok(b) => b,
            Err(_) => return,
        };
        let keyboard = HidInterfaceInfo {
            path: r"\\?\hid#vid_046d&pid_c33f&mi_00#8&x&0&0000#{g}".into(),
            vendor_id: LOGITECH_VID,
            product_id: 0xC33F,
            product_name: None,
            serial_number: None,
            max_input_report_length: 8,
            max_output_report_length: 1,
            usage_page: 0x0001,
            usage: 0x0006,
            interface_number: Some(0),
            group_key: "x".into(),
        };
        let err = backend.open(&keyboard).expect_err("must refuse the boot keyboard collection");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    }
}
