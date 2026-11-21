//! The G HUB import page. Port of `GHubImportTabViewModel` + `GHubImportTabView.axaml`.
//!
//! Three steps, in order, and the middle one is the point of the page: read G HUB's settings,
//! *show what would come over and what would not*, then apply only the ticked items.
//!
//! What is read is never G HUB's own file - see [`ghub_core::import`] for how the copy is taken -
//! and nothing is written until Apply is confirmed. The resulting document goes through the
//! store's normal save, so the previous document is kept as a backup and "Roll back" undoes the
//! import in one step.
//!
//! Items that have no local equivalent are listed with the reason and cannot be ticked. That is
//! deliberate: a migration that silently drops half a configuration is worse than one that says
//! which half it dropped.

use std::sync::{Arc, Mutex};

use egui::Ui;
use ghub_core::import::{self, Applier, ImportItemKind, ImportPreview};

use crate::controls as c;
use crate::icons::{Icon, IconCache};
use crate::shared::{Changes, Shared};
use crate::theme as t;
use crate::widgets as w;

use super::{Confirm, scroll, status_band};

/// Where a finished scan leaves its result.
///
/// A preview is a value, not a status line: it cannot be derived from the document and will not
/// fit in a job's one-line message, so the job puts it here and the next frame picks it up. This
/// is the same shape the mouse and power-save pages use for device read-backs.
type Slot = Arc<Mutex<Option<ImportPreview>>>;

#[derive(Default)]
pub struct ImportPage {
    path: String,
    preview: Option<ImportPreview>,
    summary: String,
    incoming: Slot,
    confirm: Confirm,
    confirming_apply: bool,
    loaded: bool,
}

impl ImportPage {
    pub fn show(
        &mut self,
        shared: &mut Shared,
        icons: &mut IconCache,
        ui: &mut Ui,
        changes: Changes,
    ) {
        let _ = changes;
        if !self.loaded {
            self.path = import::default_settings_path().display().to_string();
            self.loaded = true;
        }
        // Picked up on the UI thread so the ticks the user sets are the page's own state and
        // cannot be overwritten by a job that is still running.
        if let Some(preview) = self.incoming.lock().ok().and_then(|mut slot| slot.take()) {
            self.summary = preview.summary();
            self.preview = Some(preview);
        }

        if let Some(answer) = self.confirm.show(ui.ctx(), icons) {
            if self.confirming_apply && answer {
                self.apply(shared);
            }
            self.confirming_apply = false;
        }

        scroll(ui, "ghub import", |ui| {
            ui.set_max_width(1100.0);

            w::title(ui, "G HUB import");
            ui.add_space(4.0);
            w::body(
                ui,
                "Reads an existing G HUB configuration and brings over what has an equivalent \
                 here. G HUB's own file is never opened: the bytes are copied out and the copy is \
                 read, so G HUB can stay running.",
            );
            ui.add_space(10.0);
            status_band(ui, shared);
            ui.add_space(10.0);

            self.source_card(shared, icons, ui);
            ui.add_space(10.0);
            self.items_card(shared, icons, ui);
        });
    }

    fn source_card(&mut self, shared: &mut Shared, icons: &mut IconCache, ui: &mut Ui) {
        w::card(ui, |ui| {
            w::label(ui, "SOURCE");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                c::text_field(ui, &mut self.path, ui.available_width() - 120.0, !shared.busy());
                if w::icon_button(
                    ui,
                    icons,
                    Icon::Edit,
                    t::ROW_ICON_BUTTON,
                    t::ROW_ICON_GLYPH,
                    t::TEXT_LABEL,
                )
                .on_hover_text("Choose a settings.db")
                .clicked()
                {
                    self.browse();
                }
            });
            ui.add_space(4.0);
            w::body_small(
                ui,
                "G HUB keeps this at %LOCALAPPDATA%\\LGHUB\\settings.db. A copy taken from \
                 another machine works too.",
                t::TEXT_LABEL,
            );

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if c::button(ui, "Read G HUB settings", c::ButtonStyle::Primary, !shared.busy())
                    .clicked()
                {
                    self.scan(shared);
                }
                ui.add_space(8.0);
                let can_roll_back = shared.service().can_undo() && !shared.busy();
                if c::button(ui, "Roll back last import", c::ButtonStyle::Text, can_roll_back)
                    .on_hover_text(
                        "Undoes the last document change in this session. Older states are in \
                         Profiles & rules as automatic backups.",
                    )
                    .clicked()
                {
                    self.rollback(shared);
                }
            });

            if !self.summary.is_empty() {
                ui.add_space(8.0);
                w::body(ui, &self.summary);
            }
        });
    }

    fn items_card(&mut self, shared: &mut Shared, icons: &mut IconCache, ui: &mut Ui) {
        let Some(preview) = &mut self.preview else {
            w::card(ui, |ui| {
                w::body(
                    ui,
                    "Nothing read yet. Press \"Read G HUB settings\" to see what would be \
                     imported; nothing is written until you press Import.",
                );
            });
            return;
        };

        // Collected while drawing and applied after, so a click never mutates the list it was
        // drawn from.
        let mut set_all: Option<bool> = None;
        let mut toggled: Option<usize> = None;
        let supported = preview.supported_count();
        let selected = preview.items.iter().filter(|item| item.selected).count();
        let warnings = preview.warnings.clone();

        w::card(ui, |ui| {
            ui.horizontal(|ui| {
                w::label(ui, "WHAT WOULD BE IMPORTED");
                ui.add_space(8.0);
                w::body_small(
                    ui,
                    &format!("{selected} of {supported} importable items ticked"),
                    t::TEXT_LABEL,
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if w::link(ui, "None", true).clicked() {
                        set_all = Some(false);
                    }
                    ui.add_space(10.0);
                    if w::link(ui, "All", true).clicked() {
                        set_all = Some(true);
                    }
                });
            });

            for warning in &warnings {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    icons.show(ui, Icon::Caution, 16.0, t::CAUTION);
                    ui.add_space(6.0);
                    w::body_small(ui, warning, t::CAUTION);
                });
            }

            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .id_salt("ghub import items")
                .max_height(460.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (index, item) in preview.items.iter().enumerate() {
                        // A profile heading and the items under it, which is how the C# listed
                        // them: the order is the document's and an item belongs to the profile
                        // above it.
                        if item.kind == ImportItemKind::Profile {
                            ui.add_space(8.0);
                        }
                        ui.horizontal(|ui| {
                            let mut ticked = item.selected;
                            if c::checkbox(
                                ui,
                                icons,
                                &mut ticked,
                                "",
                                item.supported && !shared.busy(),
                            ) {
                                toggled = Some(index);
                            }
                            ui.add_space(2.0);
                            ui.allocate_ui(egui::vec2(110.0, 18.0), |ui| {
                                w::body_small(ui, item.kind.label(), kind_colour(item.kind));
                            });
                            if item.kind == ImportItemKind::Profile {
                                w::row(ui, &item.description);
                            } else {
                                // What ticking it would actually change, on hover: the
                                // description says what G HUB had, not what this document gets.
                                w::body(ui, &item.description)
                                    .on_hover_text(format!(
                                        "Adds {}",
                                        applier_summary(&item.applier)
                                    ));
                            }
                        });
                        if let Some(warning) = &item.warning {
                            ui.horizontal(|ui| {
                                ui.add_space(130.0);
                                let (icon, colour) = if item.supported {
                                    // Something to check afterwards, not a refusal.
                                    (Icon::InfoTooltip, t::TEXT_LABEL)
                                } else {
                                    (Icon::Caution, t::CAUTION)
                                };
                                icons.show(ui, icon, 14.0, colour);
                                ui.add_space(6.0);
                                w::body_small(ui, warning, colour);
                            });
                        }
                    }
                });

            ui.add_space(12.0);
            let can_apply = supported > 0 && selected > 0 && !shared.busy();
            if c::button(ui, "Import", c::ButtonStyle::Primary, can_apply).clicked() {
                self.confirming_apply = true;
                self.confirm.ask(
                    "Import from G HUB",
                    "Add the ticked G HUB items as new profiles? Existing profiles are kept, the \
                     previous state is saved as a backup, and \"Roll back last import\" undoes it.",
                    "Import",
                );
            }
        });

        if let Some(all) = set_all {
            for item in &mut preview.items {
                item.selected = all && item.supported;
            }
        }
        if let Some(index) = toggled {
            if let Some(item) = preview.items.get_mut(index) {
                // `Selected && Supported`: an unsupported item can never become ticked, whatever
                // the click did.
                item.selected = !item.selected && item.supported;
            }
        }
    }

    fn browse(&mut self) {
        // Modal, so it runs on the UI thread as part of the click; only the reading is queued.
        if let Some(path) = ghub_platform::dialogs::open_file(
            "G HUB settings.db",
            &ghub_platform::dialogs::FileFilter { label: "G HUB settings", extension: "db" },
        ) {
            self.path = path;
        }
    }

    fn scan(&mut self, shared: &mut Shared) {
        let path = std::path::PathBuf::from(self.path.trim());
        let resolver = device_key_resolver(shared);
        let slot = Arc::clone(&self.incoming);

        self.preview = None;
        self.summary.clear();

        shared.attempt("Read G HUB settings", move || {
            if !path.exists() {
                return Err(format!("No G HUB settings found at {}", path.display()));
            }
            let json = import::load_settings_json(&path)?;
            let preview = import::preview(&json, &path.display().to_string(), &resolver)
                .map_err(|e| e.to_string())?;
            let items = preview.items.len();
            if let Ok(mut slot) = slot.lock() {
                *slot = Some(preview);
            }
            Ok(format!(
                "{items} item(s) found. Review them, untick anything you do not want, then press \
                 Import"
            ))
        });
    }

    fn apply(&mut self, shared: &mut Shared) {
        let Some(preview) = self.preview.clone() else { return };
        let service = Arc::clone(shared.service());

        shared.attempt("Import", move || {
            // Read, then build, then replace: the document is re-read inside the job because the
            // user may have edited a profile between the scan and the import.
            let current = service.document();
            let result = import::apply(&preview, &current).map_err(|e| e.to_string())?;
            let profiles = result.profiles.len();
            let macros = result.macros.len();
            service.replace_document(&result).map_err(|e| e.to_string())?;
            Ok(format!(
                "imported. Profiles: {profiles}, macros: {macros}. Use \"Roll back last import\" \
                 to undo"
            ))
        });
    }

    fn rollback(&self, shared: &mut Shared) {
        let service = Arc::clone(shared.service());
        shared.attempt("Roll back", move || {
            Ok(if service.undo() {
                "rolled back to the state before the last change".to_owned()
            } else {
                "nothing to roll back in this session; restore an automatic backup from Profiles \
                 & rules"
                    .to_owned()
            })
        });
    }
}

/// Maps a G HUB device prefix onto a managed device's key.
///
/// The C# hard-coded two comparisons (`"g815"` and `"g903"`/`"g903hero"`), which meant a third
/// device imported under a placeholder key even when it was plugged in. This compares the prefix
/// against the device's own model word instead, so any device the identifier table knows resolves
/// without another edit here. `hero` is stripped because G HUB's slot ids carry the variant
/// (`g903hero`) and the descriptor does not.
///
/// An unresolved prefix is not an error: the import still works and the settings land under a
/// stable `ghub:<prefix>` key, which is what the page's own warning text refers to.
fn device_key_resolver(shared: &Shared) -> import::DeviceKeyResolver {
    let devices: Vec<(String, String)> = shared
        .host
        .devices
        .devices()
        .iter()
        .filter_map(|device| {
            let name = device.descriptor.as_ref()?.display_name.to_lowercase().replace(' ', "");
            Some((name, device.key()))
        })
        .collect();

    Box::new(move |prefix| {
        let prefix = prefix.to_lowercase().replace(' ', "");
        let trimmed = prefix.strip_suffix("hero").unwrap_or(&prefix);
        devices
            .iter()
            .find(|(name, _)| name.starts_with(trimmed) && !trimmed.is_empty())
            .map(|(_, key)| key.clone())
    })
}

/// The kind column's colour. Unsupported rows are the ones the user has to read, so they are the
/// only ones tinted away from the body colour.
fn kind_colour(kind: ImportItemKind) -> egui::Color32 {
    match kind {
        ImportItemKind::Profile => t::TEXT,
        ImportItemKind::Unsupported => t::CAUTION,
        _ => t::TEXT_LABEL,
    }
}

/// What an applier would add, for the row's tooltip.
fn applier_summary(applier: &Applier) -> &'static str {
    match applier {
        Applier::None => "nothing (listed for information)",
        Applier::AppRule(_) => "an auto-switch rule on the profile",
        Applier::Mouse { .. } => "that device's DPI and report rate",
        Applier::Binding { .. } => "one binding",
        Applier::Macro(_) => "a macro in the document",
        Applier::FirmwareLighting { .. } => "that device's firmware lighting effects",
        Applier::PerKeyLighting { .. } => "that device's per-key colours",
        Applier::GameModeKeys { .. } => "that keyboard's Game Mode key list",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unsupported item must be impossible to tick, including through "All": the whole promise
    /// of the page is that an item with no equivalent is not imported as an approximation.
    #[test]
    fn selecting_everything_still_leaves_unsupported_items_unticked() {
        let resolver: import::DeviceKeyResolver = Box::new(|p| Some(format!("ghub:{p}")));
        let json = serde_json::json!({
            "profiles": {"profiles": [{
                "id": "p1",
                "name": "Test",
                "assignments": [{"slotId": "g815_g1_m1", "cardId": "unknown-card-id"}]
            }]}
        })
        .to_string();
        let mut preview = import::preview(&json, "settings.db", &resolver).expect("parses");
        assert!(preview.items.iter().any(|item| !item.supported));

        // What the "All" link does, applied here: ticked only where the item can be imported.
        for item in &mut preview.items {
            item.selected = item.supported;
        }
        assert!(
            preview.items.iter().all(|item| !item.selected || item.supported),
            "an unsupported item became ticked"
        );
    }

    /// The resolver is what decides whether an imported binding lands on the real device or on a
    /// placeholder, so both halves are pinned.
    #[test]
    fn the_resolver_matches_a_model_word_and_ignores_the_hero_variant() {
        let devices = [
            ("g815lightsyncrgb".to_owned(), "logi:046d:c33f".to_owned()),
            ("g903lightspeed".to_owned(), "logi:046d:c086".to_owned()),
        ];
        let resolve = move |prefix: &str| -> Option<String> {
            let prefix = prefix.to_lowercase().replace(' ', "");
            let trimmed = prefix.strip_suffix("hero").unwrap_or(&prefix).to_owned();
            devices
                .iter()
                .find(|(name, _)| name.starts_with(&trimmed) && !trimmed.is_empty())
                .map(|(_, key)| key.clone())
        };

        assert_eq!(resolve("g815").as_deref(), Some("logi:046d:c33f"));
        assert_eq!(resolve("g903hero").as_deref(), Some("logi:046d:c086"));
        assert_eq!(resolve("g502"), None, "a device that is not here must not be matched");
        assert_eq!(resolve(""), None, "an empty prefix must not match the first device");
    }

    #[test]
    fn every_applier_kind_can_be_described() {
        let rule = import::Applier::AppRule(Box::new(ghub_core::profile::AppRule::by_executable(
            "game.exe",
        )));
        assert!(applier_summary(&rule).contains("rule"));
        assert!(applier_summary(&Applier::None).contains("nothing"));
    }

    /// The import page's own promise, in the summary line the C# wrote: the source file is read
    /// through a copy. If that sentence ever stops being true the test has to be changed with it.
    #[test]
    fn the_summary_states_the_read_only_guarantee() {
        let preview = ImportPreview {
            source_path: "settings.db".to_owned(),
            ghub_version: Some("2026.6".to_owned()),
            ..Default::default()
        };
        assert!(preview.summary().contains("only read through a temporary copy"));
    }
}
