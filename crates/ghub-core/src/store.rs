//! Atomic persistence and validation for [`ProfileDocument`]. Port of
//! `Artemis.Peripherals/Profiles/ProfileStore.cs`.
//!
//! Three jobs, in order of how much trouble they save:
//!
//! 1. **Never lose a profile set.** Writes go to a temporary file, are flushed to disk, and only
//!    then replace the live file, with the previous contents kept as a timestamped backup. A
//!    power cut mid-save leaves either the old file or the new one, never half of either.
//! 2. **Always start.** A corrupt main file is quarantined and the newest readable backup is
//!    loaded instead. Failing to start because of a bad JSON file is not an option a peripheral
//!    manager gets to take.
//! 3. **Treat every document as untrusted.** These files are hand-edited and imported. Validation
//!    runs on load *and* save, and reports every problem at once rather than the first.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::action::{ActionDefinition, MacroDefinition, MacroStep};
use crate::id::Id;
use crate::profile::{
    AnimationSettings, AppMatchKind, OnboardSnapshot, ProfileDocument, is_hex_color,
};
use crate::resolver::MAX_PATTERN_LENGTH;
use crate::time::format_stamp;

pub const MAX_IMPORT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_PROFILES: usize = 500;
pub const MAX_MACROS: usize = 2000;
pub const MAX_TEXT_LENGTH: usize = 10_000;
pub const MAX_CONDITION_DEPTH: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Every problem found, not just the first: a user fixing a hand-edited file should not have
    /// to save and reload once per mistake.
    #[error("profile validation failed: {}", .0.join("; "))]
    Validation(Vec<String>),

    #[error("profile schema {found} is newer than this application supports ({supported})")]
    SchemaTooNew { found: u32, supported: u32 },

    #[error("not valid JSON: {0}")]
    Json(String),

    #[error("{0}")]
    Io(#[from] std::io::Error),

    /// A lock this process holds was poisoned by an earlier panic. Reported rather than panicked
    /// on, so one failed save does not take the whole application with it.
    #[error("{0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

pub struct ProfileStore {
    directory: PathBuf,
    backups_to_keep: usize,
}

impl ProfileStore {
    pub fn new(directory: impl Into<PathBuf>, backups_to_keep: usize) -> std::io::Result<Self> {
        let directory = directory.into();
        std::fs::create_dir_all(&directory)?;
        let store = Self { directory, backups_to_keep };
        std::fs::create_dir_all(store.backup_directory())?;
        Ok(store)
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn file_path(&self) -> PathBuf {
        self.directory.join("peripheral-profiles.json")
    }

    pub fn backup_directory(&self) -> PathBuf {
        self.directory.join("backups")
    }

    pub fn snapshot_directory(&self) -> PathBuf {
        self.directory.join("onboard-snapshots")
    }

    /// Loads the document, falling back through the backups and finally to a fresh default.
    ///
    /// Infallible by design. The second return value lists what went wrong on the way, for the
    /// diagnostics page: a silent fallback to defaults is how a user loses a profile set without
    /// ever being told.
    pub fn load(&self) -> (ProfileDocument, Vec<String>) {
        let mut notes = Vec::new();
        let path = self.file_path();
        if !path.exists() {
            return (ProfileDocument::create_default(), notes);
        }

        match std::fs::read_to_string(&path).map_err(StoreError::from).and_then(|t| parse(&t)) {
            Ok(document) => return (document, notes),
            Err(e) => {
                notes.push(format!("{} could not be loaded: {e}", path.display()));
                // Keep the bad file for inspection rather than letting the next save overwrite
                // it. A user whose profiles vanished wants the broken file, not an apology.
                let quarantine = path.with_extension(format!("json.corrupt-{}", stamp()));
                match std::fs::copy(&path, &quarantine) {
                    Ok(_) => notes.push(format!("kept a copy at {}", quarantine.display())),
                    Err(e) => notes.push(format!("could not quarantine the bad file: {e}")),
                }
            }
        }

        for backup in self.list_backups() {
            match std::fs::read_to_string(&backup).map_err(StoreError::from).and_then(|t| parse(&t))
            {
                Ok(document) => {
                    notes.push(format!("loaded the backup {}", backup.display()));
                    return (document, notes);
                }
                Err(e) => notes.push(format!("backup {} is also bad: {e}", backup.display())),
            }
        }

        notes.push("started from a fresh default profile set".to_owned());
        (ProfileDocument::create_default(), notes)
    }

    /// Validates, then writes atomically, keeping the previous contents as a backup.
    pub fn save(&self, document: &ProfileDocument) -> Result<()> {
        validate(document)?;
        let json = serde_json::to_string_pretty(document)
            .map_err(|e| StoreError::Json(e.to_string()))?;

        let path = self.file_path();
        let temp = path.with_extension("json.tmp");
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&temp)?;
            file.write_all(json.as_bytes())?;
            // Without this the rename can land before the bytes do, and a power cut leaves an
            // empty file where the profiles used to be.
            file.sync_all()?;
        }

        if path.exists() {
            let backup = self
                .backup_directory()
                .join(format!("peripheral-profiles-{}.json", stamp_millis()));
            // Copy-then-rename rather than a three-way replace: `std::fs` has no equivalent of
            // `File.Replace`, and this ordering means the live file is only ever replaced by a
            // complete temp file, with the old contents already safely copied aside.
            std::fs::copy(&path, &backup)?;
            std::fs::rename(&temp, &path)?;
            self.prune_backups();
        } else {
            std::fs::rename(&temp, &path)?;
        }
        Ok(())
    }

    /// Backups, newest first.
    pub fn list_backups(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(self.backup_directory()) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| {
                        n.starts_with("peripheral-profiles-") && n.ends_with(".json")
                    })
            })
            .collect();
        // The stamp is fixed-width and ordered, so a plain descending sort is newest-first.
        files.sort_by(|a, b| b.cmp(a));
        files
    }

    pub fn restore_backup(&self, backup: &Path) -> Result<ProfileDocument> {
        let document = parse(&std::fs::read_to_string(backup)?)?;
        self.save(&document)?;
        Ok(document)
    }

    fn prune_backups(&self) {
        for old in self.list_backups().into_iter().skip(self.backups_to_keep) {
            // A backup that will not delete is not worth failing a save over.
            let _ = std::fs::remove_file(old);
        }
    }

    /// Writes a flash snapshot. Separate from the profile file: a flash image belongs to one
    /// physical device and must never travel with a profile a user copies to another machine.
    pub fn save_snapshot(&self, snapshot: &OnboardSnapshot) -> Result<PathBuf> {
        std::fs::create_dir_all(self.snapshot_directory())?;
        let safe: String = snapshot
            .device_key
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let path = self.snapshot_directory().join(format!("{safe}-{}.json", stamp()));
        let json =
            serde_json::to_string_pretty(snapshot).map_err(|e| StoreError::Json(e.to_string()))?;
        std::fs::write(&path, json)?;
        Ok(path)
    }

    pub fn list_snapshots(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(self.snapshot_directory()) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        files.sort_by(|a, b| b.cmp(a));
        files
    }
}

fn stamp() -> String {
    format_stamp(crate::profile::now_unix(), 0)
}

fn stamp_millis() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format_stamp(now.as_secs() as i64, now.subsec_millis())
}

/// Parses, migrates and validates a document from untrusted JSON.
pub fn parse(json: &str) -> Result<ProfileDocument> {
    if json.len() > MAX_IMPORT_BYTES {
        return Err(StoreError::Validation(vec![format!(
            "file is {} bytes, over the {MAX_IMPORT_BYTES} byte limit",
            json.len()
        )]));
    }
    let mut root: serde_json::Value =
        serde_json::from_str(json).map_err(|e| StoreError::Json(e.to_string()))?;
    if !root.is_object() {
        return Err(StoreError::Validation(vec!["root must be an object".to_owned()]));
    }

    let version = root
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1) as u32;
    if version > crate::profile::CURRENT_SCHEMA_VERSION {
        return Err(StoreError::SchemaTooNew {
            found: version,
            supported: crate::profile::CURRENT_SCHEMA_VERSION,
        });
    }
    migrate(&mut root, version);

    let document: ProfileDocument =
        serde_json::from_value(root).map_err(|e| StoreError::Json(e.to_string()))?;
    validate(&document)?;
    Ok(document)
}

/// v1 stored one `dpi` list per mouse; v2 uses `dpiStages` with an enabled flag each.
pub fn migrate(root: &mut serde_json::Value, from_version: u32) {
    if from_version < 2 {
        if let Some(profiles) = root.get_mut("profiles").and_then(|p| p.as_array_mut()) {
            for profile in profiles {
                let Some(devices) = profile.get_mut("devices").and_then(|d| d.as_object_mut())
                else {
                    continue;
                };
                for device in devices.values_mut() {
                    let Some(mouse) = device.get_mut("mouse").and_then(|m| m.as_object_mut())
                    else {
                        continue;
                    };
                    let Some(dpi) = mouse.remove("dpi") else { continue };
                    let stages: Vec<serde_json::Value> = dpi
                        .as_array()
                        .map(|values| {
                            values
                                .iter()
                                .map(|v| {
                                    serde_json::json!({
                                        "dpi": v.as_u64().unwrap_or(800),
                                        "enabled": true
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    mouse.insert("dpiStages".to_owned(), serde_json::Value::Array(stages));
                }
            }
        }
    }
    if let Some(object) = root.as_object_mut() {
        object.insert(
            "schemaVersion".to_owned(),
            serde_json::json!(crate::profile::CURRENT_SCHEMA_VERSION),
        );
    }
}

/// Checks a document and returns every problem found.
pub fn validate(doc: &ProfileDocument) -> Result<()> {
    let mut errors = Vec::new();

    if doc.profiles.is_empty() {
        errors.push("at least one profile is required".to_owned());
    }
    if doc.profiles.len() > MAX_PROFILES {
        errors.push(format!("too many profiles ({} > {MAX_PROFILES})", doc.profiles.len()));
    }
    if doc.macros.len() > MAX_MACROS {
        errors.push(format!("too many macros ({} > {MAX_MACROS})", doc.macros.len()));
    }
    if doc.profiles.iter().map(|p| p.id).collect::<HashSet<_>>().len() != doc.profiles.len() {
        errors.push("duplicate profile ids".to_owned());
    }
    if doc.profiles.iter().filter(|p| p.is_default).count() > 1 {
        errors.push("more than one default profile".to_owned());
    }

    let macro_ids: HashSet<Id> = doc.macros.iter().map(|m| m.id).collect();

    for profile in &doc.profiles {
        let name = &profile.name;
        if name.trim().is_empty() || name.chars().count() > 200 {
            errors.push(format!("profile name invalid ({})", profile.id));
        }

        for rule in &profile.rules {
            if let Some(pattern) = &rule.title_pattern {
                if pattern.len() > MAX_PATTERN_LENGTH {
                    errors.push(format!("title pattern too long in '{name}'"));
                } else if regex_lite::Regex::new(pattern).is_err() {
                    errors.push(format!("invalid title pattern '{pattern}' in '{name}'"));
                }
            }
            let blank = |value: &Option<String>| {
                value.as_ref().is_none_or(|v| v.trim().is_empty())
            };
            if rule.kind == AppMatchKind::ExactPath && blank(&rule.executable_path) {
                errors.push(format!("exact-path rule without a path in '{name}'"));
            }
            if matches!(
                rule.kind,
                AppMatchKind::ExecutableName | AppMatchKind::ExecutableAndTitle
            ) && blank(&rule.executable_name)
            {
                errors.push(format!("executable rule without a name in '{name}'"));
            }
            if matches!(rule.kind, AppMatchKind::TitleOnly | AppMatchKind::ExecutableAndTitle)
                && blank(&rule.title_pattern)
            {
                errors.push(format!("title rule without a pattern in '{name}'"));
            }
        }

        for (key, device) in &profile.devices {
            if key.len() > 200 {
                errors.push("device key too long".to_owned());
            }
            if let Some(mouse) = &device.mouse {
                if mouse.dpi_stages.is_empty() || mouse.dpi_stages.len() > 5 {
                    errors.push(format!("DPI stages must be 1-5 in '{name}'"));
                }
                if mouse.dpi_stages.iter().any(|s| s.dpi < 50 || s.dpi > 50_000) {
                    errors.push(format!("DPI out of range in '{name}'"));
                }
                if let Some(hz) = mouse.report_rate_hz {
                    if !matches!(hz, 125 | 250 | 500 | 1000 | 2000 | 4000 | 8000) {
                        errors.push(format!("unsupported report rate {hz}"));
                    }
                }
            }
            if let Some(keyboard) = &device.keyboard {
                if keyboard.key_reactions.len() > 200 {
                    errors.push(format!("too many key reactions in '{name}'"));
                }
                for reaction in &keyboard.key_reactions {
                    if reaction.key_id.trim().is_empty() || reaction.key_id.len() > 40 {
                        errors.push(format!("key reaction without a key in '{name}'"));
                    }
                    if !is_hex_color(&reaction.color) {
                        errors.push(format!(
                            "invalid key-reaction colour '{}' in '{name}'",
                            reaction.color
                        ));
                    }
                }
                let distinct: HashSet<&str> =
                    keyboard.key_reactions.iter().map(|r| r.key_id.as_str()).collect();
                if distinct.len() != keyboard.key_reactions.len() {
                    errors.push(format!("the same key has more than one reaction in '{name}'"));
                }
            }

            let lighting = &device.lighting;
            if !(0.0..=1.0).contains(&lighting.brightness) {
                errors.push(format!("brightness out of range in '{name}'"));
            }
            // Dimming after the lights have already turned off means the dim never happens; a
            // user who set it that way meant something else.
            if let (Some(dim), Some(off)) =
                (lighting.power_save_idle_seconds, lighting.power_save_sleep_seconds)
            {
                if dim > 0 && off > 0 && dim > off {
                    errors.push(format!(
                        "lighting dims after it turns off in '{name}' (dim {dim}s > off {off}s)"
                    ));
                }
            }
            for color in lighting.static_colors.values() {
                if !is_hex_color(color) {
                    errors.push(format!("invalid colour '{color}'"));
                }
            }
            validate_animation(&lighting.animation, name, &mut errors);

            for (slot, binding) in &device.bindings {
                if slot.len() > 100 {
                    errors.push("binding slot id too long".to_owned());
                }
                validate_action(
                    &binding.action,
                    &macro_ids,
                    &mut errors,
                    &format!("'{name}' {slot}"),
                );
            }
        }
    }

    if doc.app_lighting.len() > 500 {
        errors.push("too many per-application lighting rules".to_owned());
    }
    if doc.app_lighting.iter().map(|r| r.id).collect::<HashSet<_>>().len() != doc.app_lighting.len()
    {
        errors.push("duplicate per-application lighting rule ids".to_owned());
    }
    for rule in &doc.app_lighting {
        if rule.name.chars().count() > 200 {
            errors.push("per-application lighting rule name too long".to_owned());
        }
        if rule.executable_name.trim().is_empty() || rule.executable_name.len() > 260 {
            errors.push(format!(
                "per-application lighting rule '{}' needs an executable name",
                rule.name
            ));
        }
        for (key, light) in &rule.devices {
            if key.len() > 200 {
                errors.push("device key too long".to_owned());
            }
            if let Some(brightness) = light.brightness {
                if !(0.0..=1.0).contains(&brightness) {
                    errors.push(format!("brightness out of range in '{}'", rule.name));
                }
            }
            if let Some(fallback) = &light.default_color {
                if !is_hex_color(fallback) {
                    errors.push(format!("invalid colour '{fallback}' in '{}'", rule.name));
                }
            }
            for color in light.static_colors.values() {
                if !is_hex_color(color) {
                    errors.push(format!("invalid colour '{color}' in '{}'", rule.name));
                }
            }
            if let Some(animation) = &light.animation {
                validate_animation(animation, &rule.name, &mut errors);
            }
        }
    }

    if doc.triggers.len() > 500 {
        errors.push("too many triggers".to_owned());
    }
    for trigger in &doc.triggers {
        if trigger.name.trim().is_empty()
            || trigger.name.chars().count() > 200
            || trigger.r#match.len() > 260
        {
            errors.push(format!("invalid trigger '{}'", trigger.name));
        }
        if !(0..=86_400).contains(&trigger.cooldown_seconds)
            || !(0..=50_000).contains(&trigger.threshold)
        {
            errors.push(format!("trigger '{}' parameter out of range", trigger.name));
        }
        // A trigger fires without the user touching anything, so a trigger that can start a
        // program is a way to make this application run arbitrary code on a device event. Not
        // offered, in either direction.
        if matches!(trigger.action, ActionDefinition::Launch { .. }) {
            errors.push(format!(
                "trigger '{}': launching programs from triggers is not allowed",
                trigger.name
            ));
        }
        validate_action(
            &trigger.action,
            &macro_ids,
            &mut errors,
            &format!("trigger '{}'", trigger.name),
        );
    }

    for m in &doc.macros {
        if m.steps.len() > MacroDefinition::MAX_STEPS {
            errors.push(format!("macro '{}' has too many steps", m.name));
        }
        if m.repeat_count < 1 || m.repeat_count > MacroDefinition::MAX_REPEAT_COUNT {
            errors.push(format!("macro '{}' repeat count out of range", m.name));
        }
        if m.max_loop_iterations < 1 || m.max_loop_iterations > 100_000 {
            errors.push(format!("macro '{}' loop bound out of range", m.name));
        }
        if m.repeat_delay_milliseconds > MacroDefinition::MAX_DELAY_MILLISECONDS
            || m.fixed_delay_milliseconds > MacroDefinition::MAX_DELAY_MILLISECONDS
        {
            errors.push(format!("macro '{}' delay out of range", m.name));
        }
        validate_steps(&m.steps, &macro_ids, &mut errors, &m.name, 0);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(StoreError::Validation(errors))
    }
}

fn validate_steps(
    steps: &[MacroStep],
    macro_ids: &HashSet<Id>,
    errors: &mut Vec<String>,
    name: &str,
    depth: usize,
) {
    if depth > MAX_CONDITION_DEPTH {
        errors.push(format!("macro '{name}' nests conditions too deeply"));
        return;
    }
    for step in steps {
        match step {
            MacroStep::Delay { milliseconds }
                if *milliseconds > MacroDefinition::MAX_DELAY_MILLISECONDS =>
            {
                errors.push(format!("macro '{name}' has an out-of-range delay"));
            }
            MacroStep::Text { text } if text.chars().count() > MAX_TEXT_LENGTH => {
                errors.push(format!("macro '{name}' text too long"));
            }
            MacroStep::MouseMove { dx, dy } if dx.abs() > 10_000 || dy.abs() > 10_000 => {
                errors.push(format!("macro '{name}' mouse move too large"));
            }
            MacroStep::Action { action } => {
                validate_action(action, macro_ids, errors, &format!("macro '{name}'"));
            }
            MacroStep::Condition { value, then, otherwise, .. } => {
                if value.len() > 260 {
                    errors.push(format!("macro '{name}' condition value too long"));
                }
                validate_steps(then, macro_ids, errors, name, depth + 1);
                if let Some(otherwise) = otherwise {
                    validate_steps(otherwise, macro_ids, errors, name, depth + 1);
                }
            }
            _ => {}
        }
    }
}

/// An animation is checked but never rejected for naming an effect this build does not have: the
/// renderer falls back to its first effect, which keeps a profile from a newer build loadable.
fn validate_animation(animation: &AnimationSettings, where_: &str, errors: &mut Vec<String>) {
    if !(0.05..=10.0).contains(&animation.speed) {
        errors.push(format!("animation speed out of range in '{where_}'"));
    }
    if !(1..=60).contains(&animation.frames_per_second) {
        errors.push(format!("animation frame rate out of range in '{where_}'"));
    }
    if !(0.0..=2.0).contains(&animation.intensity) {
        errors.push(format!("animation intensity out of range in '{where_}'"));
    }
    if animation.colors.len() > 8 {
        errors.push(format!("too many animation colours in '{where_}'"));
    }
    for color in &animation.colors {
        if !is_hex_color(color) {
            errors.push(format!("invalid animation colour '{color}' in '{where_}'"));
        }
    }
}

fn validate_action(
    action: &ActionDefinition,
    macro_ids: &HashSet<Id>,
    errors: &mut Vec<String>,
    where_: &str,
) {
    match action {
        ActionDefinition::Text { text } if text.chars().count() > MAX_TEXT_LENGTH => {
            errors.push(format!("text too long at {where_}"));
        }
        // A dangling macro reference is a binding that silently does nothing, which is worse
        // than a load error the user can act on.
        ActionDefinition::MacroRef { macro_id } if !macro_ids.contains(macro_id) => {
            errors.push(format!("unknown macro reference at {where_}"));
        }
        ActionDefinition::Launch { target, .. }
            if target.trim().is_empty() || target.len() > 2048 =>
        {
            errors.push(format!("invalid launch target at {where_}"));
        }
        ActionDefinition::Chord { steps } if steps.is_empty() || steps.len() > 32 => {
            errors.push(format!("invalid key chord at {where_}"));
        }
        ActionDefinition::Dpi { command: crate::action::DpiCommand::Stage, stage }
            if *stage > 4 =>
        {
            errors.push(format!("invalid DPI stage at {where_}"));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{ActionBinding, KeyStroke};
    use crate::profile::{
        AppRule, DpiStage, KeyReaction, KeyReactionMode, MouseSettings, TriggerEvent, TriggerRule,
    };

    /// A unique scratch directory per test, under the OS temp dir.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("ghub-store-test-{name}-{}", Id::new()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn store(&self) -> ProfileStore {
            ProfileStore::new(&self.0, 3).unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_missing_file_loads_a_default_set_without_complaining() {
        let scratch = Scratch::new("missing");
        let (doc, notes) = scratch.store().load();
        assert_eq!(doc.profiles.len(), 1);
        assert!(notes.is_empty(), "a first run is not a problem to report: {notes:?}");
    }

    #[test]
    fn a_saved_document_round_trips() {
        let scratch = Scratch::new("roundtrip");
        let store = scratch.store();
        let mut doc = ProfileDocument::create_default();
        doc.profiles[0].rules.push(AppRule::by_executable("game.exe"));
        store.save(&doc).unwrap();

        let (loaded, notes) = store.load();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(loaded.profiles[0].rules.len(), 1);
        assert_eq!(loaded.profiles[0].id, doc.profiles[0].id);
    }

    #[test]
    fn saving_twice_keeps_the_previous_contents_as_a_backup() {
        let scratch = Scratch::new("backup");
        let store = scratch.store();
        let mut doc = ProfileDocument::create_default();
        store.save(&doc).unwrap();
        assert!(store.list_backups().is_empty(), "the first save has nothing to back up");

        doc.profiles[0].name = "renamed".into();
        store.save(&doc).unwrap();
        let backups = store.list_backups();
        assert_eq!(backups.len(), 1);
        let backed_up = parse(&std::fs::read_to_string(&backups[0]).unwrap()).unwrap();
        assert_eq!(backed_up.profiles[0].name, "Desktop (default)", "the pre-save contents");
        assert_eq!(store.load().0.profiles[0].name, "renamed");
    }

    #[test]
    fn backups_are_pruned_to_the_limit_newest_first() {
        let scratch = Scratch::new("prune");
        let store = scratch.store();
        let mut doc = ProfileDocument::create_default();
        for i in 0..8 {
            doc.profiles[0].name = format!("save {i}");
            store.save(&doc).unwrap();
            // The stamp has millisecond resolution; without this two saves can share a name and
            // the second would overwrite the first rather than adding to the list.
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let backups = store.list_backups();
        assert_eq!(backups.len(), 3, "backups_to_keep is 3: {backups:?}");
        // Newest first, so the first entry holds the most recent pre-save contents.
        let newest = parse(&std::fs::read_to_string(&backups[0]).unwrap()).unwrap();
        assert_eq!(newest.profiles[0].name, "save 6");
    }

    #[test]
    fn a_corrupt_file_is_quarantined_and_the_backup_is_loaded() {
        let scratch = Scratch::new("corrupt");
        let store = scratch.store();
        let mut doc = ProfileDocument::create_default();
        store.save(&doc).unwrap();
        doc.profiles[0].name = "good backup".into();
        store.save(&doc).unwrap();

        std::fs::write(store.file_path(), "{ this is not json").unwrap();
        let (loaded, notes) = store.load();

        assert_eq!(loaded.profiles[0].name, "Desktop (default)", "from the backup");
        assert!(notes.iter().any(|n| n.contains("could not be loaded")), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("kept a copy at")), "{notes:?}");
        // The bad file is still on disk for inspection.
        let quarantined: Vec<_> = std::fs::read_dir(store.directory())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("corrupt"))
            .collect();
        assert_eq!(quarantined.len(), 1, "the broken file must be kept, not silently dropped");
    }

    #[test]
    fn everything_unreadable_still_starts_the_application() {
        let scratch = Scratch::new("hopeless");
        let store = scratch.store();
        store.save(&ProfileDocument::create_default()).unwrap();
        store.save(&ProfileDocument::create_default()).unwrap();
        std::fs::write(store.file_path(), "garbage").unwrap();
        for backup in store.list_backups() {
            std::fs::write(backup, "also garbage").unwrap();
        }

        let (loaded, notes) = store.load();
        assert_eq!(loaded.profiles.len(), 1);
        assert!(notes.iter().any(|n| n.contains("fresh default")), "{notes:?}");
    }

    #[test]
    fn a_v1_document_migrates_its_dpi_list_to_stages() {
        let json = r#"{
            "schemaVersion": 1,
            "profiles": [{
                "id": "01",
                "name": "old",
                "isDefault": true,
                "devices": {
                    "logitech-hidpp:u1": { "mouse": { "dpi": [400, 800, 1600] } }
                }
            }]
        }"#;
        let doc = parse(json).unwrap();
        assert_eq!(doc.schema_version, crate::profile::CURRENT_SCHEMA_VERSION);
        let mouse = doc.profiles[0].devices["logitech-hidpp:u1"].mouse.as_ref().unwrap();
        assert_eq!(
            mouse.dpi_stages,
            vec![DpiStage::new(400), DpiStage::new(800), DpiStage::new(1600)]
        );
        assert!(mouse.dpi_stages.iter().all(|s| s.enabled), "migrated stages start enabled");
    }

    #[test]
    fn a_document_from_a_newer_build_is_refused_rather_than_half_read() {
        let json = r#"{"schemaVersion": 99, "profiles": [{"id":"01"}]}"#;
        match parse(json) {
            Err(StoreError::SchemaTooNew { found, supported }) => {
                assert_eq!(found, 99);
                assert_eq!(supported, crate::profile::CURRENT_SCHEMA_VERSION);
            }
            other => panic!("expected a schema error, got {other:?}"),
        }
    }

    #[test]
    fn validation_reports_every_problem_at_once() {
        let mut doc = ProfileDocument::create_default();
        doc.profiles[0].name = "   ".into();
        doc.profiles[0].rules.push(AppRule {
            kind: AppMatchKind::ExactPath,
            executable_path: None,
            ..AppRule::by_executable("x")
        });
        {
            let device = doc.profiles[0].for_device("k");
            device.lighting.brightness = 5.0;
            device.lighting.static_colors.insert("key:A".into(), "red".into());
            device.mouse = Some(MouseSettings {
                dpi_stages: vec![DpiStage::new(10)],
                report_rate_hz: Some(999),
                ..MouseSettings::default()
            });
        }

        let Err(StoreError::Validation(errors)) = validate(&doc) else {
            panic!("expected validation to fail");
        };
        let joined = errors.join(" | ");
        assert!(joined.contains("profile name invalid"), "{joined}");
        assert!(joined.contains("exact-path rule without a path"), "{joined}");
        assert!(joined.contains("brightness out of range"), "{joined}");
        assert!(joined.contains("invalid colour 'red'"), "{joined}");
        assert!(joined.contains("DPI out of range"), "{joined}");
        assert!(joined.contains("unsupported report rate 999"), "{joined}");
        assert!(errors.len() >= 6, "one error per problem, not just the first: {errors:?}");
    }

    #[test]
    fn a_dangling_macro_reference_is_refused() {
        let mut doc = ProfileDocument::create_default();
        doc.profiles[0].for_device("k").bindings.insert(
            "gkey:G1:M1".into(),
            ActionBinding::new(ActionDefinition::MacroRef { macro_id: Id::new() }),
        );
        let Err(StoreError::Validation(errors)) = validate(&doc) else {
            panic!("a binding that does nothing must not load silently");
        };
        assert!(errors.iter().any(|e| e.contains("unknown macro reference")), "{errors:?}");

        // With the macro present it validates.
        let m = MacroDefinition::new("m");
        let id = m.id;
        doc.macros.push(m);
        doc.profiles[0].for_device("k").bindings.insert(
            "gkey:G1:M1".into(),
            ActionBinding::new(ActionDefinition::MacroRef { macro_id: id }),
        );
        validate(&doc).unwrap();
    }

    #[test]
    fn a_trigger_may_not_launch_a_program() {
        let mut doc = ProfileDocument::create_default();
        let mut trigger = TriggerRule::new("on connect", TriggerEvent::DeviceConnected);
        trigger.action =
            ActionDefinition::Launch { target: "cmd.exe".into(), arguments: None };
        doc.triggers.push(trigger);

        let Err(StoreError::Validation(errors)) = validate(&doc) else {
            panic!("a device event must not be able to start a program");
        };
        assert!(errors.iter().any(|e| e.contains("not allowed")), "{errors:?}");
    }

    #[test]
    fn an_invalid_title_pattern_is_caught_at_validation_rather_than_at_match_time() {
        let mut doc = ProfileDocument::create_default();
        doc.profiles[0].rules.push(AppRule {
            kind: AppMatchKind::TitleOnly,
            executable_name: None,
            title_pattern: Some("(unclosed".into()),
            ..AppRule::by_executable("x")
        });
        let Err(StoreError::Validation(errors)) = validate(&doc) else {
            panic!("expected the bad pattern to be reported");
        };
        assert!(errors.iter().any(|e| e.contains("invalid title pattern")), "{errors:?}");
    }

    #[test]
    fn two_reactions_on_one_key_are_refused() {
        let mut doc = ProfileDocument::create_default();
        let keyboard = crate::profile::KeyboardSettings {
            key_reactions: vec![
                KeyReaction {
                    key_id: "W".into(),
                    color: "#FF0000".into(),
                    mode: KeyReactionMode::Momentary,
                    enabled: true,
                },
                KeyReaction {
                    key_id: "W".into(),
                    color: "#00FF00".into(),
                    mode: KeyReactionMode::Toggle,
                    enabled: true,
                },
            ],
            ..crate::profile::KeyboardSettings::default()
        };
        doc.profiles[0].for_device("k").keyboard = Some(keyboard);
        let Err(StoreError::Validation(errors)) = validate(&doc) else {
            panic!("the renderer cannot honour two reactions on one key");
        };
        assert!(errors.iter().any(|e| e.contains("more than one reaction")), "{errors:?}");
    }

    #[test]
    fn lighting_that_dims_after_it_turns_off_is_refused() {
        let mut doc = ProfileDocument::create_default();
        {
            let lighting = &mut doc.profiles[0].for_device("k").lighting;
            lighting.power_save_idle_seconds = Some(600);
            lighting.power_save_sleep_seconds = Some(60);
        }
        let Err(StoreError::Validation(errors)) = validate(&doc) else {
            panic!("expected the contradictory timers to be reported");
        };
        assert!(errors.iter().any(|e| e.contains("dims after it turns off")), "{errors:?}");
    }

    #[test]
    fn deeply_nested_conditions_are_refused_rather_than_recursed_into() {
        let mut doc = ProfileDocument::create_default();
        let mut step = MacroStep::Delay { milliseconds: 1 };
        for _ in 0..40 {
            step = MacroStep::Condition {
                condition: crate::action::MacroCondition::KeyHeld,
                value: "16".into(),
                then: vec![step],
                otherwise: None,
            };
        }
        let mut m = MacroDefinition::new("deep");
        m.steps = vec![step];
        doc.macros.push(m);

        let Err(StoreError::Validation(errors)) = validate(&doc) else {
            panic!("expected the nesting to be refused");
        };
        assert!(errors.iter().any(|e| e.contains("nests conditions too deeply")), "{errors:?}");
    }

    #[test]
    fn an_oversized_file_is_refused_before_it_is_parsed() {
        let huge = format!("{}{}", " ".repeat(MAX_IMPORT_BYTES + 1), "{}");
        match parse(&huge) {
            Err(StoreError::Validation(errors)) => {
                assert!(errors[0].contains("over the"), "{errors:?}");
            }
            other => panic!("expected a size refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_save_that_would_not_validate_leaves_the_existing_file_alone() {
        let scratch = Scratch::new("refuse");
        let store = scratch.store();
        let good = ProfileDocument::create_default();
        store.save(&good).unwrap();

        let mut bad = good.clone();
        bad.profiles.clear();
        assert!(store.save(&bad).is_err());

        // The good file is untouched, and no temp file is left behind.
        assert_eq!(store.load().0.profiles.len(), 1);
        assert!(!store.file_path().with_extension("json.tmp").exists());
    }

    #[test]
    fn a_snapshot_is_written_outside_the_profile_file() {
        let scratch = Scratch::new("snapshot");
        let store = scratch.store();
        let mut snapshot = OnboardSnapshot {
            device_key: "logitech-hidpp:u0123".into(),
            device_name: "G903".into(),
            taken: 1_700_000_000,
            ..OnboardSnapshot::default()
        };
        snapshot.sectors_hex.insert(0, "DEADBEEF".into());

        let path = store.save_snapshot(&snapshot).unwrap();
        assert!(path.starts_with(store.snapshot_directory()));
        // The device key becomes the file name, with everything unsafe replaced.
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("logitech_hidpp_u0123-"), "{name}");
        assert_eq!(store.list_snapshots().len(), 1);

        let read: OnboardSnapshot =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(read, snapshot);
        // And it is not in the profile document.
        assert!(!std::fs::read_to_string(store.file_path())
            .unwrap_or_default()
            .contains("DEADBEEF"));
    }

    #[test]
    fn the_backup_stamp_is_fixed_width_and_sorts_chronologically() {
        // 2023-11-14T22:13:20Z
        assert_eq!(format_stamp(1_700_000_000, 7), "20231114-221320-007");
        // The epoch itself, and a leap day, because the date maths is hand-rolled.
        assert_eq!(format_stamp(0, 0), "19700101-000000-000");
        assert_eq!(format_stamp(1_583_020_800, 0), "20200301-000000-000");
        assert_eq!(format_stamp(1_582_934_400, 0), "20200229-000000-000");
        assert!(format_stamp(1_700_000_000, 0) > format_stamp(1_699_999_999, 999));
        assert_eq!(format_stamp(0, 0).len(), format_stamp(1_700_000_000, 999).len());
    }

    #[test]
    fn restoring_a_backup_makes_it_the_live_document() {
        let scratch = Scratch::new("restore");
        let store = scratch.store();
        let mut doc = ProfileDocument::create_default();
        doc.profiles[0].name = "original".into();
        store.save(&doc).unwrap();
        doc.profiles[0].name = "changed".into();
        store.save(&doc).unwrap();

        let backup = store.list_backups().remove(0);
        let restored = store.restore_backup(&backup).unwrap();
        assert_eq!(restored.profiles[0].name, "original");
        assert_eq!(store.load().0.profiles[0].name, "original");
    }

    #[test]
    fn a_pass_through_binding_and_an_empty_chord_are_told_apart() {
        let mut doc = ProfileDocument::create_default();
        doc.profiles[0]
            .for_device("k")
            .bindings
            .insert("button:1".into(), ActionBinding::new(ActionDefinition::PassThrough));
        validate(&doc).unwrap();

        doc.profiles[0]
            .for_device("k")
            .bindings
            .insert("button:2".into(), ActionBinding::new(ActionDefinition::Chord { steps: vec![] }));
        let Err(StoreError::Validation(errors)) = validate(&doc) else {
            panic!("an empty chord does nothing and must be reported");
        };
        assert!(errors.iter().any(|e| e.contains("invalid key chord")), "{errors:?}");

        doc.profiles[0].for_device("k").bindings.insert(
            "button:2".into(),
            ActionBinding::new(ActionDefinition::Chord { steps: vec![KeyStroke::new(0x41)] }),
        );
        validate(&doc).unwrap();
    }
}
