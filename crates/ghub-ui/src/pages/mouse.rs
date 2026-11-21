//! The mouse device screen. Port of `MouseTabViewModel.cs`, `MouseTabView.axaml` and
//! `MouseTabView.axaml.cs`.
//!
//! G HUB's G903 screen: an icon rail (sensitivity, assignments, LIGHTSYNC, device settings), a
//! settings panel on the left, and the real device render on the right with the button callouts
//! and the lighting zones drawn over it. The callouts are laid out in gutters either side of the
//! render, which is where G HUB's own `g903_hero` geometry puts them - its label coordinates are
//! negative on the left and past the image width on the right, so the stage canvas is the render
//! plus a [`GUTTER`] on each side and [`TOP_GUTTER`] of head room for the wheel-click callout.
//!
//! Three things here differ in kind from the keyboard page:
//!
//! * **Nothing autosaves.** DPI and lighting are written by an explicit Save, because a DPI change
//!   takes effect on the sensor and silently changing it mid-drag is not something to do to a
//!   pointing device.
//! * **Two actions ask first.** Remapping the primary button, and writing onboard flash. Both go
//!   through [`Confirm`].
//! * **Buttons 6 and up are identified by pressing them.** They arrive through the 0x8110 spy
//!   rather than from a table, so the page shows the last one seen; that is the only way to tell
//!   which physical button is which.

use std::sync::{Arc, Mutex};

use egui::{Color32, CornerRadius, Pos2, Rect, Stroke, StrokeKind, Ui, Vec2, pos2};
use ghub_core::action::{ActionBinding, ActionDefinition, MouseButtonKind};
use ghub_core::profile::{
    DpiStage, LightingMode, LightingSettings, MouseSettings, is_hex_color,
};
use ghub_core::service::button_slot;
use ghub_core::ImageRect;
use ghub_hidpp::features::onboard::OnboardMode;
use ghub_hidpp::features::onboard_writer::{OnboardEdit, OnboardProfileWriter, OnboardWritePlan};
use ghub_hidpp::identity::{ConnectionType, PeripheralKind};

use crate::color_picker::{ColorPicker, parse_hex, to_hex};
use crate::controls as c;
use crate::icons::{Icon, IconCache};
use crate::shared::{Changes, Shared};
use crate::theme as t;
use crate::widgets as w;

use super::action_editor::ActionEditor;
use super::device_stage::Projection;
use super::keyboard::{LIGHTING_MODES, PALETTES};
use super::power_save::{self, PowerSave};
use super::{Confirm, scroll, status_band};

/// Gutter either side of the render that the callouts live in, in render units.
pub const GUTTER: f64 = 1180.0;
/// Head room above the render for the wheel-click callout.
pub const TOP_GUTTER: f64 = 430.0;
pub const CALLOUT_WIDTH: f64 = 1080.0;
pub const CALLOUT_HEIGHT: f64 = 190.0;

/// `<Viewbox MaxHeight="660">`.
const STAGE_MAX_HEIGHT: f32 = 660.0;

/// The render's own size when the artwork is not shipped, so the callouts still have somewhere to
/// be and the page still works.
const FALLBACK_RENDER: (f64, f64) = (1440.0, 2858.0);

/// The dot drawn on the button, in render units.
const MARKER_DIAMETER: f64 = 46.0;

/// `DpiStageViewModel.Palette` - the colour G HUB gives each stage pill, in stage order.
pub static STAGE_PALETTE: &[&str] = &["#00A8E8", "#7DD321", "#FFE117", "#FF7C10", "#F32A2A"];

/// `MousePage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SubPage {
    #[default]
    Sensitivity,
    Assignments,
    Lightsync,
    DeviceSettings,
}

/// Which side of the render a callout sits on, which decides where its leader line starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CalloutSide {
    Left,
    Right,
    Top,
}

/// `DpiStageViewModel`.
struct StageRow {
    dpi: i64,
    buffer: String,
    enabled: bool,
}

impl StageRow {
    fn new(dpi: i64, enabled: bool) -> Self {
        Self { dpi, buffer: dpi.to_string(), enabled }
    }

    /// The pill colour for a stage number, cycling through G HUB's five.
    fn colour(number: usize) -> Color32 {
        let hex = STAGE_PALETTE[(number + STAGE_PALETTE.len() - 1) % STAGE_PALETTE.len()];
        parse_hex(hex).unwrap_or(t::ACCENT)
    }
}

/// `MouseButtonViewModel` - one assignable control, positioned from G HUB's own geometry.
struct ButtonRow {
    /// The profile slot, already including the G-Shift suffix when that layer is being edited.
    slot: String,
    label: String,
    description: String,
    /// Where the dot sits, in stage coordinates.
    marker: Pos2,
    /// Top-left of the callout box, in stage coordinates.
    callout: Pos2,
    side: CalloutSide,
    assignable: bool,
    disabled_reason: Option<String>,
    site: String,
}

impl ButtonRow {
    /// `Tip`.
    fn tip(&self) -> String {
        match &self.disabled_reason {
            Some(reason) => reason.clone(),
            None => format!("{}: {}", self.label, self.site),
        }
    }

    /// `LeaderStart` - the leader runs from the edge of the callout facing the mouse.
    fn leader_start(&self) -> Pos2 {
        match self.side {
            CalloutSide::Left => pos2(
                self.callout.x + CALLOUT_WIDTH as f32,
                self.callout.y + CALLOUT_HEIGHT as f32 / 2.0,
            ),
            CalloutSide::Right => {
                pos2(self.callout.x, self.callout.y + CALLOUT_HEIGHT as f32 / 2.0)
            }
            CalloutSide::Top => pos2(
                self.callout.x + CALLOUT_WIDTH as f32 / 2.0,
                self.callout.y + CALLOUT_HEIGHT as f32,
            ),
        }
    }

    fn callout_rect(&self) -> Rect {
        Rect::from_min_size(
            self.callout,
            Vec2::new(CALLOUT_WIDTH as f32, CALLOUT_HEIGHT as f32),
        )
    }
}

/// `MouseZoneViewModel` - one RGB zone drawn over the render.
struct ZoneRow {
    /// The 0x8071 cluster index, which is what the profile keys the colour by.
    index: usize,
    name: String,
    /// In stage coordinates, so already offset by the gutters.
    rect: Rect,
    hex: String,
    hex_buffer: String,
    selected: bool,
}

impl ZoneRow {
    /// `Key` - `zone:0`, `zone:1`.
    fn key(&self) -> String {
        format!("zone:{}", self.index)
    }

    fn colour(&self) -> Color32 {
        parse_hex(&self.hex).unwrap_or(Color32::from_rgb(0x00, 0xBF, 0xFF))
    }
}

/// What the worker thread reads back, for the next frame to show.
///
/// The jobs run off the UI thread and these are *values the device reported*, not settings; they
/// cannot be derived from the profile and they cannot be squeezed into the job's one status line.
/// One cell rather than six keeps the locking trivial.
#[derive(Default)]
struct Readback {
    /// The sensor's current DPI, from Read or from the read-back of a Test write.
    live_dpi: Option<u32>,
    /// `OnboardInfo` - mode, active slot and the capability summary.
    onboard_info: String,
    /// `OnboardProfiles` - one line per slot.
    onboard_profiles: Vec<String>,
    /// The pending write plan, and the diff the user confirms.
    plan: Option<OnboardWritePlan>,
    plan_text: String,
}

/// What a frame's interaction asked for, applied after the draw.
#[derive(Default)]
struct Pending {
    read_live: bool,
    write_live: bool,
    save_settings: bool,
    save_lighting: bool,
    read_power_save: bool,
    apply_binding: bool,
    clear_binding: bool,
    read_onboard: bool,
    activate_slot: bool,
    plan_save: bool,
    commit_save: bool,
}

pub struct MousePage {
    mice: Vec<(String, String)>,
    selected_device: Option<String>,

    page: SubPage,

    // ------------------------------------------------------------- sensitivity
    stages: Vec<StageRow>,
    default_stage: usize,
    shift_dpi: i64,
    shift_dpi_buffer: String,
    show_dpi_notification: bool,
    rates: Vec<u32>,
    selected_rate: Option<u32>,
    wheel_high_resolution: bool,
    wheel_inverted: bool,
    live_dpi: i64,
    live_dpi_buffer: String,

    // ------------------------------------------------------------- assignments
    bindings: Vec<ButtonRow>,
    selected_binding: Option<usize>,
    edit_shift_layer: bool,
    editor: ActionEditor,
    /// `LastSpyButton` - which physical button the 0x8110 spy last saw.
    last_spy_button: String,

    // ------------------------------------------------------------- lightsync
    zones: Vec<ZoneRow>,
    lighting_mode: LightingMode,
    brightness: f64,
    device_effects: Vec<(String, u16)>,
    device_effect: Option<u16>,
    effect_period: i64,
    effect_period_buffer: String,
    current_colour: Color32,
    picker: ColorPicker,
    picker_open: bool,
    eyedropper: bool,
    palette_name: String,
    power_save: PowerSave,

    // ------------------------------------------------------------- onboard memory
    onboard_slot: i64,
    onboard_slot_buffer: String,

    artwork_folder: &'static str,
    readback: Arc<Mutex<Readback>>,
    confirm: Confirm,
    /// What the open confirmation is for, so the answer goes to the right place.
    confirming: Option<Confirming>,
    /// Set when a callout was clicked, so the editor is reloaded after the draw rather than during
    /// it - the reload reads the profile, and the stage is mid-layout.
    pending_editor_load: bool,
    loaded: bool,
}

/// Which question [`Confirm`] is currently asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confirming {
    PrimaryRemap,
    OnboardWrite,
}

impl Default for MousePage {
    fn default() -> Self {
        Self {
            mice: Vec::new(),
            selected_device: None,
            page: SubPage::default(),
            stages: Vec::new(),
            default_stage: 1,
            shift_dpi: 400,
            shift_dpi_buffer: "400".to_owned(),
            show_dpi_notification: true,
            rates: Vec::new(),
            selected_rate: None,
            wheel_high_resolution: false,
            wheel_inverted: false,
            live_dpi: 0,
            live_dpi_buffer: "0".to_owned(),
            bindings: Vec::new(),
            selected_binding: None,
            edit_shift_layer: false,
            editor: ActionEditor::default(),
            last_spy_button: String::new(),
            zones: Vec::new(),
            lighting_mode: LightingMode::Untouched,
            brightness: 100.0,
            device_effects: Vec::new(),
            device_effect: None,
            effect_period: 5000,
            effect_period_buffer: "5000".to_owned(),
            current_colour: Color32::from_rgb(0x00, 0xAA, 0xFF),
            picker: ColorPicker::new(Color32::from_rgb(0x00, 0xAA, 0xFF)),
            picker_open: false,
            eyedropper: false,
            palette_name: "VIBRANT".to_owned(),
            power_save: PowerSave::default(),
            onboard_slot: 1,
            onboard_slot_buffer: "1".to_owned(),
            artwork_folder: "g903_hero",
            readback: Arc::new(Mutex::new(Readback::default())),
            confirm: Confirm::default(),
            confirming: None,
            pending_editor_load: false,
            loaded: false,
        }
    }
}

/// `Controls` - the G903's eleven assignable positions in G HUB's own numbering, mapped onto the
/// slots this application binds.
///
/// 1-5 are seen by the low-level mouse hook. 6 and up have no OS-level identity at all: they come
/// live from the 0x8110 button spy, which is why they are only listed when the device says it has
/// that many physical buttons.
static CONTROLS: &[(&str, &str, &str)] = &[
    ("Left", "Left Click", "g903hero_g1_m1"),
    ("Right", "Right Click", "g903hero_g2_m1"),
    ("Middle", "Wheel Click", "g903hero_g3_m1"),
    ("XButton1", "Back (left side)", "g903hero_g4_m1"),
    ("XButton2", "Forward (left side)", "g903hero_g5_m1"),
    ("spy6", "Back (right side)", "g903hero_g6_m1"),
    ("spy7", "Forward (right side)", "g903hero_g7_m1"),
    ("spy8", "DPI Down", "g903hero_g8_m1"),
    ("spy9", "DPI Up", "g903hero_g9_m1"),
    ("spy10", "Wheel Left", "g903hero_g10_m1"),
    ("spy11", "Wheel Right", "g903hero_g11_m1"),
];

// =============================================================== state queries

impl MousePage {
    fn device(&self, shared: &Shared) -> Option<ghub_core::manager::ManagedDevice> {
        self.selected_device.as_deref().and_then(|key| shared.device(key))
    }

    fn dpi_capability(
        &self,
        shared: &Shared,
    ) -> Option<ghub_hidpp::capability::DpiCapability> {
        self.device(shared).and_then(|d| d.capabilities.dpi.clone())
    }

    /// `DpiRange` - what the device said about its own sensor, never a guess.
    fn dpi_range(&self, shared: &Shared) -> String {
        match self.dpi_capability(shared) {
            None => "DPI not exposed by this device".to_owned(),
            Some(dpi) if !dpi.discrete_values.is_empty() => format!(
                "Device values: {}",
                dpi.discrete_values
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Some(dpi) => format!(
                "{}-{} DPI in steps of {}",
                dpi.minimum, dpi.maximum, dpi.step
            ),
        }
    }

    fn dpi_bounds(&self, shared: &Shared) -> (i64, i64, i64) {
        match self.dpi_capability(shared) {
            Some(dpi) => (dpi.minimum as i64, dpi.maximum as i64, (dpi.step as i64).max(1)),
            None => (100, 25600, 50),
        }
    }

    fn dpi_supported(&self, shared: &Shared) -> bool {
        self.dpi_capability(shared).is_some_and(|dpi| dpi.meta.is_usable())
    }

    fn rate_supported(&self, shared: &Shared) -> bool {
        self.device(shared)
            .and_then(|d| d.capabilities.report_rate.clone())
            .is_some_and(|rate| rate.meta.is_usable())
    }

    fn lighting_supported(&self, shared: &Shared) -> bool {
        self.device(shared).is_some_and(|d| d.capabilities.lighting.is_some())
    }

    fn onboard_capability(
        &self,
        shared: &Shared,
    ) -> Option<ghub_hidpp::capability::OnboardProfilesCapability> {
        self.device(shared).and_then(|d| d.capabilities.onboard_profiles.clone())
    }

    fn onboard_writes_enabled(&self, shared: &Shared) -> bool {
        self.onboard_capability(shared).is_some_and(|onboard| onboard.writes_enabled)
    }

    /// `OnboardWriteHint`.
    fn onboard_write_hint(&self, shared: &Shared) -> &'static str {
        if self.onboard_writes_enabled(shared) {
            "Save to Device is enabled. Every write is previewed, backed up, verified by read-back \
             and rolled back on mismatch."
        } else {
            "Save to Device is off (Options \u{2192} Allow Save to Device). Onboard memory is \
             read-only."
        }
    }

    /// Whether the device exposes 0x2121, which is what the scroll-wheel block needs.
    fn wheel_support(&self, shared: &Shared) -> (bool, bool) {
        let key = match self.selected_device.as_deref() {
            Some(key) => key,
            None => return (false, false),
        };
        shared
            .host
            .devices
            .with_session(key, |session| match session.wheel() {
                Some(wheel) => (true, wheel.info.can_invert),
                None => (false, false),
            })
            .unwrap_or((false, false))
    }

    /// `Connection`.
    fn connection(&self, shared: &Shared) -> &'static str {
        match self.device(shared).and_then(|d| d.descriptor.map(|desc| desc.connection_type)) {
            Some(ConnectionType::LightspeedReceiver) => "LIGHTSPEED receiver",
            Some(ConnectionType::Wired) => "USB cable",
            _ => "-",
        }
    }

    fn device_display_name(&self, shared: &Shared) -> String {
        self.device(shared)
            .and_then(|d| d.descriptor.map(|desc| desc.display_name))
            .and_then(|name| name.split(' ').next().map(str::to_uppercase))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "MOUSE".to_owned())
    }

    fn firmware_version(&self, shared: &Shared) -> String {
        self.device(shared)
            .and_then(|d| d.descriptor.and_then(|desc| desc.firmware_version))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "unknown".to_owned())
    }

    /// `BatteryState`. "no battery reported" rather than a plausible-looking 0%.
    fn battery_state(&self, shared: &Shared) -> String {
        match self.device(shared).and_then(|d| d.health.battery) {
            Some(battery) => match battery.level_percent {
                Some(percent) => format!("{percent}% \u{b7} {:?}", battery.charging),
                None => "no battery reported".to_owned(),
            },
            None => "no battery reported".to_owned(),
        }
    }

    /// `SelectionInfo`.
    fn selection_info(&self) -> String {
        let selected = self.zones.iter().filter(|z| z.selected).count();
        if selected == 0 {
            "CLICK A LIGHTING ZONE".to_owned()
        } else if selected == self.zones.len() {
            "ALL ZONES SELECTED".to_owned()
        } else {
            format!("{selected} ZONE SELECTED")
        }
    }

    fn palette(&self) -> &'static [&'static str; 7] {
        PALETTES
            .iter()
            .find(|(name, _)| *name == self.palette_name)
            .map_or(&PALETTES[0].1, |(_, colours)| colours)
    }

    /// The canvas the stage is laid out in: the render plus its gutters.
    fn stage_size(&self, render: (f64, f64)) -> Vec2 {
        Vec2::new(
            (render.0 + GUTTER * 2.0) as f32,
            (render.1 + TOP_GUTTER) as f32,
        )
    }
}

// =============================================================== loading

impl MousePage {
    /// `Refresh()`.
    fn reload_devices(&mut self, shared: &Shared) {
        let keep = self.selected_device.clone();
        self.mice = shared
            .devices_of_kind(PeripheralKind::Mouse)
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
            .filter(|key| self.mice.iter().any(|(k, _)| k == key))
            .or_else(|| self.mice.first().map(|(k, _)| k.clone()));
    }

    /// `LoadFromProfile()`.
    fn load(&mut self, shared: &mut Shared, ctx: &egui::Context) {
        let Some(device_key) = self.selected_device.clone() else { return };
        let profile = shared.active_profile();
        let settings = profile.device(&device_key);
        let mouse = settings.and_then(|s| s.mouse.clone()).unwrap_or_default();
        let lighting = settings.map(|s| s.lighting.clone()).unwrap_or_default();

        self.stages = mouse
            .dpi_stages
            .iter()
            .map(|stage| StageRow::new(stage.dpi as i64, stage.enabled))
            .collect();
        if self.stages.is_empty() {
            // A profile with no stages would leave the page with nothing to edit and Save would
            // refuse; one stage at the sensor's own default is the smallest honest starting point.
            let (min, _, _) = self.dpi_bounds(shared);
            self.stages.push(StageRow::new(min.max(800), true));
        }
        self.default_stage = (mouse.default_stage + 1).clamp(1, self.stages.len());
        self.shift_dpi = mouse.shift_dpi.unwrap_or(400) as i64;
        self.shift_dpi_buffer = self.shift_dpi.to_string();
        self.show_dpi_notification = mouse.show_dpi_notification;
        self.wheel_high_resolution = mouse.wheel_high_resolution.unwrap_or(false);
        self.wheel_inverted = mouse.wheel_inverted.unwrap_or(false);

        self.rates = self
            .device(shared)
            .and_then(|d| d.capabilities.report_rate.clone())
            .map(|rate| rate.rates_hz)
            .unwrap_or_default();
        self.selected_rate = mouse.report_rate_hz.filter(|hz| self.rates.contains(hz));

        self.lighting_mode = lighting.mode;
        self.brightness = lighting.brightness * 100.0;
        self.effect_period = lighting.device_effect_period_ms as i64;
        self.effect_period_buffer = self.effect_period.to_string();
        let supported = power_save::supports_power_timers(&shared.host.devices, Some(&device_key));
        self.power_save.load(&lighting, supported);

        // 0x8071's effect list, de-duplicated by effect id across clusters.
        self.device_effects.clear();
        if let Some(effects) = shared.host.devices.with_session(&device_key, |session| {
            session.rgb().map(|rgb| {
                let mut seen: Vec<(String, u16)> = Vec::new();
                for effect in rgb.clusters.iter().flat_map(|cluster| cluster.effects.iter()) {
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
            // `?? DeviceEffects.FirstOrDefault(e => e.Value == 0x0001)`: the static effect, which
            // is the one a colour actually shows through.
            .or_else(|| {
                self.device_effects
                    .iter()
                    .find(|(_, id)| *id == 0x0001)
                    .map(|(_, id)| *id)
            });

        let render = self.render_size(shared, ctx);
        self.load_zones(shared, ctx, &lighting, render);
        self.load_bindings(shared, ctx, render);
    }

    /// The render's own size, or the fallback when the artwork is not shipped.
    fn render_size(&self, shared: &mut Shared, ctx: &egui::Context) -> (f64, f64) {
        match shared.artwork.get(ctx, self.artwork_folder) {
            Some(artwork) => (artwork.width() as f64, artwork.height() as f64),
            None => FALLBACK_RENDER,
        }
    }

    /// `LoadZones(LightingSettings)`.
    ///
    /// The indices are the 0x8071 cluster indices the device reports, so zone 0 is "Primary" (the
    /// DPI strip) and zone 1 "Logo" (the palm badge) on a G903. Falling back to that pair when the
    /// device has not been opened keeps the page usable offline; the geometry comes from G HUB's
    /// own `g903_hero` render either way.
    fn load_zones(
        &mut self,
        shared: &mut Shared,
        ctx: &egui::Context,
        lighting: &LightingSettings,
        render: (f64, f64),
    ) {
        let selected: Vec<usize> =
            self.zones.iter().filter(|z| z.selected).map(|z| z.index).collect();
        self.zones.clear();

        let clusters: Vec<(usize, String)> = self
            .selected_device
            .as_deref()
            .and_then(|key| {
                shared.host.devices.with_session(key, |session| {
                    session.rgb().map(|rgb| {
                        rgb.clusters
                            .iter()
                            .map(|cluster| (cluster.index as usize, cluster.location_name()))
                            .collect::<Vec<_>>()
                    })
                })
            })
            .flatten()
            .unwrap_or_else(|| vec![(0, "Primary".to_owned()), (1, "Logo".to_owned())]);

        let metadata = shared.artwork.get(ctx, self.artwork_folder).map(|a| a.metadata.clone());
        for (index, name) in clusters {
            let bounds = match &metadata {
                Some(metadata) => {
                    if name == "Logo" {
                        metadata.logo_bounds()
                    } else {
                        metadata.primary_bounds()
                    }
                }
                // Without geometry there is nowhere to draw the zone, so it is listed in the panel
                // but not on the render. Guessing a rectangle would put a highlight on the wrong
                // part of the photograph.
                None => Some(ImageRect {
                    x: render.0 / 2.0 - 120.0,
                    y: render.1 / 2.0 - 60.0,
                    width: 240.0,
                    height: 120.0,
                }),
            };
            let Some(r) = bounds else { continue };
            let hex = lighting
                .static_colors
                .get(&format!("zone:{index}"))
                .cloned()
                .unwrap_or_else(|| lighting.default_color.clone());
            self.zones.push(ZoneRow {
                index,
                name: name.to_uppercase(),
                rect: Rect::from_min_size(
                    pos2((r.x + GUTTER) as f32, (r.y + TOP_GUTTER) as f32),
                    Vec2::new(r.width as f32, r.height as f32),
                ),
                hex_buffer: hex.clone(),
                hex,
                selected: selected.contains(&index),
            });
        }
    }

    /// `LoadBindings()`.
    fn load_bindings(&mut self, shared: &mut Shared, ctx: &egui::Context, render: (f64, f64)) {
        let keep = self.selected_binding.and_then(|index| {
            self.bindings.get(index).map(|row| row.slot.clone())
        });
        self.bindings.clear();
        let Some(device_key) = self.selected_device.clone() else { return };

        let spy_buttons = shared
            .host
            .devices
            .with_session(&device_key, |session| {
                session.button_spy().map_or(0, |spy| spy.button_count)
            })
            .unwrap_or(0);

        let profile = shared.active_profile();
        let bindings = profile
            .device(&device_key)
            .map(|s| s.bindings.clone())
            .unwrap_or_default();
        let metadata = shared.artwork.get(ctx, self.artwork_folder).map(|a| a.metadata.clone());
        let stage = self.stage_size(render);
        let default_dpi = ghub_core::service::default_spy_dpi_buttons(
            self.device(shared).and_then(|d| d.descriptor).as_ref(),
        );

        for (control, label, ghub) in CONTROLS {
            let spy = control.starts_with("spy");
            if spy {
                let number: usize = control[3..].parse().unwrap_or(usize::MAX);
                if number > spy_buttons {
                    // The device does not report this many physical buttons, so there is nothing
                    // to bind and listing it would invite binding a button that does not exist.
                    continue;
                }
            }

            let slot = button_slot(control, self.edit_shift_layer);
            let current = bindings.get(&slot);
            let marker = metadata.as_ref().and_then(|m| m.assignment(ghub));
            let (marker_x, marker_y) = match marker {
                Some(marker) => (marker.x, marker.y),
                None => (720.0, 1400.0),
            };
            let side = side_of(marker.map(|m| (m.label_x, m.label_y)));

            self.bindings.push(ButtonRow {
                slot,
                label: format!(
                    "{label}{}",
                    if self.edit_shift_layer { " \u{b7} G-SHIFT" } else { "" }
                ),
                description: match current {
                    Some(binding) => binding.action.describe(),
                    None if self.edit_shift_layer => "Falls back to base layer".to_owned(),
                    None if spy => match default_dpi
                        .iter()
                        .find(|(button, _)| format!("spy{button}") == *control)
                    {
                        Some((_, command)) => format!(
                            "Default ({})",
                            ghub_core::action::ActionDefinition::Dpi {
                                command: *command,
                                stage: 0
                            }
                            .describe()
                        ),
                        None => "Default (firmware)".to_owned(),
                    },
                    None => "Default".to_owned(),
                },
                marker: pos2((marker_x + GUTTER) as f32, (marker_y + TOP_GUTTER) as f32),
                callout: pos2(
                    callout_left(side, marker.map(|m| m.x), render.0, stage.x as f64) as f32,
                    callout_top(
                        side,
                        marker.map(|m| if m.label_y != 0.0 { m.label_y } else { m.y }),
                        stage.y as f64,
                    ) as f32,
                ),
                side,
                assignable: true,
                disabled_reason: None,
                site: if spy {
                    "Live (HID++ 0x8110 button spy; the button's native action is muted while \
                     bound)"
                        .to_owned()
                } else {
                    "Software (low-level mouse hook)".to_owned()
                },
            });
        }

        // The onboard buttons are not bound here at all. Saying so on the render is the only way
        // the page can explain why pressing button 7 still does its firmware thing.
        let onboard_buttons =
            self.onboard_capability(shared).map_or(0, |onboard| onboard.button_count);
        if onboard_buttons > self.bindings.len() {
            self.bindings.push(ButtonRow {
                slot: "onboard".to_owned(),
                label: format!("Onboard buttons (1-{onboard_buttons})"),
                description: "Remap through onboard memory (Save to Device)".to_owned(),
                marker: pos2(
                    (GUTTER + render.0 / 2.0) as f32,
                    (TOP_GUTTER + render.1 - 120.0) as f32,
                ),
                callout: pos2(
                    (GUTTER + render.0 / 2.0 - CALLOUT_WIDTH / 2.0) as f32,
                    (TOP_GUTTER + render.1 - 60.0) as f32,
                ),
                side: CalloutSide::Top,
                assignable: false,
                disabled_reason: Some(
                    "Onboard bindings are what the mouse does without this app. Change them with \
                     Save to Device on the settings page."
                        .to_owned(),
                ),
                site: "Onboard memory".to_owned(),
            });
        }

        self.selected_binding = keep
            .and_then(|slot| self.bindings.iter().position(|row| row.slot == slot))
            .or(if self.bindings.is_empty() { None } else { Some(0) });
        self.load_editor(shared);
    }

    /// Loads whatever is bound to the selected control into the editor.
    fn load_editor(&mut self, shared: &Shared) {
        self.editor.macros = shared
            .document()
            .macros
            .iter()
            .map(|m| (m.name.clone(), m.id))
            .collect();
        let Some(row) = self.selected_binding.and_then(|index| self.bindings.get(index)) else {
            return;
        };
        let action: Option<ActionDefinition> = self
            .selected_device
            .as_deref()
            .and_then(|key| shared.active_profile().device(key).cloned())
            .and_then(|settings| settings.bindings.get(&row.slot).map(|b| b.action.clone()));
        self.editor.load(action.as_ref());
    }
}

/// `SideOf(marker)` - G HUB's own label coordinates decide which gutter a callout lives in.
fn side_of(label: Option<(f64, f64)>) -> CalloutSide {
    match label {
        None => CalloutSide::Top,
        Some((_, y)) if y < 0.0 => CalloutSide::Top,
        Some((x, _)) if x < 0.0 => CalloutSide::Left,
        _ => CalloutSide::Right,
    }
}

/// `CalloutLeft(marker)`.
fn callout_left(side: CalloutSide, marker_x: Option<f64>, render_width: f64, stage_width: f64) -> f64 {
    match side {
        CalloutSide::Left => GUTTER - CALLOUT_WIDTH - 70.0,
        CalloutSide::Right => GUTTER + render_width + 70.0,
        CalloutSide::Top => (marker_x.unwrap_or(720.0) + GUTTER - CALLOUT_WIDTH / 2.0)
            .clamp(0.0, (stage_width - CALLOUT_WIDTH).max(0.0)),
    }
}

/// `CalloutTop(marker)`.
fn callout_top(side: CalloutSide, label_y: Option<f64>, stage_height: f64) -> f64 {
    match side {
        CalloutSide::Top => (TOP_GUTTER - CALLOUT_HEIGHT - 60.0).max(10.0),
        _ => (label_y.unwrap_or(0.0) + TOP_GUTTER - CALLOUT_HEIGHT / 2.0)
            .clamp(10.0, (stage_height - CALLOUT_HEIGHT).max(10.0)),
    }
}

/// The store's snapshot record for a snapshot just read off the device.
fn persisted(
    snapshot: &ghub_hidpp::features::OnboardSnapshot,
) -> ghub_core::profile::OnboardSnapshot {
    ghub_core::profile::OnboardSnapshot {
        device_key: snapshot.device_key.clone(),
        device_name: snapshot.device_name.clone(),
        firmware: snapshot.firmware.clone(),
        taken: ghub_core::time::now_unix(),
        sectors_hex: snapshot.sectors_hex.clone(),
    }
}

/// `Normalize(hex)`.
/// One DPI slider. Returns the new DPI when it moved, already rounded to what the sensor takes
/// (its step, or its nearest listed value) so the number shown is the number written.
fn dpi_slider(
    ui: &mut Ui,
    capability: Option<&ghub_hidpp::capability::DpiCapability>,
    dpi: i64,
    min: i64,
    max: i64,
    step: i64,
) -> Option<i64> {
    let mut value = dpi as f64;
    if !c::slider(ui, &mut value, min as f64..=max as f64, true) {
        return None;
    }
    let raw = value.round().clamp(min as f64, max as f64) as i64;
    let snapped = match capability {
        Some(cap) => cap.clamp(raw as u32) as i64,
        None => min + ((raw - min + step / 2) / step.max(1)) * step.max(1),
    };
    (snapped != dpi).then_some(snapped)
}

fn normalise_hex(text: &str) -> String {
    let trimmed = text.trim();
    let with_hash =
        if trimmed.starts_with('#') { trimmed.to_owned() } else { format!("#{trimmed}") };
    if is_hex_color(&with_hash) { with_hash.to_uppercase() } else { "#FFFFFF".to_owned() }
}

// =============================================================== drawing

impl MousePage {
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
        if changes.devices || changes.document || !self.loaded {
            let ctx = ui.ctx().clone();
            self.load(shared, &ctx);
            self.loaded = true;
        }

        // `SpyButtonSeen` - consumed rather than observed, so the line only changes when a button
        // is actually pressed.
        if let Some((device_key, button)) = shared.spy_button.take() {
            if Some(&device_key) == self.selected_device.as_ref() {
                self.last_spy_button = format!("Last physical button pressed: {button}");
            }
        }

        if let Some(dpi) = self.readback.lock().ok().and_then(|r| r.live_dpi) {
            if dpi as i64 != self.live_dpi {
                self.live_dpi = dpi as i64;
                self.live_dpi_buffer = self.live_dpi.to_string();
            }
        }

        self.answer_confirmation(shared, ui.ctx(), icons);

        let mut pending = Pending::default();
        let busy = shared.busy();

        if let Some(id) = self.header(ui, icons, shared) {
            super::switch_profile(shared, id);
        }
        ui.add_space(4.0);
        self.status(ui, icons, shared);
        ui.add_space(8.0);

        let body = ui.available_size();
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(76.0, body.y),
                egui::Layout::top_down(egui::Align::Center),
                |ui| self.rail(ui, icons, body.y),
            );

            if self.page == SubPage::DeviceSettings {
                self.device_settings(ui, icons, shared, &mut pending);
                return;
            }

            ui.allocate_ui_with_layout(
                Vec2::new(t::PANEL_WIDTH + 2.0 * t::PANEL_MARGIN, body.y),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    w::panel(ui, |ui| match self.page {
                        SubPage::Sensitivity => {
                            self.sensitivity_panel(ui, icons, shared, &mut pending)
                        }
                        SubPage::Assignments => {
                            self.assignments_panel(ui, icons, shared, &mut pending)
                        }
                        SubPage::Lightsync => self.lightsync_panel(ui, icons, shared, &mut pending),
                        SubPage::DeviceSettings => {}
                    });
                },
            );

            ui.allocate_ui_with_layout(
                ui.available_size(),
                egui::Layout::top_down(egui::Align::Center),
                |ui| self.stage(ui, shared, &mut pending),
            );
        });

        self.apply(shared, pending, busy);
    }

    fn header(
        &mut self,
        ui: &mut Ui,
        icons: &mut IconCache,
        shared: &Shared,
    ) -> Option<ghub_core::Id> {
        let mut picked = None;
        ui.horizontal(|ui| {
            if w::icon_button(ui, icons, Icon::ArrowSide, 40.0, 24.0, t::TEXT)
                .on_hover_text("Back to sensitivity")
                .clicked()
            {
                self.page = SubPage::Sensitivity;
            }
            ui.add_space(22.0);
            w::wordmark(ui, &self.device_display_name(shared));

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if w::icon_button(ui, icons, Icon::RailPerformance, 40.0, 26.0, t::TEXT)
                    .on_hover_text("Device settings")
                    .clicked()
                {
                    self.page = SubPage::DeviceSettings;
                }
                ui.add_space(18.0);
                picked = super::profile_pill(ui, icons, shared);
            });
        });
        picked
    }

    fn status(&mut self, ui: &mut Ui, icons: &mut IconCache, shared: &Shared) {
        ui.horizontal(|ui| {
            ui.add_space(88.0);
            ui.vertical(|ui| {
                status_band(ui, shared);
                if self.selected_device.is_none() {
                    w::body_small(
                        ui,
                        "No mouse detected. Connect the G903 by cable or switch it on with the \
                         LIGHTSPEED receiver plugged in.",
                        t::ERROR,
                    );
                }
                if self.mice.len() > 1 {
                    let keys: Vec<String> = self.mice.iter().map(|(key, _)| key.clone()).collect();
                    let labels = self.mice.clone();
                    let mut selected =
                        self.selected_device.clone().unwrap_or_else(|| keys[0].clone());
                    ui.set_max_width(320.0);
                    if c::combo(
                        ui,
                        icons,
                        "mouse device",
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
                        self.loaded = false;
                    }
                }
            });
        });
    }

    fn rail(&mut self, ui: &mut Ui, icons: &mut IconCache, height: f32) {
        let entries = [
            (SubPage::Sensitivity, Icon::RailDpi, "Sensitivity (DPI)"),
            (SubPage::Assignments, Icon::RailAssign, "Assignments"),
            (SubPage::Lightsync, Icon::RailLighting, "LIGHTSYNC"),
        ];
        for (page, icon, tip) in entries {
            if w::rail(ui, icons, icon, self.page == page, true).on_hover_text(tip).clicked() {
                self.page = page;
            }
        }
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

// =============================================================== Sensitivity

impl MousePage {
    fn sensitivity_panel(
        &mut self,
        ui: &mut Ui,
        icons: &mut IconCache,
        shared: &Shared,
        pending: &mut Pending,
    ) {
        let (min, max, step) = self.dpi_bounds(shared);
        let capability = self.dpi_capability(shared);
        scroll(ui, "mouse sensitivity", |ui| {
            w::title(ui, "Sensitivity (DPI)");
            ui.add_space(14.0);
            w::body_small(ui, &self.dpi_range(shared), t::TEXT_LABEL);
            ui.add_space(14.0);

            w::label(ui, "DPI STAGES");
            ui.add_space(6.0);

            let mut remove = None;
            let mut move_up = None;
            let mut make_default = None;
            for index in 0..self.stages.len() {
                w::card(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(Vec2::splat(18.0), egui::Sense::hover());
                        ui.painter().circle_filled(
                            rect.center(),
                            9.0,
                            StageRow::colour(index + 1),
                        );
                        ui.add_space(10.0);
                        w::body(ui, &format!("STAGE {}", index + 1));
                        if index + 1 == self.default_stage {
                            ui.add_space(6.0);
                            icons
                                .show(ui, Icon::StarSelected, 16.0, t::ACCENT_BRIGHT)
                                .on_hover_text("Default stage");
                        }
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if w::icon_button(ui, icons, Icon::Close, 18.0, 18.0, t::TEXT_LABEL)
                                    .on_hover_text("Remove this stage")
                                    .clicked()
                                {
                                    remove = Some(index);
                                }
                                ui.add_space(4.0);
                                if w::icon_button(
                                    ui,
                                    icons,
                                    Icon::StepperUp,
                                    18.0,
                                    18.0,
                                    t::TEXT_LABEL,
                                )
                                .on_hover_text("Move up")
                                .clicked()
                                {
                                    move_up = Some(index);
                                }
                            },
                        );
                    });

                    ui.add_space(8.0);
                    // G HUB's `dpi-slider` beside the typed value: drag for a rough value, type
                    // for an exact one. Both land on a value the sensor accepts.
                    {
                        let stage = &mut self.stages[index];
                        if let Some(dpi) = dpi_slider(ui, capability.as_ref(), stage.dpi, min, max, step) {
                            stage.dpi = dpi;
                            stage.buffer = dpi.to_string();
                        }
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let stage = &mut self.stages[index];
                        c::numeric(
                            ui,
                            icons,
                            &mut stage.buffer,
                            &mut stage.dpi,
                            min..=max,
                            step,
                            true,
                        );
                        ui.add_space(8.0);
                        c::checkbox(ui, icons, &mut self.stages[index].enabled, "On", true);
                        ui.add_space(4.0);
                        if w::link(ui, "DEFAULT", index + 1 != self.default_stage).clicked() {
                            make_default = Some(index);
                        }
                    });
                });
                ui.add_space(8.0);
            }

            if let Some(index) = remove {
                // At least one stage always remains: a mouse with no DPI stage has no DPI.
                if self.stages.len() > 1 {
                    self.stages.remove(index);
                    self.default_stage = self.default_stage.clamp(1, self.stages.len());
                }
            }
            if let Some(index) = move_up {
                if index > 0 {
                    self.stages.swap(index, index - 1);
                    // The default follows the stage it was on, not the position.
                    if self.default_stage == index + 1 {
                        self.default_stage = index;
                    } else if self.default_stage == index {
                        self.default_stage = index + 1;
                    }
                }
            }
            if let Some(index) = make_default {
                self.default_stage = index + 1;
            }

            if c::button_wide(ui, "Add stage", c::ButtonStyle::Outlined, self.stages.len() < 5)
                .clicked()
            {
                // `(last ?? 800) * 2`, clamped to what the sensor accepts.
                let next = self.stages.last().map_or(800, |stage| stage.dpi) * 2;
                let clamped = self
                    .dpi_capability(shared)
                    .map_or(next, |dpi| dpi.clamp(next as u32) as i64);
                self.stages.push(StageRow::new(clamped, true));
            }

            c::separator(ui);
            w::label(ui, "DPI SHIFT (SNIPER)");
            ui.add_space(6.0);
            if let Some(dpi) = dpi_slider(ui, capability.as_ref(), self.shift_dpi, min, max, step) {
                self.shift_dpi = dpi;
                self.shift_dpi_buffer = dpi.to_string();
            }
            ui.add_space(4.0);
            c::numeric(
                ui,
                icons,
                &mut self.shift_dpi_buffer,
                &mut self.shift_dpi,
                min..=max,
                step,
                true,
            );
            ui.add_space(4.0);
            w::body_small(
                ui,
                "DPI used while a 'DPI shift' binding is held; the stage DPI comes back on release.",
                t::TEXT_LABEL,
            );
            ui.add_space(10.0);
            c::checkbox(
                ui,
                icons,
                &mut self.show_dpi_notification,
                "Show a notification on stage change",
                true,
            );

            if self.rate_supported(shared) {
                ui.add_space(14.0);
                // G HUB's own report-rate glyph, from its system-tray asset set, beside the
                // heading - the one place it labels this setting with an icon.
                ui.horizontal(|ui| {
                    icons.show(ui, Icon::ReportRate, 16.0, t::TEXT_HEAD);
                    ui.add_space(6.0);
                    w::label(ui, "REPORT RATE");
                });
                let rates = self.rates.clone();
                let mut selected = self.selected_rate.unwrap_or_else(|| *rates.first().unwrap_or(&1000));
                c::combo(
                    ui,
                    icons,
                    "report rate",
                    &mut selected,
                    &rates,
                    |hz| format!("{hz} Hz"),
                    true,
                );
                // Stored whether or not it changed: without a device the profile may name a rate
                // the device does not offer, and the combo has already fallen back to one it does.
                self.selected_rate = Some(selected);
            }

            let (wheel, can_invert) = self.wheel_support(shared);
            if wheel {
                ui.add_space(14.0);
                w::label(ui, "SCROLL WHEEL");
                ui.add_space(6.0);
                c::checkbox(
                    ui,
                    icons,
                    &mut self.wheel_high_resolution,
                    "High-resolution scrolling",
                    true,
                );
                ui.add_space(4.0);
                w::body_small(
                    ui,
                    "HID++ 0x2121: the wheel reports finer steps to applications that support \
                     smooth scrolling.",
                    t::TEXT_LABEL,
                );
                ui.add_space(8.0);
                // Only offered when the device says it can: inverting in software would change
                // what every other application sees, which is not what this switch claims to do.
                c::checkbox(
                    ui,
                    icons,
                    &mut self.wheel_inverted,
                    "Invert scroll direction",
                    can_invert,
                );
            }

            c::separator(ui);
            w::label(ui, "LIVE SENSOR");
            ui.add_space(6.0);
            let supported = self.dpi_supported(shared);
            ui.horizontal(|ui| {
                c::numeric(
                    ui,
                    icons,
                    &mut self.live_dpi_buffer,
                    &mut self.live_dpi,
                    min..=max,
                    step,
                    supported,
                );
                ui.add_space(6.0);
                if c::button(ui, "Read", c::ButtonStyle::Outlined, supported && !shared.busy())
                    .on_hover_text("Read the current DPI from the sensor")
                    .clicked()
                {
                    pending.read_live = true;
                }
                ui.add_space(6.0);
                if c::button(ui, "Test", c::ButtonStyle::Outlined, supported && !shared.busy())
                    .on_hover_text(
                        "Volatile: sets the sensor now and reads it back; not stored in the profile",
                    )
                    .clicked()
                {
                    pending.write_live = true;
                }
            });

            ui.add_space(14.0);
            if c::button_wide(ui, "Save and apply", c::ButtonStyle::Primary, !shared.busy())
                .clicked()
            {
                pending.save_settings = true;
            }
            ui.add_space(10.0);
            w::body_small(
                ui,
                "Not exposed by this sensor: separate X/Y DPI, surface tuning, angle snapping, \
                 acceleration. Windows pointer speed is not sensor DPI and is never changed here.",
                t::TEXT_LABEL,
            );
        });
    }
}

// =============================================================== Assignments

impl MousePage {
    fn assignments_panel(
        &mut self,
        ui: &mut Ui,
        icons: &mut IconCache,
        shared: &Shared,
        pending: &mut Pending,
    ) {
        scroll(ui, "mouse assignments", |ui| {
            w::title(ui, "Assignments");
            ui.add_space(12.0);
            w::body(
                ui,
                "Click a button on the mouse, pick its action and apply. Buttons 6 and up are \
                 read live from the mouse, so if you are not sure which is which, bind one and \
                 press it.",
            );
            if !self.last_spy_button.is_empty() {
                ui.add_space(6.0);
                w::body_small(ui, &self.last_spy_button, t::ACCENT_BRIGHT);
            }

            ui.add_space(12.0);
            if c::checkbox(ui, icons, &mut self.edit_shift_layer, "Edit the G-Shift layer", true) {
                // Only the slot names and descriptions change with the layer, but they come from
                // the same loader as the callout geometry, so it is rerun whole.
                self.loaded = false;
            }
            ui.add_space(4.0);
            w::body_small(
                ui,
                "Active while a 'G-Shift (hold)' control is held",
                t::TEXT_LABEL,
            );

            c::separator(ui);
            let selected = self.selected_binding.and_then(|index| self.bindings.get(index));
            w::label(
                ui,
                selected.map_or("SELECT A BUTTON", |row| row.label.as_str()),
            );
            ui.add_space(4.0);
            if let Some(row) = selected {
                w::body_small(ui, &row.site, t::TEXT_LABEL);
                if let Some(reason) = &row.disabled_reason {
                    ui.add_space(6.0);
                    w::callout_warning(ui, |ui| w::body_small(ui, reason, t::TEXT));
                }
            }

            ui.add_space(10.0);
            let assignable = selected.is_some_and(|row| row.assignable);
            self.editor.show(ui, icons, assignable);

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if c::button(
                    ui,
                    "Apply",
                    c::ButtonStyle::Primary,
                    assignable && !shared.busy() && !self.confirm.is_open(),
                )
                .clicked()
                {
                    pending.apply_binding = true;
                }
                ui.add_space(6.0);
                if c::button(
                    ui,
                    "Reset to default",
                    c::ButtonStyle::Outlined,
                    assignable && !shared.busy(),
                )
                .clicked()
                {
                    pending.clear_binding = true;
                }
            });
        });
    }
}

// =============================================================== LIGHTSYNC

impl MousePage {
    fn lightsync_panel(
        &mut self,
        ui: &mut Ui,
        icons: &mut IconCache,
        shared: &Shared,
        pending: &mut Pending,
    ) {
        scroll(ui, "mouse lightsync", |ui| {
            w::title(ui, "LIGHTSYNC");
            ui.add_space(14.0);
            if !self.lighting_supported(shared) {
                w::body_small(ui, "This device reports no lighting.", t::ERROR);
                ui.add_space(10.0);
            }

            w::label(ui, "EFFECT");
            c::combo(
                ui,
                icons,
                "mouse lighting mode",
                &mut self.lighting_mode,
                LIGHTING_MODES,
                |mode| format!("{mode:?}"),
                true,
            );
            ui.add_space(14.0);

            ui.horizontal(|ui| {
                w::label(ui, "COLOR");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if w::swatch(ui, self.current_colour, 24.0, self.picker_open)
                        .on_hover_text("Select every zone and open the picker")
                        .clicked()
                    {
                        // `SelectAllZones`: the bubble selects everything, because the common case
                        // is wanting the whole mouse one colour.
                        for zone in &mut self.zones {
                            zone.selected = true;
                        }
                        self.picker_open = true;
                    }
                });
            });

            if self.picker_open {
                ui.add_space(12.0);
                if self.picker.show(ui, icons, &mut self.current_colour) {
                    self.recolour_selected_zones();
                }
                self.eyedropper |= self.picker.eyedropper;
                self.picker.eyedropper = false;

                ui.add_space(12.0);
                let names: Vec<String> =
                    PALETTES.iter().map(|(name, _)| (*name).to_owned()).collect();
                c::combo(
                    ui,
                    icons,
                    "mouse palette",
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
                            self.recolour_selected_zones();
                        }
                        ui.add_space(9.0);
                    }
                });
            }

            ui.add_space(14.0);
            w::label(ui, "ZONES");
            ui.add_space(6.0);
            let mut select = None;
            for index in 0..self.zones.len() {
                ui.horizontal(|ui| {
                    let response = w::row(ui, &self.zones[index].name.clone());
                    if response.clicked() {
                        select = Some(index);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let colour = self.zones[index].colour();
                        w::swatch(ui, colour, 22.0, self.zones[index].selected);
                        ui.add_space(8.0);
                        if c::text_field(ui, &mut self.zones[index].hex_buffer, 100.0, true) {
                            let typed = self.zones[index].hex_buffer.clone();
                            match parse_hex(&typed) {
                                Some(parsed) => {
                                    self.zones[index].hex = to_hex(parsed);
                                    self.zones[index].hex_buffer =
                                        self.zones[index].hex.clone();
                                }
                                // Reverts rather than becoming black, as the picker's own text
                                // field does.
                                None => {
                                    self.zones[index].hex_buffer =
                                        self.zones[index].hex.clone();
                                }
                            }
                        }
                    });
                });
                ui.add_space(2.0);
            }
            if let Some(index) = select {
                self.select_zone(index);
            }

            ui.add_space(14.0);
            w::label(ui, "BRIGHTNESS");
            c::slider(ui, &mut self.brightness, 0.0..=100.0, true);

            ui.add_space(14.0);
            w::label(ui, "DEVICE EFFECT");
            if self.device_effects.is_empty() {
                w::body_small(ui, "This device reports no firmware effects.", t::TEXT_LABEL);
            } else {
                let ids: Vec<u16> = self.device_effects.iter().map(|(_, id)| *id).collect();
                let names = self.device_effects.clone();
                let mut selected = self.device_effect.unwrap_or(ids[0]);
                c::combo(
                    ui,
                    icons,
                    "mouse device effect",
                    &mut selected,
                    &ids,
                    |id| {
                        names
                            .iter()
                            .find(|(_, candidate)| candidate == id)
                            .map_or_else(|| format!("Effect 0x{id:04X}"), |(name, _)| name.clone())
                    },
                    true,
                );
                self.device_effect = Some(selected);
            }
            ui.add_space(8.0);
            c::field_row(ui, "Period ms", |ui| {
                c::numeric(
                    ui,
                    icons,
                    &mut self.effect_period_buffer,
                    &mut self.effect_period,
                    100..=60000,
                    500,
                    true,
                )
            });

            c::separator(ui);
            self.power_save.show(ui, icons, true);
            if self.power_save.show_read(ui, !shared.busy()) {
                pending.read_power_save = true;
            }

            ui.add_space(12.0);
            if c::button_wide(ui, "Save", c::ButtonStyle::Primary, !shared.busy()).clicked() {
                pending.save_lighting = true;
            }
            ui.add_space(10.0);
            w::body_small(
                ui,
                "Untouched leaves the device's own lighting alone. Artemis hands the device to \
                 the profile editor. Static and Device effect are written by this app and \
                 re-applied on reconnect.",
                t::TEXT_LABEL,
            );
        });
    }

    /// `OnCurrentColorChanged` - a picker edit recolours every selected zone live.
    fn recolour_selected_zones(&mut self) {
        let hex = to_hex(self.current_colour);
        for zone in self.zones.iter_mut().filter(|z| z.selected) {
            zone.hex = hex.clone();
            zone.hex_buffer = hex.clone();
        }
    }

    /// `SelectZone` - one zone, its colour loaded into the picker so editing it recolours it.
    fn select_zone(&mut self, index: usize) {
        for (candidate, zone) in self.zones.iter_mut().enumerate() {
            zone.selected = candidate == index;
        }
        let colour = self.zones[index].colour();
        self.set_colour_quietly(colour);
        self.picker_open = true;
    }

    /// `SetColorQuietly`.
    fn set_colour_quietly(&mut self, colour: Color32) {
        self.current_colour = colour;
        self.picker.set(colour);
    }
}

// =============================================================== the stage

impl MousePage {
    fn stage(&mut self, ui: &mut Ui, shared: &mut Shared, pending: &mut Pending) {
        let _ = pending;
        let ctx = ui.ctx().clone();
        let render = self.render_size(shared, &ctx);
        let canvas = self.stage_size(render);

        w::stage(ui, |ui| {
            match self.page {
                SubPage::Lightsync => {
                    w::title(ui, &self.selection_info());
                }
                SubPage::Sensitivity => {
                    w::title(ui, &format!("CONNECTED VIA {}", self.connection(shared)));
                }
                _ => {}
            }
            ui.add_space(10.0);

            let available = ui.available_size();
            let box_size =
                Vec2::new(available.x.max(1.0), available.y.clamp(1.0, STAGE_MAX_HEIGHT));
            let (frame, response) = ui.allocate_exact_size(box_size, egui::Sense::click());
            let projection = Projection::new(frame, canvas);

            // The render, placed inside the gutters, with its magnetic side plates on top.
            if let Some(artwork) = shared.artwork.get(&ctx, self.artwork_folder) {
                let into = projection.project(Rect::from_min_size(
                    pos2(GUTTER as f32, TOP_GUTTER as f32),
                    Vec2::new(render.0 as f32, render.1 as f32),
                ));
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                ui.painter().image(artwork.texture.id(), into, uv, Color32::WHITE);
                for cover in &artwork.covers {
                    let at = projection.project(Rect::from_min_size(
                        pos2(
                            (cover.position.x + GUTTER) as f32,
                            (cover.position.y + TOP_GUTTER) as f32,
                        ),
                        Vec2::new(cover.position.width as f32, cover.position.height as f32),
                    ));
                    ui.painter().image(cover.texture.id(), at, uv, Color32::WHITE);
                }
            }

            match self.page {
                SubPage::Lightsync => self.draw_zones(ui, &projection, &response),
                SubPage::Assignments => self.draw_callouts(ui, &projection, &response),
                _ => {}
            }

            ui.add_space(10.0);
            match self.page {
                SubPage::Lightsync => {
                    w::label(ui, "CLICK A ZONE ON THE MOUSE TO RECOLOUR IT");
                }
                SubPage::Assignments => {
                    ui.horizontal(|ui| {
                        if c::toggle_switch_labelled(
                            ui,
                            &mut self.edit_shift_layer,
                            "DEFAULT",
                            "G-SHIFT",
                            true,
                        ) {
                            self.loaded = false;
                        }
                    });
                }
                _ => {}
            }
        });
    }

    fn draw_zones(&mut self, ui: &mut Ui, projection: &Projection, response: &egui::Response) {
        for zone in &self.zones {
            let rect = projection.project(zone.rect);
            // `CornerRadius="24"` and `Opacity="0.5"` in render units, so the radius scales too.
            let radius = CornerRadius::same((24.0 * projection.scale).clamp(1.0, 24.0) as u8);
            ui.painter().rect_filled(rect, radius, zone.colour().gamma_multiply(0.5));
            if zone.selected {
                ui.painter().rect_stroke(
                    rect.expand(2.0),
                    radius,
                    Stroke::new((6.0 * projection.scale).max(1.5), t::ACCENT_BRIGHT),
                    StrokeKind::Outside,
                );
            }
        }

        if !response.clicked() {
            return;
        }
        let Some(at) = response
            .interact_pointer_pos()
            .and_then(|screen| projection.unproject(screen))
        else {
            return;
        };
        let hit = self.zones.iter().position(|zone| zone.rect.contains(at));
        let additive = ui.input(|i| i.modifiers.ctrl || i.modifiers.shift);

        match hit {
            None => {
                if !additive {
                    for zone in &mut self.zones {
                        zone.selected = false;
                    }
                    self.picker_open = false;
                }
            }
            Some(index) if self.eyedropper => {
                let colour = self.zones[index].colour();
                self.set_colour_quietly(colour);
                self.eyedropper = false;
            }
            Some(index) if additive => {
                self.zones[index].selected = !self.zones[index].selected;
            }
            Some(index) => self.select_zone(index),
        }
    }

    fn draw_callouts(&mut self, ui: &mut Ui, projection: &Projection, response: &egui::Response) {
        let pointer = response.hover_pos();
        let mut hovered = None;
        let mut clicked = None;

        for (index, row) in self.bindings.iter().enumerate() {
            let callout = projection.project(row.callout_rect());
            let marker = projection.project(Rect::from_center_size(
                row.marker,
                Vec2::splat(MARKER_DIAMETER as f32),
            ));
            let selected = self.selected_binding == Some(index);
            let is_hovered = pointer.is_some_and(|p| callout.contains(p));
            if is_hovered {
                hovered = Some(index);
                if response.clicked() {
                    clicked = Some(index);
                }
            }

            // The leader line first, so the callout and the dot sit on top of it.
            ui.painter().line_segment(
                [
                    projection.project(Rect::from_min_size(row.leader_start(), Vec2::ZERO)).min,
                    marker.center(),
                ],
                Stroke::new((4.0 * projection.scale).max(1.0), t::PANEL_BORDER),
            );

            // `Fill="{GHubAccent}" Stroke="White" StrokeThickness="5"`.
            ui.painter().circle_filled(marker.center(), marker.width() / 2.0, t::ACCENT);
            ui.painter().circle_stroke(
                marker.center(),
                marker.width() / 2.0,
                Stroke::new((5.0 * projection.scale).max(1.0), Color32::WHITE),
            );

            let border = if selected { t::ACCENT_BRIGHT } else { t::PANEL_BORDER };
            let radius = CornerRadius::same(t::RADIUS_PILL);
            ui.painter().rect_filled(
                callout,
                radius,
                if is_hovered { t::HOVER } else { t::PANEL },
            );
            ui.painter().rect_stroke(
                callout,
                radius,
                Stroke::new(if selected { 2.0 } else { 1.0 }, border),
                StrokeKind::Inside,
            );

            // `FontSize="46"` and `"40"` in render units: the callout text scales with the stage,
            // which is what the Viewbox did.
            let pad = (24.0 * projection.scale).max(4.0);
            let title_size = (46.0 * projection.scale).clamp(7.0, 18.0);
            let body_size = (40.0 * projection.scale).clamp(6.0, 15.0);
            let label = if row.assignable { t::TEXT } else { t::DISABLED };
            ui.painter().text(
                pos2(callout.left() + pad, callout.top() + pad),
                egui::Align2::LEFT_TOP,
                &row.label,
                egui::FontId::new(title_size, t::family(t::BODY_BOLD)),
                label,
            );
            ui.painter().text(
                pos2(callout.left() + pad, callout.top() + pad + title_size * 1.3),
                egui::Align2::LEFT_TOP,
                &row.description,
                egui::FontId::new(body_size, t::family(t::BODY)),
                Color32::from_rgb(0x8A, 0x8D, 0x93),
            );
        }

        if let Some(index) = hovered {
            response.clone().on_hover_text(self.bindings[index].tip());
        }
        if let Some(index) = clicked {
            // `EditBinding`: picking a control loads whatever is bound to it, so Apply replaces it
            // rather than starting from blank.
            self.selected_binding = Some(index);
            self.pending_editor_load = true;
        }
    }
}

// =============================================================== Device Settings

impl MousePage {
    fn device_settings(
        &mut self,
        ui: &mut Ui,
        icons: &mut IconCache,
        shared: &mut Shared,
        pending: &mut Pending,
    ) {
        let onboard = self.onboard_capability(shared);
        let writes_enabled = self.onboard_writes_enabled(shared);
        let hint = self.onboard_write_hint(shared);
        let name = self.device_display_name(shared);
        let firmware = self.firmware_version(shared);
        let battery = self.battery_state(shared);
        let connection = self.connection(shared);
        let busy = shared.busy();

        scroll(ui, "mouse device settings", |ui| {
            ui.vertical_centered(|ui| {
                ui.set_max_width(760.0);
                w::wordmark(ui, &name);
                ui.add_space(18.0);

                let ctx = ui.ctx().clone();
                if let Some(artwork) = shared.artwork.get(&ctx, self.artwork_folder) {
                    // `Height="300" Opacity="0.75"`.
                    let size = Vec2::new(300.0 * artwork.width() / artwork.height(), 300.0);
                    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                    ui.painter().image(
                        artwork.texture.id(),
                        rect,
                        uv,
                        Color32::WHITE.gamma_multiply(0.75),
                    );
                }

                for (caption, value) in [
                    ("FIRMWARE VERSION", firmware.as_str()),
                    ("BATTERY", battery.as_str()),
                    ("CONNECTION", connection),
                ] {
                    ui.add_space(18.0);
                    w::label(ui, caption);
                    ui.add_space(4.0);
                    w::body(ui, value);
                }

                ui.add_space(18.0);
                c::separator(ui);

                let Some(onboard) = onboard else {
                    w::body_small(
                        ui,
                        "This mouse reports no onboard profile memory.",
                        t::TEXT_LABEL,
                    );
                    return;
                };

                w::label(ui, "ON-BOARD MEMORY");
                ui.add_space(6.0);
                w::body_small(ui, hint, t::TEXT_LABEL);
                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    if c::button(ui, "Read onboard slots", c::ButtonStyle::Outlined, !busy)
                        .clicked()
                    {
                        pending.read_onboard = true;
                    }
                    ui.add_space(8.0);
                    w::body(ui, "Slot");
                    ui.add_space(8.0);
                    c::numeric(
                        ui,
                        icons,
                        &mut self.onboard_slot_buffer,
                        &mut self.onboard_slot,
                        1..=onboard.slot_count as i64,
                        1,
                        true,
                    );
                    ui.add_space(8.0);
                    if c::button(ui, "Activate slot", c::ButtonStyle::Outlined, !busy)
                        .on_hover_text(
                            "Selects the onboard slot (volatile selection, no memory write)",
                        )
                        .clicked()
                    {
                        pending.activate_slot = true;
                    }
                });

                let readback = self.readback.lock();
                let (info, profiles, plan_text, has_plan) = match &readback {
                    Ok(r) => (
                        r.onboard_info.clone(),
                        r.onboard_profiles.clone(),
                        r.plan_text.clone(),
                        r.plan.as_ref().is_some_and(OnboardWritePlan::has_changes),
                    ),
                    Err(_) => (String::new(), Vec::new(), String::new(), false),
                };
                drop(readback);

                if !info.is_empty() {
                    ui.add_space(10.0);
                    w::body_small(ui, &info, t::TEXT_ROW);
                }
                for line in &profiles {
                    ui.add_space(4.0);
                    w::body_small(ui, line, t::TEXT_LABEL);
                }

                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if c::button(ui, "Preview Save to Device", c::ButtonStyle::Outlined, !busy)
                        .on_hover_text("DPI stages and report rate \u{2192} the selected onboard slot")
                        .clicked()
                    {
                        pending.plan_save = true;
                    }
                    ui.add_space(8.0);
                    // Both conditions, and both for a reason: nothing to write, or writing is off.
                    if c::button(
                        ui,
                        "Save to device\u{2026}",
                        c::ButtonStyle::Primary,
                        writes_enabled && has_plan && !busy && !self.confirm.is_open(),
                    )
                    .clicked()
                    {
                        pending.commit_save = true;
                    }
                });

                if !plan_text.is_empty() {
                    ui.add_space(10.0);
                    w::card(ui, |ui| w::body_small(ui, &plan_text, t::TEXT_ROW));
                }

                ui.add_space(18.0);
                c::separator(ui);
                w::body_small(
                    ui,
                    "This application drives the mouse over HID++ directly; G HUB does not need \
                     to be installed or running.",
                    t::TEXT_LABEL,
                );
            });
        });
    }
}

// =============================================================== applying

impl MousePage {
    /// Routes the modal's answer to whichever action asked it.
    fn answer_confirmation(
        &mut self,
        shared: &mut Shared,
        ctx: &egui::Context,
        icons: &mut IconCache,
    ) {
        let Some(answer) = self.confirm.show(ctx, icons) else { return };
        let asking = self.confirming.take();
        if !answer {
            return;
        }
        match asking {
            Some(Confirming::PrimaryRemap) => self.write_binding(shared),
            Some(Confirming::OnboardWrite) => self.commit_onboard(shared),
            None => {}
        }
    }

    fn apply(&mut self, shared: &mut Shared, pending: Pending, busy: bool) {
        if std::mem::take(&mut self.pending_editor_load) {
            self.load_editor(shared);
        }

        let Some(device_key) = self.selected_device.clone() else { return };
        let capability = self.dpi_capability(shared);

        if pending.read_live && !busy {
            let manager = Arc::clone(&shared.host.devices);
            let key = device_key.clone();
            let readback = Arc::clone(&self.readback);
            shared.attempt("Read DPI", move || {
                let read = manager
                    .with_session(&key, |session| {
                        session.dpi().map(|dpi| dpi.get().map_err(|e| e.to_string()))
                    })
                    .flatten()
                    .ok_or_else(|| "The device does not expose DPI".to_owned())?;
                let state = read?;
                if let Ok(mut slot) = readback.lock() {
                    slot.live_dpi = Some(state.current);
                }
                Ok(format!(
                    "Device reports {} DPI (default {})",
                    state.current, state.default
                ))
            });
        }

        if pending.write_live && !busy {
            let manager = Arc::clone(&shared.host.devices);
            let key = device_key.clone();
            let readback = Arc::clone(&self.readback);
            let target = capability
                .as_ref()
                .map_or(self.live_dpi as u32, |dpi| dpi.clamp(self.live_dpi.max(0) as u32));
            shared.attempt("Set DPI", move || {
                let written = manager
                    .with_session(&key, |session| {
                        session.dpi().map(|dpi| dpi.set(target).map_err(|e| e.to_string()))
                    })
                    .flatten()
                    .ok_or_else(|| "The device does not expose DPI".to_owned())?;
                let read_back = written?;
                if let Ok(mut slot) = readback.lock() {
                    slot.live_dpi = Some(read_back);
                }
                // The read-back is stated separately from the target, because a sensor that
                // rounds to its own grid answers with a different number and the user needs to
                // see which one the mouse is actually using.
                Ok(format!(
                    "DPI set to {target}; device read-back {read_back} (volatile, not saved to \
                     profile)"
                ))
            });
        }

        if pending.save_settings {
            self.save_settings(shared);
        }
        if pending.save_lighting {
            self.save_lighting(shared);
        }

        if pending.read_power_save && !busy {
            let manager = Arc::clone(&shared.host.devices);
            let key = device_key.clone();
            let state = self.power_save.state_cell();
            shared.attempt("Read lighting power timers", move || {
                power_save::read_device(&manager, &key, &state)
            });
        }

        if pending.apply_binding {
            self.apply_binding(shared);
        }

        if pending.clear_binding {
            if let Some(slot) =
                self.selected_binding.and_then(|index| self.bindings.get(index)).map(|row| row.slot.clone())
            {
                let key = device_key.clone();
                shared.edit_profile("Binding removed", move |profile| {
                    profile.for_device(&key).bindings.remove(&slot);
                });
            }
        }

        if pending.read_onboard && !busy {
            self.read_onboard(shared);
        }

        if pending.activate_slot && !busy {
            let manager = Arc::clone(&shared.host.devices);
            let key = device_key.clone();
            let slot = self.onboard_slot.max(1) as usize;
            shared.attempt("Activate onboard slot", move || {
                let active = manager
                    .with_session(&key, |session| {
                        session
                            .onboard()
                            .map(|onboard| onboard.set_active_slot(slot).map_err(|e| e.to_string()))
                    })
                    .flatten()
                    .ok_or_else(|| "The device has no onboard profiles".to_owned())?;
                Ok(format!(
                    "Device active slot now {} (volatile selection)",
                    active?
                ))
            });
        }

        if pending.plan_save && !busy {
            self.plan_onboard(shared);
        }

        if pending.commit_save && !busy {
            self.ask_onboard_confirmation();
        }
    }

    /// `SaveSettingsAsync`.
    fn save_settings(&mut self, shared: &mut Shared) {
        let Some(device_key) = self.selected_device.clone() else { return };
        let capability = self.dpi_capability(shared);
        let stages: Vec<DpiStage> = self
            .stages
            .iter()
            .map(|stage| {
                let dpi = stage.dpi.max(0) as u32;
                DpiStage {
                    dpi: capability.as_ref().map_or(dpi, |c| c.clamp(dpi)),
                    enabled: stage.enabled,
                }
            })
            .collect();

        // Refused rather than silently fixed: every stage off means the DPI bindings have nothing
        // to step through, and quietly enabling one would be inventing a setting.
        if stages.iter().all(|stage| !stage.enabled) {
            shared.error = Some("At least one DPI stage must be enabled".to_owned());
            return;
        }

        let default_stage = (self.default_stage.max(1) - 1).min(stages.len() - 1);
        let shift_dpi = capability
            .as_ref()
            .map_or(self.shift_dpi.max(0) as u32, |c| c.clamp(self.shift_dpi.max(0) as u32));
        let report_rate = self.selected_rate;
        let notify = self.show_dpi_notification;
        let (wheel_supported, can_invert) = self.wheel_support(shared);
        let high_resolution = self.wheel_high_resolution;
        let inverted = self.wheel_inverted;

        let manager = Arc::clone(&shared.host.devices);
        let service = Arc::clone(shared.service());
        let key = device_key.clone();
        let apply_key = device_key.clone();
        shared.edit_profile("Mouse settings", move |profile| {
            let mouse = profile
                .for_device(&key)
                .mouse
                .get_or_insert_with(MouseSettings::default);
            mouse.dpi_stages = stages;
            mouse.default_stage = default_stage;
            // The active stage follows the default on save, which is what makes "Save and apply"
            // actually change the pointer rather than only the stored default.
            mouse.active_stage = default_stage;
            mouse.shift_dpi = Some(shift_dpi);
            mouse.report_rate_hz = report_rate;
            mouse.show_dpi_notification = notify;
            if wheel_supported {
                mouse.wheel_high_resolution = Some(high_resolution);
                mouse.wheel_inverted = Some(can_invert && inverted);
            }
        });

        // `Status += " · " + ApplyToDeviceAsync(...)`: a second job, so the save is reported even
        // if applying it to the hardware fails.
        shared.attempt("Apply to device", move || {
            let Some(device) = manager.get(&apply_key) else {
                return Ok("Device not connected; the profile was saved".to_owned());
            };
            let results = service.apply_to_device(&device);
            Ok(if results.is_empty() {
                "Nothing to apply for this device".to_owned()
            } else {
                results.join(" \u{b7} ")
            })
        });
    }

    /// `SaveLightingAsync`.
    fn save_lighting(&mut self, shared: &mut Shared) {
        let Some(device_key) = self.selected_device.clone() else { return };
        let mode = self.lighting_mode;
        let brightness = (self.brightness / 100.0).clamp(0.0, 1.0);
        let zones: Vec<(String, String)> = self
            .zones
            .iter()
            .map(|zone| (zone.key(), normalise_hex(&zone.hex)))
            .collect();
        // The first zone's colour doubles as the default, which is what an effect that takes one
        // colour is handed.
        let default_color = normalise_hex(
            self.zones.first().map_or("#FFFFFF", |zone| zone.hex.as_str()),
        );
        let effect = self.device_effect;
        let period = self.effect_period.clamp(100, 60000) as u16;
        let power_save = self.power_save.snapshot();

        shared.edit_profile("Mouse lighting", move |profile| {
            let lighting = &mut profile.for_device(&device_key).lighting;
            lighting.mode = mode;
            lighting.brightness = brightness;
            for (key, hex) in zones {
                lighting.static_colors.insert(key, hex);
            }
            lighting.default_color = default_color;
            lighting.device_effect_id = effect;
            lighting.device_effect_period_ms = period;
            power_save.store(lighting);
        });
    }

    /// `ApplyBindingAsync` - with the primary-button guard in front of it.
    fn apply_binding(&mut self, shared: &mut Shared) {
        let Some(row) = self.selected_binding.and_then(|index| self.bindings.get(index)) else {
            return;
        };
        if !row.assignable {
            return;
        }
        let Some(action) = self.editor.build() else {
            // The editor sets its own error, which the panel already shows.
            return;
        };

        // Remapping the left button away from "left click" is the one binding that can leave the
        // user unable to undo it. The guard is the same one the service enforces: if nothing else
        // clicks, the left button keeps clicking.
        let primary = row.slot.starts_with("button:Left");
        let is_left_click =
            matches!(action, ActionDefinition::MouseButton { button: MouseButtonKind::Left });
        if primary && !is_left_click && !self.has_primary_recovery(shared) {
            self.confirming = Some(Confirming::PrimaryRemap);
            self.confirm.ask(
                "Remap the primary button?",
                "Assign 'Left click' to another button first. Without that, the left button keeps \
                 clicking normally (safety guard) and this binding is stored but inactive.",
                "Store anyway",
            );
            return;
        }
        self.write_binding(shared);
    }

    /// Whether some other control on this device is bound to a left click.
    fn has_primary_recovery(&self, shared: &Shared) -> bool {
        self.selected_device
            .as_deref()
            .and_then(|key| shared.active_profile().device(key).cloned())
            .is_some_and(|settings| {
                settings.bindings.values().any(|binding| {
                    matches!(
                        binding.action,
                        ActionDefinition::MouseButton { button: MouseButtonKind::Left }
                    )
                })
            })
    }

    fn write_binding(&mut self, shared: &mut Shared) {
        let Some(device_key) = self.selected_device.clone() else { return };
        let Some(row) = self.selected_binding.and_then(|index| self.bindings.get(index)) else {
            return;
        };
        let Some(action) = self.editor.build() else { return };
        let what = format!("{} \u{2192} {}", row.label, action.describe());
        let slot = row.slot.clone();
        shared.edit_profile(what, move |profile| {
            profile
                .for_device(&device_key)
                .bindings
                .insert(slot, ActionBinding::new(action));
        });
    }

    /// `ReadOnboardAsync`.
    fn read_onboard(&mut self, shared: &mut Shared) {
        let Some(device_key) = self.selected_device.clone() else { return };
        let manager = Arc::clone(&shared.host.devices);
        let readback = Arc::clone(&self.readback);
        shared.attempt("Read onboard memory", move || {
            let read = manager
                .with_session(&device_key, |session| {
                    let onboard = session.onboard()?;
                    let mode = onboard.get_mode().map_err(|e| e.to_string());
                    let slot = onboard.get_active_slot().map_err(|e| e.to_string());
                    let profiles = onboard.read_profiles().map_err(|e| e.to_string());
                    Some((mode, slot, profiles, onboard.capability.clone()))
                })
                .flatten()
                .ok_or_else(|| "The device has no onboard profiles".to_owned())?;

            let (mode, slot, profiles, capability) = read;
            let mode = mode?;
            let slot = slot?;
            let profiles = profiles?;

            let info = format!(
                "Mode: {mode:?} ({}) \u{b7} active slot: {} \u{b7} {} slots, {} buttons, format {}",
                if mode == OnboardMode::Host {
                    "host software controls settings"
                } else {
                    "device runs its stored profile"
                },
                if slot == 0 { "none (host mode)".to_owned() } else { slot.to_string() },
                capability.slot_count,
                capability.button_count,
                capability.profile_format
            );

            let lines: Vec<String> = profiles
                .iter()
                .map(|profile| {
                    format!(
                        "Slot {}{}{}{} '{}': {} Hz, DPI {} (default {}, shift {}) \u{b7} buttons: {}",
                        profile.slot,
                        if profile.enabled { "" } else { " (disabled)" },
                        if profile.from_rom { " [ROM defaults]" } else { "" },
                        if profile.crc_valid { "" } else { " [CRC INVALID]" },
                        profile.name,
                        profile.report_rate_hz,
                        profile
                            .dpi_stages
                            .iter()
                            .map(u32::to_string)
                            .collect::<Vec<_>>()
                            .join("/"),
                        profile.default_dpi_index + 1,
                        profile.shift_dpi_index + 1,
                        profile
                            .buttons
                            .iter()
                            .enumerate()
                            .map(|(i, button)| format!("{}={}", i + 1, button.describe()))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
                .collect();

            let count = lines.len();
            if let Ok(mut slot) = readback.lock() {
                slot.onboard_info = info;
                slot.onboard_profiles = lines;
            }
            Ok(format!("Read {count} onboard slots"))
        });
    }

    /// `PlanSaveAsync` - builds the diff the user will confirm. Reads only.
    fn plan_onboard(&mut self, shared: &mut Shared) {
        let Some(device_key) = self.selected_device.clone() else { return };
        let capability = self.dpi_capability(shared);
        let stages: Vec<u32> = self
            .stages
            .iter()
            .filter(|stage| stage.enabled)
            .map(|stage| {
                let dpi = stage.dpi.max(0) as u32;
                capability.as_ref().map_or(dpi, |c| c.clamp(dpi))
            })
            .collect();
        if stages.is_empty() {
            shared.error = Some("At least one DPI stage must be enabled".to_owned());
            return;
        }
        let default_index = (self.default_stage.max(1) - 1).min(stages.len() - 1);
        let slot = self.onboard_slot.max(1) as usize;
        let report_rate = self.selected_rate;

        let manager = Arc::clone(&shared.host.devices);
        let readback = Arc::clone(&self.readback);
        shared.attempt("Preview Save to Device", move || {
            let edit = OnboardEdit {
                slot,
                dpi_stages: Some(stages),
                default_dpi_index: Some(default_index),
                report_rate_hz: report_rate,
                ..OnboardEdit::default()
            };
            let planned = manager
                .with_session(&device_key, |session| {
                    let onboard = session.onboard()?;
                    Some(
                        OnboardProfileWriter::new(&onboard)
                            .plan(&edit)
                            .map_err(|e| e.to_string()),
                    )
                })
                .flatten()
                .ok_or_else(|| "The device has no onboard profiles".to_owned())?;
            let plan = planned?;

            let text = if plan.has_changes() {
                plan.changes
                    .iter()
                    .map(|change| format!("{}: {} \u{2192} {}", change.field, change.before, change.after))
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                "No differences: the slot already matches.".to_owned()
            };
            if let Ok(mut slot) = readback.lock() {
                slot.plan_text = text;
                slot.plan = Some(plan);
            }
            Ok(format!("Preview ready for slot {slot}"))
        });
    }

    fn ask_onboard_confirmation(&mut self) {
        let Ok(readback) = self.readback.lock() else { return };
        let Some(plan) = readback.plan.as_ref().filter(|plan| plan.has_changes()) else { return };
        let body = format!(
            "Write these changes to onboard slot {}?\n\n{}\n\nA backup of all readable onboard \
             memory is stored first; the write is verified by reading it back.",
            plan.slot, readback.plan_text
        );
        drop(readback);
        self.confirming = Some(Confirming::OnboardWrite);
        self.confirm.ask("Save to device", &body, "Save to device");
    }

    /// `CommitSaveAsync` - snapshot, write, verify by read-back, roll back on mismatch.
    ///
    /// The snapshot is persisted *before* the write and the commit refuses to run without one, so
    /// a failed write always has something to restore from.
    fn commit_onboard(&mut self, shared: &mut Shared) {
        let Some(device_key) = self.selected_device.clone() else { return };
        let Some(device) = self.device(shared) else { return };
        let plan = match self.readback.lock() {
            Ok(readback) => readback.plan.clone(),
            Err(_) => None,
        };
        let Some(plan) = plan.filter(OnboardWritePlan::has_changes) else { return };

        let manager = Arc::clone(&shared.host.devices);
        let service = Arc::clone(shared.service());
        let readback = Arc::clone(&self.readback);
        let name = device
            .descriptor
            .as_ref()
            .map_or_else(String::new, |d| d.display_name.clone());
        let firmware = device.descriptor.and_then(|d| d.firmware_version);

        shared.attempt("Save to device", move || {
            let written = manager
                .with_session(&device_key, |session| {
                    let onboard = session.onboard()?;
                    let writer = OnboardProfileWriter::new(&onboard);
                    let snapshot = match writer.snapshot(&device_key, &name, firmware.as_deref()) {
                        Ok(snapshot) => snapshot,
                        Err(e) => return Some(Err(e.to_string())),
                    };
                    // Persisted first: the commit refuses a plan with no backup on disk. The
                    // store's own type carries the time it was taken, which the wire form has no
                    // field for; a backup whose age is unknown is hard to choose between.
                    if let Err(e) = service.store().save_snapshot(&persisted(&snapshot)) {
                        return Some(Err(e.to_string()));
                    }
                    Some(
                        writer
                            .commit(&plan, &snapshot)
                            .map_err(|e| e.to_string())
                            .map(|profile| (profile, service.store().snapshot_directory())),
                    )
                })
                .flatten()
                .ok_or_else(|| "The device has no onboard profiles".to_owned())?;
            let (profile, backup) = written?;

            if let Ok(mut slot) = readback.lock() {
                slot.plan = None;
                slot.plan_text.clear();
            }
            Ok(format!(
                "Slot {} written and verified by read-back: DPI {}, {} Hz. Backup: {}",
                profile.slot,
                profile.dpi_stages.iter().map(u32::to_string).collect::<Vec<_>>().join("/"),
                profile.report_rate_hz,
                backup.display()
            ))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> MousePage {
        MousePage::default()
    }

    #[test]
    fn the_stage_palette_is_g_hubs_five_and_cycles_past_them() {
        assert_eq!(STAGE_PALETTE.len(), 5);
        for hex in STAGE_PALETTE {
            assert!(parse_hex(hex).is_some(), "{hex} does not parse");
        }
        // Stage 1 takes the first colour and stage 6 wraps back onto it.
        assert_eq!(StageRow::colour(1), parse_hex(STAGE_PALETTE[0]).unwrap());
        assert_eq!(StageRow::colour(5), parse_hex(STAGE_PALETTE[4]).unwrap());
        assert_eq!(StageRow::colour(6), StageRow::colour(1));
    }

    #[test]
    fn the_control_table_covers_the_g903s_eleven_positions_without_a_duplicate_slot() {
        assert_eq!(CONTROLS.len(), 11);
        let mut slots: Vec<String> =
            CONTROLS.iter().map(|(control, _, _)| button_slot(control, false)).collect();
        slots.sort();
        let before = slots.len();
        slots.dedup();
        assert_eq!(slots.len(), before, "two controls share a slot");
    }

    #[test]
    fn the_shift_layer_gives_every_control_a_separate_slot() {
        for (control, _, _) in CONTROLS {
            assert_ne!(button_slot(control, false), button_slot(control, true));
            assert!(button_slot(control, true).ends_with(":shift"));
        }
    }

    #[test]
    fn a_marker_with_no_geometry_puts_its_callout_above_the_render() {
        assert_eq!(side_of(None), CalloutSide::Top);
        // G HUB's own label coordinates: negative x is the left gutter, negative y is overhead.
        assert_eq!(side_of(Some((-1400.0, 500.0))), CalloutSide::Left);
        assert_eq!(side_of(Some((2840.0, 500.0))), CalloutSide::Right);
        assert_eq!(side_of(Some((500.0, -200.0))), CalloutSide::Top);
    }

    #[test]
    fn the_left_and_right_callouts_sit_in_the_gutters_and_never_overlap_the_render() {
        let stage = (FALLBACK_RENDER.0 + GUTTER * 2.0, FALLBACK_RENDER.1 + TOP_GUTTER);
        let left = callout_left(CalloutSide::Left, None, FALLBACK_RENDER.0, stage.0);
        let right = callout_left(CalloutSide::Right, None, FALLBACK_RENDER.0, stage.0);

        assert!(left >= 0.0, "the left callout runs off the canvas");
        assert!(
            left + CALLOUT_WIDTH < GUTTER,
            "the left callout overlaps the render"
        );
        assert!(
            right > GUTTER + FALLBACK_RENDER.0,
            "the right callout overlaps the render"
        );
        assert!(
            right + CALLOUT_WIDTH <= stage.0,
            "the right callout runs off the canvas"
        );
    }

    #[test]
    fn a_top_callout_stays_inside_the_canvas_even_for_an_edge_marker() {
        let stage = (FALLBACK_RENDER.0 + GUTTER * 2.0, FALLBACK_RENDER.1 + TOP_GUTTER);
        for marker_x in [0.0, 720.0, FALLBACK_RENDER.0] {
            let left = callout_left(CalloutSide::Top, Some(marker_x), FALLBACK_RENDER.0, stage.0);
            assert!(left >= 0.0 && left + CALLOUT_WIDTH <= stage.0, "{marker_x} escaped");
        }
        let top = callout_top(CalloutSide::Top, None, stage.1);
        assert!(top >= 10.0);
        assert!(top + CALLOUT_HEIGHT <= TOP_GUTTER, "the top callout covers the render");
    }

    #[test]
    fn a_side_callout_is_clamped_to_the_canvas() {
        let stage_height = FALLBACK_RENDER.1 + TOP_GUTTER;
        for label_y in [-5000.0, 0.0, 1400.0, 99999.0] {
            let top = callout_top(CalloutSide::Right, Some(label_y), stage_height);
            assert!(top >= 10.0, "{label_y} went above the canvas");
            assert!(top + CALLOUT_HEIGHT <= stage_height, "{label_y} went below it");
        }
    }

    #[test]
    fn the_leader_line_leaves_the_callout_on_the_side_facing_the_mouse() {
        let row = |side| ButtonRow {
            slot: "button:Left".to_owned(),
            label: "Left Click".to_owned(),
            description: "Default".to_owned(),
            marker: pos2(1900.0, 900.0),
            callout: pos2(100.0, 800.0),
            side,
            assignable: true,
            disabled_reason: None,
            site: "Software".to_owned(),
        };
        // A callout in the left gutter points right, towards the render.
        assert_eq!(row(CalloutSide::Left).leader_start().x, 100.0 + CALLOUT_WIDTH as f32);
        // One in the right gutter points left.
        assert_eq!(row(CalloutSide::Right).leader_start().x, 100.0);
        // One overhead points down.
        assert_eq!(row(CalloutSide::Top).leader_start().y, 800.0 + CALLOUT_HEIGHT as f32);
    }

    #[test]
    fn the_tip_explains_why_an_unassignable_row_cannot_be_edited() {
        let mut row = ButtonRow {
            slot: "onboard".to_owned(),
            label: "Onboard buttons (1-11)".to_owned(),
            description: String::new(),
            marker: Pos2::ZERO,
            callout: Pos2::ZERO,
            side: CalloutSide::Top,
            assignable: false,
            disabled_reason: Some("because the mouse owns them".to_owned()),
            site: "Onboard memory".to_owned(),
        };
        assert_eq!(row.tip(), "because the mouse owns them");

        row.disabled_reason = None;
        assert_eq!(row.tip(), "Onboard buttons (1-11): Onboard memory");
    }

    #[test]
    fn the_zone_key_matches_what_the_profile_stores() {
        let zone = ZoneRow {
            index: 1,
            name: "LOGO".to_owned(),
            rect: Rect::NOTHING,
            hex: "#00AAFF".to_owned(),
            hex_buffer: "#00AAFF".to_owned(),
            selected: false,
        };
        assert_eq!(zone.key(), "zone:1");
    }

    #[test]
    fn the_selection_heading_distinguishes_all_zones_from_some() {
        let mut p = page();
        assert_eq!(p.selection_info(), "CLICK A LIGHTING ZONE");

        for (index, name) in [(0usize, "PRIMARY"), (1, "LOGO")] {
            p.zones.push(ZoneRow {
                index,
                name: name.to_owned(),
                rect: Rect::NOTHING,
                hex: "#00AAFF".to_owned(),
                hex_buffer: "#00AAFF".to_owned(),
                selected: false,
            });
        }
        assert_eq!(p.selection_info(), "CLICK A LIGHTING ZONE");

        p.zones[0].selected = true;
        assert_eq!(p.selection_info(), "1 ZONE SELECTED");
        p.zones[1].selected = true;
        assert_eq!(p.selection_info(), "ALL ZONES SELECTED");
    }

    #[test]
    fn selecting_one_zone_loads_its_colour_without_repainting_the_others() {
        let mut p = page();
        for (index, hex) in [(0usize, "#FF0000"), (1, "#00FF00")] {
            p.zones.push(ZoneRow {
                index,
                name: format!("ZONE {index}"),
                rect: Rect::NOTHING,
                hex: hex.to_owned(),
                hex_buffer: hex.to_owned(),
                selected: false,
            });
        }
        p.select_zone(1);
        assert_eq!(p.current_colour, parse_hex("#00FF00").unwrap());
        // The unselected zone keeps its own colour: loading a zone's colour is not an edit.
        assert_eq!(p.zones[0].hex, "#FF0000");
        assert!(p.zones[1].selected && !p.zones[0].selected);
        assert!(p.picker_open);
    }

    #[test]
    fn a_picker_edit_recolours_every_selected_zone_and_nothing_else() {
        let mut p = page();
        for index in 0..3usize {
            p.zones.push(ZoneRow {
                index,
                name: format!("ZONE {index}"),
                rect: Rect::NOTHING,
                hex: "#000000".to_owned(),
                hex_buffer: "#000000".to_owned(),
                selected: index != 1,
            });
        }
        p.current_colour = Color32::from_rgb(0x12, 0x34, 0x56);
        p.recolour_selected_zones();
        assert_eq!(p.zones[0].hex, "#123456");
        assert_eq!(p.zones[1].hex, "#000000");
        assert_eq!(p.zones[2].hex, "#123456");
        // The edit buffer follows, or the next frame's text field would show the old value.
        assert_eq!(p.zones[2].hex_buffer, "#123456");
    }

    #[test]
    fn a_bad_zone_colour_becomes_white_rather_than_black() {
        assert_eq!(normalise_hex("nope"), "#FFFFFF");
        assert_eq!(normalise_hex("00aaff"), "#00AAFF");
        assert_eq!(normalise_hex("  #112233 "), "#112233");
    }

    #[test]
    fn the_fallback_render_is_the_g903s_own_size_so_the_gutters_still_line_up() {
        // Without the artwork the callouts still need a canvas; these are the `g903_hero`
        // dimensions, so a page running without the render puts them where the render would be.
        assert_eq!(FALLBACK_RENDER, (1440.0, 2858.0));
        let p = page();
        let stage = p.stage_size(FALLBACK_RENDER);
        assert_eq!(stage.x as f64, FALLBACK_RENDER.0 + GUTTER * 2.0);
        assert_eq!(stage.y as f64, FALLBACK_RENDER.1 + TOP_GUTTER);
    }

    #[test]
    fn the_four_rail_destinations_are_distinct() {
        let pages = [
            SubPage::Sensitivity,
            SubPage::Assignments,
            SubPage::Lightsync,
            SubPage::DeviceSettings,
        ];
        for (index, page) in pages.iter().enumerate() {
            assert!(
                !pages[..index].contains(page),
                "{page:?} is listed twice"
            );
        }
        assert_eq!(SubPage::default(), SubPage::Sensitivity);
    }

    #[test]
    fn a_confirmation_only_runs_the_action_when_it_is_accepted() {
        let mut confirm = Confirm::default();
        assert!(!confirm.is_open());
        confirm.ask("Title", "Body", "Do it");
        assert!(confirm.is_open());
    }
}
