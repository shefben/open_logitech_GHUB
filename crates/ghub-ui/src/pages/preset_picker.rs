//! G HUB's ready-made assignments, for users who would rather pick than build.
//!
//! Three sources, the tabs of G HUB's Assignments panel: SYSTEM (its built-in cards), COMMANDS
//! for the Desktop (its 44 Windows shortcuts) and COMMANDS for a game (816 games' key lists). All
//! three come from G HUB's own data, copied into this repository, so G HUB need not be installed.
//!
//! Picking an entry hands back a plain [`ActionDefinition`]; nothing about where it came from is
//! stored, so a profile never depends on these lists.

use std::path::PathBuf;
use std::sync::OnceLock;

use ghub_core::ActionDefinition;
use ghub_core::presets::{self, CommandPreset, GameLibrary};

use crate::controls as c;
use crate::icons::IconCache;
use crate::theme as t;
use crate::widgets as w;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Source {
    #[default]
    System,
    Desktop,
    Game,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Self::System => "SYSTEM",
            Self::Desktop => "DESKTOP",
            Self::Game => "GAMES",
        }
    }
}

#[derive(Default)]
pub struct PresetPicker {
    pub open: bool,
    source: Source,
    search: String,
    game_search: String,
    game: Option<String>,
    show_unavailable: bool,
}

/// The game list, read the first time it is wanted. 1.2 MB, so not before.
fn games() -> &'static Result<GameLibrary, String> {
    static GAMES: OnceLock<Result<GameLibrary, String>> = OnceLock::new();
    GAMES.get_or_init(|| GameLibrary::load(&games_path()))
}

fn games_path() -> PathBuf {
    let beside_exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("assets/presets/games.json")));
    if let Some(path) = beside_exe.filter(|p| p.is_file()) {
        return path;
    }
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/presets/games.json"))
}

fn matches(preset: &CommandPreset, needle: &str) -> bool {
    needle.is_empty()
        || preset.name.to_lowercase().contains(needle)
        || preset.category.to_lowercase().contains(needle)
        || preset.keys.as_deref().is_some_and(|k| k.to_lowercase().contains(needle))
}

impl PresetPicker {
    /// Draws the picker. Returns the action the user chose, if they chose one this frame.
    pub fn show(&mut self, ui: &mut egui::Ui, icons: &mut IconCache, enabled: bool) -> Option<ActionDefinition> {
        let caption = if self.open { "Hide G HUB presets" } else { "Choose from G HUB presets…" };
        if c::button(ui, caption, c::ButtonStyle::Text, enabled).clicked() {
            self.open = !self.open;
        }
        if !self.open {
            return None;
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            for source in [Source::System, Source::Desktop, Source::Game] {
                let style = if source == self.source {
                    c::ButtonStyle::Primary
                } else {
                    c::ButtonStyle::Outlined
                };
                if c::button(ui, source.label(), style, enabled).clicked() {
                    self.source = source;
                }
                ui.add_space(6.0);
            }
        });
        ui.add_space(6.0);

        let owned;
        let list: &[CommandPreset] = match self.source {
            Source::System => presets::system_presets(),
            Source::Desktop => presets::desktop_commands(),
            Source::Game => match self.game_list(ui, enabled) {
                Some(list) => {
                    owned = list;
                    &owned
                }
                None => return None,
            },
        };

        w::label(ui, "SEARCH");
        c::text_field(ui, &mut self.search, ui.available_width(), enabled);
        if self.source == Source::System {
            c::checkbox(ui, icons, &mut self.show_unavailable, "Show cards for other devices", enabled);
        }
        ui.add_space(6.0);

        let needle = self.search.trim().to_lowercase();
        let mut chosen = None;
        egui::ScrollArea::vertical()
            .id_salt(("preset list", self.source as u8))
            .max_height(260.0)
            .show(ui, |ui| {
                let mut category = "";
                for preset in list.iter().filter(|p| matches(p, &needle)) {
                    if !preset.available() && !self.show_unavailable {
                        continue;
                    }
                    if preset.category != category {
                        category = &preset.category;
                        ui.add_space(6.0);
                        w::label(ui, &category.to_uppercase());
                    }
                    let title = match &preset.keys {
                        Some(keys) => format!("{}   {}", preset.name, keys),
                        None => preset.name.clone(),
                    };
                    match &preset.action {
                        Ok(action) => {
                            if w::anim_row(ui, &title, false).on_hover_text("Assign this").clicked() {
                                chosen = Some(action.clone());
                            }
                        }
                        Err(reason) => {
                            w::body_small(ui, &title, t::TEXT_LABEL);
                            w::body_small(ui, reason, t::TEXT_LABEL);
                        }
                    }
                }
            });
        if chosen.is_some() {
            self.open = false;
        }
        chosen
    }

    /// The game chooser. Returns the chosen game's commands once one is picked.
    fn game_list(&mut self, ui: &mut egui::Ui, enabled: bool) -> Option<Vec<CommandPreset>> {
        let library = match games() {
            Ok(library) => library,
            Err(e) => {
                w::body_small(ui, &format!("The game list could not be read: {e}"), t::ERROR);
                return None;
            }
        };
        if let Some(game) = self.game.as_deref().and_then(|id| library.by_id(id)) {
            ui.horizontal(|ui| {
                w::label(ui, &game.name.to_uppercase());
                if c::button(ui, "Change game", c::ButtonStyle::Text, enabled).clicked() {
                    self.game = None;
                }
            });
            if self.game.is_some() {
                return Some(game.presets());
            }
        }

        w::label(ui, "GAME");
        c::text_field(ui, &mut self.game_search, ui.available_width(), enabled);
        ui.add_space(6.0);
        egui::ScrollArea::vertical().id_salt("preset games").max_height(260.0).show(ui, |ui| {
            for game in library.search(&self.game_search).into_iter().take(200) {
                let label = format!("{}  ·  {} commands", game.name, game.commands.len());
                if w::anim_row(ui, &label, false).clicked() {
                    self.game = Some(game.id.clone());
                    self.search.clear();
                }
            }
        });
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_game_list_is_found_from_the_source_tree() {
        let library = games().as_ref().expect("games.json");
        assert!(library.games.len() > 800);
    }

    #[test]
    fn search_matches_name_category_and_keys() {
        let copy = presets::system_presets()
            .iter()
            .find(|p| p.id == "SYSTEM_WINDOWS_COPY")
            .unwrap();
        assert!(matches(copy, "copy"));
        assert!(matches(copy, "editing"));
        assert!(matches(copy, "lctrl"));
        assert!(!matches(copy, "volume"));
    }
}
