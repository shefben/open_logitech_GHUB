//! Per-application lighting. Port of `AppLightingTabViewModel.cs` and `AppLightingTabView.axaml`.
//!
//! While a program is in the foreground its rule replaces the lighting the active profile would
//! give each device - and nothing else. Bindings, DPI and macros stay exactly as the profile has
//! them, because this is the lighting-only half of what an application rule can do; the other half
//! lives on the Profiles page. Saying so on the page matters: "why did my DPI not change" is the
//! obvious question otherwise.

use std::collections::{HashMap, HashSet};

use egui::{Color32, Ui, Vec2};
use ghub_core::Id;
use ghub_core::profile::{AppLightingOverride, AppLightingRule, LightingMode, is_hex_color};
use ghub_hidpp::identity::PeripheralKind;
use ghub_platform::apps::RunningApp;

use crate::color_picker::parse_hex;
use crate::controls as c;
use crate::icons::IconCache;
use crate::shared::{Changes, Shared};
use crate::theme as t;
use crate::widgets as w;

use super::keyboard::{KeyboardPage, QUICK_GROUP_NAMES};
use super::{Confirm, scroll, status_band};

/// `AppLightingItem` - one rule in the list.
struct RuleItem {
    id: Id,
    name: String,
    executable_name: String,
    enabled: bool,
    devices: usize,
    /// Whether this rule is the one currently deciding some device's lighting.
    matching: bool,
}

impl RuleItem {
    /// `Title` - the rule's name, or the executable when it has none.
    fn title(&self) -> &str {
        if self.name.is_empty() { &self.executable_name } else { &self.name }
    }

    /// `Badges`.
    fn badges(&self) -> String {
        let mut parts = vec![self.executable_name.clone()];
        parts.push(if self.devices == 1 {
            "1 device".to_owned()
        } else {
            format!("{} devices", self.devices)
        });
        if !self.enabled {
            parts.push("off".to_owned());
        }
        if self.matching {
            parts.push("RUNNING NOW".to_owned());
        }
        parts.join(" \u{b7} ")
    }
}

/// `AppLightingDeviceRow` - a device this rule can light, with the colours it gives it.
struct DeviceRow {
    device_key: String,
    display_name: String,
    /// Keyboards colour a key group; mice colour their two zones. The row shows only what applies.
    is_keyboard: bool,
    included: bool,
    primary_hex: String,
    primary_buffer: String,
    secondary_hex: String,
    secondary_buffer: String,
    brightness: i64,
    brightness_buffer: String,
    key_group: String,
}

impl DeviceRow {
    fn primary_label(&self) -> &'static str {
        if self.is_keyboard { "KEYS" } else { "PRIMARY ZONE" }
    }

    fn secondary_label(&self) -> &'static str {
        if self.is_keyboard { "EVERY OTHER KEY" } else { "LOGO ZONE" }
    }
}

#[derive(Default)]
struct Pending {
    add: bool,
    save: bool,
    delete: bool,
    duplicate: bool,
    refresh_apps: bool,
    use_current_colors: bool,
}

#[derive(Default)]
pub struct AppLightingPage {
    rules: Vec<RuleItem>,
    selected_rule: Option<Id>,
    devices: Vec<DeviceRow>,
    apps: Vec<RunningApp>,
    picked_app: Option<String>,

    rule_name: String,
    rule_executable: String,
    rule_enabled: bool,
    rule_priority: i64,
    priority_buffer: String,

    /// `Foreground` - what is in front right now, so a rule that is not matching can be checked
    /// against something.
    foreground: String,
    confirm: Confirm,
    loaded: bool,
}

impl AppLightingPage {
    fn selected(&self) -> Option<&RuleItem> {
        let id = self.selected_rule?;
        self.rules.iter().find(|rule| rule.id == id)
    }

    /// `Refresh()`.
    fn reload(&mut self, shared: &Shared) {
        let keep = self.selected_rule;
        let document = shared.document();
        let devices = shared.host.devices.devices();

        self.rules = document
            .app_lighting
            .iter()
            .map(|rule| RuleItem {
                id: rule.id,
                name: rule.name.clone(),
                executable_name: rule.executable_name.clone(),
                enabled: rule.enabled,
                devices: rule.devices.len(),
                matching: devices.iter().any(|device| {
                    shared
                        .service()
                        .matching_app_lighting(&device.key())
                        .is_some_and(|(matched, _)| matched.id == rule.id)
                }),
            })
            .collect();

        self.selected_rule = keep
            .filter(|id| self.rules.iter().any(|rule| rule.id == *id))
            .or_else(|| self.rules.first().map(|rule| rule.id));

        self.foreground = match shared.service().foreground_executable() {
            Some(exe) if !exe.is_empty() => format!("Foreground now: {exe}"),
            _ => "Foreground program unknown".to_owned(),
        };

        if self.apps.is_empty() {
            self.apps = ghub_platform::apps::running_apps();
        }
        self.load_selected(shared);
        self.loaded = true;
    }

    /// `LoadSelected()` - fills the editor and lists every managed device the rule could light.
    fn load_selected(&mut self, shared: &Shared) {
        self.devices.clear();
        let document = shared.document();
        let Some(rule) = self
            .selected_rule
            .and_then(|id| document.app_lighting.iter().find(|rule| rule.id == id))
        else {
            self.rule_name.clear();
            self.rule_executable.clear();
            self.rule_enabled = true;
            self.rule_priority = 0;
            self.priority_buffer = "0".to_owned();
            return;
        };

        self.rule_name = rule.name.clone();
        self.rule_executable = rule.executable_name.clone();
        self.rule_enabled = rule.enabled;
        self.rule_priority = rule.priority as i64;
        self.priority_buffer = self.rule_priority.to_string();

        let mut devices: Vec<_> = shared
            .host
            .devices
            .devices()
            .into_iter()
            .filter(|device| device.descriptor.is_some())
            .collect();
        devices.sort_by_key(|device| {
            device.descriptor.as_ref().map(|d| d.display_name.clone()).unwrap_or_default()
        });

        for device in devices {
            let descriptor = device.descriptor.as_ref().expect("filtered above");
            let is_keyboard = descriptor.kind == PeripheralKind::Keyboard;
            let over = rule.devices.get(&device.key());
            let primary = primary_of(over);
            let secondary = over
                .and_then(|o| o.default_color.clone())
                .unwrap_or_else(|| "#000000".to_owned());
            let brightness = (over.and_then(|o| o.brightness).unwrap_or(1.0) * 100.0).round() as i64;

            self.devices.push(DeviceRow {
                device_key: device.key(),
                display_name: descriptor.display_name.clone(),
                is_keyboard,
                included: over.is_some(),
                primary_buffer: primary.clone(),
                primary_hex: primary,
                secondary_buffer: secondary.clone(),
                secondary_hex: secondary,
                brightness,
                brightness_buffer: brightness.to_string(),
                key_group: if is_keyboard { group_of(over) } else { "ALL".to_owned() },
            });
        }
    }

    /// `LoadColorsFromProfile()` - seeds the rule's colours from what the profile gives each device
    /// now, which is almost always the starting point the user wants.
    fn load_colors_from_profile(&mut self, shared: &mut Shared) {
        let profile = shared.active_profile();
        for row in &mut self.devices {
            let Some(lighting) = profile.device(&row.device_key).map(|s| &s.lighting) else {
                continue;
            };
            row.secondary_hex = lighting.default_color.clone();
            row.secondary_buffer = row.secondary_hex.clone();
            row.primary_hex = lighting
                .static_colors
                .get(if row.is_keyboard { "key:W" } else { "zone:0" })
                .cloned()
                .unwrap_or_else(|| lighting.default_color.clone());
            row.primary_buffer = row.primary_hex.clone();
            row.brightness = (lighting.brightness * 100.0).round() as i64;
            row.brightness_buffer = row.brightness.to_string();
        }
        shared.status =
            Some("Colours taken from the active profile; press Save to store them in this rule."
                .to_owned());
    }
}

/// `PrimaryOf(over, keyboard)`.
fn primary_of(over: Option<&AppLightingOverride>) -> String {
    let Some(over) = over else { return "#00AAFF".to_owned() };
    // `StaticColors.Values.FirstOrDefault()`. A `HashMap`'s iteration order is not stable, so the
    // lowest key is taken instead - the C# got whichever the dictionary happened to yield, and
    // showing a different swatch on each reload would look like a bug.
    let mut keys: Vec<&String> = over.static_colors.keys().collect();
    keys.sort();
    keys.first()
        .and_then(|key| over.static_colors.get(*key).cloned())
        .or_else(|| over.default_color.clone())
        .unwrap_or_else(|| "#00AAFF".to_owned())
}

/// `GroupOf(over)` - which quick group the stored key set came from.
///
/// A rule stores plain colour keys, so the group is recovered by matching the key set against the
/// quick groups. Anything that matches none is shown as ALL, which is what the C# did; the
/// alternative - inventing a "custom" entry the user cannot choose - would make the combo lie
/// about what Save will write.
fn group_of(over: Option<&AppLightingOverride>) -> String {
    let Some(over) = over.filter(|o| !o.static_colors.is_empty()) else {
        return "ALL".to_owned();
    };
    let keys: HashSet<&str> = over
        .static_colors
        .keys()
        .filter_map(|key| key.strip_prefix("key:"))
        .collect();
    for group in QUICK_GROUP_NAMES {
        let in_group: HashSet<&str> =
            KeyboardPage::quick_group_keys(group).into_iter().collect();
        if keys == in_group {
            return (*group).to_owned();
        }
    }
    "ALL".to_owned()
}

/// `BuildOverride(row)` - a keyboard colours the chosen key group and leaves everything else at
/// the "every other key" colour; a mouse colours its two zones.
fn build_override(row: &DeviceRow) -> AppLightingOverride {
    let mut over = AppLightingOverride {
        // Always `Static`: a lighting rule is a set of colours, and an override that said
        // "Untouched" would be a rule that does nothing.
        mode: Some(LightingMode::Static),
        brightness: Some((row.brightness as f64 / 100.0).clamp(0.0, 1.0)),
        default_color: Some(normalise_hex(&row.secondary_hex)),
        static_colors: HashMap::new(),
        device_effect_id: None,
        animation: None,
    };

    if row.is_keyboard {
        let hex = normalise_hex(&row.primary_hex);
        for key_id in KeyboardPage::quick_group_keys(&row.key_group) {
            over.static_colors.insert(format!("key:{key_id}"), hex.clone());
        }
    } else {
        over.static_colors.insert("zone:0".to_owned(), normalise_hex(&row.primary_hex));
        over.static_colors.insert("zone:1".to_owned(), normalise_hex(&row.secondary_hex));
    }
    over
}

/// `Normalize(hex)`.
fn normalise_hex(text: &str) -> String {
    let trimmed = text.trim();
    let with_hash =
        if trimmed.starts_with('#') { trimmed.to_owned() } else { format!("#{trimmed}") };
    if is_hex_color(&with_hash) { with_hash.to_uppercase() } else { "#FFFFFF".to_owned() }
}

/// The file name without its extension, for naming a rule after the program it matches.
fn stem_of(file_name: &str) -> String {
    match file_name.rfind('.') {
        Some(dot) if dot > 0 => file_name[..dot].to_owned(),
        _ => file_name.to_owned(),
    }
}

// =============================================================== drawing

impl AppLightingPage {
    pub fn show(
        &mut self,
        shared: &mut Shared,
        icons: &mut IconCache,
        ui: &mut Ui,
        changes: Changes,
    ) {
        if changes.document || changes.devices || !self.loaded {
            self.reload(shared);
        }

        // The delete confirmation, answered before anything is drawn so the list it would remove
        // from is already correct.
        if let Some(true) = self.confirm.show(ui.ctx(), icons) {
            if let Some(id) = self.selected_rule {
                shared.edit_document("Delete lighting rule", move |document| {
                    document.app_lighting.retain(|rule| rule.id != id);
                });
                self.selected_rule = None;
            }
        }

        let mut pending = Pending::default();

        scroll(ui, "app lighting", |ui| {
            ui.set_max_width(1200.0);
            w::title(ui, "Lighting per application");
            ui.add_space(8.0);
            w::body(
                ui,
                "While one of these programs is in the foreground, its rule decides what the \
                 keyboard and the mouse look like. The active profile keeps everything else - \
                 bindings, DPI, macros - so this is lighting only. For a program that should \
                 change those as well, give it a profile on the Profiles & rules tab.",
            );
            ui.add_space(6.0);
            w::body_small(ui, &self.foreground, t::TEXT_LABEL);
            ui.add_space(6.0);
            status_band(ui, shared);
            ui.add_space(12.0);

            ui.horizontal_top(|ui| {
                // `ColumnDefinitions="360,*"`.
                ui.allocate_ui_with_layout(
                    Vec2::new(360.0, ui.available_height()),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| self.rules_card(ui, icons, shared, &mut pending),
                );
                ui.add_space(12.0);
                if self.selected().is_some() {
                    ui.allocate_ui_with_layout(
                        ui.available_size(),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| self.rule_card(ui, icons, shared, &mut pending),
                    );
                }
            });
        });

        self.apply(shared, pending);
    }

    fn rules_card(
        &mut self,
        ui: &mut Ui,
        icons: &mut IconCache,
        shared: &Shared,
        pending: &mut Pending,
    ) {
        w::card(ui, |ui| {
            w::title(ui, "Rules");
            ui.add_space(8.0);
            if self.rules.is_empty() {
                w::body_small(
                    ui,
                    "No rules yet. Pick a running program (or type its executable name) and press \
                     Add.",
                    t::TEXT_LABEL,
                );
            }

            let mut select = None;
            egui::ScrollArea::vertical()
                .id_salt("app lighting rules")
                .max_height(300.0)
                .show(ui, |ui| {
                    for rule in &self.rules {
                        let active = self.selected_rule == Some(rule.id);
                        let response = w::anim_row(ui, rule.title(), active);
                        w::body_small(
                            ui,
                            &rule.badges(),
                            // A rule that is deciding something right now is worth seeing at a
                            // glance; one that is switched off should not look active.
                            if rule.matching {
                                t::ACCENT_BRIGHT
                            } else if rule.enabled {
                                t::TEXT_LABEL
                            } else {
                                t::DISABLED
                            },
                        );
                        ui.add_space(4.0);
                        if response.clicked() {
                            select = Some(rule.id);
                        }
                    }
                });
            if let Some(id) = select {
                self.selected_rule = Some(id);
                self.load_selected(shared);
            }

            ui.add_space(10.0);
            w::label(ui, "RUNNING PROGRAMS");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if self.apps.is_empty() {
                    w::body_small(ui, "No windowed programs found.", t::TEXT_LABEL);
                } else {
                    let names: Vec<String> =
                        self.apps.iter().map(|app| app.name.clone()).collect();
                    let paths = self.apps.clone();
                    let mut picked = self.picked_app.clone().unwrap_or_else(|| names[0].clone());
                    if c::combo(
                        ui,
                        icons,
                        "running programs",
                        &mut picked,
                        &names,
                        |name| {
                            // The path disambiguates two programs with the same file name, which
                            // is common enough (several `launcher.exe`) to matter.
                            paths
                                .iter()
                                .find(|app| &app.name == name)
                                .map_or_else(|| name.clone(), |app| app.path.clone())
                        },
                        true,
                    ) {
                        // `OnPickedAppChanged`: fills the executable, and the name too when it is
                        // still blank.
                        self.rule_executable = picked.clone();
                        if self.rule_name.is_empty() {
                            self.rule_name = stem_of(&picked);
                        }
                    }
                    self.picked_app = Some(picked);
                }
                ui.add_space(6.0);
                if c::button(ui, "Refresh", c::ButtonStyle::Text, true).clicked() {
                    pending.refresh_apps = true;
                }
            });

            ui.add_space(10.0);
            let has_selection = self.selected().is_some();
            ui.horizontal(|ui| {
                if c::button(ui, "Add", c::ButtonStyle::Primary, !shared.busy()).clicked() {
                    pending.add = true;
                }
                ui.add_space(6.0);
                if c::button(
                    ui,
                    "Duplicate",
                    c::ButtonStyle::Outlined,
                    has_selection && !shared.busy(),
                )
                .clicked()
                {
                    pending.duplicate = true;
                }
                ui.add_space(6.0);
                if c::button(
                    ui,
                    "Delete",
                    c::ButtonStyle::Text,
                    has_selection && !shared.busy() && !self.confirm.is_open(),
                )
                .clicked()
                {
                    pending.delete = true;
                }
            });
        });
    }

    fn rule_card(
        &mut self,
        ui: &mut Ui,
        icons: &mut IconCache,
        shared: &Shared,
        pending: &mut Pending,
    ) {
        w::card(ui, |ui| {
            w::title(ui, "Rule");
            ui.add_space(10.0);

            // `ColumnDefinitions="140,*"`.
            for (caption, field) in [
                ("Name", Field::Name),
                ("Executable", Field::Executable),
            ] {
                ui.horizontal(|ui| {
                    ui.allocate_ui(Vec2::new(140.0, 20.0), |ui| w::body(ui, caption));
                    let buffer = match field {
                        Field::Name => &mut self.rule_name,
                        Field::Executable => &mut self.rule_executable,
                    };
                    c::text_field(ui, buffer, ui.available_width().min(420.0), true);
                });
                ui.add_space(4.0);
            }
            ui.horizontal(|ui| {
                ui.allocate_ui(Vec2::new(140.0, 20.0), |ui| w::body(ui, "Priority"));
                c::numeric(
                    ui,
                    icons,
                    &mut self.priority_buffer,
                    &mut self.rule_priority,
                    -100..=100,
                    1,
                    true,
                );
            });
            ui.add_space(2.0);
            w::body_small(
                ui,
                "If two rules name the same program, the higher priority wins.",
                t::TEXT_LABEL,
            );
            ui.add_space(8.0);
            c::checkbox(ui, icons, &mut self.rule_enabled, "Rule enabled", true);

            c::separator(ui);
            w::title(ui, "Devices");
            ui.add_space(6.0);
            w::body_small(
                ui,
                "Tick a device to give it its own colours while this program runs. Unticked \
                 devices keep the profile's lighting.",
                t::TEXT_LABEL,
            );
            ui.add_space(8.0);
            if c::button(
                ui,
                "Take the colours the profile uses now",
                c::ButtonStyle::Outlined,
                !self.devices.is_empty(),
            )
            .clicked()
            {
                pending.use_current_colors = true;
            }
            ui.add_space(10.0);

            if self.devices.is_empty() {
                w::body_small(
                    ui,
                    "No devices are connected, so there is nothing for this rule to light yet. \
                     The rule still saves.",
                    t::TEXT_LABEL,
                );
            }

            for index in 0..self.devices.len() {
                w::card(ui, |ui| {
                    let name = self.devices[index].display_name.clone();
                    c::checkbox(ui, icons, &mut self.devices[index].included, &name, true);
                    ui.add_space(8.0);

                    let enabled = self.devices[index].included;
                    if self.devices[index].is_keyboard {
                        ui.horizontal(|ui| {
                            ui.allocate_ui(Vec2::new(120.0, 20.0), |ui| {
                                w::body(ui, "Keys to colour")
                            });
                            let groups: Vec<String> =
                                QUICK_GROUP_NAMES.iter().map(|g| (*g).to_owned()).collect();
                            c::combo(
                                ui,
                                icons,
                                ("app lighting key group", index),
                                &mut self.devices[index].key_group,
                                &groups,
                                |group| group.clone(),
                                enabled,
                            );
                        });
                        ui.add_space(6.0);
                    }

                    for which in [Swatch::Primary, Swatch::Secondary] {
                        ui.horizontal(|ui| {
                            let caption = match which {
                                Swatch::Primary => self.devices[index].primary_label(),
                                Swatch::Secondary => self.devices[index].secondary_label(),
                            };
                            ui.allocate_ui(Vec2::new(120.0, 20.0), |ui| w::body(ui, caption));
                            let row = &mut self.devices[index];
                            let (hex, buffer) = match which {
                                Swatch::Primary => (&mut row.primary_hex, &mut row.primary_buffer),
                                Swatch::Secondary => {
                                    (&mut row.secondary_hex, &mut row.secondary_buffer)
                                }
                            };
                            if c::text_field(ui, buffer, 110.0, enabled) {
                                match parse_hex(buffer) {
                                    Some(_) => *hex = normalise_hex(buffer),
                                    // Reverts rather than becoming black, as every other colour
                                    // field in the application does.
                                    None => *buffer = hex.clone(),
                                }
                                *buffer = hex.clone();
                            }
                            ui.add_space(8.0);
                            let colour = parse_hex(hex).unwrap_or(Color32::GRAY);
                            w::swatch(ui, colour, 22.0, false);
                        });
                        ui.add_space(6.0);
                    }

                    ui.horizontal(|ui| {
                        ui.allocate_ui(Vec2::new(120.0, 20.0), |ui| w::body(ui, "Brightness %"));
                        let row = &mut self.devices[index];
                        c::numeric(
                            ui,
                            icons,
                            &mut row.brightness_buffer,
                            &mut row.brightness,
                            0..=100,
                            5,
                            enabled,
                        );
                    });
                });
                ui.add_space(8.0);
            }

            ui.add_space(6.0);
            if c::button(ui, "Save rule", c::ButtonStyle::Primary, !shared.busy()).clicked() {
                pending.save = true;
            }
            ui.add_space(10.0);
            w::body_small(
                ui,
                "Colours are written to the device while the program is in front and the \
                 profile's lighting comes back when it is not. A device whose lighting mode is \
                 \"Untouched\" or \"Artemis\" in the profile is taken over by the rule (static) \
                 for as long as it matches.",
                t::TEXT_LABEL,
            );
            let _ = icons;
        });
    }
}

enum Field {
    Name,
    Executable,
}

enum Swatch {
    Primary,
    Secondary,
}

// =============================================================== applying

impl AppLightingPage {
    fn apply(&mut self, shared: &mut Shared, pending: Pending) {
        if pending.refresh_apps {
            self.apps = ghub_platform::apps::running_apps();
            self.picked_app = None;
        }
        if pending.use_current_colors {
            self.load_colors_from_profile(shared);
        }

        if pending.add {
            // `RuleExecutable` first, then the picked program, then a placeholder - so Add always
            // produces a rule the user can then edit rather than refusing.
            let executable = if self.rule_executable.trim().is_empty() {
                self.picked_app.clone().unwrap_or_else(|| "game.exe".to_owned())
            } else {
                self.rule_executable.trim().to_owned()
            };
            let name = if self.rule_name.trim().is_empty() {
                stem_of(&executable)
            } else {
                self.rule_name.trim().to_owned()
            };
            shared.edit_document("Add lighting rule", move |document| {
                document.app_lighting.push(AppLightingRule {
                    id: Id::new(),
                    name,
                    executable_name: executable,
                    enabled: true,
                    priority: 0,
                    devices: HashMap::new(),
                });
            });
        }

        if pending.duplicate {
            if let Some(id) = self.selected_rule {
                shared.edit_document("Duplicate lighting rule", move |document| {
                    let Some(source) =
                        document.app_lighting.iter().find(|rule| rule.id == id).cloned()
                    else {
                        return;
                    };
                    document.app_lighting.push(AppLightingRule {
                        id: Id::new(),
                        name: format!("{} (copy)", source.name),
                        ..source
                    });
                });
            }
        }

        if pending.delete {
            if let Some(rule) = self.selected() {
                self.confirm.ask(
                    "Delete lighting rule",
                    &format!(
                        "Delete '{}'? The profile's own lighting is not affected.",
                        rule.title()
                    ),
                    "Delete",
                );
            }
        }

        if pending.save {
            let Some(id) = self.selected_rule else { return };
            if self.rule_executable.trim().is_empty() {
                // Refused rather than saved empty: a rule with no executable matches nothing, and
                // would sit in the list looking configured.
                shared.error = Some(
                    "The rule needs the executable name of the program (for example cs2.exe)"
                        .to_owned(),
                );
                return;
            }
            let name = self.rule_name.trim().to_owned();
            let executable = self.rule_executable.trim().to_owned();
            let enabled = self.rule_enabled;
            let priority = self.rule_priority.clamp(-100, 100) as i32;
            let overrides: Vec<(String, AppLightingOverride)> = self
                .devices
                .iter()
                .filter(|row| row.included)
                .map(|row| (row.device_key.clone(), build_override(row)))
                .collect();

            shared.edit_document("Save lighting rule", move |document| {
                let Some(rule) = document.app_lighting.iter_mut().find(|rule| rule.id == id) else {
                    return;
                };
                rule.name = name;
                rule.executable_name = executable;
                rule.enabled = enabled;
                rule.priority = priority;
                // Replaced wholesale, so unticking a device removes its override rather than
                // leaving a stale one behind.
                rule.devices.clear();
                for (key, over) in overrides {
                    rule.devices.insert(key, over);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyboard_row(group: &str) -> DeviceRow {
        DeviceRow {
            device_key: "kb".to_owned(),
            display_name: "G815".to_owned(),
            is_keyboard: true,
            included: true,
            primary_hex: "#FF0000".to_owned(),
            primary_buffer: "#FF0000".to_owned(),
            secondary_hex: "#000080".to_owned(),
            secondary_buffer: "#000080".to_owned(),
            brightness: 80,
            brightness_buffer: "80".to_owned(),
            key_group: group.to_owned(),
        }
    }

    fn mouse_row() -> DeviceRow {
        DeviceRow {
            device_key: "mouse".to_owned(),
            display_name: "G903".to_owned(),
            is_keyboard: false,
            included: true,
            primary_hex: "#00FF00".to_owned(),
            primary_buffer: "#00FF00".to_owned(),
            secondary_hex: "#0000FF".to_owned(),
            secondary_buffer: "#0000FF".to_owned(),
            brightness: 100,
            brightness_buffer: "100".to_owned(),
            key_group: "ALL".to_owned(),
        }
    }

    #[test]
    fn a_keyboard_override_colours_exactly_the_chosen_group() {
        let over = build_override(&keyboard_row("WASD"));
        let keys: HashSet<&str> =
            over.static_colors.keys().map(|key| key.as_str()).collect();
        assert_eq!(
            keys,
            HashSet::from(["key:W", "key:A", "key:S", "key:D"]),
            "the group decides the key set"
        );
        for hex in over.static_colors.values() {
            assert_eq!(hex, "#FF0000");
        }
        // Everything outside the group falls back to the "every other key" colour.
        assert_eq!(over.default_color.as_deref(), Some("#000080"));
        assert_eq!(over.brightness, Some(0.8));
        assert_eq!(over.mode, Some(LightingMode::Static));
    }

    #[test]
    fn a_mouse_override_colours_its_two_zones() {
        let over = build_override(&mouse_row());
        assert_eq!(over.static_colors.get("zone:0").map(String::as_str), Some("#00FF00"));
        assert_eq!(over.static_colors.get("zone:1").map(String::as_str), Some("#0000FF"));
        assert!(!over.static_colors.keys().any(|key| key.starts_with("key:")));
    }

    #[test]
    fn every_quick_group_round_trips_through_the_stored_key_set() {
        // This is the whole reason `group_of` exists: the rule stores keys, not a group name, so
        // reopening a saved rule has to recover which group was chosen.
        for group in QUICK_GROUP_NAMES {
            let over = build_override(&keyboard_row(group));
            assert_eq!(group_of(Some(&over)), *group, "{group} did not round trip");
        }
    }

    #[test]
    fn a_key_set_that_is_no_quick_group_reads_as_all() {
        let mut over = build_override(&keyboard_row("WASD"));
        over.static_colors.insert("key:Escape".to_owned(), "#FF0000".to_owned());
        assert_eq!(group_of(Some(&over)), "ALL");
        assert_eq!(group_of(None), "ALL");
    }

    #[test]
    fn the_primary_swatch_is_stable_across_reloads() {
        // `HashMap` iteration order is not, so taking "the first value" has to mean something
        // deterministic or the swatch changes colour every time the page is opened.
        let over = build_override(&keyboard_row("NUMBERS"));
        let first = primary_of(Some(&over));
        for _ in 0..20 {
            assert_eq!(primary_of(Some(&over)), first);
        }
        assert_eq!(first, "#FF0000");
    }

    #[test]
    fn an_override_with_no_colours_still_offers_a_primary() {
        let over = AppLightingOverride {
            default_color: Some("#123456".to_owned()),
            ..AppLightingOverride::default()
        };
        assert_eq!(primary_of(Some(&over)), "#123456");
        assert_eq!(primary_of(None), "#00AAFF");
    }

    #[test]
    fn the_badges_name_the_program_the_device_count_and_whether_it_is_live() {
        let mut item = RuleItem {
            id: Id::new(),
            name: "Counter-Strike".to_owned(),
            executable_name: "cs2.exe".to_owned(),
            enabled: true,
            devices: 1,
            matching: false,
        };
        assert_eq!(item.title(), "Counter-Strike");
        assert_eq!(item.badges(), "cs2.exe \u{b7} 1 device");

        item.devices = 2;
        item.enabled = false;
        item.matching = true;
        assert_eq!(item.badges(), "cs2.exe \u{b7} 2 devices \u{b7} off \u{b7} RUNNING NOW");
    }

    #[test]
    fn a_rule_with_no_name_is_titled_by_its_executable() {
        let item = RuleItem {
            id: Id::new(),
            name: String::new(),
            executable_name: "game.exe".to_owned(),
            enabled: true,
            devices: 0,
            matching: false,
        };
        assert_eq!(item.title(), "game.exe");
        assert_eq!(item.badges(), "game.exe \u{b7} 0 devices");
    }

    #[test]
    fn a_row_labels_itself_for_the_kind_of_device_it_is() {
        assert_eq!(keyboard_row("ALL").primary_label(), "KEYS");
        assert_eq!(keyboard_row("ALL").secondary_label(), "EVERY OTHER KEY");
        assert_eq!(mouse_row().primary_label(), "PRIMARY ZONE");
        assert_eq!(mouse_row().secondary_label(), "LOGO ZONE");
    }

    #[test]
    fn a_rule_is_named_after_the_program_without_its_extension() {
        assert_eq!(stem_of("cs2.exe"), "cs2");
        assert_eq!(stem_of("my.game.exe"), "my.game");
        assert_eq!(stem_of("game"), "game");
        // A leading dot is the whole name, not an empty one.
        assert_eq!(stem_of(".hidden"), ".hidden");
    }

    #[test]
    fn a_bad_colour_becomes_white_rather_than_black() {
        assert_eq!(normalise_hex("zzz"), "#FFFFFF");
        assert_eq!(normalise_hex("00aaff"), "#00AAFF");
    }

    #[test]
    fn brightness_is_stored_as_a_fraction_and_clamped() {
        let mut row = mouse_row();
        row.brightness = 250;
        assert_eq!(build_override(&row).brightness, Some(1.0));
        row.brightness = -10;
        assert_eq!(build_override(&row).brightness, Some(0.0));
    }
}
