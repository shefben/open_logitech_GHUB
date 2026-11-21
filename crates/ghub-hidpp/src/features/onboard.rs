//! 0x8100 Onboard profiles: mode and slot selection (RAM), and sector reads.
//!
//! Port of `OnboardProfile.cs` plus the `HidppOnboardProfilesFeature` half of `HidppFeatures.cs`.
//! Sector **writes** live in [`super::onboard_writer`] and are gated behind an explicit opt-in;
//! everything in this module is a read or a RAM-only mode change.
//!
//! The sector layout is libratbag's documented structure for the profile format used by
//! G903-class mice: report rate, DPI indexes, five little-endian DPI values, buttons, a UTF-16LE
//! name, and a CRC-CCITT trailer.

use crate::capability::{CapabilityMeta, CapabilityStatus, OnboardProfilesCapability, PersistenceKind};
use crate::channel::RequestOptions;
use crate::device::{HidppDevice, feature};
use crate::error::{HidppError, Result};

/// DPI stages a profile sector holds.
pub const PROFILE_DPI_COUNT: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardMode {
    Unknown,
    /// The device executes its stored profile itself.
    Onboard,
    /// The host drives the device.
    Host,
}

impl OnboardMode {
    fn wire(self) -> Option<u8> {
        match self {
            Self::Onboard => Some(1),
            Self::Host => Some(2),
            Self::Unknown => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardButtonBinding {
    pub index: usize,
    pub kind: u8,
    pub sub_type: u8,
    pub value: u16,
    /// The low byte of `value`, which the HID key binding uses as the usage code.
    pub raw3: u8,
}

impl OnboardButtonBinding {
    pub fn describe(&self) -> String {
        match (self.kind, self.sub_type) {
            (0x80, 0x01) => {
                if self.value == 0 {
                    "Mouse (none)".into()
                } else {
                    format!("Mouse button {}", self.value.trailing_zeros() + 1)
                }
            }
            (0x80, 0x02) => {
                format!("Key 0x{:02X} (mods 0x{:02X})", self.raw3, (self.value >> 8) as u8)
            }
            (0x80, 0x03) => format!("Consumer 0x{:04X}", self.value),
            (0x80, 0x00) => "No-op".into(),
            (0x90, special) => special_name(special),
            (0x00, _) => "Onboard macro".into(),
            (0xFF, _) => "Disabled".into(),
            (other, _) => format!("Unknown 0x{other:02X}"),
        }
    }
}

fn special_name(special: u8) -> String {
    match special {
        0x01 => "Tilt left".into(),
        0x02 => "Tilt right".into(),
        0x03 => "Next DPI".into(),
        0x04 => "Previous DPI".into(),
        0x05 => "Cycle DPI".into(),
        0x06 => "Default DPI".into(),
        0x07 => "DPI shift".into(),
        0x08 => "Next profile".into(),
        0x09 => "Previous profile".into(),
        0x0A => "Cycle profile".into(),
        0x0B => "G-shift".into(),
        0x0C => "Battery indicator".into(),
        0x0D => "Enable profile".into(),
        0x0E => "Performance switch".into(),
        0x0F => "Host switch".into(),
        0x10 => "Scroll down".into(),
        0x11 => "Scroll up".into(),
        other => format!("Special 0x{other:02X}"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OnboardProfile {
    pub slot: usize,
    pub address: u16,
    pub enabled: bool,
    /// Read from the ROM defaults rather than a user sector. Never written back.
    pub from_rom: bool,
    pub crc_valid: bool,
    pub report_rate_hz: u32,
    pub default_dpi_index: u8,
    pub shift_dpi_index: u8,
    pub dpi_stages: Vec<u32>,
    pub buttons: Vec<OnboardButtonBinding>,
    pub name: String,
    pub raw_sector: Vec<u8>,
}

impl OnboardProfile {
    pub fn parse(
        slot: usize,
        address: u16,
        enabled: bool,
        from_rom: bool,
        sector: &[u8],
        button_count: usize,
    ) -> Result<Self> {
        // The name lives at 160..208 and the CRC in the last two bytes, so anything shorter than
        // 0xD2 cannot be this format and must not be indexed into.
        if sector.len() < 0xD0 + 2 {
            return Err(HidppError::Malformed(format!(
                "onboard profile sector too small ({} bytes)",
                sector.len()
            )));
        }

        let crc_valid = crc::is_valid(sector);
        let report_rate_ms = (sector[0] as u32).max(1);

        let mut dpi_stages = Vec::with_capacity(PROFILE_DPI_COUNT);
        for i in 0..PROFILE_DPI_COUNT {
            // DPI values are little-endian here, unlike the big-endian 0x2201 feature.
            let value = sector[3 + 2 * i] as u32 | ((sector[4 + 2 * i] as u32) << 8);
            // 0 is an empty stage and 0xFFFF is erased flash; neither is a DPI the user set.
            if value == 0 || value == 0xFFFF {
                continue;
            }
            dpi_stages.push(value);
        }

        let count = button_count.min(16);
        let mut buttons = Vec::with_capacity(count);
        for i in 0..count {
            let o = 32 + 4 * i;
            buttons.push(OnboardButtonBinding {
                index: i,
                kind: sector[o],
                sub_type: sector[o + 1],
                value: ((sector[o + 2] as u16) << 8) | sector[o + 3] as u16,
                raw3: sector[o + 3],
            });
        }

        Ok(Self {
            slot,
            address,
            enabled,
            from_rom,
            crc_valid,
            report_rate_hz: 1000 / report_rate_ms,
            default_dpi_index: sector[1],
            shift_dpi_index: sector[2],
            dpi_stages,
            buttons,
            name: decode_name(&sector[160..208]),
            raw_sector: sector.to_vec(),
        })
    }
}

/// Profile names are UTF-16LE on current firmware; an unset name is 0xFF filled.
fn decode_name(raw: &[u8]) -> String {
    let mut units = Vec::new();
    let mut i = 0;
    while i + 1 < raw.len() {
        let ch = raw[i] as u16 | ((raw[i + 1] as u16) << 8);
        if ch == 0 || ch == 0xFFFF {
            break;
        }
        units.push(ch);
        i += 2;
    }
    // Lossy rather than failing: a mangled name is cosmetic, and refusing to show the profile
    // would hide a profile the user can otherwise edit.
    String::from_utf16_lossy(&units)
}

/// CRC-CCITT (seed 0xFFFF, polynomial 0x1021) over everything but the last two bytes, which hold
/// it big-endian. Firmware rejects a sector whose CRC does not match, so this is also what makes a
/// corrupt read detectable before we ever consider writing.
pub mod crc {
    pub fn compute(data: &[u8]) -> u16 {
        let mut crc: u16 = 0xFFFF;
        for b in data {
            let temp = (crc >> 8) ^ (*b as u16);
            crc <<= 8;
            let mut quick = temp ^ (temp >> 4);
            crc ^= quick;
            quick <<= 5;
            crc ^= quick;
            quick <<= 7;
            crc ^= quick;
        }
        crc
    }

    pub fn is_valid(sector: &[u8]) -> bool {
        if sector.len() < 3 {
            return false;
        }
        let stored =
            ((sector[sector.len() - 2] as u16) << 8) | sector[sector.len() - 1] as u16;
        compute(&sector[..sector.len() - 2]) == stored
    }

    /// Writes the CRC into the last two bytes of a sector being prepared for a write.
    pub fn seal(sector: &mut [u8]) {
        if sector.len() < 3 {
            return;
        }
        let crc = compute(&sector[..sector.len() - 2]);
        let n = sector.len();
        sector[n - 2] = (crc >> 8) as u8;
        sector[n - 1] = crc as u8;
    }
}

/// One entry of the directory sector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub address: u16,
    pub enabled: bool,
}

/// Directory sector: 4-byte entries of [address big-endian][enabled][reserved], terminated by
/// 0xFFFF. A directory whose CRC does not check out yields nothing: acting on a corrupt directory
/// would mean reading - or worse, writing - arbitrary sector addresses.
pub fn parse_directory(sector: &[u8], slot_count: usize) -> Vec<DirectoryEntry> {
    let mut entries = Vec::new();
    if !crc::is_valid(sector) {
        return entries;
    }
    for i in 0..slot_count {
        if 4 * i + 3 >= sector.len() {
            break;
        }
        let address = ((sector[4 * i] as u16) << 8) | sector[4 * i + 1] as u16;
        if address == 0xFFFF {
            break;
        }
        entries.push(DirectoryEntry { address, enabled: sector[4 * i + 2] != 0 });
    }
    entries
}

pub struct OnboardProfilesFeature<'d> {
    pub device: &'d HidppDevice,
    pub capability: OnboardProfilesCapability,
}

impl<'d> OnboardProfilesFeature<'d> {
    pub fn create(device: &'d HidppDevice, writes_enabled: bool) -> Result<Option<Self>> {
        if !device.has_feature(feature::ONBOARD_PROFILES) {
            return Ok(None);
        }
        let desc = device.call(feature::ONBOARD_PROFILES, 0, &[], RequestOptions::default())?;
        Ok(Some(Self {
            device,
            capability: parse_description(desc.parameters(), writes_enabled),
        }))
    }

    pub fn get_mode(&self) -> Result<OnboardMode> {
        let r = self.device.call(feature::ONBOARD_PROFILES, 2, &[], RequestOptions::default())?;
        Ok(match r.param(0) {
            1 => OnboardMode::Onboard,
            2 => OnboardMode::Host,
            _ => OnboardMode::Unknown,
        })
    }

    /// Switches between onboard and host mode. RAM state, not a flash write.
    pub fn set_mode(&self, mode: OnboardMode) -> Result<()> {
        let wire = mode
            .wire()
            .ok_or_else(|| HidppError::Unsupported("cannot set the mode to Unknown".into()))?;
        self.device.call(feature::ONBOARD_PROFILES, 1, &[wire], RequestOptions::default())?;
        Ok(())
    }

    pub fn get_active_slot(&self) -> Result<u8> {
        let r = self.device.call(feature::ONBOARD_PROFILES, 4, &[], RequestOptions::default())?;
        Ok((r.param_u16(0) & 0xFF) as u8)
    }

    pub fn set_active_slot(&self, slot: usize) -> Result<u8> {
        if slot < 1 || slot > self.capability.slot_count {
            return Err(HidppError::Unsupported(format!(
                "slot must be 1-{}",
                self.capability.slot_count
            )));
        }
        self.device.call(
            feature::ONBOARD_PROFILES,
            3,
            &[0x00, slot as u8],
            RequestOptions::default(),
        )?;
        self.get_active_slot()
    }

    pub fn get_active_dpi_index(&self) -> Result<u8> {
        let r = self.device.call(feature::ONBOARD_PROFILES, 0xB, &[], RequestOptions::default())?;
        Ok(r.param(0))
    }

    pub fn set_active_dpi_index(&self, index: usize) -> Result<u8> {
        if index >= PROFILE_DPI_COUNT {
            return Err(HidppError::Unsupported(format!(
                "DPI stage index must be 0-{}",
                PROFILE_DPI_COUNT - 1
            )));
        }
        self.device.call(
            feature::ONBOARD_PROFILES,
            0xC,
            &[index as u8],
            RequestOptions::default(),
        )?;
        self.get_active_dpi_index()
    }

    /// Reads one sector in 16-byte chunks.
    pub fn read_sector(&self, sector: u16) -> Result<Vec<u8>> {
        let size = self.capability.sector_size;
        if !(16..=4096).contains(&size) {
            return Err(HidppError::Malformed(format!("implausible sector size {size}")));
        }
        let mut data = vec![0u8; size];
        let mut offset = 0usize;
        while offset < size {
            // Firmware rejects a read that would run past size - 16, so the final chunk is read
            // from size - 16 and overlaps the previous one. This is libratbag's behaviour.
            let o = if size - offset < 16 { size - 16 } else { offset };
            let r = self.device.call(
                feature::ONBOARD_PROFILES,
                5,
                &[(sector >> 8) as u8, sector as u8, (o >> 8) as u8, o as u8],
                RequestOptions::long(),
            )?;
            if r.parameters().len() < 16 {
                return Err(HidppError::Malformed("short memory-read response".into()));
            }
            data[o..o + 16].copy_from_slice(&r.parameters()[..16]);
            offset += 16;
        }
        Ok(data)
    }

    /// Reads every slot. A user sector with a bad CRC falls back to the ROM default **for display
    /// only**; it is never written back, because we do not know what the user meant to be there.
    pub fn read_profiles(&self) -> Result<Vec<OnboardProfile>> {
        let directory = self.read_sector(0x0000)?;
        let entries = parse_directory(&directory, self.capability.slot_count);

        let mut profiles = Vec::with_capacity(self.capability.slot_count);
        for i in 0..self.capability.slot_count {
            let (address, enabled) = match entries.get(i) {
                Some(e) => (e.address, e.enabled),
                None => (0, false),
            };
            let from_rom = address == 0;
            let sector = if from_rom { self.rom_sector(i) } else { address };
            let data = self.read_sector(sector)?;
            let mut profile =
                OnboardProfile::parse(i + 1, sector, enabled, from_rom, &data, self.capability.button_count)?;

            if !from_rom && !profile.crc_valid {
                log::warn!(
                    "onboard slot {} (sector 0x{:04X}) has a bad CRC; showing the ROM default",
                    i + 1,
                    address
                );
                let rom = self.rom_sector(i);
                let rom_data = self.read_sector(rom)?;
                profile = OnboardProfile::parse(
                    i + 1,
                    rom,
                    enabled,
                    true,
                    &rom_data,
                    self.capability.button_count,
                )?;
            }
            profiles.push(profile);
        }
        Ok(profiles)
    }

    fn rom_sector(&self, slot_index: usize) -> u16 {
        0x0100 + (slot_index + 1).min(self.capability.rom_slot_count.max(1)) as u16
    }
}

pub fn parse_description(p: &[u8], writes_enabled: bool) -> OnboardProfilesCapability {
    let at = |i: usize| p.get(i).copied().unwrap_or(0);
    let sector_size = ((at(7) as usize) << 8) | at(8) as usize;
    // Memory model 1 with profile format 1-6 is the layout `OnboardProfile::parse` implements.
    // Anything else is read-only: writing a format we cannot parse would corrupt the sector.
    let known_format = at(0) == 0x01 && (0x01..=0x06).contains(&at(1));

    OnboardProfilesCapability {
        meta: CapabilityMeta {
            status: if known_format {
                CapabilityStatus::Supported
            } else {
                CapabilityStatus::ReadOnly
            },
            reason: if known_format {
                None
            } else {
                Some(format!(
                    "Unknown onboard memory model/format {}/{}; read-only",
                    at(0),
                    at(1)
                ))
            },
            persistence: PersistenceKind::Onboard,
            feature_id: Some(feature::ONBOARD_PROFILES),
        },
        memory_model: at(0),
        profile_format: at(1),
        slot_count: at(3) as usize,
        rom_slot_count: at(4) as usize,
        button_count: at(5) as usize,
        sector_count: at(6) as usize,
        sector_size,
        writes_enabled: writes_enabled && known_format,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::from_hex;

    /// Ported from `HidppParsingTests.OnboardDescription_Observed`. All three vectors are as
    /// observed on hardware.
    #[test]
    fn the_observed_descriptions_parse() {
        let g903 = parse_description(&from_hex("01040105010C1000FF0A040000000000").unwrap(), false);
        assert_eq!(g903.slot_count, 5);
        assert_eq!(g903.button_count, 12);
        assert_eq!(g903.sector_size, 255);
        assert!(!g903.writes_enabled, "writes stay off until explicitly enabled");
        assert_eq!(g903.meta.status, CapabilityStatus::Supported);

        let g815 = parse_description(&from_hex("01040103030810010036010000000000").unwrap(), true);
        assert_eq!(g815.slot_count, 3);
        assert_eq!(g815.sector_size, 256);
        assert!(g815.writes_enabled);

        // Memory model 2, format 9: a layout this code cannot parse.
        let unknown =
            parse_description(&from_hex("02090105010C1000FF0A040000000000").unwrap(), true);
        assert_eq!(unknown.meta.status, CapabilityStatus::ReadOnly);
        assert!(
            !unknown.writes_enabled,
            "an unknown format must stay read-only even with writes enabled"
        );
        assert!(unknown.meta.reason.is_some());
    }

    /// Ported from `HidppParsingTests.Crc_RoundTrips_AndDetectsCorruption`.
    #[test]
    fn the_crc_round_trips_and_catches_a_single_bit_flip() {
        let mut sector = vec![0u8; 255];
        // Deterministic filler rather than a seeded RNG: the point is the CRC, not the data.
        for (i, b) in sector.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(31).wrapping_add(7);
        }
        crc::seal(&mut sector);
        assert!(crc::is_valid(&sector));

        sector[10] ^= 0x01;
        assert!(!crc::is_valid(&sector), "a one-bit flip must not pass");

        // Too short to carry a CRC at all.
        assert!(!crc::is_valid(&[0x00, 0x01]));
    }

    /// Ported from `HidppParsingTests.Directory_RequiresValidCrc`.
    #[test]
    fn a_directory_with_a_bad_crc_yields_nothing() {
        let mut dir = vec![0xFFu8; 255];
        let entries = from_hex("000101FF000201FF000300FF").unwrap();
        dir[..entries.len()].copy_from_slice(&entries);
        // No CRC sealed: reading sector addresses out of a corrupt directory could have us read,
        // or later write, arbitrary flash.
        assert!(parse_directory(&dir, 5).is_empty());

        crc::seal(&mut dir);
        let parsed = parse_directory(&dir, 5);
        assert_eq!(parsed.len(), 3, "0xFFFF terminates the list");
        assert_eq!(parsed[0], DirectoryEntry { address: 0x0001, enabled: true });
        assert_eq!(parsed[2], DirectoryEntry { address: 0x0003, enabled: false });
    }

    /// Builds a 255-byte sector in the documented layout.
    fn sector(dpi: &[u16], name: &str, report_rate_ms: u8) -> Vec<u8> {
        let mut s = vec![0u8; 255];
        s[0] = report_rate_ms;
        s[1] = 0; // default DPI index
        s[2] = 2; // shift DPI index
        for (i, value) in dpi.iter().enumerate().take(PROFILE_DPI_COUNT) {
            s[3 + 2 * i] = *value as u8;
            s[4 + 2 * i] = (*value >> 8) as u8;
        }
        // Button 0: HID key binding, usage 0x04 ("a") with no modifiers.
        s[32] = 0x80;
        s[33] = 0x02;
        s[34] = 0x00;
        s[35] = 0x04;
        // Button 1: special, DPI shift.
        s[36] = 0x90;
        s[37] = 0x07;
        for (i, unit) in name.encode_utf16().enumerate() {
            s[160 + 2 * i] = unit as u8;
            s[161 + 2 * i] = (unit >> 8) as u8;
        }
        crc::seal(&mut s);
        s
    }

    /// Mirrors the `DpiStages` assertions in `HidppParsingTests`.
    #[test]
    fn a_profile_sector_parses_into_stages_buttons_and_a_name() {
        let data = sector(&[1000, 1400, 1800, 1950, 2050], "Default", 1);
        let p = OnboardProfile::parse(3, 0x0103, true, false, &data, 12).unwrap();
        assert!(p.crc_valid);
        assert_eq!(p.dpi_stages, vec![1000, 1400, 1800, 1950, 2050]);
        assert_eq!(p.default_dpi_index, 0);
        assert_eq!(p.shift_dpi_index, 2);
        assert_eq!(p.report_rate_hz, 1000);
        assert_eq!(p.name, "Default");
        assert_eq!(p.slot, 3);
        assert_eq!(p.buttons.len(), 12);
        assert_eq!(p.buttons[0].describe(), "Key 0x04 (mods 0x00)");
        assert_eq!(p.buttons[1].describe(), "DPI shift");
        // Type 0x00 is the macro binding, so an all-zero slot reads as "Onboard macro" rather
        // than as unassigned. Worth knowing before any write path trusts a zeroed button.
        assert_eq!(p.buttons[2].kind, 0x00);
        assert_eq!(p.buttons[2].describe(), "Onboard macro");
    }

    #[test]
    fn empty_and_erased_dpi_stages_are_skipped() {
        // 0 is an unused stage and 0xFFFF is erased flash. Offering either as a DPI would set the
        // mouse to nothing or to 65535.
        let data = sector(&[400, 0, 1600, 0xFFFF, 3200], "x", 1);
        let p = OnboardProfile::parse(1, 0, true, true, &data, 4).unwrap();
        assert_eq!(p.dpi_stages, vec![400, 1600, 3200]);
    }

    #[test]
    fn a_short_sector_is_refused_rather_than_indexed_into() {
        assert!(matches!(
            OnboardProfile::parse(1, 0, true, false, &[0u8; 64], 12),
            Err(HidppError::Malformed(_))
        ));
    }

    #[test]
    fn a_corrupt_sector_still_parses_but_is_flagged() {
        // The UI needs to show something and say it is corrupt; refusing to parse would leave the
        // slot blank with no explanation.
        let mut data = sector(&[800], "Broken", 1);
        data[5] ^= 0xFF;
        let p = OnboardProfile::parse(1, 0x0101, true, false, &data, 4).unwrap();
        assert!(!p.crc_valid);
        assert!(!p.name.is_empty());
    }

    #[test]
    fn an_unset_name_is_empty_rather_than_filled_with_replacement_characters() {
        let mut data = sector(&[800], "", 1);
        data[160..208].fill(0xFF);
        crc::seal(&mut data);
        assert_eq!(OnboardProfile::parse(1, 0, true, true, &data, 4).unwrap().name, "");
    }

    #[test]
    fn button_descriptions_cover_every_binding_kind() {
        let b = |kind, sub, value: u16| OnboardButtonBinding {
            index: 0,
            kind,
            sub_type: sub,
            value,
            raw3: value as u8,
        };
        assert_eq!(b(0x80, 0x01, 0x0001).describe(), "Mouse button 1");
        assert_eq!(b(0x80, 0x01, 0x0008).describe(), "Mouse button 4");
        assert_eq!(b(0x80, 0x01, 0).describe(), "Mouse (none)");
        assert_eq!(b(0x80, 0x03, 0x00E9).describe(), "Consumer 0x00E9");
        assert_eq!(b(0x00, 0x00, 0).describe(), "Onboard macro");
        assert_eq!(b(0xFF, 0x00, 0).describe(), "Disabled");
        assert_eq!(b(0x90, 0x0B, 0).describe(), "G-shift");
        // An unknown special still gets a stable label rather than being dropped.
        assert_eq!(b(0x90, 0x7F, 0).describe(), "Special 0x7F");
        assert_eq!(b(0x55, 0x00, 0).describe(), "Unknown 0x55");
    }

    #[test]
    fn setting_the_mode_to_unknown_is_refused() {
        // `Unknown` is what we return for a byte we do not recognise. Sending it back would write
        // an arbitrary mode byte.
        assert!(OnboardMode::Unknown.wire().is_none());
        assert_eq!(OnboardMode::Onboard.wire(), Some(1));
        assert_eq!(OnboardMode::Host.wire(), Some(2));
    }
}
