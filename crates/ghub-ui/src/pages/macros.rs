//! The macro editor. Port of `MacrosTabViewModel.cs` and `MacrosTabView.axaml`.
//!
//! A macro is a list of steps with a timeline beside it: recorded from physical input, or built by
//! hand from key taps, delays, text and library actions. Every macro here runs in software; none
//! is stored in device memory, and the page says so rather than letting the user assume a bound
//! macro survives the application closing.
//!
//! Two safety details are carried over deliberately. `max_loop_iterations` bounds the repeating
//! modes, so a dropped key-release does not mean a macro that types forever. And test playback
//! waits two seconds before it starts, because it types into whatever window has focus - which
//! would otherwise be this one, mid-click.

use std::time::Duration;

use egui::{Ui, Vec2};
use ghub_core::Id;
use ghub_core::action::{
    ActionDefinition, MacroDefinition, MacroRepeatMode, MacroStep, MouseButtonKind, key_name,
};

use crate::controls as c;
use crate::icons::{Icon, IconCache};
use crate::shared::{Changes, Shared};
use crate::theme as t;
use crate::widgets as w;

use super::action_editor::{ActionEditor, parse_key};
use super::{Confirm, scroll, status_band};

/// `RepeatModes`.
pub static REPEAT_MODES: &[MacroRepeatMode] = &[
    MacroRepeatMode::Once,
    MacroRepeatMode::NoRepeat,
    MacroRepeatMode::WhileHeld,
    MacroRepeatMode::Toggle,
    MacroRepeatMode::RepeatN,
];

/// `Play` waits this long before injecting, so the user can click into the test box.
const PLAYBACK_DELAY: Duration = Duration::from_secs(2);

/// `MacroItem` - one macro in the list.
struct MacroItem {
    id: Id,
    name: String,
    mode: String,
    steps: usize,
}

impl MacroItem {
    /// `Summary`. "software" is stated on every row because it is the thing most easily assumed
    /// wrong about a macro bound to a G-key.
    fn summary(&self) -> String {
        format!("{} \u{b7} {} steps \u{b7} software", self.mode, self.steps)
    }
}

/// `StepRow` - one step with the time it happens at.
struct StepRow {
    index: usize,
    offset_ms: u64,
}

/// `ModeHelp`.
fn mode_help(mode: MacroRepeatMode) -> &'static str {
    match mode {
        MacroRepeatMode::Once => "Plays once per press.",
        MacroRepeatMode::NoRepeat => "Plays once; presses during playback are ignored.",
        MacroRepeatMode::WhileHeld => {
            "Repeats while the key is held (bounded by max loops)."
        }
        MacroRepeatMode::Toggle => {
            "Press to start repeating, press again to stop (bounded by max loops)."
        }
        MacroRepeatMode::RepeatN => "Repeats the set number of times.",
    }
}

/// `StepRow.Kind`.
fn step_kind(step: &MacroStep) -> &'static str {
    match step {
        MacroStep::KeyDown { .. } | MacroStep::KeyUp { .. } => "Key",
        MacroStep::MouseDown { .. }
        | MacroStep::MouseUp { .. }
        | MacroStep::MouseMove { .. }
        | MacroStep::Wheel { .. } => "Mouse",
        MacroStep::Delay { .. } => "Delay",
        MacroStep::Text { .. } => "Text",
        MacroStep::Action { .. } => "Action",
        MacroStep::Condition { .. } => "Condition",
    }
}

/// `StepRow.Text`.
fn step_text(step: &MacroStep) -> String {
    match step {
        MacroStep::KeyDown { virtual_key } => format!("\u{25bc} {}", key_name(*virtual_key)),
        MacroStep::KeyUp { virtual_key } => format!("\u{25b2} {}", key_name(*virtual_key)),
        MacroStep::MouseDown { button } => format!("\u{25bc} {}", button_name(*button)),
        MacroStep::MouseUp { button } => format!("\u{25b2} {}", button_name(*button)),
        MacroStep::MouseMove { dx, dy } => format!("Move {dx},{dy}"),
        MacroStep::Wheel { delta, horizontal } => {
            format!("Wheel {delta}{}", if *horizontal { " (horizontal)" } else { "" })
        }
        MacroStep::Delay { milliseconds } => format!("Wait {milliseconds} ms"),
        MacroStep::Text { text } => format!("Type \"{text}\""),
        MacroStep::Action { action } => action.describe(),
        MacroStep::Condition { condition, value, then, otherwise } => format!(
            "If {condition:?} = {value} ({} / {} steps)",
            then.len(),
            otherwise.as_ref().map_or(0, Vec::len)
        ),
    }
}

fn button_name(button: MouseButtonKind) -> &'static str {
    match button {
        MouseButtonKind::Left => "Left",
        MouseButtonKind::Right => "Right",
        MouseButtonKind::Middle => "Middle",
        MouseButtonKind::XButton1 => "XButton1",
        MouseButtonKind::XButton2 => "XButton2",
    }
}

/// `StepRow.BarWidth` - delays are drawn proportionally (capped), events at a fixed width.
///
/// The cap is what keeps one 60-second delay from making every other step invisible.
fn bar_width(step: &MacroStep) -> f32 {
    match step {
        MacroStep::Delay { milliseconds } => (*milliseconds as f32 / 4.0).clamp(4.0, 400.0),
        _ => 8.0,
    }
}

#[derive(Default)]
struct Pending {
    new_macro: bool,
    duplicate: bool,
    delete: bool,
    save: bool,
    record: bool,
    stop_record: bool,
    play: bool,
    stop_all: bool,
}

pub struct MacrosPage {
    macros: Vec<MacroItem>,
    selected_macro: Option<Id>,

    name: String,
    repeat_mode: MacroRepeatMode,
    repeat_count: i64,
    repeat_count_buffer: String,
    repeat_delay: i64,
    repeat_delay_buffer: String,
    max_loops: i64,
    max_loops_buffer: String,
    use_recorded_delays: bool,
    fixed_delay: i64,
    fixed_delay_buffer: String,

    /// The steps being edited, which is the authority until Save. The list the document holds is
    /// only read when a macro is loaded.
    steps: Vec<MacroStep>,
    rows: Vec<StepRow>,
    selected_step: Option<usize>,

    new_key: String,
    new_delay: i64,
    new_delay_buffer: String,
    new_text: String,
    editor: ActionEditor,
    editor_open: bool,

    record_delays: bool,
    record_mouse: bool,
    /// What the runtime last reported, drained from its own status queue.
    run_status: String,
    /// The scratch box the test playback types into.
    test_text: String,

    confirm: Confirm,
    /// An error produced while drawing - a key name that will not parse - raised after the frame,
    /// because the banner lives above the widget that produced it.
    pending_error: Option<String>,
    loaded: bool,
}

impl Default for MacrosPage {
    fn default() -> Self {
        Self {
            macros: Vec::new(),
            selected_macro: None,
            name: String::new(),
            repeat_mode: MacroRepeatMode::Once,
            repeat_count: 1,
            repeat_count_buffer: "1".to_owned(),
            repeat_delay: 50,
            repeat_delay_buffer: "50".to_owned(),
            max_loops: 500,
            max_loops_buffer: "500".to_owned(),
            use_recorded_delays: true,
            fixed_delay: 20,
            fixed_delay_buffer: "20".to_owned(),
            steps: Vec::new(),
            rows: Vec::new(),
            selected_step: None,
            new_key: "A".to_owned(),
            new_delay: 50,
            new_delay_buffer: "50".to_owned(),
            new_text: String::new(),
            editor: ActionEditor::default(),
            editor_open: false,
            record_delays: true,
            record_mouse: false,
            run_status: "Idle".to_owned(),
            test_text: String::new(),
            confirm: Confirm::default(),
            pending_error: None,
            loaded: false,
        }
    }
}

impl MacrosPage {
    /// `Refresh()`.
    fn reload(&mut self, shared: &Shared) {
        let keep = self.selected_macro;
        let document = shared.document();
        self.macros = document
            .macros
            .iter()
            .map(|m| MacroItem {
                id: m.id,
                name: m.name.clone(),
                mode: format!("{:?}", m.repeat_mode),
                steps: m.steps.len(),
            })
            .collect();
        self.selected_macro = keep
            .filter(|id| self.macros.iter().any(|m| m.id == *id))
            .or_else(|| self.macros.first().map(|m| m.id));
        self.editor.macros =
            document.macros.iter().map(|m| (m.name.clone(), m.id)).collect();
        self.load_macro(shared);
        self.loaded = true;
    }

    /// `LoadMacro()`.
    fn load_macro(&mut self, shared: &Shared) {
        let document = shared.document();
        let Some(definition) = self
            .selected_macro
            .and_then(|id| document.macros.iter().find(|m| m.id == id))
        else {
            self.steps.clear();
            self.rebuild_steps();
            return;
        };

        self.name = definition.name.clone();
        self.repeat_mode = definition.repeat_mode;
        self.repeat_count = definition.repeat_count as i64;
        self.repeat_count_buffer = self.repeat_count.to_string();
        self.repeat_delay = definition.repeat_delay_milliseconds as i64;
        self.repeat_delay_buffer = self.repeat_delay.to_string();
        self.max_loops = definition.max_loop_iterations as i64;
        self.max_loops_buffer = self.max_loops.to_string();
        self.use_recorded_delays = definition.use_recorded_delays;
        self.fixed_delay = definition.fixed_delay_milliseconds as i64;
        self.fixed_delay_buffer = self.fixed_delay.to_string();
        self.steps = definition.steps.clone();
        self.selected_step = None;
        self.rebuild_steps();
    }

    /// `RebuildSteps()` - recomputes each step's offset on the timeline.
    ///
    /// The offsets follow the macro's *own* delay policy, so turning off "use recorded delays"
    /// re-times the whole timeline rather than leaving it showing times playback will not use.
    fn rebuild_steps(&mut self) {
        self.rows.clear();
        let mut offset = 0u64;
        for (index, step) in self.steps.iter().enumerate() {
            self.rows.push(StepRow { index, offset_ms: offset });
            if let MacroStep::Delay { milliseconds } = step {
                offset += if self.use_recorded_delays {
                    u64::from(*milliseconds)
                } else {
                    self.fixed_delay.max(0) as u64
                };
            }
        }
        if self.selected_step.is_some_and(|index| index >= self.steps.len()) {
            self.selected_step = None;
        }
    }

    /// `Insert(steps)` - after the selected step, or at the end.
    fn insert(&mut self, steps: Vec<MacroStep>) {
        let at = self.selected_step.map_or(self.steps.len(), |index| index + 1);
        let count = steps.len();
        for (offset, step) in steps.into_iter().enumerate() {
            self.steps.insert(at + offset, step);
        }
        // The selection follows the insertion, so inserting twice builds a sequence rather than
        // stacking everything at one point.
        self.selected_step = Some(at + count - 1);
        self.rebuild_steps();
    }

    /// `Move(delta)`.
    fn move_step(&mut self, delta: isize) {
        let Some(index) = self.selected_step else { return };
        let target = index as isize + delta;
        if target < 0 || target as usize >= self.steps.len() {
            return;
        }
        let target = target as usize;
        self.steps.swap(index, target);
        self.selected_step = Some(target);
        self.rebuild_steps();
    }

    /// `Build()` - the macro as edited, clamped to its documented bounds.
    fn build(&self, shared: &Shared) -> Option<MacroDefinition> {
        let id = self.selected_macro?;
        let document = shared.document();
        let base = document.macros.iter().find(|m| m.id == id)?;
        let mut definition = base.clone();
        definition.name =
            if self.name.trim().is_empty() { "Macro".to_owned() } else { self.name.trim().to_owned() };
        definition.repeat_mode = self.repeat_mode;
        definition.repeat_count =
            (self.repeat_count.max(0) as u32).clamp(1, MacroDefinition::MAX_REPEAT_COUNT);
        definition.repeat_delay_milliseconds =
            (self.repeat_delay.max(0) as u32).min(MacroDefinition::MAX_DELAY_MILLISECONDS);
        definition.max_loop_iterations = (self.max_loops.max(0) as u32).clamp(1, 100_000);
        definition.use_recorded_delays = self.use_recorded_delays;
        definition.fixed_delay_milliseconds =
            (self.fixed_delay.max(0) as u32).min(MacroDefinition::MAX_DELAY_MILLISECONDS);
        definition.steps = self.steps.clone();
        Some(definition)
    }
}

// =============================================================== drawing

impl MacrosPage {
    pub fn show(
        &mut self,
        shared: &mut Shared,
        icons: &mut IconCache,
        ui: &mut Ui,
        changes: Changes,
    ) {
        if changes.document || !self.loaded {
            self.reload(shared);
        }

        // `StatusChanged` - the runtime queues what it did and the page drains it, so a playback
        // that ends while the window is idle still reports itself.
        for status in shared.service().runtime().take_status() {
            self.run_status = format!(
                "{}: {:?}{}{}",
                status.name,
                status.state,
                if status.iteration > 1 {
                    format!(" (iteration {})", status.iteration)
                } else {
                    String::new()
                },
                status.detail.map_or(String::new(), |detail| format!(" \u{2013} {detail}"))
            );
        }
        // A macro mid-playback or a recording in progress both change on their own.
        if !shared.service().runtime().running().is_empty() || shared.service().is_recording() {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }

        if let Some(true) = self.confirm.show(ui.ctx(), icons) {
            self.delete_selected(shared);
        }

        let mut pending = Pending::default();

        ui.horizontal_top(|ui| {
            // `ColumnDefinitions="260,*"`.
            ui.allocate_ui_with_layout(
                Vec2::new(260.0, ui.available_height()),
                egui::Layout::top_down(egui::Align::Min),
                |ui| self.list(ui, shared, &mut pending),
            );
            ui.add_space(12.0);
            ui.allocate_ui_with_layout(
                ui.available_size(),
                egui::Layout::top_down(egui::Align::Min),
                |ui| self.editor_column(ui, icons, shared, &mut pending),
            );
        });

        self.apply(shared, pending);
    }

    fn list(&mut self, ui: &mut Ui, shared: &Shared, pending: &mut Pending) {
        ui.horizontal(|ui| {
            if c::button(ui, "New", c::ButtonStyle::Primary, !shared.busy()).clicked() {
                pending.new_macro = true;
            }
            ui.add_space(4.0);
            if c::button(
                ui,
                "Duplicate",
                c::ButtonStyle::Outlined,
                self.selected_macro.is_some() && !shared.busy(),
            )
            .clicked()
            {
                pending.duplicate = true;
            }
            ui.add_space(4.0);
            if c::button(
                ui,
                "Delete",
                c::ButtonStyle::Text,
                self.selected_macro.is_some() && !shared.busy() && !self.confirm.is_open(),
            )
            .clicked()
            {
                pending.delete = true;
            }
        });
        ui.add_space(6.0);

        let mut select = None;
        scroll(ui, "macro list", |ui| {
            for item in &self.macros {
                let active = self.selected_macro == Some(item.id);
                let response = w::anim_row(ui, &item.name, active);
                w::body_small(ui, &item.summary(), t::TEXT_LABEL);
                ui.add_space(4.0);
                if response.clicked() {
                    select = Some(item.id);
                }
            }
            if self.macros.is_empty() {
                w::body_small(
                    ui,
                    "No macros yet. New creates one with a single key tap to edit.",
                    t::TEXT_LABEL,
                );
            }
        });
        if let Some(id) = select {
            self.selected_macro = Some(id);
            self.load_macro(shared);
        }
    }

    fn editor_column(
        &mut self,
        ui: &mut Ui,
        icons: &mut IconCache,
        shared: &Shared,
        pending: &mut Pending,
    ) {
        scroll(ui, "macro editor", |ui| {
            status_band(ui, shared);
            ui.add_space(8.0);

            if self.selected_macro.is_none() {
                w::card(ui, |ui| {
                    w::body(ui, "Select a macro on the left, or press New to create one.")
                });
                return;
            }

            self.settings_card(ui, icons, shared, pending);
            ui.add_space(12.0);
            w::title(ui, "Event timeline");
            ui.add_space(6.0);
            self.timeline_card(ui, icons, shared, pending);
            ui.add_space(12.0);
            w::title(ui, "Test in a safe window");
            ui.add_space(6.0);
            self.test_card(ui, shared, pending);
        });
    }

    fn settings_card(
        &mut self,
        ui: &mut Ui,
        icons: &mut IconCache,
        shared: &Shared,
        pending: &mut Pending,
    ) {
        w::card(ui, |ui| {
            w::label(ui, "MACRO NAME");
            c::text_field(ui, &mut self.name, ui.available_width().min(420.0), true);
            ui.add_space(8.0);

            ui.horizontal_wrapped(|ui| {
                w::body(ui, "Trigger mode");
                ui.add_space(6.0);
                ui.allocate_ui(Vec2::new(200.0, 28.0), |ui| {
                    c::combo(
                        ui,
                        icons,
                        "macro repeat mode",
                        &mut self.repeat_mode,
                        REPEAT_MODES,
                        |mode| format!("{mode:?}"),
                        true,
                    );
                });
                ui.add_space(12.0);
                w::body(ui, "Repeat count");
                ui.add_space(6.0);
                c::numeric(
                    ui,
                    icons,
                    &mut self.repeat_count_buffer,
                    &mut self.repeat_count,
                    1..=i64::from(MacroDefinition::MAX_REPEAT_COUNT),
                    1,
                    true,
                );
                ui.add_space(12.0);
                w::body(ui, "Repeat interval ms");
                ui.add_space(6.0);
                c::numeric(
                    ui,
                    icons,
                    &mut self.repeat_delay_buffer,
                    &mut self.repeat_delay,
                    0..=i64::from(MacroDefinition::MAX_DELAY_MILLISECONDS),
                    10,
                    true,
                );
                ui.add_space(12.0);
                w::body(ui, "Max loops");
                ui.add_space(6.0);
                c::numeric(
                    ui,
                    icons,
                    &mut self.max_loops_buffer,
                    &mut self.max_loops,
                    1..=100_000,
                    50,
                    true,
                );
            });
            ui.add_space(6.0);
            w::body_small(ui, mode_help(self.repeat_mode), t::TEXT_LABEL);

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if c::checkbox(
                    ui,
                    icons,
                    &mut self.use_recorded_delays,
                    "Use recorded delays",
                    true,
                ) {
                    // The timeline is timed by this, so it is re-laid-out immediately rather than
                    // after a save.
                    self.rebuild_steps();
                }
                ui.add_space(8.0);
                w::body(ui, "otherwise fixed delay ms");
                ui.add_space(6.0);
                if c::numeric(
                    ui,
                    icons,
                    &mut self.fixed_delay_buffer,
                    &mut self.fixed_delay,
                    0..=i64::from(MacroDefinition::MAX_DELAY_MILLISECONDS),
                    10,
                    !self.use_recorded_delays,
                ) {
                    self.rebuild_steps();
                }
            });

            ui.add_space(10.0);
            if c::button(ui, "Save macro", c::ButtonStyle::Primary, !shared.busy()).clicked() {
                pending.save = true;
            }
            ui.add_space(8.0);
            w::body_small(
                ui,
                "All macros run in software (SendInput) while this program runs; none are stored \
                 in device memory. Emergency stop: Ctrl+Shift+Pause.",
                t::TEXT_LABEL,
            );
        });
    }

    fn timeline_card(
        &mut self,
        ui: &mut Ui,
        icons: &mut IconCache,
        shared: &Shared,
        pending: &mut Pending,
    ) {
        w::card(ui, |ui| {
            let recording = shared.service().is_recording();
            ui.horizontal_wrapped(|ui| {
                if w::icon_button(
                    ui,
                    icons,
                    if recording { Icon::RecordDisabled } else { Icon::Record },
                    28.0,
                    20.0,
                    if recording { t::DISABLED } else { t::ERROR },
                )
                .on_hover_text("Record physical input")
                .clicked()
                    && !recording
                {
                    pending.record = true;
                }
                ui.add_space(8.0);
                if w::icon_button(
                    ui,
                    icons,
                    Icon::StopRecord,
                    28.0,
                    20.0,
                    if recording { t::TEXT } else { t::DISABLED },
                )
                .on_hover_text("Stop recording")
                .clicked()
                    && recording
                {
                    pending.stop_record = true;
                }
                ui.add_space(12.0);
                c::checkbox(ui, icons, &mut self.record_delays, "Record delays", !recording);
                ui.add_space(12.0);
                c::checkbox(
                    ui,
                    icons,
                    &mut self.record_mouse,
                    "Record mouse buttons and wheel",
                    !recording,
                );
            });
            if recording {
                ui.add_space(6.0);
                w::callout_warning(ui, |ui| {
                    w::body_small(
                        ui,
                        "Recording physical input\u{2026} press Stop recording when done (this \
                         program's own window is included).",
                        t::TEXT,
                    );
                });
            }

            ui.add_space(8.0);
            let mut select = None;
            egui::ScrollArea::vertical()
                .id_salt("macro steps")
                .max_height(320.0)
                .show(ui, |ui| {
                    if self.rows.is_empty() {
                        w::body_small(
                            ui,
                            "No steps. Record some input, or insert a key tap below.",
                            t::TEXT_LABEL,
                        );
                    }
                    for row in &self.rows {
                        let step = &self.steps[row.index];
                        let selected = self.selected_step == Some(row.index);
                        let response = ui.horizontal(|ui| {
                            // `ColumnDefinitions="80,70,*,Auto"`, monospace time so the column
                            // lines up.
                            ui.allocate_ui(Vec2::new(80.0, 18.0), |ui| {
                                let text = format!("{:>6} ms", row.offset_ms);
                                ui.painter().text(
                                    ui.cursor().min,
                                    egui::Align2::LEFT_TOP,
                                    text,
                                    egui::FontId::new(12.0, t::family(t::MONO)),
                                    if selected { t::TEXT } else { t::TEXT_ROW },
                                );
                            });
                            ui.allocate_ui(Vec2::new(70.0, 18.0), |ui| {
                                w::body_small(ui, step_kind(step), t::TEXT_LABEL)
                            });
                            w::body(ui, &step_text(step));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let (rect, _) = ui.allocate_exact_size(
                                        Vec2::new(bar_width(step), 8.0),
                                        egui::Sense::hover(),
                                    );
                                    ui.painter().rect_filled(
                                        rect,
                                        egui::CornerRadius::same(2),
                                        t::ACCENT,
                                    );
                                },
                            );
                        });
                        if response.response.interact(egui::Sense::click()).clicked() {
                            select = Some(row.index);
                        }
                        ui.add_space(2.0);
                    }
                });
            if let Some(index) = select {
                self.selected_step = Some(index);
            }

            ui.add_space(8.0);
            let has_step = self.selected_step.is_some();
            ui.horizontal_wrapped(|ui| {
                if c::button(ui, "Move up", c::ButtonStyle::Outlined, has_step).clicked() {
                    self.move_step(-1);
                }
                ui.add_space(6.0);
                if c::button(ui, "Move down", c::ButtonStyle::Outlined, has_step).clicked() {
                    self.move_step(1);
                }
                ui.add_space(6.0);
                if c::button(ui, "Duplicate step", c::ButtonStyle::Outlined, has_step).clicked() {
                    if let Some(index) = self.selected_step {
                        let step = self.steps[index].clone();
                        self.steps.insert(index + 1, step);
                        self.selected_step = Some(index + 1);
                        self.rebuild_steps();
                    }
                }
                ui.add_space(6.0);
                if c::button(ui, "Delete step", c::ButtonStyle::Text, has_step).clicked() {
                    if let Some(index) = self.selected_step {
                        self.steps.remove(index);
                        self.selected_step = index.checked_sub(1).filter(|_| !self.steps.is_empty());
                        self.rebuild_steps();
                    }
                }
            });

            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                w::label(ui, "KEY");
                ui.add_space(4.0);
                c::text_field(ui, &mut self.new_key, 90.0, true);
                ui.add_space(6.0);
                if c::button(ui, "Insert key tap", c::ButtonStyle::Outlined, true).clicked() {
                    // A tap is down, wait, up - three steps, because a key that goes down and
                    // straight back up is often dropped by the receiving application.
                    match parse_key(&self.new_key) {
                        Ok(vk) => {
                            let delay = self.new_delay.max(0) as u32;
                            self.insert(vec![
                                MacroStep::KeyDown { virtual_key: vk },
                                MacroStep::Delay { milliseconds: delay },
                                MacroStep::KeyUp { virtual_key: vk },
                            ]);
                        }
                        Err(message) => self.pending_error = Some(message),
                    }
                }
                ui.add_space(12.0);
                c::numeric(
                    ui,
                    icons,
                    &mut self.new_delay_buffer,
                    &mut self.new_delay,
                    0..=i64::from(MacroDefinition::MAX_DELAY_MILLISECONDS),
                    10,
                    true,
                );
                ui.add_space(6.0);
                if c::button(ui, "Insert delay", c::ButtonStyle::Outlined, true).clicked() {
                    let milliseconds = (self.new_delay.max(0) as u32)
                        .min(MacroDefinition::MAX_DELAY_MILLISECONDS);
                    self.insert(vec![MacroStep::Delay { milliseconds }]);
                }
            });
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                w::label(ui, "TEXT");
                ui.add_space(4.0);
                c::text_field(ui, &mut self.new_text, 200.0, true);
                ui.add_space(6.0);
                if c::button(
                    ui,
                    "Insert text",
                    c::ButtonStyle::Outlined,
                    !self.new_text.is_empty(),
                )
                .clicked()
                {
                    self.insert(vec![MacroStep::Text { text: self.new_text.clone() }]);
                }
            });

            ui.add_space(8.0);
            let caption = if self.editor_open {
                "Insert library action (launch, media, DPI, profile\u{2026}) \u{25be}"
            } else {
                "Insert library action (launch, media, DPI, profile\u{2026}) \u{25b8}"
            };
            if w::link(ui, caption, true).clicked() {
                self.editor_open = !self.editor_open;
            }
            if self.editor_open {
                ui.add_space(6.0);
                self.editor.show(ui, icons, true);
                ui.add_space(6.0);
                if c::button(ui, "Insert action", c::ButtonStyle::Outlined, true).clicked() {
                    // `PassThrough` is "do nothing", which as a macro step would be a step that
                    // silently does not exist.
                    if let Some(action) =
                        self.editor.build().filter(|a| *a != ActionDefinition::PassThrough)
                    {
                        self.insert(vec![MacroStep::Action { action }]);
                    }
                }
            }
        });
    }

    fn test_card(&mut self, ui: &mut Ui, shared: &Shared, pending: &mut Pending) {
        w::card(ui, |ui| {
            ui.horizontal(|ui| {
                if c::button(
                    ui,
                    "\u{25b6} Play (after 2 s)",
                    c::ButtonStyle::Primary,
                    self.selected_macro.is_some() && !shared.busy(),
                )
                .clicked()
                {
                    pending.play = true;
                }
                ui.add_space(6.0);
                if c::button(ui, "Stop all macros", c::ButtonStyle::Outlined, true).clicked() {
                    pending.stop_all = true;
                }
                ui.add_space(6.0);
                if c::button(ui, "Clear", c::ButtonStyle::Text, !self.test_text.is_empty())
                    .clicked()
                {
                    self.test_text.clear();
                }
                ui.add_space(8.0);
                w::body(ui, &self.run_status);
            });
            ui.add_space(6.0);
            w::body_small(
                ui,
                "Click into the box below before playback; the macro types into whatever has \
                 focus.",
                t::TEXT_LABEL,
            );
            ui.add_space(4.0);
            c::text_area(ui, &mut self.test_text, 4, true);
        });
    }
}

// =============================================================== applying

impl MacrosPage {
    fn delete_selected(&mut self, shared: &mut Shared) {
        let Some(id) = self.selected_macro else { return };
        self.selected_macro = None;
        shared.run("Delete", {
            let service = std::sync::Arc::clone(shared.service());
            move || {
                service
                    .update_document(|document| {
                        document.macros.retain(|m| m.id != id);
                        // A binding to a deleted macro would silently do nothing, so the bindings
                        // go with it. This is the reason deleting asks first.
                        for profile in &mut document.profiles {
                            for device in profile.devices.values_mut() {
                                device.bindings.retain(|_, binding| {
                                    binding.action.macro_id() != Some(id)
                                });
                            }
                        }
                    })
                    .map_err(|e| crate::host::JobError::Failed(e.to_string()))?;
                Ok(Some("Macro deleted (bindings using it were cleared)".to_owned()))
            }
        });
    }

    fn apply(&mut self, shared: &mut Shared, pending: Pending) {
        if let Some(message) = self.pending_error.take() {
            shared.error = Some(message);
        }

        if pending.new_macro {
            // Seeded with a single key tap rather than empty, so the timeline shows what a step
            // looks like and Save has something to write.
            let mut definition = MacroDefinition::new(format!(
                "Macro {}",
                ghub_core::time::format_readable(ghub_core::time::now_unix_millis())
            ));
            definition.steps = vec![
                MacroStep::KeyDown { virtual_key: 0x41 },
                MacroStep::Delay { milliseconds: 30 },
                MacroStep::KeyUp { virtual_key: 0x41 },
            ];
            let id = definition.id;
            shared.edit_document("New macro", move |document| {
                document.macros.push(definition);
            });
            self.selected_macro = Some(id);
        }

        if pending.duplicate {
            if let Some(id) = self.selected_macro {
                shared.edit_document("Duplicate macro", move |document| {
                    let Some(source) = document.macros.iter().find(|m| m.id == id).cloned() else {
                        return;
                    };
                    document.macros.push(MacroDefinition {
                        id: Id::new(),
                        name: format!("{} (copy)", source.name),
                        ..source
                    });
                });
            }
        }

        if pending.delete {
            if let Some(item) = self
                .selected_macro
                .and_then(|id| self.macros.iter().find(|m| m.id == id))
            {
                self.confirm.ask(
                    "Delete macro",
                    &format!(
                        "Delete '{}'? Any binding that runs it is cleared at the same time, \
                         because a binding to a macro that is gone does nothing.",
                        item.name
                    ),
                    "Delete",
                );
            }
        }

        if pending.save {
            if let Some(definition) = self.build(shared) {
                let name = definition.name.clone();
                let steps = definition.steps.len();
                let id = definition.id;
                shared.edit_document(format!("Saved '{name}' ({steps} steps)"), move |document| {
                    if let Some(slot) = document.macros.iter_mut().find(|m| m.id == id) {
                        *slot = definition;
                    }
                });
            }
        }

        if pending.record {
            shared
                .service()
                .start_free_recording(self.record_delays, self.record_mouse);
            shared.status = Some(
                "Recording physical input\u{2026} press Stop recording when done.".to_owned(),
            );
        }

        if pending.stop_record {
            let mut recorded = shared.service().stop_free_recording();
            // Drop the click on the Stop button itself when mouse buttons were recorded; otherwise
            // every recording ends with a click on this page.
            if self.record_mouse {
                trim_stop_click(&mut recorded);
            }
            let count = recorded.len();
            self.steps = recorded;
            self.selected_step = None;
            self.rebuild_steps();
            shared.status = Some(format!("Recorded {count} events. Review, then Save."));
        }

        if pending.play {
            if let Some(definition) = self.build(shared) {
                let runtime = std::sync::Arc::clone(shared.service().runtime());
                self.run_status =
                    "Click into the test box: playback starts in 2 seconds\u{2026}".to_owned();
                shared.attempt("Play macro", move || {
                    // The wait happens on the worker thread, which is what makes the UI stay
                    // responsive - and clickable - during it.
                    std::thread::sleep(PLAYBACK_DELAY);
                    let state = runtime.play(definition);
                    Ok(format!("Test playback: {state:?}"))
                });
            }
        }

        if pending.stop_all {
            shared.service().runtime().stop_all("stop button");
            self.run_status = "Stopped".to_owned();
        }
    }
}

/// Removes the trailing click that pressed "Stop recording".
///
/// The recorder sees this program's own window too, so the last mouse-down/up pair in a
/// mouse-recording session is the click that stopped it. Trimming from the last mouse-down - and
/// the delay in front of it - is what the C# did, and it is the difference between a usable
/// recording and one that ends by clicking wherever the Stop button happened to be.
fn trim_stop_click(steps: &mut Vec<MacroStep>) {
    if steps.len() < 2 || !matches!(steps.last(), Some(MacroStep::MouseUp { .. })) {
        return;
    }
    let Some(down) = steps.iter().rposition(|step| matches!(step, MacroStep::MouseDown { .. }))
    else {
        return;
    };
    let cut =
        if down > 0 && matches!(steps[down - 1], MacroStep::Delay { .. }) { down - 1 } else { down };
    steps.truncate(cut);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> MacrosPage {
        MacrosPage::default()
    }

    #[test]
    fn every_repeat_mode_is_offered_and_explained() {
        assert_eq!(REPEAT_MODES.len(), 5);
        assert!(REPEAT_MODES.contains(&MacroRepeatMode::default()));
        for mode in REPEAT_MODES {
            let help = mode_help(*mode);
            assert!(!help.is_empty(), "{mode:?} has no explanation");
        }
        // The two unbounded modes have to say they are bounded, or the max-loops field looks
        // decorative.
        assert!(mode_help(MacroRepeatMode::WhileHeld).contains("max loops"));
        assert!(mode_help(MacroRepeatMode::Toggle).contains("max loops"));
    }

    #[test]
    fn the_timeline_times_each_step_from_the_delays_before_it() {
        let mut p = page();
        p.steps = vec![
            MacroStep::KeyDown { virtual_key: 0x41 },
            MacroStep::Delay { milliseconds: 100 },
            MacroStep::KeyUp { virtual_key: 0x41 },
            MacroStep::Delay { milliseconds: 250 },
            MacroStep::Text { text: "hi".to_owned() },
        ];
        p.rebuild_steps();
        assert_eq!(
            p.rows.iter().map(|row| row.offset_ms).collect::<Vec<_>>(),
            vec![0, 0, 100, 100, 350]
        );
    }

    #[test]
    fn turning_off_recorded_delays_re_times_the_timeline() {
        let mut p = page();
        p.steps = vec![
            MacroStep::Delay { milliseconds: 1000 },
            MacroStep::Delay { milliseconds: 1000 },
            MacroStep::KeyDown { virtual_key: 0x41 },
        ];
        p.use_recorded_delays = false;
        p.fixed_delay = 20;
        p.rebuild_steps();
        // Showing the recorded times here would promise a playback that will not happen.
        assert_eq!(
            p.rows.iter().map(|row| row.offset_ms).collect::<Vec<_>>(),
            vec![0, 20, 40]
        );
    }

    #[test]
    fn a_long_delay_cannot_squeeze_the_other_bars_out_of_existence() {
        assert_eq!(bar_width(&MacroStep::Delay { milliseconds: 0 }), 4.0);
        assert_eq!(bar_width(&MacroStep::Delay { milliseconds: 400 }), 100.0);
        assert_eq!(bar_width(&MacroStep::Delay { milliseconds: 600_000 }), 400.0);
        assert_eq!(bar_width(&MacroStep::KeyDown { virtual_key: 0x41 }), 8.0);
    }

    #[test]
    fn inserting_goes_after_the_selection_so_a_sequence_builds_in_order() {
        let mut p = page();
        p.steps = vec![
            MacroStep::Text { text: "a".to_owned() },
            MacroStep::Text { text: "c".to_owned() },
        ];
        p.selected_step = Some(0);
        p.insert(vec![MacroStep::Text { text: "b".to_owned() }]);

        let texts: Vec<String> = p
            .steps
            .iter()
            .map(|step| match step {
                MacroStep::Text { text } => text.clone(),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(texts, vec!["a", "b", "c"]);
        // And the selection moved onto what was just inserted, so inserting again continues.
        assert_eq!(p.selected_step, Some(1));
    }

    #[test]
    fn inserting_with_nothing_selected_appends() {
        let mut p = page();
        p.steps = vec![MacroStep::Text { text: "a".to_owned() }];
        p.selected_step = None;
        p.insert(vec![MacroStep::Text { text: "b".to_owned() }]);
        assert_eq!(p.steps.len(), 2);
        assert_eq!(p.selected_step, Some(1));
    }

    #[test]
    fn moving_a_step_carries_the_selection_with_it() {
        let mut p = page();
        p.steps = vec![
            MacroStep::Delay { milliseconds: 1 },
            MacroStep::Delay { milliseconds: 2 },
            MacroStep::Delay { milliseconds: 3 },
        ];
        p.selected_step = Some(2);
        p.move_step(-1);
        assert_eq!(p.selected_step, Some(1));
        assert!(matches!(p.steps[1], MacroStep::Delay { milliseconds: 3 }));

        // At the edges it does nothing rather than wrapping.
        p.selected_step = Some(0);
        p.move_step(-1);
        assert_eq!(p.selected_step, Some(0));
        p.selected_step = Some(2);
        p.move_step(1);
        assert_eq!(p.selected_step, Some(2));
    }

    #[test]
    fn the_recording_loses_the_click_that_stopped_it() {
        let mut steps = vec![
            MacroStep::KeyDown { virtual_key: 0x41 },
            MacroStep::Delay { milliseconds: 50 },
            MacroStep::KeyUp { virtual_key: 0x41 },
            MacroStep::Delay { milliseconds: 900 },
            MacroStep::MouseDown { button: MouseButtonKind::Left },
            MacroStep::Delay { milliseconds: 20 },
            MacroStep::MouseUp { button: MouseButtonKind::Left },
        ];
        trim_stop_click(&mut steps);
        // The click, and the wait in front of it, are both gone.
        assert_eq!(steps.len(), 3);
        assert!(matches!(steps.last(), Some(MacroStep::KeyUp { .. })));
    }

    #[test]
    fn a_recording_that_does_not_end_in_a_click_is_left_alone() {
        let original = vec![
            MacroStep::KeyDown { virtual_key: 0x41 },
            MacroStep::KeyUp { virtual_key: 0x41 },
        ];
        let mut steps = original.clone();
        trim_stop_click(&mut steps);
        assert_eq!(steps, original);

        // And neither is one too short to contain a click.
        let mut one = vec![MacroStep::MouseUp { button: MouseButtonKind::Left }];
        trim_stop_click(&mut one);
        assert_eq!(one.len(), 1);
    }

    #[test]
    fn every_step_kind_describes_itself() {
        let steps = [
            MacroStep::KeyDown { virtual_key: 0x41 },
            MacroStep::KeyUp { virtual_key: 0x41 },
            MacroStep::MouseDown { button: MouseButtonKind::Left },
            MacroStep::MouseUp { button: MouseButtonKind::XButton2 },
            MacroStep::MouseMove { dx: 10, dy: -5 },
            MacroStep::Wheel { delta: 120, horizontal: true },
            MacroStep::Delay { milliseconds: 40 },
            MacroStep::Text { text: "hi".to_owned() },
            MacroStep::Action { action: ActionDefinition::GShift },
            MacroStep::Condition {
                condition: ghub_core::action::MacroCondition::KeyHeld,
                value: "A".to_owned(),
                then: vec![MacroStep::Delay { milliseconds: 1 }],
                otherwise: None,
            },
        ];
        for step in &steps {
            assert!(!step_kind(step).is_empty());
            assert_ne!(step_kind(step), "?", "{step:?} has no kind");
            assert!(!step_text(step).is_empty(), "{step:?} has no description");
        }
        assert_eq!(step_text(&steps[0]), "\u{25bc} A");
        assert_eq!(step_text(&steps[1]), "\u{25b2} A");
        assert_eq!(step_text(&steps[4]), "Move 10,-5");
        assert_eq!(step_text(&steps[5]), "Wheel 120 (horizontal)");
        assert_eq!(step_text(&steps[6]), "Wait 40 ms");
    }

    #[test]
    fn the_summary_says_a_macro_runs_in_software() {
        let item = MacroItem {
            id: Id::new(),
            name: "Reload".to_owned(),
            mode: "Once".to_owned(),
            steps: 3,
        };
        assert_eq!(item.summary(), "Once \u{b7} 3 steps \u{b7} software");
    }

    #[test]
    fn the_playback_delay_is_long_enough_to_click_elsewhere() {
        assert_eq!(PLAYBACK_DELAY, Duration::from_secs(2));
    }
}
