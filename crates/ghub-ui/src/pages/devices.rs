//! The Devices page. Port of `PeripheralDevicesTabViewModel.cs` and `PeripheralDevicesTabView.axaml`.
//!
//! One card per discovered device, listing what the device said about itself and what it can do.
//! Every value here is read from the hardware or from the verified identifier table - nothing is
//! inferred, and a field the protocol does not expose says so rather than showing a plausible
//! number. That is why `Battery` distinguishes "No battery (wired / not exposed)" from "Not read
//! yet", and why an estimated percentage is labelled as estimated.

use egui::Ui;
use ghub_core::manager::ManagedDevice;
use ghub_hidpp::capability::{CapabilityStatus, PersistenceKind};
use ghub_hidpp::features::battery::ChargingState;
use ghub_hidpp::identity::{ConnectionType, DeviceConnectionState};

use crate::controls as c;
use crate::icons::{Icon, IconCache};
use crate::shared::{Changes, Shared, UNAVAILABLE_REASON};
use crate::theme as t;
use crate::widgets as w;

use super::{scroll, status_band};

/// One row of the capability table: what it is, whether it is there, and the evidence.
struct CapabilityRow {
    name: &'static str,
    status: String,
    detail: String,
}

/// A device card, built from a [`ManagedDevice`] snapshot. `DeviceCardViewModel` in the C#.
struct DeviceCard {
    key: String,
    /// G HUB's own glyph for how the device is attached, when the descriptor says.
    connection_icon: Option<Icon>,
    /// G HUB's own battery glyph for the level it reported, when it reported one.
    battery_icon: Option<Icon>,
    name: String,
    known: &'static str,
    state: String,
    connection: String,
    usb_ids: String,
    firmware: String,
    battery: String,
    last_seen: String,
    error: Option<String>,
    capabilities: Vec<CapabilityRow>,
    raw_features: String,
}

impl DeviceCard {
    fn new(device: &ManagedDevice) -> Self {
        let descriptor = device.descriptor.as_ref();

        let connection = match descriptor {
            None => "-".to_owned(),
            Some(d) => match d.connection_type {
                ConnectionType::LightspeedReceiver => format!(
                    "LIGHTSPEED receiver (slot {})",
                    d.device_index.map_or_else(|| "?".to_owned(), |i| i.to_string())
                ),
                ConnectionType::Wired => "USB cable".to_owned(),
                other => other.label().to_owned(),
            },
        };

        // `d.IsConnected ? "Connected" : d.Health.State.ToString()`.
        let state = if device.connected {
            "Connected".to_owned()
        } else {
            match device.health.state {
                DeviceConnectionState::Connected => "Connected",
                DeviceConnectionState::Disconnected => "Disconnected",
                DeviceConnectionState::Error => "Error",
            }
            .to_owned()
        };

        let battery = match device.health.battery {
            Some(b) => {
                let level = b.level_percent.map_or_else(|| "?".to_owned(), |p| format!("{p}%"));
                let voltage =
                    b.voltage_millivolts.map_or_else(String::new, |mv| format!(" ({mv} mV)"));
                let low = if b.is_low { " · LOW" } else { "" };
                // The source string is carried through so an estimate is never read as a
                // measurement. This is the whole reason `BatteryStatus::source` exists.
                let estimated = if b.source.contains("estimated") {
                    " · % estimated from voltage"
                } else {
                    ""
                };
                format!("{level}{voltage} · {:?}{low}{estimated}", b.charging)
            }
            None if device.capabilities.battery.is_none() => {
                "No battery (wired / not exposed)".to_owned()
            }
            None => "Not read yet".to_owned(),
        };

        let capabilities = device
            .capabilities
            .enumerate()
            .into_iter()
            .map(|(name, meta)| {
                let status = match meta {
                    None => "Not present".to_owned(),
                    Some(m) => match m.status {
                        CapabilityStatus::Supported => {
                            if m.persistence == PersistenceKind::Software {
                                "Software equivalent"
                            } else {
                                "Native hardware"
                            }
                        }
                        CapabilityStatus::ReadOnly => "Read-only",
                        CapabilityStatus::NotSupported => "Unavailable on this hardware",
                        // Present but unreachable right now: asleep, owned elsewhere, or
                        // erroring. The C# folded this into its `_ =>` arm.
                        CapabilityStatus::Unavailable => "Unavailable",
                    }
                    .to_owned(),
                };
                let detail = match meta {
                    Some(m) => match (&m.reason, m.feature_id) {
                        (Some(reason), _) => reason.clone(),
                        (None, Some(feature)) => {
                            format!("HID++ 0x{feature:04X}, {:?}", m.persistence)
                        }
                        (None, None) => String::new(),
                    },
                    None => String::new(),
                };
                CapabilityRow { name, status, detail }
            })
            .collect();

        Self {
            key: device.key(),
            connection_icon: descriptor.map(|d| connection_glyph(d.connection_type)),
            battery_icon: device.health.battery.map(|b| battery_icon(b.charging, b.level_percent)),
            name: descriptor
                .map(|d| d.display_name.clone())
                .unwrap_or_else(|| device.key()),
            known: if descriptor.is_some_and(|d| d.is_known_model) {
                "Verified identifier table"
            } else {
                "Unknown model: generic HID++ capabilities only"
            },
            state,
            connection,
            usb_ids: descriptor.map(|d| d.usb_id_string()).unwrap_or_else(|| "-".to_owned()),
            firmware: descriptor
                .and_then(|d| d.firmware_version.clone())
                .unwrap_or_else(|| "Not reported".to_owned()),
            battery,
            last_seen: device
                .last_seen
                .map(ghub_core::time::format_readable)
                .unwrap_or_else(|| "never".to_owned()),
            error: device.last_error.clone().or_else(|| device.health.last_error.clone()),
            capabilities,
            raw_features: device.capabilities.raw_features.join("\n"),
        }
    }
}

#[derive(Default)]
pub struct DevicesPage {
    cards: Vec<DeviceCard>,
    /// Which cards have their HID++ feature table expanded. `<Expander Header="HID++ feature
    /// table">` was collapsed by default and remembered per card.
    expanded: std::collections::HashSet<String>,
    loaded: bool,
}

impl DevicesPage {
    pub fn show(
        &mut self,
        shared: &mut Shared,
        icons: &mut IconCache,
        ui: &mut Ui,
        changes: Changes,
    ) {
        // `Refresh()`: on device change, and once on first show.
        if changes.devices || !self.loaded {
            self.reload(shared);
        }

        scroll(ui, "devices", |ui| {
            // `MaxWidth="1200"` on the page body.
            ui.set_max_width(1200.0);

            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_max_width(ui.available_width() - 200.0);
                    w::title(ui, "Peripherals");
                    ui.add_space(4.0);
                    w::body(
                        ui,
                        "Logitech G devices managed natively over HID++ (no G HUB needed). \
                         Settings come from the active profile:",
                    );
                    ui.add_space(2.0);
                    let explain = shared
                        .service()
                        .active()
                        .map(|r| r.explain())
                        .unwrap_or_else(|| "-".to_owned());
                    w::row(ui, &explain);
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                    if c::button(ui, "Scan for devices", c::ButtonStyle::Primary, !shared.busy())
                        .on_hover_text("Re-enumerate USB / LIGHTSPEED devices")
                        .clicked()
                    {
                        self.rescan(shared);
                    }
                });
            });

            ui.add_space(10.0);
            status_band(ui, shared);

            if !cfg!(windows) {
                ui.add_space(10.0);
                w::card(ui, |ui| w::body(ui, UNAVAILABLE_REASON));
            }

            if self.cards.is_empty() {
                ui.add_space(10.0);
                w::card(ui, |ui| {
                    w::body(
                        ui,
                        "No supported devices found. Plug in the G815 or the G903 (cable or \
                         LIGHTSPEED receiver), make sure the mouse is switched on, then press \
                         \"Scan for devices\". If a device is still missing, open Diagnostics.",
                    );
                });
            }

            ui.add_space(10.0);
            // Collected first so the card loop does not borrow `self` while calling into `shared`.
            let mut apply = None;
            let mut toggle = None;
            for card in &self.cards {
                w::card(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.set_max_width(ui.available_width() - 190.0);
                            w::title(ui, &card.name);
                            w::body_small(ui, card.known, t::TEXT_LABEL);
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                            if c::button(
                                ui,
                                "Apply active profile",
                                c::ButtonStyle::Outlined,
                                !shared.busy(),
                            )
                            .clicked()
                            {
                                apply = Some(card.key.clone());
                            }
                        });
                    });

                    ui.add_space(6.0);
                    // `ColumnDefinitions="160,*"`, with a 20px glyph column in front of the
                    // value for the two rows G HUB itself illustrates.
                    for (caption, value, glyph) in [
                        ("State", &card.state, None),
                        ("Connection", &card.connection, card.connection_icon),
                        ("USB ids", &card.usb_ids, None),
                        ("Firmware (read-only)", &card.firmware, None),
                        ("Battery", &card.battery, card.battery_icon),
                        ("Last seen", &card.last_seen, None),
                    ] {
                        ui.horizontal(|ui| {
                            ui.allocate_ui(egui::vec2(160.0, 18.0), |ui| w::body(ui, caption));
                            // Allocated whether or not there is a glyph, so the values stay on
                            // one left edge rather than jogging in and out by row.
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(20.0, 18.0),
                                egui::Sense::hover(),
                            );
                            if let Some(glyph) = glyph {
                                icons.draw(ui, glyph, rect, 16.0, t::TEXT_LABEL);
                            }
                            w::body(ui, value);
                        });
                    }

                    if let Some(error) = &card.error {
                        ui.add_space(4.0);
                        w::body_small(ui, error, t::ERROR);
                    }

                    ui.add_space(8.0);
                    w::row(ui, "Capabilities (discovered from the device)");
                    ui.add_space(4.0);
                    for row in &card.capabilities {
                        // `ColumnDefinitions="160,220,*"`.
                        ui.horizontal(|ui| {
                            ui.allocate_ui(egui::vec2(160.0, 18.0), |ui| w::body(ui, row.name));
                            ui.allocate_ui(egui::vec2(220.0, 18.0), |ui| w::body(ui, &row.status));
                            w::body_small(ui, &row.detail, t::TEXT_LABEL);
                        });
                    }

                    ui.add_space(6.0);
                    let open = self.expanded.contains(&card.key);
                    let caption =
                        if open { "HID++ feature table ▾" } else { "HID++ feature table ▸" };
                    if w::link(ui, caption, true).clicked() {
                        toggle = Some(card.key.clone());
                    }
                    if open {
                        // `SelectableTextBlock FontFamily="Consolas,monospace" FontSize="11"`.
                        let mut text = card.raw_features.clone();
                        ui.add(
                            egui::TextEdit::multiline(&mut text)
                                .desired_width(f32::INFINITY)
                                .font(egui::FontId::new(11.0, t::family(t::MONO)))
                                .text_color(t::TEXT_ROW)
                                .frame(egui::Frame::NONE)
                                // Still selectable, which is what `SelectableTextBlock`
                                // gave: the feature table is something a user gets asked to copy
                                // into a bug report.
                                .interactive(true),
                        );
                    }
                });
                ui.add_space(10.0);
            }

            if let Some(key) = toggle {
                if !self.expanded.remove(&key) {
                    self.expanded.insert(key);
                }
            }
            if let Some(key) = apply {
                self.apply(shared, key);
            }
        });
    }

    fn reload(&mut self, shared: &Shared) {
        // `OrderBy(Kind).ThenBy(DisplayName)`.
        let mut devices = shared.host.devices.devices();
        // `PeripheralKind` has no ordering of its own, so the key is its debug label. That
        // happens to put Keyboard before Mouse, which is the order the C# produced from the
        // enum's declaration order too.
        devices.sort_by_key(|d| {
            let descriptor = d.descriptor.as_ref();
            (
                descriptor.map(|x| format!("{:?}", x.kind)).unwrap_or_default(),
                descriptor.map(|x| x.display_name.clone()).unwrap_or_default(),
            )
        });
        self.cards = devices.iter().map(DeviceCard::new).collect();
        self.loaded = true;
    }

    fn rescan(&self, shared: &mut Shared) {
        let manager = std::sync::Arc::clone(&shared.host.devices);
        shared.run("Rescan", move || {
            manager.scan();
            let connected = manager.devices().iter().filter(|d| d.connected).count();
            Ok(Some(format!("Scan complete: {connected} device(s) connected")))
        });
    }

    fn apply(&self, shared: &mut Shared, key: String) {
        let service = std::sync::Arc::clone(shared.service());
        let manager = std::sync::Arc::clone(&shared.host.devices);
        shared.run("Apply profile", move || {
            let Some(device) = manager.get(&key) else {
                return Ok(Some("Device not found".to_owned()));
            };
            let results = service.apply_to_device(&device);
            Ok(Some(if results.is_empty() {
                "Nothing to apply for this device in the active profile".to_owned()
            } else {
                results.join(" · ")
            }))
        });
    }
}

/// The glyph for a battery's state: charging wins over level, as it does in G HUB.
///
/// `ChargingSlow` is charging too - firmware reports it separately because the rate differs, not
/// because the battery is doing something else. `Full` is the 100% glyph rather than the charging
/// one, because a full battery is no longer charging.
fn battery_icon(charging: ChargingState, level_percent: Option<u8>) -> Icon {
    match charging {
        ChargingState::Charging | ChargingState::ChargingSlow => Icon::BatteryCharging,
        ChargingState::Full => Icon::Battery100,
        ChargingState::Discharging | ChargingState::Error => battery_glyph(level_percent),
    }
}

/// G HUB's own battery glyph for a reported percentage.
///
/// Five steps, because that is what G HUB ships: `battery_0`, `_25`, `_50`, `_75`, `_100`. The
/// thresholds round to the nearest step rather than down, so a 90% battery does not show the 75%
/// glyph. A device that reported no percentage at all gets the empty glyph, because the row's text
/// already says whether the level is unknown - and an unknown level is not a full battery.
fn battery_glyph(level_percent: Option<u8>) -> Icon {
    match level_percent.unwrap_or(0) {
        0..=12 => Icon::Battery0,
        13..=37 => Icon::Battery25,
        38..=62 => Icon::Battery50,
        63..=87 => Icon::Battery75,
        _ => Icon::Battery100,
    }
}

/// G HUB's own glyph for how a device is attached.
///
/// G HUB ships three: USB, LIGHTSPEED and Bluetooth. A receiver is LIGHTSPEED, a cable is USB, and
/// anything else falls back to USB rather than being left blank, because every connection this
/// application can see is one of the two wired-or-receiver cases.
fn connection_glyph(connection: ConnectionType) -> Icon {
    match connection {
        ConnectionType::LightspeedReceiver => Icon::ConnectionLightspeed,
        ConnectionType::Bluetooth => Icon::ConnectionBluetooth,
        _ => Icon::ConnectionUsb,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The glyph has to match what the row says, and the interesting cases are the boundaries:
    /// G HUB's five steps are 25 points apart, so rounding down would show 75% for a nearly full
    /// battery.
    #[test]
    fn the_battery_glyph_rounds_to_the_nearest_step_g_hub_ships() {
        assert_eq!(battery_glyph(Some(0)), Icon::Battery0);
        assert_eq!(battery_glyph(Some(12)), Icon::Battery0);
        assert_eq!(battery_glyph(Some(13)), Icon::Battery25);
        assert_eq!(battery_glyph(Some(50)), Icon::Battery50);
        assert_eq!(battery_glyph(Some(90)), Icon::Battery100);
        assert_eq!(battery_glyph(Some(100)), Icon::Battery100);
    }

    /// An unreported level is not a full battery. The row's text says "Not read yet"; the glyph
    /// must not contradict it.
    #[test]
    fn an_unknown_battery_level_is_not_drawn_as_full() {
        assert_eq!(battery_glyph(None), Icon::Battery0);
    }

    /// Charging is a state, not a level: a device at 10% that is plugged in must not show the
    /// empty-battery glyph, and a full one must not show the charging glyph forever.
    #[test]
    fn charging_outranks_the_level_and_full_does_not() {
        assert_eq!(battery_icon(ChargingState::Charging, Some(10)), Icon::BatteryCharging);
        assert_eq!(battery_icon(ChargingState::ChargingSlow, Some(10)), Icon::BatteryCharging);
        assert_eq!(battery_icon(ChargingState::Full, Some(99)), Icon::Battery100);
        assert_eq!(battery_icon(ChargingState::Discharging, Some(10)), Icon::Battery0);
    }

    #[test]
    fn each_connection_kind_gets_the_glyph_g_hub_uses_for_it() {
        assert_eq!(
            connection_glyph(ConnectionType::LightspeedReceiver),
            Icon::ConnectionLightspeed
        );
        assert_eq!(connection_glyph(ConnectionType::Wired), Icon::ConnectionUsb);
        assert_eq!(connection_glyph(ConnectionType::Bluetooth), Icon::ConnectionBluetooth);
    }
}
