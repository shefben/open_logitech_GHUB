//! Read-only G HUB migration. Port of `Import/GHubImporter.cs` and `Import/GHubSettingsReader.cs`.
//!
//! G HUB (observed 2026.6) keeps its whole configuration as one JSON document inside a SQLite
//! database at `%LOCALAPPDATA%\LGHUB\settings.db`, table `data`, column `file`, latest row wins.
//!
//! Three things about this module are deliberate and must stay true:
//!
//! * **G HUB's own file is never opened by the importer.** The bytes are copied out - database,
//!   `-wal` and `-shm` - with the same sharing G HUB itself uses, and the *copy* is what gets
//!   opened, read-only. G HUB may be running, and a migration tool that corrupts the thing it is
//!   migrating from is worse than no migration tool.
//! * **Nothing is guessed.** A card type with no local equivalent becomes an item that says so
//!   and cannot be selected, with the reason in it. The alternative - importing a plausible
//!   approximation - produces a profile the user believes is their G HUB profile and is not.
//! * **The preview is the whole decision.** [`preview`] touches no document; [`apply`] builds a
//!   new one from the items the user left ticked and returns it for the caller to save, so the
//!   store's own backup is the rollback point.
//!
//! ## Divergence from the C#
//!
//! `ImportPreview` carried a `Dictionary<ImportItem, Action<ImportTarget>>`: a closure per item,
//! keyed by the item record. Here each item carries its own [`Applier`], which is data rather than
//! a closure. Three reasons, in order of how much they matter:
//!
//! 1. The applier can be inspected and asserted in a test without running it against a document.
//! 2. Keying a map by a record whose `Selected` the UI mutates is a trap in any language; in Rust
//!    it is not expressible without interior mutability.
//! 3. An item and the edit it performs cannot drift apart, because they are one value.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::Id;
use crate::action::{
    ActionBinding, ActionDefinition, ExecutionSite, KeyStroke, MacroDefinition,
    MacroStep, MouseButtonKind, modifiers_from_hid_usages, virtual_key_from_hid_usage,
};
use crate::profile::{
    AppMatchKind, AppRule, DpiStage, KeyboardSettings, LightingMode, MouseSettings,
    PeripheralProfile, ProfileDocument, ZoneEffect,
};
use crate::store::{self, MAX_IMPORT_BYTES, MAX_TEXT_LENGTH, StoreError};

/// G HUB device slot prefixes that are keyboards, and so number their controls as G-keys. Taken
/// from the C# list unchanged; anything not in it is treated as a mouse.
pub const KNOWN_KEYBOARDS: &[&str] = &[
    "g815", "g915", "g915tkl", "g913", "g813", "g910", "g810", "g815tkl", "g512", "g513", "g613",
    "g715", "g713", "g413", "g213", "g pro",
];

/// The id every G HUB "unassigned" slot points at.
const UNASSIGNED_CARD: &str = "ffffffff-ffff-ffff-ffff-ffffffffffff";

/// Prefix of G HUB's built-in command cards, which are not stored in `settings.db`.
const BUILT_IN_PREFIX: &str = "0f82f693-5b78-4cf5-867e-";

/// The most DPI stages G HUB's table can map onto, matching the local model.
const MAX_DPI_STAGES: usize = 5;

/// What an import item would bring over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportItemKind {
    Profile,
    MouseSettings,
    Keyboard,
    Binding,
    Macro,
    AppRule,
    Lighting,
    Unsupported,
}

impl ImportItemKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Profile => "Profile",
            Self::MouseSettings => "MouseSettings",
            Self::Keyboard => "Keyboard",
            Self::Binding => "Binding",
            Self::Macro => "Macro",
            Self::AppRule => "AppRule",
            Self::Lighting => "Lighting",
            Self::Unsupported => "Unsupported",
        }
    }
}

/// The edit an item performs when it is applied.
///
/// Data, not a closure - see the module note. [`Applier::None`] is an item that is only there to
/// be read: an unsupported card, or a lighting feature that is not converted.
#[derive(Debug, Clone, PartialEq)]
pub enum Applier {
    None,
    /// Added to the profile this item belongs to.
    AppRule(Box<AppRule>),
    /// Replaces that device's mouse settings in the profile.
    Mouse { device_key: String, settings: Box<MouseSettings> },
    /// One binding, keyed by the local slot id.
    Binding { device_key: String, slot: String, binding: Box<ActionBinding> },
    /// Added to the document's macro list if an id like it is not already there.
    Macro(Box<MacroDefinition>),
    /// A G HUB firmware lighting card: one 0x8071 effect per zone, keyed by cluster location.
    ///
    /// Switches the device to [`LightingMode::DeviceEffect`] only when nothing else imported has
    /// claimed its lighting, so a ticked per-key lighting item wins whatever the item order.
    FirmwareLighting { device_key: String, zones: HashMap<u16, ZoneEffect> },
    /// A G HUB fixed (freestyle) per-key effect, as `key:{id}` colours.
    PerKeyLighting { device_key: String, colors: HashMap<String, String> },
    /// The keys game mode locks out, as HID usages.
    GameModeKeys { device_key: String, usages: Vec<u8> },
}

/// One line in the preview list.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportItem {
    pub kind: ImportItemKind,
    /// Where in the G HUB document it came from, such as `profile 3` or a slot id.
    pub source: String,
    pub description: String,
    /// False when there is no local equivalent. An unsupported item cannot be selected.
    pub supported: bool,
    /// Why it is unsupported, or what to check after importing it.
    pub warning: Option<String>,
    /// The UI's tick. Starts equal to `supported`, as the C#'s `Selected` initialiser did.
    pub selected: bool,
    pub applier: Applier,
}

impl ImportItem {
    fn new(
        kind: ImportItemKind,
        source: impl Into<String>,
        description: impl Into<String>,
        supported: bool,
        warning: Option<String>,
        applier: Applier,
    ) -> Self {
        Self {
            kind,
            source: source.into(),
            description: description.into(),
            supported,
            warning,
            selected: supported,
            applier,
        }
    }

    fn unsupported(
        source: impl Into<String>,
        description: impl Into<String>,
        warning: impl Into<String>,
    ) -> Self {
        Self::new(
            ImportItemKind::Unsupported,
            source,
            description,
            false,
            Some(warning.into()),
            Applier::None,
        )
    }
}

/// Everything found in a G HUB document, and nothing applied yet.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportPreview {
    pub source_path: String,
    pub ghub_version: Option<String>,
    pub items: Vec<ImportItem>,
    /// Problems with the document as a whole, rather than with one item.
    pub warnings: Vec<String>,
    /// The profiles the [`ImportItemKind::Profile`] items stand for, in the same order.
    pub profiles: Vec<PeripheralProfile>,
}

impl ImportPreview {
    pub fn supported_count(&self) -> usize {
        self.items.iter().filter(|item| item.supported).count()
    }

    pub fn unsupported_count(&self) -> usize {
        self.items.len() - self.supported_count()
    }

    /// The summary line the page shows, worded as the C#'s was.
    pub fn summary(&self) -> String {
        format!(
            "G HUB {}: {} items, {} importable, {} not convertible (reasons listed). The original \
             file was only read through a temporary copy.",
            self.ghub_version.as_deref().unwrap_or("(version not recorded)"),
            self.items.len(),
            self.supported_count(),
            self.unsupported_count()
        )
    }
}

/// Resolves a G HUB device slot prefix such as `g903hero` onto a local device key.
pub type DeviceKeyResolver = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// `%LOCALAPPDATA%\LGHUB\settings.db`.
pub fn default_settings_path() -> std::path::PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("LGHUB")
        .join("settings.db")
}

/// The one piece of parsed slot id: `g903hero_g4_m1_shifted`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotRef {
    pub device: String,
    pub gkey: usize,
    pub bank: usize,
    pub shifted: bool,
}

impl SlotRef {
    /// The local slot id: keyboards get `gkey:G{n}:M{bank}`, mice `ghub-button:G{n}`.
    ///
    /// Mouse buttons keep G HUB's own numbering rather than being renamed, because G HUB numbers
    /// them in an order of its own and silently mapping G6 onto some local button would move a
    /// binding to the wrong physical button. The item carries a warning saying to check them.
    pub fn local_slot(&self) -> String {
        let mut slot = if self.is_keyboard() {
            format!("gkey:G{}:M{}", self.gkey, self.bank)
        } else {
            format!("ghub-button:G{}", self.gkey)
        };
        if self.shifted {
            slot.push_str(":shift");
        }
        slot
    }

    pub fn is_keyboard(&self) -> bool {
        KNOWN_KEYBOARDS.contains(&self.device.as_str())
    }
}

/// Parses `{device}_g{n}_m{bank}[_shifted]`.
///
/// Hand-written rather than a regex, which keeps the binary free of a regex engine for one
/// pattern. The shape is pinned by tests, including the ids that must *not* parse.
pub fn parse_slot(slot: &str) -> Option<SlotRef> {
    let (rest, shifted) = match slot.strip_suffix("_shifted") {
        Some(rest) => (rest, true),
        None => (slot, false),
    };
    let (rest, bank) = rest.rsplit_once("_m")?;
    let (device, gkey) = rest.rsplit_once("_g")?;
    if device.is_empty() || !device.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) {
        return None;
    }
    Some(SlotRef {
        device: device.to_owned(),
        gkey: gkey.parse().ok()?,
        bank: bank.parse().ok()?,
        shifted,
    })
}

/// What a built-in G HUB card id decodes to.
#[derive(Debug, Clone, PartialEq)]
pub enum BuiltInCard {
    /// A command with a local equivalent.
    Action(ActionDefinition),
    /// Decoded, and means "nothing is bound here".
    Unassigned,
    /// A type whose meaning is not known. The string says so; it is never guessed at.
    Undecoded(String),
}

/// `logi.protocol.macros.Macro.Mouse.Action`, read out of the serialized protobuf descriptor in
/// G HUB's `lghub_agent.exe` (2026.6). The gaps (7, 13) are G HUB's, not omissions.
const MOUSE_ACTIONS: &[(u8, &str)] = &[
    (1, "DPI_UP"),
    (2, "DPI_DOWN"),
    (3, "DPI_SHIFT"),
    (4, "DPI_CYCLE"),
    (5, "DPI_DEFAULT"),
    (8, "BUTTON_DOUBLE_CLICK"),
    (9, "WHEEL_UP"),
    (10, "WHEEL_DOWN"),
    (11, "WHEEL_LEFT"),
    (12, "WHEEL_RIGHT"),
    (14, "DPI_GO_TO_SHIFT"),
    (15, "WHEEL_MODE_TOGGLE"),
    (16, "RATCHET_FORCE_CYCLE"),
];

/// `logi.protocol.macros.Macro.Device.Action`, from the same descriptor.
const DEVICE_ACTIONS: &[(u8, &str)] = &[
    (1, "PROFILE_CYCLE"),
    (2, "PROFILE_NEXT"),
    (3, "PROFILE_PREV"),
    (4, "PROFILE_ACTIVATE"),
    (5, "G_SHIFT"),
    (6, "BATTERY_LIFE"),
    (7, "G_SHIFT_DEFAULT"),
    (8, "NATIVE_ACTION"),
    (9, "REPORT_RATE_CYCLE"),
];

/// Decodes `0f82f693-5b78-4cf5-867e-TTVV00000000`, G HUB's built-in command cards.
///
/// These are not stored in `settings.db`, so their id is the only evidence of what they do.
///
/// * `TT=01`: keyboard HID usage `VV`. G HUB's own UI bundle ties ENTER to `0128`, ESC to `0129`.
/// * `TT=02`: mouse button `VV`; the G903 depot's defaults bind input *n* to `02nn`.
/// * `TT=04`: `Macro.Mouse.Action` value `VV` ([`MOUSE_ACTIONS`]). The G903's defaults put
///   `0401`/`0402` on the two DPI buttons and `040B`/`040C` on the wheel tilts.
/// * `TT=09`: `Macro.Device.Action` value `VV` ([`DEVICE_ACTIONS`]). G HUB's UI names `0907` its
///   `G_SHIFT_DEFAULT_CARD` and binds `0909` to the report-rate-cycle key, matching values 7 and 9.
///
/// A decoded command with no local equivalent is reported by name. A type or value outside
/// those tables is reported as undecoded rather than guessed: a wrong guess silently rebinds a key.
///
/// `None` means the id is not a built-in card id at all.
pub fn decode_built_in_card(card_id: &str) -> Option<BuiltInCard> {
    let lower = card_id.to_ascii_lowercase();
    if !lower.starts_with(BUILT_IN_PREFIX) || lower.len() != 36 || !lower.ends_with("00000000") {
        return None;
    }
    let kind = u8::from_str_radix(&lower[24..26], 16).ok()?;
    let value = u8::from_str_radix(&lower[26..28], 16).ok()?;
    Some(match (kind, value) {
        (0x01, usage) => match virtual_key_from_hid_usage(usage) {
            Some(vk) => BuiltInCard::Action(ActionDefinition::Key(KeyStroke::new(vk))),
            None => BuiltInCard::Undecoded(format!(
                "Built-in G HUB keyboard command uses HID usage 0x{usage:02X}, which has no \
                 virtual-key equivalent; not imported"
            )),
        },
        (0x02, button @ 1..=5) => BuiltInCard::Action(ActionDefinition::MouseButton {
            button: match button {
                1 => MouseButtonKind::Left,
                2 => MouseButtonKind::Right,
                3 => MouseButtonKind::Middle,
                4 => MouseButtonKind::XButton1,
                _ => MouseButtonKind::XButton2,
            },
        }),
        (0x09, 0x07) => BuiltInCard::Unassigned,
        (0x04, value) if lookup(MOUSE_ACTIONS, value).is_some() => {
            let name = lookup(MOUSE_ACTIONS, value).expect("checked by the guard");
            named_command("MOUSE", "mouse", name)
        }
        (0x09, value) if lookup(DEVICE_ACTIONS, value).is_some() => {
            let name = lookup(DEVICE_ACTIONS, value).expect("checked by the guard");
            named_command("DEVICE", "device", name)
        }
        (kind, value) => BuiltInCard::Undecoded(format!(
            "Built-in G HUB command type 0x{kind:02X} value 0x{value:02X} is not decoded (no \
             evidence of its meaning); not imported"
        )),
    })
}

fn lookup(table: &[(u8, &'static str)], value: u8) -> Option<&'static str> {
    table.iter().find(|(v, _)| *v == value).map(|(_, name)| *name)
}

/// A decoded built-in command, mapped through the same table G HUB's preset cards use, so a
/// built-in `DPI_UP` and a preset `DPI_UP` can never import differently.
fn named_command(kind: &str, field: &str, action: &str) -> BuiltInCard {
    let definition = serde_json::json!({ "type": kind, field: { "action": action } });
    match crate::presets::card_action(action, &definition) {
        Ok(action) => BuiltInCard::Action(action),
        Err(reason) => BuiltInCard::Undecoded(format!(
            "Built-in G HUB command {action}: {reason}; not imported"
        )),
    }
}

/// Reads the newest settings JSON out of a G HUB database, through a temporary copy.
pub fn load_settings_json(settings_db: &std::path::Path) -> Result<String, String> {
    let temp = temp_directory()?;
    let result = (|| {
        let copy = temp.join("settings.db");
        copy_shared(settings_db, &copy)?;
        // The `-wal` is not optional: SQLite in WAL mode keeps the newest rows there, so a copy
        // of the database alone can be an older document than the one G HUB is showing.
        for suffix in ["-wal", "-shm"] {
            let side = with_suffix(settings_db, suffix);
            if side.exists() {
                copy_shared(&side, &with_suffix(&copy, suffix))?;
            }
        }
        read_latest_blob(&copy)
    })();
    // Best effort: a copy left behind in the temp directory is untidy, not a failure, and
    // reporting it instead of the read's own result would bury the real outcome.
    let _ = std::fs::remove_dir_all(&temp);
    result
}

/// A private directory for the copy.
///
/// `create_dir`, not `create_dir_all`: a name that already exists is an error here rather than a
/// directory to share, because this is where someone else's G HUB configuration is about to be
/// written.
fn temp_directory() -> Result<std::path::PathBuf, String> {
    let unique = format!(
        "ghub-rs-import-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    );
    let path = std::env::temp_dir().join(unique);
    std::fs::create_dir(&path)
        .map(|()| path)
        .map_err(|e| format!("could not create a temporary directory for the copy: {e}"))
}

fn with_suffix(path: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    std::path::PathBuf::from(name)
}

/// Copies a file G HUB may have open.
///
/// `std::fs::File::open` asks for read access and grants read, write and delete sharing, which is
/// the same sharing the C# spelled out as `FileShare.ReadWrite | FileShare.Delete`. A plain
/// `fs::copy` is not equivalent: it can fail on a database SQLite holds open.
fn copy_shared(source: &std::path::Path, destination: &std::path::Path) -> Result<(), String> {
    let mut input = std::fs::File::open(source)
        .map_err(|e| format!("{} could not be read: {e}", source.display()))?;
    let mut output = std::fs::File::create_new(destination)
        .map_err(|e| format!("the temporary copy could not be created: {e}"))?;
    std::io::copy(&mut input, &mut output)
        .map(|_| ())
        .map_err(|e| format!("{} could not be copied: {e}", source.display()))
}

/// The newest `data.file` blob from a copy of a G HUB database, opened read-only.
pub fn read_latest_blob(database_copy: &std::path::Path) -> Result<String, String> {
    use rusqlite::OpenFlags;

    // Read-only and without a shared cache, so this cannot create, migrate or journal anything
    // even if the copy turns out to be something other than a G HUB database.
    let connection = rusqlite::Connection::open_with_flags(
        database_copy,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("the G HUB settings database could not be opened: {e}"))?;

    let mut statement = connection
        .prepare("SELECT file FROM data ORDER BY _id DESC LIMIT 1")
        .map_err(|e| format!("the G HUB settings database has no 'data' table: {e}"))?;
    let mut rows = statement
        .query([])
        .map_err(|e| format!("the G HUB settings database could not be read: {e}"))?;
    let Some(row) = rows
        .next()
        .map_err(|e| format!("the G HUB settings database could not be read: {e}"))?
    else {
        return Err("the G HUB settings database has no data row".to_owned());
    };

    // The column is a blob in every observed G HUB build, but SQLite does not enforce a column
    // type, so a text value is accepted rather than refused.
    match row.get_ref(0) {
        Ok(rusqlite::types::ValueRef::Blob(bytes)) => Ok(String::from_utf8_lossy(bytes).into_owned()),
        Ok(rusqlite::types::ValueRef::Text(bytes)) => Ok(String::from_utf8_lossy(bytes).into_owned()),
        _ => Err("the G HUB settings row does not hold a document".to_owned()),
    }
}

/// Reads a G HUB document and lists what could be brought over. Touches no local state.
pub fn preview(
    json: &str,
    source_path: &str,
    resolve_device_key: &DeviceKeyResolver,
) -> Result<ImportPreview, StoreError> {
    // The G HUB document is several times the size of a profile document holding the same
    // settings, so the import limit is scaled rather than reused directly - the C# used the same
    // factor.
    if json.len() > MAX_IMPORT_BYTES.saturating_mul(4) {
        return Err(StoreError::Validation(vec![
            "G HUB settings document too large".to_owned(),
        ]));
    }
    // `serde_json`'s own recursion limit is 128, which is the `MaxDepth` the C# asked for.
    let root: Value = serde_json::from_str(json)
        .map_err(|e| StoreError::Validation(vec![format!("G HUB settings is not valid JSON: {e}")]))?;

    let mut preview = ImportPreview {
        source_path: source_path.to_owned(),
        ghub_version: string_at(&root, "last_run_version"),
        ..Default::default()
    };

    let cards = index_by(&root, "cards", "cards", "id");
    let (app_names, app_paths) = applications(&root);

    let Some(profiles) = root
        .get("profiles")
        .and_then(|p| p.get("profiles"))
        .and_then(Value::as_array)
    else {
        preview.warnings.push("No profiles found in G HUB settings".to_owned());
        return Ok(preview);
    };

    // One macro per card, not per binding: two G-keys pointing at the same G HUB macro card must
    // end up pointing at the same local macro, or editing it later would only fix one of them.
    let mut macros_by_card: HashMap<String, Id> = HashMap::new();

    for (order, gp) in profiles.iter().enumerate() {
        let mut name = string_at(gp, "name").unwrap_or_else(|| "G HUB profile".to_owned());
        if name == "PROFILE_NAME_DEFAULT" {
            name = "Default (G HUB)".to_owned();
        }
        let app_id = string_at(gp, "applicationId");
        let mut app_name = app_id
            .as_ref()
            .and_then(|id| app_names.get(id).cloned())
            .unwrap_or_else(|| "Desktop".to_owned());
        if app_name == "APPLICATION_NAME_DESKTOP" {
            app_name = "Desktop".to_owned();
        }

        let mut profile = PeripheralProfile::new(format!("{name} \u{2014} {app_name}"));
        // Ordered after everything the user already has, so an import never reshuffles their own
        // list. `apply` renumbers densely from the end of the real list.
        profile.order = 1000 + order as i32;
        let profile_name = profile.name.clone();
        preview.profiles.push(profile);
        preview.items.push(ImportItem::new(
            ImportItemKind::Profile,
            format!("profile {}", string_at(gp, "id").unwrap_or_default()),
            format!("Profile '{profile_name}'"),
            true,
            None,
            Applier::None,
        ));

        match (&app_id, app_id.as_ref().and_then(|id| app_paths.get(id))) {
            (Some(_), Some(exe)) if !exe.trim().is_empty() => {
                preview.items.push(ImportItem::new(
                    ImportItemKind::AppRule,
                    format!("application {}", app_id.clone().unwrap_or_default()),
                    format!("Auto-switch for {exe}"),
                    true,
                    None,
                    Applier::AppRule(Box::new(AppRule {
                        id: Id::new(),
                        kind: AppMatchKind::ExactPath,
                        executable_path: Some(exe.clone()),
                        executable_name: None,
                        title_pattern: None,
                        enabled: true,
                        priority: 0,
                    })),
                ));
            }
            (Some(id), _) if app_name != "Desktop" => {
                preview.items.push(ImportItem::new(
                    ImportItemKind::AppRule,
                    format!("application {id}"),
                    format!("Application '{app_name}'"),
                    false,
                    Some(
                        "G HUB stores no executable path for this application; add a rule manually"
                            .to_owned(),
                    ),
                    Applier::None,
                ));
            }
            _ => {}
        }

        if let Some(assignments) = gp.get("assignments").and_then(Value::as_array) {
            for assignment in assignments {
                let (Some(slot), Some(card_id)) =
                    (string_at(assignment, "slotId"), string_at(assignment, "cardId"))
                else {
                    continue;
                };
                map_assignment(
                    &mut preview,
                    &slot,
                    &card_id,
                    &cards,
                    &mut macros_by_card,
                    resolve_device_key,
                );
            }
        }

        // The profile's software (LIGHTSYNC) lighting. Listed after the assignments so that, when
        // both are ticked, the per-key colours are what the device ends up showing - which is
        // what G HUB shows while it runs; the firmware card is only its onboard fallback.
        if let Some(card) = string_at(gp, "lightingCard").and_then(|id| cards.get(&id)) {
            map_software_lighting(&mut preview, &root, card, resolve_device_key);
        }
    }

    Ok(preview)
}

/// G HUB firmware effect name to 0x8071 effect id. The ids are the ones
/// `ghub_hidpp::features::lighting::effect_id` uses, which come from the HID++ 0x8071 spec.
fn firmware_effect_id(name: &str) -> Option<u16> {
    Some(match name {
        "OFF" => 0x0000,
        "FIXED" => 0x0001,
        "CYCLE" => 0x0003,
        "COLORWAVE" => 0x0004,
        "BREATHING" => 0x000A,
        "RIPPLE" => 0x000B,
        _ => return None,
    })
}

fn effect_name(effect_id: u16) -> &'static str {
    match effect_id {
        0x0000 => "off",
        0x0001 => "fixed",
        0x0003 => "cycle",
        0x0004 => "colour wave",
        0x000A => "breathing",
        0x000B => "ripple",
        _ => "effect",
    }
}

/// G HUB zone type to 0x8071 cluster location.
fn zone_location(zone: &str) -> Option<u16> {
    match zone {
        "ZONE_PRIMARY" => Some(0x0001),
        "ZONE_BRANDING" => Some(0x0002),
        _ => None,
    }
}

fn zone_name(location: u16) -> &'static str {
    match location {
        0x0001 => "primary",
        0x0002 => "logo",
        _ => "zone",
    }
}

/// `firmwareLightingSettings.effects` as zone effects, plus what could not be converted.
///
/// G HUB keeps a positional list with `{}` placeholders for zones the device lacks; those are
/// skipped silently because they carry nothing.
fn read_firmware_effects(effects: &[Value]) -> (HashMap<u16, ZoneEffect>, Vec<String>) {
    let mut zones = HashMap::new();
    let mut skipped = Vec::new();
    for effect in effects {
        let Some(name) = string_at(effect, "id") else { continue };
        let zone = string_at(effect, "zoneType").unwrap_or_default();
        let (Some(effect_id), Some(location)) = (firmware_effect_id(&name), zone_location(&zone))
        else {
            skipped.push(format!("{name} on {zone}"));
            continue;
        };
        // Each effect keeps its parameters under `{lowercase id}Params`.
        let params = effect.get(format!("{}Params", name.to_ascii_lowercase()));
        let period_ms = params
            .and_then(|p| p.get("periodInMs"))
            .and_then(Value::as_u64)
            .map_or(5000, |ms| ms.clamp(1, u64::from(u16::MAX)) as u16);
        let color = params
            .and_then(|p| p.get("color"))
            .and_then(|c| string_at(c, "hex"))
            .and_then(|hex| normalise_hex(&hex));
        zones.insert(location, ZoneEffect { effect_id, period_ms, color });
    }
    (zones, skipped)
}

/// `#rrggbb` to `#RRGGBB`; anything else is not a colour.
fn normalise_hex(hex: &str) -> Option<String> {
    let digits = hex.strip_prefix('#')?;
    (digits.len() == 6 && digits.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| format!("#{}", digits.to_ascii_uppercase()))
}

/// G HUB per-key slot and code to a layout key id.
///
/// `PERKEY_KEYBOARD` codes are HID keyboard usages, `PERKEY_CONSUMER` HID consumer usages, and
/// the G-key, branding and indicator codes are 1-based indexes - G HUB's own `g815_us` geometry
/// has five G-keys, one logo and one indicator (the brightness key).
fn per_key_id(slot: &str, code: u32) -> Option<&'static str> {
    let id = match (slot, code) {
        ("PERKEY_KEYBOARD", usage) => {
            return u8::try_from(usage).ok().and_then(crate::layout::by_usage).map(|k| k.id);
        }
        ("PERKEY_GKEY", 1) => "G1",
        ("PERKEY_GKEY", 2) => "G2",
        ("PERKEY_GKEY", 3) => "G3",
        ("PERKEY_GKEY", 4) => "G4",
        ("PERKEY_GKEY", 5) => "G5",
        ("PERKEY_BRANDING", 1) => "Logo",
        ("PERKEY_INDICATOR", 1) => "Brightness",
        ("PERKEY_CONSUMER", 0xB5) => "MediaNext",
        ("PERKEY_CONSUMER", 0xB6) => "MediaPrevious",
        ("PERKEY_CONSUMER", 0xCD) => "MediaPlayPause",
        ("PERKEY_CONSUMER", 0xE2) => "MediaMute",
        _ => return None,
    };
    crate::layout::by_id(id).map(|k| k.id)
}

/// A profile's `SOFTWARE_LIGHTING_SETTINGS` card: each device's effect, converted when it is a
/// fixed (freestyle) per-key effect and reported by kind when it is not.
fn map_software_lighting(
    preview: &mut ImportPreview,
    root: &Value,
    card: &Value,
    resolve_device_key: &DeviceKeyResolver,
) {
    let Some(settings) = card.get("softwareLightingSettings") else { return };
    let Some(devices) = settings.get("devices").and_then(Value::as_object) else { return };
    let effects = settings.get("effects").and_then(Value::as_array).map(Vec::as_slice);

    for (prefix, support) in devices {
        let support = support.as_str().unwrap_or_default();
        let source = format!("{prefix}_software_lighting");
        // The card's effect for this device's lighting type.
        let effect_id = effects.unwrap_or_default().iter().find_map(|effect| {
            let supported = effect
                .get("metadata")
                .and_then(|m| m.get("deviceSupport"))
                .and_then(Value::as_array)
                .is_none_or(|list| list.iter().any(|s| s.as_str() == Some(support)));
            supported.then(|| string_at(effect, "id")).flatten()
        });
        let Some(effect_id) = effect_id else { continue };
        let name = prefab_name(root, &effect_id).unwrap_or_else(|| effect_id.clone());

        let Some(effect) = root.get(format!("lighting_effects/{effect_id}")) else {
            preview.items.push(ImportItem::unsupported(
                source,
                format!("{prefix}: lighting effect '{name}'"),
                "G HUB built-in lighting preset (not stored in settings); pick the matching \
                 animation in the lighting editor",
            ));
            continue;
        };
        let Some(slots) = effect
            .get("fixedInfo")
            .and_then(|f| f.get("lightingSlots"))
            .and_then(|s| s.get("infoMap"))
            .and_then(Value::as_object)
        else {
            let kind = effect
                .as_object()
                .and_then(|o| o.keys().find(|k| k.ends_with("Info")).cloned())
                .unwrap_or_else(|| "unknown".to_owned());
            preview.items.push(ImportItem::unsupported(
                source,
                format!("{prefix}: lighting effect '{name}'"),
                format!("G HUB {kind} effects are animated per-frame by G HUB; not convertible"),
            ));
            continue;
        };

        let mut colors = HashMap::new();
        let mut unknown = 0usize;
        for (slot, info) in slots {
            let Some(map) = info
                .get("perKeyMap")
                .and_then(|m| m.get("colorCodeMap"))
                .and_then(Value::as_object)
            else {
                continue;
            };
            for (code, color) in map {
                let id = code.parse::<u32>().ok().and_then(|c| per_key_id(slot, c));
                let hex = string_at(color, "hex").and_then(|h| normalise_hex(&h));
                match (id, hex) {
                    (Some(id), Some(hex)) => {
                        colors.insert(format!("key:{id}"), hex);
                    }
                    _ => unknown += 1,
                }
            }
        }
        if colors.is_empty() {
            preview.items.push(ImportItem::unsupported(
                source,
                format!("{prefix}: lighting effect '{name}'"),
                "G HUB effect has no key this layout knows",
            ));
            continue;
        }
        let device_key = resolve_device_key(prefix).unwrap_or_else(|| format!("ghub:{prefix}"));
        let count = colors.len();
        preview.items.push(ImportItem::new(
            ImportItemKind::Lighting,
            source,
            format!("{prefix}: per-key lighting '{name}' ({count} keys)"),
            true,
            (unknown > 0).then(|| format!("{unknown} key colour(s) for keys not in this layout")),
            Applier::PerKeyLighting { device_key, colors },
        ));
    }
}

/// The name G HUB shows for a saved effect, from its `lighting_prefabs` list.
fn prefab_name(root: &Value, effect_id: &str) -> Option<String> {
    root.get("lighting_prefabs")?
        .get("list")?
        .as_array()?
        .iter()
        .find(|p| string_at(p, "id").as_deref() == Some(effect_id))
        .and_then(|p| string_at(p, "name"))
}

fn map_assignment(
    preview: &mut ImportPreview,
    slot: &str,
    card_id: &str,
    cards: &HashMap<String, Value>,
    macros_by_card: &mut HashMap<String, Id>,
    resolve_device_key: &DeviceKeyResolver,
) {
    if card_id == UNASSIGNED_CARD {
        add_binding(
            preview,
            slot,
            ActionDefinition::Disabled,
            "Disabled",
            None,
            resolve_device_key,
        );
        return;
    }

    let card = cards.get(card_id);
    if card.is_none() {
        match decode_built_in_card(card_id) {
            Some(BuiltInCard::Action(action)) => {
                let described = action.describe();
                add_binding(
                    preview,
                    slot,
                    action,
                    &format!("G HUB default: {described}"),
                    None,
                    resolve_device_key,
                );
            }
            // Nothing to import and nothing to report: an empty G-Shift layer is the normal state
            // of almost every slot, and an item per empty slot would bury the real ones.
            Some(BuiltInCard::Unassigned) => {}
            Some(BuiltInCard::Undecoded(reason)) => {
                preview.items.push(ImportItem::unsupported(
                    slot,
                    format!("{slot}: built-in card {}", &card_id[24..]),
                    reason,
                ));
            }
            None => {
                let head = &card_id[..card_id.len().min(8)];
                preview.items.push(ImportItem::unsupported(
                    slot,
                    format!("{slot}: built-in G HUB card {head}\u{2026}"),
                    "Built-in G HUB default command (card not stored locally); not imported",
                ));
            }
        }
        return;
    }

    let card = card.expect("checked above");
    let attribute = string_at(card, "attribute").unwrap_or_else(|| "?".to_owned());
    match attribute.as_str() {
        "MOUSE_SETTINGS" if card.get("mouseSettings").is_some() => {
            let settings = card.get("mouseSettings").expect("checked above");
            let prefix = slot.split('_').next().unwrap_or(slot);
            let device_key = resolve_device_key(prefix).unwrap_or_else(|| format!("ghub:{prefix}"));
            let mouse = read_mouse_settings(settings);
            let stages = mouse
                .dpi_stages
                .iter()
                .map(|stage| stage.dpi.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let shift = mouse.shift_dpi.map_or_else(|| "-".to_owned(), |dpi| dpi.to_string());
            let rate = mouse
                .report_rate_hz
                .map_or_else(|| "unchanged".to_owned(), |hz| format!("{hz} Hz"));
            preview.items.push(ImportItem::new(
                ImportItemKind::MouseSettings,
                slot,
                format!("{prefix}: DPI [{stages}], shift {shift}, {rate}"),
                true,
                None,
                Applier::Mouse { device_key, settings: Box::new(mouse) },
            ));
        }
        "MACRO_PLAYBACK" if card.get("macro").is_some() => {
            let name = string_at(card, "name").unwrap_or_else(|| "G HUB macro".to_owned());
            match map_macro(
                card.get("macro").expect("checked above"),
                &name,
                card_id,
                preview,
                macros_by_card,
            ) {
                Ok(action) => {
                    let described = action.describe();
                    let label = if name.is_empty() { described } else { name };
                    add_binding(preview, slot, action, &label, None, resolve_device_key);
                }
                Err(reason) => {
                    preview.items.push(ImportItem::unsupported(
                        slot,
                        format!("{slot}: {name}"),
                        reason,
                    ));
                }
            }
        }
        "DISABLE_KEY_SETTINGS" => {
            let prefix = slot.split('_').next().unwrap_or(slot);
            let device_key = resolve_device_key(prefix).unwrap_or_else(|| format!("ghub:{prefix}"));
            // G HUB's UI writes `disableKeysSettings.hidUsages` once the user edits the list. Its
            // read-only default card carries no list at all; the keyboard then uses its firmware
            // default, which is the Windows keys - the same as the local default.
            let stored = card
                .get("disableKeysSettings")
                .and_then(|s| s.get("hidUsages"))
                .and_then(Value::as_array);
            let (usages, origin) = match stored {
                Some(list) => (
                    list.iter()
                        .filter_map(Value::as_u64)
                        .filter(|u| (1..=255).contains(u))
                        .map(|u| u as u8)
                        .collect::<Vec<u8>>(),
                    "G HUB list",
                ),
                None => (
                    KeyboardSettings::default().game_mode_disabled_usages,
                    "G HUB default",
                ),
            };
            let names = usages
                .iter()
                .map(|u| {
                    crate::layout::by_usage(*u)
                        .map_or_else(|| format!("0x{u:02X}"), |k| k.id.to_owned())
                })
                .collect::<Vec<_>>()
                .join(", ");
            let shown = if names.is_empty() { "none".to_owned() } else { names };
            preview.items.push(ImportItem::new(
                ImportItemKind::Keyboard,
                slot,
                format!("{prefix}: Game Mode disabled keys ({origin}): {shown}"),
                true,
                None,
                Applier::GameModeKeys { device_key, usages },
            ));
        }
        "FIRMWARE_LIGHTING_SETTINGS" => {
            let prefix = slot.split('_').next().unwrap_or(slot);
            let device_key = resolve_device_key(prefix).unwrap_or_else(|| format!("ghub:{prefix}"));
            let effects = card
                .get("firmwareLightingSettings")
                .and_then(|s| s.get("effects"))
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let (zones, skipped) = read_firmware_effects(effects);
            if zones.is_empty() {
                preview.items.push(ImportItem::unsupported(
                    slot,
                    format!("{prefix}: firmware lighting"),
                    if skipped.is_empty() {
                        "G HUB firmware lighting card has no effects".to_owned()
                    } else {
                        format!("G HUB firmware effects not convertible: {}", skipped.join(", "))
                    },
                ));
                return;
            }
            let mut parts: Vec<(u16, String)> = zones
                .iter()
                .map(|(location, z)| {
                    (
                        *location,
                        format!(
                            "{} {}{}",
                            zone_name(*location),
                            effect_name(z.effect_id),
                            z.color.as_deref().map(|c| format!(" {c}")).unwrap_or_default()
                        ),
                    )
                })
                .collect();
            parts.sort_unstable_by_key(|(location, _)| *location);
            let warning = (!skipped.is_empty())
                .then(|| format!("Not converted: {}", skipped.join(", ")));
            preview.items.push(ImportItem::new(
                ImportItemKind::Lighting,
                slot,
                format!(
                    "{prefix}: firmware lighting ({})",
                    parts.into_iter().map(|(_, p)| p).collect::<Vec<_>>().join("; ")
                ),
                true,
                warning,
                Applier::FirmwareLighting { device_key, zones },
            ));
        }
        other => {
            preview.items.push(ImportItem::unsupported(
                slot,
                format!("{slot}: {other}"),
                format!("G HUB card type {other} has no equivalent importer"),
            ));
        }
    }
}

fn read_mouse_settings(settings: &Value) -> MouseSettings {
    let mut mouse = MouseSettings::default();
    if let Some(table) = settings.get("dpiTable") {
        let levels: Vec<u32> = table
            .get("levels")
            .and_then(Value::as_array)
            .map(|levels| {
                levels
                    .iter()
                    .filter_map(Value::as_i64)
                    // The bounds are the ones the C# applied: a level outside them is not a DPI
                    // this hardware can be set to, so it is dropped rather than clamped into
                    // something the user never chose.
                    .filter(|dpi| (50..=50_000).contains(dpi))
                    .map(|dpi| dpi as u32)
                    .take(MAX_DPI_STAGES)
                    .collect()
            })
            .unwrap_or_default();
        if !levels.is_empty() {
            mouse.dpi_stages = levels.into_iter().map(DpiStage::new).collect();
        }
        if let Some(shift) = table.get("shiftDpi").and_then(Value::as_i64) {
            mouse.shift_dpi = Some(shift as u32);
        }
        if let Some(default) = table.get("defaultDpi").and_then(Value::as_i64) {
            // A default DPI that is not one of the levels falls back to the first stage, which is
            // what `FindIndex` returning -1 and `Math.Max(0, ..)` did.
            let index = mouse
                .dpi_stages
                .iter()
                .position(|stage| stage.dpi as i64 == default)
                .unwrap_or(0);
            mouse.default_stage = index;
            mouse.active_stage = index;
        }
    }
    if let Some(rate) = settings
        .get("reportRate")
        .and_then(|rate| rate.get("value"))
        .and_then(Value::as_i64)
    {
        mouse.report_rate_hz = Some(rate as u32);
    }
    mouse
}

/// Maps one G HUB macro card onto an action. `Err` carries the reason it could not be.
fn map_macro(
    macro_value: &Value,
    name: &str,
    card_id: &str,
    preview: &mut ImportPreview,
    macros_by_card: &mut HashMap<String, Id>,
) -> Result<ActionDefinition, String> {
    match string_at(macro_value, "type").as_deref() {
        Some("KEYSTROKE") => {
            let keystroke = macro_value
                .get("keystroke")
                .ok_or_else(|| "Keystroke macro without a keystroke".to_owned())?;
            let code = keystroke.get("code").and_then(Value::as_i64).unwrap_or(-1);
            let usages: Vec<u8> = keystroke
                .get("modifiers")
                .and_then(Value::as_array)
                .map(|mods| {
                    mods.iter()
                        .filter_map(Value::as_i64)
                        .filter(|usage| (0..=255).contains(usage))
                        .map(|usage| usage as u8)
                        .collect()
                })
                .unwrap_or_default();
            let vk = (0..=255)
                .contains(&code)
                .then(|| virtual_key_from_hid_usage(code as u8))
                .flatten()
                .ok_or_else(|| format!("Unknown key usage {code}"))?;
            Ok(ActionDefinition::Key(KeyStroke::with(vk, modifiers_from_hid_usages(&usages))))
        }
        Some("TEXT_BLOCK") => {
            let text = string_at(macro_value, "textBlock")
                .or_else(|| macro_value.get("textBlock").and_then(|tb| string_at(tb, "text")))
                .ok_or_else(|| "Text macro without text".to_owned())?;
            Ok(ActionDefinition::Text {
                text: text.chars().take(MAX_TEXT_LENGTH).collect(),
            })
        }
        Some("SEQUENCE") => {
            if let Some(existing) = macros_by_card.get(card_id) {
                return Ok(ActionDefinition::MacroRef { macro_id: *existing });
            }
            let sequence = macro_value
                .get("sequence")
                .ok_or_else(|| "Sequence macro without a sequence".to_owned())?;
            let mut definition = MacroDefinition::new(name);
            definition.tags = vec!["imported".to_owned(), "ghub".to_owned()];
            if let Some(components) = sequence.get("componentsList").and_then(Value::as_array) {
                for component in components.iter().take(MacroDefinition::MAX_STEPS) {
                    if let Some(keyboard) = component.get("keyboard") {
                        let code = keyboard.get("key").and_then(Value::as_i64).unwrap_or(-1);
                        // Absent `isDown` means a press: G HUB omits the field for the common
                        // case, so defaulting it to "up" would invert every recorded macro.
                        let down = keyboard.get("isDown").and_then(Value::as_bool).unwrap_or(true);
                        if let Some(vk) = (0..=255)
                            .contains(&code)
                            .then(|| virtual_key_from_hid_usage(code as u8))
                            .flatten()
                        {
                            definition.steps.push(if down {
                                MacroStep::KeyDown { virtual_key: vk }
                            } else {
                                MacroStep::KeyUp { virtual_key: vk }
                            });
                        }
                    } else if let Some(milliseconds) = component
                        .get("delay")
                        .and_then(|delay| delay.get("durationMs"))
                        .and_then(Value::as_i64)
                    {
                        definition.steps.push(MacroStep::Delay {
                            milliseconds: milliseconds
                                .clamp(0, MacroDefinition::MAX_DELAY_MILLISECONDS as i64)
                                as u32,
                        });
                    }
                }
            }
            if definition.steps.is_empty() {
                return Err("Sequence macro has no convertible steps".to_owned());
            }

            let id = definition.id;
            let steps = definition.steps.len();
            macros_by_card.insert(card_id.to_owned(), id);
            preview.items.push(ImportItem::new(
                ImportItemKind::Macro,
                card_id,
                format!("Macro '{name}' ({steps} steps)"),
                true,
                None,
                Applier::Macro(Box::new(definition)),
            ));
            Ok(ActionDefinition::MacroRef { macro_id: id })
        }
        Some("ACTION") => {
            Err("G HUB integration actions (e.g. OBS/Overwolf) are not portable".to_owned())
        }
        Some(other) => Err(format!("G HUB macro type {other} is not supported")),
        None => Err("G HUB macro has no type".to_owned()),
    }
}

fn add_binding(
    preview: &mut ImportPreview,
    slot: &str,
    action: ActionDefinition,
    description: &str,
    warning: Option<String>,
    resolve_device_key: &DeviceKeyResolver,
) {
    let Some(parsed) = parse_slot(slot) else {
        preview.items.push(ImportItem::unsupported(
            slot,
            format!("{slot}: {description}"),
            "Unrecognised G HUB slot id",
        ));
        return;
    };

    let device_key =
        resolve_device_key(&parsed.device).unwrap_or_else(|| format!("ghub:{}", parsed.device));
    let local_slot = parsed.local_slot();
    let warning = warning.or_else(|| {
        (!parsed.is_keyboard()).then(|| {
            "G HUB mouse button numbers are mapped by number; verify them in the mouse editor"
                .to_owned()
        })
    });
    preview.items.push(ImportItem::new(
        ImportItemKind::Binding,
        slot,
        format!("{} {local_slot} \u{2192} {description}", parsed.device),
        true,
        warning,
        Applier::Binding {
            device_key,
            slot: local_slot,
            binding: Box::new(ActionBinding { action, site: ExecutionSite::Software }),
        },
    ));
}

/// Applies the ticked items into a copy of `current`.
///
/// The caller saves the result; the store keeps the previous document as a backup, and that backup
/// is the rollback point. Nothing here writes anything.
pub fn apply(
    preview: &ImportPreview,
    current: &ProfileDocument,
) -> Result<ProfileDocument, StoreError> {
    let mut result = current.clone();
    // Which preview profile an item belongs to: items follow their profile item in document
    // order, which is how the C# tracked it too.
    let mut profiles_seen = 0usize;
    let mut active: Option<usize> = None;
    // Devices whose lighting a per-key item has set, per result profile: a firmware card is
    // G HUB's onboard fallback and must not override the software lighting it shows while running.
    let mut per_key_claimed: HashSet<(usize, String)> = HashSet::new();

    for item in &preview.items {
        if item.kind == ImportItemKind::Profile {
            let source = preview.profiles.get(profiles_seen);
            profiles_seen += 1;
            active = None;
            if !item.selected {
                // An unticked profile takes its own items with it: a binding with no profile to
                // live in has nowhere to go.
                continue;
            }
            if let Some(source) = source {
                let mut copy = PeripheralProfile::new(unique_name(&result, &source.name));
                copy.order = result.profiles.len() as i32;
                result.profiles.push(copy);
                active = Some(result.profiles.len() - 1);
            }
            continue;
        }

        if !item.selected {
            continue;
        }
        let Some(index) = active else { continue };

        match &item.applier {
            Applier::None => {}
            Applier::AppRule(rule) => result.profiles[index].rules.push((**rule).clone()),
            Applier::Mouse { device_key, settings } => {
                result.profiles[index].for_device(device_key).mouse = Some((**settings).clone());
            }
            Applier::Binding { device_key, slot, binding } => {
                result.profiles[index]
                    .for_device(device_key)
                    .bindings
                    .insert(slot.clone(), (**binding).clone());
            }
            Applier::Macro(definition) => {
                if !result.macros.iter().any(|existing| existing.id == definition.id) {
                    result.macros.push((**definition).clone());
                }
            }
            Applier::FirmwareLighting { device_key, zones } => {
                let claimed = per_key_claimed.contains(&(index, device_key.clone()));
                let lighting = &mut result.profiles[index].for_device(device_key).lighting;
                lighting.zone_effects = zones.clone();
                // The primary zone doubles as the profile-wide effect, so the lighting page has
                // one to show and a zone the card did not mention still gets something sensible.
                let main = zones.get(&0x0001).or_else(|| zones.values().next());
                if let Some(main) = main {
                    lighting.device_effect_id = Some(main.effect_id);
                    lighting.device_effect_period_ms = main.period_ms;
                }
                if !claimed {
                    lighting.mode = LightingMode::DeviceEffect;
                }
            }
            Applier::PerKeyLighting { device_key, colors } => {
                per_key_claimed.insert((index, device_key.clone()));
                let lighting = &mut result.profiles[index].for_device(device_key).lighting;
                lighting.mode = LightingMode::Static;
                lighting.static_colors = colors.clone();
            }
            Applier::GameModeKeys { device_key, usages } => {
                result.profiles[index]
                    .for_device(device_key)
                    .keyboard
                    .get_or_insert_with(KeyboardSettings::default)
                    .game_mode_disabled_usages = usages.clone();
            }
        }
    }

    // A binding whose macro item was unticked would silently do nothing, which is worse than not
    // being imported: the user would see a bound key that never fires.
    let macro_ids: HashSet<Id> = result.macros.iter().map(|m| m.id).collect();
    for profile in &mut result.profiles {
        for device in profile.devices.values_mut() {
            device.bindings.retain(|_, binding| match binding.action.macro_id() {
                Some(id) => macro_ids.contains(&id),
                None => true,
            });
        }
    }

    store::validate(&result)?;
    Ok(result)
}

fn unique_name(document: &ProfileDocument, name: &str) -> String {
    let mut candidate = name.to_owned();
    let mut suffix = 2;
    while document.profiles.iter().any(|profile| profile.name == candidate) {
        candidate = format!("{name} ({suffix})");
        suffix += 1;
    }
    candidate
}

// ------------------------------------------------------------------ JSON helpers

/// A string property, or `None` when it is absent or not a string. G HUB writes numbers where it
/// once wrote strings, so a type mismatch is an absent value rather than an error.
fn string_at(value: &Value, name: &str) -> Option<String> {
    value.get(name)?.as_str().map(str::to_owned)
}

/// `root[outer][inner]` as an array, indexed by each element's `key` property.
fn index_by(root: &Value, outer: &str, inner: &str, key: &str) -> HashMap<String, Value> {
    root.get(outer)
        .and_then(|value| value.get(inner))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| string_at(item, key).map(|id| (id, item.clone())))
                .collect()
        })
        .unwrap_or_default()
}

/// Application names and executable paths, keyed by G HUB's `applicationId`.
fn applications(root: &Value) -> (HashMap<String, String>, HashMap<String, String>) {
    let mut names = HashMap::new();
    let mut paths = HashMap::new();
    let Some(applications) = root
        .get("applications")
        .and_then(|value| value.get("applications"))
        .and_then(Value::as_array)
    else {
        return (names, paths);
    };
    for application in applications {
        let Some(id) = string_at(application, "applicationId") else { continue };
        names.insert(id.clone(), string_at(application, "name").unwrap_or_else(|| id.clone()));
        if let Some(path) = string_at(application, "applicationPath")
            .or_else(|| string_at(application, "processPath"))
        {
            paths.insert(id, path);
        }
    }
    (names, paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{DpiCommand, ProfileCommand};

    fn resolver() -> DeviceKeyResolver {
        Box::new(|prefix| Some(format!("ghub:{prefix}")))
    }

    /// A G HUB document with one profile, one application, a macro card, a mouse-settings card and
    /// a built-in card, shaped as the real file is.
    fn document() -> String {
        serde_json::json!({
            "last_run_version": "2026.6.1234",
            "applications": {"applications": [
                {"applicationId": "app-1", "name": "Doom", "applicationPath": "C:\\Games\\doom.exe"},
                {"applicationId": "app-2", "name": "No Path Here"}
            ]},
            "cards": {"cards": [
                {
                    "id": "card-macro",
                    "name": "Reload",
                    "attribute": "MACRO_PLAYBACK",
                    "macro": {"type": "SEQUENCE", "sequence": {"componentsList": [
                        {"keyboard": {"key": 4, "isDown": true}},
                        {"delay": {"durationMs": 40}},
                        {"keyboard": {"key": 4, "isDown": false}}
                    ]}}
                },
                {
                    "id": "card-keystroke",
                    "name": "Jump",
                    "attribute": "MACRO_PLAYBACK",
                    "macro": {"type": "KEYSTROKE", "keystroke": {"code": 44, "modifiers": [224]}}
                },
                {
                    "id": "card-mouse",
                    "attribute": "MOUSE_SETTINGS",
                    "mouseSettings": {
                        "dpiTable": {"levels": [400, 800, 1600, 3200], "shiftDpi": 400, "defaultDpi": 1600},
                        "reportRate": {"value": 1000}
                    }
                },
                {"id": "card-weird", "attribute": "SOMETHING_NEW"}
            ]},
            "profiles": {"profiles": [
                {
                    "id": "profile-1",
                    "name": "PROFILE_NAME_DEFAULT",
                    "applicationId": "app-1",
                    "assignments": [
                        {"slotId": "g815_g1_m1", "cardId": "card-macro"},
                        {"slotId": "g815_g2_m1", "cardId": "card-keystroke"},
                        {"slotId": "g815_g3_m1", "cardId": "card-macro"},
                        {"slotId": "g815_g4_m1", "cardId": "ffffffff-ffff-ffff-ffff-ffffffffffff"},
                        {"slotId": "g815_g5_m1", "cardId": "0f82f693-5b78-4cf5-867e-010400000000"},
                        {"slotId": "g815_g5_m1_shifted", "cardId": "0f82f693-5b78-4cf5-867e-090700000000"},
                        {"slotId": "g903hero_g4_m1", "cardId": "card-mouse"},
                        {"slotId": "g903hero_g5_m1", "cardId": "card-weird"}
                    ]
                },
                {"id": "profile-2", "name": "Second", "applicationId": "app-2"}
            ]},
            "lighting_effects": {"effects": []}
        })
        .to_string()
    }

    #[test]
    fn the_slot_pattern_reads_device_key_and_bank() {
        let parsed = parse_slot("g903hero_g4_m2").expect("a plain slot parses");
        assert_eq!(parsed.device, "g903hero");
        assert_eq!((parsed.gkey, parsed.bank, parsed.shifted), (4, 2, false));

        let shifted = parse_slot("g815_g12_m3_shifted").expect("a shifted slot parses");
        assert_eq!((shifted.gkey, shifted.bank, shifted.shifted), (12, 3, true));
    }

    /// An id that is nearly a slot id must not parse, because a wrong device prefix would send a
    /// binding to a device key that does not exist and the import would look like it worked.
    #[test]
    fn a_malformed_slot_is_refused() {
        for slot in ["", "g815", "g815_g1", "g815_m1", "_g1_m1", "g815_gx_m1", "g815_g1_mx"] {
            assert!(parse_slot(slot).is_none(), "{slot} should not parse");
        }
    }

    #[test]
    fn a_keyboard_slot_becomes_a_gkey_and_a_mouse_slot_keeps_its_number() {
        assert_eq!(parse_slot("g815_g3_m2").unwrap().local_slot(), "gkey:G3:M2");
        assert_eq!(parse_slot("g815_g3_m2_shifted").unwrap().local_slot(), "gkey:G3:M2:shift");
        assert_eq!(parse_slot("g903hero_g6_m1").unwrap().local_slot(), "ghub-button:G6");
    }

    #[test]
    fn a_built_in_keyboard_card_decodes_to_its_key() {
        // HID usage 0x04 is 'a'.
        let decoded = decode_built_in_card("0f82f693-5b78-4cf5-867e-010400000000");
        let Some(BuiltInCard::Action(ActionDefinition::Key(stroke))) = decoded else {
            panic!("expected a key action, got {decoded:?}");
        };
        assert_eq!(stroke.virtual_key, virtual_key_from_hid_usage(4).unwrap());
    }

    #[test]
    fn a_built_in_mouse_card_decodes_to_its_button() {
        let decoded = decode_built_in_card("0f82f693-5b78-4cf5-867e-020200000000");
        assert_eq!(
            decoded,
            Some(BuiltInCard::Action(ActionDefinition::MouseButton {
                button: MouseButtonKind::Right
            }))
        );
    }

    #[test]
    fn an_empty_gshift_slot_decodes_to_nothing_to_import() {
        assert_eq!(
            decode_built_in_card("0f82f693-5b78-4cf5-867e-090700000000"),
            Some(BuiltInCard::Unassigned)
        );
    }

    /// The whole reason this decoder is allowed to exist: a type it has no evidence for says so,
    /// rather than being mapped onto whatever action looks similar.
    #[test]
    fn an_unknown_built_in_type_is_reported_not_guessed() {
        let decoded = decode_built_in_card("0f82f693-5b78-4cf5-867e-7f1200000000");
        let Some(BuiltInCard::Undecoded(reason)) = decoded else {
            panic!("expected an undecoded report, got {decoded:?}");
        };
        assert!(reason.contains("0x7F"), "the reason must name the type: {reason}");
        assert!(reason.contains("not decoded"));
    }

    #[test]
    fn a_card_id_that_is_not_a_built_in_card_is_not_decoded() {
        assert!(decode_built_in_card("card-macro").is_none());
        assert!(decode_built_in_card("ffffffff-ffff-ffff-ffff-ffffffffffff").is_none());
        // Right prefix, wrong tail: not a built-in id, so it must not be read as one.
        assert!(decode_built_in_card("0f82f693-5b78-4cf5-867e-010400000001").is_none());
    }

    #[test]
    fn the_preview_lists_the_profiles_and_their_version() {
        let preview = preview(&document(), "settings.db", &resolver()).expect("the document parses");
        assert_eq!(preview.ghub_version.as_deref(), Some("2026.6.1234"));
        let profiles: Vec<&ImportItem> =
            preview.items.iter().filter(|i| i.kind == ImportItemKind::Profile).collect();
        assert_eq!(profiles.len(), 2);
        // `PROFILE_NAME_DEFAULT` is G HUB's placeholder, not a name a user chose.
        assert!(profiles[0].description.contains("Default (G HUB)"));
        assert!(profiles[0].description.contains("Doom"));
    }

    #[test]
    fn an_application_with_a_path_becomes_an_exact_path_rule() {
        let preview = preview(&document(), "settings.db", &resolver()).expect("the document parses");
        let rule = preview
            .items
            .iter()
            .find(|i| i.kind == ImportItemKind::AppRule && i.supported)
            .expect("the application with a path is importable");
        let Applier::AppRule(rule) = &rule.applier else { panic!("expected an app rule") };
        assert_eq!(rule.kind, AppMatchKind::ExactPath);
        assert_eq!(rule.executable_path.as_deref(), Some("C:\\Games\\doom.exe"));
    }

    /// G HUB does not always record a path. That is reported as something to do by hand rather
    /// than imported as a rule that would match nothing.
    #[test]
    fn an_application_without_a_path_is_reported_instead_of_invented() {
        let preview = preview(&document(), "settings.db", &resolver()).expect("the document parses");
        let item = preview
            .items
            .iter()
            .find(|i| i.kind == ImportItemKind::AppRule && !i.supported)
            .expect("the application without a path is listed");
        assert!(!item.selected, "an unsupported item starts unticked");
        assert!(item.warning.as_deref().unwrap_or_default().contains("no executable path"));
    }

    #[test]
    fn a_sequence_macro_becomes_one_macro_however_many_keys_point_at_it() {
        let preview = preview(&document(), "settings.db", &resolver()).expect("the document parses");
        let macros: Vec<&ImportItem> =
            preview.items.iter().filter(|i| i.kind == ImportItemKind::Macro).collect();
        assert_eq!(macros.len(), 1, "two slots share one card, so there is one macro");

        let Applier::Macro(definition) = &macros[0].applier else { panic!("expected a macro") };
        assert_eq!(definition.steps.len(), 3);
        assert_eq!(definition.tags, vec!["imported".to_owned(), "ghub".to_owned()]);

        // Both bindings point at that one macro's id.
        let referencing: Vec<Id> = preview
            .items
            .iter()
            .filter_map(|item| match &item.applier {
                Applier::Binding { binding, .. } => binding.action.macro_id(),
                _ => None,
            })
            .collect();
        assert_eq!(referencing.len(), 2);
        assert!(referencing.iter().all(|id| *id == definition.id));
    }

    #[test]
    fn a_keystroke_macro_becomes_a_key_action_with_its_modifiers() {
        let preview = preview(&document(), "settings.db", &resolver()).expect("the document parses");
        let item = preview
            .items
            .iter()
            .find(|i| matches!(&i.applier, Applier::Binding { slot, .. } if slot == "gkey:G2:M1"))
            .expect("G2 is bound");
        let Applier::Binding { binding, .. } = &item.applier else { panic!("expected a binding") };
        let ActionDefinition::Key(stroke) = &binding.action else { panic!("expected a key") };
        // HID usage 44 is space, 224 is left control.
        assert_eq!(stroke.virtual_key, virtual_key_from_hid_usage(44).unwrap());
        assert!(stroke.modifiers.contains(crate::action::KeyModifiers::CONTROL));
    }

    #[test]
    fn mouse_settings_come_over_with_the_default_stage_the_user_had() {
        let preview = preview(&document(), "settings.db", &resolver()).expect("the document parses");
        let item = preview
            .items
            .iter()
            .find(|i| i.kind == ImportItemKind::MouseSettings)
            .expect("the mouse card is listed");
        let Applier::Mouse { device_key, settings } = &item.applier else {
            panic!("expected mouse settings")
        };
        assert_eq!(device_key, "ghub:g903hero");
        assert_eq!(
            settings.dpi_stages.iter().map(|s| s.dpi).collect::<Vec<_>>(),
            vec![400, 800, 1600, 3200]
        );
        assert_eq!(settings.shift_dpi, Some(400));
        assert_eq!(settings.report_rate_hz, Some(1000));
        // 1600 is the third level, so the default stage is index 2 - not the raw DPI.
        assert_eq!((settings.default_stage, settings.active_stage), (2, 2));
    }

    /// A DPI value the hardware cannot be set to is dropped rather than clamped: clamping would
    /// put a stage the user never chose into their profile.
    #[test]
    fn an_out_of_range_dpi_level_is_dropped() {
        let settings = read_mouse_settings(&serde_json::json!({
            "dpiTable": {"levels": [10, 800, 999_999, 1600]}
        }));
        assert_eq!(settings.dpi_stages.iter().map(|s| s.dpi).collect::<Vec<_>>(), vec![800, 1600]);
    }

    #[test]
    fn an_unknown_card_type_is_listed_as_not_convertible() {
        let preview = preview(&document(), "settings.db", &resolver()).expect("the document parses");
        let item = preview
            .items
            .iter()
            .find(|i| i.description.contains("SOMETHING_NEW"))
            .expect("the unknown card is listed");
        assert_eq!(item.kind, ImportItemKind::Unsupported);
        assert!(!item.supported);
        assert!(item.warning.as_deref().unwrap_or_default().contains("no equivalent importer"));
    }

    /// The shapes below are copied from a real G HUB 2026.6 `settings.db`.
    fn lighting_document() -> String {
        serde_json::json!({
            "cards": {"cards": [
                {
                    "id": "fw-g903",
                    "attribute": "FIRMWARE_LIGHTING_SETTINGS",
                    "firmwareLightingSettings": {"effects": [
                        {"breathingParams": {"color": {"hex": "#ff0026"}, "intensity": 1, "periodInMs": 8000},
                         "id": "BREATHING", "zoneType": "ZONE_BRANDING"},
                        {"cycleParams": {"intensity": 1, "periodInMs": 8000}, "id": "CYCLE", "zoneType": "ZONE_PRIMARY"}
                    ]}
                },
                {
                    "id": "fw-g815",
                    "attribute": "FIRMWARE_LIGHTING_SETTINGS",
                    "firmwareLightingSettings": {"effects": [
                        {"id": "RIPPLE", "rippleParams": {"color": {"hex": "#0eccac"}, "periodInMs": 20}, "zoneType": "ZONE_PRIMARY"},
                        {},
                        {"fixedParams": {"color": {"hex": "#0eccac"}}, "id": "FIXED", "zoneType": "ZONE_BRANDING"}
                    ]}
                },
                {
                    "id": "sw",
                    "attribute": "SOFTWARE_LIGHTING_SETTINGS",
                    "softwareLightingSettings": {
                        "devices": {"g815": "KEYBOARD_RGB_PER_KEY"},
                        "effects": [{"id": "fx-1", "layerType": "INTERNAL_STANDARD",
                                     "metadata": {"deviceSupport": ["KEYBOARD_RGB_PER_KEY"]}}]
                    }
                },
                {"id": "dk", "attribute": "DISABLE_KEY_SETTINGS", "name": "Disable Keys", "readOnly": true},
                {"id": "dk-custom", "attribute": "DISABLE_KEY_SETTINGS",
                 "disableKeysSettings": {"hidUsages": [227, 43]}}
            ]},
            "lighting_effects/fx-1": {"fixedInfo": {"lightingSlots": {"infoMap": {
                "PERKEY_KEYBOARD": {"perKeyMap": {"colorCodeMap": {"4": {"hex": "#00ffff"}, "26": {"hex": "#f82f25"}}}},
                "PERKEY_GKEY": {"perKeyMap": {"colorCodeMap": {"1": {"hex": "#ea0c5d"}, "10": {"hex": "#ea0c5d"}}}},
                "PERKEY_BRANDING": {"perKeyMap": {"colorCodeMap": {"1": {"hex": "#ff002e"}}}}
            }}}},
            "lighting_prefabs": {"list": [{"id": "fx-1", "name": "New Freestyle 2", "type": "FIXED"}]},
            "profiles": {"profiles": [
                {
                    "id": "p1",
                    "name": "One",
                    "lightingCard": "sw",
                    "assignments": [
                        {"slotId": "g903hero_lighting_setting_firmware", "cardId": "fw-g903"},
                        {"slotId": "g815_lighting_setting_firmware", "cardId": "fw-g815"},
                        {"slotId": "g815_disable_keys", "cardId": "dk"}
                    ]
                },
                {
                    "id": "p2",
                    "name": "Two",
                    "assignments": [{"slotId": "g815_disable_keys", "cardId": "dk-custom"}]
                }
            ]}
        })
        .to_string()
    }

    #[test]
    fn a_firmware_lighting_card_becomes_per_zone_device_effects() {
        let preview = preview(&lighting_document(), "settings.db", &resolver()).expect("parses");
        let item = preview
            .items
            .iter()
            .find(|i| i.source == "g903hero_lighting_setting_firmware")
            .expect("the firmware card is listed");
        assert!(item.supported, "{:?}", item.warning);
        let Applier::FirmwareLighting { device_key, zones } = &item.applier else {
            panic!("expected firmware lighting, got {:?}", item.applier)
        };
        assert_eq!(device_key, "ghub:g903hero");
        assert_eq!(
            zones.get(&0x0002),
            Some(&ZoneEffect { effect_id: 0x000A, period_ms: 8000, color: Some("#FF0026".into()) })
        );
        assert_eq!(
            zones.get(&0x0001),
            Some(&ZoneEffect { effect_id: 0x0003, period_ms: 8000, color: None })
        );

        let after = apply(&preview, &ProfileDocument::create_default()).expect("valid");
        let profile = after.profiles.iter().find(|p| p.name.starts_with("One")).expect("imported");
        let lighting = &profile.device("ghub:g903hero").expect("mouse").lighting;
        assert_eq!(lighting.mode, LightingMode::DeviceEffect);
        assert_eq!(lighting.device_effect_id, Some(0x0003), "primary zone is the main effect");
        assert_eq!(lighting.zone_effects.len(), 2);
    }

    /// G HUB pads the effect list with `{}` for zones a device lacks; that is not a failure.
    #[test]
    fn an_empty_firmware_effect_placeholder_is_skipped_silently() {
        let preview = preview(&lighting_document(), "settings.db", &resolver()).expect("parses");
        let item = preview
            .items
            .iter()
            .find(|i| i.source == "g815_lighting_setting_firmware")
            .expect("listed");
        assert!(item.supported);
        assert!(item.warning.is_none(), "{:?}", item.warning);
    }

    #[test]
    fn a_freestyle_effect_becomes_per_key_colours_and_wins_over_firmware() {
        let preview = preview(&lighting_document(), "settings.db", &resolver()).expect("parses");
        let item = preview
            .items
            .iter()
            .find(|i| i.source == "g815_software_lighting")
            .expect("the software lighting is listed");
        assert!(item.supported, "{:?}", item.warning);
        assert!(item.description.contains("New Freestyle 2"));
        let Applier::PerKeyLighting { colors, .. } = &item.applier else { panic!("per-key") };
        assert_eq!(colors.get("key:A").map(String::as_str), Some("#00FFFF"), "usage 4 is A");
        assert_eq!(colors.get("key:W").map(String::as_str), Some("#F82F25"), "usage 26 is W");
        assert_eq!(colors.get("key:G1").map(String::as_str), Some("#EA0C5D"));
        assert_eq!(colors.get("key:Logo").map(String::as_str), Some("#FF002E"));
        // G10 does not exist on a G815; it is counted, not invented.
        assert!(item.warning.as_deref().unwrap_or_default().contains("1 key"));

        let after = apply(&preview, &ProfileDocument::create_default()).expect("valid");
        let profile = after.profiles.iter().find(|p| p.name.starts_with("One")).expect("imported");
        let lighting = &profile.device("ghub:g815").expect("keyboard").lighting;
        assert_eq!(lighting.mode, LightingMode::Static, "per-key wins over the firmware card");
        assert!(!lighting.zone_effects.is_empty(), "the firmware card is still kept");
    }

    #[test]
    fn game_mode_keys_import_from_the_card_or_g_hubs_default() {
        let preview = preview(&lighting_document(), "settings.db", &resolver()).expect("parses");
        let lists: Vec<&Vec<u8>> = preview
            .items
            .iter()
            .filter_map(|i| match &i.applier {
                Applier::GameModeKeys { usages, .. } => Some(usages),
                _ => None,
            })
            .collect();
        assert_eq!(lists, vec![&vec![0xE3, 0xE7], &vec![0xE3, 0x2B]]);

        let after = apply(&preview, &ProfileDocument::create_default()).expect("valid");
        let two = after.profiles.iter().find(|p| p.name.starts_with("Two")).expect("imported");
        let keyboard = two.device("ghub:g815").and_then(|d| d.keyboard.as_ref()).expect("kb");
        assert_eq!(keyboard.game_mode_disabled_usages, vec![0xE3, 0x2B]);
    }

    /// The four built-in ids the G903's own defaults use, decoded through G HUB's protobuf enums.
    #[test]
    fn built_in_mouse_and_device_commands_decode() {
        let decode = |tail: &str| decode_built_in_card(&format!("0f82f693-5b78-4cf5-867e-{tail}"));
        assert_eq!(
            decode("040100000000"),
            Some(BuiltInCard::Action(ActionDefinition::Dpi { command: DpiCommand::Next, stage: 0 }))
        );
        assert_eq!(
            decode("040200000000"),
            Some(BuiltInCard::Action(ActionDefinition::Dpi {
                command: DpiCommand::Previous,
                stage: 0
            }))
        );
        assert_eq!(
            decode("040b00000000"),
            Some(BuiltInCard::Action(ActionDefinition::MouseWheel { delta: -120, horizontal: true }))
        );
        assert_eq!(
            decode("040c00000000"),
            Some(BuiltInCard::Action(ActionDefinition::MouseWheel { delta: 120, horizontal: true }))
        );
        assert_eq!(
            decode("090100000000"),
            Some(BuiltInCard::Action(ActionDefinition::Profile {
                command: ProfileCommand::Next,
                profile_id: None
            }))
        );
        assert_eq!(
            decode("090500000000"),
            Some(BuiltInCard::Action(ActionDefinition::GShift))
        );
        // A known command with no local equivalent says which command it is.
        let Some(BuiltInCard::Undecoded(reason)) = decode("090900000000") else { panic!() };
        assert!(reason.contains("REPORT_RATE_CYCLE"), "{reason}");
        // A value outside G HUB's enum is still not guessed.
        let Some(BuiltInCard::Undecoded(reason)) = decode("040700000000") else { panic!() };
        assert!(reason.contains("not decoded"), "{reason}");
    }

    /// An empty G-Shift slot is the normal state of nearly every slot, so it must not produce a
    /// row: a preview of 300 "nothing here" rows hides the ten that matter.
    #[test]
    fn an_unassigned_shift_slot_produces_no_row() {
        let preview = preview(&document(), "settings.db", &resolver()).expect("the document parses");
        assert!(
            !preview.items.iter().any(|i| i.source == "g815_g5_m1_shifted"),
            "the unassigned shifted slot should be silent"
        );
    }

    #[test]
    fn applying_adds_profiles_without_touching_the_existing_ones() {
        let preview = preview(&document(), "settings.db", &resolver()).expect("the document parses");
        let before = ProfileDocument::create_default();
        let existing = before.profiles.len();

        let after = apply(&preview, &before).expect("the import is valid");
        assert_eq!(after.profiles.len(), existing + 2);
        for (index, profile) in before.profiles.iter().enumerate() {
            assert_eq!(&after.profiles[index], profile, "an existing profile changed");
        }
        // The imported profiles are renumbered onto the end of the real list rather than keeping
        // the preview's 1000+ placeholder order.
        assert_eq!(after.profiles[existing].order, existing as i32);
    }

    #[test]
    fn applying_puts_the_bindings_on_the_profile_they_came_from() {
        let preview = preview(&document(), "settings.db", &resolver()).expect("the document parses");
        let after = apply(&preview, &ProfileDocument::create_default()).expect("the import is valid");
        let imported = after
            .profiles
            .iter()
            .find(|p| p.name.contains("Default (G HUB)"))
            .expect("the first imported profile exists");
        let keyboard = imported.device("ghub:g815").expect("the keyboard has settings");
        assert!(keyboard.bindings.contains_key("gkey:G1:M1"));
        assert!(keyboard.bindings.contains_key("gkey:G4:M1"), "the disabled slot is a binding too");
        let mouse = imported.device("ghub:g903hero").expect("the mouse has settings");
        assert!(mouse.mouse.is_some());
    }

    /// Unticking a profile has to take its items with it; otherwise a binding would be applied to
    /// whichever profile happened to be current, which is not the one it came from.
    #[test]
    fn unticking_a_profile_drops_everything_under_it() {
        let mut preview = preview(&document(), "settings.db", &resolver()).expect("parses");
        for item in &mut preview.items {
            if item.kind == ImportItemKind::Profile && item.description.contains("Default (G HUB)")
            {
                item.selected = false;
            }
        }
        let before = ProfileDocument::create_default();
        let after = apply(&preview, &before).expect("the import is valid");
        assert_eq!(after.profiles.len(), before.profiles.len() + 1);
        assert!(!after.profiles.iter().any(|p| p.name.contains("Default (G HUB)")));
        // And nothing from it leaked into the profile that *was* ticked.
        let second = after.profiles.last().expect("the second profile was imported");
        assert!(second.devices.is_empty());
    }

    /// A binding to a macro the user did not import would be a key that looks bound and does
    /// nothing.
    #[test]
    fn unticking_a_macro_drops_the_bindings_that_ran_it() {
        let mut preview = preview(&document(), "settings.db", &resolver()).expect("parses");
        for item in &mut preview.items {
            if item.kind == ImportItemKind::Macro {
                item.selected = false;
            }
        }
        let after = apply(&preview, &ProfileDocument::create_default()).expect("the import is valid");
        let imported = after
            .profiles
            .iter()
            .find(|p| p.name.contains("Default (G HUB)"))
            .expect("the profile is there");
        let keyboard = imported.device("ghub:g815").expect("the keyboard has settings");
        assert!(after.macros.is_empty());
        assert!(!keyboard.bindings.contains_key("gkey:G1:M1"), "the macro binding was kept");
        assert!(keyboard.bindings.contains_key("gkey:G2:M1"), "the keystroke binding was dropped");
    }

    /// Importing twice must not produce two profiles with one name: the profile list is addressed
    /// by name in the UI and in the automation API.
    #[test]
    fn a_second_import_gets_distinct_names() {
        let preview = preview(&document(), "settings.db", &resolver()).expect("parses");
        let once = apply(&preview, &ProfileDocument::create_default()).expect("valid");
        let twice = apply(&preview, &once).expect("valid");
        let mut names: Vec<&str> = twice.profiles.iter().map(|p| p.name.as_str()).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "two profiles share a name");
    }

    #[test]
    fn a_resolver_that_knows_the_device_is_used_instead_of_the_placeholder_key() {
        let resolver: DeviceKeyResolver = Box::new(|prefix| {
            (prefix == "g815").then(|| "logi:046d:c33f".to_owned())
        });
        let preview = preview(&document(), "settings.db", &resolver).expect("parses");
        let keys: Vec<&str> = preview
            .items
            .iter()
            .filter_map(|item| match &item.applier {
                Applier::Binding { device_key, .. } => Some(device_key.as_str()),
                _ => None,
            })
            .collect();
        assert!(keys.contains(&"logi:046d:c33f"), "the resolved key is used: {keys:?}");
        // The mouse is not resolved, so it keeps the placeholder - which is still a stable key,
        // so the settings are importable and can be moved later.
        assert!(
            preview.items.iter().any(
                |item| matches!(&item.applier, Applier::Mouse { device_key, .. } if device_key == "ghub:g903hero")
            )
        );
    }

    #[test]
    fn a_document_without_profiles_says_so_rather_than_failing() {
        let preview = preview("{}", "settings.db", &resolver()).expect("an empty object parses");
        assert!(preview.items.is_empty());
        assert_eq!(preview.warnings, vec!["No profiles found in G HUB settings".to_owned()]);
    }

    #[test]
    fn invalid_json_is_an_error_with_the_reason_in_it() {
        let error = preview("{not json", "settings.db", &resolver()).expect_err("must fail");
        let StoreError::Validation(errors) = error else { panic!("expected a validation error") };
        assert!(errors[0].contains("not valid JSON"), "{errors:?}");
    }

    #[test]
    fn an_oversized_document_is_refused_before_it_is_parsed() {
        let huge = " ".repeat(MAX_IMPORT_BYTES * 4 + 1);
        let error = preview(&huge, "settings.db", &resolver()).expect_err("must fail");
        let StoreError::Validation(errors) = error else { panic!("expected a validation error") };
        assert_eq!(errors, vec!["G HUB settings document too large".to_owned()]);
    }

    #[test]
    fn the_summary_counts_both_halves_of_the_list() {
        let preview = preview(&document(), "settings.db", &resolver()).expect("parses");
        assert_eq!(
            preview.supported_count() + preview.unsupported_count(),
            preview.items.len()
        );
        let summary = preview.summary();
        assert!(summary.contains("2026.6.1234"));
        assert!(summary.contains("temporary copy"), "the read-only guarantee is part of the line");
    }

    #[test]
    fn the_default_settings_path_ends_where_g_hub_keeps_it() {
        let path = default_settings_path();
        let text = path.to_string_lossy().replace('\\', "/");
        assert!(text.ends_with("LGHUB/settings.db"), "{text}");
    }

    /// The reader has to work against a real SQLite file, because the point of it is the query and
    /// the blob column - both of which a stub would assume rather than check.
    #[test]
    fn the_reader_takes_the_newest_row() {
        let directory = temp_directory().expect("a temporary directory");
        let path = directory.join("settings.db");
        {
            let connection = rusqlite::Connection::open(&path).expect("a database");
            connection
                .execute_batch(
                    "CREATE TABLE data (_id INTEGER PRIMARY KEY, file BLOB);
                     INSERT INTO data (_id, file) VALUES (1, CAST('{\"v\":1}' AS BLOB));
                     INSERT INTO data (_id, file) VALUES (2, CAST('{\"v\":2}' AS BLOB));",
                )
                .expect("the table is created");
        }

        let json = read_latest_blob(&path).expect("the blob is read");
        assert_eq!(json, "{\"v\":2}");

        // And the whole path, including the temporary copy, works on a file that exists.
        let through_copy = load_settings_json(&path).expect("the copy is read");
        assert_eq!(through_copy, "{\"v\":2}");

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_database_without_the_data_table_is_an_error_not_a_panic() {
        let directory = temp_directory().expect("a temporary directory");
        let path = directory.join("settings.db");
        {
            let connection = rusqlite::Connection::open(&path).expect("a database");
            connection.execute_batch("CREATE TABLE other (x INTEGER)").expect("created");
        }
        let error = read_latest_blob(&path).expect_err("must fail");
        assert!(error.contains("no 'data' table"), "{error}");
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// Against the real thing, when the real thing is there.
    ///
    /// Skipped rather than failed when G HUB is not installed, because the suite has to pass on a
    /// machine without it. On a machine with it, this is the only test that exercises the actual
    /// file format - including a multi-megabyte `-wal`, which is where a running G HUB keeps the
    /// newest document and the one thing a hand-made fixture cannot reproduce.
    #[test]
    fn the_installed_g_hub_settings_can_be_read_and_previewed() {
        let path = default_settings_path();
        if !path.exists() {
            eprintln!("skipped: no G HUB settings at {}", path.display());
            return;
        }

        let before = std::fs::metadata(&path).expect("the settings file is readable");
        let json = match load_settings_json(&path) {
            Ok(json) => json,
            Err(e) => panic!("{} could not be read: {e}", path.display()),
        };
        assert!(json.trim_start().starts_with('{'), "the blob is not a JSON document");

        let resolver: DeviceKeyResolver = Box::new(|p| Some(format!("ghub:{p}")));
        let preview = preview(&json, &path.display().to_string(), &resolver)
            .unwrap_or_else(|e| panic!("the real document did not preview: {e}"));
        eprintln!(
            "G HUB {:?}: {} items ({} importable, {} not), {} profiles",
            preview.ghub_version,
            preview.items.len(),
            preview.supported_count(),
            preview.unsupported_count(),
            preview.profiles.len()
        );

        // The promise this module makes about the original file, checked rather than asserted in
        // a comment: same size, same modification time afterwards.
        let after = std::fs::metadata(&path).expect("the settings file is still readable");
        assert_eq!(before.len(), after.len(), "the original file changed size");
        assert_eq!(
            before.modified().ok(),
            after.modified().ok(),
            "the original file's timestamp moved"
        );
    }

    #[test]
    fn a_missing_file_is_reported_by_path() {
        let missing = std::env::temp_dir().join("ghub-rs-no-such-settings.db");
        let error = load_settings_json(&missing).expect_err("must fail");
        assert!(error.contains("could not be read"), "{error}");
    }
}
