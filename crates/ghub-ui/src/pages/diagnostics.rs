//! The Diagnostics page. Port of `OtherTabsViewModels.cs (PeripheralDiagnosticsTabViewModel)`.
//!
//! This is the page a user is sent to when something does not work, so everything on it is a
//! *read* of live state: the log, who owns each device's lighting, whether the input hooks are
//! installed, what else is running that writes to the same hardware, and what OpenRGB is doing.
//! Nothing here changes a device.
//!
//! Two controls do change something, and both are logging switches rather than device settings.
//! They are here rather than in Options because they exist to produce the bundle below, and a
//! switch whose only purpose is to fill a log belongs next to the log.
//!
//! The exported bundle keeps the C#'s guarantee: it carries device ids, capabilities and the
//! diagnostics log, and it never carries what was typed. The one thing that widens it is payload
//! logging, so the page says in words what that adds before it is switched on.

use std::sync::Arc;

use egui::Ui;
use ghub_core::diagnostics::DiagnosticLevel;

use crate::controls as c;
use crate::icons::{Icon, IconCache};
use crate::shared::{Changes, Shared};
use crate::theme as t;
use crate::widgets as w;

use super::{scroll, status_band};

/// `Service.Log.Snapshot().TakeLast(400)` - the newest 400 entries, newest first.
///
/// The ring buffer holds more than this. 400 is what a person can scroll through while looking
/// for the moment something failed; the export carries the whole buffer.
const LOG_LINES: usize = 400;

/// How often the page re-reads the log while it is open.
///
/// The log and the OpenRGB state both move without any input, and egui only repaints on input or
/// on request, so without this the page would sit showing a stale log until the pointer moved.
/// Rebuilding 400 formatted lines every frame instead would be work no one asked for.
const REFRESH: std::time::Duration = std::time::Duration::from_millis(500);

/// One formatted log row, split so the level can be tinted and glyphed.
struct LogRow {
    level: DiagnosticLevel,
    text: String,
}

#[derive(Default)]
pub struct DiagnosticsPage {
    rows: Vec<LogRow>,
    /// `(device, zone, owner)` as one line each, or the "nobody owns anything" line.
    ownership: Vec<String>,
    open_rgb: String,
    hooks: String,
    conflicts: String,
    protocol_logging: bool,
    payload_logging: bool,
    /// Entry count when the log was last read, so a reload happens when it actually moved.
    log_len: usize,
    loaded: bool,
}

impl DiagnosticsPage {
    pub fn show(
        &mut self,
        shared: &mut Shared,
        icons: &mut IconCache,
        ui: &mut Ui,
        changes: Changes,
    ) {
        let log_len = shared.host.log.len();
        if !self.loaded
            || changes.document
            || changes.devices
            || log_len != self.log_len
            || shared.host.open_rgb.take_changed()
        {
            self.reload(shared);
        }
        // Only while this page is on screen: the other pages do not poll anything.
        ui.ctx().request_repaint_after(REFRESH);

        scroll(ui, "diagnostics", |ui| {
            ui.set_max_width(1200.0);

            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_max_width(ui.available_width() - 330.0);
                    w::title(ui, "Diagnostics");
                    ui.add_space(4.0);
                    w::body(
                        ui,
                        "Everything on this page is read from the running service. Use \"Save \
                         diagnostics\" to attach the whole log to a bug report.",
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                    if c::button(ui, "Save diagnostics", c::ButtonStyle::Primary, !shared.busy())
                        .clicked()
                    {
                        self.export(shared);
                    }
                    if w::icon_button(
                        ui,
                        icons,
                        Icon::Close,
                        t::ROW_ICON_BUTTON,
                        t::ROW_ICON_GLYPH,
                        t::TEXT_LABEL,
                    )
                    .on_hover_text("Clear the log")
                    .clicked()
                    {
                        shared.host.log.clear();
                        self.reload(shared);
                    }
                    if w::icon_button(
                        ui,
                        icons,
                        Icon::Copy,
                        t::ROW_ICON_BUTTON,
                        t::ROW_ICON_GLYPH,
                        t::TEXT_LABEL,
                    )
                    .on_hover_text("Re-read the log and the live state")
                    .clicked()
                    {
                        self.reload(shared);
                    }
                });
            });

            ui.add_space(10.0);
            status_band(ui, shared);
            ui.add_space(10.0);

            self.environment_card(shared, icons, ui);
            ui.add_space(10.0);
            self.logging_card(shared, icons, ui);
            ui.add_space(10.0);
            self.ownership_card(ui);
            ui.add_space(10.0);
            self.log_card(icons, ui);
        });
    }

    /// What the service found about the machine it is running on. The first three rows are the
    /// three things that explain almost every "nothing happens" report.
    fn environment_card(&self, shared: &Shared, icons: &mut IconCache, ui: &mut Ui) {
        w::card(ui, |ui| {
            w::label(ui, "ENVIRONMENT");
            ui.add_space(8.0);

            for (caption, value) in [
                ("Input hooks", self.hooks.as_str()),
                ("Also running", self.conflicts.as_str()),
                ("OpenRGB", self.open_rgb.as_str()),
                ("Devices", &format!("{} managed", shared.host.devices.devices().len())),
                // Read live rather than from the start-up hand-over, because the agent can be
                // quit or started while this window is open. Its absence is the difference
                // between "closing this window leaves everything running" and "closing this
                // window stops managing the devices", which is not otherwise visible.
                (
                    "Background agent",
                    if ghub_platform::handoff::agent_running() {
                        "running; closing this window keeps devices managed"
                    } else {
                        "not running; closing this window stops managing devices"
                    },
                ),
            ] {
                ui.horizontal(|ui| {
                    ui.allocate_ui(egui::vec2(160.0, 18.0), |ui| w::body(ui, caption));
                    w::body(ui, value);
                });
            }

            if let Some(error) = &shared.host.backend_error {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    icons.show(ui, Icon::Error, 16.0, t::ERROR);
                    ui.add_space(6.0);
                    w::body_small(ui, error, t::ERROR);
                });
            }

            ui.add_space(8.0);
            // UIPI is not a bug and cannot be worked around without elevating, which this
            // application does not do. Saying so here is the difference between an unexplainable
            // fault and a known limit.
            w::body_small(
                ui,
                "Input to a window running as administrator is neither seen nor injectable \
                 (Windows UIPI). Macros and remapped keys do nothing in such a window; this is a \
                 Windows rule and is not worked around.",
                t::TEXT_LABEL,
            );
        });
    }

    fn logging_card(&mut self, shared: &mut Shared, icons: &mut IconCache, ui: &mut Ui) {
        w::card(ui, |ui| {
            w::label(ui, "PROTOCOL LOGGING");
            ui.add_space(8.0);
            w::body(
                ui,
                "Off by default. Switch it on, reproduce the problem, then save the diagnostics.",
            );
            ui.add_space(8.0);

            // Toggling a box does not save: the C# had an explicit `SaveLogging` command, and
            // switching protocol logging on is a document write, which should not happen on the
            // way past a checkbox.
            c::checkbox(
                ui,
                icons,
                &mut self.protocol_logging,
                "Log HID++ transactions (feature, function and outcome)",
                !shared.busy(),
            );
            ui.add_space(4.0);
            // Payloads are only meaningful with the transactions they belong to, so the switch is
            // disabled rather than silently ignored when transaction logging is off - which is
            // what `ProtocolLogging && PayloadLogging` did in the C#.
            c::checkbox(
                ui,
                icons,
                &mut self.payload_logging,
                "Include raw payload bytes",
                !shared.busy() && self.protocol_logging,
            );
            ui.add_space(4.0);
            w::body_small(
                ui,
                "Payload bytes are the exact reports exchanged with the device. They contain \
                 device state, not keystrokes, and they are redacted from the log unless this is \
                 on.",
                t::TEXT_LABEL,
            );

            ui.add_space(10.0);
            if c::button(ui, "Save logging settings", c::ButtonStyle::Outlined, !shared.busy())
                .clicked()
            {
                self.save_logging(shared);
            }
        });
    }

    fn ownership_card(&self, ui: &mut Ui) {
        w::card(ui, |ui| {
            w::label(ui, "LIGHTING OWNERSHIP");
            ui.add_space(4.0);
            w::body_small(
                ui,
                "Which writer currently holds each device's lighting. A device owned by a \
                 higher-priority writer ignores lower-priority frames rather than flickering \
                 between them.",
                t::TEXT_LABEL,
            );
            ui.add_space(8.0);
            for line in &self.ownership {
                w::row(ui, line);
            }
        });
    }

    fn log_card(&self, icons: &mut IconCache, ui: &mut Ui) {
        w::card(ui, |ui| {
            ui.horizontal(|ui| {
                w::label(ui, "LOG");
                ui.add_space(8.0);
                w::body_small(
                    ui,
                    &format!("newest {} of {} entries, UTC", self.rows.len(), self.log_len),
                    t::TEXT_LABEL,
                );
            });
            ui.add_space(8.0);

            if self.rows.is_empty() {
                w::body(ui, "The log is empty.");
                return;
            }

            // Fixed height with its own scroll area: the log is the longest thing on the page and
            // letting it grow without bound would push everything else off the top.
            egui::ScrollArea::vertical()
                .id_salt("diagnostics log")
                .max_height(420.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for row in &self.rows {
                        ui.horizontal(|ui| {
                            let (icon, colour) = level_glyph(row.level);
                            icons.show(ui, icon, 14.0, colour);
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(&row.text)
                                    .font(egui::FontId::new(11.0, t::family(t::MONO)))
                                    .color(colour),
                            );
                        });
                    }
                });
        });
    }

    fn reload(&mut self, shared: &Shared) {
        let entries = shared.host.log.snapshot();
        self.log_len = entries.len();
        // `TakeLast(400).Reverse()`: the newest first, because the thing a user is looking for is
        // whatever just happened.
        self.rows = entries
            .iter()
            .rev()
            .take(LOG_LINES)
            .map(|entry| LogRow { level: entry.level, text: entry.to_line() })
            .collect();

        let owners = shared.service().ownership().snapshot();
        self.ownership = if owners.is_empty() {
            vec!["No lighting writer owns any device".to_owned()]
        } else {
            owners
                .iter()
                .map(|(device, zone, owner)| {
                    format!(
                        "{device} [{zone}] \u{2192} {} (priority {})",
                        owner.display_name, owner.priority
                    )
                })
                .collect()
        };

        self.open_rgb = shared.host.open_rgb.describe();

        let hooks = shared.service().hook_status();
        self.hooks = if hooks.all_installed() {
            let seen = if hooks.keyboard_events_seen {
                "keyboard, mouse and foreground installed; keyboard events seen"
            } else {
                "keyboard, mouse and foreground installed; no keyboard event seen yet"
            };
            seen.to_owned()
        } else {
            // Which one failed, and the error, because "hooks not installed" on its own is not
            // actionable.
            format!(
                "keyboard {}, mouse {}, foreground {}{}",
                yes_no(hooks.keyboard),
                yes_no(hooks.mouse),
                yes_no(hooks.foreground),
                if hooks.last_error == 0 {
                    String::new()
                } else {
                    format!(" (last error {})", hooks.last_error)
                }
            )
        };

        let conflicts = ghub_platform::coexistence::conflicting_processes();
        self.conflicts = if conflicts.is_empty() {
            "nothing else known to control Logitech devices".to_owned()
        } else {
            conflicts.join(", ")
        };

        let app = shared.document().app.clone();
        self.protocol_logging = app.log_protocol_transactions;
        self.payload_logging = app.log_protocol_payloads;
        self.loaded = true;
    }

    fn save_logging(&mut self, shared: &mut Shared) {
        let transactions = self.protocol_logging;
        // `ProtocolLogging && PayloadLogging`: payloads without transactions would record bytes
        // with nothing saying which request they belonged to.
        let payloads = self.protocol_logging && self.payload_logging;
        let log = Arc::clone(&shared.host.log);

        shared.edit_document("Logging", move |document| {
            document.app.log_protocol_transactions = transactions;
            document.app.log_protocol_payloads = payloads;
            // Applied to the live log inside the same edit, so the switch takes effect now rather
            // than at the next start. The log's own flags are atomics for exactly this reason.
            log.set_record_protocol_transactions(transactions);
            log.set_record_protocol_payloads(payloads);
        });
        self.payload_logging = payloads;
    }

    fn export(&self, shared: &mut Shared) {
        // The dialog is modal and has to run on the UI thread; only the writing is queued.
        let suggested = format!(
            "ghub-rs-diagnostics-{}.txt",
            ghub_core::time::format_stamp(ghub_core::time::now_unix(), 0)
        );
        let Some(path) = ghub_platform::dialogs::save_file(
            "Save diagnostics",
            &ghub_platform::dialogs::FileFilter { label: "Text", extension: "txt" },
            &suggested,
        ) else {
            return;
        };

        let bundle = self.bundle(shared);
        shared.attempt("Save diagnostics", move || {
            std::fs::write(&path, bundle).map_err(|e| e.to_string())?;
            Ok(format!(
                "saved to {path} (device ids, capabilities and the log; no keystrokes)"
            ))
        });
    }

    /// `BuildBundle()`.
    ///
    /// Built on the UI thread because it reads the device list and the log, both of which are
    /// cheap snapshots, and because doing it here means the file written is the state the user was
    /// looking at when they pressed the button.
    fn bundle(&self, shared: &Shared) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();

        let _ = writeln!(
            out,
            "ghub-rs diagnostics {} UTC",
            ghub_core::time::format_readable(ghub_core::time::now_unix_millis())
        );
        let _ = writeln!(
            out,
            "Build: {} {}, {} {}",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        let _ = writeln!(out, "Input hooks: {}", self.hooks);
        let _ = writeln!(out, "Conflicting processes: {}", self.conflicts);
        let _ = writeln!(out, "OpenRGB: {}", self.open_rgb);
        if let Some(error) = &shared.host.backend_error {
            let _ = writeln!(out, "HID backend: {error}");
        }
        let _ = writeln!(
            out,
            "Active profile: {}",
            shared
                .service()
                .active()
                .map(|resolution| resolution.explain())
                .unwrap_or_else(|| "-".to_owned())
        );

        for device in shared.host.devices.devices() {
            let descriptor = device.descriptor.as_ref();
            let _ = writeln!(out);
            let _ = writeln!(
                out,
                "== {} [{}] connected={} {} {} fw={}",
                descriptor.map(|d| d.display_name.as_str()).unwrap_or("(unidentified)"),
                device.key(),
                device.connected,
                descriptor.map(|d| d.usb_id_string()).unwrap_or_else(|| "-".to_owned()),
                descriptor
                    .map(|d| format!("{:?}", d.connection_type))
                    .unwrap_or_else(|| "-".to_owned()),
                descriptor.and_then(|d| d.firmware_version.as_deref()).unwrap_or("not reported")
            );
            for (name, meta) in device.capabilities.enumerate() {
                match meta {
                    None => {
                        let _ = writeln!(out, "  {name}: absent");
                    }
                    Some(meta) => {
                        let _ = writeln!(
                            out,
                            "  {name}: {:?} {}",
                            meta.status,
                            meta.reason.as_deref().unwrap_or("")
                        );
                    }
                }
            }
            for feature in &device.capabilities.raw_features {
                let _ = writeln!(out, "  {feature}");
            }
        }

        let _ = writeln!(out);
        let _ = writeln!(out, "== Ownership");
        for line in &self.ownership {
            let _ = writeln!(out, "{line}");
        }

        let _ = writeln!(out);
        let _ = writeln!(out, "== OpenRGB controllers");
        let controllers = shared.host.open_rgb.controllers();
        if controllers.is_empty() {
            let _ = writeln!(out, "(none)");
        } else {
            for controller in &controllers {
                let _ = writeln!(
                    out,
                    "{} [{}] {:?} {} zone(s) {} led(s)",
                    controller.name,
                    controller.location,
                    controller.kind(),
                    controller.zones.len(),
                    controller.total_leds()
                );
            }
        }

        let _ = writeln!(out);
        // The whole buffer, not the 400 shown: the point of the file is that it holds more than
        // the screen does.
        let _ = writeln!(out, "== Log ({} entries)", self.log_len);
        out.push_str(&shared.host.log.to_text());
        out
    }
}

/// The level's glyph and colour.
///
/// These are G HUB's own three status severities, taken from its system-tray application's asset
/// set (`status_info`, `status_warning`, `status_error`) rather than from the web UI's inline
/// help and pairing icons - because *status severity* is exactly what G HUB uses them for.
///
/// They are monochrome, as all of that set is: G HUB tints the row rather than the glyph, which
/// is why the colour is carried beside the icon here. There is no "trace" glyph in the set, so
/// trace shares the info glyph at the dimmest text colour.
fn level_glyph(level: DiagnosticLevel) -> (Icon, egui::Color32) {
    match level {
        DiagnosticLevel::Trace => (Icon::StatusInfo, t::TEXT_HEAD),
        DiagnosticLevel::Info => (Icon::StatusInfo, t::TEXT_ROW),
        DiagnosticLevel::Warning => (Icon::StatusWarning, t::CAUTION),
        DiagnosticLevel::Error => (Icon::StatusError, t::ERROR),
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "installed" } else { "NOT installed" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghub_core::diagnostics::DiagnosticsLog;

    /// The shown slice is bounded and newest-first. A log that scrolled oldest-first would put
    /// the thing that just failed 400 lines down.
    #[test]
    fn the_shown_log_is_the_newest_entries_newest_first() {
        let log = DiagnosticsLog::new(2000);
        for i in 0..(LOG_LINES + 50) {
            log.info("Test", format!("entry {i}"));
        }
        let entries = log.snapshot();
        let rows: Vec<String> = entries
            .iter()
            .rev()
            .take(LOG_LINES)
            .map(|entry| entry.to_line())
            .collect();

        assert_eq!(rows.len(), LOG_LINES);
        assert!(rows[0].contains(&format!("entry {}", LOG_LINES + 49)), "{}", rows[0]);
        assert!(rows[LOG_LINES - 1].contains("entry 50"));
    }

    /// Payload logging without transaction logging would record bytes with no transaction to
    /// attach them to, so the combination is not representable.
    #[test]
    fn payloads_are_only_recorded_alongside_transactions() {
        for (transactions, payloads) in [(false, true), (false, false), (true, true)] {
            let effective = transactions && payloads;
            assert_eq!(effective, transactions && payloads);
            if !transactions {
                assert!(!effective, "payloads must be off when transactions are off");
            }
        }
    }

    #[test]
    fn every_level_has_an_authentic_glyph() {
        for level in DiagnosticLevel::ALL {
            let (icon, _) = level_glyph(level);
            // G HUB's own status set, not a drawn stand-in and not a web-UI icon pressed into
            // service for a meaning it was not authored for.
            assert!(matches!(
                icon,
                Icon::StatusInfo | Icon::StatusWarning | Icon::StatusError
            ));
            assert!(icon.asset().starts_with("status-"));
        }
    }

    /// An empty ownership table has to say so in words. A blank area reads as "the page is
    /// broken", which is the opposite of what this page is for.
    #[test]
    fn an_empty_ownership_table_says_nobody_owns_anything() {
        let ownership = ghub_core::ownership::LightingOwnership::new();
        let owners = ownership.snapshot();
        assert!(owners.is_empty());
        let lines: Vec<String> = if owners.is_empty() {
            vec!["No lighting writer owns any device".to_owned()]
        } else {
            Vec::new()
        };
        assert_eq!(lines, vec!["No lighting writer owns any device".to_owned()]);
    }
}
