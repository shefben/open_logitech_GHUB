//! The action library and parameter editor. Port of
//! `Artemis.UI/Screens/Peripherals/ActionEditorViewModel.cs` and `ActionEditorView.axaml`.
//!
//! One editor serves every assignable control - G-keys, mouse buttons, the spy buttons - because
//! in G HUB they all draw from the same command list. The seventeen templates are a flat library
//! with a search box over it, and each one states whether it can live in the device's own memory
//! or only runs while this program does. That last bit is not decoration: a binding the user
//! believes is onboard but is not stops working the moment the app closes, and the only honest
//! place to say so is next to the thing being built.

use ghub_core::Id;
use ghub_core::action::{
    ActionDefinition, DpiCommand, KeyModifiers, KeyStroke, LightingCommand, MediaCommand,
    MouseButtonKind, ProfileCommand, key_name,
};

use crate::controls as c;
use crate::icons::IconCache;
use crate::theme as t;
use crate::widgets as w;

/// One entry in the library. `ActionTemplate` in the C#, field for field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionTemplate {
    pub name: &'static str,
    pub category: &'static str,
    /// The discriminator [`ActionEditor::build`] switches on. Matches the C# `Kind` strings so the
    /// two stay comparable.
    pub kind: &'static str,
    pub description: &'static str,
    /// Whether the action can be stored in onboard memory. Drives [`ActionEditor::site_hint`].
    pub hardware_capable: bool,
}

/// `ActionEditorViewModel.Library`, in order.
pub static LIBRARY: &[ActionTemplate] = &[
    ActionTemplate {
        name: "Default (no change)",
        category: "General",
        kind: "default",
        description: "Leave the control's normal behaviour",
        hardware_capable: true,
    },
    ActionTemplate {
        name: "Disabled",
        category: "General",
        kind: "disabled",
        description: "The control does nothing",
        hardware_capable: true,
    },
    ActionTemplate {
        name: "Key / shortcut",
        category: "Keys",
        kind: "key",
        description: "A key with optional Ctrl/Shift/Alt/Win, held while the control is held",
        hardware_capable: true,
    },
    ActionTemplate {
        name: "Key sequence",
        category: "Keys",
        kind: "chord",
        description: "Several shortcuts in a row, e.g. Ctrl+K, Ctrl+C (comma separated)",
        hardware_capable: false,
    },
    ActionTemplate {
        name: "Type text",
        category: "Text",
        kind: "text",
        description: "Types the text (Unicode)",
        hardware_capable: false,
    },
    ActionTemplate {
        name: "Mouse button",
        category: "Mouse",
        kind: "mouseButton",
        description: "Left / right / middle / back / forward click",
        hardware_capable: true,
    },
    ActionTemplate {
        name: "Scroll wheel",
        category: "Mouse",
        kind: "wheel",
        description: "Scroll up/down/left/right",
        hardware_capable: false,
    },
    ActionTemplate {
        name: "Media control",
        category: "Media",
        kind: "media",
        description: "Play/pause, next, previous, volume, mute",
        hardware_capable: false,
    },
    ActionTemplate {
        name: "Open program / file / URL",
        category: "System",
        kind: "launch",
        description: "Starts with your normal (non-elevated) rights",
        hardware_capable: false,
    },
    ActionTemplate {
        name: "Run macro",
        category: "Macros",
        kind: "macro",
        description: "Software macro from the Macros tab",
        hardware_capable: false,
    },
    ActionTemplate {
        name: "DPI shift (hold)",
        category: "DPI",
        kind: "dpiShift",
        description: "Profile shift DPI while held, restored on release",
        hardware_capable: true,
    },
    ActionTemplate {
        name: "DPI next / previous / cycle",
        category: "DPI",
        kind: "dpi",
        description: "Step through enabled DPI stages",
        hardware_capable: true,
    },
    ActionTemplate {
        name: "DPI stage",
        category: "DPI",
        kind: "dpiStage",
        description: "Jump to a specific DPI stage",
        hardware_capable: false,
    },
    ActionTemplate {
        name: "G-Shift (hold)",
        category: "DPI",
        kind: "gshift",
        description: "While held, controls use their G-Shift binding layer",
        hardware_capable: true,
    },
    ActionTemplate {
        name: "Switch profile",
        category: "Profiles",
        kind: "profile",
        description: "Next / previous profile or lock toggle",
        hardware_capable: true,
    },
    ActionTemplate {
        name: "G-key bank",
        category: "Profiles",
        kind: "bank",
        description: "Select M1/M2/M3 or cycle banks",
        hardware_capable: false,
    },
    ActionTemplate {
        name: "Lighting brightness",
        category: "Lighting",
        kind: "lighting",
        description: "Keyboard brightness up/down/off",
        hardware_capable: false,
    },
];

/// `ActionEditorViewModel.MouseButtons` - named by the action's own `Describe`, so the list and the
/// binding row always agree on what a button is called.
pub static MOUSE_BUTTONS: &[MouseButtonKind] = &[
    MouseButtonKind::Left,
    MouseButtonKind::Right,
    MouseButtonKind::Middle,
    MouseButtonKind::XButton1,
    MouseButtonKind::XButton2,
];

/// `Wheels`: the four directions, encoded the way the C# did. 121 marks the horizontal pair, and
/// the sign is the direction; [`ActionEditor::build`] turns that back into (delta, horizontal).
pub static WHEELS: &[(&str, i32)] = &[
    ("Scroll up", 120),
    ("Scroll down", -120),
    ("Scroll right", 121),
    ("Scroll left", -121),
];

pub static MEDIA_COMMANDS: &[MediaCommand] = &[
    MediaCommand::PlayPause,
    MediaCommand::Stop,
    MediaCommand::Next,
    MediaCommand::Previous,
    MediaCommand::VolumeUp,
    MediaCommand::VolumeDown,
    MediaCommand::Mute,
];

/// `DpiCommands`. Deliberately not every `DpiCommand`: `Shift` and `Stage` have their own
/// templates, so offering them here too would give two ways to build the same action.
pub static DPI_COMMANDS: &[(&str, DpiCommand)] = &[
    ("Next stage", DpiCommand::Next),
    ("Previous stage", DpiCommand::Previous),
    ("Cycle stages", DpiCommand::Cycle),
    ("Default stage", DpiCommand::Default),
];

/// `ProfileCommands`. `Select` is left out for the same reason: it needs a profile id the editor
/// has no field for.
pub static PROFILE_COMMANDS: &[(&str, ProfileCommand)] = &[
    ("Next profile", ProfileCommand::Next),
    ("Previous profile", ProfileCommand::Previous),
    ("Toggle profile lock", ProfileCommand::ToggleLock),
];

pub static LIGHTING_COMMANDS: &[LightingCommand] = &[
    LightingCommand::BrightnessUp,
    LightingCommand::BrightnessDown,
    LightingCommand::ToggleOff,
    LightingCommand::BrightnessCycle,
];

/// The editor's state: one field per template's parameters, all of them live at once.
///
/// The C# did the same, and it is worth keeping: switching template and switching back leaves the
/// half-typed key name and the ticked modifiers where they were, which is what makes trying two
/// candidate bindings bearable.
pub struct ActionEditor {
    pub search: String,
    pub template: usize,
    pub key_text: String,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub win: bool,
    /// Shared by "Key sequence" and "Type text", as `Text` was in the C#.
    pub text: String,
    pub mouse_button: MouseButtonKind,
    pub wheel: i32,
    pub media: MediaCommand,
    pub target: String,
    pub macro_id: Option<Id>,
    pub dpi_command: DpiCommand,
    pub dpi_stage: i64,
    pub profile_command: ProfileCommand,
    pub bank: i64,
    pub lighting_command: LightingCommand,
    /// Set by a failed [`Self::build`], shown under the form. `FormatException` in the C#.
    pub error: Option<String>,
    /// The document's macros, `(name, id)`, reloaded whenever the document changes.
    pub macros: Vec<(String, Id)>,
    dpi_stage_buffer: String,
    bank_buffer: String,
    /// G HUB's ready-made assignments; a pick loads into the form above.
    presets: super::preset_picker::PresetPicker,
}

impl Default for ActionEditor {
    fn default() -> Self {
        Self {
            search: String::new(),
            template: 0,
            key_text: String::new(),
            ctrl: false,
            shift: false,
            alt: false,
            win: false,
            text: String::new(),
            mouse_button: MouseButtonKind::Left,
            wheel: 120,
            media: MediaCommand::PlayPause,
            target: String::new(),
            macro_id: None,
            dpi_command: DpiCommand::Next,
            dpi_stage: 1,
            profile_command: ProfileCommand::Next,
            bank: 0,
            lighting_command: LightingCommand::BrightnessUp,
            error: None,
            macros: Vec::new(),
            dpi_stage_buffer: "1".to_owned(),
            bank_buffer: "0".to_owned(),
            presets: Default::default(),
        }
    }
}

impl ActionEditor {
    /// The editor with a macro list, which is what the Macros page's own binding editor would
    /// hand it. Only the tests build one this way today - every page that shows the editor fills
    /// `macros` from the document as it draws - so it is compiled for them rather than left as an
    /// unused constructor.
    #[cfg(test)]
    pub fn with_macros(macros: Vec<(String, Id)>) -> Self {
        let macro_id = macros.first().map(|(_, id)| *id);
        Self { macros, macro_id, ..Self::default() }
    }

    pub fn kind(&self) -> &'static str {
        LIBRARY.get(self.template).map_or("default", |t| t.kind)
    }

    fn selected(&self) -> &'static ActionTemplate {
        LIBRARY.get(self.template).unwrap_or(&LIBRARY[0])
    }

    /// `FilteredLibrary`: name, category or description, case-insensitively.
    pub fn filtered(&self) -> Vec<usize> {
        let needle = self.search.trim().to_lowercase();
        LIBRARY
            .iter()
            .enumerate()
            .filter(|(_, template)| {
                needle.is_empty()
                    || template.name.to_lowercase().contains(&needle)
                    || template.category.to_lowercase().contains(&needle)
                    || template.description.to_lowercase().contains(&needle)
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// `SiteHint`.
    pub fn site_hint(&self) -> &'static str {
        if self.selected().hardware_capable {
            "Runs in software here; can also be stored on the device where onboard memory supports it."
        } else {
            "Software only: runs while this program is running."
        }
    }

    fn select_kind(&mut self, kind: &str) {
        if let Some(index) = LIBRARY.iter().position(|t| t.kind == kind) {
            self.template = index;
        }
    }

    fn modifiers(&self) -> KeyModifiers {
        let mut mods = KeyModifiers::NONE;
        if self.ctrl {
            mods |= KeyModifiers::CONTROL;
        }
        if self.shift {
            mods |= KeyModifiers::SHIFT;
        }
        if self.alt {
            mods |= KeyModifiers::ALT;
        }
        if self.win {
            mods |= KeyModifiers::WINDOWS;
        }
        mods
    }

    /// `Load(ActionDefinition?)`: puts an existing binding back into the form.
    pub fn load(&mut self, action: Option<&ActionDefinition>) {
        self.error = None;
        let Some(action) = action else {
            self.template = 0;
            return;
        };
        match action {
            ActionDefinition::Key(stroke) => {
                self.select_kind("key");
                self.key_text = key_name(stroke.virtual_key);
                self.ctrl = stroke.modifiers.contains(KeyModifiers::CONTROL);
                self.shift = stroke.modifiers.contains(KeyModifiers::SHIFT);
                self.alt = stroke.modifiers.contains(KeyModifiers::ALT);
                self.win = stroke.modifiers.contains(KeyModifiers::WINDOWS);
            }
            ActionDefinition::Chord { steps } => {
                self.select_kind("chord");
                self.text =
                    steps.iter().map(KeyStroke::describe).collect::<Vec<_>>().join(", ");
            }
            ActionDefinition::Text { text } => {
                self.select_kind("text");
                self.text = text.clone();
            }
            ActionDefinition::MouseButton { button } => {
                self.select_kind("mouseButton");
                self.mouse_button = *button;
            }
            ActionDefinition::MouseWheel { delta, horizontal } => {
                self.select_kind("wheel");
                // Back to the encoded form: sign carries the direction, magnitude the axis.
                self.wheel = delta.signum() * if *horizontal { 121 } else { 120 };
            }
            ActionDefinition::Media { command } => {
                self.select_kind("media");
                self.media = *command;
            }
            ActionDefinition::Launch { target, .. } => {
                self.select_kind("launch");
                self.target = target.clone();
            }
            ActionDefinition::MacroRef { macro_id } => {
                self.select_kind("macro");
                // Only if the macro still exists; a binding to a deleted macro must not make the
                // combo show a name that is no longer in the document.
                self.macro_id = self
                    .macros
                    .iter()
                    .find(|(_, id)| id == macro_id)
                    .map(|(_, id)| *id);
            }
            ActionDefinition::Dpi { command: DpiCommand::Shift, .. } => {
                self.select_kind("dpiShift");
            }
            ActionDefinition::Dpi { command: DpiCommand::Stage, stage } => {
                self.select_kind("dpiStage");
                // Stored zero-based, shown one-based.
                self.dpi_stage = *stage as i64 + 1;
                self.dpi_stage_buffer = self.dpi_stage.to_string();
            }
            ActionDefinition::Dpi { command, .. } => {
                self.select_kind("dpi");
                self.dpi_command =
                    if DPI_COMMANDS.iter().any(|(_, c)| c == command) { *command } else { DpiCommand::Next };
            }
            ActionDefinition::GShift => self.select_kind("gshift"),
            ActionDefinition::Profile { command, .. } => {
                self.select_kind("profile");
                self.profile_command = if PROFILE_COMMANDS.iter().any(|(_, c)| c == command) {
                    *command
                } else {
                    ProfileCommand::Next
                };
            }
            ActionDefinition::Bank { bank } => {
                self.select_kind("bank");
                self.bank = *bank as i64;
                self.bank_buffer = self.bank.to_string();
            }
            ActionDefinition::Lighting { command } => {
                self.select_kind("lighting");
                self.lighting_command = *command;
            }
            ActionDefinition::Disabled => self.select_kind("disabled"),
            ActionDefinition::PassThrough => self.template = 0,
        }
    }

    /// `Build()`: the action, or `None` with [`Self::error`] set.
    ///
    /// Invalid input returns `None` rather than a `PassThrough`, because silently binding "nothing"
    /// to a key the user was trying to configure is worse than refusing.
    pub fn build(&mut self) -> Option<ActionDefinition> {
        self.error = None;
        let built = match self.kind() {
            "default" => Ok(ActionDefinition::PassThrough),
            "disabled" => Ok(ActionDefinition::Disabled),
            "key" => parse_key(&self.key_text)
                .map(|vk| ActionDefinition::Key(KeyStroke::with(vk, self.modifiers()))),
            "chord" => {
                let steps: Result<Vec<KeyStroke>, String> = self
                    .text
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(parse_shortcut)
                    .collect();
                steps.map(|steps| ActionDefinition::Chord { steps })
            }
            "text" => {
                if self.text.is_empty() {
                    Err("Enter the text to type".to_owned())
                } else {
                    Ok(ActionDefinition::Text { text: self.text.clone() })
                }
            }
            "mouseButton" => Ok(ActionDefinition::MouseButton { button: self.mouse_button }),
            "wheel" => Ok(ActionDefinition::MouseWheel {
                delta: self.wheel.signum() * 120,
                horizontal: self.wheel.abs() == 121,
            }),
            "media" => Ok(ActionDefinition::Media { command: self.media }),
            "launch" => {
                if self.target.trim().is_empty() {
                    Err("Enter a program, file or URL".to_owned())
                } else {
                    Ok(ActionDefinition::Launch {
                        target: self.target.trim().to_owned(),
                        arguments: None,
                    })
                }
            }
            "macro" => match self.macro_id {
                Some(macro_id) => Ok(ActionDefinition::MacroRef { macro_id }),
                None => Err("Create a macro first (Macros tab)".to_owned()),
            },
            "dpiShift" => Ok(ActionDefinition::Dpi { command: DpiCommand::Shift, stage: 0 }),
            "dpi" => Ok(ActionDefinition::Dpi { command: self.dpi_command, stage: 0 }),
            "dpiStage" => Ok(ActionDefinition::Dpi {
                command: DpiCommand::Stage,
                stage: (self.dpi_stage.clamp(1, 5) - 1) as usize,
            }),
            "gshift" => Ok(ActionDefinition::GShift),
            "profile" => {
                Ok(ActionDefinition::Profile { command: self.profile_command, profile_id: None })
            }
            "bank" => Ok(ActionDefinition::Bank { bank: self.bank.clamp(0, 3) as usize }),
            "lighting" => Ok(ActionDefinition::Lighting { command: self.lighting_command }),
            _ => Ok(ActionDefinition::PassThrough),
        };

        match built {
            Ok(action) => Some(action),
            Err(message) => {
                self.error = Some(message);
                None
            }
        }
    }

    /// Draws the form. Returns true on any edit, which the host page uses to clear a stale error.
    pub fn show(&mut self, ui: &mut egui::Ui, icons: &mut IconCache, enabled: bool) -> bool {
        let mut changed = false;

        // G HUB's own lists first: picking one fills in the form, so Apply works as usual.
        if let Some(action) = self.presets.show(ui, icons, enabled) {
            self.load(Some(&action));
            changed = true;
        }
        ui.add_space(6.0);

        // `<TextBox Watermark="Search actions…">`.
        w::label(ui, "SEARCH ACTIONS");
        changed |= c::text_field(ui, &mut self.search, ui.available_width(), enabled);
        ui.add_space(6.0);

        // The combo lists only what the search matched, but `template` indexes the whole library,
        // so the pick is mapped back through `filtered`.
        let shown = self.filtered();
        let mut picked = self.template;
        if !shown.is_empty() && !shown.contains(&picked) {
            // The search no longer matches what is selected. The C# left the selection alone and
            // showed an empty combo; showing the first match instead keeps the form consistent
            // with what the list says is chosen.
            picked = shown[0];
        }
        if c::combo(ui, icons, "action template", &mut picked, &shown, |index| {
            let template = &LIBRARY[*index];
            format!("{}  ·  {}", template.name, template.category)
        }, enabled)
        {
            self.template = picked;
            changed = true;
        } else if picked != self.template {
            self.template = picked;
        }

        ui.add_space(4.0);
        w::body_small(ui, self.selected().description, t::TEXT_LABEL);
        ui.add_space(6.0);

        match self.kind() {
            "key" => {
                w::label(ui, "KEY (F5, ENTER, A, NUM 4, SPACE…)");
                changed |= c::text_field(ui, &mut self.key_text, ui.available_width(), enabled);
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    changed |= c::checkbox(ui, icons, &mut self.ctrl, "Ctrl", enabled);
                    ui.add_space(8.0);
                    changed |= c::checkbox(ui, icons, &mut self.shift, "Shift", enabled);
                    ui.add_space(8.0);
                    changed |= c::checkbox(ui, icons, &mut self.alt, "Alt", enabled);
                    ui.add_space(8.0);
                    changed |= c::checkbox(ui, icons, &mut self.win, "Win", enabled);
                });
            }
            "chord" => {
                w::label(ui, "SEQUENCE (CTRL+K, CTRL+C)");
                changed |= c::text_field(ui, &mut self.text, ui.available_width(), enabled);
            }
            "text" => {
                w::label(ui, "TEXT TO TYPE");
                changed |= c::text_area(ui, &mut self.text, 3, enabled);
            }
            "mouseButton" => {
                changed |= c::combo(
                    ui,
                    icons,
                    "action mouse button",
                    &mut self.mouse_button,
                    MOUSE_BUTTONS,
                    |button| ActionDefinition::MouseButton { button: *button }.describe(),
                    enabled,
                );
            }
            "wheel" => {
                changed |= c::combo(
                    ui,
                    icons,
                    "action wheel",
                    &mut self.wheel,
                    &WHEELS.iter().map(|(_, v)| *v).collect::<Vec<_>>(),
                    |value| {
                        WHEELS
                            .iter()
                            .find(|(_, v)| v == value)
                            .map_or_else(String::new, |(name, _)| (*name).to_owned())
                    },
                    enabled,
                );
            }
            "media" => {
                changed |= c::combo(
                    ui,
                    icons,
                    "action media",
                    &mut self.media,
                    MEDIA_COMMANDS,
                    |command| ActionDefinition::Media { command: *command }.describe(),
                    enabled,
                );
            }
            "launch" => {
                w::label(ui, "PROGRAM, FILE OR URL");
                changed |= c::text_field(ui, &mut self.target, ui.available_width(), enabled);
            }
            "macro" => {
                if self.macros.is_empty() {
                    w::body_small(
                        ui,
                        "No macros in this document yet. Create one on the Macros page.",
                        t::TEXT_LABEL,
                    );
                } else {
                    let ids: Vec<Id> = self.macros.iter().map(|(_, id)| *id).collect();
                    let mut selected = self.macro_id.unwrap_or(ids[0]);
                    let names = self.macros.clone();
                    if c::combo(
                        ui,
                        icons,
                        "action macro",
                        &mut selected,
                        &ids,
                        |id| {
                            names
                                .iter()
                                .find(|(_, candidate)| candidate == id)
                                .map_or_else(|| "(deleted macro)".to_owned(), |(name, _)| name.clone())
                        },
                        enabled,
                    ) {
                        changed = true;
                    }
                    self.macro_id = Some(selected);
                }
            }
            "dpi" => {
                changed |= c::combo(
                    ui,
                    icons,
                    "action dpi command",
                    &mut self.dpi_command,
                    &DPI_COMMANDS.iter().map(|(_, c)| *c).collect::<Vec<_>>(),
                    |command| {
                        DPI_COMMANDS
                            .iter()
                            .find(|(_, c)| c == command)
                            .map_or_else(String::new, |(name, _)| (*name).to_owned())
                    },
                    enabled,
                );
            }
            "dpiStage" => {
                changed |= c::field_row(ui, "Stage", |ui| {
                    c::numeric(
                        ui,
                        icons,
                        &mut self.dpi_stage_buffer,
                        &mut self.dpi_stage,
                        1..=5,
                        1,
                        enabled,
                    )
                });
            }
            "profile" => {
                changed |= c::combo(
                    ui,
                    icons,
                    "action profile command",
                    &mut self.profile_command,
                    &PROFILE_COMMANDS.iter().map(|(_, c)| *c).collect::<Vec<_>>(),
                    |command| {
                        PROFILE_COMMANDS
                            .iter()
                            .find(|(_, c)| c == command)
                            .map_or_else(String::new, |(name, _)| (*name).to_owned())
                    },
                    enabled,
                );
            }
            "bank" => {
                changed |= c::field_row(ui, "Bank (0 = cycle)", |ui| {
                    c::numeric(ui, icons, &mut self.bank_buffer, &mut self.bank, 0..=3, 1, enabled)
                });
            }
            "lighting" => {
                changed |= c::combo(
                    ui,
                    icons,
                    "action lighting command",
                    &mut self.lighting_command,
                    LIGHTING_COMMANDS,
                    |command| format!("{command:?}"),
                    enabled,
                );
            }
            _ => {}
        }

        ui.add_space(6.0);
        w::body_small(ui, self.site_hint(), t::TEXT_LABEL);
        if let Some(error) = &self.error {
            w::body_small(ui, error, t::ERROR);
        }
        changed
    }
}

/// `ParseKey`: accepts a key name as the app shows it, or an explicit `VK 0x41`.
///
/// The reverse lookup walks every virtual key rather than keeping a name-to-code map, exactly as
/// the C# did. 254 string comparisons once per Build is not worth a second table that could fall
/// out of step with [`key_name`].
pub fn parse_key(text: &str) -> Result<u16, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Enter a key (e.g. F5, Enter, A)".to_owned());
    }
    if let Some(hex) = text.strip_prefix("VK 0x").or_else(|| text.strip_prefix("vk 0x")) {
        if let Ok(vk) = u16::from_str_radix(hex.trim(), 16) {
            return Ok(vk);
        }
    }
    for vk in 1u16..0xFF {
        if key_name(vk).eq_ignore_ascii_case(text) {
            return Ok(vk);
        }
    }
    Err(format!("Unknown key '{text}'"))
}

/// `ParseShortcut`: one step of a key sequence, such as `Ctrl+Shift+K`.
pub fn parse_shortcut(text: &str) -> Result<KeyStroke, String> {
    let parts: Vec<&str> =
        text.split('+').map(str::trim).filter(|s| !s.is_empty()).collect();
    let Some((key, modifiers)) = parts.split_last() else {
        return Err("Enter a key (e.g. F5, Enter, A)".to_owned());
    };

    let mut mods = KeyModifiers::NONE;
    for part in modifiers {
        mods |= match part.to_lowercase().as_str() {
            "ctrl" | "control" => KeyModifiers::CONTROL,
            "shift" => KeyModifiers::SHIFT,
            "alt" => KeyModifiers::ALT,
            "win" | "windows" => KeyModifiers::WINDOWS,
            _ => return Err(format!("Unknown modifier '{part}'")),
        };
    }
    Ok(KeyStroke::with(parse_key(key)?, mods))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_is_the_seventeen_entries_the_c_sharp_listed() {
        assert_eq!(LIBRARY.len(), 17);
        // The first two are what an unconfigured and a deliberately dead control get, and the
        // editor opens on the first, so their order is load-bearing.
        assert_eq!(LIBRARY[0].kind, "default");
        assert_eq!(LIBRARY[1].kind, "disabled");
    }

    #[test]
    fn every_template_kind_builds_something() {
        for (index, template) in LIBRARY.iter().enumerate() {
            let mut editor = ActionEditor {
                template: index,
                // Enough input to satisfy the templates that validate.
                key_text: "F5".to_owned(),
                text: "Ctrl+K, Ctrl+C".to_owned(),
                target: "notepad.exe".to_owned(),
                macro_id: Some(Id::new()),
                ..ActionEditor::default()
            };
            assert!(
                editor.build().is_some(),
                "{} produced nothing from valid input: {:?}",
                template.kind,
                editor.error
            );
        }
    }

    #[test]
    fn a_round_trip_through_load_keeps_the_action() {
        let cases = [
            ActionDefinition::Key(KeyStroke::with(0x74, KeyModifiers::CONTROL)),
            ActionDefinition::Text { text: "hello".to_owned() },
            ActionDefinition::MouseButton { button: MouseButtonKind::XButton1 },
            ActionDefinition::MouseWheel { delta: -120, horizontal: true },
            ActionDefinition::Media { command: MediaCommand::Mute },
            ActionDefinition::Launch { target: "notepad.exe".to_owned(), arguments: None },
            ActionDefinition::Dpi { command: DpiCommand::Shift, stage: 0 },
            ActionDefinition::Dpi { command: DpiCommand::Stage, stage: 2 },
            ActionDefinition::Dpi { command: DpiCommand::Cycle, stage: 0 },
            ActionDefinition::GShift,
            ActionDefinition::Profile { command: ProfileCommand::ToggleLock, profile_id: None },
            ActionDefinition::Bank { bank: 2 },
            ActionDefinition::Lighting { command: LightingCommand::ToggleOff },
            ActionDefinition::Disabled,
            ActionDefinition::PassThrough,
        ];

        for action in cases {
            let mut editor = ActionEditor::default();
            editor.load(Some(&action));
            assert_eq!(editor.build(), Some(action.clone()), "round trip lost {action:?}");
        }
    }

    /// The wheel is the one case where the stored form and the editor's form differ, so it gets its
    /// own check: 121 is a marker for the horizontal axis, never a delta.
    #[test]
    fn the_wheel_encoding_survives_both_directions_of_both_axes() {
        for (delta, horizontal) in [(120, false), (-120, false), (120, true), (-120, true)] {
            let action = ActionDefinition::MouseWheel { delta, horizontal };
            let mut editor = ActionEditor::default();
            editor.load(Some(&action));
            assert_eq!(editor.wheel.abs(), if horizontal { 121 } else { 120 });
            assert_eq!(editor.build(), Some(action));
        }
    }

    #[test]
    fn a_chord_round_trips_through_its_own_text() {
        let steps = vec![
            KeyStroke::with(parse_key("K").unwrap(), KeyModifiers::CONTROL),
            KeyStroke::with(parse_key("C").unwrap(), KeyModifiers::CONTROL),
        ];
        let action = ActionDefinition::Chord { steps: steps.clone() };
        let mut editor = ActionEditor::default();
        editor.load(Some(&action));
        // The text is what `Describe` produced, and parsing it back has to give the same strokes -
        // otherwise opening a chord binding and pressing Apply would change it.
        assert_eq!(editor.build(), Some(ActionDefinition::Chord { steps }));
    }

    #[test]
    fn an_unknown_key_is_reported_rather_than_guessed() {
        let mut editor = ActionEditor {
            template: LIBRARY.iter().position(|t| t.kind == "key").unwrap(),
            key_text: "Zorkmid".to_owned(),
            ..ActionEditor::default()
        };
        assert_eq!(editor.build(), None);
        assert_eq!(editor.error.as_deref(), Some("Unknown key 'Zorkmid'"));
    }

    #[test]
    fn an_empty_key_asks_for_one() {
        assert_eq!(parse_key("  "), Err("Enter a key (e.g. F5, Enter, A)".to_owned()));
    }

    #[test]
    fn a_raw_virtual_key_is_accepted_so_an_unnamed_key_is_still_bindable() {
        assert_eq!(parse_key("VK 0x41"), Ok(0x41));
        assert_eq!(parse_key("vk 0x5B"), Ok(0x5B));
    }

    #[test]
    fn an_unknown_modifier_names_itself() {
        assert_eq!(parse_shortcut("Hyper+K"), Err("Unknown modifier 'Hyper'".to_owned()));
    }

    #[test]
    fn the_search_matches_name_category_and_description() {
        let mut editor = ActionEditor { search: "dpi".to_owned(), ..ActionEditor::default() };
        let by_category = editor.filtered();
        assert_eq!(by_category.len(), 4, "the four DPI templates");

        editor.search = "unicode".to_owned();
        let by_description = editor.filtered();
        assert_eq!(by_description.len(), 1);
        assert_eq!(LIBRARY[by_description[0]].kind, "text");

        editor.search = String::new();
        assert_eq!(editor.filtered().len(), LIBRARY.len());
    }

    #[test]
    fn the_site_hint_distinguishes_onboard_capable_actions() {
        let mut editor = ActionEditor::default();
        editor.select_kind("key");
        assert!(editor.site_hint().contains("stored on the device"));
        editor.select_kind("text");
        assert!(editor.site_hint().starts_with("Software only"));
    }

    #[test]
    fn a_macro_binding_whose_macro_was_deleted_does_not_point_at_another_one() {
        let kept = Id::new();
        let deleted = Id::new();
        let mut editor = ActionEditor::with_macros(vec![("Kept".to_owned(), kept)]);
        editor.load(Some(&ActionDefinition::MacroRef { macro_id: deleted }));
        // Not silently repointed at "Kept": the user would have saved a binding to a macro they
        // never chose.
        assert_eq!(editor.macro_id, None);
        assert_eq!(editor.build(), None);
        assert_eq!(editor.error.as_deref(), Some("Create a macro first (Macros tab)"));
    }
}
