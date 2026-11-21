//! The keyboard device screen. Port of `KeyboardTabViewModel.cs`, `KeyboardTabView.axaml` and
//! `KeyboardTabView.axaml.cs`.
//!
//! This is G HUB's device screen for the G815: an icon rail down the left, a settings panel beside
//! it, and the keyboard itself filling the rest. The rail's five destinations are LIGHTSYNC (with
//! its own PRESETS / FREESTYLE / ANIMATIONS tabs), Assignments, Game Mode, Press Reactions and
//! Device Settings.
//!
//! Three behaviours are worth stating because they are easy to lose in a port:
//!
//! * **Key colours save themselves.** Painting is a drag, so a write per change would be a profile
//!   write per mouse move. Edits are collapsed by [`SAVE_DEBOUNCE`] and written once the pointer
//!   settles, which is why the panel's Save button says it is only needed for the *other* settings.
//! * **A reload must not eat an unsaved stroke.** `_lightingDirty` guarded that in the C#: while
//!   edits are pending, a document change refreshes everything *except* the colour map. Without it
//!   the autosave's own `DocumentChanged` would overwrite whatever was painted while it was saving.
//! * **The picker recolours the selection live.** Dragging in the colour square repaints the
//!   selected keys as it moves, and the whole drag is one undo step - that is what `_colorSession`
//!   is for.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use egui::{Color32, Ui, Vec2};
use ghub_core::effects::{self, KeyEffectDefinition};
use ghub_core::layout::{self, KeyGroup};
use ghub_core::profile::{
    AnimationDirection, AnimationSettings, KeyReaction, KeyReactionMode, KeyboardSettings,
    LightingMode, LightingSettings, is_hex_color,
};
use ghub_core::service::{MacroRecordState, gkey_slot};
use ghub_hidpp::identity::PeripheralKind;

use crate::color_picker::{ColorPicker, parse_hex, to_hex};
use crate::controls as c;
use crate::icons::{Icon, IconCache};
use crate::shared::{Changes, Shared};
use crate::theme as t;
use crate::widgets as w;

use super::action_editor::ActionEditor;
use super::device_stage::{
    self, GameModeKeyState, KeyView, Overlay, StageAction, StageInput, build_keys,
};
use super::power_save::{self, PowerSave};
use super::{scroll, status_band};

/// `Task.Delay(450)` - how long a colour edit waits before it is written.
///
/// Long enough that a whole brush stroke is one write, short enough that letting go of the mouse
/// feels like it saved.
pub const SAVE_DEBOUNCE: Duration = Duration::from_millis(450);

/// `_paintUndo` is capped at this many steps.
const UNDO_DEPTH: usize = 50;

/// `KeyboardTabViewModel.Palettes` - G HUB's swatch rows.
pub static PALETTES: &[(&str, [&str; 7])] = &[
    ("VIBRANT", ["#F32A2A", "#18A3E0", "#6CFF1A", "#8C3BF0", "#FF7C10", "#7DD321", "#FFE117"]),
    ("PASTEL", ["#FF9AA2", "#FFB7B2", "#FFDAC1", "#E2F0CB", "#B5EAD7", "#C7CEEA", "#E0BBE4"]),
    ("WARM", ["#FF0000", "#FF3D00", "#FF6A00", "#FF9500", "#FFC400", "#FFE600", "#FF0066"]),
    ("COOL", ["#00FFFF", "#00B4FF", "#0066FF", "#2A00FF", "#7A00FF", "#00FF9C", "#00FF3C"]),
    ("MONO", ["#FFFFFF", "#D0D0D0", "#A0A0A0", "#707070", "#404040", "#202020", "#000000"]),
];

/// `QuickGroupNames`. "ALL" is not drawn as a button - it is reachable with Ctrl+A, exactly as in
/// the C#, where `QuickGroups` filtered it out.
pub static QUICK_GROUP_NAMES: &[&str] = &[
    "WASD",
    "NUMBERS",
    "F-KEYS",
    "G-KEYS",
    "MODIFIER",
    "ARROW KEYS",
    "KEYS ONLY",
    "LOGO",
    "ALL",
];

/// `DefaultDisabledUsages` - what the keyboard locks out by default in game mode: both Windows
/// keys, which is what G HUB shows hatched.
pub static DEFAULT_DISABLED_USAGES: &[u8] = &[0xE3, 0xE7];

/// `ReactionModes`, worded the way the list shows them.
pub static REACTION_MODES: &[(KeyReactionMode, &str)] = &[
    (KeyReactionMode::Momentary, "While held"),
    (KeyReactionMode::Toggle, "Until pressed again"),
];

pub static LIGHTING_MODES: &[LightingMode] = &[
    LightingMode::Artemis,
    LightingMode::Static,
    LightingMode::DeviceEffect,
    LightingMode::Untouched,
    LightingMode::Animated,
];

pub static ANIMATION_DIRECTIONS: &[AnimationDirection] = &[
    AnimationDirection::Right,
    AnimationDirection::Left,
    AnimationDirection::Up,
    AnimationDirection::Down,
    AnimationDirection::Out,
    AnimationDirection::In,
];

/// `KeyboardPage` - the rail's destinations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SubPage {
    #[default]
    Lightsync,
    Assignments,
    GameMode,
    /// Per-key press reactions. Not a G HUB page; see [`ReactionRow`].
    Reactions,
    DeviceSettings,
}

/// The three tabs inside LIGHTSYNC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LightsyncTab {
    /// Effects the keyboard's own firmware runs (0x8071).
    Presets,
    /// The colour layout this application writes per key.
    #[default]
    Freestyle,
    /// Effects rendered here and streamed to the keyboard.
    Animations,
}

/// `ColorUsageRow` - one colour and how many keys currently show it.
#[derive(Debug, Clone)]
struct ColorUsage {
    hex: String,
    count: usize,
}

impl ColorUsage {
    /// `Count == 1 ? "1 KEY" : $"{Count} KEYS"`.
    fn text(&self) -> String {
        if self.count == 1 { "1 KEY".to_owned() } else { format!("{} KEYS", self.count) }
    }
}

/// `GameModeKeyRow`.
#[derive(Debug, Clone)]
struct GameModeKeyRow {
    usage: u8,
    disabled: bool,
}

/// `GKeySlotRow`.
#[derive(Debug, Clone)]
struct SlotRow {
    gkey: usize,
    bank: usize,
    shift: bool,
    description: String,
}

impl SlotRow {
    fn label(&self) -> String {
        format!(
            "G{} · M{}{}",
            self.gkey,
            self.bank,
            if self.shift { " · G-Shift" } else { "" }
        )
    }

    fn slot(&self) -> String {
        gkey_slot(self.gkey, self.bank, self.shift)
    }
}

/// `KeyReactionRow` - one key's press reaction.
///
/// The hex string is what is stored and the picker edits a colour; the C# kept both in sync behind
/// a re-entrancy guard. Here the hex is the single source of truth and the picker is handed a
/// colour parsed from it each frame, which removes the guard and the loop it was guarding against.
struct ReactionRow {
    key_id: String,
    label: String,
    hex: String,
    mode: KeyReactionMode,
    enabled: bool,
    picker_open: bool,
    picker: ColorPicker,
    /// The hex field's edit buffer, so a half-typed `#F` does not reset on the next frame.
    hex_buffer: String,
}

impl ReactionRow {
    fn new(key_id: String, label: String, hex: String, mode: KeyReactionMode, enabled: bool) -> Self {
        let colour = parse_hex(&hex).unwrap_or(Color32::RED);
        Self {
            key_id,
            label,
            hex_buffer: hex.clone(),
            hex,
            mode,
            enabled,
            picker_open: false,
            picker: ColorPicker::new(colour),
        }
    }

    fn colour(&self) -> Color32 {
        parse_hex(&self.hex).unwrap_or(Color32::RED)
    }

    /// `ToModel()`.
    fn to_model(&self) -> KeyReaction {
        KeyReaction {
            key_id: self.key_id.clone(),
            color: self.hex.clone(),
            mode: self.mode,
            enabled: self.enabled,
        }
    }
}

/// What a frame's interaction asked for, applied after the draw.
///
/// Collecting them means the drawing code never calls into [`Shared`] mid-layout, which is what
/// keeps a click from reordering the list it is being drawn from.
#[derive(Default)]
struct Pending {
    save_lighting: bool,
    save_keyboard: bool,
    save_reactions: bool,
    save_animation: bool,
    save_custom_colors: bool,
    read_power_save: bool,
    select_bank: Option<usize>,
    apply_slot: bool,
    clear_slot: bool,
    copy_bank_to: Option<usize>,
    reset_bank: bool,
    start_record: bool,
    stop_record: bool,
    cancel_record: bool,
}

pub struct KeyboardPage {
    // ------------------------------------------------------------- device
    /// `(key, label)` per keyboard, as `Keyboards` held `DeviceChoice`.
    keyboards: Vec<(String, String)>,
    selected_device: Option<String>,

    // ------------------------------------------------------------- stage
    keys: Vec<KeyView>,
    stage: StageInput,
    /// The asset folder the render came from. One model for now, which is why it is a constant
    /// rather than resolved from the descriptor.
    artwork_folder: &'static str,
    /// Whether [`Self::keys`] carry the render's geometry. Tracked rather than inferred from a
    /// key's bounds, because some keys legitimately have none even with the artwork loaded.
    keys_from_artwork: bool,

    // ------------------------------------------------------------- navigation
    page: SubPage,
    lightsync_tab: LightsyncTab,

    // ------------------------------------------------------------- colour editing
    current_colour: Color32,
    picker: ColorPicker,
    picker_open: bool,
    brush_mode: bool,
    eyedropper: bool,
    palette_name: String,
    custom_colors: Vec<String>,
    colors_on_device: Vec<ColorUsage>,
    /// `_colors`: the per-key colour map, keyed `key:<id>`, exactly as the profile stores it.
    colors: HashMap<String, String>,
    paint_undo: std::collections::VecDeque<HashMap<String, String>>,
    /// One undo step per continuous picker drag on the same selection.
    color_session: bool,
    /// Unsaved edits: a profile reload must not overwrite them.
    lighting_dirty: bool,
    save_due: Option<Instant>,

    // ------------------------------------------------------------- lighting settings
    lighting_mode: LightingMode,
    default_color: String,
    default_color_buffer: String,
    light_brightness: f64,
    brightness_percent: i64,
    brightness_buffer: String,

    // ------------------------------------------------------------- PRESETS
    /// `(name, effect id)` from 0x8071, read-only.
    device_effects: Vec<(String, u16)>,
    device_effect: Option<u16>,
    effect_period: i64,
    effect_period_buffer: String,

    // ------------------------------------------------------------- ANIMATIONS
    selected_animation: usize,
    animation_speed: f64,
    animation_fps: i64,
    animation_fps_buffer: String,
    animation_intensity: f64,
    wave_direction: AnimationDirection,
    animation_colors: Vec<String>,

    // ------------------------------------------------------------- Game Mode
    game_mode_enabled: bool,
    game_mode_keys: Vec<GameModeKeyRow>,
    software_gkeys: bool,

    // ------------------------------------------------------------- Assignments
    active_bank: usize,
    show_shift_layer: bool,
    slots: Vec<SlotRow>,
    selected_slot: Option<usize>,
    editor: ActionEditor,
    record_gkey: i64,
    record_gkey_buffer: String,

    // ------------------------------------------------------------- Press Reactions
    reactions: Vec<ReactionRow>,
    selected_reaction: Option<usize>,

    power_save: PowerSave,
    loaded: bool,
}

impl Default for KeyboardPage {
    fn default() -> Self {
        Self {
            keyboards: Vec::new(),
            selected_device: None,
            // Built once with no metadata; `load` rebuilds them against the artwork when it is
            // available, because the geometry depends on it.
            keys: build_keys(None),
            stage: StageInput::default(),
            artwork_folder: "g815_us",
            keys_from_artwork: false,
            page: SubPage::default(),
            lightsync_tab: LightsyncTab::default(),
            // `_currentColor = Color.Parse("#00FFFF")`.
            current_colour: Color32::from_rgb(0x00, 0xFF, 0xFF),
            picker: ColorPicker::new(Color32::from_rgb(0x00, 0xFF, 0xFF)),
            picker_open: false,
            brush_mode: false,
            eyedropper: false,
            palette_name: "VIBRANT".to_owned(),
            custom_colors: Vec::new(),
            colors_on_device: Vec::new(),
            colors: HashMap::new(),
            paint_undo: std::collections::VecDeque::new(),
            color_session: false,
            lighting_dirty: false,
            save_due: None,
            lighting_mode: LightingMode::Untouched,
            default_color: "#FFFFFF".to_owned(),
            default_color_buffer: "#FFFFFF".to_owned(),
            light_brightness: 100.0,
            brightness_percent: 100,
            brightness_buffer: "100".to_owned(),
            device_effects: Vec::new(),
            device_effect: None,
            effect_period: 5000,
            effect_period_buffer: "5000".to_owned(),
            selected_animation: 0,
            animation_speed: 100.0,
            animation_fps: 25,
            animation_fps_buffer: "25".to_owned(),
            animation_intensity: 100.0,
            wave_direction: AnimationDirection::Right,
            animation_colors: Vec::new(),
            game_mode_enabled: false,
            game_mode_keys: Vec::new(),
            software_gkeys: true,
            active_bank: 1,
            show_shift_layer: false,
            slots: Vec::new(),
            selected_slot: None,
            editor: ActionEditor::default(),
            record_gkey: 1,
            record_gkey_buffer: "1".to_owned(),
            reactions: Vec::new(),
            selected_reaction: None,
            power_save: PowerSave::default(),
            loaded: false,
        }
    }
}

// =============================================================== state queries

impl KeyboardPage {
    fn is_freestyle(&self) -> bool {
        self.page == SubPage::Lightsync && self.lightsync_tab == LightsyncTab::Freestyle
    }

    /// `SelectionInfo` - the heading over the keyboard.
    fn selection_info(&self) -> String {
        let selected = self.keys.iter().filter(|k| k.selected).count();
        if selected > 0 {
            format!("{selected} SELECTED")
        } else if self.brush_mode {
            "BRUSH: CLICK OR DRAG OVER KEYS".to_owned()
        } else {
            "CLICK OR DRAG TO SELECT".to_owned()
        }
    }

    /// `DeviceDisplayName` - the first word of the model name, upper-cased.
    fn device_display_name(&self, shared: &Shared) -> String {
        self.device(shared)
            .and_then(|d| d.descriptor.as_ref().map(|desc| desc.display_name.clone()))
            .and_then(|name| name.split(' ').next().map(str::to_uppercase))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "KEYBOARD".to_owned())
    }

    fn firmware_version(&self, shared: &Shared) -> String {
        self.device(shared)
            .and_then(|d| d.descriptor.as_ref().and_then(|desc| desc.firmware_version.clone()))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "unknown".to_owned())
    }

    fn device(&self, shared: &Shared) -> Option<ghub_core::manager::ManagedDevice> {
        self.selected_device.as_deref().and_then(|key| shared.device(key))
    }

    /// `GKeyInfo` - what the device said about its G-keys, and what that means in practice.
    fn gkey_info(&self, shared: &Shared) -> String {
        match self.device(shared).and_then(|d| d.capabilities.gkeys.clone()) {
            None => "No G-keys reported.".to_owned(),
            Some(gkeys) => {
                let banks = self
                    .device(shared)
                    .and_then(|d| d.capabilities.mode_keys.as_ref().map(|m| m.count))
                    .unwrap_or(1);
                format!(
                    "{} G-keys × {banks} banks = {} positions (+ G-Shift layer). Executed in \
                     software; G-keys report through HID++ 0x8010 when software control is on. The \
                     physical bank (M-key) state cannot be read from the device; the app keeps it \
                     and re-applies the indicator on reconnect.",
                    gkeys.count,
                    gkeys.count * banks
                )
            }
        }
    }

    /// `RecordState`.
    fn record_state(&self, shared: &Shared) -> String {
        match shared.record_state() {
            MacroRecordState::SelectTarget => {
                "MR pressed: press the G-key to record to (MR again cancels)".to_owned()
            }
            MacroRecordState::Recording => {
                let target = shared.service().recording_target();
                format!(
                    "Recording to G{} M{}… press MR or Stop to save",
                    target.as_ref().map_or(0, |t| t.gkey),
                    target.as_ref().map_or(0, |t| t.bank)
                )
            }
            MacroRecordState::Idle => {
                "Idle. Physical workflow: MR → G-key → type → MR.".to_owned()
            }
        }
    }

    fn animation(&self) -> Option<&'static KeyEffectDefinition> {
        effects::CATALOG.get(self.selected_animation)
    }

    /// `MaxAnimationColors` - 0 means the effect paints its own rainbow.
    fn max_animation_colors(&self) -> usize {
        self.animation().map_or(0, |e| e.max_colors)
    }

    fn animation_running(&self) -> bool {
        self.lighting_mode == LightingMode::Animated
    }

    /// `AnimationSummary`.
    fn animation_summary(&self) -> String {
        match self.animation() {
            None => "Pick an animation to light the keyboard.".to_owned(),
            Some(effect) if self.animation_running() => {
                format!("{} is running at {} fps.", effect.name, self.animation_fps)
            }
            Some(effect) => format!("{} selected. Press Play to run it.", effect.name),
        }
    }

    /// `ReactionsNeedLighting`: reactions are painted *over* lighting this app writes, so on a
    /// firmware preset, on Artemis streaming or left untouched they have nothing to draw on.
    ///
    /// Saying so is the whole point - without it the page looks configured while the keyboard does
    /// nothing when a key is pressed.
    fn reactions_need_lighting(&self) -> bool {
        !matches!(self.lighting_mode, LightingMode::Static | LightingMode::Animated)
    }

    fn palette(&self) -> &'static [&'static str; 7] {
        PALETTES
            .iter()
            .find(|(name, _)| *name == self.palette_name)
            .map_or(&PALETTES[0].1, |(_, colours)| colours)
    }

    /// `ColorOf` - a key's colour, falling back to the unpainted-key colour.
    fn color_of(&self, index: usize) -> String {
        let id = self.keys[index].key.id;
        self.colors
            .get(&format!("key:{id}"))
            .cloned()
            .unwrap_or_else(|| normalise_hex(&self.default_color))
    }
}

// =============================================================== loading

impl KeyboardPage {
    /// `Refresh()` - rebuild the keyboard list, keeping the selection if it is still there.
    fn reload_devices(&mut self, shared: &Shared) {
        let keep = self.selected_device.clone();
        self.keyboards = shared
            .devices_of_kind(PeripheralKind::Keyboard)
            .into_iter()
            .map(|d| {
                let state = if d.connected { "connected" } else { "disconnected" };
                let name = d
                    .descriptor
                    .as_ref()
                    .map_or_else(|| d.key(), |desc| desc.display_name.clone());
                (d.identity.key(), format!("{name} ({state})"))
            })
            .collect();
        self.selected_device = keep
            .filter(|key| self.keyboards.iter().any(|(k, _)| k == key))
            .or_else(|| self.keyboards.first().map(|(k, _)| k.clone()));
    }

    /// `Load()`.
    fn load(&mut self, shared: &mut Shared, ctx: &egui::Context) {
        // The key geometry depends on the artwork, so it is resolved here rather than in `default`:
        // the texture cache needs a context and the first frame is the first chance to get one.
        let metadata = shared
            .artwork
            .get(ctx, self.artwork_folder)
            .map(|art| art.metadata.clone());
        if !self.keys_from_artwork {
            if let Some(metadata) = &metadata {
                // The selection is the user's, so it survives the rebuild.
                let selected: Vec<bool> = self.keys.iter().map(|k| k.selected).collect();
                self.keys = build_keys(Some(metadata));
                for (key, was) in self.keys.iter_mut().zip(selected) {
                    key.selected = was;
                }
                self.keys_from_artwork = true;
            }
        }

        let Some(device_key) = self.selected_device.clone() else { return };
        let profile = shared.active_profile();
        let settings = profile.device(&device_key);
        let keyboard = settings.and_then(|s| s.keyboard.clone()).unwrap_or_default();
        let lighting = settings.map(|s| s.lighting.clone()).unwrap_or_default();

        self.active_bank = keyboard.active_bank;
        self.brightness_percent = keyboard.brightness_percent.unwrap_or(100) as i64;
        self.brightness_buffer = self.brightness_percent.to_string();
        self.game_mode_enabled = keyboard.game_mode_enabled;
        self.software_gkeys = keyboard.software_gkeys;

        let document = shared.document();
        if self.custom_colors != document.app.custom_colors {
            self.custom_colors = document
                .app
                .custom_colors
                .iter()
                .filter(|hex| is_hex_color(hex))
                .cloned()
                .collect();
        }

        // A reload after an autosave must not drop strokes made while it was saving.
        if !self.lighting_dirty {
            self.lighting_mode = lighting.mode;
            self.default_color = lighting.default_color.clone();
            self.default_color_buffer = self.default_color.clone();
            self.light_brightness = lighting.brightness * 100.0;
            let supported =
                power_save::supports_power_timers(&shared.host.devices, Some(&device_key));
            self.power_save.load(&lighting, supported);
            self.colors = lighting.static_colors.clone();
            self.load_animation(&lighting);
        }

        // 0x8071's effect list, de-duplicated by effect id across clusters: the same effect is
        // reported once per cluster and the PRESETS tab offers one entry per effect.
        self.device_effects.clear();
        if let Some(effects) = shared.host.devices.with_session(&device_key, |session| {
            session.rgb().map(|rgb| {
                let mut seen = Vec::new();
                for effect in rgb.clusters.iter().flat_map(|c| c.effects.iter()) {
                    if !seen.iter().any(|(_, id)| *id == effect.effect_id) {
                        seen.push((effect.name(), effect.effect_id));
                    }
                }
                seen
            })
        }) {
            self.device_effects = effects.unwrap_or_default();
        }
        self.device_effect = lighting
            .device_effect_id
            .filter(|id| self.device_effects.iter().any(|(_, candidate)| candidate == id))
            .or_else(|| self.device_effects.first().map(|(_, id)| *id));
        self.effect_period = lighting.device_effect_period_ms as i64;
        self.effect_period_buffer = self.effect_period.to_string();

        self.update_fills();
        self.load_reactions(&keyboard);

        // `OrderBy(Group).ThenBy(Id)`. `KeyGroup` has no ordering of its own, so the debug label
        // stands in for the declaration order the C# sorted by.
        self.game_mode_keys = layout::game_mode_candidates()
            .into_iter()
            // The label is not carried on the row: the list is drawn from `layout::by_usage`,
            // so a copy here would be a second name for the same key that could go stale.
            .map(|key| GameModeKeyRow {
                usage: key.hid_usage,
                disabled: keyboard.game_mode_disabled_usages.contains(&key.hid_usage),
            })
            .collect();
        self.game_mode_keys.sort_by_key(|row| {
            let key = layout::by_usage(row.usage);
            (
                key.map(|k| format!("{:?}", k.group)).unwrap_or_default(),
                key.map(|k| k.id).unwrap_or_default().to_owned(),
            )
        });
        self.sync_game_states();

        self.editor.macros = document
            .macros
            .iter()
            .map(|m| (m.name.clone(), m.id))
            .collect();
        self.load_slots(shared);
    }

    /// `LoadSlots()`.
    fn load_slots(&mut self, shared: &Shared) {
        self.slots.clear();
        let Some(device_key) = self.selected_device.clone() else { return };
        let profile = shared.active_profile();
        let bindings = profile
            .device(&device_key)
            .map(|s| s.bindings.clone())
            .unwrap_or_default();
        let device = self.device(shared);
        let banks = device
            .as_ref()
            .and_then(|d| d.capabilities.mode_keys.as_ref().map(|m| m.count))
            .unwrap_or(3);
        let gkeys = device
            .as_ref()
            .and_then(|d| d.capabilities.gkeys.as_ref().map(|g| g.count))
            .unwrap_or(5);

        for bank in 1..=banks {
            for gkey in 1..=gkeys {
                let slot = gkey_slot(gkey, bank, self.show_shift_layer);
                let description = bindings.get(&slot).map(|b| b.action.describe()).unwrap_or_else(
                    || {
                        if self.show_shift_layer {
                            "(base layer)".to_owned()
                        } else {
                            "Unassigned".to_owned()
                        }
                    },
                );
                self.slots.push(SlotRow { gkey, bank, shift: self.show_shift_layer, description });
            }
        }
        // The selection is an index into a list that was just rebuilt, so it only survives if the
        // list is still at least that long.
        if self.selected_slot.is_some_and(|index| index >= self.slots.len()) {
            self.selected_slot = None;
        }
    }

    /// `LoadAnimation(LightingSettings)`.
    fn load_animation(&mut self, lighting: &LightingSettings) {
        let animation = &lighting.animation;
        let wanted = effects::definition(&animation.effect).id;
        self.selected_animation =
            effects::CATALOG.iter().position(|e| e.id == wanted).unwrap_or(0);
        self.animation_speed = (animation.speed * 100.0).clamp(5.0, 1000.0);
        self.animation_fps = (animation.frames_per_second as i64).clamp(1, 60);
        self.animation_fps_buffer = self.animation_fps.to_string();
        self.animation_intensity = (animation.intensity * 100.0).clamp(0.0, 200.0);
        self.wave_direction = animation.direction;
        self.animation_colors =
            animation.colors.iter().filter(|hex| is_hex_color(hex)).cloned().collect();
    }

    /// `LoadReactions(KeyboardSettings)`.
    fn load_reactions(&mut self, keyboard: &KeyboardSettings) {
        self.reactions = keyboard
            .key_reactions
            .iter()
            .filter_map(|reaction| {
                let key = layout::by_id(&reaction.key_id)?;
                Some(ReactionRow::new(
                    reaction.key_id.clone(),
                    key_label(key.id, key.label),
                    reaction.color.clone(),
                    reaction.mode,
                    reaction.enabled,
                ))
            })
            .collect();
        self.selected_reaction = if self.reactions.is_empty() { None } else { Some(0) };
        self.update_reaction_overlay();
    }

    /// `UpdateFills()` - mirrors the colour map onto the keys and rebuilds "colors on device".
    fn update_fills(&mut self) {
        let mut usage: HashMap<String, usize> = HashMap::new();
        for index in 0..self.keys.len() {
            if !self.keys[index].lit() {
                self.keys[index].colour = None;
                continue;
            }
            let hex = self.color_of(index);
            // An unparseable hex becomes grey rather than vanishing, so a corrupt profile still
            // shows the key as painted - which is the truth, and the user can repaint it.
            self.keys[index].colour = Some(parse_hex(&hex).unwrap_or(Color32::GRAY));
            *usage.entry(hex).or_default() += 1;
        }

        // `OrderByDescending(Value).ThenBy(Key)`.
        let mut rows: Vec<ColorUsage> =
            usage.into_iter().map(|(hex, count)| ColorUsage { hex, count }).collect();
        rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.hex.cmp(&b.hex)));
        self.colors_on_device = rows;
    }

    /// `SyncGameStates()` - mirrors the disabled-key list onto the keys drawn over the render.
    fn sync_game_states(&mut self) {
        let disabled: Vec<u8> =
            self.game_mode_keys.iter().filter(|r| r.disabled).map(|r| r.usage).collect();
        for key in &mut self.keys {
            let usage = key.key.hid_usage;
            key.game_state = if usage == 0 || !key.key.is_game_mode_candidate() {
                GameModeKeyState::Locked
            } else if disabled.contains(&usage) {
                if DEFAULT_DISABLED_USAGES.contains(&usage) {
                    GameModeKeyState::DisabledByDefault
                } else {
                    GameModeKeyState::DisabledByUser
                }
            } else {
                GameModeKeyState::Enabled
            };
        }
    }

    /// `UpdateReactionOverlay()`.
    fn update_reaction_overlay(&mut self) {
        let active: Vec<(String, Color32)> = self
            .reactions
            .iter()
            .filter(|r| r.enabled)
            .map(|r| (r.key_id.clone(), r.colour()))
            .collect();
        for key in &mut self.keys {
            key.reaction = active
                .iter()
                .find(|(id, _)| id == key.key.id)
                .map(|(_, colour)| *colour);
        }
    }
}

// =============================================================== editing

impl KeyboardPage {
    fn push_paint_undo(&mut self) {
        self.paint_undo.push_back(self.colors.clone());
        while self.paint_undo.len() > UNDO_DEPTH {
            self.paint_undo.pop_front();
        }
    }

    /// `UndoPaint`.
    fn undo_paint(&mut self) {
        if let Some(previous) = self.paint_undo.pop_back() {
            self.colors = previous;
            self.color_session = false;
            self.update_fills();
            self.schedule_save();
        }
    }

    /// `MarkLightingEdited()` - editing a freestyle layout implies static lighting, because
    /// "Untouched" would never show it.
    fn mark_lighting_edited(&mut self) {
        self.lighting_dirty = true;
        if self.lighting_mode == LightingMode::Untouched {
            self.lighting_mode = LightingMode::Static;
        }
    }

    /// `ScheduleSave()`.
    fn schedule_save(&mut self) {
        self.save_due = Some(Instant::now() + SAVE_DEBOUNCE);
    }

    /// `SelectKeys(keys, applyColor)`.
    fn select_keys(&mut self, indices: &[usize], apply_color: bool) {
        for (index, key) in self.keys.iter_mut().enumerate() {
            key.selected = indices.contains(&index) && key.lit();
        }
        // `SelectionChanged()`: a new selection ends the current undo session, so the next picker
        // drag starts its own step.
        self.color_session = false;

        if !apply_color {
            return;
        }
        let hex = to_hex(self.current_colour);
        let changed: Vec<usize> = self
            .keys
            .iter()
            .enumerate()
            .filter(|(index, key)| key.selected && self.color_of(*index) != hex)
            .map(|(index, _)| index)
            .collect();
        if changed.is_empty() {
            return;
        }
        self.push_paint_undo();
        for index in changed {
            self.colors.insert(format!("key:{}", self.keys[index].key.id), hex.clone());
        }
        self.mark_lighting_edited();
        self.update_fills();
        self.schedule_save();
    }

    /// `ClickAt(x, y, additive)`.
    fn click_at(&mut self, at: egui::Pos2, additive: bool, has_artwork: bool) {
        let hit = device_stage::key_at(&self.keys, has_artwork, at, true);
        let Some(index) = hit else {
            if !additive {
                self.select_keys(&[], false);
                self.picker_open = false;
            }
            return;
        };

        // Ctrl-clicking a selected key removes it, which is the only way to shrink a selection.
        if additive && self.keys[index].selected {
            self.keys[index].selected = false;
            self.color_session = false;
            return;
        }

        let mut selection: Vec<usize> = if additive {
            self.keys
                .iter()
                .enumerate()
                .filter(|(_, key)| key.selected)
                .map(|(i, _)| i)
                .collect()
        } else {
            Vec::new()
        };
        selection.push(index);
        self.select_keys(&selection, true);
    }

    /// `BrushAt(x, y)`.
    fn brush_at(&mut self, at: egui::Pos2, has_artwork: bool) {
        let Some(index) = device_stage::key_at(&self.keys, has_artwork, at, true) else { return };
        let hex = to_hex(self.current_colour);
        if self.color_of(index) == hex {
            return;
        }
        self.colors.insert(format!("key:{}", self.keys[index].key.id), hex);
        self.mark_lighting_edited();
        self.update_fills();
        self.schedule_save();
    }

    /// `SampleAt(x, y)` - the eyedropper.
    fn sample_at(&mut self, at: egui::Pos2, has_artwork: bool) {
        let Some(index) = device_stage::key_at(&self.keys, has_artwork, at, true) else { return };
        let hex = self.color_of(index);
        if let Some(colour) = parse_hex(&hex) {
            self.set_colour_quietly(colour);
        }
        self.eyedropper = false;
    }

    /// `SetColorQuietly` - changes the picker's colour without recolouring the selection.
    fn set_colour_quietly(&mut self, colour: Color32) {
        self.current_colour = colour;
        self.picker.set(colour);
    }

    /// `OnCurrentColorChanged` - a picker edit recolours the current selection live.
    ///
    /// The whole drag is one undo step, which is what `_colorSession` tracks: without it, dragging
    /// across the colour square would push fifty undo steps and Ctrl+Z would walk back one shade
    /// at a time.
    fn current_colour_changed(&mut self) {
        self.picker.set(self.current_colour);
        let selected: Vec<usize> = self
            .keys
            .iter()
            .enumerate()
            .filter(|(_, key)| key.selected && key.lit())
            .map(|(index, _)| index)
            .collect();
        if selected.is_empty() {
            return;
        }
        if !self.color_session {
            self.push_paint_undo();
            self.color_session = true;
        }
        let hex = to_hex(self.current_colour);
        for index in selected {
            self.colors.insert(format!("key:{}", self.keys[index].key.id), hex.clone());
        }
        self.mark_lighting_edited();
        self.update_fills();
        self.schedule_save();
    }

    /// `AddReactionsAsync(keyIds)` - one row per key that does not have one yet, seeded with the
    /// colour in the picker. A key already in the list is selected instead of duplicated.
    fn add_reactions(&mut self, key_ids: &[&str]) -> bool {
        let mut added = false;
        for key_id in key_ids {
            let Some(key) = layout::by_id(key_id).filter(|k| k.led_id != 0) else { continue };
            if let Some(existing) = self.reactions.iter().position(|r| r.key_id == *key_id) {
                self.selected_reaction = Some(existing);
                continue;
            }
            self.reactions.push(ReactionRow::new(
                (*key_id).to_owned(),
                key_label(key.id, key.label),
                to_hex(self.current_colour),
                KeyReactionMode::Momentary,
                true,
            ));
            self.selected_reaction = Some(self.reactions.len() - 1);
            added = true;
        }
        if added {
            self.update_reaction_overlay();
        }
        added
    }

    /// `ToggleGameKeyAsync(x, y)` - clicking a key on the render toggles whether it is locked out.
    fn toggle_game_key(&mut self, at: egui::Pos2, has_artwork: bool) -> bool {
        let Some(index) = device_stage::key_at(&self.keys, has_artwork, at, false) else {
            return false;
        };
        let usage = self.keys[index].key.hid_usage;
        if usage == 0 {
            return false;
        }
        let Some(row) = self.game_mode_keys.iter_mut().find(|r| r.usage == usage) else {
            return false;
        };
        row.disabled = !row.disabled;
        self.sync_game_states();
        true
    }

    /// `QuickGroupKeys(group)`.
    pub fn quick_group_keys(group: &str) -> Vec<&'static str> {
        match group {
            "WASD" => vec!["W", "A", "S", "D"],
            "NUMBERS" => vec!["D1", "D2", "D3", "D4", "D5", "D6", "D7", "D8", "D9", "D0"],
            "F-KEYS" => layout::keys()
                .iter()
                .filter(|k| k.group == KeyGroup::Function && k.id.starts_with('F'))
                .map(|k| k.id)
                .collect(),
            "G-KEYS" => layout::keys()
                .iter()
                .filter(|k| k.group == KeyGroup::GKey)
                .map(|k| k.id)
                .collect(),
            "MODIFIER" => vec![
                "LeftShift",
                "RightShift",
                "LeftControl",
                "RightControl",
                "LeftAlt",
                "RightAlt",
                "LeftWindows",
                "RightWindows",
                "Menu",
            ],
            "ARROW KEYS" => vec!["Up", "Down", "Left", "Right"],
            "KEYS ONLY" => layout::keys()
                .iter()
                .filter(|k| {
                    matches!(
                        k.group,
                        KeyGroup::Main
                            | KeyGroup::Function
                            | KeyGroup::Navigation
                            | KeyGroup::Numpad
                    )
                })
                .map(|k| k.id)
                .collect(),
            "LOGO" => vec!["Logo"],
            "ALL" => layout::keys().iter().map(|k| k.id).collect(),
            _ => Vec::new(),
        }
    }

    fn select_quick_group(&mut self, group: &str) {
        let ids = Self::quick_group_keys(group);
        self.brush_mode = false;
        let indices: Vec<usize> = self
            .keys
            .iter()
            .enumerate()
            .filter(|(_, key)| key.lit() && ids.contains(&key.key.id))
            .map(|(index, _)| index)
            .collect();
        // As in G HUB: highlight the group and pop the picker up; picking a colour then recolours
        // the whole group.
        self.select_keys(&indices, false);
        self.picker_open = true;
    }
}

// =============================================================== saving

impl KeyboardPage {
    /// `SaveKeyboardSettings()`.
    fn save_keyboard_settings(&self, shared: &mut Shared) {
        let Some(device_key) = self.selected_device.clone() else { return };
        let brightness = self.brightness_percent.clamp(0, 100) as u8;
        let game_mode_enabled = self.game_mode_enabled;
        let disabled: Vec<u8> =
            self.game_mode_keys.iter().filter(|r| r.disabled).map(|r| r.usage).collect();
        let software_gkeys = self.software_gkeys;
        shared.edit_profile("Keyboard settings", move |profile| {
            let keyboard = profile
                .for_device(&device_key)
                .keyboard
                .get_or_insert_with(KeyboardSettings::default);
            keyboard.brightness_percent = Some(brightness);
            keyboard.game_mode_enabled = game_mode_enabled;
            keyboard.game_mode_disabled_usages = disabled;
            keyboard.software_gkeys = software_gkeys;
        });
    }

    /// `SaveLightingNow()`.
    fn save_lighting_now(&mut self, shared: &mut Shared) {
        self.save_due = None;
        let Some(device_key) = self.selected_device.clone() else { return };
        let colors = self.colors.clone();
        let mode = self.lighting_mode;
        let default_color = normalise_hex(&self.default_color);
        let brightness = (self.light_brightness / 100.0).clamp(0.0, 1.0);
        let effect = self.device_effect;
        let period = self.effect_period.clamp(100, 60000) as u16;
        // A snapshot, because the job runs later and the editor may have moved on.
        let power_save = self.power_save.snapshot();
        self.lighting_dirty = false;
        shared.edit_profile("Keyboard lighting", move |profile| {
            let lighting = &mut profile.for_device(&device_key).lighting;
            lighting.mode = mode;
            lighting.default_color = default_color;
            lighting.brightness = brightness;
            lighting.static_colors = colors;
            lighting.device_effect_id = effect;
            lighting.device_effect_period_ms = period;
            power_save.store(lighting);
        });
    }

    /// `SaveReactionsAsync()`.
    fn save_reactions(&mut self, shared: &mut Shared) {
        self.update_reaction_overlay();
        let Some(device_key) = self.selected_device.clone() else { return };
        let reactions: Vec<KeyReaction> = self.reactions.iter().map(ReactionRow::to_model).collect();
        shared.edit_profile("Key reactions", move |profile| {
            profile
                .for_device(&device_key)
                .keyboard
                .get_or_insert_with(KeyboardSettings::default)
                .key_reactions = reactions;
        });
    }

    /// `SaveAnimationAsync()` - writing the animation is what starts it: the service re-applies the
    /// device and the animation engine picks the settings up.
    fn save_animation(&mut self, shared: &mut Shared) {
        let Some(device_key) = self.selected_device.clone() else { return };
        let Some(effect) = self.animation().copied() else { return };
        let settings = AnimationSettings {
            effect: effect.id.to_owned(),
            speed: (self.animation_speed / 100.0).clamp(0.05, 10.0),
            frames_per_second: self.animation_fps.clamp(1, 60) as u32,
            intensity: (self.animation_intensity / 100.0).clamp(0.0, 2.0),
            direction: self.wave_direction,
            colors: self
                .animation_colors
                .iter()
                .take(effect.max_colors)
                .filter(|hex| is_hex_color(hex))
                .cloned()
                .collect(),
        };
        let mode = self.lighting_mode;
        let brightness = (self.light_brightness / 100.0).clamp(0.0, 1.0);
        let what = format!("Animation {}", effect.name);
        shared.edit_profile(what, move |profile| {
            let lighting = &mut profile.for_device(&device_key).lighting;
            lighting.mode = mode;
            lighting.brightness = brightness;
            lighting.animation = settings;
        });
    }

    /// `SaveCustomColors()` - these live on the document, not the profile: a colour the user saved
    /// is theirs, not the active profile's.
    fn save_custom_colors(&self, shared: &mut Shared) {
        let colors = self.custom_colors.clone();
        shared.edit_document("Custom colours", move |document| {
            document.app.custom_colors = colors;
        });
    }
}

// =============================================================== drawing

impl KeyboardPage {
    pub fn show(
        &mut self,
        shared: &mut Shared,
        icons: &mut IconCache,
        ui: &mut Ui,
        changes: Changes,
    ) {
        if changes.devices || !self.loaded {
            self.reload_devices(shared);
        }
        if changes.devices || changes.document || changes.recording || !self.loaded {
            let ctx = ui.ctx().clone();
            self.load(shared, &ctx);
            self.loaded = true;
        }

        // The debounced save. egui draws only on input, so a pending save needs a standing repaint
        // or letting go of the mouse would leave the write until the next time the pointer moved.
        if let Some(due) = self.save_due {
            let now = Instant::now();
            if now >= due {
                self.save_lighting_now(shared);
            } else {
                ui.ctx().request_repaint_after(due - now);
            }
        }

        self.shortcuts(ui);

        let mut pending = Pending::default();
        let busy = shared.busy();

        // `RowDefinitions="88,Auto,*"`.
        if let Some(id) = self.header(ui, icons, shared) {
            super::switch_profile(shared, id);
        }
        ui.add_space(4.0);
        self.status(ui, icons, shared);
        ui.add_space(8.0);

        let body = ui.available_size();
        ui.horizontal_top(|ui| {
            // `ColumnDefinitions="76,*"` - the icon rail.
            ui.allocate_ui_with_layout(
                Vec2::new(76.0, body.y),
                egui::Layout::top_down(egui::Align::Center),
                |ui| self.rail(ui, icons, body.y),
            );

            if self.page == SubPage::DeviceSettings {
                self.device_settings(ui, icons, shared, &mut pending);
                return;
            }

            // `ColumnDefinitions="323,*"` - panel then stage.
            ui.allocate_ui_with_layout(
                Vec2::new(t::PANEL_WIDTH + 2.0 * t::PANEL_MARGIN, body.y),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    w::panel(ui, |ui| match self.page {
                        SubPage::Lightsync => self.lightsync_panel(ui, icons, shared, &mut pending),
                        SubPage::Assignments => {
                            self.assignments_panel(ui, icons, shared, &mut pending)
                        }
                        SubPage::GameMode => self.game_mode_panel(ui, icons, &mut pending),
                        SubPage::Reactions => self.reactions_panel(ui, icons, &mut pending),
                        SubPage::DeviceSettings => {}
                    });
                },
            );

            ui.allocate_ui_with_layout(
                ui.available_size(),
                egui::Layout::top_down(egui::Align::Center),
                |ui| self.stage(ui, icons, shared, &mut pending),
            );
        });

        self.apply(shared, pending, busy);
    }

    /// Ctrl+Z, Ctrl+A and Escape, the three `UserControl.KeyBindings`.
    fn shortcuts(&mut self, ui: &Ui) {
        if !self.is_freestyle() {
            return;
        }
        let (undo, select_all, clear) = ui.input(|i| {
            (
                i.modifiers.command && i.key_pressed(egui::Key::Z),
                i.modifiers.command && i.key_pressed(egui::Key::A),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if undo {
            self.undo_paint();
        }
        if select_all {
            let lit: Vec<usize> = self
                .keys
                .iter()
                .enumerate()
                .filter(|(_, key)| key.lit())
                .map(|(index, _)| index)
                .collect();
            self.select_keys(&lit, false);
        }
        if clear {
            self.select_keys(&[], false);
        }
    }

    fn header(
        &mut self,
        ui: &mut Ui,
        icons: &mut IconCache,
        shared: &Shared,
    ) -> Option<ghub_core::Id> {
        let mut picked = None;
        ui.horizontal(|ui| {
            // The back arrow returns to lighting, which is G HUB's device-screen home.
            if w::icon_button(ui, icons, Icon::ArrowSide, 40.0, 24.0, t::TEXT)
                .on_hover_text("Back to lighting")
                .clicked()
            {
                self.page = SubPage::Lightsync;
            }
            ui.add_space(22.0);
            w::wordmark(ui, &self.device_display_name(shared));

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // The gear: G HUB's asar carries no gear glyph under a CSS class, so the rail's
                // performance icon stands in. Noted rather than silently substituted.
                if w::icon_button(ui, icons, Icon::RailPerformance, 40.0, 26.0, t::TEXT)
                    .on_hover_text("Device settings")
                    .clicked()
                {
                    self.page = SubPage::DeviceSettings;
                }
                ui.add_space(18.0);
                // The profile pill: a click opens the profile list, as G HUB's does.
                picked = super::profile_pill(ui, icons, shared);
            });
        });
        picked
    }

    fn status(&mut self, ui: &mut Ui, icons: &mut IconCache, shared: &Shared) {
        // `Margin="88 0 24 8"` - the band lines up with the panel, not the rail.
        ui.horizontal(|ui| {
            ui.add_space(88.0);
            ui.vertical(|ui| {
                status_band(ui, shared);
                if self.selected_device.is_none() {
                    w::body_small(
                        ui,
                        "No keyboard detected. Connect the G815 by USB.",
                        t::ERROR,
                    );
                }
                // `IsVisible="{Binding HasManyKeyboards}"` - the picker only earns its space when
                // there is something to pick between.
                if self.keyboards.len() > 1 {
                    let keys: Vec<String> =
                        self.keyboards.iter().map(|(key, _)| key.clone()).collect();
                    let labels = self.keyboards.clone();
                    let mut selected =
                        self.selected_device.clone().unwrap_or_else(|| keys[0].clone());
                    ui.set_max_width(320.0);
                    if c::combo(
                        ui,
                        icons,
                        "keyboard device",
                        &mut selected,
                        &keys,
                        |key| {
                            labels
                                .iter()
                                .find(|(candidate, _)| candidate == key)
                                .map_or_else(|| key.clone(), |(_, label)| label.clone())
                            },
                        true,
                    ) {
                        self.selected_device = Some(selected);
                        // `OnSelectedDeviceChanged() => Load()`.
                        self.loaded = false;
                    }
                }
            });
        });
    }

    fn rail(&mut self, ui: &mut Ui, icons: &mut IconCache, height: f32) {
        ui.add_space(0.0);
        let entries = [
            (SubPage::Lightsync, Icon::RailLighting, "LIGHTSYNC"),
            (SubPage::Assignments, Icon::RailAssign, "Assignments"),
            (SubPage::GameMode, Icon::RailDisabledKeys, "Game Mode"),
            (SubPage::Reactions, Icon::RailRapidSwitches, "Press reactions"),
        ];
        for (page, icon, tip) in entries {
            if w::rail(ui, icons, icon, self.page == page, true)
                .on_hover_text(tip)
                .clicked()
            {
                self.page = page;
            }
        }

        // `DockPanel.Dock="Bottom"` - settings sits at the foot of the rail.
        let used = entries.len() as f32 * (t::RAIL_SIZE + t::RAIL_GAP);
        ui.add_space((height - used - t::RAIL_SIZE - 16.0).max(0.0));
        if w::rail(
            ui,
            icons,
            Icon::RailPerformance,
            self.page == SubPage::DeviceSettings,
            true,
        )
        .on_hover_text("Settings")
        .clicked()
        {
            self.page = SubPage::DeviceSettings;
        }
    }
}

// =============================================================== LIGHTSYNC

impl KeyboardPage {
    fn lightsync_panel(
        &mut self,
        ui: &mut Ui,
        icons: &mut IconCache,
        shared: &Shared,
        pending: &mut Pending,
    ) {
        scroll(ui, "keyboard lightsync", |ui| {
            w::title(ui, "LIGHTSYNC");
            ui.add_space(14.0);

            ui.horizontal(|ui| {
                for (tab, caption) in [
                    (LightsyncTab::Presets, "PRESETS"),
                    (LightsyncTab::Freestyle, "FREESTYLE"),
                    (LightsyncTab::Animations, "ANIMATIONS"),
                ] {
                    if w::subtab(ui, caption, self.lightsync_tab == tab).clicked() {
                        self.lightsync_tab = tab;
                    }
                    ui.add_space(t::TAB_GAP);
                }
            });
            ui.add_space(14.0);

            w::label(ui, "EFFECT");
            if c::combo(
                ui,
                icons,
                "lighting mode",
                &mut self.lighting_mode,
                LIGHTING_MODES,
                |mode| format!("{mode:?}"),
                true,
            ) {
                // The mode is one of the settings the Save button exists for, so it is not
                // autosaved - but it does change what the other sections mean, so the panel
                // redraws around it immediately.
                self.lighting_dirty = true;
            }
            ui.add_space(14.0);

            match self.lightsync_tab {
                LightsyncTab::Freestyle => self.freestyle_section(ui, icons, pending),
                LightsyncTab::Presets => self.presets_section(ui, icons, pending),
                LightsyncTab::Animations => self.animations_section(ui, icons, pending),
            }

            // Presets and animations share the brightness control; freestyle's keys carry their
            // own colours and brightness would only mute them.
            if !self.is_freestyle() {
                ui.add_space(14.0);
                w::label(ui, "BRIGHTNESS");
                if c::slider(ui, &mut self.light_brightness, 0.0..=100.0, true) {
                    self.lighting_dirty = true;
                }
            }

            c::separator(ui);
            if self.power_save.show(ui, icons, true) {
                self.lighting_dirty = true;
            }
            if self.power_save.show_read(ui, !shared.busy()) {
                pending.read_power_save = true;
            }

            ui.add_space(10.0);
            if c::button_wide(ui, "Save", c::ButtonStyle::Primary, !shared.busy())
                .on_hover_text(
                    "Key colours save automatically. Use this after changing effect, \
                     unpainted-key colour, brightness or power saving.",
                )
                .clicked()
            {
                pending.save_lighting = true;
            }
        });
    }

    fn freestyle_section(&mut self, ui: &mut Ui, icons: &mut IconCache, pending: &mut Pending) {
        w::label(ui, "SELECTION MODE");
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            // G HUB's freestyle toolbar has no select tool - it always paints - so there is no
            // rail or tool class for one. The arrow glyph is the nearest thing in G HUB's own set
            // and reads as a pointer, which is what the tool does.
            if w::tool(ui, icons, Icon::ArrowSide, !self.brush_mode)
                .on_hover_text(
                    "Select: click or drag to select keys (they take the colour); Ctrl adds or \
                     removes.",
                )
                .clicked()
            {
                self.brush_mode = false;
                self.eyedropper = false;
            }
            ui.add_space(6.0);
            if w::tool(ui, icons, Icon::Brush, self.brush_mode)
                .on_hover_text("Brush: click or drag across keys to paint them")
                .clicked()
            {
                self.brush_mode = true;
                self.eyedropper = false;
                // `SetBrushMode` clears the selection: the brush paints what it touches, and a
                // leftover selection would make the next picker move repaint keys out of view.
                self.select_keys(&[], false);
            }
            ui.add_space(6.0);
            if w::tool(ui, icons, Icon::Eyedropper, self.eyedropper)
                .on_hover_text("Eyedropper: take the colour of a key")
                .clicked()
            {
                self.eyedropper = !self.eyedropper;
            }
        });
        ui.add_space(14.0);

        ui.horizontal(|ui| {
            w::label(ui, "COLOR");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if w::swatch(ui, self.current_colour, 24.0, self.picker_open)
                    .on_hover_text("Show or hide the colour picker")
                    .clicked()
                {
                    self.picker_open = !self.picker_open;
                }
            });
        });

        // Inline, exactly as G HUB: expanding pushes the rest of the panel down rather than
        // floating over it.
        if self.picker_open {
            ui.add_space(12.0);
            if self.picker.show(ui, icons, &mut self.current_colour) {
                self.current_colour_changed();
            }
            self.eyedropper |= self.picker.eyedropper;
            self.picker.eyedropper = false;

            ui.add_space(12.0);
            let names: Vec<String> =
                PALETTES.iter().map(|(name, _)| (*name).to_owned()).collect();
            c::combo(
                ui,
                icons,
                "palette",
                &mut self.palette_name,
                &names,
                |name| name.clone(),
                true,
            );

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                for hex in *self.palette() {
                    let colour = parse_hex(hex).unwrap_or(Color32::GRAY);
                    if w::swatch(ui, colour, 24.0, false).on_hover_text(hex).clicked() {
                        self.current_colour = colour;
                        self.current_colour_changed();
                    }
                    ui.add_space(9.0);
                }
            });

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                w::label(ui, "CUSTOM");
                ui.add_space(6.0);
                w::help(
                    ui,
                    icons,
                    "+ saves the current colour. Right-click a custom colour to remove it.",
                );
            });
            ui.add_space(6.0);
            self.custom_swatches(ui, icons, pending);
        }

        c::separator(ui);
        w::label(ui, "COLORS ON DEVICE");
        ui.add_space(6.0);
        // Picking a colour on the device selects its keys and loads the colour, so editing it
        // recolours them all at once.
        let mut picked = None;
        for row in &self.colors_on_device {
            let response = w::row(ui, &row.text());
            let swatch = egui::Rect::from_center_size(
                egui::pos2(response.rect.right() - 13.0, response.rect.center().y),
                Vec2::splat(22.0),
            );
            ui.painter().circle_filled(
                swatch.center(),
                11.0,
                parse_hex(&row.hex).unwrap_or(Color32::GRAY),
            );
            if response
                .on_hover_text(format!("{}: select these keys and edit their colour", row.hex))
                .clicked()
            {
                picked = Some(row.hex.clone());
            }
        }
        if let Some(hex) = picked {
            self.brush_mode = false;
            if let Some(colour) = parse_hex(&hex) {
                self.set_colour_quietly(colour);
            }
            let indices: Vec<usize> = (0..self.keys.len())
                .filter(|index| self.keys[*index].lit() && self.color_of(*index) == hex)
                .collect();
            self.select_keys(&indices, false);
            self.picker_open = true;
        }
    }

    fn custom_swatches(&mut self, ui: &mut Ui, icons: &mut IconCache, pending: &mut Pending) {
        let mut use_colour = None;
        let mut remove = None;
        let mut add = false;
        ui.horizontal_wrapped(|ui| {
            for hex in &self.custom_colors {
                let colour = parse_hex(hex).unwrap_or(Color32::GRAY);
                let response = w::swatch(ui, colour, 24.0, false).on_hover_text(format!(
                    "{hex} (right-click to remove)"
                ));
                if response.clicked() {
                    use_colour = Some(colour);
                }
                // Right-click removes, as `CustomSwatchReleased` did.
                if response.secondary_clicked() {
                    remove = Some(hex.clone());
                }
                ui.add_space(9.0);
            }
            if w::icon_button(ui, icons, Icon::Add, 26.0, 26.0, t::TEXT)
                .on_hover_text("Save the current colour")
                .clicked()
            {
                add = true;
            }
        });

        if let Some(colour) = use_colour {
            self.current_colour = colour;
            self.current_colour_changed();
        }
        if let Some(hex) = remove {
            self.custom_colors.retain(|candidate| *candidate != hex);
            pending.save_custom_colors = true;
        }
        if add {
            let hex = to_hex(self.current_colour);
            if !self.custom_colors.contains(&hex) {
                self.custom_colors.push(hex);
                pending.save_custom_colors = true;
            }
        }
    }

    fn presets_section(&mut self, ui: &mut Ui, icons: &mut IconCache, pending: &mut Pending) {
        w::body_small(
            ui,
            "These run on the keyboard itself, so they keep going when this app is closed. Set \
             the effect to \"DeviceEffect\" above to use one.",
            t::TEXT_LABEL,
        );
        ui.add_space(14.0);

        w::label(ui, "FIRMWARE EFFECT");
        if self.device_effects.is_empty() {
            w::body_small(ui, "This keyboard reports no firmware effects.", t::TEXT_LABEL);
        } else {
            let ids: Vec<u16> = self.device_effects.iter().map(|(_, id)| *id).collect();
            let names = self.device_effects.clone();
            let mut selected = self.device_effect.unwrap_or(ids[0]);
            if c::combo(
                ui,
                icons,
                "firmware effect",
                &mut selected,
                &ids,
                |id| {
                    names
                        .iter()
                        .find(|(_, candidate)| candidate == id)
                        .map_or_else(|| format!("Effect 0x{id:04X}"), |(name, _)| name.clone())
                },
                true,
            ) {
                self.device_effect = Some(selected);
                // `OnDeviceEffectChanged`: only writes when the device effect is what is running,
                // so browsing the list does not take over the keyboard.
                if self.lighting_mode == LightingMode::DeviceEffect {
                    pending.save_lighting = true;
                }
            } else {
                self.device_effect = Some(selected);
            }
        }

        ui.add_space(14.0);
        c::field_row(ui, "Cycle over ms", |ui| {
            if c::numeric(
                ui,
                icons,
                &mut self.effect_period_buffer,
                &mut self.effect_period,
                100..=60000,
                500,
                true,
            ) {
                self.lighting_dirty = true;
            }
        });

        ui.add_space(14.0);
        w::label(ui, "EFFECT COLOR");
        if c::text_field(ui, &mut self.default_color_buffer, ui.available_width(), true) {
            self.default_color = normalise_hex(&self.default_color_buffer);
            self.default_color_buffer = self.default_color.clone();
            // `OnDefaultColorChanged() => UpdateFills()`: the unpainted keys follow it.
            self.update_fills();
            self.lighting_dirty = true;
        }
    }

    fn animations_section(&mut self, ui: &mut Ui, icons: &mut IconCache, pending: &mut Pending) {
        w::body_small(ui, &self.animation_summary(), t::TEXT_LABEL);
        ui.add_space(14.0);

        let mut play = None;
        // Grouped the way G HUB groups them: its software effects, its five keyframe
        // ANIMATIONS, then this application's own.
        const G_HUB_EFFECTS: &[&str] =
            &["wave", "breathing", "cycle", "ripple", "starlight", "echo", "screen", "audio"];
        let group_of = |e: &KeyEffectDefinition| {
            if G_HUB_EFFECTS.contains(&e.id) {
                0
            } else if e.is_keyframe() {
                1
            } else {
                2
            }
        };
        for (group, caption) in ["G HUB EFFECTS", "G HUB ANIMATIONS", "MORE EFFECTS"].into_iter().enumerate() {
            let belongs = |e: &KeyEffectDefinition| group_of(e) == group;
            w::label(ui, caption);
            ui.add_space(6.0);
            for (index, effect) in effects::CATALOG.iter().enumerate().filter(|(_, e)| belongs(e)) {
                let active = index == self.selected_animation && self.animation_running();
                let response = w::anim_row(ui, effect.name, active);
                w::body_small(ui, effect.description, t::TEXT_LABEL);
                ui.add_space(6.0);
                if response.on_hover_text(effect.description).clicked() {
                    play = Some(index);
                }
            }
            ui.add_space(8.0);
        }
        if let Some(index) = play {
            self.selected_animation = index;
            self.lighting_mode = LightingMode::Animated;
            pending.save_animation = true;
        }

        c::separator(ui);
        w::label(ui, "SPEED");
        if c::slider(ui, &mut self.animation_speed, 5.0..=400.0, true) {
            pending.save_animation = true;
        }

        if self.animation().is_some_and(|e| e.uses_direction) {
            ui.add_space(10.0);
            w::label(ui, "DIRECTION");
            if c::combo(
                ui,
                icons,
                "wave direction",
                &mut self.wave_direction,
                ANIMATION_DIRECTIONS,
                |direction| format!("{direction:?}"),
                true,
            ) {
                pending.save_animation = true;
            }
        }

        if self.animation().is_some_and(|e| e.reactive) {
            ui.add_space(10.0);
            w::label(ui, "PRESS STRENGTH");
            if c::slider(ui, &mut self.animation_intensity, 0.0..=200.0, true) {
                pending.save_animation = true;
            }
        }

        if self.max_animation_colors() > 0 {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                w::label(ui, "COLORS");
                ui.add_space(6.0);
                w::help(
                    ui,
                    icons,
                    "No colours means the effect paints the full spectrum. + adds the colour in \
                     the picker above.",
                );
            });
            ui.add_space(6.0);

            let mut remove = None;
            let mut add = false;
            ui.horizontal_wrapped(|ui| {
                for hex in &self.animation_colors {
                    let colour = parse_hex(hex).unwrap_or(Color32::GRAY);
                    if w::swatch(ui, colour, 24.0, false)
                        .on_hover_text("Remove this colour")
                        .clicked()
                    {
                        remove = Some(hex.clone());
                    }
                    ui.add_space(9.0);
                }
                if w::icon_button(ui, icons, Icon::Add, 26.0, 26.0, t::TEXT)
                    .on_hover_text("Add the picker's colour to the animation")
                    .clicked()
                {
                    add = true;
                }
            });
            if let Some(hex) = remove {
                self.animation_colors.retain(|candidate| *candidate != hex);
                pending.save_animation = true;
            }
            if add && self.animation_colors.len() < self.max_animation_colors() {
                self.animation_colors.push(to_hex(self.current_colour));
                pending.save_animation = true;
            }
        }

        ui.add_space(10.0);
        c::field_row(ui, "Frames per second", |ui| {
            if c::numeric(
                ui,
                icons,
                &mut self.animation_fps_buffer,
                &mut self.animation_fps,
                1..=60,
                1,
                true,
            ) {
                pending.save_animation = true;
            }
        });
        w::body_small(
            ui,
            "Lower this if the animation stutters: every frame is sent over the keyboard's own \
             link.",
            t::TEXT_LABEL,
        );

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if self.animation_running()
                && c::button(ui, "Stop", c::ButtonStyle::Outlined, true)
                    .on_hover_text("Go back to the static colours on FREESTYLE")
                    .clicked()
            {
                self.lighting_mode = LightingMode::Static;
                pending.save_lighting = true;
            }
            ui.add_space(8.0);
            if c::button(ui, "Save", c::ButtonStyle::Outlined, true).clicked() {
                pending.save_animation = true;
            }
        });
    }
}

// =============================================================== Assignments

impl KeyboardPage {
    fn assignments_panel(
        &mut self,
        ui: &mut Ui,
        icons: &mut IconCache,
        shared: &Shared,
        pending: &mut Pending,
    ) {
        scroll(ui, "keyboard assignments", |ui| {
            w::title(ui, "Assignments");
            ui.add_space(12.0);
            w::body_small(ui, &self.gkey_info(shared), t::TEXT_LABEL);
            ui.add_space(12.0);

            w::label(ui, "BANK");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                for bank in 1..=3usize {
                    let style = if bank == self.active_bank {
                        c::ButtonStyle::Primary
                    } else {
                        c::ButtonStyle::Outlined
                    };
                    if c::button(ui, &format!("M{bank}"), style, !shared.busy()).clicked() {
                        pending.select_bank = Some(bank);
                    }
                    ui.add_space(6.0);
                }
            });
            ui.add_space(10.0);
            if c::checkbox(ui, icons, &mut self.show_shift_layer, "Show G-Shift layer", true) {
                // `OnShowShiftLayerChanged() => LoadSlots()`.
                self.load_slots(shared);
            }

            ui.add_space(12.0);
            w::label(ui, "POSITIONS");
            ui.add_space(6.0);
            let mut edit = None;
            for (index, row) in self.slots.iter().enumerate() {
                let response = w::anim_row(ui, &row.label(), Some(index) == self.selected_slot);
                w::body_small(ui, &row.description, t::TEXT_LABEL);
                ui.add_space(4.0);
                if response.on_hover_text("Click to edit this position").clicked() {
                    edit = Some(index);
                }
            }
            if let Some(index) = edit {
                // `EditSlot`: selecting a position loads whatever is bound there into the editor,
                // so Apply either keeps it or replaces it rather than starting from blank.
                self.selected_slot = Some(index);
                let slot = self.slots[index].slot();
                let action = self
                    .selected_device
                    .as_deref()
                    .and_then(|key| shared.active_profile().device(key).cloned())
                    .and_then(|settings| settings.bindings.get(&slot).map(|b| b.action.clone()));
                self.editor.load(action.as_ref());
            }

            c::separator(ui);
            let caption = self
                .selected_slot
                .and_then(|index| self.slots.get(index))
                .map_or_else(|| "SELECT A POSITION".to_owned(), SlotRow::label);
            w::label(ui, &caption);
            ui.add_space(6.0);
            self.editor.show(ui, icons, true);

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let has_slot = self.selected_slot.is_some();
                if c::button(ui, "Apply", c::ButtonStyle::Primary, has_slot && !shared.busy())
                    .clicked()
                {
                    pending.apply_slot = true;
                }
                ui.add_space(6.0);
                if c::button(ui, "Clear", c::ButtonStyle::Outlined, has_slot && !shared.busy())
                    .clicked()
                {
                    pending.clear_slot = true;
                }
            });

            c::separator(ui);
            w::label(ui, "ON-THE-FLY MACRO (MR)");
            ui.add_space(6.0);
            w::body(ui, &self.record_state(shared));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                c::numeric(
                    ui,
                    icons,
                    &mut self.record_gkey_buffer,
                    &mut self.record_gkey,
                    1..=5,
                    1,
                    true,
                );
                ui.add_space(6.0);
                let recording = shared.record_state() != MacroRecordState::Idle;
                if c::button(ui, "Start", c::ButtonStyle::Outlined, !recording && !shared.busy())
                    .clicked()
                {
                    pending.start_record = true;
                }
                ui.add_space(6.0);
                if c::button(ui, "Stop", c::ButtonStyle::Outlined, recording).clicked() {
                    pending.stop_record = true;
                }
                ui.add_space(6.0);
                if c::button(ui, "Cancel", c::ButtonStyle::Text, recording).clicked() {
                    pending.cancel_record = true;
                }
            });

            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                for bank in 1..=3usize {
                    let caption = if bank == 1 {
                        format!("Copy bank → M{bank}")
                    } else {
                        format!("→ M{bank}")
                    };
                    if c::button(ui, &caption, c::ButtonStyle::Text, !shared.busy()).clicked() {
                        pending.copy_bank_to = Some(bank);
                    }
                    ui.add_space(6.0);
                }
                if c::button(ui, "Reset", c::ButtonStyle::Text, !shared.busy()).clicked() {
                    pending.reset_bank = true;
                }
            });
        });
    }
}

// =============================================================== Game Mode

impl KeyboardPage {
    fn game_mode_panel(&mut self, ui: &mut Ui, icons: &mut IconCache, pending: &mut Pending) {
        scroll(ui, "keyboard game mode", |ui| {
            w::title(ui, "Game Mode");
            ui.add_space(16.0);
            w::body(
                ui,
                "Select the keys you don't want to accidentally hit while gaming. Enabling Game \
                 Mode on your keyboard will disable these keys.",
            );
            ui.add_space(16.0);

            // The three-state legend, in G HUB's own order.
            for (fill, caption) in [
                (LegendFill::Hatch, "Keys disabled by default"),
                (LegendFill::Accent, "Keys disabled by you"),
                (LegendFill::Outline, "Keys cannot be modified"),
            ] {
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(Vec2::splat(16.0), egui::Sense::hover());
                    match fill {
                        LegendFill::Hatch => w::hatch(ui, rect),
                        LegendFill::Accent => {
                            ui.painter().rect_filled(
                                rect,
                                egui::CornerRadius::same(t::RADIUS_ROW),
                                t::ACCENT,
                            );
                        }
                        LegendFill::Outline => {
                            ui.painter().rect_stroke(
                                rect,
                                egui::CornerRadius::same(t::RADIUS_ROW),
                                egui::Stroke::new(1.0, Color32::from_rgb(0x8A, 0x8D, 0x93)),
                                egui::StrokeKind::Inside,
                            );
                        }
                    }
                    ui.add_space(10.0);
                    w::body(ui, caption);
                });
                ui.add_space(10.0);
            }

            ui.add_space(6.0);
            if c::button_wide(ui, "RESTORE DEFAULT SETTINGS", c::ButtonStyle::Outlined, true)
                .clicked()
            {
                for row in &mut self.game_mode_keys {
                    row.disabled = DEFAULT_DISABLED_USAGES.contains(&row.usage);
                }
                self.sync_game_states();
                pending.save_keyboard = true;
            }

            c::separator(ui);
            if c::checkbox(
                ui,
                icons,
                &mut self.game_mode_enabled,
                "Game mode on (selected keys disabled)",
                true,
            ) {
                pending.save_keyboard = true;
            }
            ui.add_space(8.0);
            if c::checkbox(
                ui,
                icons,
                &mut self.software_gkeys,
                "G-keys under software control",
                true,
            ) {
                pending.save_keyboard = true;
            }
            ui.add_space(4.0);
            w::body_small(ui, "Off: G1-G5 send their default keystrokes", t::TEXT_LABEL);

            ui.add_space(12.0);
            c::field_row(ui, "Brightness %", |ui| {
                if c::numeric(
                    ui,
                    icons,
                    &mut self.brightness_buffer,
                    &mut self.brightness_percent,
                    0..=100,
                    5,
                    true,
                ) {
                    pending.save_keyboard = true;
                }
            });

            ui.add_space(12.0);
            if c::button_wide(ui, "Save keyboard settings", c::ButtonStyle::Primary, true).clicked()
            {
                pending.save_keyboard = true;
            }
            ui.add_space(10.0);
            w::body_small(
                ui,
                "Disabled keys are sent to the keyboard (HID++ 0x4522) and take effect whenever \
                 its own Game-mode key is engaged.",
                t::TEXT_LABEL,
            );
        });
    }
}

enum LegendFill {
    Hatch,
    Accent,
    Outline,
}

// =============================================================== Press Reactions

impl KeyboardPage {
    fn reactions_panel(&mut self, ui: &mut Ui, icons: &mut IconCache, pending: &mut Pending) {
        scroll(ui, "keyboard reactions", |ui| {
            w::title(ui, "Press Reactions");
            ui.add_space(14.0);
            w::body(
                ui,
                "Click a key on the keyboard to give it a press colour. Each key is configured on \
                 its own and keeps its lighting colour until it is pressed.",
            );
            ui.add_space(14.0);

            if self.reactions_need_lighting() {
                w::callout_warning(ui, |ui| {
                    w::body_small(
                        ui,
                        "The keyboard's lighting is not being written by this app, so reactions \
                         have nothing to draw over and nothing will change when you press a key.",
                        t::TEXT,
                    );
                    ui.add_space(8.0);
                    if c::button(ui, "Use FREESTYLE colours", c::ButtonStyle::Outlined, true)
                        .clicked()
                    {
                        self.lighting_mode = LightingMode::Static;
                        pending.save_lighting = true;
                    }
                });
                ui.add_space(14.0);
            }

            w::label(ui, "REACTION COLOR");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                w::body(ui, "Used for keys you add");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if w::swatch(ui, self.current_colour, 24.0, self.picker_open)
                        .on_hover_text("Show or hide the colour picker")
                        .clicked()
                    {
                        self.picker_open = !self.picker_open;
                    }
                });
            });

            if self.picker_open {
                ui.add_space(12.0);
                if self.picker.show(ui, icons, &mut self.current_colour) {
                    // No live recolouring here: on this page the picker seeds *new* rows, and
                    // repainting the lighting selection from it would be a surprise.
                    self.picker.set(self.current_colour);
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    for hex in *self.palette() {
                        let colour = parse_hex(hex).unwrap_or(Color32::GRAY);
                        if w::swatch(ui, colour, 24.0, false).on_hover_text(hex).clicked() {
                            self.set_colour_quietly(colour);
                        }
                        ui.add_space(9.0);
                    }
                });
            }

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if c::button(ui, "Add selected keys", c::ButtonStyle::Primary, true)
                    .on_hover_text("Adds a reaction for every key selected on the LIGHTSYNC page")
                    .clicked()
                {
                    let ids: Vec<&'static str> = self
                        .keys
                        .iter()
                        .filter(|key| key.selected && key.lit())
                        .map(|key| key.key.id)
                        .collect();
                    if self.add_reactions(&ids) {
                        pending.save_reactions = true;
                    }
                }
                ui.add_space(8.0);
                if c::button(ui, "Remove all", c::ButtonStyle::Outlined, !self.reactions.is_empty())
                    .clicked()
                {
                    self.reactions.clear();
                    self.selected_reaction = None;
                    pending.save_reactions = true;
                }
            });

            c::separator(ui);
            w::label(ui, "CONFIGURED KEYS");
            ui.add_space(6.0);
            if self.reactions.is_empty() {
                w::body_small(
                    ui,
                    "No keys configured. Click any key on the keyboard to add one.",
                    t::TEXT_LABEL,
                );
            }

            let mut remove = None;
            let mut edited = false;
            for index in 0..self.reactions.len() {
                w::card(ui, |ui| {
                    ui.horizontal(|ui| {
                        let colour = self.reactions[index].colour();
                        if w::swatch(ui, colour, 20.0, self.reactions[index].picker_open)
                            .on_hover_text("Pick this key's press colour")
                            .clicked()
                        {
                            self.reactions[index].picker_open =
                                !self.reactions[index].picker_open;
                            self.selected_reaction = Some(index);
                        }
                        ui.add_space(10.0);
                        w::body(ui, &self.reactions[index].label.clone());
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if w::icon_button(ui, icons, Icon::Close, 20.0, 20.0, t::TEXT_LABEL)
                                    .on_hover_text("Remove this reaction")
                                    .clicked()
                                {
                                    remove = Some(index);
                                }
                                ui.add_space(6.0);
                                let mut enabled = self.reactions[index].enabled;
                                if c::checkbox(ui, icons, &mut enabled, "", true) {
                                    self.reactions[index].enabled = enabled;
                                    edited = true;
                                }
                            },
                        );
                    });

                    ui.add_space(7.0);
                    ui.horizontal(|ui| {
                        w::label(ui, "COLOR");
                        ui.add_space(10.0);
                        if c::text_field(
                            ui,
                            &mut self.reactions[index].hex_buffer,
                            ui.available_width(),
                            true,
                        ) {
                            let typed = self.reactions[index].hex_buffer.clone();
                            match parse_hex(&typed) {
                                Some(colour) => {
                                    self.reactions[index].hex = to_hex(colour);
                                    self.reactions[index].hex_buffer =
                                        self.reactions[index].hex.clone();
                                    self.reactions[index].picker.set(colour);
                                    edited = true;
                                }
                                // Unparseable reverts, as the C# picker's `UpdateText` did: a
                                // half-typed colour must not become black.
                                None => {
                                    self.reactions[index].hex_buffer =
                                        self.reactions[index].hex.clone();
                                }
                            }
                        }
                    });

                    if self.reactions[index].picker_open {
                        ui.add_space(10.0);
                        let mut colour = self.reactions[index].colour();
                        // Moved out and back because the picker and the row's hex both live on
                        // the same element of the list, and the picker needs the whole row's slot.
                        let mut picker = std::mem::replace(
                            &mut self.reactions[index].picker,
                            ColorPicker::new(colour),
                        );
                        if picker.show(ui, icons, &mut colour) {
                            self.reactions[index].hex = to_hex(colour);
                            self.reactions[index].hex_buffer =
                                self.reactions[index].hex.clone();
                            edited = true;
                        }
                        self.reactions[index].picker = picker;

                        ui.add_space(10.0);
                        ui.horizontal_wrapped(|ui| {
                            for hex in &PALETTES[0].1 {
                                let swatch = parse_hex(hex).unwrap_or(Color32::GRAY);
                                if w::swatch(ui, swatch, 22.0, false)
                                    .on_hover_text(*hex)
                                    .clicked()
                                {
                                    self.reactions[index].hex = (*hex).to_owned();
                                    self.reactions[index].hex_buffer = (*hex).to_owned();
                                    self.reactions[index].picker.set(swatch);
                                    edited = true;
                                }
                                ui.add_space(8.0);
                            }
                        });
                    }

                    ui.add_space(7.0);
                    ui.horizontal(|ui| {
                        w::label(ui, "GOES BACK");
                        ui.add_space(10.0);
                        let modes: Vec<KeyReactionMode> =
                            REACTION_MODES.iter().map(|(mode, _)| *mode).collect();
                        let mut mode = self.reactions[index].mode;
                        if c::combo(
                            ui,
                            icons,
                            ("reaction mode", index),
                            &mut mode,
                            &modes,
                            |mode| {
                                REACTION_MODES
                                    .iter()
                                    .find(|(candidate, _)| candidate == mode)
                                    .map_or_else(String::new, |(_, label)| (*label).to_owned())
                            },
                            true,
                        ) {
                            self.reactions[index].mode = mode;
                            edited = true;
                        }
                    })
                    .response
                    .on_hover_text(
                        "While held: the colour lasts as long as the key is down. Until pressed \
                         again: the colour stays until the next press.",
                    );
                });
                ui.add_space(8.0);
            }

            if let Some(index) = remove {
                self.reactions.remove(index);
                self.selected_reaction = if self.reactions.is_empty() { None } else { Some(0) };
                pending.save_reactions = true;
            }
            if edited {
                self.update_reaction_overlay();
                pending.save_reactions = true;
            }

            ui.add_space(10.0);
            if c::button_wide(ui, "Save reactions", c::ButtonStyle::Primary, true)
                .on_hover_text(
                    "Edits save as you make them; this writes them again and re-applies them to \
                     the keyboard.",
                )
                .clicked()
            {
                pending.save_reactions = true;
            }
            ui.add_space(10.0);
            w::body_small(
                ui,
                "Reactions are drawn over the LIGHTSYNC lighting, so the keyboard must be on an \
                 effect this app writes: a FREESTYLE colour layout or an ANIMATION. They do \
                 nothing on a firmware preset or while the lighting is left untouched.",
                t::TEXT_LABEL,
            );
        });
    }
}

// =============================================================== the stage

impl KeyboardPage {
    fn stage(
        &mut self,
        ui: &mut Ui,
        icons: &mut IconCache,
        shared: &mut Shared,
        pending: &mut Pending,
    ) {
        let overlay = match self.page {
            SubPage::GameMode => Overlay::GameMode,
            SubPage::Reactions => Overlay::Reactions,
            _ => Overlay::Lighting { editable: self.is_freestyle() },
        };

        w::stage(ui, |ui| {
            // `FontSize="20"`, centred over the keyboard.
            w::title(ui, &self.selection_info());
            ui.add_space(12.0);

            let ctx = ui.ctx().clone();
            let artwork = shared.artwork.get(&ctx, self.artwork_folder);
            let has_artwork = artwork.is_some();
            let action = device_stage::show(
                ui,
                icons,
                &mut self.stage,
                &self.keys,
                artwork,
                overlay,
                self.brush_mode,
                self.eyedropper,
            );
            self.handle_stage(action, has_artwork, overlay, pending);

            ui.add_space(12.0);
            match self.page {
                SubPage::Lightsync if self.is_freestyle() => self.quick_groups(ui),
                SubPage::Assignments => self.bank_pills(ui, shared, pending),
                _ => {}
            }
        });
    }

    fn handle_stage(
        &mut self,
        action: StageAction,
        has_artwork: bool,
        overlay: Overlay,
        pending: &mut Pending,
    ) {
        if !action.any() {
            return;
        }
        match overlay {
            Overlay::GameMode => {
                if let Some((at, _)) = action.clicked {
                    if self.toggle_game_key(at, has_artwork) {
                        pending.save_keyboard = true;
                    }
                }
            }
            Overlay::Reactions => {
                if let Some((at, _)) = action.clicked {
                    if let Some(index) = device_stage::key_at(&self.keys, has_artwork, at, true) {
                        let id = self.keys[index].key.id;
                        if self.add_reactions(&[id]) {
                            pending.save_reactions = true;
                        }
                    }
                }
            }
            Overlay::Lighting { .. } => {
                if let Some(at) = action.sampled {
                    self.sample_at(at, has_artwork);
                }
                if action.stroke_began {
                    self.push_paint_undo();
                    self.color_session = false;
                }
                if let Some(at) = action.painted {
                    self.brush_at(at, has_artwork);
                }
                if let Some((at, additive)) = action.clicked {
                    self.click_at(at, additive, has_artwork);
                }
                if let Some((rect, additive)) = action.marquee {
                    let mut hits = device_stage::keys_in_rect(&self.keys, has_artwork, rect);
                    if additive {
                        for (index, key) in self.keys.iter().enumerate() {
                            if key.selected && !hits.contains(&index) {
                                hits.push(index);
                            }
                        }
                    }
                    self.select_keys(&hits, true);
                }
            }
        }
    }

    /// The quick groups under the keyboard, exactly where G HUB places them.
    fn quick_groups(&mut self, ui: &mut Ui) {
        w::label(ui, "QUICK GROUP");
        ui.add_space(6.0);
        let mut group = None;
        ui.horizontal_wrapped(|ui| {
            // "ALL" is omitted: it is Ctrl+A, as in the C#, where `QuickGroups` filtered it out.
            for name in QUICK_GROUP_NAMES.iter().filter(|name| **name != "ALL") {
                if w::link(ui, name, false).clicked() {
                    group = Some(*name);
                }
            }
        });
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if w::link(ui, "CLEAR SELECTION", true).on_hover_text("Esc").clicked() {
                self.select_keys(&[], false);
            }
            if w::link(ui, "UNDO", true).on_hover_text("Ctrl+Z").clicked() {
                self.undo_paint();
            }
        });
        if let Some(name) = group {
            self.select_quick_group(name);
        }
    }

    /// The M-bank pills and the G-Shift switch, as on G HUB's Assignments page.
    fn bank_pills(&mut self, ui: &mut Ui, shared: &Shared, pending: &mut Pending) {
        ui.horizontal(|ui| {
            for bank in 1..=3usize {
                if w::link(ui, &format!("M{bank}"), bank != self.active_bank).clicked() {
                    pending.select_bank = Some(bank);
                }
                ui.add_space(10.0);
            }
        });
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            w::label(ui, "DEFAULT");
            ui.add_space(10.0);
            if c::toggle_switch(ui, &mut self.show_shift_layer, true) {
                self.load_slots(shared);
            }
            ui.add_space(10.0);
            w::label(ui, "G-SHIFT");
        });
    }
}

// =============================================================== Device Settings

impl KeyboardPage {
    fn device_settings(
        &mut self,
        ui: &mut Ui,
        icons: &mut IconCache,
        shared: &mut Shared,
        pending: &mut Pending,
    ) {
        scroll(ui, "keyboard device settings", |ui| {
            // `MaxWidth="760"`, centred.
            ui.vertical_centered(|ui| {
                ui.set_max_width(760.0);
                w::wordmark(ui, &self.device_display_name(shared));
                ui.add_space(18.0);

                let ctx = ui.ctx().clone();
                if let Some(artwork) = shared.artwork.get(&ctx, self.artwork_folder) {
                    // `Width="440" Opacity="0.75"`.
                    let size = Vec2::new(440.0, 440.0 * artwork.height() / artwork.width());
                    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                    ui.painter().image(
                        artwork.texture.id(),
                        rect,
                        uv,
                        Color32::WHITE.gamma_multiply(0.75),
                    );
                }

                ui.add_space(18.0);
                w::label(ui, "FIRMWARE VERSION");
                ui.add_space(4.0);
                // Displayed, never written: there is no firmware update path in this application,
                // by design.
                w::body(ui, &self.firmware_version(shared));

                ui.add_space(18.0);
                c::separator(ui);

                ui.horizontal(|ui| {
                    w::label(ui, "ON-BOARD MEMORY MODE");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // `OnboardMemoryMode` is the inverse of `SoftwareGKeys`: handing the
                        // keyboard back to its own memory is the same thing as taking the G-keys
                        // out of software control.
                        let mut onboard = !self.software_gkeys;
                        if c::toggle_switch_labelled(ui, &mut onboard, "OFF", "ON", true) {
                            self.software_gkeys = !onboard;
                            pending.save_keyboard = true;
                        }
                    });
                });
                ui.add_space(4.0);
                w::body_small(
                    ui,
                    "On-board mode runs the keyboard from its own memory; software control is \
                     then off.",
                    t::TEXT_LABEL,
                );

                ui.add_space(18.0);
                w::label(ui, "PROFILE SLOTS");
                ui.add_space(8.0);
                for bank in 1..=3usize {
                    ui.horizontal(|ui| {
                        ui.allocate_ui(Vec2::new(90.0, 20.0), |ui| {
                            w::body(ui, &format!("M{bank} SLOT"));
                        });
                        ui.add_space(10.0);
                        if w::link(ui, &format!("PROFILE {bank}"), bank != self.active_bank)
                            .clicked()
                        {
                            pending.select_bank = Some(bank);
                        }
                    });
                    ui.add_space(6.0);
                }
                if w::link(ui, "RESTORE ALL PROFILE SLOTS TO DEFAULT", true).clicked() {
                    pending.reset_bank = true;
                }

                ui.add_space(18.0);
                c::separator(ui);
                w::body_small(
                    ui,
                    "This application drives the keyboard over HID++ directly; G HUB does not \
                     need to be installed or running.",
                    t::TEXT_LABEL,
                );
                let _ = icons;
            });
        });
    }
}

// =============================================================== applying

impl KeyboardPage {
    fn apply(&mut self, shared: &mut Shared, pending: Pending, busy: bool) {
        if pending.save_lighting {
            self.save_lighting_now(shared);
        }
        if pending.save_keyboard {
            self.save_keyboard_settings(shared);
        }
        if pending.save_reactions {
            self.save_reactions(shared);
        }
        if pending.save_animation {
            self.save_animation(shared);
        }
        if pending.save_custom_colors {
            self.save_custom_colors(shared);
        }

        let Some(device_key) = self.selected_device.clone() else { return };

        if pending.read_power_save && !busy {
            let manager = std::sync::Arc::clone(&shared.host.devices);
            let key = device_key.clone();
            let state = self.power_save.state_cell();
            shared.attempt("Read lighting power timers", move || {
                power_save::read_device(&manager, &key, &state)
            });
        }

        if let Some(bank) = pending.select_bank {
            let service = std::sync::Arc::clone(shared.service());
            let key = device_key.clone();
            self.active_bank = bank;
            shared.attempt("Bank", move || {
                service.set_bank(&key, bank);
                Ok(format!("Active bank M{bank}"))
            });
        }

        if pending.apply_slot {
            if let Some(row) = self.selected_slot.and_then(|index| self.slots.get(index)).cloned() {
                if let Some(action) = self.editor.build() {
                    let what = format!("{} → {}", row.label(), action.describe());
                    let slot = row.slot();
                    let key = device_key.clone();
                    shared.edit_profile(what, move |profile| {
                        profile
                            .for_device(&key)
                            .bindings
                            .insert(slot, ghub_core::action::ActionBinding::new(action));
                    });
                } else if let Some(error) = self.editor.error.clone() {
                    // The editor's own validation message, surfaced where the user is looking.
                    shared.error = Some(error);
                }
            }
        }

        if pending.clear_slot {
            if let Some(row) = self.selected_slot.and_then(|index| self.slots.get(index)).cloned() {
                let what = format!("{} cleared", row.label());
                let slot = row.slot();
                let key = device_key.clone();
                shared.edit_profile(what, move |profile| {
                    profile.for_device(&key).bindings.remove(&slot);
                });
            }
        }

        if let Some(target) = pending.copy_bank_to {
            let from = self.active_bank;
            let key = device_key.clone();
            shared.edit_profile(format!("Copied M{from} → M{target}"), move |profile| {
                let settings = profile.for_device(&key);
                let moved: Vec<(String, ghub_core::action::ActionBinding)> = settings
                    .bindings
                    .iter()
                    .filter(|(slot, _)| {
                        slot.starts_with("gkey:") && slot.contains(&format!(":M{from}"))
                    })
                    .map(|(slot, binding)| {
                        (slot.replace(&format!(":M{from}"), &format!(":M{target}")), binding.clone())
                    })
                    .collect();
                for (slot, binding) in moved {
                    settings.bindings.insert(slot, binding);
                }
            });
        }

        if pending.reset_bank {
            let bank = self.active_bank;
            let key = device_key.clone();
            shared.edit_profile(format!("Bank M{bank} reset"), move |profile| {
                let settings = profile.for_device(&key);
                settings.bindings.retain(|slot, _| {
                    !(slot.starts_with("gkey:") && slot.contains(&format!(":M{bank}")))
                });
            });
        }

        if pending.start_record {
            let service = std::sync::Arc::clone(shared.service());
            let key = device_key.clone();
            let gkey = self.record_gkey.clamp(1, 5) as usize;
            let bank = self.active_bank;
            shared.attempt("Record", move || {
                service.start_recording_from_ui(&key, gkey);
                Ok(format!(
                    "Recording to G{gkey} (M{bank}). Type in any window, then press Stop (or MR \
                     on the keyboard)."
                ))
            });
        }

        if pending.stop_record {
            let service = std::sync::Arc::clone(shared.service());
            shared.run("Record", move || {
                service.finish_recording();
                Ok(None)
            });
        }

        if pending.cancel_record {
            shared.service().cancel_recording("cancelled in app");
        }
    }
}

/// `KeyLabel(KeyDefinition)` - a human-readable name for the reaction list.
fn key_label(id: &str, label: &str) -> String {
    match id {
        "Space" => "Space".to_owned(),
        "MediaMute" => "Mute".to_owned(),
        "MediaPlayPause" => "Play / Pause".to_owned(),
        "MediaNext" => "Next track".to_owned(),
        "MediaPrevious" => "Previous track".to_owned(),
        "Brightness" => "Brightness".to_owned(),
        "GameMode" => "Game mode".to_owned(),
        "Logo" => "Logo".to_owned(),
        _ if !label.is_empty() => label.to_owned(),
        _ => id.to_owned(),
    }
}

/// `Hex(string)` - normalises a typed colour, falling back to white.
///
/// White rather than black, and rather than refusing: this is the colour of every key the user has
/// not painted, so a bad value that blacked the keyboard out would look like the app broke.
fn normalise_hex(text: &str) -> String {
    let trimmed = text.trim();
    let with_hash =
        if trimmed.starts_with('#') { trimmed.to_owned() } else { format!("#{trimmed}") };
    if is_hex_color(&with_hash) {
        with_hash.to_uppercase()
    } else {
        "#FFFFFF".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> KeyboardPage {
        KeyboardPage::default()
    }

    #[test]
    fn every_palette_has_seven_swatches_and_they_all_parse() {
        assert_eq!(PALETTES.len(), 5);
        for (name, colours) in PALETTES {
            for hex in colours {
                assert!(parse_hex(hex).is_some(), "{name} has an unparseable swatch {hex}");
                assert!(is_hex_color(hex), "{name}'s {hex} would be rejected by the store");
            }
        }
    }

    #[test]
    fn the_palette_falls_back_to_vibrant_for_an_unknown_name() {
        let mut p = page();
        p.palette_name = "NOT A PALETTE".to_owned();
        assert_eq!(p.palette(), &PALETTES[0].1);
    }

    #[test]
    fn every_quick_group_resolves_to_keys_that_exist() {
        for group in QUICK_GROUP_NAMES {
            let ids = KeyboardPage::quick_group_keys(group);
            assert!(!ids.is_empty(), "{group} resolved to nothing");
            for id in ids {
                assert!(layout::by_id(id).is_some(), "{group} names a missing key {id}");
            }
        }
    }

    #[test]
    fn the_f_keys_group_is_the_twelve_function_keys() {
        let ids = KeyboardPage::quick_group_keys("F-KEYS");
        assert_eq!(ids.len(), 12);
        for n in 1..=12 {
            let wanted = format!("F{n}");
            assert!(ids.iter().any(|id| *id == wanted), "F{n} is missing");
        }
    }

    #[test]
    fn an_unknown_quick_group_selects_nothing_rather_than_everything() {
        assert!(KeyboardPage::quick_group_keys("NOPE").is_empty());
    }

    #[test]
    fn painting_a_key_records_it_against_the_profile_key_the_store_uses() {
        let mut p = page();
        p.current_colour = Color32::from_rgb(0x12, 0x34, 0x56);
        let escape = p.keys.iter().position(|k| k.key.id == "Escape").unwrap();
        p.select_keys(&[escape], true);
        assert_eq!(p.colors.get("key:Escape").map(String::as_str), Some("#123456"));
        // Editing the layout implies static lighting, or "Untouched" would never show it.
        assert_eq!(p.lighting_mode, LightingMode::Static);
        assert!(p.lighting_dirty);
        assert!(p.save_due.is_some(), "the write is debounced, not skipped");
    }

    #[test]
    fn selecting_an_unlit_key_is_refused_so_the_colour_goes_nowhere() {
        let mut p = page();
        let m1 = p.keys.iter().position(|k| k.key.id == "M1").unwrap();
        p.select_keys(&[m1], true);
        assert!(!p.keys[m1].selected);
        assert!(p.colors.is_empty());
    }

    #[test]
    fn undo_restores_the_colour_map_one_step_at_a_time() {
        let mut p = page();
        let w_key = p.keys.iter().position(|k| k.key.id == "W").unwrap();

        p.current_colour = Color32::RED;
        p.select_keys(&[w_key], true);
        p.current_colour = Color32::BLUE;
        p.color_session = false;
        p.select_keys(&[w_key], true);
        assert_eq!(p.colors.get("key:W").map(String::as_str), Some("#0000FF"));

        p.undo_paint();
        assert_eq!(p.colors.get("key:W").map(String::as_str), Some("#FF0000"));
        p.undo_paint();
        assert_eq!(p.colors.get("key:W"), None);
    }

    #[test]
    fn a_picker_drag_is_one_undo_step() {
        let mut p = page();
        let w_key = p.keys.iter().position(|k| k.key.id == "W").unwrap();
        p.select_keys(&[w_key], false);

        // Fifty shades dragged through in one gesture.
        for step in 0..50u8 {
            p.current_colour = Color32::from_rgb(step * 5, 0, 0);
            p.current_colour_changed();
        }
        assert_eq!(p.paint_undo.len(), 1, "the whole drag has to collapse into one step");

        // A new selection ends the session, so the next drag gets its own step.
        p.select_keys(&[w_key], false);
        p.current_colour = Color32::GREEN;
        p.current_colour_changed();
        assert_eq!(p.paint_undo.len(), 2);
    }

    #[test]
    fn the_undo_stack_is_capped() {
        let mut p = page();
        for _ in 0..UNDO_DEPTH + 20 {
            p.push_paint_undo();
        }
        assert_eq!(p.paint_undo.len(), UNDO_DEPTH);
    }

    #[test]
    fn colors_on_device_counts_keys_and_orders_by_how_many() {
        let mut p = page();
        p.default_color = "#FFFFFF".to_owned();
        let ids = ["W", "A", "S"];
        for id in ids {
            let index = p.keys.iter().position(|k| k.key.id == id).unwrap();
            p.colors.insert(format!("key:{id}"), "#FF0000".to_owned());
            let _ = index;
        }
        p.update_fills();

        let red = p.colors_on_device.iter().find(|row| row.hex == "#FF0000").unwrap();
        assert_eq!(red.count, 3);
        assert_eq!(red.text(), "3 KEYS");
        // Every other lit key still shows the unpainted colour, so white must be the bigger row
        // and therefore first.
        assert_eq!(p.colors_on_device[0].hex, "#FFFFFF");
        assert!(p.colors_on_device[0].count > red.count);
    }

    #[test]
    fn one_key_reads_as_singular() {
        let row = ColorUsage { hex: "#FF0000".to_owned(), count: 1 };
        assert_eq!(row.text(), "1 KEY");
    }

    #[test]
    fn game_states_mark_the_two_default_locked_keys_as_default_not_user() {
        let mut p = page();
        p.game_mode_keys = layout::game_mode_candidates()
            .into_iter()
            .map(|key| GameModeKeyRow {
                usage: key.hid_usage,
                // Everything disabled, so the only thing distinguishing the two Windows keys is
                // that they are in the default list.
                disabled: true,
            })
            .collect();
        p.sync_game_states();

        for key in &p.keys {
            if DEFAULT_DISABLED_USAGES.contains(&key.key.hid_usage) {
                assert_eq!(
                    key.game_state,
                    GameModeKeyState::DisabledByDefault,
                    "{} should be hatched",
                    key.key.id
                );
            } else if key.key.is_game_mode_candidate() {
                assert_eq!(key.game_state, GameModeKeyState::DisabledByUser, "{}", key.key.id);
            } else {
                assert_eq!(key.game_state, GameModeKeyState::Locked, "{}", key.key.id);
            }
        }
    }

    /// The media keys are game-mode candidates only if someone changes `is_game_mode_candidate`.
    /// Locking volume out would be a surprise, not a feature, so this pins it.
    #[test]
    fn the_media_keys_are_never_lockable() {
        let p = page();
        for key in p.keys.iter().filter(|k| k.key.group == KeyGroup::Media) {
            assert!(!key.key.is_game_mode_candidate(), "{} is lockable", key.key.id);
        }
    }

    #[test]
    fn adding_a_reaction_twice_selects_the_existing_row_rather_than_duplicating_it() {
        let mut p = page();
        assert!(p.add_reactions(&["Escape"]));
        assert_eq!(p.reactions.len(), 1);

        assert!(!p.add_reactions(&["Escape"]), "nothing was added the second time");
        assert_eq!(p.reactions.len(), 1);
        assert_eq!(p.selected_reaction, Some(0));
    }

    #[test]
    fn a_reaction_cannot_be_added_to_a_key_with_no_led() {
        let mut p = page();
        assert!(!p.add_reactions(&["M1"]));
        assert!(p.reactions.is_empty());
    }

    #[test]
    fn only_enabled_reactions_are_drawn_on_the_render() {
        let mut p = page();
        p.current_colour = Color32::from_rgb(0xFF, 0x00, 0x00);
        p.add_reactions(&["Escape", "W"]);
        p.reactions[1].enabled = false;
        p.update_reaction_overlay();

        let escape = p.keys.iter().find(|k| k.key.id == "Escape").unwrap();
        let w_key = p.keys.iter().find(|k| k.key.id == "W").unwrap();
        assert_eq!(escape.reaction, Some(Color32::from_rgb(0xFF, 0x00, 0x00)));
        assert_eq!(w_key.reaction, None, "a disabled reaction must not look configured");
    }

    #[test]
    fn reactions_are_flagged_as_useless_unless_this_app_writes_the_lighting() {
        let mut p = page();
        for (mode, needs) in [
            (LightingMode::Static, false),
            (LightingMode::Animated, false),
            (LightingMode::DeviceEffect, true),
            (LightingMode::Untouched, true),
            (LightingMode::Artemis, true),
        ] {
            p.lighting_mode = mode;
            assert_eq!(p.reactions_need_lighting(), needs, "{mode:?}");
        }
    }

    #[test]
    fn a_reaction_round_trips_through_the_profile() {
        let mut p = page();
        p.current_colour = Color32::from_rgb(0x12, 0x34, 0x56);
        p.add_reactions(&["Escape"]);
        p.reactions[0].mode = KeyReactionMode::Toggle;

        let settings = KeyboardSettings {
            key_reactions: p.reactions.iter().map(ReactionRow::to_model).collect(),
            ..KeyboardSettings::default()
        };
        let mut reloaded = page();
        reloaded.load_reactions(&settings);
        assert_eq!(reloaded.reactions.len(), 1);
        assert_eq!(reloaded.reactions[0].key_id, "Escape");
        assert_eq!(reloaded.reactions[0].hex, "#123456");
        assert_eq!(reloaded.reactions[0].mode, KeyReactionMode::Toggle);
        assert_eq!(reloaded.reactions[0].label, "Esc");
    }

    #[test]
    fn a_reaction_for_a_key_that_is_not_in_the_layout_is_dropped_rather_than_crashing() {
        let settings = KeyboardSettings {
            key_reactions: vec![KeyReaction {
                key_id: "NotAKey".to_owned(),
                color: "#FF0000".to_owned(),
                mode: KeyReactionMode::Momentary,
                enabled: true,
            }],
            ..KeyboardSettings::default()
        };
        let mut p = page();
        p.load_reactions(&settings);
        assert!(p.reactions.is_empty());
        assert_eq!(p.selected_reaction, None);
    }

    #[test]
    fn the_media_keys_get_their_own_names_in_the_reaction_list() {
        assert_eq!(key_label("MediaPlayPause", "\u{23ef}"), "Play / Pause");
        assert_eq!(key_label("MediaMute", "\u{1f507}"), "Mute");
        assert_eq!(key_label("GameMode", "Game"), "Game mode");
        // Anything else keeps the layout's own legend.
        assert_eq!(key_label("Escape", "Esc"), "Esc");
        // And a key with no legend falls back to its id rather than showing nothing.
        assert_eq!(key_label("Weird", ""), "Weird");
    }

    #[test]
    fn a_bad_unpainted_colour_becomes_white_not_black() {
        // Black would look like the app blanked the keyboard; white is what an unconfigured
        // keyboard shows.
        assert_eq!(normalise_hex("not a colour"), "#FFFFFF");
        assert_eq!(normalise_hex(""), "#FFFFFF");
        // A missing hash is accepted, and the result is upper-cased so the colour map's keys
        // compare equal across edits.
        assert_eq!(normalise_hex("00ff00"), "#00FF00");
        assert_eq!(normalise_hex("  #abcdef  "), "#ABCDEF");
    }

    #[test]
    fn the_slot_label_and_id_agree_about_the_shift_layer() {
        let base = SlotRow { gkey: 3, bank: 2, shift: false, description: String::new() };
        assert_eq!(base.label(), "G3 · M2");
        assert_eq!(base.slot(), gkey_slot(3, 2, false));

        let shifted = SlotRow { shift: true, ..base.clone() };
        assert_eq!(shifted.label(), "G3 · M2 · G-Shift");
        assert_eq!(shifted.slot(), gkey_slot(3, 2, true));
        assert_ne!(base.slot(), shifted.slot());
    }

    #[test]
    fn the_selection_heading_says_what_the_pointer_will_do() {
        let mut p = page();
        assert_eq!(p.selection_info(), "CLICK OR DRAG TO SELECT");
        p.brush_mode = true;
        assert_eq!(p.selection_info(), "BRUSH: CLICK OR DRAG OVER KEYS");

        let escape = p.keys.iter().position(|k| k.key.id == "Escape").unwrap();
        p.keys[escape].selected = true;
        assert_eq!(p.selection_info(), "1 SELECTED");
    }

    #[test]
    fn the_animation_summary_distinguishes_selected_from_running() {
        let mut p = page();
        p.selected_animation = 0;
        let name = effects::CATALOG[0].name;
        p.lighting_mode = LightingMode::Static;
        assert_eq!(p.animation_summary(), format!("{name} selected. Press Play to run it."));
        p.lighting_mode = LightingMode::Animated;
        assert_eq!(p.animation_summary(), format!("{name} is running at 25 fps."));
    }

    #[test]
    fn loading_an_unknown_animation_id_falls_back_to_the_catalogue_rather_than_failing() {
        let mut p = page();
        let lighting = LightingSettings {
            animation: AnimationSettings {
                effect: "an effect from a newer build".to_owned(),
                ..AnimationSettings::default()
            },
            ..LightingSettings::default()
        };
        p.load_animation(&lighting);
        assert!(p.selected_animation < effects::CATALOG.len());
    }

    #[test]
    fn animation_settings_are_clamped_on_the_way_in_and_out() {
        let mut p = page();
        let lighting = LightingSettings {
            animation: AnimationSettings {
                speed: 0.0,
                frames_per_second: 9999,
                intensity: 50.0,
                ..AnimationSettings::default()
            },
            ..LightingSettings::default()
        };
        p.load_animation(&lighting);
        assert_eq!(p.animation_speed, 5.0);
        assert_eq!(p.animation_fps, 60);
        assert_eq!(p.animation_intensity, 200.0);
    }

    #[test]
    fn the_lighting_mode_list_is_every_mode_so_none_becomes_unreachable() {
        // There is no way to enumerate a Rust enum, so the list is pinned against the one value
        // that would be missed: a mode left out here cannot be chosen at all.
        assert_eq!(LIGHTING_MODES.len(), 5);
        for mode in LIGHTING_MODES {
            assert!(LIGHTING_MODES.iter().filter(|m| *m == mode).count() == 1, "{mode:?} twice");
        }
        assert!(LIGHTING_MODES.contains(&LightingMode::default()));
    }

    #[test]
    fn the_direction_list_covers_every_direction() {
        assert_eq!(ANIMATION_DIRECTIONS.len(), 6);
        assert!(ANIMATION_DIRECTIONS.contains(&AnimationDirection::default()));
    }

    #[test]
    fn both_reaction_modes_are_offered() {
        assert_eq!(REACTION_MODES.len(), 2);
        assert!(REACTION_MODES.iter().any(|(mode, _)| *mode == KeyReactionMode::Momentary));
        assert!(REACTION_MODES.iter().any(|(mode, _)| *mode == KeyReactionMode::Toggle));
    }

    #[test]
    fn the_debounce_is_the_interval_the_c_sharp_used() {
        assert_eq!(SAVE_DEBOUNCE, Duration::from_millis(450));
    }
}
