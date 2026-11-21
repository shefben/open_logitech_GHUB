//! Profiles and the rules that pick them. Port of `ProfilesTabViewModel.cs` and
//! `ProfilesTabView.axaml`.
//!
//! A profile is a complete set of device settings; an application rule says which program should
//! get it. The page's job is to make the *resolution order* legible, because "why is this profile
//! active" is otherwise unanswerable: it states the order in full, shows what won right now, and
//! offers a simulator so a rule can be tested without launching the game.
//!
//! **One section of the C# is deliberately absent.** `ProfilesTabViewModel` could link a
//! peripheral profile to an Artemis lighting profile (`ArtemisProfileConfigurationId`). There is
//! no Artemis host here to link to, and the standalone profile model has no such field, so the
//! control is gone rather than present and inert.

use egui::{Ui, Vec2};
use ghub_core::profile::{AppMatchKind, AppRule, PeripheralProfile};
use ghub_core::resolver::{self, ForegroundApp};
use ghub_core::{Id, store};
use ghub_platform::dialogs::{self, FileFilter};

use crate::controls as c;
use crate::icons::IconCache;
use crate::shared::{Changes, Shared};
use crate::theme as t;
use crate::widgets as w;

use super::{Confirm, scroll, status_band};

/// `OrderHelp` - the resolution order, stated in full.
///
/// This is the single most useful sentence on the page: every "wrong profile" report comes down to
/// a more specific rule winning, and nothing else in the UI can explain that.
pub const ORDER_HELP: &str =
    "Resolution order: manual lock > exact executable path > executable + window title > \
     executable name > window title > manually selected profile > default. Ties: higher rule \
     priority, then list order.";

pub static RULE_KINDS: &[AppMatchKind] = &[
    AppMatchKind::ExactPath,
    AppMatchKind::ExecutableName,
    AppMatchKind::ExecutableAndTitle,
    AppMatchKind::TitleOnly,
];

/// `ProfileItem`.
struct ProfileRow {
    id: Id,
    name: String,
    is_default: bool,
    is_active: bool,
    is_locked: bool,
    rules: usize,
}

impl ProfileRow {
    /// `Badges`.
    fn badges(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.is_default {
            parts.push("default".to_owned());
        }
        if self.is_active {
            parts.push("ACTIVE".to_owned());
        }
        if self.is_locked {
            parts.push("locked".to_owned());
        }
        if self.rules > 0 {
            parts.push(format!("{} rule(s)", self.rules));
        }
        parts.join(" ")
    }
}

/// `RuleItem` - one rule, already rendered as the line the list shows.
struct RuleRow {
    id: Id,
    text: String,
}

/// `$"{r.Kind}: {...}"`.
fn describe_rule(rule: &AppRule) -> String {
    let target = rule
        .executable_path
        .clone()
        .or_else(|| rule.executable_name.clone())
        .unwrap_or_else(|| "(nothing)".to_owned());
    let title = rule
        .title_pattern
        .as_ref()
        .map_or(String::new(), |pattern| format!(" title ~ /{pattern}/"));
    format!(
        "{:?}: {target}{title} (priority {}){}",
        rule.kind,
        rule.priority,
        if rule.enabled { "" } else { " [disabled]" }
    )
}

/// `Unique(d, name)` - appends a number until the name is free.
fn unique_name(existing: &[PeripheralProfile], wanted: &str) -> String {
    let mut candidate = wanted.to_owned();
    let mut n = 2;
    while existing.iter().any(|profile| profile.name == candidate) {
        candidate = format!("{wanted} {n}");
        n += 1;
    }
    candidate
}

/// Which question the modal is asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confirming {
    Delete,
    RestoreBackup,
}

#[derive(Default)]
struct Pending {
    add: bool,
    duplicate: bool,
    rename: bool,
    delete: bool,
    make_default: bool,
    move_up: bool,
    move_down: bool,
    lock: bool,
    unlock: bool,
    select_persistent: bool,
    save_auto_switch: bool,
    add_rule: bool,
    remove_rule: bool,
    refresh_apps: bool,
    use_picked_app: bool,
    simulate: bool,
    export: bool,
    import: bool,
    backup_now: bool,
    restore_backup: bool,
    undo: bool,
    redo: bool,
}

pub struct ProfilesPage {
    profiles: Vec<ProfileRow>,
    selected_profile: Option<Id>,
    profile_name: String,

    rules: Vec<RuleRow>,
    selected_rule: Option<Id>,
    rule_kind: AppMatchKind,
    rule_exe: String,
    rule_path: String,
    rule_title: String,
    rule_priority: i64,
    priority_buffer: String,

    apps: Vec<ghub_platform::apps::RunningApp>,
    picked_app: Option<String>,

    sim_path: String,
    sim_title: String,
    sim_result: String,

    auto_switch: bool,
    backups: Vec<std::path::PathBuf>,
    selected_backup: Option<std::path::PathBuf>,

    /// `ActiveExplain` - what resolution picked, and why.
    active_explain: String,

    confirm: Confirm,
    confirming: Option<Confirming>,
    loaded: bool,
}

impl Default for ProfilesPage {
    fn default() -> Self {
        Self {
            profiles: Vec::new(),
            selected_profile: None,
            profile_name: String::new(),
            rules: Vec::new(),
            selected_rule: None,
            rule_kind: AppMatchKind::default(),
            rule_exe: String::new(),
            rule_path: String::new(),
            rule_title: String::new(),
            rule_priority: 0,
            priority_buffer: "0".to_owned(),
            apps: Vec::new(),
            picked_app: None,
            sim_path: String::new(),
            sim_title: String::new(),
            sim_result: String::new(),
            auto_switch: true,
            backups: Vec::new(),
            selected_backup: None,
            active_explain: "-".to_owned(),
            confirm: Confirm::default(),
            confirming: None,
            loaded: false,
        }
    }
}

impl ProfilesPage {
    fn selected(&self) -> Option<&ProfileRow> {
        let id = self.selected_profile?;
        self.profiles.iter().find(|profile| profile.id == id)
    }

    /// `Refresh()`.
    fn reload(&mut self, shared: &Shared) {
        let keep = self.selected_profile;
        let document = shared.document();
        let active = shared.active_profile().id;

        let mut ordered: Vec<&PeripheralProfile> = document.profiles.iter().collect();
        ordered.sort_by_key(|profile| profile.order);
        self.profiles = ordered
            .iter()
            .map(|profile| ProfileRow {
                id: profile.id,
                name: profile.name.clone(),
                is_default: profile.is_default,
                is_active: profile.id == active,
                is_locked: document.locked_profile_id == Some(profile.id),
                rules: profile.rules.len(),
            })
            .collect();

        self.selected_profile = keep
            .filter(|id| self.profiles.iter().any(|profile| profile.id == *id))
            .or_else(|| {
                self.profiles.iter().find(|profile| profile.is_active).map(|profile| profile.id)
            });
        self.auto_switch = document.auto_switch_enabled;
        self.backups = shared.service().store().list_backups();
        self.selected_backup = self
            .selected_backup
            .take()
            .filter(|path| self.backups.contains(path))
            .or_else(|| self.backups.first().cloned());
        self.active_explain = shared
            .service()
            .active()
            .map(|resolution| resolution.explain())
            .unwrap_or_else(|| "-".to_owned());

        if self.apps.is_empty() {
            self.apps = ghub_platform::apps::running_apps();
        }
        self.load_rules(shared);
        self.loaded = true;
    }

    /// `LoadRules()`.
    fn load_rules(&mut self, shared: &Shared) {
        self.rules.clear();
        let document = shared.document();
        let profile = self
            .selected_profile
            .and_then(|id| document.profiles.iter().find(|profile| profile.id == id));
        self.profile_name = profile.map(|p| p.name.clone()).unwrap_or_default();
        for rule in profile.map(|p| p.rules.as_slice()).unwrap_or_default() {
            self.rules.push(RuleRow { id: rule.id, text: describe_rule(rule) });
        }
        if self.selected_rule.is_some_and(|id| !self.rules.iter().any(|rule| rule.id == id)) {
            self.selected_rule = None;
        }
    }
}

// =============================================================== drawing

impl ProfilesPage {
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

        if let Some(answer) = self.confirm.show(ui.ctx(), icons) {
            let asking = self.confirming.take();
            if answer {
                match asking {
                    Some(Confirming::Delete) => self.delete_selected(shared),
                    Some(Confirming::RestoreBackup) => self.restore_backup(shared),
                    None => {}
                }
            }
        }

        let mut pending = Pending::default();

        scroll(ui, "profiles", |ui| {
            ui.set_max_width(1200.0);
            w::title(ui, "Profiles and application rules");
            ui.add_space(8.0);
            w::body(ui, &format!("Active: {}", self.active_explain));
            ui.add_space(6.0);
            w::body_small(ui, ORDER_HELP, t::TEXT_LABEL);
            ui.add_space(6.0);
            status_band(ui, shared);
            ui.add_space(10.0);

            ui.horizontal_top(|ui| {
                // `ColumnDefinitions="320,16,*"`.
                ui.allocate_ui_with_layout(
                    Vec2::new(320.0, ui.available_height()),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| self.profile_column(ui, icons, shared, &mut pending),
                );
                ui.add_space(16.0);
                ui.allocate_ui_with_layout(
                    ui.available_size(),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| self.rule_column(ui, icons, shared, &mut pending),
                );
            });
        });

        self.apply(shared, pending);
    }

    fn profile_column(
        &mut self,
        ui: &mut Ui,
        icons: &mut IconCache,
        shared: &Shared,
        pending: &mut Pending,
    ) {
        let mut select = None;
        egui::ScrollArea::vertical()
            .id_salt("profile list")
            .min_scrolled_height(200.0)
            .max_height(260.0)
            .show(ui, |ui| {
                for profile in &self.profiles {
                    let active = self.selected_profile == Some(profile.id);
                    let response = w::anim_row(ui, &profile.name, active);
                    w::body_small(
                        ui,
                        &profile.badges(),
                        if profile.is_active { t::ACCENT_BRIGHT } else { t::TEXT_LABEL },
                    );
                    ui.add_space(4.0);
                    if response.clicked() {
                        select = Some(profile.id);
                    }
                }
            });
        if let Some(id) = select {
            self.selected_profile = Some(id);
            self.selected_rule = None;
            self.load_rules(shared);
        }

        ui.add_space(6.0);
        let has_selection = self.selected().is_some();
        let busy = shared.busy();
        ui.horizontal_wrapped(|ui| {
            if c::button(ui, "Add", c::ButtonStyle::Primary, !busy).clicked() {
                pending.add = true;
            }
            ui.add_space(4.0);
            if c::button(ui, "Duplicate", c::ButtonStyle::Outlined, has_selection && !busy)
                .clicked()
            {
                pending.duplicate = true;
            }
            ui.add_space(4.0);
            if c::button(
                ui,
                "Delete",
                c::ButtonStyle::Text,
                has_selection && !busy && !self.confirm.is_open(),
            )
            .clicked()
            {
                pending.delete = true;
            }
            ui.add_space(4.0);
            if c::button(ui, "\u{2191}", c::ButtonStyle::Outlined, has_selection && !busy)
                .on_hover_text("Move up")
                .clicked()
            {
                pending.move_up = true;
            }
            ui.add_space(4.0);
            if c::button(ui, "\u{2193}", c::ButtonStyle::Outlined, has_selection && !busy)
                .on_hover_text("Move down")
                .clicked()
            {
                pending.move_down = true;
            }
            ui.add_space(4.0);
            if c::button(ui, "Make default", c::ButtonStyle::Outlined, has_selection && !busy)
                .clicked()
            {
                pending.make_default = true;
            }
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            c::text_field(ui, &mut self.profile_name, 200.0, has_selection);
            ui.add_space(4.0);
            if c::button(
                ui,
                "Rename",
                c::ButtonStyle::Outlined,
                has_selection && !self.profile_name.trim().is_empty() && !busy,
            )
            .clicked()
            {
                pending.rename = true;
            }
        });

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            if c::button(ui, "Use (persistent)", c::ButtonStyle::Outlined, has_selection && !busy)
                .on_hover_text(
                    "Becomes the fallback instead of the default; app rules still win",
                )
                .clicked()
            {
                pending.select_persistent = true;
            }
            ui.add_space(4.0);
            if c::button(ui, "Lock", c::ButtonStyle::Outlined, has_selection && !busy)
                .on_hover_text("Manual lock: wins over every rule until unlocked")
                .clicked()
            {
                pending.lock = true;
            }
            ui.add_space(4.0);
            if c::button(ui, "Unlock", c::ButtonStyle::Text, !busy).clicked() {
                pending.unlock = true;
            }
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            c::checkbox(ui, icons, &mut self.auto_switch, "Auto-switch on foreground app", true);
            ui.add_space(6.0);
            if c::button(ui, "Save", c::ButtonStyle::Outlined, !busy).clicked() {
                pending.save_auto_switch = true;
            }
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            // Enabled from the service's own history, so a greyed button means there is genuinely
            // nothing to undo rather than that the page has not noticed yet.
            if c::button(
                ui,
                "Undo",
                c::ButtonStyle::Outlined,
                shared.service().can_undo() && !busy,
            )
            .clicked()
            {
                pending.undo = true;
            }
            ui.add_space(4.0);
            if c::button(
                ui,
                "Redo",
                c::ButtonStyle::Outlined,
                shared.service().can_redo() && !busy,
            )
            .clicked()
            {
                pending.redo = true;
            }
        });
    }

    fn rule_column(
        &mut self,
        ui: &mut Ui,
        icons: &mut IconCache,
        shared: &Shared,
        pending: &mut Pending,
    ) {
        let busy = shared.busy();
        w::title(ui, "Rules of the selected profile");
        ui.add_space(6.0);
        w::card(ui, |ui| {
            let mut select = None;
            egui::ScrollArea::vertical()
                .id_salt("rule list")
                .min_scrolled_height(80.0)
                .max_height(180.0)
                .show(ui, |ui| {
                    if self.rules.is_empty() {
                        w::body_small(
                            ui,
                            "No rules. This profile is only used when it is the default, locked, \
                             or selected.",
                            t::TEXT_LABEL,
                        );
                    }
                    for rule in &self.rules {
                        let active = self.selected_rule == Some(rule.id);
                        if w::anim_row(ui, &rule.text, active).clicked() {
                            select = Some(rule.id);
                        }
                        ui.add_space(2.0);
                    }
                });
            if let Some(id) = select {
                self.selected_rule = Some(id);
            }

            ui.add_space(6.0);
            if c::button(
                ui,
                "Remove selected rule",
                c::ButtonStyle::Text,
                self.selected_rule.is_some() && !busy,
            )
            .clicked()
            {
                pending.remove_rule = true;
            }

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if self.apps.is_empty() {
                    w::body_small(ui, "No windowed programs found.", t::TEXT_LABEL);
                } else {
                    let names: Vec<String> =
                        self.apps.iter().map(|app| app.name.clone()).collect();
                    let paths = self.apps.clone();
                    let mut picked = self.picked_app.clone().unwrap_or_else(|| names[0].clone());
                    ui.allocate_ui(Vec2::new(360.0, 28.0), |ui| {
                        c::combo(
                            ui,
                            icons,
                            "profile rule app",
                            &mut picked,
                            &names,
                            |name| {
                                paths
                                    .iter()
                                    .find(|app| &app.name == name)
                                    .map_or_else(|| name.clone(), |app| {
                                        format!("{} \u{2014} {}", app.name, app.path)
                                    })
                            },
                            true,
                        );
                    });
                    self.picked_app = Some(picked);
                }
                ui.add_space(6.0);
                if c::button(ui, "Use", c::ButtonStyle::Outlined, self.picked_app.is_some())
                    .clicked()
                {
                    pending.use_picked_app = true;
                }
                ui.add_space(6.0);
                if c::button(ui, "Refresh list", c::ButtonStyle::Text, true).clicked() {
                    pending.refresh_apps = true;
                }
            });

            ui.add_space(10.0);
            // `ColumnDefinitions="130,*"`.
            ui.horizontal(|ui| {
                ui.allocate_ui(Vec2::new(130.0, 20.0), |ui| w::body(ui, "Match"));
                c::combo(
                    ui,
                    icons,
                    "rule kind",
                    &mut self.rule_kind,
                    RULE_KINDS,
                    |kind| format!("{kind:?}"),
                    true,
                );
            });
            for (caption, field) in [
                ("Executable name", RuleField::Exe),
                ("Full path", RuleField::Path),
                ("Window title regex", RuleField::Title),
            ] {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.allocate_ui(Vec2::new(130.0, 20.0), |ui| w::body(ui, caption));
                    let buffer = match field {
                        RuleField::Exe => &mut self.rule_exe,
                        RuleField::Path => &mut self.rule_path,
                        RuleField::Title => &mut self.rule_title,
                    };
                    c::text_field(ui, buffer, ui.available_width().min(420.0), true);
                });
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.allocate_ui(Vec2::new(130.0, 20.0), |ui| w::body(ui, "Priority"));
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

            ui.add_space(10.0);
            if c::button(
                ui,
                "Add rule",
                c::ButtonStyle::Primary,
                self.selected().is_some() && !busy,
            )
            .clicked()
            {
                pending.add_rule = true;
            }
        });

        ui.add_space(12.0);
        w::title(ui, "Rule simulation");
        ui.add_space(6.0);
        w::card(ui, |ui| {
            w::label(ui, "EXECUTABLE PATH TO TEST");
            c::text_field(ui, &mut self.sim_path, ui.available_width().min(520.0), true);
            ui.add_space(6.0);
            w::label(ui, "WINDOW TITLE TO TEST");
            c::text_field(ui, &mut self.sim_title, ui.available_width().min(520.0), true);
            ui.add_space(8.0);
            if c::button(ui, "Which profile would be active?", c::ButtonStyle::Outlined, true)
                .clicked()
            {
                pending.simulate = true;
            }
            if !self.sim_result.is_empty() {
                ui.add_space(8.0);
                w::body(ui, &self.sim_result);
            }
        });

        ui.add_space(12.0);
        w::title(ui, "Export, import and backups");
        ui.add_space(6.0);
        w::card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if c::button(ui, "Export all\u{2026}", c::ButtonStyle::Outlined, !busy).clicked() {
                    pending.export = true;
                }
                ui.add_space(6.0);
                if c::button(ui, "Import (merge)\u{2026}", c::ButtonStyle::Outlined, !busy)
                    .clicked()
                {
                    pending.import = true;
                }
                ui.add_space(6.0);
                if c::button(ui, "Back up now\u{2026}", c::ButtonStyle::Outlined, !busy).clicked() {
                    pending.backup_now = true;
                }
            });

            ui.add_space(10.0);
            w::body_small(
                ui,
                "Automatic backups (one per save, newest first)",
                t::TEXT_LABEL,
            );
            ui.add_space(4.0);
            if self.backups.is_empty() {
                w::body_small(ui, "No backups yet.", t::TEXT_LABEL);
            } else {
                let backups = self.backups.clone();
                let mut selected =
                    self.selected_backup.clone().unwrap_or_else(|| backups[0].clone());
                c::combo(
                    ui,
                    icons,
                    "backup",
                    &mut selected,
                    &backups,
                    |path| {
                        path.file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string())
                    },
                    true,
                );
                self.selected_backup = Some(selected);
                ui.add_space(8.0);
                if c::button(
                    ui,
                    "Restore selected backup\u{2026}",
                    c::ButtonStyle::Text,
                    !busy && !self.confirm.is_open(),
                )
                .clicked()
                {
                    pending.restore_backup = true;
                }
            }
        });
    }
}

enum RuleField {
    Exe,
    Path,
    Title,
}

// =============================================================== applying

impl ProfilesPage {
    fn delete_selected(&mut self, shared: &mut Shared) {
        let Some(id) = self.selected_profile else { return };
        self.selected_profile = None;
        shared.edit_document("Delete", move |document| {
            document.profiles.retain(|profile| profile.id != id);
            // A lock or a persistent selection pointing at a deleted profile would make
            // resolution fall through to the default without saying why.
            if document.locked_profile_id == Some(id) {
                document.locked_profile_id = None;
            }
            if document.persistent_profile_id == Some(id) {
                document.persistent_profile_id = None;
            }
        });
    }

    fn restore_backup(&mut self, shared: &mut Shared) {
        let Some(path) = self.selected_backup.clone() else { return };
        let service = std::sync::Arc::clone(shared.service());
        shared.attempt("Restore", move || {
            let json = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let document = store::parse(&json).map_err(|e| e.to_string())?;
            service.replace_document(&document).map_err(|e| e.to_string())?;
            Ok("Backup restored".to_owned())
        });
    }

    fn apply(&mut self, shared: &mut Shared, pending: Pending) {
        if pending.refresh_apps {
            self.apps = ghub_platform::apps::running_apps();
            self.picked_app = None;
        }
        if pending.use_picked_app {
            if let Some(app) =
                self.picked_app.as_ref().and_then(|name| self.apps.iter().find(|a| &a.name == name))
            {
                self.rule_exe = app.name.clone();
                self.rule_path = app.path.clone();
                // The simulator is seeded too, so "use this program, then check what it resolves
                // to" is two clicks rather than retyping the path.
                self.sim_path = app.path.clone();
            }
        }
        if pending.simulate {
            let document = shared.document();
            let app = ForegroundApp::new(self.sim_path.trim(), self.sim_title.clone());
            self.sim_result = resolver::resolve(&document, Some(&app))
                .map(|resolution| resolution.explain())
                .unwrap_or_else(|| {
                    "No profile would be active (the document has no profiles).".to_owned()
                });
        }

        if pending.add {
            shared.edit_document("Add profile", |document| {
                let name = unique_name(&document.profiles, "New profile");
                let order = document.profiles.len() as i32;
                let mut profile = PeripheralProfile::new(name);
                profile.order = order;
                document.profiles.push(profile);
            });
        }

        if pending.duplicate {
            if let Some(id) = self.selected_profile {
                shared.edit_document("Duplicate", move |document| {
                    let Some(source) =
                        document.profiles.iter().find(|profile| profile.id == id).cloned()
                    else {
                        return;
                    };
                    let name = unique_name(&document.profiles, &format!("{} (copy)", source.name));
                    let order = document.profiles.len() as i32;
                    let mut copy = PeripheralProfile { id: Id::new(), name, order, ..source };
                    copy.is_default = false;
                    // Fresh rule ids: two rules sharing an id would make "remove this rule"
                    // ambiguous.
                    for rule in &mut copy.rules {
                        rule.id = Id::new();
                    }
                    document.profiles.push(copy);
                });
            }
        }

        if pending.rename {
            if let (Some(id), name) = (self.selected_profile, self.profile_name.trim().to_owned()) {
                if !name.is_empty() {
                    shared.edit_document("Rename", move |document| {
                        if let Some(profile) =
                            document.profiles.iter_mut().find(|profile| profile.id == id)
                        {
                            profile.name = name;
                        }
                    });
                }
            }
        }

        if pending.delete {
            // Copied out, because asking the question mutates `self` and the row borrows it.
            if let Some((name, is_default)) =
                self.selected().map(|profile| (profile.name.clone(), profile.is_default))
            {
                if is_default {
                    // Refused rather than asked: with no default there is nothing to fall back to.
                    shared.error = Some(
                        "The default profile cannot be deleted (make another profile default \
                         first)"
                            .to_owned(),
                    );
                } else {
                    self.confirming = Some(Confirming::Delete);
                    self.confirm.ask(
                        "Delete profile",
                        &format!(
                            "Delete '{name}'? Device flash is not affected. Undo is available."
                        ),
                        "Delete",
                    );
                }
            }
        }

        if pending.make_default {
            if let Some(id) = self.selected_profile {
                shared.edit_document("Set default", move |document| {
                    for profile in &mut document.profiles {
                        profile.is_default = profile.id == id;
                    }
                });
            }
        }

        if pending.move_up || pending.move_down {
            let delta: isize = if pending.move_up { -1 } else { 1 };
            if let Some(id) = self.selected_profile {
                shared.edit_document("Reorder", move |document| {
                    let mut ordered: Vec<Id> = {
                        let mut profiles: Vec<&PeripheralProfile> =
                            document.profiles.iter().collect();
                        profiles.sort_by_key(|profile| profile.order);
                        profiles.iter().map(|profile| profile.id).collect()
                    };
                    let Some(index) = ordered.iter().position(|candidate| *candidate == id) else {
                        return;
                    };
                    let target = index as isize + delta;
                    if target < 0 || target as usize >= ordered.len() {
                        return;
                    }
                    ordered.swap(index, target as usize);
                    // Rewritten densely from 0, so a gap left by an earlier delete cannot make
                    // two profiles share a position.
                    for (position, candidate) in ordered.iter().enumerate() {
                        if let Some(profile) =
                            document.profiles.iter_mut().find(|profile| profile.id == *candidate)
                        {
                            profile.order = position as i32;
                        }
                    }
                });
            }
        }

        if pending.lock {
            if let Some(id) = self.selected_profile {
                shared.edit_document("Lock", move |document| {
                    document.locked_profile_id = Some(id);
                });
            }
        }
        if pending.unlock {
            shared.edit_document("Unlock", |document| {
                document.locked_profile_id = None;
            });
        }
        if pending.select_persistent {
            if let Some(id) = self.selected_profile {
                shared.edit_document("Select", move |document| {
                    document.persistent_profile_id = Some(id);
                    // A manual lock would outrank the selection that was just made, which would
                    // look like the button did nothing.
                    document.locked_profile_id = None;
                });
            }
        }
        if pending.save_auto_switch {
            let enabled = self.auto_switch;
            shared.edit_document("Auto-switch", move |document| {
                document.auto_switch_enabled = enabled;
            });
        }

        if pending.add_rule {
            if let Some(id) = self.selected_profile {
                let rule = AppRule {
                    id: Id::new(),
                    kind: self.rule_kind,
                    executable_path: non_empty(&self.rule_path),
                    executable_name: non_empty(&self.rule_exe),
                    title_pattern: non_empty(&self.rule_title),
                    enabled: true,
                    priority: self.rule_priority.clamp(-100, 100) as i32,
                };
                shared.edit_document("Add rule", move |document| {
                    if let Some(profile) =
                        document.profiles.iter_mut().find(|profile| profile.id == id)
                    {
                        profile.rules.push(rule);
                    }
                });
            }
        }

        if pending.remove_rule {
            if let Some(id) = self.selected_rule {
                self.selected_rule = None;
                shared.edit_document("Remove rule", move |document| {
                    for profile in &mut document.profiles {
                        profile.rules.retain(|rule| rule.id != id);
                    }
                });
            }
        }

        // The dialogs are modal and have to run on this thread, so the path is chosen here and
        // only the file work is queued.
        if pending.export || pending.backup_now {
            let (title, suggested) = if pending.export {
                ("Export peripheral profiles", "ghub-profiles.json".to_owned())
            } else {
                (
                    "Back up peripheral profiles",
                    format!(
                        "ghub-backup-{}.json",
                        ghub_core::time::format_stamp(ghub_core::time::now_unix(), 0)
                    ),
                )
            };
            if let Some(path) = dialogs::save_file(title, &FileFilter::JSON, &suggested) {
                let json = shared.service().export_json();
                let what = if pending.export { "Exported" } else { "Backup written" };
                shared.attempt("Export", move || {
                    std::fs::write(&path, json).map_err(|e| e.to_string())?;
                    Ok(format!("{what} to {path}"))
                });
            }
        }

        if pending.import {
            if let Some(path) = dialogs::open_file("Import peripheral profiles", &FileFilter::JSON)
            {
                let service = std::sync::Arc::clone(shared.service());
                shared.attempt("Import", move || {
                    let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
                    // Checked before reading, not after: the point of the limit is not to load a
                    // gigabyte into memory in the first place.
                    if size as usize > store::MAX_IMPORT_BYTES {
                        return Err(format!(
                            "file is {size} bytes, over the {} byte limit",
                            store::MAX_IMPORT_BYTES
                        ));
                    }
                    let json = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
                    let count =
                        service.import_json(&json, false).map_err(|e| e.to_string())?;
                    Ok(format!(
                        "Imported {count} profile(s) (validated; untrusted content is never \
                         executed). Undo is available."
                    ))
                });
            }
        }

        if pending.restore_backup {
            if let Some(path) = self.selected_backup.clone() {
                self.confirming = Some(Confirming::RestoreBackup);
                self.confirm.ask(
                    "Restore backup",
                    &format!(
                        "Replace all peripheral profiles with the backup '{}'?",
                        path.file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string())
                    ),
                    "Restore",
                );
            }
        }

        if pending.undo {
            let service = std::sync::Arc::clone(shared.service());
            shared.attempt("Undo", move || {
                Ok(if service.undo() { "Undone" } else { "Nothing to undo" }.to_owned())
            });
        }
        if pending.redo {
            let service = std::sync::Arc::clone(shared.service());
            shared.attempt("Redo", move || {
                Ok(if service.redo() { "Redone" } else { "Nothing to redo" }.to_owned())
            });
        }
    }
}

/// A trimmed field, or `None` when it is blank - which is what distinguishes "no path given" from
/// "match the empty path".
fn non_empty(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() { None } else { Some(trimmed.to_owned()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_badges_say_what_is_true_of_a_profile() {
        let mut row = ProfileRow {
            id: Id::new(),
            name: "Default".to_owned(),
            is_default: true,
            is_active: true,
            is_locked: false,
            rules: 0,
        };
        assert_eq!(row.badges(), "default ACTIVE");

        row.is_locked = true;
        row.rules = 3;
        assert_eq!(row.badges(), "default ACTIVE locked 3 rule(s)");

        row.is_default = false;
        row.is_active = false;
        row.is_locked = false;
        row.rules = 0;
        assert_eq!(row.badges(), "", "nothing notable means no badges");
    }

    #[test]
    fn a_duplicate_name_gets_the_next_free_number() {
        let existing: Vec<PeripheralProfile> = ["New profile", "New profile 2", "Other"]
            .iter()
            .map(|name| PeripheralProfile::new(*name))
            .collect();
        assert_eq!(unique_name(&existing, "New profile"), "New profile 3");
        assert_eq!(unique_name(&existing, "Other"), "Other 2");
        assert_eq!(unique_name(&existing, "Fresh"), "Fresh");
        assert_eq!(unique_name(&[], "First"), "First");
    }

    #[test]
    fn a_rule_describes_what_it_matches_on() {
        let mut rule = AppRule::by_executable("game.exe");
        rule.priority = 5;
        let text = describe_rule(&rule);
        assert!(text.contains("game.exe"), "{text}");
        assert!(text.contains("priority 5"), "{text}");
        assert!(!text.contains("[disabled]"), "{text}");

        rule.enabled = false;
        rule.title_pattern = Some("^Level".to_owned());
        let text = describe_rule(&rule);
        assert!(text.contains("title ~ /^Level/"), "{text}");
        assert!(text.ends_with("[disabled]"), "{text}");
    }

    #[test]
    fn the_full_path_wins_over_the_name_in_the_description() {
        // The path is the more specific of the two, so it is the one worth showing.
        let rule = AppRule {
            id: Id::new(),
            kind: AppMatchKind::ExactPath,
            executable_path: Some(r"C:\Games\game.exe".to_owned()),
            executable_name: Some("game.exe".to_owned()),
            title_pattern: None,
            enabled: true,
            priority: 0,
        };
        assert!(describe_rule(&rule).contains(r"C:\Games\game.exe"));
    }

    #[test]
    fn a_rule_with_nothing_to_match_on_says_so_rather_than_looking_empty() {
        let rule = AppRule {
            id: Id::new(),
            kind: AppMatchKind::TitleOnly,
            executable_path: None,
            executable_name: None,
            title_pattern: None,
            enabled: true,
            priority: 0,
        };
        assert!(describe_rule(&rule).contains("(nothing)"));
    }

    #[test]
    fn a_blank_field_is_stored_as_absent_not_as_an_empty_match() {
        assert_eq!(non_empty("  "), None);
        assert_eq!(non_empty(""), None);
        assert_eq!(non_empty("  game.exe "), Some("game.exe".to_owned()));
    }

    #[test]
    fn every_match_kind_is_offered_in_order_of_specificity() {
        assert_eq!(RULE_KINDS.len(), 4);
        let mut previous = None;
        for kind in RULE_KINDS {
            let specificity = kind.specificity();
            if let Some(before) = previous {
                assert!(specificity > before, "{kind:?} is out of order");
            }
            previous = Some(specificity);
        }
        assert!(RULE_KINDS.contains(&AppMatchKind::default()));
    }

    /// The order the page prints has to be the order the resolver actually applies, or the page is
    /// documenting something that is not true.
    #[test]
    fn the_order_help_lists_the_steps_the_resolver_uses() {
        for step in [
            "manual lock",
            "exact executable path",
            "executable + window title",
            "executable name",
            "window title",
            "manually selected profile",
            "default",
        ] {
            assert!(ORDER_HELP.contains(step), "{step} is missing from the order help");
        }
        // And that the more specific kinds come first in the sentence.
        let path = ORDER_HELP.find("exact executable path").unwrap();
        let name = ORDER_HELP.find("executable name").unwrap();
        // `rfind`, because "window title" also appears inside "executable + window title",
        // which is *more* specific than a name match and sits earlier in the sentence.
        let title = ORDER_HELP.rfind("window title >").unwrap();
        assert!(path < name && name < title);
    }
}
