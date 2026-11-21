//! Explicit "Save to device" for HID++ 0x8100 onboard profiles.
//! Port of `Artemis.Peripherals/Logitech/Hidpp/Features/OnboardProfileWriter.cs`.
//!
//! This is the only code in the crate that writes flash, so it is the only code that can brick a
//! profile. The sequence is libratbag's documented one - `memory-addr-write` (fn 6),
//! `memory-write` in 16-byte chunks (fn 7), `memory-write-end` (fn 8), CRC-CCITT trailer - wrapped
//! in: read current, build a plan and a diff, require a persisted snapshot, re-read and confirm
//! nothing changed under us, write, read back, compare, and on any mismatch write the previous
//! sector back.
//!
//! Hard refusals, each covered by a test below: writes disabled, an unsupported profile format,
//! the directory sector, any ROM sector, a plan with no backup, a plan whose CRC or length is
//! wrong, and a device whose memory changed since the plan was made.
//!
//! **Split from the C#:** the original took an `ActionDefinition` and encoded it here. The action
//! model lives in `ghub-core`, which depends on this crate, so [`OnboardEdit`] takes bindings
//! already encoded to their four wire bytes and `ghub-core` owns the encoding.

use std::collections::HashMap;

use crate::channel::RequestOptions;
use crate::device::feature;
use crate::error::{HidppError, Result};
use crate::features::onboard::{
    OnboardProfile, OnboardProfilesFeature, PROFILE_DPI_COUNT, crc,
};

use serde::{Deserialize, Serialize};

/// Profile formats this code will write. Reading is tolerant; writing is not, because a format we
/// parse only approximately would be written back wrong.
pub const SUPPORTED_PROFILE_FORMATS: [u8; 1] = [0x04];

/// A requested change to one onboard slot. `None` fields are left exactly as stored on the device.
#[derive(Debug, Clone, Default)]
pub struct OnboardEdit {
    pub slot: usize,
    pub dpi_stages: Option<Vec<u32>>,
    pub default_dpi_index: Option<usize>,
    pub shift_dpi_index: Option<usize>,
    pub report_rate_hz: Option<u32>,
    /// Onboard button index (0-based, device order) to its four encoded wire bytes.
    pub buttons: Option<HashMap<usize, [u8; 4]>>,
}

/// One human-readable field change, for the confirmation dialog. The user confirms this list, not
/// a hex diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardDiff {
    pub field: String,
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardWritePlan {
    pub slot: usize,
    pub sector: u16,
    pub before: Vec<u8>,
    pub after: Vec<u8>,
    pub changes: Vec<OnboardDiff>,
}

impl OnboardWritePlan {
    pub fn has_changes(&self) -> bool {
        !self.changes.is_empty()
    }
}

/// Every readable sector, for backup and restore. Persisted by the caller **before** a commit; a
/// commit refuses to run without one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OnboardSnapshot {
    pub device_key: String,
    pub device_name: String,
    pub firmware: Option<String>,
    /// Sector number to its contents as uppercase hex.
    pub sectors_hex: HashMap<u16, String>,
}

/// A write that failed. `rolled_back` says whether the previous sector was successfully restored,
/// which is the difference between "nothing happened" and "restore from the snapshot".
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct OnboardWriteError {
    pub message: String,
    pub rolled_back: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum WriteFailure {
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    Write(#[from] OnboardWriteError),
    #[error(transparent)]
    Hidpp(#[from] HidppError),
}

type WriteResult<T> = std::result::Result<T, WriteFailure>;

pub struct OnboardProfileWriter<'f, 'd> {
    feature: &'f OnboardProfilesFeature<'d>,
}

impl<'f, 'd> OnboardProfileWriter<'f, 'd> {
    pub fn new(feature: &'f OnboardProfilesFeature<'d>) -> Self {
        Self { feature }
    }

    /// Reads the live slot and builds a plan against it.
    pub fn plan(&self, edit: &OnboardEdit) -> WriteResult<OnboardWritePlan> {
        let profiles = self.feature.read_profiles()?;
        let current = profiles
            .iter()
            .find(|p| p.slot == edit.slot)
            .ok_or_else(|| WriteFailure::Refused(format!("slot {} does not exist", edit.slot)))?;

        if current.from_rom {
            // Creating a user sector means allocating flash and editing the directory. Neither is
            // a format we have verified, so it is out of scope rather than attempted.
            return Err(WriteFailure::Refused(format!(
                "slot {} is backed by ROM defaults; initialising a user sector is not supported",
                edit.slot
            )));
        }
        if !current.crc_valid {
            return Err(WriteFailure::Refused(format!(
                "slot {} has an invalid CRC; refusing to modify it",
                edit.slot
            )));
        }
        build_plan(current, edit, self.feature.capability.button_count)
    }

    /// Reads every readable sector: the directory, the user slots and the ROM slots.
    pub fn snapshot(
        &self,
        device_key: &str,
        device_name: &str,
        firmware: Option<&str>,
    ) -> Result<OnboardSnapshot> {
        let mut sectors: Vec<u16> = vec![0x0000];
        sectors.extend((1..=self.feature.capability.slot_count).map(|i| i as u16));
        sectors.extend(
            (1..=self.feature.capability.rom_slot_count).map(|i| 0x0100 + i as u16),
        );

        let mut sectors_hex = HashMap::new();
        for sector in sectors {
            match self.feature.read_sector(sector) {
                Ok(data) => {
                    sectors_hex.insert(
                        sector,
                        data.iter().map(|b| format!("{b:02X}")).collect::<String>(),
                    );
                }
                // An unwritten sector may refuse reads, and there is nothing there to back up.
                Err(HidppError::Protocol(e)) => {
                    log::debug!("sector 0x{sector:04X} not readable ({e}); nothing to back up");
                }
                Err(e) => return Err(e),
            }
        }

        Ok(OnboardSnapshot {
            device_key: device_key.to_owned(),
            device_name: device_name.to_owned(),
            firmware: firmware.map(str::to_owned),
            sectors_hex,
        })
    }

    /// Writes the plan and verifies it by read-back. The snapshot must already be on disk.
    pub fn commit(
        &self,
        plan: &OnboardWritePlan,
        persisted_snapshot: &OnboardSnapshot,
    ) -> WriteResult<OnboardProfile> {
        if let Some(refusal) =
            check_commit_preconditions(&self.feature.capability, plan, persisted_snapshot)
        {
            return Err(WriteFailure::Refused(refusal.to_string()));
        }

        let live = self.feature.read_sector(plan.sector)?;
        if live != plan.before {
            // G HUB may be running alongside us. Writing a sector built from stale contents would
            // silently discard whatever it just changed.
            return Err(OnboardWriteError {
                message: "device memory changed since the plan was made (another application?); \
                          re-read and try again"
                    .into(),
                rolled_back: false,
            }
            .into());
        }

        match self.write_and_verify(plan) {
            Ok(profile) => Ok(profile),
            Err(first) => Err(self.roll_back(plan, &first).into()),
        }
    }

    fn write_and_verify(&self, plan: &OnboardWritePlan) -> Result<OnboardProfile> {
        self.write_sector(plan.sector, &plan.after)?;
        let read_back = self.feature.read_sector(plan.sector)?;
        if read_back != plan.after {
            return Err(HidppError::Malformed("read-back verification failed".into()));
        }
        OnboardProfile::parse(
            plan.slot,
            plan.sector,
            true,
            false,
            &read_back,
            self.feature.capability.button_count,
        )
    }

    /// Puts the previous sector back and reports whether it took.
    fn roll_back(&self, plan: &OnboardWritePlan, cause: &HidppError) -> OnboardWriteError {
        let restored = self
            .write_sector(plan.sector, &plan.before)
            .and_then(|()| self.feature.read_sector(plan.sector))
            .map(|data| data == plan.before);

        match restored {
            Ok(true) => OnboardWriteError {
                message: format!("save to device failed ({cause}); previous sector restored"),
                rolled_back: true,
            },
            Ok(false) => OnboardWriteError {
                message: format!(
                    "save to device failed ({cause}); RESTORE FAILED - use the snapshot"
                ),
                rolled_back: false,
            },
            Err(restore_error) => OnboardWriteError {
                message: format!(
                    "save to device failed ({cause}) and restore failed ({restore_error}); \
                     restore from snapshot"
                ),
                rolled_back: false,
            },
        }
    }

    /// The raw write sequence. Every call is non-idempotent: memory writes are a sequential
    /// stream, so a resent chunk after a lost reply would shift the rest of the sector.
    fn write_sector(&self, sector: u16, data: &[u8]) -> Result<()> {
        let device = self.feature.device;
        let size = data.len();
        device.call(
            feature::ONBOARD_PROFILES,
            6,
            &[
                (sector >> 8) as u8,
                sector as u8,
                0,
                0,
                (size >> 8) as u8,
                size as u8,
            ],
            RequestOptions::non_idempotent().forcing_long(),
        )?;

        for offset in (0..size).step_by(16) {
            let mut chunk = [0u8; 16];
            let take = 16.min(size - offset);
            chunk[..take].copy_from_slice(&data[offset..offset + take]);
            device.call(
                feature::ONBOARD_PROFILES,
                7,
                &chunk,
                RequestOptions::non_idempotent().forcing_long(),
            )?;
        }

        device.call(
            feature::ONBOARD_PROFILES,
            8,
            &[],
            RequestOptions::non_idempotent(),
        )?;
        Ok(())
    }
}

/// Why a commit must not proceed, or `None` when it may.
///
/// Separated from [`OnboardProfileWriter::commit`] so every refusal is reachable - and tested -
/// without a device attached, since the whole point of these checks is that they fire *before*
/// anything is sent to flash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitRefusal {
    /// The user has not enabled onboard writes.
    WritesDisabled,
    /// The device's profile format is not one whose layout has been verified.
    UnsupportedFormat(u8),
    /// The directory sector, or a ROM sector.
    ProtectedSector(u16),
    /// No persisted snapshot covers the target sector.
    NoBackup(u16),
    /// The planned sector has a bad CRC or the wrong length.
    PlanInvalid,
}

impl std::fmt::Display for CommitRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WritesDisabled => f.write_str(
                "onboard writes are disabled (Settings, Peripherals, Allow Save to Device)",
            ),
            Self::UnsupportedFormat(format) => {
                write!(f, "profile format {format} is not supported for writing")
            }
            Self::ProtectedSector(sector) => write!(
                f,
                "refusing to write sector 0x{sector:04X}: the directory and ROM sectors are protected"
            ),
            Self::NoBackup(sector) => write!(f, "no backup of the target sector 0x{sector:04X}"),
            Self::PlanInvalid => f.write_str("the planned sector failed validation"),
        }
    }
}

pub fn check_commit_preconditions(
    capability: &crate::capability::OnboardProfilesCapability,
    plan: &OnboardWritePlan,
    persisted_snapshot: &OnboardSnapshot,
) -> Option<CommitRefusal> {
    if !capability.writes_enabled {
        return Some(CommitRefusal::WritesDisabled);
    }
    if !SUPPORTED_PROFILE_FORMATS.contains(&capability.profile_format) {
        return Some(CommitRefusal::UnsupportedFormat(capability.profile_format));
    }
    // 0x0000 is the directory, which decides where every profile lives; 0x0100 and up are the
    // factory ROM defaults, the only fallback if a user sector is corrupted.
    if plan.sector == 0x0000 || plan.sector >= 0x0100 {
        return Some(CommitRefusal::ProtectedSector(plan.sector));
    }
    if !persisted_snapshot.sectors_hex.contains_key(&plan.sector) {
        return Some(CommitRefusal::NoBackup(plan.sector));
    }
    if !crc::is_valid(&plan.after) || plan.after.len() != capability.sector_size {
        return Some(CommitRefusal::PlanInvalid);
    }
    None
}

/// Builds the new sector and the diff the user confirms. Pure: it touches no device, which is what
/// makes every validation rule below testable without hardware.
pub fn build_plan(
    current: &OnboardProfile,
    edit: &OnboardEdit,
    button_count: usize,
) -> WriteResult<OnboardWritePlan> {
    let before = current.raw_sector.clone();
    let mut after = before.clone();

    if let Some(hz) = edit.report_rate_hz {
        if !matches!(hz, 125 | 250 | 500 | 1000) {
            return Err(WriteFailure::Refused(format!(
                "onboard report rate {hz} Hz is not representable"
            )));
        }
        after[0] = (1000 / hz) as u8;
    }

    if let Some(stages) = &edit.dpi_stages {
        if stages.is_empty()
            || stages.len() > PROFILE_DPI_COUNT
            || stages.iter().any(|d| *d < 50 || *d > 50_000)
        {
            return Err(WriteFailure::Refused(
                "1-5 DPI stages between 50 and 50000".into(),
            ));
        }
        for i in 0..PROFILE_DPI_COUNT {
            // Unused trailing stages are zeroed, which is how the firmware marks them empty.
            let v = stages.get(i).copied().unwrap_or(0);
            after[3 + 2 * i] = v as u8;
            after[4 + 2 * i] = (v >> 8) as u8;
        }
    }

    // Index bounds follow the stage count after the edit, not before it: shrinking the list must
    // not leave the default pointing past the end.
    let stage_count = edit
        .dpi_stages
        .as_ref()
        .map(Vec::len)
        .unwrap_or(current.dpi_stages.len());
    let last = stage_count.saturating_sub(1);
    if let Some(def) = edit.default_dpi_index {
        after[1] = def.min(last) as u8;
    }
    if let Some(shift) = edit.shift_dpi_index {
        after[2] = shift.min(last) as u8;
    }

    if let Some(buttons) = &edit.buttons {
        // Sorted so a plan built from the same edit is byte-identical every time.
        let mut indexes: Vec<usize> = buttons.keys().copied().collect();
        indexes.sort_unstable();
        for index in indexes {
            if index >= button_count.min(16) {
                return Err(WriteFailure::Refused(format!("button {index} does not exist")));
            }
            after[32 + 4 * index..36 + 4 * index].copy_from_slice(&buttons[&index]);
        }
    }

    crc::seal(&mut after);

    let parsed_after = OnboardProfile::parse(
        current.slot,
        current.address,
        current.enabled,
        false,
        &after,
        button_count,
    )?;

    let mut changes = Vec::new();
    let mut diff = |field: &str, a: String, b: String| {
        if a != b {
            changes.push(OnboardDiff { field: field.to_owned(), before: a, after: b });
        }
    };

    let join = |v: &[u32]| v.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
    diff(
        "Report rate",
        format!("{} Hz", current.report_rate_hz),
        format!("{} Hz", parsed_after.report_rate_hz),
    );
    diff("DPI stages", join(&current.dpi_stages), join(&parsed_after.dpi_stages));
    diff(
        "Default DPI stage",
        (current.default_dpi_index + 1).to_string(),
        (parsed_after.default_dpi_index + 1).to_string(),
    );
    diff(
        "DPI shift stage",
        (current.shift_dpi_index + 1).to_string(),
        (parsed_after.shift_dpi_index + 1).to_string(),
    );
    for i in 0..parsed_after.buttons.len() {
        diff(
            &format!("Button {}", i + 1),
            current.buttons[i].describe(),
            parsed_after.buttons[i].describe(),
        );
    }

    Ok(OnboardWritePlan {
        slot: current.slot,
        sector: current.address,
        before,
        after,
        changes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{CapabilityMeta, OnboardProfilesCapability, PersistenceKind};

    /// A 255-byte sector in the documented layout, with its CRC sealed.
    fn sector(dpi: &[u16], report_rate_ms: u8) -> Vec<u8> {
        let mut s = vec![0u8; 255];
        s[0] = report_rate_ms;
        s[1] = 0;
        s[2] = 1;
        for (i, value) in dpi.iter().enumerate().take(PROFILE_DPI_COUNT) {
            s[3 + 2 * i] = *value as u8;
            s[4 + 2 * i] = (*value >> 8) as u8;
        }
        // Button 0: mouse button 1.
        s[32..36].copy_from_slice(&[0x80, 0x01, 0x00, 0x01]);
        crc::seal(&mut s);
        s
    }

    fn profile(dpi: &[u16], report_rate_ms: u8) -> OnboardProfile {
        OnboardProfile::parse(1, 0x0001, true, false, &sector(dpi, report_rate_ms), 4).unwrap()
    }

    fn capability(writes_enabled: bool, profile_format: u8) -> OnboardProfilesCapability {
        OnboardProfilesCapability {
            meta: CapabilityMeta::supported(feature::ONBOARD_PROFILES, PersistenceKind::Onboard),
            slot_count: 3,
            rom_slot_count: 3,
            button_count: 4,
            sector_size: 255,
            sector_count: 10,
            profile_format,
            memory_model: 1,
            writes_enabled,
        }
    }

    #[test]
    fn a_plan_with_no_edit_has_no_changes_and_an_identical_sector() {
        // The confirmation dialog must not offer to write when nothing changed.
        let current = profile(&[800, 1600], 1);
        let plan = build_plan(&current, &OnboardEdit { slot: 1, ..Default::default() }, 4).unwrap();
        assert!(!plan.has_changes());
        assert_eq!(plan.before, plan.after);
        assert!(crc::is_valid(&plan.after));
    }

    #[test]
    fn a_plan_diffs_only_the_fields_that_changed() {
        let current = profile(&[800, 1600], 1);
        let plan = build_plan(
            &current,
            &OnboardEdit {
                slot: 1,
                dpi_stages: Some(vec![400, 800, 1600]),
                report_rate_hz: Some(500),
                ..Default::default()
            },
            4,
        )
        .unwrap();

        let fields: Vec<&str> = plan.changes.iter().map(|c| c.field.as_str()).collect();
        assert!(fields.contains(&"DPI stages"));
        assert!(fields.contains(&"Report rate"));
        assert!(!fields.contains(&"Button 1"), "untouched buttons must not appear");
        let rate = plan.changes.iter().find(|c| c.field == "Report rate").unwrap();
        assert_eq!((rate.before.as_str(), rate.after.as_str()), ("1000 Hz", "500 Hz"));
        // The new sector must be self-consistent, or the firmware rejects it.
        assert!(crc::is_valid(&plan.after));
    }

    #[test]
    fn an_unrepresentable_report_rate_is_refused() {
        // 8000 Hz is a real Logitech rate but not one this sector format can store: the field is
        // a period in whole milliseconds.
        let current = profile(&[800], 1);
        for hz in [8000, 999, 0, 2000] {
            assert!(
                build_plan(
                    &current,
                    &OnboardEdit { slot: 1, report_rate_hz: Some(hz), ..Default::default() },
                    4
                )
                .is_err(),
                "{hz} Hz must be refused"
            );
        }
    }

    #[test]
    fn dpi_stage_lists_are_bounds_checked() {
        let current = profile(&[800], 1);
        let try_stages = |stages: Vec<u32>| {
            build_plan(
                &current,
                &OnboardEdit { slot: 1, dpi_stages: Some(stages), ..Default::default() },
                4,
            )
        };
        assert!(try_stages(vec![]).is_err(), "an empty list would blank every stage");
        assert!(try_stages(vec![400; 6]).is_err(), "only five stages fit the sector");
        assert!(try_stages(vec![49]).is_err(), "below the floor");
        assert!(try_stages(vec![50_001]).is_err(), "above the ceiling");
        assert!(try_stages(vec![400, 800, 1600, 3200, 6400]).is_ok());
    }

    /// Mirrors `OnboardWriterTests`: six stages must be rejected rather than truncated.
    #[test]
    fn shrinking_the_stage_list_clamps_the_default_and_shift_indexes() {
        // Going from five stages to two must not leave the default pointing at stage 5, which no
        // longer exists; the firmware would then select an empty stage.
        let current = profile(&[400, 800, 1600, 3200, 6400], 1);
        let plan = build_plan(
            &current,
            &OnboardEdit {
                slot: 1,
                dpi_stages: Some(vec![400, 800]),
                default_dpi_index: Some(4),
                shift_dpi_index: Some(4),
                ..Default::default()
            },
            4,
        )
        .unwrap();
        assert_eq!(plan.after[1], 1, "default clamped to the last remaining stage");
        assert_eq!(plan.after[2], 1);
    }

    #[test]
    fn a_button_index_past_the_device_count_is_refused() {
        let current = profile(&[800], 1);
        let mut buttons = HashMap::new();
        buttons.insert(9usize, [0x90, 0x0B, 0x00, 0x00]);
        assert!(
            build_plan(
                &current,
                &OnboardEdit { slot: 1, buttons: Some(buttons), ..Default::default() },
                4
            )
            .is_err(),
            "writing past the button table would corrupt the name field"
        );
    }

    #[test]
    fn an_encoded_button_lands_at_its_own_offset_and_nowhere_else() {
        let current = profile(&[800], 1);
        let mut buttons = HashMap::new();
        buttons.insert(2usize, [0x90, 0x07, 0x00, 0x00]);
        let plan = build_plan(
            &current,
            &OnboardEdit { slot: 1, buttons: Some(buttons), ..Default::default() },
            4,
        )
        .unwrap();
        assert_eq!(&plan.after[40..44], &[0x90, 0x07, 0x00, 0x00]);
        // Button 0 must be untouched.
        assert_eq!(&plan.after[32..36], &[0x80, 0x01, 0x00, 0x01]);
        let button3 = plan.changes.iter().find(|c| c.field == "Button 3").unwrap();
        assert_eq!(button3.after, "DPI shift");
    }

    #[test]
    fn a_plan_is_byte_identical_across_runs() {
        // Plans are shown to the user and then committed; two builds of the same edit must not
        // differ because of hash iteration order.
        let current = profile(&[800, 1600], 1);
        let mut buttons = HashMap::new();
        for i in 0..4usize {
            buttons.insert(i, [0x90, 0x03 + i as u8, 0x00, 0x00]);
        }
        let edit =
            OnboardEdit { slot: 1, buttons: Some(buttons), ..Default::default() };
        let first = build_plan(&current, &edit, 4).unwrap();
        for _ in 0..20 {
            assert_eq!(build_plan(&current, &edit, 4).unwrap(), first);
        }
    }

    /// Every commit refusal, exercised through the real function rather than a copy of it. Each
    /// one reaching the device would be a flash write we did not intend.
    #[test]
    fn commit_refusals_fire_before_anything_is_sent() {
        let current = profile(&[800], 1);
        let plan = build_plan(
            &current,
            &OnboardEdit { slot: 1, report_rate_hz: Some(500), ..Default::default() },
            4,
        )
        .unwrap();

        let mut snapshot = OnboardSnapshot::default();
        snapshot.sectors_hex.insert(0x0001, String::new());
        let check = check_commit_preconditions;

        assert_eq!(
            check(&capability(false, 0x04), &plan, &snapshot),
            Some(CommitRefusal::WritesDisabled)
        );
        assert_eq!(
            check(&capability(true, 0x09), &plan, &snapshot),
            Some(CommitRefusal::UnsupportedFormat(0x09))
        );
        assert_eq!(
            check(&capability(true, 0x04), &plan, &OnboardSnapshot::default()),
            Some(CommitRefusal::NoBackup(0x0001))
        );

        let directory = OnboardWritePlan { sector: 0x0000, ..plan.clone() };
        assert_eq!(
            check(&capability(true, 0x04), &directory, &snapshot),
            Some(CommitRefusal::ProtectedSector(0x0000))
        );
        let rom = OnboardWritePlan { sector: 0x0101, ..plan.clone() };
        assert_eq!(
            check(&capability(true, 0x04), &rom, &snapshot),
            Some(CommitRefusal::ProtectedSector(0x0101))
        );

        // A sector whose CRC no longer matches its contents: firmware would reject it, and
        // writing it leaves the slot corrupt.
        let mut corrupt = plan.clone();
        corrupt.after[5] ^= 0xFF;
        assert_eq!(
            check(&capability(true, 0x04), &corrupt, &snapshot),
            Some(CommitRefusal::PlanInvalid)
        );

        // A well-formed sector of the wrong length: writing it would leave the tail of the old
        // sector in place behind valid-looking data.
        let mut short = plan.clone();
        short.after.truncate(128);
        crc::seal(&mut short.after);
        assert!(crc::is_valid(&short.after), "the CRC is fine; only the length is wrong");
        assert_eq!(
            check(&capability(true, 0x04), &short, &snapshot),
            Some(CommitRefusal::PlanInvalid)
        );

        // Everything in order.
        assert_eq!(check(&capability(true, 0x04), &plan, &snapshot), None);
    }

    #[test]
    fn every_refusal_says_something_actionable() {
        // These strings land in a dialog, so "error" is not good enough.
        for refusal in [
            CommitRefusal::WritesDisabled,
            CommitRefusal::UnsupportedFormat(9),
            CommitRefusal::ProtectedSector(0),
            CommitRefusal::NoBackup(1),
            CommitRefusal::PlanInvalid,
        ] {
            let text = refusal.to_string();
            assert!(text.len() > 20, "{refusal:?} reads as {text:?}");
        }
    }

    #[test]
    fn only_profile_format_four_is_writable() {
        // Reading tolerates formats 1 to 6; writing accepts only the one whose layout is verified.
        assert_eq!(SUPPORTED_PROFILE_FORMATS, [0x04]);
        for format in [0x01u8, 0x02, 0x03, 0x05, 0x06] {
            assert!(
                !SUPPORTED_PROFILE_FORMATS.contains(&format),
                "format {format} is readable but must not be writable"
            );
        }
    }
}
