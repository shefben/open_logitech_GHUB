//! State and helpers every page shares. Port of
//! `Artemis.UI/Screens/Peripherals/Tabs/PeripheralTabBase.cs`.
//!
//! The C# got this by inheritance: each tab extended `PeripheralTabBase`, which held the service,
//! the status and error banners, the busy flag, and the two work helpers. Rust has no
//! implementation inheritance, and faking it with a trait would mean every page method taking
//! `&mut self` *and* `&Shared` while the borrow checker objects to the page living inside the
//! struct it borrows. So the shape is inverted: [`Shared`] is a sibling of the pages, and each
//! page's `show` takes `&mut Shared`.
//!
//! The behaviour carried over exactly:
//!
//! * **Everything that touches a device runs off the UI thread.** [`Shared::run`] is `Run`, and a
//!   page never calls a device method directly.
//! * **An edit to the active profile is read-modify-write under the service's own lock**, by id
//!   rather than by reference, so an edit cannot be applied to a profile that stopped being active
//!   while the job was queued. That is what `EditActiveProfile` did, and [`Shared::edit_profile`]
//!   does the same for the same reason.
//! * **Refresh is driven by events, not polling.** The C# subscribed to `DeviceConnected`,
//!   `DeviceDisconnected`, `DeviceUpdated`, `DocumentChanged` and `StatusMessage`. Here the host
//!   collects those into a queue and [`Shared::pump`] drains it once per frame, returning what
//!   changed so pages reload only when they must.

use std::sync::Arc;

use ghub_core::manager::ManagedDevice;
use ghub_core::profile::{PeripheralProfile, ProfileDocument};
use ghub_core::service::{MacroRecordState, PeripheralService, ServiceEvent};
use ghub_core::store::StoreError;
use ghub_hidpp::identity::PeripheralKind;

use crate::artwork::ArtworkCache;
use crate::host::{Host, JobError, Outcome};

/// What changed since the last frame, so a page can reload the minimum.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Changes {
    /// The document was edited, or the active profile changed. Pages reload their settings.
    pub document: bool,
    /// A device connected, disconnected, or had its capabilities re-read. Pages reload their
    /// device list and anything derived from capabilities.
    pub devices: bool,
    /// The macro recorder changed state, which the keyboard page's record banner shows.
    pub recording: bool,
}

impl Changes {
    /// Whether anything moved. Used by this module's tests to pin what `all` and the default
    /// mean; the pages each look at the one flag they care about rather than asking this.
    #[cfg(test)]
    pub fn any(self) -> bool {
        self.document || self.devices || self.recording
    }

    /// Everything, used for a page's first load.
    pub fn all() -> Self {
        Self { document: true, devices: true, recording: true }
    }
}

pub struct Shared {
    pub host: Host,
    pub artwork: ArtworkCache,
    /// `PeripheralTabBase.Status` - the last thing that happened, in the page's footer.
    pub status: Option<String>,
    /// `PeripheralTabBase.Error` - a failure, shown in red until the next successful action.
    pub error: Option<String>,
    /// A physical button the 0x8110 spy saw, for the mouse page's "press the button to identify
    /// it" flow. Set from the service event, consumed by the page.
    pub spy_button: Option<(String, usize)>,
    /// `ConflictWarning`. Computed once at start-up and on each device change rather than every
    /// frame: walking the process list is a syscall per process, which is not something to do at
    /// 60 Hz.
    conflict: String,
}

impl Shared {
    pub fn new(host: Host) -> Self {
        Self {
            host,
            artwork: ArtworkCache::new(),
            status: None,
            error: None,
            spy_button: None,
            conflict: ghub_platform::coexistence::conflict_warning(),
        }
    }

    // ------------------------------------------------------------- accessors

    pub fn service(&self) -> &Arc<PeripheralService> {
        &self.host.service
    }

    /// `PeripheralTabBase.Doc`.
    pub fn document(&self) -> Arc<ProfileDocument> {
        self.host.service.document()
    }

    /// `PeripheralTabBase.ActiveProfile`.
    pub fn active_profile(&self) -> PeripheralProfile {
        self.host.service.active_profile()
    }

    /// `PeripheralTabBase.DevicesOfKind`, including its ordering by display name.
    pub fn devices_of_kind(&self, kind: PeripheralKind) -> Vec<ManagedDevice> {
        let mut devices: Vec<ManagedDevice> = self
            .host
            .devices
            .devices()
            .into_iter()
            .filter(|d| d.descriptor.as_ref().is_some_and(|desc| desc.kind == kind))
            .collect();
        devices.sort_by_key(ManagedDevice::display_name);
        devices
    }

    pub fn device(&self, key: &str) -> Option<ManagedDevice> {
        self.host.devices.get(key)
    }

    /// `PeripheralTabBase.ConflictWarning`.
    pub fn conflict_warning(&self) -> &str {
        &self.conflict
    }

    pub fn has_conflict(&self) -> bool {
        !self.conflict.is_empty()
    }

    /// `PeripheralTabBase.Busy`.
    pub fn busy(&self) -> bool {
        self.host.worker.busy()
    }

    pub fn record_state(&self) -> MacroRecordState {
        self.host.service.record_state()
    }

    // ------------------------------------------------------------- work

    /// `PeripheralTabBase.Run`: device or profile work off the UI thread, reporting success or the
    /// exact failure.
    pub fn run<F>(&mut self, what: impl Into<String>, action: F)
    where
        F: FnOnce() -> Result<Option<String>, JobError> + Send + 'static,
    {
        // `Error = null` before the attempt, so a retry that works clears the red banner. Without
        // this a transient failure stays on screen behind a successful result.
        self.error = None;
        self.host.worker.spawn(what, action);
    }

    /// Same, for the common case of an action that either works or returns a message.
    pub fn attempt<F>(&mut self, what: impl Into<String>, action: F)
    where
        F: FnOnce() -> Result<String, String> + Send + 'static,
    {
        self.run(what, move || action().map(Some).map_err(JobError::Failed));
    }

    /// `PeripheralTabBase.EditActiveProfile`: an atomic edit to the active profile, saved and
    /// re-applied to the devices.
    ///
    /// The profile is found by id inside the job rather than captured, which is what makes the
    /// edit safe to queue: by the time it runs the user may have switched profiles, and silently
    /// writing the edit into whatever is active *then* would move a key binding to the wrong
    /// profile.
    pub fn edit_profile<F>(&mut self, what: impl Into<String>, edit: F)
    where
        F: FnOnce(&mut PeripheralProfile) + Send + 'static,
    {
        let what = what.into();
        let service = Arc::clone(&self.host.service);
        let id = service.active_profile().id;
        self.run(what, move || {
            let mut found = false;
            service
                .update_document(|document| {
                    if let Some(profile) = document.profile_mut(id) {
                        edit(profile);
                        profile.touch();
                        found = true;
                    }
                })
                .map_err(store_error)?;
            if !found {
                // The profile was deleted between the click and the job. Reporting it is the
                // honest outcome; the C# would have thrown `InvalidOperationException` out of
                // `First` and shown "failed: Sequence contains no matching element", which told
                // the user nothing.
                return Err(JobError::Failed(
                    "the profile was deleted before the edit could be saved".to_owned(),
                ));
            }
            Ok(Some(format!("saved to profile '{}'", service.active_profile().name)))
        });
    }

    /// A document-wide edit (macros, triggers, app settings, the profile list itself).
    pub fn edit_document<F>(&mut self, what: impl Into<String>, edit: F)
    where
        F: FnOnce(&mut ProfileDocument) + Send + 'static,
    {
        let service = Arc::clone(&self.host.service);
        self.run(what, move || {
            service.update_document(edit).map_err(store_error)?;
            Ok(None)
        });
    }

    // ------------------------------------------------------------- frame

    /// Drains the worker and the service event queue. Call once per frame, before drawing.
    ///
    /// Returns what changed so each page can reload only what it needs; a page that reloaded its
    /// whole state every frame would fight the user's own edits, because a half-typed profile name
    /// would be overwritten by the saved one between keystrokes.
    pub fn pump(&mut self) -> Changes {
        for outcome in self.host.worker.drain() {
            match outcome {
                Outcome::Status(message) => {
                    self.status = Some(message);
                    self.error = None;
                }
                Outcome::Error(message) => self.error = Some(message),
            }
        }

        let mut changes = Changes::default();
        let mut devices_changed = false;
        for event in self.host.drain_events() {
            match event {
                ServiceEvent::DocumentChanged | ServiceEvent::ActiveProfileChanged(_) => {
                    changes.document = true;
                }
                ServiceEvent::Status(message) => self.status = Some(message),
                ServiceEvent::RecordStateChanged => changes.recording = true,
                ServiceEvent::SpyButtonSeen { device_key, button, pressed } => {
                    // Only the press, not the release: the identify flow wants one event per
                    // physical click.
                    if pressed {
                        self.spy_button = Some((device_key, button));
                    }
                }
                ServiceEvent::Notification { title, message } => {
                    self.status = Some(format!("{title}: {message}"));
                }
            }
        }

        // The device manager's changes do not come through the service's event list, so they are
        // read from the manager's own listener queue by the host. A device change can alter the
        // conflict list too - plugging a keyboard in is often what makes G HUB start.
        if self.host.take_device_change() {
            changes.devices = true;
            devices_changed = true;
        }
        if devices_changed {
            self.conflict = ghub_platform::coexistence::conflict_warning();
        }
        changes
    }
}

/// Maps a store failure onto the banner wording the C# used for it.
fn store_error(e: StoreError) -> JobError {
    match e {
        StoreError::Validation(errors) => JobError::Validation(errors.join("; ")),
        other => JobError::Failed(other.to_string()),
    }
}

/// `PeripheralsViewModel`'s unavailable text, kept here so the pages that show it agree.
pub const UNAVAILABLE_REASON: &str =
    "The peripheral manager is not running (Windows only; see Diagnostics).";


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_validation_failure_lists_the_rules_it_broke() {
        let e = StoreError::Validation(vec!["needs a name".to_owned(), "needs a device".to_owned()]);
        assert_eq!(
            store_error(e),
            JobError::Validation("needs a name; needs a device".to_owned())
        );
    }

    #[test]
    fn any_other_failure_is_reported_as_the_operation_breaking() {
        let e = StoreError::Internal("the lock was poisoned".to_owned());
        assert_eq!(store_error(e), JobError::Failed("the lock was poisoned".to_owned()));
    }

    #[test]
    fn changes_are_empty_until_something_happens() {
        assert!(!Changes::default().any());
        assert!(Changes::all().any());
    }
}
