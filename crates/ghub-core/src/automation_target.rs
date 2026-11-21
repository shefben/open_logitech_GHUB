//! The automation API's view of the service. Port of
//! `Artemis.Peripherals/Automation/PeripheralAutomationTarget.cs`.
//!
//! Every method here is either a read, an ordinary profile edit, or a volatile device write - the
//! same three things the UI can do. The API deliberately cannot do anything the UI cannot: there
//! is no endpoint that writes flash, changes the onboard-write policy, or reaches a device the
//! service is not already managing. A loopback HTTP server with a bearer token is still a remote
//! control for the user's hardware, so its reach is bounded here rather than in the server.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::Id;
use crate::automation::AutomationTarget;
use crate::layout;
use crate::ownership::ALL_ZONES;
use crate::profile::parse_color;
use crate::service::PeripheralService;

/// Maps the automation API onto a [`PeripheralService`].
pub struct PeripheralAutomationTarget {
    service: Arc<PeripheralService>,
}

impl PeripheralAutomationTarget {
    pub fn new(service: Arc<PeripheralService>) -> Self {
        Self { service }
    }
}

impl AutomationTarget for PeripheralAutomationTarget {
    fn status(&self) -> Value {
        let active = self.service.active_profile();
        let document = self.service.document();
        json!({
            "activeProfile": {
                "id": active.id.to_string(),
                "name": active.name,
                // The *reason* as well as the name: a script that switched a profile and sees
                // another one active needs to know whether a lock or a rule outranked it.
                "reason": self
                    .service
                    .active()
                    .map(|resolution| format!("{:?}", resolution.reason))
                    .unwrap_or_default(),
            },
            "locked": document.locked_profile_id.map(|id| id.to_string()),
            "gShift": self.service.gshift_held(),
            "recording": format!("{:?}", self.service.record_state()),
            "devices": self.service.devices().devices().iter().filter(|d| d.connected).count(),
        })
    }

    fn devices(&self) -> Value {
        let devices: Vec<Value> = self
            .service
            .devices()
            .devices()
            .iter()
            .map(|device| {
                let descriptor = device.descriptor.as_ref();
                json!({
                    "key": device.key(),
                    "name": descriptor.map(|d| d.display_name.clone()),
                    "kind": descriptor.map(|d| format!("{:?}", d.kind)),
                    "connection": descriptor.map(|d| format!("{:?}", d.connection_type)),
                    "connected": device.connected,
                    "firmware": descriptor.and_then(|d| d.firmware_version.clone()),
                    "battery": device.health.battery.map(|battery| json!({
                        "percent": battery.level_percent,
                        "millivolts": battery.voltage_millivolts,
                        "charging": format!("{:?}", battery.charging),
                        // Carried through so a script cannot mistake a voltage estimate for a
                        // reading the device actually reported.
                        "estimated": battery.source.contains("estimated"),
                    })),
                    "bank": device
                        .capabilities
                        .mode_keys
                        .as_ref()
                        .map(|_| self.service.active_bank(&device.key())),
                })
            })
            .collect();
        Value::Array(devices)
    }

    fn profiles(&self) -> Value {
        let document = self.service.document();
        let active = self.service.active_profile().id;
        let mut ordered: Vec<&crate::profile::PeripheralProfile> =
            document.profiles.iter().collect();
        ordered.sort_by_key(|profile| profile.order);
        Value::Array(
            ordered
                .iter()
                .map(|profile| {
                    json!({
                        "id": profile.id.to_string(),
                        "name": profile.name,
                        "isDefault": profile.is_default,
                        "active": profile.id == active,
                    })
                })
                .collect(),
        )
    }

    fn select_profile(&self, id: Id, lock_profile: bool) -> Result<(), String> {
        // Checked first, so an unknown id is a clear error rather than a silent no-op that leaves
        // the caller believing it switched.
        if !self.service.document().profiles.iter().any(|profile| profile.id == id) {
            return Err("Unknown profile id".to_owned());
        }
        if lock_profile {
            self.service.set_lock(Some(id)).map_err(|e| e.to_string())
        } else {
            // `persistent: true`: an API switch outlives the next foreground change, which is
            // what a script asking for a profile means.
            self.service.select_profile(id, true).map_err(|e| e.to_string())
        }
    }

    fn unlock(&self) -> Result<(), String> {
        self.service.set_lock(None).map_err(|e| e.to_string())
    }

    fn run_macro(&self, id: Id) -> bool {
        let Some(definition) = self.service.document().macro_by_id(id).cloned() else {
            return false;
        };
        let runtime = Arc::clone(self.service.runtime());
        // Spawned, because a macro can run for as long as its author wants and the HTTP handler
        // must answer now.
        let _ = std::thread::Builder::new()
            .name("ghub automation macro".to_owned())
            .spawn(move || {
                runtime.play(definition);
            });
        true
    }

    fn set_dpi(&self, device_key: Option<&str>, dpi: u32) -> Option<u32> {
        let manager = self.service.devices();
        // Without a key, the first connected mouse that actually exposes DPI - not merely the
        // first device, which on this hardware is usually the keyboard.
        let device = match device_key {
            Some(key) => manager.get(key)?,
            None => manager.devices().into_iter().find(|device| {
                device.connected
                    && device.capabilities.dpi.as_ref().is_some_and(|dpi| dpi.meta.is_usable())
            })?,
        };
        let capability = device.capabilities.dpi.clone()?;
        manager
            .with_session(&device.key(), |session| {
                session.dpi().and_then(|feature| feature.set(capability.clamp(dpi)).ok())
            })
            .flatten()
    }

    fn set_bank(&self, device_key: &str, bank: usize) -> Result<(), String> {
        self.service.set_bank(device_key, bank);
        Ok(())
    }

    fn set_static_color(&self, device_key: &str, hex: &str) -> bool {
        let Some(color) = parse_color(hex) else { return false };
        // Ownership first: if another writer owns this device's lighting, the API must not fight
        // it. Returning false is what tells the caller the colour was not applied.
        if !self.service.ownership().request(device_key, &native_writer(), ALL_ZONES) {
            return false;
        }

        let manager = self.service.devices();
        manager
            .with_session(device_key, |session| {
                if !session.is_connected() {
                    return false;
                }
                if session.per_key_lighting().is_some() {
                    if session.acquire_per_key_lighting().is_err() {
                        return false;
                    }
                    let frame: HashMap<u8, ghub_hidpp::LedColor> =
                        layout::led_ids().into_iter().map(|id| (id, color)).collect();
                    return session.write_per_key_frame(&frame).is_ok();
                }
                if let Some(rgb) = session.rgb() {
                    let zones: Vec<usize> = rgb.zones.iter().map(|zone| zone.index).collect();
                    if zones.is_empty() {
                        return false;
                    }
                    return zones
                        .iter()
                        .all(|index| rgb.set_static_color(*index, color).is_ok());
                }
                false
            })
            .unwrap_or(false)
    }

    fn stop_all_macros(&self) {
        self.service.runtime().stop_all("automation API");
    }
}

/// The same writer identity the service itself uses for its native HID++ writes, so an API colour
/// and a profile colour contend for ownership rather than both appearing to succeed.
fn native_writer() -> crate::ownership::LightingWriter {
    crate::ownership::LightingWriter::new(
        "native-hidpp",
        "Native Logitech HID++ (profile static/effect)",
        20,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The API's writer identity has to be byte-for-byte the service's own, or an API write would
    /// claim a second, separate ownership of the same device and the two would overwrite each
    /// other without either noticing.
    #[test]
    fn the_api_writes_as_the_same_writer_the_service_does() {
        let writer = native_writer();
        assert_eq!(writer.id, "native-hidpp");
        assert_eq!(writer.priority, 20);
    }

    /// Every lit key, so a whole-keyboard colour really is whole: the 0x8081 frame is validated as
    /// a unit and a missing id would be a gap in the colour.
    #[test]
    fn a_static_colour_frame_covers_every_addressable_led() {
        let ids = layout::led_ids();
        assert!(!ids.is_empty());
        let frame: HashMap<u8, ghub_hidpp::LedColor> = ids
            .iter()
            .map(|id| (*id, ghub_hidpp::LedColor { r: 1, g: 2, b: 3 }))
            .collect();
        assert_eq!(frame.len(), ids.len(), "an id appeared twice");
        assert!(!frame.contains_key(&0), "0 means 'not individually lit'");
    }
}
