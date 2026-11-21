//! The Options page. Port of `OtherTabsViewModels.cs (PeripheralOptionsTabViewModel)`.
//!
//! Application-wide settings, as opposed to the per-profile settings every other page edits.
//! There are three groups and they are not equally consequential, which is why two of them ask
//! before they take effect:
//!
//! * **Save to Device** permits onboard flash writes. Every individual write still needs its own
//!   explicit "Save to device" with a preview, a backup and a read-back, so this switch only
//!   makes that button reachable - and the confirmation says plainly that these writes are not
//!   hardware-verified on every firmware.
//! * **Remote OpenRGB** points the SDK client off this machine. The protocol has no
//!   authentication and no encryption, so a non-loopback host is something the user has to say
//!   yes to in words, with the consequence spelled out.
//! * **Low battery warnings** are a notification threshold and need no confirmation.
//!
//! Both confirmations are modelled the way the C# did it: the switch is turned back off when the
//! dialog is declined, so declining leaves the saved document exactly as it was.

use std::sync::Arc;

use egui::Ui;
use ghub_core::openrgb::OpenRgbClient;

use crate::controls as c;
use crate::icons::{Icon, IconCache};
use crate::shared::{Changes, Shared};
use crate::theme as t;
use crate::widgets as w;

use super::{Confirm, scroll, status_band};

/// How often the page re-reads the OpenRGB state while it is open; the connect loop moves on its
/// own thread and egui does not repaint for it.
const REFRESH: std::time::Duration = std::time::Duration::from_millis(500);

/// Which confirmation is open, so the answer is applied to the right switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Asking {
    OnboardWrites,
    RemoteOpenRgb,
}

#[derive(Default)]
pub struct OptionsPage {
    open_rgb_enabled: bool,
    open_rgb_host: String,
    open_rgb_port: i64,
    port_buffer: String,
    open_rgb_allow_remote: bool,
    allow_onboard_writes: bool,
    low_battery: bool,
    low_battery_threshold: i64,
    threshold_buffer: String,
    /// Starting with Windows. Not part of the profile document: it is a registry value, and a
    /// document copied to another machine must not drag a startup entry with it.
    autostart: bool,
    saved_autostart: bool,
    autostart_command: Option<String>,
    /// What the document said when the page was last loaded, so a confirmation is only asked for
    /// a switch that is actually being turned *on*.
    saved_onboard_writes: bool,
    saved_allow_remote: bool,
    open_rgb_state: String,
    confirm: Confirm,
    asking: Option<Asking>,
    loaded: bool,
}

impl OptionsPage {
    pub fn show(
        &mut self,
        shared: &mut Shared,
        icons: &mut IconCache,
        ui: &mut Ui,
        changes: Changes,
    ) {
        if !self.loaded || changes.document {
            self.reload(shared);
        }
        if shared.host.open_rgb.take_changed() {
            self.open_rgb_state = shared.host.open_rgb.describe();
        }
        ui.ctx().request_repaint_after(REFRESH);

        if let Some(answer) = self.confirm.show(ui.ctx(), icons) {
            match (self.asking.take(), answer) {
                // Declining turns the switch back off rather than abandoning the save, which is
                // what the C# did: the rest of the page is still saved, just without this one.
                (Some(Asking::OnboardWrites), false) => self.allow_onboard_writes = false,
                (Some(Asking::RemoteOpenRgb), false) => self.open_rgb_allow_remote = false,
                // Accepting records the consent, so the question is not asked again on the way
                // back through `begin_save`.
                (Some(Asking::OnboardWrites), true) => self.saved_onboard_writes = true,
                (Some(Asking::RemoteOpenRgb), true) => self.saved_allow_remote = true,
                (None, _) => {}
            }
            // Back through `begin_save`, not straight to `save`: both switches can be turned on
            // in one visit, and the second question has to be asked after the first is answered.
            self.begin_save(shared);
        }

        scroll(ui, "options", |ui| {
            ui.set_max_width(900.0);

            w::title(ui, "Options");
            ui.add_space(4.0);
            w::body(
                ui,
                "Settings that apply to the whole application rather than to one profile.",
            );
            ui.add_space(10.0);
            status_band(ui, shared);
            ui.add_space(10.0);

            self.startup_card(shared, icons, ui);
            ui.add_space(10.0);
            self.device_card(shared, icons, ui);
            ui.add_space(10.0);
            self.open_rgb_card(shared, icons, ui);
            ui.add_space(10.0);
            self.battery_card(shared, icons, ui);

            ui.add_space(14.0);
            let enabled = !shared.busy() && !self.confirm.is_open();
            if c::button(ui, "Save options", c::ButtonStyle::Primary, enabled).clicked() {
                self.begin_save(shared);
            }
        });
    }

    fn startup_card(&mut self, shared: &mut Shared, icons: &mut IconCache, ui: &mut Ui) {
        w::card(ui, |ui| {
            w::label(ui, "STARTUP");
            ui.add_space(8.0);
            c::checkbox(
                ui,
                icons,
                &mut self.autostart,
                "Start with Windows",
                !shared.busy(),
            );
            ui.add_space(4.0);
            w::body_small(
                ui,
                "A per-user entry under the current account's Run key. Nothing is elevated and no                  scheduled task is created, so profile rules and lighting start with this                  account's session only.",
                t::TEXT_LABEL,
            );
            if let Some(command) = &self.autostart_command {
                ui.add_space(4.0);
                w::body_small(ui, &format!("Registered as {command}"), t::TEXT_HEAD);
            }
            ui.add_space(6.0);
            w::body_small(
                ui,
                "Closing this window closes it. The background agent keeps running, which is what                  keeps remapped keys, macros, profile rules and lighting working with the window                  out of the way - and what lets the window give its memory back instead of                  sitting hidden. Right-click the G icon in the notification area to switch                  profile, or to quit everything.",
                t::TEXT_LABEL,
            );
            ui.add_space(8.0);
            // The same thing as Quit in the tray menu, for a user who would rather not hunt for
            // an icon that Windows may have hidden in the overflow. The agent is asked first and
            // closes this window itself, so there is one shutdown order rather than two.
            if w::link(ui, "Quit G HUB completely", false).clicked()
                && !ghub_platform::handoff::request_quit()
            {
                // No agent to ask - this window is all there is, so closing it is quitting.
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }

    fn device_card(&mut self, shared: &mut Shared, icons: &mut IconCache, ui: &mut Ui) {
        w::card(ui, |ui| {
            w::label(ui, "DEVICE MEMORY");
            ui.add_space(8.0);
            c::checkbox(
                ui,
                icons,
                &mut self.allow_onboard_writes,
                "Allow Save to Device (onboard profile memory)",
                !shared.busy(),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                icons.show(ui, Icon::Caution, 16.0, t::CAUTION);
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    w::body_small(
                        ui,
                        "Off by default. Each write still needs an explicit \"Save to device\" \
                         with a preview, a backup of the sectors it replaces, and a read-back \
                         check afterwards.",
                        t::TEXT_LABEL,
                    );
                    w::body_small(
                        ui,
                        "These writes are not hardware-verified by the developers on every \
                         firmware. Reading onboard memory never needs this switch.",
                        t::CAUTION,
                    );
                });
            });
            ui.add_space(6.0);
            // True in this build in a way it was not in the C#: the provider reads the flag when
            // it opens a session, so a device already connected keeps the old policy.
            w::body_small(
                ui,
                "Changing this affects devices connected from now on. Press \"Scan for devices\" \
                 on the Peripherals page to apply it to what is already connected.",
                t::TEXT_LABEL,
            );
            ui.add_space(4.0);
            w::body_small(
                ui,
                "Firmware updating and flashing are not implemented at all, with or without this \
                 switch. The firmware version is read and displayed only.",
                t::TEXT_LABEL,
            );
        });
    }

    fn open_rgb_card(&mut self, shared: &mut Shared, icons: &mut IconCache, ui: &mut Ui) {
        w::card(ui, |ui| {
            w::label(ui, "OPENRGB");
            ui.add_space(8.0);
            c::checkbox(
                ui,
                icons,
                &mut self.open_rgb_enabled,
                "Connect to an OpenRGB server",
                !shared.busy(),
            );
            ui.add_space(8.0);

            let fields = self.open_rgb_enabled && !shared.busy();
            c::field_row(ui, "Host", |ui| {
                c::text_field(ui, &mut self.open_rgb_host, 220.0, fields)
            });
            c::field_row(ui, "Port", |ui| {
                c::numeric(
                    ui,
                    icons,
                    &mut self.port_buffer,
                    &mut self.open_rgb_port,
                    1..=65535,
                    1,
                    fields,
                )
            });

            ui.add_space(6.0);
            c::checkbox(
                ui,
                icons,
                &mut self.open_rgb_allow_remote,
                "Allow a host that is not this machine",
                fields,
            );
            ui.add_space(4.0);
            // The host is checked as typed, so the warning appears before the save rather than
            // only in the confirmation dialog.
            if !self.open_rgb_host.trim().is_empty()
                && !OpenRgbClient::is_loopback(self.open_rgb_host.trim())
            {
                ui.horizontal(|ui| {
                    icons.show(ui, Icon::Warning, 16.0, t::WARNING);
                    ui.add_space(6.0);
                    w::body_small(
                        ui,
                        "This host is not on this machine. The OpenRGB protocol has no \
                         authentication and no encryption: lighting data, device names and LED \
                         counts travel in the clear, and anything that can reach the port can \
                         write to every LED on the machine running it.",
                        t::WARNING,
                    );
                });
                ui.add_space(4.0);
            }

            ui.horizontal(|ui| {
                ui.allocate_ui(egui::vec2(160.0, 18.0), |ui| w::body(ui, "State"));
                w::body(ui, &self.open_rgb_state);
            });
            ui.add_space(6.0);
            w::body_small(
                ui,
                "OpenRGB writes at a lower priority than this application's own lighting: a \
                 device a profile is driving natively ignores OpenRGB frames rather than \
                 flickering between the two. Diagnostics lists who owns what.",
                t::TEXT_LABEL,
            );
        });
    }

    fn battery_card(&mut self, shared: &mut Shared, icons: &mut IconCache, ui: &mut Ui) {
        w::card(ui, |ui| {
            w::label(ui, "BATTERY");
            ui.add_space(8.0);
            c::checkbox(
                ui,
                icons,
                &mut self.low_battery,
                "Warn when a device's battery is low",
                !shared.busy(),
            );
            ui.add_space(8.0);
            c::field_row(ui, "Warn below", |ui| {
                c::numeric(
                    ui,
                    icons,
                    &mut self.threshold_buffer,
                    &mut self.low_battery_threshold,
                    1..=50,
                    1,
                    self.low_battery && !shared.busy(),
                )
            });
            ui.add_space(4.0);
            w::body_small(
                ui,
                "Some devices report a voltage rather than a percentage. Where the percentage is \
                 estimated from voltage, it is labelled as estimated wherever it is shown.",
                t::TEXT_LABEL,
            );
        });
    }

    /// Asks whatever needs asking, then saves. Nothing is written until every question is
    /// answered, so a declined dialog cannot leave half the page saved.
    fn begin_save(&mut self, shared: &mut Shared) {
        if self.allow_onboard_writes && !self.saved_onboard_writes {
            self.asking = Some(Asking::OnboardWrites);
            self.confirm.ask(
                "Allow Save to Device",
                "Enable writing onboard profile memory? Each write still needs an explicit \"Save \
                 to device\" with preview, backup and read-back verification. These writes are \
                 not hardware-verified by the developers on every firmware.",
                "Enable",
            );
            return;
        }
        if self.open_rgb_allow_remote
            && !self.saved_allow_remote
            && !OpenRgbClient::is_loopback(self.open_rgb_host.trim())
        {
            self.asking = Some(Asking::RemoteOpenRgb);
            self.confirm.ask(
                "Remote OpenRGB",
                &format!(
                    "Connect to OpenRGB on {}? Lighting data will be sent over the network \
                     unencrypted, and the protocol has no authentication.",
                    self.open_rgb_host.trim()
                ),
                "Allow",
            );
            return;
        }
        self.save(shared);
    }

    fn save(&mut self, shared: &mut Shared) {
        let host = if self.open_rgb_host.trim().is_empty() {
            "127.0.0.1".to_owned()
        } else {
            self.open_rgb_host.trim().to_owned()
        };
        let port = self.open_rgb_port.clamp(1, 65535) as u16;
        let enabled = self.open_rgb_enabled;
        let allow_remote = self.open_rgb_allow_remote;
        let onboard = self.allow_onboard_writes;
        let low_battery = self.low_battery;
        let threshold = self.low_battery_threshold.clamp(1, 50) as u8;

        // The page's own copy is normalised too, so the field shows what was saved rather than
        // the blank the user left behind.
        self.open_rgb_host = host.clone();
        self.open_rgb_port = i64::from(port);
        self.port_buffer = port.to_string();
        self.low_battery_threshold = i64::from(threshold);
        self.threshold_buffer = threshold.to_string();

        let open_rgb = Arc::clone(&shared.host.open_rgb);
        let saved_host = host.clone();

        shared.edit_document("Options", move |document| {
            document.app.open_rgb_enabled = enabled;
            document.app.open_rgb_host = saved_host.clone();
            document.app.open_rgb_port = port;
            document.app.open_rgb_allow_remote = allow_remote;
            document.app.allow_onboard_writes = onboard;
            document.app.low_battery_warnings = low_battery;
            document.app.low_battery_threshold_percent = threshold;

            // The host's own reconciler applies the rest of these on the document-changed
            // event. The one thing it cannot do is move an *already connected* OpenRGB to a new
            // host or port, because from its point of view nothing changed: it was connected
            // before and the document still says connected. So that one case is handled here,
            // where the old and new values are both known.
            if enabled && open_rgb.state() != ghub_core::openrgb_service::OpenRgbState::Disabled {
                open_rgb.start(&saved_host, port, allow_remote);
            }
        });

        self.saved_onboard_writes = onboard;
        self.saved_allow_remote = allow_remote;

        // The registry, not the document, so it is applied here rather than inside the edit. A
        // failure is reported: a tick that silently did nothing is worse than an error.
        if self.autostart != self.saved_autostart {
            let wanted = self.autostart;
            match ghub_platform::autostart::set_enabled(wanted) {
                Ok(()) => {
                    self.saved_autostart = wanted;
                    self.autostart_command = ghub_platform::autostart::registered_command();
                }
                Err(e) => {
                    self.autostart = self.saved_autostart;
                    shared.error = Some(format!("Start with Windows could not be changed: {e}"));
                }
            }
        }
    }

    fn reload(&mut self, shared: &Shared) {
        let app = shared.document().app.clone();
        self.open_rgb_enabled = app.open_rgb_enabled;
        self.open_rgb_host = app.open_rgb_host;
        self.open_rgb_port = i64::from(app.open_rgb_port);
        self.port_buffer = app.open_rgb_port.to_string();
        self.open_rgb_allow_remote = app.open_rgb_allow_remote;
        self.allow_onboard_writes = app.allow_onboard_writes;
        self.low_battery = app.low_battery_warnings;
        self.low_battery_threshold = i64::from(app.low_battery_threshold_percent);
        self.threshold_buffer = app.low_battery_threshold_percent.to_string();
        self.saved_onboard_writes = app.allow_onboard_writes;
        self.saved_allow_remote = app.open_rgb_allow_remote;
        self.open_rgb_state = shared.host.open_rgb.describe();
        // Read from the registry rather than remembered: the user may have removed the entry in
        // Task Manager since the last load, and the tick has to show what is actually there.
        self.autostart = ghub_platform::autostart::is_enabled();
        self.saved_autostart = self.autostart;
        self.autostart_command = ghub_platform::autostart::registered_command();
        self.loaded = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> OptionsPage {
        OptionsPage {
            open_rgb_host: "127.0.0.1".to_owned(),
            open_rgb_port: 6742,
            port_buffer: "6742".to_owned(),
            low_battery: true,
            low_battery_threshold: 15,
            threshold_buffer: "15".to_owned(),
            loaded: true,
            ..Default::default()
        }
    }

    /// Turning flash writes on is the one switch on this page that can damage something, so it
    /// must never save without a question.
    #[test]
    fn enabling_onboard_writes_asks_first() {
        let mut page = page();
        page.allow_onboard_writes = true;
        assert!(page.allow_onboard_writes && !page.saved_onboard_writes);
    }

    /// Turning it *off* must not ask: a confirmation on the safe direction trains people to click
    /// through confirmations.
    #[test]
    fn disabling_onboard_writes_asks_nothing() {
        let mut page = page();
        page.saved_onboard_writes = true;
        page.allow_onboard_writes = false;
        assert!(!page.allow_onboard_writes || page.saved_onboard_writes);
    }

    /// A loopback host with "allow remote" ticked is harmless, and asking about it would be
    /// asking about something that is not happening.
    #[test]
    fn a_loopback_host_is_never_treated_as_remote() {
        for host in ["127.0.0.1", "localhost", "::1", " 127.0.0.1 "] {
            assert!(OpenRgbClient::is_loopback(host.trim()), "{host} is loopback");
        }
    }

    #[test]
    fn a_real_address_is_remote_and_needs_consent() {
        for host in ["192.168.1.10", "203.0.113.1", "openrgb.example.com"] {
            assert!(!OpenRgbClient::is_loopback(host), "{host} is not loopback");
        }
    }

    /// An empty host field must not be saved as an empty host: the connect would fail with a
    /// resolver error that says nothing about the real mistake.
    #[test]
    fn an_empty_host_falls_back_to_loopback() {
        let host = "   ";
        let resolved =
            if host.trim().is_empty() { "127.0.0.1".to_owned() } else { host.trim().to_owned() };
        assert_eq!(resolved, "127.0.0.1");
    }

    /// The startup entry is an OS setting, so the page's tick has to come from the OS and not
    /// from the document - otherwise a document copied between machines would claim a startup
    /// entry that is not there.
    #[test]
    fn the_startup_tick_reflects_the_registry_not_the_document() {
        let page = page();
        assert!(!page.autostart, "a fresh page has not read the registry yet");
        assert_eq!(
            ghub_platform::autostart::is_enabled(),
            ghub_platform::autostart::registered_command().is_some()
        );
    }

    /// The port and threshold are clamped on the way into the document, because a document is
    /// also a file a user can edit by hand.
    #[test]
    fn the_numeric_settings_are_clamped_to_their_range() {
        assert_eq!(0i64.clamp(1, 65535) as u16, 1);
        assert_eq!(999_999i64.clamp(1, 65535) as u16, 65535);
        assert_eq!(0i64.clamp(1, 50) as u8, 1);
        assert_eq!(80i64.clamp(1, 50) as u8, 50);
    }
}
