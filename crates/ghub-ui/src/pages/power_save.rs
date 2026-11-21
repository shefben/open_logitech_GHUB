//! The lighting power-saving editor. Port of
//! `Artemis.UI/Screens/Peripherals/Tabs/PowerSaveViewModel.cs`.
//!
//! Two firmware timers behind 0x8071: dim after N seconds idle, off after M. They are *volatile* -
//! the device forgets them when it loses power - so the profile carries them and the service
//! re-applies on connect. That is why the panel shows two numbers at once: what the profile will
//! write, and (after "Read device") what the device is actually using. Those disagree more often
//! than one would like, and a single field would hide it.

use std::sync::{Arc, Mutex};

use ghub_core::manager::DeviceManager;
use ghub_core::profile::LightingSettings;

use crate::controls as c;
use crate::icons::IconCache;
use crate::widgets as w;

/// `PowerSaveViewModel`. Shared by the mouse and keyboard pages, which is why it lives here rather
/// than inside either one.
pub struct PowerSave {
    pub enabled: bool,
    pub dim_seconds: i64,
    pub off_seconds: i64,
    /// What "Read device" last reported, or empty. Never merged into the editable values: the
    /// device's own timers are evidence, not a default.
    ///
    /// Shared because the read happens on the worker thread: [`read_device`] writes here and the
    /// next frame shows it. The alternative - routing it back through the job's status string -
    /// would put the device's timers in the status band but leave the label beside the button
    /// permanently empty.
    device_state: Arc<Mutex<String>>,
    /// False when the device exposes no 0x8071, which hides the whole block rather than offering a
    /// control that cannot do anything.
    pub supported: bool,
    dim_buffer: String,
    off_buffer: String,
}

impl Default for PowerSave {
    fn default() -> Self {
        // `_dimSeconds = 60`, `_offSeconds = 300`.
        Self {
            enabled: false,
            dim_seconds: 60,
            off_seconds: 300,
            device_state: Arc::new(Mutex::new(String::new())),
            supported: false,
            dim_buffer: "60".to_owned(),
            off_buffer: "300".to_owned(),
        }
    }
}

impl PowerSave {
    /// `Load(LightingSettings, ManagedDevice?)`.
    ///
    /// `enabled` is derived from the settings rather than stored: a profile with neither timer set
    /// is a profile with power saving off, and keeping a separate flag would let the two disagree.
    /// `supported` comes from [`supports_power_timers`] rather than being discovered here, so the
    /// loader stays a pure function of the settings and can be tested without a device.
    pub fn load(&mut self, lighting: &LightingSettings, supported: bool) {
        self.enabled =
            lighting.power_save_idle_seconds.is_some() || lighting.power_save_sleep_seconds.is_some();
        self.dim_seconds = lighting.power_save_idle_seconds.unwrap_or(60) as i64;
        self.off_seconds = lighting.power_save_sleep_seconds.unwrap_or(300) as i64;
        self.dim_buffer = self.dim_seconds.to_string();
        self.off_buffer = self.off_seconds.to_string();
        self.supported = supported;
        self.set_device_state(String::new());
    }

    /// The cell [`read_device`] writes into, to be moved into the job.
    pub fn state_cell(&self) -> Arc<Mutex<String>> {
        Arc::clone(&self.device_state)
    }

    /// What the device last reported, or an empty string.
    pub fn device_state(&self) -> String {
        // A poisoned lock means a panic in a job; showing nothing is better than propagating it
        // into the frame loop and taking the window down.
        self.device_state.lock().map(|state| state.clone()).unwrap_or_default()
    }

    fn set_device_state(&self, state: String) {
        if let Ok(mut slot) = self.device_state.lock() {
            *slot = state;
        }
    }

    /// The editable values, detached from the editor.
    ///
    /// A lighting save is queued and runs later, so it must write what was on screen when the
    /// button was pressed rather than reading the editor again from the worker thread.
    pub fn snapshot(&self) -> PowerSaveSnapshot {
        PowerSaveSnapshot {
            enabled: self.enabled,
            dim_seconds: self.dim_seconds,
            off_seconds: self.off_seconds,
        }
    }

}

/// [`PowerSave`]'s two values with none of its editing state, so it can be moved into a job.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PowerSaveSnapshot {
    pub enabled: bool,
    pub dim_seconds: i64,
    pub off_seconds: i64,
}

impl PowerSaveSnapshot {
    pub fn store(&self, lighting: &mut LightingSettings) {
        if self.enabled {
            lighting.power_save_idle_seconds =
                Some(self.dim_seconds.clamp(0, u16::MAX as i64) as u16);
            lighting.power_save_sleep_seconds =
                Some(self.off_seconds.clamp(0, u16::MAX as i64) as u16);
        } else {
            // Zero would be a request to dim immediately; absence is the absence of a request.
            lighting.power_save_idle_seconds = None;
            lighting.power_save_sleep_seconds = None;
        }
    }
}

impl PowerSave {

    /// Draws the block. Returns true when an edit happened, so the caller can persist.
    ///
    /// The whole thing is hidden when unsupported, matching `IsVisible="{Binding
    /// PowerSave.Supported}"`.
    pub fn show(&mut self, ui: &mut egui::Ui, icons: &mut IconCache, enabled: bool) -> bool {
        if !self.supported {
            return false;
        }

        let mut changed = false;
        changed |= c::checkbox(ui, icons, &mut self.enabled, "Lighting power saving", enabled);
        ui.add_space(4.0);

        // `IsEnabled="{Binding PowerSave.Enabled}"` on both rows.
        let rows_enabled = enabled && self.enabled;
        for (caption, value, buffer) in [
            ("Dim after s", &mut self.dim_seconds, &mut self.dim_buffer),
            ("Off after s", &mut self.off_seconds, &mut self.off_buffer),
        ] {
            changed |= c::field_row(ui, caption, |ui| {
                // `Minimum="0" Maximum="65535" Increment="15"`.
                c::numeric(ui, icons, buffer, value, 0..=65535, 15, rows_enabled)
            });
        }
        changed
    }

    /// Draws the "Read device" button and whatever it last said. Separate from [`Self::show`]
    /// because reading needs the host and the rest does not.
    pub fn show_read(&self, ui: &mut egui::Ui, enabled: bool) -> bool {
        if !self.supported {
            return false;
        }
        let mut read = false;
        let state = self.device_state();
        ui.horizontal(|ui| {
            if c::button(ui, "Read device", c::ButtonStyle::Outlined, enabled).clicked() {
                read = true;
            }
            if !state.is_empty() {
                ui.add_space(6.0);
                w::body(ui, &state);
            }
        });
        read
    }
}

/// `device?.Session?.GetFeature<HidppRgbEffectsFeature>() != null`.
///
/// Asks the session rather than the cached capability record, because 0x8071 is what carries the
/// timers and a device can advertise lighting without it.
pub fn supports_power_timers(devices: &DeviceManager, device_key: Option<&str>) -> bool {
    device_key
        .and_then(|key| devices.with_session(key, |session| session.rgb().is_some()))
        .unwrap_or(false)
}

/// `ReadAsync`: reads the timers the device is using right now.
///
/// Read-only, and the only device call this module makes. On success the line is both stored in
/// `state` - where [`PowerSave::show_read`] shows it - and returned, so it also lands in the page's
/// status band the way `Run`'s result did.
pub fn read_device(
    devices: &DeviceManager,
    device_key: &str,
    state: &Mutex<String>,
) -> Result<String, String> {
    let read = devices.with_session(device_key, |session| match session.rgb() {
        None => Err("Device has no lighting power timers".to_owned()),
        Some(rgb) => rgb
            .get_power_timers()
            .map(|t| {
                format!(
                    "Device now: dim after {}s, off after {}s",
                    t.idle_seconds, t.sleep_seconds
                )
            })
            .map_err(|e| e.to_string()),
    });
    let result = match read {
        Some(result) => result,
        None => Err("Device has no lighting power timers".to_owned()),
    };
    if let (Ok(line), Ok(mut slot)) = (&result, state.lock()) {
        *slot = line.clone();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_saving_is_on_when_either_timer_is_set() {
        let mut lighting = LightingSettings::default();
        let mut power = PowerSave::default();
        power.load(&lighting, false);
        assert!(!power.enabled, "no timers means power saving is off");

        lighting.power_save_sleep_seconds = Some(90);
        power.load(&lighting, false);
        assert!(power.enabled, "one timer is enough to mean it is on");
        assert_eq!(power.off_seconds, 90);
        // The other keeps its default rather than becoming zero, which would turn the display off
        // the moment the user ticked the box.
        assert_eq!(power.dim_seconds, 60);
    }

    #[test]
    fn switching_it_off_clears_both_timers_rather_than_zeroing_them() {
        let mut lighting = LightingSettings::default();
        let mut power = PowerSave { enabled: true, ..PowerSave::default() };
        power.snapshot().store(&mut lighting);
        assert_eq!(lighting.power_save_idle_seconds, Some(60));
        assert_eq!(lighting.power_save_sleep_seconds, Some(300));

        power.enabled = false;
        power.snapshot().store(&mut lighting);
        // Zero would be a request to dim immediately. None is the absence of a request.
        assert_eq!(lighting.power_save_idle_seconds, None);
        assert_eq!(lighting.power_save_sleep_seconds, None);
    }

    #[test]
    fn the_timers_saturate_at_what_the_wire_can_carry() {
        let mut lighting = LightingSettings::default();
        let power = PowerSave {
            enabled: true,
            dim_seconds: 999_999,
            off_seconds: -5,
            ..PowerSave::default()
        };
        power.snapshot().store(&mut lighting);
        assert_eq!(lighting.power_save_idle_seconds, Some(u16::MAX));
        assert_eq!(lighting.power_save_sleep_seconds, Some(0));
    }
}
