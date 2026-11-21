//! The ten peripheral pages. Each is the port target of the matching
//! `Artemis.UI/Screens/Peripherals/Tabs/*TabViewModel.cs` + `*TabView.axaml` pair.
//!
//! The C# gave each tab its own view-model instance, created by the router and kept alive while
//! the tab was in the back stack. [`Pages`] is the same thing without the router: one struct per
//! page, all of them alive, each reloading only when [`Changes`] says something it cares about
//! moved. A page's state is its own - which device is selected, which sub-tab is open, what is
//! half-typed into a field - and keeping it across a tab switch is what makes switching tabs and
//! coming back behave the way it did under `RoutableScreen`.

use egui::Ui;

use crate::icons::{Icon, IconCache};
use crate::shared::{Changes, Shared};
use crate::{Page, widgets as w};

mod app_lighting;
mod automation;
mod devices;
mod diagnostics;
mod import;
mod keyboard;
mod macros;
mod mouse;
mod options;
mod profiles;

pub mod action_editor;
pub mod device_stage;
pub mod power_save;
pub mod preset_picker;

/// Every page's state, held for the life of the window.
#[derive(Default)]
pub struct Pages {
    pub devices: devices::DevicesPage,
    pub mouse: mouse::MousePage,
    pub keyboard: keyboard::KeyboardPage,
    pub app_lighting: app_lighting::AppLightingPage,
    pub macros: macros::MacrosPage,
    pub profiles: profiles::ProfilesPage,
    pub automation: automation::AutomationPage,
    pub import: import::ImportPage,
    pub diagnostics: diagnostics::DiagnosticsPage,
    pub options: options::OptionsPage,
}

impl Pages {
    pub fn show(
        &mut self,
        shared: &mut Shared,
        icons: &mut IconCache,
        ui: &mut Ui,
        page: Page,
        changes: Changes,
    ) {
        match page {
            Page::Devices => self.devices.show(shared, icons, ui, changes),
            Page::Mouse => self.mouse.show(shared, icons, ui, changes),
            Page::Keyboard => self.keyboard.show(shared, icons, ui, changes),
            Page::AppLighting => self.app_lighting.show(shared, icons, ui, changes),
            Page::Macros => self.macros.show(shared, icons, ui, changes),
            Page::Profiles => self.profiles.show(shared, icons, ui, changes),
            Page::Automation => self.automation.show(shared, icons, ui, changes),
            Page::Import => self.import.show(shared, icons, ui, changes),
            Page::Diagnostics => self.diagnostics.show(shared, icons, ui, changes),
            Page::Options => self.options.show(shared, icons, ui, changes),
        }
    }
}

/// The status band every page carries: the coexistence warning, the error, then the status.
///
/// `PeripheralTabBase` exposed `ConflictWarning`, `Error` and `Status` and each view bound all
/// three in that order. Drawn in one place so a page cannot forget one - an action whose failure
/// is never shown is indistinguishable from an action that did nothing.
pub fn status_band(ui: &mut Ui, shared: &Shared) {
    if shared.has_conflict() {
        // `Foreground="#F0A030"` in the keyboard view; the token behind it is `--warning-main-500`.
        w::body_small(ui, shared.conflict_warning(), crate::theme::CAUTION);
    }
    if let Some(error) = &shared.error {
        w::body_small(ui, error, crate::theme::ERROR);
    }
    if let Some(status) = &shared.status {
        w::body_small(ui, status, crate::theme::TEXT_LABEL);
    }
}

/// The profile pill in a device header: the active profile's name, and a click opens the list of
/// profiles to switch to. Returns the profile picked this frame.
///
/// Picking is G HUB's dropdown: the choice becomes the persistent profile (app rules still win
/// while their app is in front). Apply it with [`switch_profile`].
pub fn profile_pill(
    ui: &mut Ui,
    icons: &mut IconCache,
    shared: &Shared,
) -> Option<ghub_core::Id> {
    use crate::theme as t;
    use egui::{Color32, CornerRadius, Sense, Stroke};

    let active = shared.active_profile();
    let frame = egui::Frame::new()
        .fill(t::PANEL)
        .corner_radius(CornerRadius::same(t::RADIUS_PILL))
        .inner_margin(t::PILL_PAD)
        .show(ui, |ui| {
            ui.set_min_width(t::PILL_MIN_WIDTH - t::PILL_PAD.sum().x);
            ui.horizontal(|ui| {
                icons.show(ui, Icon::RailCharacterMenu, 18.0, t::TEXT_ROW);
                ui.add_space(10.0);
                w::body(ui, &active.name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    icons.show(ui, Icon::Chevron, 18.0, t::TEXT_ROW);
                });
            });
        });
    let rect = frame.response.rect;
    let popup = ui.scope_id().with("profile pill");
    // Registered after the pill's contents, so it sits on top of them and gets the click.
    let response = ui
        .interact(rect, popup, Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("Switch profile");
    if response.clicked() {
        let open = ui.memory(|m| m.data.get_temp::<bool>(popup).unwrap_or(false));
        ui.memory_mut(|m| m.data.insert_temp(popup, !open));
    }
    if !ui.memory(|m| m.data.get_temp::<bool>(popup).unwrap_or(false)) {
        return None;
    }

    let document = shared.document();
    let mut profiles: Vec<_> = document.profiles.iter().collect();
    profiles.sort_by_key(|profile| profile.order);
    let shown = egui::Area::new(popup.with("list"))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(rect.left(), rect.bottom() + 2.0))
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(t::PANEL)
                .stroke(Stroke::new(1.0, t::PANEL_BORDER))
                .corner_radius(CornerRadius::same(t::RADIUS_PILL))
                .inner_margin(egui::Margin::same(4))
                .show(ui, |ui| {
                    ui.set_min_width(rect.width() - 8.0);
                    let mut picked = None;
                    for profile in &profiles {
                        if w::row_inner(
                            ui,
                            &profile.name,
                            t::ANIM_ROW_PAD,
                            Color32::TRANSPARENT,
                            profile.id == active.id,
                        )
                        .clicked()
                        {
                            picked = Some(profile.id);
                        }
                    }
                    picked
                })
                .inner
        });

    if let Some(id) = shown.inner {
        ui.memory_mut(|m| m.data.insert_temp(popup, false));
        return (id != active.id).then_some(id);
    }
    if !response.clicked() && shown.response.clicked_elsewhere() {
        ui.memory_mut(|m| m.data.insert_temp(popup, false));
    }
    None
}

/// Makes `id` the persistent profile, the way the Profiles page's "Use (persistent)" does. A
/// manual lock would outrank it and look like the pick did nothing, so the lock is cleared too.
pub fn switch_profile(shared: &mut Shared, id: ghub_core::Id) {
    shared.edit_document("Switch profile", move |document| {
        document.persistent_profile_id = Some(id);
        document.locked_profile_id = None;
    });
}

/// A modal yes/no question. Stands in for `IWindowService.ShowConfirmContentDialog`.
///
/// Only two things in the application ask one, and both are things that should be hard to do by
/// accident: remapping the primary mouse button, and writing a device's flash. Neither is allowed
/// to proceed on a single click, so the dialog is modal and the destructive answer is the one that
/// has to be chosen explicitly.
#[derive(Default)]
pub struct Confirm {
    question: Option<(String, String, String)>,
}

impl Confirm {
    /// Poses the question. `accept` is the wording on the button that goes ahead with it - never
    /// "OK", always what will happen.
    pub fn ask(&mut self, title: &str, body: &str, accept: &str) {
        self.question = Some((title.to_owned(), body.to_owned(), accept.to_owned()));
    }

    pub fn is_open(&self) -> bool {
        self.question.is_some()
    }

    /// Draws it. `Some(true)` on the frame the user accepted, `Some(false)` when they declined or
    /// dismissed it, `None` while it is still open or not shown at all.
    pub fn show(&mut self, ctx: &egui::Context, icons: &mut IconCache) -> Option<bool> {
        let (title, body, accept) = self.question.clone()?;

        let shown = egui::Modal::new(egui::Id::NULL.with("ghub confirm"))
            .frame(
                egui::Frame::new()
                    .fill(crate::theme::PANEL)
                    .stroke(egui::Stroke::new(1.0, crate::theme::PANEL_BORDER))
                    .corner_radius(egui::CornerRadius::same(crate::theme::RADIUS_PANEL))
                    .inner_margin(crate::theme::PANEL_PAD),
            )
            .show(ctx, |ui| {
                ui.set_max_width(440.0);
                ui.horizontal(|ui| {
                    icons.show(ui, Icon::Caution, 20.0, crate::theme::CAUTION);
                    ui.add_space(8.0);
                    w::title(ui, &title);
                });
                ui.add_space(10.0);
                w::body(ui, &body);
                ui.add_space(16.0);

                let mut answer = None;
                ui.horizontal(|ui| {
                    if crate::controls::button(
                        ui,
                        "Cancel",
                        crate::controls::ButtonStyle::Text,
                        true,
                    )
                    .clicked()
                    {
                        answer = Some(false);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if crate::controls::button(
                            ui,
                            &accept,
                            crate::controls::ButtonStyle::Primary,
                            true,
                        )
                        .clicked()
                        {
                            answer = Some(true);
                        }
                    });
                });
                answer
            });

        // Clicking away is a decline, not an accept: the dialog exists because the action should
        // not happen by accident, and dismissing it is not choosing it.
        let answer = shown.inner.or_else(|| shown.backdrop_response.clicked().then_some(false));
        if answer.is_some() {
            self.question = None;
        }
        answer
    }
}

/// A scrolling page body. Every view was wrapped in
/// `<ScrollViewer HorizontalScrollBarVisibility="Disabled">`, so vertical only.
pub fn scroll<R>(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, false])
        .show(ui, add)
        .inner
}
