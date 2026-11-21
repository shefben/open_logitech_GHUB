//! Minimal HID report descriptor parser.
//!
//! This exists because `hidapi` does not expose `caps.InputReportByteLength` /
//! `caps.OutputReportByteLength`, which is what the C# got from HidSharp's `ReportDescriptor` and
//! what [`crate::channel`] needs in order to tell a device's short-report collection from its
//! long-report one. `hidapi` *does* expose the raw report descriptor on every platform we target,
//! so the lengths are recovered here rather than guessed from the Logitech usage convention.
//!
//! Only what that decision needs is parsed: the first top-level usage, and the longest input and
//! output report declared. Logical ranges, strings, physical descriptors and designators are
//! walked over. The parser never writes to a device, so a descriptor it misreads can at worst
//! make a collection look like the wrong size, never make us send a malformed report to hardware.

/// Report lengths and top-level usage recovered from a report descriptor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedDescriptor {
    /// Usage page of the first top-level collection (0 when the descriptor declares none).
    pub usage_page: u16,
    /// Usage id of the first top-level collection.
    pub usage: u16,
    /// Longest input report including its leading report-id byte, matching the Windows
    /// `InputReportByteLength` convention the C# relied on.
    pub max_input_report_length: usize,
    /// Longest output report including its leading report-id byte.
    pub max_output_report_length: usize,
    /// Every report id this collection declares, ascending. Empty for an unnumbered collection.
    pub report_ids: Vec<u8>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DescriptorError {
    #[error("report descriptor ends mid-item at byte {0}")]
    Truncated(usize),
    #[error("report descriptor declares a {0}-byte report, which is not a HID report size")]
    AbsurdReportSize(usize),
}

/// Main-item tags we care about.
const TAG_INPUT: u8 = 0x8;
const TAG_OUTPUT: u8 = 0x9;
const TAG_FEATURE: u8 = 0xB;
const TAG_COLLECTION: u8 = 0xA;
const TAG_END_COLLECTION: u8 = 0xC;

/// Global-item tags.
const TAG_USAGE_PAGE: u8 = 0x0;
const TAG_REPORT_SIZE: u8 = 0x7;
const TAG_REPORT_ID: u8 = 0x8;
const TAG_REPORT_COUNT: u8 = 0x9;
const TAG_PUSH: u8 = 0xA;
const TAG_POP: u8 = 0xB;

/// Local-item tag.
const TAG_USAGE: u8 = 0x0;

const TYPE_MAIN: u8 = 0;
const TYPE_GLOBAL: u8 = 1;
const TYPE_LOCAL: u8 = 2;

/// A report is at most 64 bytes of payload on full-speed HID; anything an order of magnitude past
/// that is a misparse, and we would rather say so than hand the channel a nonsense buffer size.
const SANE_REPORT_BYTES: usize = 1024;

#[derive(Clone, Copy, Default)]
struct GlobalState {
    usage_page: u16,
    report_size: u32,
    report_count: u32,
    report_id: u8,
}

/// Bit totals per report id, for one report kind.
struct BitTally {
    /// Index 0 holds the unnumbered report; 1..=255 hold numbered ones.
    bits: [u64; 256],
}

impl Default for BitTally {
    fn default() -> Self {
        // `derive(Default)` stops at 32-element arrays.
        Self { bits: [0; 256] }
    }
}

impl BitTally {
    fn add(&mut self, report_id: u8, bits: u64) {
        self.bits[report_id as usize] = self.bits[report_id as usize].saturating_add(bits);
    }

    /// Longest report in bytes, including the report-id byte. Zero when nothing was declared.
    fn max_bytes(&self) -> usize {
        let widest = self.bits.iter().copied().max().unwrap_or(0);
        if widest == 0 {
            return 0;
        }
        // Round the payload up to whole bytes, then add the leading report-id byte. Windows counts
        // that byte even for an unnumbered collection, where it reads back as zero.
        (widest as usize).div_ceil(8) + 1
    }
}

/// Parses a report descriptor as returned by `hidapi`'s `get_report_descriptor`.
pub fn parse(bytes: &[u8]) -> Result<ParsedDescriptor, DescriptorError> {
    let mut global = GlobalState::default();
    let mut stack: Vec<GlobalState> = Vec::new();
    // First usage seen since the last main item, as a 32-bit extended usage.
    let mut pending_usage: Option<u32> = None;
    let mut depth: u32 = 0;
    let mut top_level: Option<(u16, u16)> = None;
    let mut input = BitTally::default();
    let mut output = BitTally::default();
    let mut report_ids: Vec<u8> = Vec::new();

    let mut i = 0usize;
    while i < bytes.len() {
        let prefix = bytes[i];
        i += 1;

        // 0xFE introduces a long item, which no HID++ device uses but which we must step over
        // correctly rather than desynchronise on.
        if prefix == 0xFE {
            let size = *bytes.get(i).ok_or(DescriptorError::Truncated(i))? as usize;
            // data size byte + tag byte + payload
            i = i
                .checked_add(2 + size)
                .filter(|end| *end <= bytes.len())
                .ok_or(DescriptorError::Truncated(i))?;
            continue;
        }

        let tag = prefix >> 4;
        let item_type = (prefix >> 2) & 0x3;
        let data_len = match prefix & 0x3 {
            0 => 0,
            1 => 1,
            2 => 2,
            _ => 4,
        };
        let end = i + data_len;
        if end > bytes.len() {
            return Err(DescriptorError::Truncated(i));
        }
        let data = &bytes[i..end];
        i = end;

        // Items are little-endian on the wire, unlike HID++ payloads.
        let value = data
            .iter()
            .enumerate()
            .fold(0u32, |acc, (n, b)| acc | ((*b as u32) << (8 * n)));

        match item_type {
            TYPE_GLOBAL => match tag {
                TAG_USAGE_PAGE => global.usage_page = value as u16,
                TAG_REPORT_SIZE => global.report_size = value,
                TAG_REPORT_COUNT => global.report_count = value,
                TAG_REPORT_ID => {
                    global.report_id = value as u8;
                    if global.report_id != 0 && !report_ids.contains(&global.report_id) {
                        report_ids.push(global.report_id);
                    }
                }
                TAG_PUSH => stack.push(global),
                TAG_POP => {
                    if let Some(popped) = stack.pop() {
                        global = popped;
                    }
                }
                _ => {}
            },
            TYPE_LOCAL => {
                if tag == TAG_USAGE && pending_usage.is_none() {
                    // A 4-byte usage carries its own page in the high word; a 1- or 2-byte one
                    // takes the page from global state.
                    pending_usage = Some(if data_len == 4 {
                        value
                    } else {
                        ((global.usage_page as u32) << 16) | (value & 0xFFFF)
                    });
                }
            }
            TYPE_MAIN => {
                match tag {
                    TAG_COLLECTION => {
                        if depth == 0 && top_level.is_none() {
                            let raw = pending_usage
                                .unwrap_or((global.usage_page as u32) << 16);
                            top_level = Some(((raw >> 16) as u16, (raw & 0xFFFF) as u16));
                        }
                        depth += 1;
                    }
                    TAG_END_COLLECTION => depth = depth.saturating_sub(1),
                    TAG_INPUT | TAG_OUTPUT | TAG_FEATURE => {
                        let bits = (global.report_size as u64) * (global.report_count as u64);
                        match tag {
                            TAG_INPUT => input.add(global.report_id, bits),
                            TAG_OUTPUT => output.add(global.report_id, bits),
                            // Feature reports go over the control endpoint and never carry HID++,
                            // so they are counted in neither length.
                            _ => {}
                        }
                    }
                    _ => {}
                }
                // Every main item clears local state, usage included.
                pending_usage = None;
            }
            _ => {}
        }
    }

    let (usage_page, usage) = top_level.unwrap_or((0, 0));
    let max_input_report_length = input.max_bytes();
    let max_output_report_length = output.max_bytes();
    for len in [max_input_report_length, max_output_report_length] {
        if len > SANE_REPORT_BYTES {
            return Err(DescriptorError::AbsurdReportSize(len));
        }
    }
    report_ids.sort_unstable();

    Ok(ParsedDescriptor {
        usage_page,
        usage,
        max_input_report_length,
        max_output_report_length,
        report_ids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Logitech HID++ short collection: vendor page 0xFF00, usage 1, report id 0x10 with six
    /// bytes in and six out. This is the shape every HID++ 1.0-capable device exposes.
    const SHORT_COLLECTION: &[u8] = &[
        0x06, 0x00, 0xFF, // Usage Page (0xFF00)
        0x09, 0x01, // Usage (1)
        0xA1, 0x01, // Collection (Application)
        0x85, 0x10, //   Report ID (0x10)
        0x75, 0x08, //   Report Size (8)
        0x95, 0x06, //   Report Count (6)
        0x15, 0x00, //   Logical Minimum (0)
        0x26, 0xFF, 0x00, //   Logical Maximum (255)
        0x09, 0x01, //   Usage (1)
        0x81, 0x00, //   Input (Data,Array,Abs)
        0x09, 0x01, //   Usage (1)
        0x91, 0x00, //   Output (Data,Array,Abs)
        0xC0, // End Collection
    ];

    /// The long collection on the same interface: usage 2, report id 0x11, nineteen bytes.
    const LONG_COLLECTION: &[u8] = &[
        0x06, 0x00, 0xFF, // Usage Page (0xFF00)
        0x09, 0x02, // Usage (2)
        0xA1, 0x01, // Collection (Application)
        0x85, 0x11, //   Report ID (0x11)
        0x75, 0x08, //   Report Size (8)
        0x95, 0x13, //   Report Count (19)
        0x09, 0x02, //   Usage (2)
        0x81, 0x00, //   Input
        0x09, 0x02, //   Usage (2)
        0x91, 0x00, //   Output
        0xC0, // End Collection
    ];

    #[test]
    fn short_collection_is_seven_bytes_on_the_vendor_page() {
        let d = parse(SHORT_COLLECTION).unwrap();
        assert_eq!(d.usage_page, 0xFF00);
        assert_eq!(d.usage, 0x0001);
        // 6 payload bytes + the report id byte is exactly a HID++ short report.
        assert_eq!(d.max_input_report_length, crate::report::constants::SHORT_LENGTH);
        assert_eq!(d.max_output_report_length, crate::report::constants::SHORT_LENGTH);
        assert_eq!(d.report_ids, vec![0x10]);
    }

    #[test]
    fn long_collection_is_twenty_bytes() {
        let d = parse(LONG_COLLECTION).unwrap();
        assert_eq!(d.usage, 0x0002);
        assert_eq!(d.max_input_report_length, crate::report::constants::LONG_LENGTH);
        assert_eq!(d.max_output_report_length, crate::report::constants::LONG_LENGTH);
        assert_eq!(d.report_ids, vec![0x11]);
    }

    #[test]
    fn a_collection_declaring_both_sizes_reports_the_longest() {
        // Some devices put short and long in one collection. The channel must then see 20, since
        // sizing its read buffer to 7 would silently truncate every long answer.
        let mut both = Vec::new();
        both.extend_from_slice(&[0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01]);
        both.extend_from_slice(&[0x85, 0x10, 0x75, 0x08, 0x95, 0x06, 0x81, 0x00, 0x91, 0x00]);
        both.extend_from_slice(&[0x85, 0x11, 0x75, 0x08, 0x95, 0x13, 0x81, 0x00, 0x91, 0x00]);
        both.push(0xC0);
        let d = parse(&both).unwrap();
        assert_eq!(d.max_input_report_length, 20);
        assert_eq!(d.report_ids, vec![0x10, 0x11]);
    }

    #[test]
    fn feature_reports_do_not_inflate_the_lengths() {
        // A 64-byte feature report on the same collection must not make the channel think it can
        // read 65-byte input reports.
        let desc: &[u8] = &[
            0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01, //
            0x85, 0x10, 0x75, 0x08, 0x95, 0x06, 0x81, 0x00, // 6-byte input
            0x85, 0x20, 0x75, 0x08, 0x95, 0x40, 0xB1, 0x00, // 64-byte feature
            0xC0,
        ];
        let d = parse(desc).unwrap();
        assert_eq!(d.max_input_report_length, 7);
        assert_eq!(d.max_output_report_length, 0, "no output report was declared");
    }

    #[test]
    fn push_and_pop_restore_report_size() {
        // Push/Pop is rare but legal, and getting it wrong would mis-size a report.
        let desc: &[u8] = &[
            0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01, //
            0x85, 0x11, 0x75, 0x08, 0x95, 0x13, //
            0xA4, // Push
            0x75, 0x01, 0x95, 0x08, // Report Size 1, Count 8 inside the push
            0xB4, // Pop
            0x81, 0x00, // Input, using the restored 8x19
            0xC0,
        ];
        assert_eq!(parse(desc).unwrap().max_input_report_length, 20);
    }

    #[test]
    fn standard_keyboard_collection_is_recognised_as_not_vendor() {
        // The boot keyboard collection on the same physical device. We must be able to read it and
        // see page 0x0001, which is how enumeration knows not to write to it.
        let desc: &[u8] = &[
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x06, // Usage (Keyboard)
            0xA1, 0x01, // Collection (Application)
            0x75, 0x08, 0x95, 0x08, 0x81, 0x00, //
            0xC0,
        ];
        let d = parse(desc).unwrap();
        assert_eq!((d.usage_page, d.usage), (0x0001, 0x0006));
    }

    #[test]
    fn a_four_byte_usage_carries_its_own_page() {
        let desc: &[u8] = &[
            0x0B, 0x01, 0x00, 0x43, 0xFF, // Usage (0xFF430001), extended form
            0xA1, 0x01, 0x75, 0x08, 0x95, 0x13, 0x81, 0x00, 0xC0,
        ];
        let d = parse(desc).unwrap();
        assert_eq!((d.usage_page, d.usage), (0xFF43, 0x0001));
    }

    #[test]
    fn truncated_descriptors_are_rejected_rather_than_half_parsed() {
        // A descriptor cut off mid-item: better an error than a plausible-looking wrong length.
        assert_eq!(parse(&[0x06, 0x00]), Err(DescriptorError::Truncated(1)));
        assert_eq!(parse(&[0xFE, 0x04, 0x01]), Err(DescriptorError::Truncated(1)));
        // Empty is not an error; it just tells us nothing.
        assert_eq!(parse(&[]).unwrap(), ParsedDescriptor::default());
    }

    #[test]
    fn long_items_are_stepped_over_without_desynchronising() {
        let mut desc = vec![0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01];
        desc.extend_from_slice(&[0xFE, 0x02, 0x7F, 0xAA, 0xBB]); // long item, 2 payload bytes
        desc.extend_from_slice(&[0x85, 0x11, 0x75, 0x08, 0x95, 0x13, 0x81, 0x00, 0xC0]);
        let d = parse(&desc).unwrap();
        assert_eq!(d.usage_page, 0xFF00);
        assert_eq!(d.max_input_report_length, 20);
    }
}
