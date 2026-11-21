//! The orchestrator: the port of `PeripheralService.cs`.
//!
//! One profile store, one action runtime, one device manager. Applies the active profile to every
//! connected device, routes device and hook events to bindings, and implements the bank, G-Shift,
//! DPI and on-the-fly macro recording workflows.
//!
//! Everything else in this crate is a pure component with a narrow job; this is where they meet,
//! so it is also where the invariants that span them are enforced:
//!
//! * **Copy on write for the document.** The published [`ProfileDocument`] is never mutated in
//!   place. Every read-modify-save clones, mutates the clone, validates and saves it, and only
//!   then republishes. Concurrent edits from the UI, from device threads (bank, macro record) and
//!   from macro actions therefore cannot lose each other's changes, and a validation failure
//!   leaves the live document untouched.
//! * **No stuck keys across a profile change.** Switching profile stops every running macro and
//!   releases what it held, because the key-up for a binding that no longer exists will never
//!   arrive.
//! * **Read back every write.** Device settings are applied by writing and then reporting what
//!   the device says it now holds, never the value that was asked for. The G903's sensor rounds
//!   DPI; a UI showing the requested number would be lying.
//! * **Leave the hardware usable without this program.** On shutdown, G-keys go back to firmware
//!   control, the mouse's runtime button mapping is restored, and per-key lighting is handed back
//!   to the onboard effect. A peripheral manager that bricks your keyboard's extra keys when it
//!   exits is worse than no manager.
//! * **Never touch what the profile does not manage.** The spy-button mapping is left exactly as
//!   it was - possibly G HUB's - unless this profile actually rebinds one of those buttons.
//!
//! ## Shape differences from the C#
//!
//! The C# was `async` throughout, with a `SemaphoreSlim` per concern. Here the work is blocking
//! and the locks are plain mutexes, in a fixed order: `document` before `apply` before any
//! session lock. Nothing in this file takes a session lock while holding `document`, which is
//! what keeps a device that has stopped answering from freezing the UI's next edit.
//!
//! The C#'s `Try` helper returned `"what: FAILED (message)"` strings for the UI's result list.
//! That is kept verbatim, because those strings are what the device page shows, and a user
//! reading "DPI 1600: FAILED (feature 0x2201 returned 0x05)" can act on it.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ghub_hidpp::capability::DeviceCapabilities;
use ghub_hidpp::color::LedColor;
use ghub_hidpp::identity::{DeviceEvent, PeripheralKind};

use crate::action::{
    ActionBinding, ActionDefinition, DpiCommand, LightingCommand, MacroDefinition, MacroStep,
    MouseButtonKind, ProfileCommand,
};
use crate::animation::{AnimationEngine, FrameSink};
use crate::diagnostics::DiagnosticsLog;
use crate::effects;
use crate::id::Id;
use crate::injection::InputInjector;
use crate::layout;
use crate::macros::{ActionHost, MacroRuntime};
use crate::manager::{DeviceChange, DeviceChangeKind, DeviceManager, ManagedDevice};
use crate::ownership::{LightingOwnership, LightingWriter, ALL_ZONES};
use crate::platform::{HookSink, HookStatus, PlatformServices};
use crate::profile::{
    parse_color, AppLightingOverride, AppLightingRule, DeviceProfileSettings, LightingMode,
    LightingSettings, MouseSettings, PeripheralProfile, ProfileDocument, TriggerEvent,
};
use crate::reactions::{Frame, ReactionEngine};
use crate::recorder::MacroRecorder;
use crate::resolver::{self, ForegroundApp, ProfileResolution};
use crate::store::{ProfileStore, StoreError};
use crate::triggers::{TriggerEngine, TriggerOccurrence};

/// First button number handled by the 0x8110 spy.
///
/// Buttons 1-5 reach Windows as standard mouse buttons and are handled by the software hook
/// instead; diverting them through the spy would mean the mouse stops clicking the moment this
/// program stops answering.
pub const FIRST_SPY_BUTTON: usize = 6;

/// Undo depth. Fifty edits is far more than a session's worth of profile tweaking, and the cost
/// is fifty serialised documents in memory.
pub const HISTORY_LIMIT: usize = 50;

fn native_writer() -> LightingWriter {
    LightingWriter::new(
        "native-hidpp",
        "Native Logitech HID++ (profile static/effect)",
        20,
    )
}

/// The RGB.NET bridge the Artemis host used. Kept as a writer identity so a profile saved while
/// Artemis owned a device still resolves its ownership record on import.
fn artemis_writer() -> LightingWriter {
    LightingWriter::new("artemis", "Artemis lighting (RGB.NET)", 15)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MacroRecordState {
    #[default]
    Idle,
    /// MR pressed: waiting for the target G-key.
    SelectTarget,
    /// Capturing keystrokes for [`PeripheralService::recording_target`].
    Recording,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingTarget {
    pub device_key: String,
    pub gkey: usize,
    pub bank: usize,
}

/// Something the UI should show or redraw.
#[derive(Debug, Clone)]
pub enum ServiceEvent {
    DocumentChanged,
    ActiveProfileChanged(ProfileResolution),
    Status(String),
    RecordStateChanged,
    /// A physical button seen by the 0x8110 spy, for the "press the button to identify it" flow.
    SpyButtonSeen { device_key: String, button: usize, pressed: bool },
    /// A desktop notification the host should raise.
    Notification { title: String, message: String },
}

type EventListener = Box<dyn Fn(&ServiceEvent) + Send + Sync + 'static>;

/// Writes animation frames straight to the device that owns them.
struct DeviceFrameSink {
    manager: Arc<DeviceManager>,
    ownership: Arc<LightingOwnership>,
    log: Arc<DiagnosticsLog>,
}

impl FrameSink for DeviceFrameSink {
    fn write_frame(&self, device_key: &str, frame: &Frame) -> bool {
        if !self.ownership.can_write(device_key, &native_writer(), ALL_ZONES) {
            // Someone else took the device mid-animation. Stopping the loop is the whole point of
            // the bool return: it does not fight the new owner frame by frame.
            return false;
        }
        match self.manager.with_session(device_key, |session| {
            session.write_per_key_frame(&frame.clone())
        }) {
            Some(Ok(())) => true,
            Some(Err(e)) => {
                // One failed frame is not fatal - a wireless keyboard drops reports - but a
                // disconnected device must stop the loop rather than log once per frame.
                if e.is_disconnected() {
                    self.log.info("Lighting", "Animation stopped: device disconnected");
                    return false;
                }
                self.log.warning_with("Lighting", "Animation frame failed", e.to_string());
                true
            }
            None => false,
        }
    }
}

pub struct PeripheralService {
    store: ProfileStore,
    manager: Arc<DeviceManager>,
    platform: Arc<dyn PlatformServices>,
    log: Arc<DiagnosticsLog>,
    ownership: Arc<LightingOwnership>,
    runtime: Arc<MacroRuntime>,
    recorder: Mutex<MacroRecorder>,
    reactions: Arc<Mutex<ReactionEngine>>,
    animations: Arc<AnimationEngine>,
    triggers: Mutex<Option<Arc<TriggerEngine>>>,

    /// Published document. Replaced wholesale, never mutated in place.
    document: Mutex<Arc<ProfileDocument>>,
    /// Serialises read-modify-save. Separate from `document` so a save (which touches the disk)
    /// does not hold the lock that every UI read wants.
    edit_lock: Mutex<()>,
    /// Serialises applying a profile to devices, so two triggers cannot interleave writes.
    apply_lock: Mutex<()>,
    active: Mutex<Option<ProfileResolution>>,

    undo: Mutex<Vec<String>>,
    redo: Mutex<Vec<String>>,

    gshift: AtomicBool,
    /// DPI stage chosen by a binding, overriding the profile's stored stage until the profile
    /// changes. Not persisted: a DPI button press is a transient choice.
    dpi_stage_override: Mutex<HashMap<String, usize>>,
    dpi_shift_held: Mutex<HashSet<String>>,
    /// Devices whose G-keys this program has diverted to software control. Tracked so they can be
    /// handed back on shutdown and when a profile stops managing the keyboard.
    diverted_gkeys: Mutex<HashSet<String>>,
    /// Spy buttons (1-based) muted per device; their actions come from 0x8110 events.
    spy_muted: Mutex<HashMap<String, Vec<usize>>>,
    /// Mice this program switched from onboard to host mode, so the switch can be undone when
    /// the hardware is handed back.
    host_mode_set: Mutex<HashSet<String>>,

    record_state: Mutex<MacroRecordState>,
    recording_target: Mutex<Option<RecordingTarget>>,

    /// Foreground program the lighting rules were last evaluated against.
    foreground: Mutex<Option<ForegroundApp>>,
    last_foreground: Mutex<String>,
    /// Which per-application lighting rule each device last matched, so a change re-applies.
    last_lighting_rules: Mutex<String>,
    saw_keyboard_hook: AtomicBool,
    hook_status: Mutex<HookStatus>,

    listeners: Mutex<Vec<EventListener>>,
}

impl std::fmt::Debug for PeripheralService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeripheralService")
            .field("active", &self.active_profile().name)
            .field("record_state", &self.record_state())
            .finish_non_exhaustive()
    }
}

impl PeripheralService {
    /// Loads the document and wires the components together.
    ///
    /// Loading is infallible by design: a corrupt file is quarantined and the newest readable
    /// backup is used, and the notes describing what happened are logged rather than swallowed.
    /// Failing to start because of a bad JSON file is not an option a peripheral manager has.
    pub fn new(
        store: ProfileStore,
        manager: Arc<DeviceManager>,
        injector: Arc<dyn InputInjector>,
        platform: Arc<dyn PlatformServices>,
        log: Arc<DiagnosticsLog>,
        ownership: Arc<LightingOwnership>,
    ) -> Arc<Self> {
        let (document, notes) = store.load();
        for note in &notes {
            log.warning("Profiles", note.clone());
        }
        let active = resolver::resolve(&document, None);
        let reactions = Arc::new(Mutex::new(ReactionEngine::new()));
        let sink = Arc::new(DeviceFrameSink {
            manager: Arc::clone(&manager),
            ownership: Arc::clone(&ownership),
            log: Arc::clone(&log),
        });
        let animations = Arc::new(AnimationEngine::new(sink, Arc::clone(&reactions)));

        let service = Arc::new_cyclic(|weak: &std::sync::Weak<Self>| {
            let host: Arc<dyn ActionHost> = Arc::new(ServiceHost { service: weak.clone() });
            Self {
                store,
                manager: Arc::clone(&manager),
                platform,
                log: Arc::clone(&log),
                ownership,
                runtime: Arc::new(MacroRuntime::new(injector, host)),
                recorder: Mutex::new(MacroRecorder::new()),
                reactions,
                animations,
                triggers: Mutex::new(None),
                document: Mutex::new(Arc::new(document)),
                edit_lock: Mutex::new(()),
                apply_lock: Mutex::new(()),
                active: Mutex::new(active),
                undo: Mutex::new(Vec::new()),
                redo: Mutex::new(Vec::new()),
                gshift: AtomicBool::new(false),
                dpi_stage_override: Mutex::new(HashMap::new()),
                dpi_shift_held: Mutex::new(HashSet::new()),
                diverted_gkeys: Mutex::new(HashSet::new()),
                spy_muted: Mutex::new(HashMap::new()),
                host_mode_set: Mutex::new(HashSet::new()),
                record_state: Mutex::new(MacroRecordState::Idle),
                recording_target: Mutex::new(None),
                foreground: Mutex::new(None),
                last_foreground: Mutex::new(String::new()),
                last_lighting_rules: Mutex::new(String::new()),
                saw_keyboard_hook: AtomicBool::new(false),
                hook_status: Mutex::new(HookStatus::default()),
                listeners: Mutex::new(Vec::new()),
            }
        });

        // The trigger engine executes actions through the runtime, so it can only be built once
        // the service exists. Its rules are read live from the document, which is what lets an
        // edited trigger take effect without rebuilding the engine.
        let for_rules = Arc::downgrade(&service);
        let for_exec = Arc::downgrade(&service);
        let engine = Arc::new(TriggerEngine::new(
            move || {
                for_rules
                    .upgrade()
                    .map(|s| s.document().triggers.clone())
                    .unwrap_or_default()
            },
            move |action| {
                let Some(service) = for_exec.upgrade() else {
                    return Err("the service is shutting down".to_owned());
                };
                service.runtime.on_trigger("trigger", Some(action), true);
                service.runtime.on_trigger("trigger", None, false);
                Ok(())
            },
            Arc::clone(&log),
        ));
        if let Ok(mut slot) = service.triggers.lock() {
            *slot = Some(engine);
        }

        let for_devices = Arc::downgrade(&service);
        manager.on_change(move |change| {
            if let Some(service) = for_devices.upgrade() {
                service.on_device_change(change);
            }
        });
        let for_events = Arc::downgrade(&service);
        manager.on_event(move |device, event| {
            if let Some(service) = for_events.upgrade() {
                service.on_device_event(device, event);
            }
        });

        service
    }

    // ------------------------------------------------------------- accessors

    pub fn store(&self) -> &ProfileStore {
        &self.store
    }

    pub fn devices(&self) -> &Arc<DeviceManager> {
        &self.manager
    }

    pub fn log(&self) -> &Arc<DiagnosticsLog> {
        &self.log
    }

    pub fn ownership(&self) -> &Arc<LightingOwnership> {
        &self.ownership
    }

    pub fn runtime(&self) -> &Arc<MacroRuntime> {
        &self.runtime
    }

    pub fn animations(&self) -> &Arc<AnimationEngine> {
        &self.animations
    }

    pub fn reactions(&self) -> &Arc<Mutex<ReactionEngine>> {
        &self.reactions
    }

    pub fn triggers(&self) -> Option<Arc<TriggerEngine>> {
        self.triggers.lock().ok()?.clone()
    }

    /// The live document. Cheap: an `Arc` clone, so the caller can read it without a lock.
    pub fn document(&self) -> Arc<ProfileDocument> {
        self.document
            .lock()
            .map(|d| Arc::clone(&d))
            .unwrap_or_else(|e| Arc::clone(&e.into_inner()))
    }

    /// How the active profile was chosen. `None` only before the first resolution, which the
    /// constructor performs, so in practice always `Some`.
    pub fn active(&self) -> Option<ProfileResolution> {
        self.active.lock().ok()?.clone()
    }

    /// The active profile itself, read live out of the document.
    ///
    /// The resolution records an id, not a copy, so an edit to the profile that is currently
    /// active takes effect without a second resolution pass. The two fallbacks exist because a
    /// locked or persisted id can name a profile that has since been deleted; validation
    /// guarantees at least one profile exists, so the last one is unreachable in practice and is
    /// there to keep this infallible.
    pub fn active_profile(&self) -> PeripheralProfile {
        let document = self.document();
        self.active()
            .and_then(|resolution| document.profile(resolution.profile_id).cloned())
            .or_else(|| document.default_profile().cloned())
            .unwrap_or_else(|| PeripheralProfile::new("Default"))
    }

    pub fn gshift_held(&self) -> bool {
        self.gshift.load(Ordering::SeqCst)
    }

    pub fn record_state(&self) -> MacroRecordState {
        self.record_state.lock().map(|s| *s).unwrap_or_default()
    }

    pub fn recording_target(&self) -> Option<RecordingTarget> {
        self.recording_target.lock().ok()?.clone()
    }

    pub fn hook_status(&self) -> HookStatus {
        self.hook_status.lock().map(|s| *s).unwrap_or_default()
    }

    pub fn on_event(&self, listener: impl Fn(&ServiceEvent) + Send + Sync + 'static) {
        if let Ok(mut listeners) = self.listeners.lock() {
            listeners.push(Box::new(listener));
        }
    }

    fn raise(&self, event: ServiceEvent) {
        let Ok(listeners) = self.listeners.lock() else { return };
        for listener in listeners.iter() {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(&event))).is_err() {
                self.log.error("Service", "An event subscriber panicked");
            }
        }
    }

    fn status(&self, message: impl Into<String>) {
        self.raise(ServiceEvent::Status(message.into()));
    }

    fn fire_trigger(&self, event: TriggerEvent, subject: impl Into<String>, value: i32) {
        if let Some(engine) = self.triggers() {
            engine.raise(&TriggerOccurrence::with_value(event, subject, value));
        }
    }

    // ------------------------------------------------------------- document edits

    /// Atomically replaces the document and re-applies the active profile.
    pub fn update_document(
        self: &Arc<Self>,
        mutate: impl FnOnce(&mut ProfileDocument),
    ) -> Result<(), StoreError> {
        self.mutate(mutate, true, true)
    }

    fn mutate(
        self: &Arc<Self>,
        mutate: impl FnOnce(&mut ProfileDocument),
        record_history: bool,
        reapply: bool,
    ) -> Result<(), StoreError> {
        {
            let _guard = self.edit_lock.lock().map_err(|_| {
                StoreError::Internal(
                    "the edit lock is poisoned; a previous save panicked".to_owned(),
                )
            })?;
            let before = self.document();
            // Round-tripped through JSON rather than cloned, so the undo entry is exactly what
            // would be written to disk and a redo cannot resurrect a field the schema dropped.
            let serialized = serde_json::to_string(&*before)
                .map_err(|e| StoreError::Json(e.to_string()))?;
            let mut copy = crate::store::parse(&serialized)?;
            mutate(&mut copy);
            // Saving validates. A rejected document leaves the live one untouched, which is why
            // the publish below comes after.
            self.store.save(&copy)?;
            if record_history {
                push_history(&self.undo, serialized);
                if let Ok(mut redo) = self.redo.lock() {
                    redo.clear();
                }
            }
            let published = Arc::new(copy);
            if let Ok(mut slot) = self.document.lock() {
                *slot = Arc::clone(&published);
            }
            // Nothing to refresh on the resolution: it holds an id, and `active_profile` reads
            // the profile out of the document that was just published.
            let _ = &published;
        }

        self.raise(ServiceEvent::DocumentChanged);
        if reapply {
            self.reevaluate(true, None);
        }
        Ok(())
    }

    pub fn can_undo(&self) -> bool {
        self.undo.lock().map(|u| !u.is_empty()).unwrap_or(false)
    }

    pub fn can_redo(&self) -> bool {
        self.redo.lock().map(|r| !r.is_empty()).unwrap_or(false)
    }

    /// Reverts the last profile or macro edit. Application state only: device flash is never
    /// touched by undo.
    pub fn undo(self: &Arc<Self>) -> bool {
        self.step_history(true)
    }

    pub fn redo(self: &Arc<Self>) -> bool {
        self.step_history(false)
    }

    fn step_history(self: &Arc<Self>, undoing: bool) -> bool {
        {
            let Ok(_guard) = self.edit_lock.lock() else { return false };
            let (from, to) = if undoing {
                (&self.undo, &self.redo)
            } else {
                (&self.redo, &self.undo)
            };
            let Some(entry) = from.lock().ok().and_then(|mut stack| stack.pop()) else {
                return false;
            };
            let Ok(target) = crate::store::parse(&entry) else {
                self.log.warning("Profiles", "A history entry could not be parsed; dropping it");
                return false;
            };
            if let Err(e) = self.store.save(&target) {
                self.log.warning_with("Profiles", "Undo could not be saved", e.to_string());
                return false;
            }
            let current = self.document();
            if let Ok(serialized) = serde_json::to_string(&*current) {
                push_history(to, serialized);
            }
            if let Ok(mut slot) = self.document.lock() {
                *slot = Arc::new(target);
            }
        }
        self.raise(ServiceEvent::DocumentChanged);
        self.reevaluate(true, None);
        true
    }

    /// The whole document as portable JSON: profiles, rules, macros, bindings.
    pub fn export_json(&self) -> String {
        serde_json::to_string_pretty(&*self.document()).unwrap_or_default()
    }

    /// Imports profiles and macros from an exported file.
    ///
    /// Untrusted input: validated, and ids are regenerated on collision. Macro references *inside*
    /// imported macros follow the same remapping - without that, an imported macro would silently
    /// run whichever local macro happens to share the old id.
    pub fn import_json(self: &Arc<Self>, json: &str, replace: bool) -> Result<usize, StoreError> {
        let incoming = crate::store::parse(json)?;
        let count = incoming.profiles.len();
        if replace {
            self.update_document(|d| {
                d.profiles = incoming.profiles;
                d.macros = incoming.macros;
                d.locked_profile_id = None;
            })?;
            return Ok(count);
        }

        self.update_document(|d| {
            let mut macro_map: HashMap<Id, Id> = HashMap::new();
            let mut macros = incoming.macros;
            for definition in &mut macros {
                let new_id = if d.macros.iter().any(|x| x.id == definition.id) {
                    Id::new()
                } else {
                    definition.id
                };
                macro_map.insert(definition.id, new_id);
                definition.id = new_id;
            }
            for mut definition in macros {
                definition.steps = remap_steps(definition.steps, &macro_map);
                d.macros.push(definition);
            }

            for mut profile in incoming.profiles {
                if d.profiles.iter().any(|x| x.id == profile.id) {
                    profile.id = Id::new();
                }
                profile.is_default = false;
                profile.order = d.profiles.len() as i32;
                for settings in profile.devices.values_mut() {
                    for binding in settings.bindings.values_mut() {
                        if let ActionDefinition::MacroRef { macro_id } = &binding.action {
                            if let Some(mapped) = macro_map.get(macro_id) {
                                binding.action = ActionDefinition::MacroRef { macro_id: *mapped };
                            }
                        }
                    }
                }
                d.profiles.push(profile);
            }
        })?;
        Ok(count)
    }

    /// Replaces the whole document: G HUB import, or a backup restore.
    pub fn replace_document(self: &Arc<Self>, document: &ProfileDocument) -> Result<(), StoreError> {
        let serialized =
            serde_json::to_string(document).map_err(|e| StoreError::Json(e.to_string()))?;
        self.mutate(
            |d| {
                if let Ok(fresh) = crate::store::parse(&serialized) {
                    d.profiles = fresh.profiles;
                    d.macros = fresh.macros;
                    d.locked_profile_id = fresh.locked_profile_id;
                    d.persistent_profile_id = fresh.persistent_profile_id;
                    d.auto_switch_enabled = fresh.auto_switch_enabled;
                    d.app = fresh.app;
                    d.app_lighting = fresh.app_lighting;
                    d.triggers = fresh.triggers;
                }
            },
            true,
            true,
        )
    }

    /// Re-reads the document from disk and publishes it, writing nothing back.
    ///
    /// Needed because the application runs as two processes that take turns owning the hardware:
    /// while the window is open it is the one making edits, so when the agent takes over again
    /// its own in-memory copy is whatever the document looked like before the window started. An
    /// agent that kept running on that copy would apply the profile the user just renamed, and
    /// would overwrite the edit on its next save.
    ///
    /// Deliberately not [`PeripheralService::replace_document`], which is for an import or a
    /// backup restore: that one saves, validates, rotates a backup and pushes an undo entry.
    /// Re-reading what is already on disk must do none of those - a save would rotate a backup
    /// every time the window closed, and an undo entry would let the user "undo" a reload.
    ///
    /// Returns the warnings the store raised while reading, which the caller logs.
    pub fn reload_from_store(self: &Arc<Self>) -> Vec<String> {
        let (document, warnings) = {
            let Ok(_guard) = self.edit_lock.lock() else {
                return vec!["the edit lock is poisoned; the document was not reloaded".to_owned()];
            };
            let (document, warnings) = self.store.load();
            let published = Arc::new(document);
            if let Ok(mut slot) = self.document.lock() {
                *slot = Arc::clone(&published);
            }
            (published, warnings)
        };
        let _ = document;
        self.raise(ServiceEvent::DocumentChanged);
        // Forced: the active profile may be a different one now, and the device state was set by
        // the other process from a document this one had not read.
        self.reevaluate(true, None);
        warnings
    }

    pub fn select_profile(self: &Arc<Self>, id: Id, persistent: bool) -> Result<(), StoreError> {
        self.update_document(|d| {
            if persistent {
                d.persistent_profile_id = Some(id);
            } else {
                d.locked_profile_id = Some(id);
            }
        })
    }

    pub fn set_lock(self: &Arc<Self>, id: Option<Id>) -> Result<(), StoreError> {
        self.update_document(|d| d.locked_profile_id = id)
    }

    // ------------------------------------------------------------- resolution

    /// Called by the foreground monitor. Cheap when nothing changed.
    pub fn on_foreground_changed(self: &Arc<Self>, app: Option<ForegroundApp>) {
        let key = format!(
            "{}|{}",
            app.as_ref().and_then(|a| a.executable_path.as_deref()).unwrap_or(""),
            app.as_ref().and_then(|a| a.window_title.as_deref()).unwrap_or("")
        );
        {
            let Ok(mut last) = self.last_foreground.lock() else { return };
            if *last == key {
                return;
            }
            *last = key;
        }
        if let Ok(mut slot) = self.foreground.lock() {
            *slot = app.clone();
        }
        if let Some(exe) = app.as_ref().and_then(|a| a.executable_name()).map(str::to_owned) {
            self.fire_trigger(TriggerEvent::ForegroundApp, exe, 0);
        }
        self.reevaluate(false, app);
    }

    /// Re-resolves the active profile and pushes it to every connected device.
    pub fn reevaluate(self: &Arc<Self>, force: bool, app: Option<ForegroundApp>) {
        let app = app.or_else(|| self.platform.foreground_app());
        if let Ok(mut slot) = self.foreground.lock() {
            *slot = app.clone();
        }
        let document = self.document();
        let resolution = resolver::resolve(&document, app.as_ref());
        let changed = {
            let Ok(mut active) = self.active.lock() else { return };
            let changed = active.as_ref().map(|a| a.profile_id) != resolution.as_ref().map(|r| r.profile_id);
            *active = resolution.clone();
            changed
        };
        let Some(resolution) = resolution else {
            // No profile could be resolved, which means the document has none. Validation
            // prevents that, so there is nothing sensible to apply and nothing to report.
            return;
        };

        // A per-application lighting rule can start or stop matching without the profile
        // changing, so the devices must be rewritten whenever the matching set moves.
        let devices = self.manager.devices();
        let rules: Vec<String> = devices
            .iter()
            .map(|d| {
                let matched = self
                    .matching_app_lighting(&d.key())
                    .map(|(rule, _)| rule.id.to_string())
                    .unwrap_or_default();
                format!("{}:{matched}", d.key())
            })
            .collect();
        let signature = rules.join("|");
        let lighting_changed = {
            let Ok(mut last) = self.last_lighting_rules.lock() else { return };
            let moved = *last != signature;
            *last = signature;
            moved
        };

        if !changed && !force && !lighting_changed {
            return;
        }
        if changed {
            // A profile change cancels running macros and releases held input, so no key is left
            // down by a binding the new profile does not have.
            self.runtime.stop_all("profile change");
            if let Ok(mut overrides) = self.dpi_stage_override.lock() {
                overrides.clear();
            }
            self.log.info("Profiles", format!("Active profile: {}", resolution.explain()));
            self.fire_trigger(
                TriggerEvent::ProfileActivated,
                resolution.profile_name.clone(),
                0,
            );
            self.raise(ServiceEvent::ActiveProfileChanged(resolution));
        }

        for device in devices.iter().filter(|d| d.connected) {
            self.apply_to_device(device);
        }
    }

    // ------------------------------------------------------------- device events

    fn on_device_change(self: &Arc<Self>, change: &DeviceChange) {
        match change.kind {
            DeviceChangeKind::Connected => {
                // Volatile settings were reset by the power cycle: re-apply the software state.
                self.apply_to_device(&change.device);
                self.fire_trigger(TriggerEvent::DeviceConnected, change.device.display_name(), 0);
            }
            DeviceChangeKind::Disconnected => {
                let key = change.key.clone();
                // Release keys and buttons held by this device's bindings: their key-up will
                // never arrive.
                self.runtime.release_held(&format!("{key}|"), "device disconnected");
                self.ownership.release(&key, &native_writer(), ALL_ZONES);
                self.animations.forget(&key);
                if let Ok(mut reactions) = self.reactions.lock() {
                    reactions.forget(&key);
                }
                self.fire_trigger(
                    TriggerEvent::DeviceDisconnected,
                    change.device.display_name(),
                    0,
                );
                if self.recording_target().is_some_and(|t| t.device_key == key) {
                    self.cancel_recording("device disconnected");
                }
            }
            DeviceChangeKind::Updated | DeviceChangeKind::Health => {
                if let Some(battery) = change.device.health.battery {
                    self.check_low_battery(&change.device, &battery);
                }
            }
            DeviceChangeKind::Error => {}
        }
    }

    fn check_low_battery(&self, device: &ManagedDevice, battery: &ghub_hidpp::features::battery::BatteryStatus) {
        let document = self.document();
        let Some(level) = battery.level_percent else { return };
        if !battery.is_discharging() {
            return;
        }
        self.fire_trigger(TriggerEvent::BatteryBelow, device.display_name(), i32::from(level));
        if document.app.low_battery_warnings
            && level <= document.app.low_battery_threshold_percent
        {
            self.raise(ServiceEvent::Notification {
                title: "Low battery".to_owned(),
                message: format!("{}: about {level}% (estimated)", device.display_name()),
            });
        }
    }

    fn on_device_event(self: &Arc<Self>, device: &ManagedDevice, event: &DeviceEvent) {
        let key = device.key();
        match *event {
            DeviceEvent::GKey { gkey, pressed } => {
                // G-keys carry no HID usage, so their reactions cannot come from the keyboard
                // hook; the device tells us directly.
                let writes = self
                    .reactions
                    .lock()
                    .map(|mut r| r.on_gkey(gkey, pressed))
                    .unwrap_or_default();
                self.write_frames(writes);

                match self.record_state() {
                    MacroRecordState::SelectTarget if pressed => {
                        self.begin_recording(&key, gkey);
                        return;
                    }
                    // G-keys are not recorded: the recording is for the keys the user types.
                    MacroRecordState::Recording => return,
                    _ => {}
                }

                let binding = if pressed {
                    self.find_binding(&key, &gkey_slot(gkey, self.active_bank(&key), false))
                } else {
                    None
                };
                if binding.is_some() || !pressed {
                    self.runtime.on_trigger(
                        &format!("{key}|G{gkey}"),
                        binding.as_ref().map(|b| &b.action),
                        pressed,
                    );
                }
            }
            DeviceEvent::ModeKey { mode } => {
                if self.record_state() != MacroRecordState::Idle {
                    self.cancel_recording("bank changed");
                }
                self.set_bank(&key, mode);
            }
            DeviceEvent::MacroRecordKey { pressed: true } => self.on_macro_record_key(device),
            DeviceEvent::MacroRecordKey { pressed: false } => {}
            DeviceEvent::SpyButton { button, pressed } => {
                if pressed {
                    self.log.trace(device.display_name(), format!("Spy button {button} pressed"));
                }
                self.raise(ServiceEvent::SpyButtonSeen {
                    device_key: key.clone(),
                    button,
                    pressed,
                });
                let muted = self
                    .spy_muted
                    .lock()
                    .map(|m| m.get(&key).is_some_and(|b| b.contains(&button)))
                    .unwrap_or(false);
                if !muted {
                    // The firmware still performs the native action; dispatching ours too would
                    // double it.
                    return;
                }
                let binding = if pressed { self.spy_binding(device, button) } else { None };
                if binding.is_some() || !pressed {
                    self.runtime.on_trigger(
                        &format!("{key}|spy{button}"),
                        binding.as_ref().map(|b| &b.action),
                        pressed,
                    );
                }
            }
            DeviceEvent::Link { established: true } => {
                let reapply = self
                    .spy_muted
                    .lock()
                    .map(|m| m.contains_key(&key))
                    .unwrap_or(false);
                if reapply {
                    // The firmware drops the runtime 0x8110 mapping on sleep and link-up, so the
                    // remap has to be re-sent - after a pause, because the device is not ready
                    // to accept it the instant the link comes back.
                    let service = Arc::clone(self);
                    let device = device.clone();
                    let _ = std::thread::Builder::new()
                        .name("ghub spy reapply".to_owned())
                        .spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(500));
                            service.apply_to_device(&device);
                        });
                }
            }
            DeviceEvent::Link { established: false } => {}
            DeviceEvent::Button { control_id, pressed } => {
                let slot = button_slot(&format!("cid{control_id:04X}"), false);
                let binding = if pressed { self.find_binding(&key, &slot) } else { None };
                if binding.is_some() || !pressed {
                    self.runtime.on_trigger(
                        &format!("{key}|cid{control_id:04X}"),
                        binding.as_ref().map(|b| &b.action),
                        pressed,
                    );
                }
            }
            DeviceEvent::Battery(status) => self.check_low_battery(device, &status),
        }
    }

    // ------------------------------------------------------------- applying a profile

    /// Pushes the active profile's volatile settings to one device, reading back each value.
    ///
    /// Returns one human-readable line per attempted setting, which is what the device page shows
    /// and what lands in the log.
    pub fn apply_to_device(self: &Arc<Self>, device: &ManagedDevice) -> Vec<String> {
        let mut results: Vec<String> = Vec::new();
        let key = device.key();
        if !device.connected {
            return results;
        }
        let settings = self.active_profile().devices.get(&key).cloned();
        let capabilities = device.capabilities.clone();

        {
            let Ok(_guard) = self.apply_lock.lock() else { return results };
            self.apply_mouse(&key, settings.as_ref(), &capabilities, &mut results);
            self.apply_spy(device, settings.as_ref(), &mut results);
            self.apply_keyboard(&key, settings.as_ref(), &mut results);
            let lighting = self.effective_lighting(
                &key,
                settings.as_ref().map(|s| &s.lighting),
                Some(&mut results),
            );
            self.apply_lighting(device, &lighting, &mut results);
        }

        let source = device.display_name();
        for line in &results {
            self.log.info(source.clone(), line.clone());
        }
        results
    }

    fn apply_mouse(
        &self,
        key: &str,
        settings: Option<&DeviceProfileSettings>,
        capabilities: &DeviceCapabilities,
        results: &mut Vec<String>,
    ) {
        let Some(mouse) = settings.and_then(|s| s.mouse.as_ref()) else { return };

        if let Some(dpi_capability) = capabilities.dpi.as_ref().filter(|c| c.meta.is_usable()) {
            let stage = self.current_stage(key, mouse);
            let enabled: Vec<&crate::profile::DpiStage> =
                mouse.dpi_stages.iter().filter(|s| s.enabled).collect();
            let wanted = enabled
                .get(stage)
                .or(enabled.first())
                .map(|s| s.dpi)
                .or_else(|| mouse.dpi_stages.first().map(|s| s.dpi));
            if let Some(wanted) = wanted {
                let target = dpi_capability.clamp(wanted);
                self.set_dpi_indicator(key, stage);
                results.push(self.attempt(format!("DPI {target}"), || {
                    self.manager
                        .with_session(key, |session| {
                            session
                                .dpi()
                                .ok_or_else(|| "the device has no DPI feature".to_owned())?
                                .set(target)
                                .map(|back| format!("read-back {back}"))
                                .map_err(|e| e.to_string())
                        })
                        .unwrap_or_else(|| Err("the device is not connected".to_owned()))
                }));
            }
        }

        if let Some(hz) = mouse.report_rate_hz {
            let supported = capabilities
                .report_rate
                .as_ref()
                .is_some_and(|r| r.rates_hz.contains(&hz));
            if supported {
                results.push(self.attempt(format!("Report rate {hz}"), || {
                    self.manager
                        .with_session(key, |session| {
                            session
                                .report_rate()
                                .ok_or_else(|| "the device has no report-rate feature".to_owned())?
                                .set_hz(hz)
                                .map(|back| format!("read-back {back}"))
                                .map_err(|e| e.to_string())
                        })
                        .unwrap_or_else(|| Err("the device is not connected".to_owned()))
                }));
            }
        }

        if mouse.wheel_high_resolution.is_some() || mouse.wheel_inverted.is_some() {
            let wanted_hi = mouse.wheel_high_resolution;
            let wanted_inverted = mouse.wheel_inverted;
            results.push(self.attempt("Scroll wheel", || {
                self.manager
                    .with_session(key, |session| {
                        let wheel = session
                            .wheel()
                            .ok_or_else(|| "the device has no wheel feature".to_owned())?;
                        // Read before write: the bits we are not asked to change must keep the
                        // value the device currently holds, not a guessed default.
                        let now = wheel.get_mode().map_err(|e| e.to_string())?;
                        let hi_res = wanted_hi.unwrap_or(now & 0x02 != 0);
                        let inverted =
                            wheel.info.can_invert && wanted_inverted.unwrap_or(now & 0x04 != 0);
                        let back = wheel.set(hi_res, inverted).map_err(|e| e.to_string())?;
                        Ok(format!(
                            "high resolution {}, inverted {}",
                            if back & 0x02 != 0 { "on" } else { "off" },
                            if back & 0x04 != 0 { "on" } else { "off" }
                        ))
                    })
                    .unwrap_or_else(|| Err("the device is not connected".to_owned()))
            }));
        }
    }

    /// Buttons 6+ that this application handles in software: those with a non-pass-through
    /// binding on either layer, plus the device's factory DPI buttons unless the profile assigns
    /// them pass-through on the base layer (the user asking for the firmware's own behaviour).
    pub fn bound_spy_buttons(
        settings: Option<&DeviceProfileSettings>,
        button_count: usize,
        defaults: &[(usize, DpiCommand)],
    ) -> Vec<usize> {
        let binding = |slot: &str| settings.and_then(|s| s.bindings.get(slot));
        (FIRST_SPY_BUTTON..=button_count)
            .filter(|button| {
                let explicit = [spy_slot(*button, false), spy_slot(*button, true)]
                    .iter()
                    .any(|slot| {
                        binding(slot)
                            .is_some_and(|b| !matches!(b.action, ActionDefinition::PassThrough))
                    });
                let by_default = defaults.iter().any(|(b, _)| b == button)
                    && binding(&spy_slot(*button, false)).is_none();
                explicit || by_default
            })
            .collect()
    }

    /// The action a spy button runs: the profile's binding (shift layer first), else the device's
    /// factory DPI role.
    fn spy_binding(&self, device: &ManagedDevice, button: usize) -> Option<ActionBinding> {
        let key = device.key();
        if let Some(binding) = self.find_binding(&key, &spy_slot(button, false)) {
            return Some(binding);
        }
        default_spy_dpi_buttons(device.descriptor.as_ref())
            .iter()
            .find(|(b, _)| *b == button)
            .map(|(_, command)| {
                ActionBinding::new(ActionDefinition::Dpi { command: *command, stage: 0 })
            })
    }

    fn apply_spy(
        &self,
        device: &ManagedDevice,
        settings: Option<&DeviceProfileSettings>,
        results: &mut Vec<String>,
    ) {
        if device.descriptor.as_ref().map(|d| d.kind) != Some(PeripheralKind::Mouse) {
            return;
        }
        let key = device.key();
        let Some(button_count) =
            self.manager.with_session(&key, |s| s.button_spy().map(|spy| spy.button_count))
        else {
            return;
        };
        let Some(button_count) = button_count else { return };

        // DPI buttons only count when the device has a usable DPI feature to step; otherwise
        // muting them would just make them dead.
        //
        // Nor in onboard-profile mode. Observed on a G903 HERO in onboard mode: the mapping write
        // is accepted and reads back, but the mouse sends no 0x8110 press events at all - so a
        // muted DPI button is simply dead, and the firmware's own DPI step is lost with it.
        //
        // So, like G HUB, a mouse with factory DPI buttons is moved to host mode first: read the
        // mode, switch only if it is onboard, and trust only the mode read back afterwards.
        let wants_dpi_buttons = !default_spy_dpi_buttons(device.descriptor.as_ref()).is_empty()
            && device.capabilities.dpi.as_ref().is_some_and(|d| d.meta.is_usable());
        let host_mode = if wants_dpi_buttons {
            self.ensure_host_mode(&key, results)
        } else {
            self.manager
                .with_session(&key, |s| s.onboard().map(|o| o.get_mode()))
                .flatten()
                .is_none_or(|mode| {
                    !matches!(mode, Ok(ghub_hidpp::features::onboard::OnboardMode::Onboard))
                })
        };
        let defaults = if host_mode
            && device.capabilities.dpi.as_ref().is_some_and(|d| d.meta.is_usable())
        {
            default_spy_dpi_buttons(device.descriptor.as_ref())
        } else {
            &[]
        };
        let bound = Self::bound_spy_buttons(settings, button_count, defaults);
        let had_muted =
            self.spy_muted.lock().map(|m| m.contains_key(&key)).unwrap_or(false);
        if bound.is_empty() && !had_muted {
            // Never touch the mapping - which may be G HUB's - unless this application actually
            // handles a spy button.
            return;
        }

        let label = if bound.is_empty() {
            "Live remap off".to_owned()
        } else {
            format!(
                "Live remap buttons {}",
                bound.iter().map(usize::to_string).collect::<Vec<_>>().join(",")
            )
        };
        let bound_for_write = bound.clone();
        let outcome = self.attempt(label, || {
            self.manager
                .with_session(&key, |session| {
                    let spy = session
                        .button_spy()
                        .ok_or_else(|| "the device has no button spy".to_owned())?;
                    let wanted = ghub_hidpp::features::mouse::muted_mapping(spy.button_count, &bound_for_write);
                    let back = spy.set_mapping(&wanted).map_err(|e| e.to_string())?;
                    if bound_for_write.is_empty() {
                        spy.stop().map_err(|e| e.to_string())?;
                    } else {
                        spy.start().map_err(|e| e.to_string())?;
                    }
                    let mut note = if back == wanted {
                        "mapping read-back ok".to_owned()
                    } else {
                        format!("mapping read-back {}", crate::diagnostics::to_hex(&back))
                    };
                    // Worth saying plainly: in onboard mode the firmware may keep its own
                    // binding, so the user sees "it did not work" with no other explanation.
                    if !bound_for_write.is_empty() {
                        if let Some(onboard) = session.onboard() {
                            if matches!(
                                onboard.get_mode(),
                                Ok(ghub_hidpp::features::onboard::OnboardMode::Onboard)
                            ) {
                                note.push_str(
                                    " (the device is in onboard-profile mode: the firmware may \
                                     keep the onboard binding)",
                                );
                            }
                        }
                    }
                    Ok(note)
                })
                .unwrap_or_else(|| Err("the device is not connected".to_owned()))
        });
        let succeeded = !outcome.contains("FAILED");
        results.push(outcome);
        if succeeded {
            if let Ok(mut muted) = self.spy_muted.lock() {
                if bound.is_empty() {
                    muted.remove(&key);
                } else {
                    muted.insert(key, bound);
                }
            }
        }
    }

    /// Puts a mouse in host mode if it is in onboard mode. Returns whether it is in host mode
    /// afterwards, by read-back. A RAM setting: a power cycle returns the mouse to onboard mode,
    /// and [`Self::hand_back_hardware`] switches it back explicitly.
    fn ensure_host_mode(&self, key: &str, results: &mut Vec<String>) -> bool {
        use ghub_hidpp::features::onboard::OnboardMode;
        let outcome = self.manager.with_session(key, |session| {
            let onboard = session.onboard()?;
            Some((|| {
                let before = onboard.get_mode().map_err(|e| e.to_string())?;
                if before != OnboardMode::Onboard {
                    return Ok((before, before));
                }
                onboard.set_mode(OnboardMode::Host).map_err(|e| e.to_string())?;
                let after = onboard.get_mode().map_err(|e| e.to_string())?;
                Ok::<_, String>((before, after))
            })())
        });
        match outcome {
            // No onboard-profile feature: nothing to switch, the host already drives it.
            None | Some(None) => true,
            Some(Some(Ok((before, after)))) => {
                if before == OnboardMode::Onboard {
                    results.push(format!("Host mode: switched from onboard, read-back {after:?}"));
                    if after == OnboardMode::Host {
                        if let Ok(mut set) = self.host_mode_set.lock() {
                            set.insert(key.to_owned());
                        }
                    }
                }
                after == OnboardMode::Host
            }
            Some(Some(Err(e))) => {
                results.push(format!("Host mode: FAILED ({e})"));
                false
            }
        }
    }

    fn apply_keyboard(
        &self,
        key: &str,
        settings: Option<&DeviceProfileSettings>,
        results: &mut Vec<String>,
    ) {
        let Some(keyboard) = settings.and_then(|s| s.keyboard.as_ref()) else {
            // This profile does not manage the keyboard: hand G-keys back to the firmware, but
            // only if we are the ones who took them.
            let was_diverted =
                self.diverted_gkeys.lock().map(|mut d| d.remove(key)).unwrap_or(false);
            if was_diverted {
                results.push(self.attempt("G-key software control", || {
                    self.manager
                        .with_session(key, |session| {
                            session
                                .gkeys()
                                .ok_or_else(|| "no G-key feature".to_owned())?
                                .set_software_control(false)
                                .map(|()| "off (the profile has no keyboard settings)".to_owned())
                                .map_err(|e| e.to_string())
                        })
                        .unwrap_or_else(|| Err("the device is not connected".to_owned()))
                }));
            }
            return;
        };

        let software_gkeys = keyboard.software_gkeys;
        let gkey_result = self.manager.with_session(key, |session| {
            session.gkeys().map(|gkeys| gkeys.set_software_control(software_gkeys))
        });
        if let Some(Some(outcome)) = gkey_result {
            match outcome {
                Ok(()) => {
                    if let Ok(mut diverted) = self.diverted_gkeys.lock() {
                        if software_gkeys {
                            diverted.insert(key.to_owned());
                        } else {
                            diverted.remove(key);
                        }
                    }
                    results.push(format!(
                        "G-key software control: {}",
                        if software_gkeys { "on" } else { "off" }
                    ));
                }
                Err(e) => results.push(format!("G-key software control: FAILED ({e})")),
            }
        }

        let bank = keyboard.active_bank;
        if let Some(Some(outcome)) = self
            .manager
            .with_session(key, |s| s.mode_keys().map(|m| m.set_mode_indicator(bank)))
        {
            results.push(match outcome {
                Ok(()) => format!("Bank M{bank}: indicator set"),
                Err(e) => format!("Bank M{bank}: FAILED ({e})"),
            });
        }

        if let Some(percent) = keyboard.brightness_percent {
            results.push(self.attempt(format!("Brightness {percent}"), || {
                self.manager
                    .with_session(key, |session| {
                        let brightness = session
                            .brightness()
                            .ok_or_else(|| "no brightness feature".to_owned())?;
                        // The device's scale is not 0-100: convert through its own maximum.
                        let maximum = brightness.maximum;
                        let target = (i32::from(percent) * i32::from(maximum) / 100).clamp(0, i32::from(u16::MAX)) as u16;
                        brightness
                            .set(target)
                            .map(|back| format!("read-back {back}"))
                            .map_err(|e| e.to_string())
                    })
                    .unwrap_or_else(|| Err("the device is not connected".to_owned()))
            }));
        }

        // The firmware list applies while the keyboard's own game-mode key is engaged; the
        // software hook covers the in-app toggle. Both exist because neither alone is enough.
        let usages = keyboard.game_mode_disabled_usages.clone();
        if !usages.is_empty() || keyboard.game_mode_enabled {
            let count = usages.len();
            results.push(self.attempt(format!("Game-mode key list ({count} keys)"), || {
                self.manager
                    .with_session(key, |session| {
                        session
                            .disable_keys()
                            .ok_or_else(|| "no key-disable feature".to_owned())?
                            .set_disabled_keys(&usages)
                            .map(|()| "sent to the keyboard".to_owned())
                            .map_err(|e| e.to_string())
                    })
                    .unwrap_or_else(|| Err("the device is not connected".to_owned()))
            }));
        }
    }

    // ------------------------------------------------------------- lighting

    /// The lighting a device should show right now.
    ///
    /// The active profile's, unless a per-application lighting rule matches the foreground
    /// program, in which case the rule's values replace the ones it sets - key by key for static
    /// colours, so a rule may recolour WASD and leave the rest of the layout alone.
    pub fn effective_lighting(
        &self,
        device_key: &str,
        profile_lighting: Option<&LightingSettings>,
        mut results: Option<&mut Vec<String>>,
    ) -> LightingSettings {
        let baseline = profile_lighting.cloned().unwrap_or_default();
        let Some((rule, over)) = self.matching_app_lighting(device_key) else {
            return baseline;
        };
        let merged = over.apply_to(&baseline);
        if let Some(results) = results.as_mut() {
            results.push(format!(
                "Lighting rule '{}' ({}) applied",
                rule.name, rule.executable_name
            ));
        }
        merged
    }

    /// The file name of whatever is in the foreground right now, for the pages that explain why a
    /// rule is or is not matching.
    ///
    /// Read from the platform rather than from the cached value, because the cache is only updated
    /// when the foreground *changes* and a page can be opened long after the last change.
    pub fn foreground_executable(&self) -> Option<String> {
        self.platform.foreground_app()?.executable_name().map(str::to_owned)
    }

    /// The highest-priority enabled lighting rule that matches the foreground program for this
    /// device.
    pub fn matching_app_lighting(
        &self,
        device_key: &str,
    ) -> Option<(AppLightingRule, AppLightingOverride)> {
        let foreground = self.foreground.lock().ok()?.clone()?;
        let exe = foreground.executable_name()?.to_ascii_lowercase();
        let document = self.document();
        let mut candidates: Vec<&AppLightingRule> = document
            .app_lighting
            .iter()
            .filter(|r| r.enabled && r.executable_name.trim().to_ascii_lowercase() == exe)
            .collect();
        // Highest priority first; ties broken by id so the choice is deterministic rather than
        // whichever happened to be saved first.
        candidates.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.id.cmp(&b.id)));
        for rule in candidates {
            if let Some(over) = rule.devices.get(device_key) {
                return Some((rule.clone(), over.clone()));
            }
        }
        None
    }

    fn apply_lighting(
        self: &Arc<Self>,
        device: &ManagedDevice,
        lighting: &LightingSettings,
        results: &mut Vec<String>,
    ) {
        let key = device.key();

        if lighting.power_save_idle_seconds.is_some() || lighting.power_save_sleep_seconds.is_some()
        {
            let idle = lighting.power_save_idle_seconds;
            let sleep = lighting.power_save_sleep_seconds;
            if let Some(Some(_)) = self.manager.with_session(&key, |s| s.rgb().map(|_| ())) {
                results.push(self.attempt("Lighting power saving", || {
                    self.manager
                        .with_session(&key, |session| {
                            let rgb = session
                                .rgb()
                                .ok_or_else(|| "no RGB effects feature".to_owned())?;
                            let now = rgb.get_power_timers().map_err(|e| e.to_string())?;
                            let wanted = ghub_hidpp::features::lighting::RgbPowerTimers {
                                idle_seconds: idle.unwrap_or(now.idle_seconds),
                                sleep_seconds: sleep.unwrap_or(now.sleep_seconds),
                                // Flags are the device's own; carried through untouched.
                                flags: now.flags,
                            };
                            if wanted == now {
                                return Ok(format!(
                                    "unchanged (dim {}s, off {}s)",
                                    now.idle_seconds, now.sleep_seconds
                                ));
                            }
                            let back =
                                rgb.set_power_timers(wanted).map_err(|e| e.to_string())?;
                            Ok(if back == wanted {
                                format!("dim {}s, off {}s", back.idle_seconds, back.sleep_seconds)
                            } else {
                                format!(
                                    "the device read back dim {}s, off {}s",
                                    back.idle_seconds, back.sleep_seconds
                                )
                            })
                        })
                        .unwrap_or_else(|| Err("the device is not connected".to_owned()))
                }));
            }
        }

        match lighting.mode {
            LightingMode::Untouched => {
                self.ownership.release(&key, &native_writer(), ALL_ZONES);
                self.ownership.release(&key, &artemis_writer(), ALL_ZONES);
                self.forget_lighting(&key);
                let acquired = self
                    .manager
                    .with_session(&key, |s| {
                        s.per_key_lighting().is_some_and(|p| p.acquired)
                    })
                    .unwrap_or(false);
                if acquired {
                    results.push(self.attempt("Lighting returned to the device", || {
                        self.manager
                            .with_session(&key, |session| {
                                session
                                    .release_per_key_lighting()
                                    .map(|()| "ok".to_owned())
                                    .map_err(|e| e.to_string())
                            })
                            .unwrap_or_else(|| Err("the device is not connected".to_owned()))
                    }));
                }
            }
            LightingMode::Artemis => {
                self.ownership.release(&key, &native_writer(), ALL_ZONES);
                self.forget_lighting(&key);
                results.push(if self.ownership.request(&key, &artemis_writer(), ALL_ZONES) {
                    "Lighting: Artemis profile streaming".to_owned()
                } else {
                    format!(
                        "Lighting: owned by {}",
                        self.owner_name(&key)
                    )
                });
            }
            LightingMode::Animated => {
                if !self.ownership.request(&key, &native_writer(), ALL_ZONES) {
                    results.push(format!("Lighting skipped: owned by {}", self.owner_name(&key)));
                    self.forget_lighting(&key);
                    return;
                }
                let has_per_key = self
                    .manager
                    .with_session(&key, |s| s.per_key_lighting().is_some())
                    .unwrap_or(false);
                if !has_per_key {
                    // Only a per-key keyboard can be animated; a zone-lit device keeps its static
                    // colours rather than being left dark.
                    self.animations.forget(&key);
                    results.push("Animation needs per-key lighting; this device has none".to_owned());
                    return;
                }

                let effect_name = effects::definition(&lighting.animation.effect).name;
                let settings = lighting.animation.clone();
                let brightness = lighting.brightness;
                let reactions_for_key = self
                    .active_profile()
                    .devices
                    .get(&key)
                    .and_then(|d| d.keyboard.as_ref())
                    .map(|k| k.key_reactions.clone())
                    .unwrap_or_default();

                results.push(self.attempt(format!("Animation '{effect_name}'"), || {
                    let acquired = self
                        .manager
                        .with_session(&key, |session| {
                            if session.per_key_lighting().is_some_and(|p| p.acquired) {
                                Ok(())
                            } else {
                                session.acquire_per_key_lighting()
                            }
                        })
                        .unwrap_or_else(|| {
                            Err(ghub_hidpp::error::HidppError::Unsupported(
                                "the device is not connected".into(),
                            ))
                        });
                    acquired.map_err(|e| e.to_string())?;
                    // Reactions sit on top of the animation, so they are configured against its
                    // first frame rather than a blank one.
                    let first = effects::render(
                        &settings,
                        0.0,
                        &effects::EffectState::default(),
                        brightness,
                    );
                    if let Ok(mut engine) = self.reactions.lock() {
                        engine.configure(&key, &first, &reactions_for_key, brightness);
                    }
                    self.animations.configure(&key, &settings, brightness);
                    Ok(format!("{} fps", settings.frames_per_second))
                }));
            }
            LightingMode::Static => {
                self.animations.forget(&key);
                if !self.ownership.request(&key, &native_writer(), ALL_ZONES) {
                    results.push(format!("Lighting skipped: owned by {}", self.owner_name(&key)));
                    if let Ok(mut engine) = self.reactions.lock() {
                        engine.forget(&key);
                    }
                    return;
                }
                self.apply_static_lighting(&key, lighting, results);
            }
            LightingMode::DeviceEffect => {
                self.forget_lighting(&key);
                if lighting.device_effect_id.is_none() && lighting.zone_effects.is_empty() {
                    return;
                }
                if !self.ownership.request(&key, &native_writer(), ALL_ZONES) {
                    results.push(format!("Lighting skipped: owned by {}", self.owner_name(&key)));
                    return;
                }
                // A device effect is the firmware's own animation, so software per-key control
                // has to be handed back first or the two fight over the LEDs.
                let _ = self.manager.with_session(&key, |session| {
                    if session.per_key_lighting().is_some_and(|p| p.acquired) {
                        let _ = session.release_per_key_lighting();
                    }
                });

                let clusters = self
                    .manager
                    .with_session(&key, |s| {
                        s.rgb()
                            .map(|rgb| {
                                // A zone's own effect wins; otherwise the profile-wide one. A
                                // cluster that offers neither is left alone.
                                rgb.clusters
                                    .iter()
                                    .filter_map(|c| {
                                        let zone = lighting.zone_effects.get(&c.location);
                                        let effect = zone
                                            .map(|z| z.effect_id)
                                            .or(lighting.device_effect_id)?;
                                        rgb.effect_index_for(usize::from(c.index), effect)?;
                                        Some((
                                            usize::from(c.index),
                                            c.location_name(),
                                            effect,
                                            zone.cloned(),
                                        ))
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default()
                    })
                    .unwrap_or_default();

                for (index, name, effect, zone) in clusters {
                    let fallback = lighting
                        .static_colors
                        .get(&format!("zone:{index}"))
                        .unwrap_or(&lighting.default_color);
                    let color = parse_color(
                        zone.as_ref().and_then(|z| z.color.as_ref()).unwrap_or(fallback),
                    )
                    .unwrap_or(LedColor { r: 255, g: 255, b: 255 });
                    let period =
                        zone.as_ref().map_or(lighting.device_effect_period_ms, |z| z.period_ms);
                    let brightness = (lighting.brightness * 100.0).round().clamp(0.0, 100.0) as u8;
                    results.push(self.attempt(format!("{name} effect 0x{effect:04X}"), || {
                        self.manager
                            .with_session(&key, |session| {
                                session
                                    .rgb()
                                    .ok_or_else(|| "no RGB effects feature".to_owned())?
                                    .set_effect(index, effect, color, period, brightness)
                                    .map(|()| "set".to_owned())
                                    .map_err(|e| e.to_string())
                            })
                            .unwrap_or_else(|| Err("the device is not connected".to_owned()))
                    }));
                }
            }
        }
    }

    fn apply_static_lighting(
        &self,
        key: &str,
        lighting: &LightingSettings,
        results: &mut Vec<String>,
    ) {
        let has_per_key = self
            .manager
            .with_session(key, |s| s.per_key_lighting().is_some())
            .unwrap_or(false);
        if !has_per_key {
            // Zone-lit devices (the mouse) have no per-key reactions.
            if let Ok(mut engine) = self.reactions.lock() {
                engine.forget(key);
            }
            let zones = self
                .manager
                .with_session(key, |s| {
                    s.rgb()
                        .map(|rgb| {
                            rgb.zones.iter().map(|z| (z.index, z.name.clone())).collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            for (index, name) in zones {
                let color = parse_color(
                    lighting
                        .static_colors
                        .get(&format!("zone:{index}"))
                        .unwrap_or(&lighting.default_color),
                )
                .unwrap_or(LedColor { r: 255, g: 255, b: 255 });
                let scaled = effects::scale(color, lighting.brightness);
                results.push(self.attempt(format!("Zone {name} static"), || {
                    self.manager
                        .with_session(key, |session| {
                            session
                                .rgb()
                                .ok_or_else(|| "no RGB effects feature".to_owned())?
                                .set_static_color(index, scaled)
                                .map(|()| {
                                    format!("#{:02X}{:02X}{:02X}", scaled.r, scaled.g, scaled.b)
                                })
                                .map_err(|e| e.to_string())
                        })
                        .unwrap_or_else(|| Err("the device is not connected".to_owned()))
                }));
            }
            return;
        }

        let reactions_for_key = self
            .active_profile()
            .devices
            .get(key)
            .and_then(|d| d.keyboard.as_ref())
            .map(|k| k.key_reactions.clone())
            .unwrap_or_default();
        let frame = self.static_frame(lighting);
        let count = frame.len();
        results.push(self.attempt("Static per-key lighting", || {
            self.manager
                .with_session(key, |session| {
                    if !session.per_key_lighting().is_some_and(|p| p.acquired) {
                        session.acquire_per_key_lighting().map_err(|e| e.to_string())?;
                    }
                    session.write_per_key_frame(&frame).map_err(|e| e.to_string())
                })
                .unwrap_or_else(|| Err("the device is not connected".to_owned()))
                .map(|()| format!("{count} keys"))
        }));
        // This frame is the unreacted layout, so it is also what pressed keys return to.
        let overlay = self
            .reactions
            .lock()
            .ok()
            .and_then(|mut engine| {
                engine.configure(key, &frame, &reactions_for_key, lighting.brightness)
            });
        if let Some(overlay) = overlay {
            let _ = self
                .manager
                .with_session(key, |session| session.write_per_key_frame(&overlay));
        }
    }

    /// The static layout as LED ids: every key's own colour if the profile names one, otherwise
    /// the default colour, both scaled by brightness.
    pub fn static_frame(&self, lighting: &LightingSettings) -> Frame {
        let default = effects::scale(
            parse_color(&lighting.default_color).unwrap_or(LedColor { r: 255, g: 255, b: 255 }),
            lighting.brightness,
        );
        layout::keys()
            .iter()
            .filter(|k| k.led_id != 0)
            .map(|k| {
                let color = lighting
                    .static_colors
                    .get(&format!("key:{}", k.id))
                    .and_then(|hex| parse_color(hex))
                    .map(|c| effects::scale(c, lighting.brightness))
                    .unwrap_or(default);
                (k.led_id, color)
            })
            .collect()
    }

    fn forget_lighting(&self, key: &str) {
        self.animations.forget(key);
        if let Ok(mut engine) = self.reactions.lock() {
            engine.forget(key);
        }
    }

    fn owner_name(&self, key: &str) -> String {
        self.ownership
            .owner_of(key, ALL_ZONES)
            .map(|w| w.display_name)
            .unwrap_or_else(|| "another writer".to_owned())
    }

    /// Writes reaction frames straight to their devices.
    fn write_frames(&self, writes: crate::reactions::Writes) {
        for (key, frame) in writes {
            if !self.ownership.can_write(&key, &native_writer(), ALL_ZONES) {
                continue;
            }
            let _ = self
                .manager
                .with_session(&key, |session| session.write_per_key_frame(&frame));
        }
    }

    /// `"what: result"`, or `"what: FAILED (message)"`. Kept verbatim from the C# because these
    /// strings are what the device page shows the user.
    fn attempt(
        &self,
        what: impl Into<String>,
        action: impl FnOnce() -> Result<String, String>,
    ) -> String {
        let what = what.into();
        match action() {
            Ok(outcome) => format!("{what}: {outcome}"),
            Err(e) => format!("{what}: FAILED ({e})"),
        }
    }

    fn current_stage(&self, device_key: &str, mouse: &MouseSettings) -> usize {
        self.dpi_stage_override
            .lock()
            .ok()
            .and_then(|o| o.get(device_key).copied())
            .unwrap_or(mouse.active_stage)
    }

    // ------------------------------------------------------------- input routing

    /// Finds the binding for a slot, honouring the G-Shift layer and falling back to the base
    /// layer when the shifted slot is unbound.
    pub fn find_binding(&self, device_key: &str, base_slot: &str) -> Option<ActionBinding> {
        let profile = self.active_profile();
        let settings = profile.devices.get(device_key)?;
        if self.gshift_held() {
            if let Some(shifted) = settings.bindings.get(&format!("{base_slot}:shift")) {
                return Some(shifted.clone());
            }
        }
        settings.bindings.get(base_slot).cloned()
    }

    /// Software mouse button hook entry. Returns whether the event must be suppressed because a
    /// binding handled it.
    pub fn on_mouse_button(
        self: &Arc<Self>,
        device_key: &str,
        button: MouseButtonKind,
        pressed: bool,
    ) -> bool {
        if crate::injection::is_injecting() {
            return false;
        }
        if self.record_state() == MacroRecordState::Recording {
            if let Ok(mut recorder) = self.recorder.lock() {
                recorder.on_mouse_button(button, pressed);
            }
        }
        if !pressed {
            // The release follows the press-time action, even if the binding changed in between.
            self.runtime.on_trigger(&format!("{device_key}|{button:?}"), None, false);
            return true;
        }

        let Some(binding) = self.find_binding(device_key, &button_slot(&format!("{button:?}"), false))
        else {
            return false;
        };
        if matches!(binding.action, ActionDefinition::PassThrough) {
            return false;
        }
        if button == MouseButtonKind::Left && !self.has_primary_click_recovery(device_key) {
            // Safety: the primary click is only remapped when another control still provides a
            // left click. Otherwise the user cannot click the button that would undo it.
            return false;
        }
        self.runtime
            .on_trigger(&format!("{device_key}|{button:?}"), Some(&binding.action), true);
        true
    }

    /// Whether some other binding of the profile still produces a left click.
    pub fn has_primary_click_recovery(&self, device_key: &str) -> bool {
        let profile = self.active_profile();
        let Some(settings) = profile.devices.get(device_key) else { return false };
        let left_prefix = button_slot("Left", false);
        settings.bindings.iter().any(|(slot, binding)| {
            !slot.starts_with(&left_prefix)
                && matches!(
                    binding.action,
                    ActionDefinition::MouseButton { button: MouseButtonKind::Left }
                )
        })
    }

    /// Software mouse hooks cannot tell mice apart, so bindings apply to the first managed mouse.
    pub fn primary_mouse_key(&self) -> Option<String> {
        self.manager
            .devices()
            .into_iter()
            .find(|d| d.descriptor.as_ref().map(|x| x.kind) == Some(PeripheralKind::Mouse))
            .map(|d| d.key())
    }

    /// Keyboard hook entry: drives macro recording and per-key press reactions.
    ///
    /// `extended` is the hook's extended-key flag, which separates the numpad's Enter from the
    /// main Enter.
    pub fn on_keyboard_key(self: &Arc<Self>, virtual_key: u16, down: bool, extended: bool) {
        if crate::injection::is_injecting() {
            return;
        }
        if let Ok(mut recorder) = self.recorder.lock() {
            if recorder.is_recording() {
                recorder.on_key(virtual_key, down);
            }
        }
        if !self.saw_keyboard_hook.swap(true, Ordering::SeqCst) {
            // Said once: it separates "the hook never fires" from "the hook fires but nothing
            // matched".
            self.log.info_with(
                "Input",
                "Keyboard hook delivering",
                format!("first key vk 0x{virtual_key:02X}"),
            );
        }

        let writes = self
            .reactions
            .lock()
            .map(|mut r| r.on_key(virtual_key, down, extended))
            .unwrap_or_default();
        self.write_frames(writes);

        if down && self.animations.is_active() {
            for key_id in crate::reactions::key_ids_for(virtual_key, extended) {
                self.animations.on_key(key_id, true);
            }
        }
    }

    /// Software game mode: whether the keyboard hook must swallow this key.
    pub fn is_game_mode_blocked(&self, virtual_key: u16) -> bool {
        if crate::injection::is_injecting() {
            return false;
        }
        let profile = self.active_profile();
        profile.devices.values().any(|settings| {
            settings
                .keyboard
                .as_ref()
                .is_some_and(|kb| kb.game_mode_enabled)
                && settings.keyboard.as_ref().is_some_and(|kb| {
                    kb.game_mode_disabled_usages
                        .iter()
                        .any(|usage| {
                            crate::action::virtual_key_from_hid_usage(*usage) == Some(virtual_key)
                        })
                })
        })
    }

    pub fn set_game_mode(self: &Arc<Self>, device_key: &str, enabled: bool) -> Result<(), StoreError> {
        let profile_id = self.active_profile().id;
        let key = device_key.to_owned();
        self.update_document(|d| {
            if let Some(profile) = d.profiles.iter_mut().find(|p| p.id == profile_id) {
                let settings = profile.for_device(&key);
                settings.keyboard.get_or_insert_with(Default::default).game_mode_enabled = enabled;
            }
        })?;
        self.status(if enabled { "Game mode on (software)" } else { "Game mode off" });
        Ok(())
    }

    pub fn active_bank(&self, device_key: &str) -> usize {
        self.active_profile()
            .devices
            .get(device_key)
            .and_then(|d| d.keyboard.as_ref())
            .map(|k| k.active_bank)
            .unwrap_or(1)
    }

    pub fn set_bank(self: &Arc<Self>, device_key: &str, bank: usize) {
        let max = self
            .manager
            .get(device_key)
            .and_then(|d| d.capabilities.mode_keys.as_ref().map(|m| m.count))
            .unwrap_or(3);
        if bank < 1 || bank > max {
            return;
        }
        // Bindings held from the old bank are released through their original press-time actions.
        self.runtime.release_held(&format!("{device_key}|"), "bank change");

        // Persisted so the app-side bank reconciles on reconnect: the M-key state is not readable
        // back from the device.
        let profile_id = self.active_profile().id;
        let key = device_key.to_owned();
        if let Err(e) = self.mutate(
            |d| {
                // Index first, then borrow once: `find(..).or_else(|| first_mut())` borrows
                // `profiles` twice and does not compile.
                let index = d
                    .profiles
                    .iter()
                    .position(|p| p.id == profile_id)
                    .unwrap_or(0);
                if let Some(profile) = d.profiles.get_mut(index) {
                    let settings = profile.for_device(&key);
                    settings.keyboard.get_or_insert_with(Default::default).active_bank = bank;
                }
            },
            false,
            false,
        ) {
            self.log.warning_with("Profiles", "Could not persist the bank", e.to_string());
        }

        if let Some(Some(Err(e))) = self
            .manager
            .with_session(device_key, |s| s.mode_keys().map(|m| m.set_mode_indicator(bank)))
        {
            self.log
                .warning_with("Keyboard", "Bank indicator update failed", e.to_string());
        }

        self.status(format!("G-key bank M{bank}"));
        let name = self
            .manager
            .get(device_key)
            .map(|d| d.display_name())
            .unwrap_or_else(|| device_key.to_owned());
        self.fire_trigger(TriggerEvent::BankChanged, name, bank as i32);
    }

    // ------------------------------------------------------------- MR workflow

    fn on_macro_record_key(self: &Arc<Self>, device: &ManagedDevice) {
        match self.record_state() {
            MacroRecordState::Idle => {
                self.set_record_state(MacroRecordState::SelectTarget);
                self.set_mr_indicator(&device.key(), true);
                self.status("Macro record: press the G-key to record to (MR again cancels)");
            }
            MacroRecordState::SelectTarget => self.cancel_recording("MR pressed again"),
            MacroRecordState::Recording => {
                self.finish_recording();
            }
        }
        self.raise(ServiceEvent::RecordStateChanged);
    }

    /// Starts recording for the Macros page, which captures input without a G-key to bind it to.
    ///
    /// Deliberately outside the [`MacroRecordState`] machine: that state exists to track the MR
    /// workflow's "which G-key?" question, and this has no answer to it. The input hooks feed the
    /// recorder whenever it is running, so starting it is all that is needed.
    pub fn start_free_recording(&self, record_delays: bool, record_mouse_buttons: bool) {
        if let Ok(mut recorder) = self.recorder.lock() {
            recorder.record_delays = record_delays;
            recorder.record_mouse_buttons = record_mouse_buttons;
            recorder.start();
        }
    }

    /// Stops a [`Self::start_free_recording`] session and returns what it captured.
    pub fn stop_free_recording(&self) -> Vec<MacroStep> {
        self.recorder.lock().map(|mut recorder| recorder.stop()).unwrap_or_default()
    }

    pub fn is_recording(&self) -> bool {
        self.recorder.lock().map(|recorder| recorder.is_recording()).unwrap_or(false)
    }

    /// In-app equivalent of the MR workflow, for keyboards with no MR key or from the UI.
    pub fn start_recording_from_ui(self: &Arc<Self>, device_key: &str, gkey: usize) {
        self.set_record_state(MacroRecordState::SelectTarget);
        self.begin_recording(device_key, gkey);
        self.set_mr_indicator(device_key, true);
    }

    fn begin_recording(self: &Arc<Self>, device_key: &str, gkey: usize) {
        let bank = self.active_bank(device_key);
        if let Ok(mut target) = self.recording_target.lock() {
            *target = Some(RecordingTarget {
                device_key: device_key.to_owned(),
                gkey,
                bank,
            });
        }
        self.set_record_state(MacroRecordState::Recording);
        if let Ok(mut recorder) = self.recorder.lock() {
            recorder.record_delays = true;
            recorder.start();
        }
        self.status(format!("Recording macro for G{gkey} (M{bank}). Press MR to save."));
        self.raise(ServiceEvent::RecordStateChanged);
    }

    /// Stops recording and binds the captured macro to the target G-key.
    pub fn finish_recording(self: &Arc<Self>) -> Option<Id> {
        if self.record_state() != MacroRecordState::Recording {
            self.cancel_recording("nothing recorded");
            return None;
        }
        let Some(target) = self.recording_target() else {
            self.cancel_recording("nothing recorded");
            return None;
        };

        let steps = self
            .recorder
            .lock()
            .map(|mut recorder| recorder.stop())
            .unwrap_or_default();
        self.set_record_state(MacroRecordState::Idle);
        if let Ok(mut slot) = self.recording_target.lock() {
            *slot = None;
        }
        self.set_mr_indicator(&target.device_key, false);

        if steps.is_empty() {
            self.status("Recording discarded (no keys captured)");
            self.raise(ServiceEvent::RecordStateChanged);
            return None;
        }

        let count = steps.len();
        let mut definition = MacroDefinition::new(format!(
            "G{} M{} recording {}",
            target.gkey,
            target.bank,
            crate::time::format_readable(crate::time::now_unix_millis())
        ));
        definition.steps = steps;
        definition.tags = vec!["recorded".to_owned()];
        let macro_id = definition.id;

        let profile_id = self.active_profile().id;
        let slot = gkey_slot(target.gkey, target.bank, false);
        let device_key = target.device_key.clone();
        if let Err(e) = self.update_document(|d| {
            d.macros.push(definition);
            if let Some(profile) = d.profiles.iter_mut().find(|p| p.id == profile_id) {
                profile
                    .for_device(&device_key)
                    .bindings
                    .insert(slot, ActionBinding::new(ActionDefinition::MacroRef { macro_id }));
            }
        }) {
            self.log.error_with("Macros", "The recording could not be saved", e.to_string());
            self.status("Recording could not be saved");
            self.raise(ServiceEvent::RecordStateChanged);
            return None;
        }
        self.status(format!(
            "Saved macro to G{} (M{}), {count} events",
            target.gkey, target.bank
        ));
        self.raise(ServiceEvent::RecordStateChanged);
        Some(macro_id)
    }

    pub fn cancel_recording(self: &Arc<Self>, reason: &str) {
        if self.record_state() == MacroRecordState::Idle {
            return;
        }
        let target = self.recording_target();
        if let Ok(mut recorder) = self.recorder.lock() {
            recorder.cancel();
        }
        self.set_record_state(MacroRecordState::Idle);
        if let Ok(mut slot) = self.recording_target.lock() {
            *slot = None;
        }
        match target {
            Some(target) => self.set_mr_indicator(&target.device_key, false),
            // No target recorded: clear the indicator on every keyboard that has one, rather than
            // leaving a lit MR key with no recording behind it.
            None => {
                for device in self.manager.devices() {
                    let has_mr = device
                        .capabilities
                        .mode_keys
                        .as_ref()
                        .is_some_and(|m| m.has_macro_record_key);
                    if has_mr {
                        self.set_mr_indicator(&device.key(), false);
                    }
                }
            }
        }
        self.status(format!("Macro recording cancelled ({reason})"));
        self.raise(ServiceEvent::RecordStateChanged);
    }

    fn set_record_state(&self, state: MacroRecordState) {
        if let Ok(mut slot) = self.record_state.lock() {
            *slot = state;
        }
    }

    fn set_mr_indicator(&self, device_key: &str, on: bool) {
        let outcome = self.manager.with_session(device_key, |session| {
            session
                .mode_keys()
                .filter(|m| m.has_macro_record_key)
                .map(|m| m.set_macro_record_indicator(on))
        });
        if let Some(Some(Err(e))) = outcome {
            self.log
                .warning_with("Keyboard", "MR indicator update failed", e.to_string());
        }
    }

    // ------------------------------------------------------------- shutdown

    /// Stops everything and hands the hardware back to its firmware.
    pub fn shutdown(self: &Arc<Self>) {
        self.hand_back_hardware("shutting down");
        self.manager.shutdown();
    }

    /// Hands the hardware back but leaves the manager able to take it again.
    ///
    /// What the agent does when the window starts, and the reason it is not `shutdown`: that one
    /// ends the manager permanently - `scan` refuses afterwards - so an agent that released with
    /// it and then resumed held no devices at all while believing it did.
    ///
    /// Everything the user can feel is restored exactly as on exit: the mouse's native button
    /// mapping, the keyboard's G-keys under firmware control, per-key lighting back to the
    /// onboard effect. That is deliberate rather than an optimisation to skip - between the two
    /// processes the hardware belongs to its own firmware, so a hand-over that skipped it would
    /// leave the keyboard dark for the second or two the other process takes to start.
    pub fn suspend(self: &Arc<Self>) {
        self.hand_back_hardware("handing the devices to the window");
        self.manager.suspend();
    }

    /// Everything both of the above do before they part ways.
    ///
    /// `reason` is what a macro cut short by this reports, which is why it is a parameter rather
    /// than the literal it used to be: a user handing the devices to the window would otherwise
    /// read "shutting down" in the log for something that is not a shutdown.
    fn hand_back_hardware(self: &Arc<Self>, reason: &str) {
        self.animations.stop_all();
        self.runtime.stop_all(reason);

        let diverted: Vec<String> = self
            .diverted_gkeys
            .lock()
            .map(|mut d| std::mem::take(&mut *d).into_iter().collect())
            .unwrap_or_default();
        let spied: Vec<String> = self
            .spy_muted
            .lock()
            .map(|mut m| std::mem::take(&mut *m).into_keys().collect())
            .unwrap_or_default();

        // Leave the mouse usable without this program: every button back to native behaviour.
        for key in spied {
            let outcome = self.manager.with_session(&key, |session| {
                let spy = session.button_spy()?;
                let identity = ghub_hidpp::features::mouse::identity_mapping(spy.button_count);
                Some(spy.set_mapping(&identity).map(|_| ()).and_then(|()| spy.stop()))
            });
            if let Some(Some(Err(e))) = outcome {
                self.log.warning_with(
                    "Mouse",
                    "Could not restore the native button mapping on exit",
                    e.to_string(),
                );
            }
        }

        // And back to onboard mode, so the mouse runs its stored profile again.
        let hosted: Vec<String> = self
            .host_mode_set
            .lock()
            .map(|mut set| std::mem::take(&mut *set).into_iter().collect())
            .unwrap_or_default();
        for key in hosted {
            let outcome = self.manager.with_session(&key, |session| {
                let onboard = session.onboard()?;
                Some(onboard.set_mode(ghub_hidpp::features::onboard::OnboardMode::Onboard))
            });
            if let Some(Some(Err(e))) = outcome {
                self.log.warning_with(
                    "Mouse",
                    "Could not return the mouse to onboard mode on exit",
                    e.to_string(),
                );
            }
        }

        // Leave the keyboard usable without this program: G-keys back to firmware behaviour.
        for key in diverted {
            let outcome = self
                .manager
                .with_session(&key, |s| s.gkeys().map(|g| g.set_software_control(false)));
            if let Some(Some(Err(e))) = outcome {
                self.log.warning_with(
                    "Keyboard",
                    "Could not hand the G-keys back to the firmware on exit",
                    e.to_string(),
                );
            }
        }

        // And never leave the board dark: per-key control goes back to the onboard effect.
        for device in self.manager.devices() {
            let key = device.key();
            let acquired = self
                .manager
                .with_session(&key, |s| s.per_key_lighting().is_some_and(|p| p.acquired))
                .unwrap_or(false);
            if acquired {
                let _ = self
                    .manager
                    .with_session(&key, |session| session.release_per_key_lighting());
            }
        }
        self.ownership.release_all(&native_writer());
    }
}

// ----------------------------------------------------------------- slot names

pub fn gkey_slot(gkey: usize, bank: usize, shift: bool) -> String {
    format!("gkey:G{gkey}:M{bank}{}", if shift { ":shift" } else { "" })
}

pub fn button_slot(button: &str, shift: bool) -> String {
    format!("button:{button}{}", if shift { ":shift" } else { "" })
}

pub fn spy_slot(button: usize, shift: bool) -> String {
    button_slot(&format!("spy{button}"), shift)
}

/// G HUB's factory role for a mouse's DPI buttons, as `(spy button, command)`.
///
/// From G HUB's own `g903_hero` assignment geometry, where input 8 is "DPI Down" and input 9 is
/// "DPI Up". While G HUB runs it mutes both in the 0x8110 mapping (observed as found:
/// `01020304050405000000...`) and steps the DPI table itself. Without the same default here, an
/// unassigned DPI button does whatever the firmware's stored profile says - stepping a DPI list
/// this application neither shows nor manages, or nothing at all in host mode.
pub fn default_spy_dpi_buttons(
    descriptor: Option<&ghub_hidpp::identity::DeviceDescriptor>,
) -> &'static [(usize, DpiCommand)] {
    const G903: &[(usize, DpiCommand)] = &[(8, DpiCommand::Previous), (9, DpiCommand::Next)];
    let Some(descriptor) = descriptor else { return &[] };
    let ids: Vec<u16> =
        std::iter::once(descriptor.product_id).chain(descriptor.wireless_product_id).collect();
    match ghub_hidpp::model::by_model_ids(&ids).map(|model| model.family) {
        Some(ghub_hidpp::model::ModelFamily::G903 | ghub_hidpp::model::ModelFamily::G903Hero) => {
            G903
        }
        _ => &[],
    }
}

/// The DPI stage closest to `dpi`, among enabled stages. Exact matches win; otherwise the nearest
/// value, so a mouse left at 1550 by something else steps from 1600 rather than from stage one.
fn nearest_stage(enabled: &[u32], dpi: u32) -> Option<usize> {
    enabled
        .iter()
        .enumerate()
        .min_by_key(|(_, stage)| stage.abs_diff(dpi))
        .map(|(index, _)| index)
}

fn push_history(stack: &Mutex<Vec<String>>, json: String) {
    if let Ok(mut stack) = stack.lock() {
        stack.push(json);
        while stack.len() > HISTORY_LIMIT {
            stack.remove(0);
        }
    }
}

fn remap_steps(steps: Vec<MacroStep>, map: &HashMap<Id, Id>) -> Vec<MacroStep> {
    steps
        .into_iter()
        .map(|step| match step {
            MacroStep::Action { action: ActionDefinition::MacroRef { macro_id } } => {
                MacroStep::Action {
                    action: ActionDefinition::MacroRef {
                        macro_id: map.get(&macro_id).copied().unwrap_or(macro_id),
                    },
                }
            }
            MacroStep::Condition { condition, value, then, otherwise } => MacroStep::Condition {
                condition,
                value,
                then: remap_steps(then, map),
                otherwise: otherwise.map(|steps| remap_steps(steps, map)),
            },
            other => other,
        })
        .collect()
}

// ----------------------------------------------------------------- ActionHost

/// The service as the macro runtime's host.
///
/// A separate type holding a `Weak` rather than `impl ActionHost for PeripheralService`: the
/// runtime is owned *by* the service, so a strong `Arc<dyn ActionHost>` pointing back at it would
/// be a cycle that never frees.
struct ServiceHost {
    service: std::sync::Weak<PeripheralService>,
}

impl ServiceHost {
    fn service(&self) -> Option<Arc<PeripheralService>> {
        self.service.upgrade()
    }
}

impl ActionHost for ServiceHost {
    fn find_macro(&self, id: Id) -> Option<MacroDefinition> {
        let service = self.service()?;
        service.document().macros.iter().find(|m| m.id == id).cloned()
    }

    fn dpi(&self, command: DpiCommand, stage: usize, pressed: bool) {
        let Some(service) = self.service() else { return };
        service.apply_dpi(command, stage, pressed);
    }

    fn set_gshift(&self, held: bool) {
        if let Some(service) = self.service() {
            service.gshift.store(held, Ordering::SeqCst);
        }
    }

    fn profile(&self, command: ProfileCommand, profile_id: Option<Id>) {
        let Some(service) = self.service() else { return };
        service.apply_profile_command(command, profile_id);
    }

    fn bank(&self, bank: usize) {
        let Some(service) = self.service() else { return };
        for device in service.manager.devices() {
            let Some(mode_keys) = device.capabilities.mode_keys.as_ref() else { continue };
            let max = mode_keys.count.max(1);
            // Bank 0 means "cycle", which is what the G HUB UI's single M-key action does.
            let target = if bank == 0 {
                service.active_bank(&device.key()) % max + 1
            } else {
                bank
            };
            service.set_bank(&device.key(), target);
        }
    }

    fn lighting(&self, command: LightingCommand) {
        let Some(service) = self.service() else { return };
        service.apply_lighting_command(command);
    }

    fn is_user_key_held(&self, virtual_key: u16) -> bool {
        self.service()
            .is_some_and(|service| service.platform.is_key_held(virtual_key))
    }

    fn foreground_executable_name(&self) -> Option<String> {
        let service = self.service()?;
        service
            .platform
            .foreground_app()?
            .executable_name()
            .map(str::to_owned)
    }

    fn launch(&self, target: &str, arguments: Option<&str>) {
        let Some(service) = self.service() else { return };
        if let Err(e) = service.platform.launch(target, arguments) {
            service
                .log
                .warning_with("Actions", format!("Could not launch {target}"), e.clone());
            service.status(format!("Could not launch {target}: {e}"));
        }
    }
}

impl PeripheralService {
    /// Lights the mouse's DPI stage LEDs (the G903's three bars). Firmware drives them from the
    /// onboard-profile current DPI index (0x8100 function 0xC); a bare 0x2201 sensor write leaves
    /// them alone. Setting the index can also load that slot's stored DPI, so callers write the
    /// real DPI afterwards. Best effort: no onboard feature, or a rejected call, keeps the LEDs.
    fn set_dpi_indicator(&self, key: &str, stage: usize) {
        let index = stage.min(ghub_hidpp::features::onboard::PROFILE_DPI_COUNT - 1);
        let outcome = self
            .manager
            .with_session(key, |session| session.onboard().map(|o| o.set_active_dpi_index(index)));
        match outcome {
            Some(Some(Ok(back))) => self.log.info(
                "Mouse",
                format!("DPI indicator: stage {} requested, device index read-back {back}", index + 1),
            ),
            Some(Some(Err(e))) => {
                self.log.warning_with("Mouse", "DPI indicator update failed", e.to_string())
            }
            _ => {}
        }
    }

    fn apply_dpi(self: &Arc<Self>, command: DpiCommand, requested_stage: usize, pressed: bool) {
        for device in self.manager.devices() {
            if !device.connected {
                continue;
            }
            if !device.capabilities.dpi.as_ref().is_some_and(|d| d.meta.is_usable()) {
                continue;
            }
            let key = device.key();
            let profile = self.active_profile();
            let mouse = profile
                .devices
                .get(&key)
                .and_then(|d| d.mouse.clone())
                .unwrap_or_default();
            let enabled: Vec<u32> =
                mouse.dpi_stages.iter().filter(|s| s.enabled).map(|s| s.dpi).collect();
            if enabled.is_empty() {
                continue;
            }
            let has_override = self
                .dpi_stage_override
                .lock()
                .map(|o| o.contains_key(&key))
                .unwrap_or(false);
            let mut stage = if has_override {
                self.current_stage(&key, &mouse)
            } else {
                // First press since the profile applied: step from what the sensor is actually
                // at (read, never assumed), so the first DPI Up after start-up moves one stage
                // from the real value rather than jumping to a stored index.
                self.manager
                    .with_session(&key, |session| session.dpi().map(|dpi| dpi.get()))
                    .flatten()
                    .and_then(Result::ok)
                    .and_then(|state| nearest_stage(&enabled, state.current))
                    .unwrap_or_else(|| self.current_stage(&key, &mouse))
            };
            stage = stage.min(enabled.len() - 1);
            let mut target: Option<u32> = None;

            match command {
                DpiCommand::Shift => {
                    if pressed {
                        if let Some(shift) = mouse.shift_dpi {
                            if let Ok(mut held) = self.dpi_shift_held.lock() {
                                held.insert(key.clone());
                            }
                            target = Some(shift);
                        }
                    } else {
                        let was_held = self
                            .dpi_shift_held
                            .lock()
                            .map(|mut held| held.remove(&key))
                            .unwrap_or(false);
                        if was_held {
                            // Restore the stage that was active before the shift, not the
                            // profile's stored one: a stage change during the shift must stick.
                            target = Some(enabled[stage.min(enabled.len() - 1)]);
                        }
                    }
                }
                DpiCommand::Next if pressed => stage = (stage + 1).min(enabled.len() - 1),
                DpiCommand::Previous if pressed => stage = stage.saturating_sub(1),
                DpiCommand::Cycle if pressed => stage = (stage + 1) % enabled.len(),
                DpiCommand::Default if pressed => {
                    stage = mouse.default_stage.min(enabled.len() - 1);
                }
                DpiCommand::Stage if pressed => stage = requested_stage.min(enabled.len() - 1),
                _ => continue,
            }

            if command != DpiCommand::Shift {
                if let Ok(mut overrides) = self.dpi_stage_override.lock() {
                    overrides.insert(key.clone(), stage);
                }
                let shifting = self
                    .dpi_shift_held
                    .lock()
                    .map(|held| held.contains(&key))
                    .unwrap_or(false);
                if !shifting {
                    target = Some(enabled[stage]);
                }
            }

            let Some(wanted) = target else { continue };
            let clamped = device
                .capabilities
                .dpi
                .as_ref()
                .map(|c| c.clamp(wanted))
                .unwrap_or(wanted);
            if command != DpiCommand::Shift {
                self.set_dpi_indicator(&key, stage);
            }
            let outcome = self
                .manager
                .with_session(&key, |session| session.dpi().map(|dpi| dpi.set(clamped)));
            match outcome {
                Some(Some(Ok(read_back))) => {
                    self.log.info(
                        device.display_name(),
                        format!("DPI {command:?}: stage {} set {clamped}, read-back {read_back}", stage + 1),
                    );
                    self.fire_trigger(
                        TriggerEvent::DpiChanged,
                        device.display_name(),
                        read_back as i32,
                    );
                    if mouse.show_dpi_notification {
                        self.status(if command == DpiCommand::Shift {
                            format!("DPI shift {read_back}")
                        } else {
                            format!("DPI stage {}: {read_back}", stage + 1)
                        });
                    }
                }
                Some(Some(Err(e))) => {
                    self.log.warning_with("Mouse", "DPI change failed", e.to_string());
                }
                _ => {}
            }
        }
    }

    fn apply_profile_command(self: &Arc<Self>, command: ProfileCommand, profile_id: Option<Id>) {
        let document = self.document();
        let mut ordered: Vec<&PeripheralProfile> = document.profiles.iter().collect();
        ordered.sort_by_key(|p| (p.order, p.id));
        if ordered.is_empty() {
            return;
        }
        let active_id = self.active_profile().id;
        let index = ordered.iter().position(|p| p.id == active_id).unwrap_or(0);
        let outcome = match command {
            ProfileCommand::Next => {
                self.set_lock(Some(ordered[(index + 1) % ordered.len()].id))
            }
            ProfileCommand::Previous => self.set_lock(Some(
                ordered[(index + ordered.len() - 1) % ordered.len()].id,
            )),
            ProfileCommand::Select => match profile_id {
                Some(id) => self.set_lock(Some(id)),
                None => Ok(()),
            },
            ProfileCommand::ToggleLock => {
                let locked = document.locked_profile_id.is_some();
                self.set_lock(if locked { None } else { Some(active_id) })
            }
        };
        if let Err(e) = outcome {
            self.log
                .warning_with("Profiles", "The profile change could not be saved", e.to_string());
        }
    }

    fn apply_lighting_command(self: &Arc<Self>, command: LightingCommand) {
        for device in self.manager.devices() {
            if !device.connected {
                continue;
            }
            let key = device.key();
            let outcome = self.manager.with_session(&key, |session| {
                let brightness = session.brightness()?;
                let maximum = brightness.maximum;
                let now = match brightness.get() {
                    Ok(now) => now,
                    Err(e) => return Some(Err(e)),
                };
                // A tenth of the device's own range per step, never zero: a maximum of 4 would
                // otherwise make every step a no-op.
                let step = (maximum / 10).max(1);
                let target = match command {
                    LightingCommand::BrightnessUp => now.saturating_add(step).min(maximum),
                    LightingCommand::BrightnessDown => now.saturating_sub(step),
                    LightingCommand::ToggleOff => {
                        if now == 0 {
                            maximum
                        } else {
                            0
                        }
                    }
                    // G HUB's levels: 100, 75, 50, 25, 0 %, then back to full.
                    LightingCommand::BrightnessCycle => cycle_brightness(now, maximum),
                };
                Some(brightness.set(target).map(|_| ()))
            });
            if let Some(Some(Err(e))) = outcome {
                self.log
                    .warning_with("Lighting", "Brightness change failed", e.to_string());
            }
        }
    }
}

// ----------------------------------------------------------------- HookSink

impl HookSink for PeripheralService {
    fn on_keyboard_key(&self, virtual_key: u16, down: bool, extended: bool) {
        // The hook sink is held as an `Arc<dyn HookSink>`, so the `Arc<Self>` the routing needs
        // is reconstructed here rather than threaded through the trait.
        let service = unsafe_self_arc(self);
        service.on_keyboard_key(virtual_key, down, extended);
    }

    fn is_game_mode_blocked(&self, virtual_key: u16) -> bool {
        PeripheralService::is_game_mode_blocked(self, virtual_key)
    }

    fn primary_mouse_key(&self) -> Option<String> {
        PeripheralService::primary_mouse_key(self)
    }

    fn on_mouse_button(&self, device_key: &str, button: MouseButtonKind, down: bool) -> bool {
        let service = unsafe_self_arc(self);
        service.on_mouse_button(device_key, button, down)
    }

    fn on_wheel(&self, delta: i16) {
        if self.record_state() == MacroRecordState::Recording {
            if let Ok(mut recorder) = self.recorder.lock() {
                recorder.on_wheel(i32::from(delta), false);
            }
        }
    }

    fn on_emergency_stop(&self) {
        self.runtime.stop_all("emergency stop (Ctrl+Shift+Pause)");
        self.status("All macros stopped (Ctrl+Shift+Pause)");
    }

    fn on_foreground_changed(&self, app: Option<ForegroundApp>) {
        let service = unsafe_self_arc(self);
        service.on_foreground_changed(app);
    }

    fn on_hooks_installed(&self, status: HookStatus) {
        if let Ok(mut slot) = self.hook_status.lock() {
            *slot = status;
        }
        if status.all_installed() {
            self.log.info("Input", status.describe());
        } else {
            self.log.warning("Input", status.describe());
        }
    }
}

/// Rebuilds the `Arc<PeripheralService>` from a `&PeripheralService` that is known to live inside
/// one.
///
/// Needed because `HookSink` takes `&self` while the routing methods need `Arc<Self>` (they spawn
/// threads that outlive the call). The service is always reached through an `Arc` - `new` returns
/// one and there is no other constructor - so incrementing the count from the pointer is sound.
/// The cleaner alternative, making every `HookSink` method take `Arc<Self>`, would push the same
/// requirement onto every implementor including the test doubles.
fn unsafe_self_arc(service: &PeripheralService) -> Arc<PeripheralService> {
    // SAFETY: `service` points at the inside of an `Arc<PeripheralService>` (the only way to
    // obtain one is `PeripheralService::new`, which returns `Arc<Self>`). `from_raw` would take
    // ownership of a count we do not hold, so the reconstructed `Arc` is wrapped in `ManuallyDrop`
    // and cloned: the clone owns a fresh count and the borrowed one is never dropped.
    let borrowed = std::mem::ManuallyDrop::new(unsafe { Arc::from_raw(service as *const _) });
    Arc::clone(&borrowed)
}


/// The next of G HUB's brightness levels below `now`, wrapping from off to full.
fn cycle_brightness(now: u16, maximum: u16) -> u16 {
    let levels = [maximum, maximum * 3 / 4, maximum / 2, maximum / 4, 0];
    levels.iter().copied().find(|level| *level < now).unwrap_or(maximum)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::ManagerOptions;
    use crate::profile::{DpiStage, KeyboardSettings};
    use ghub_hidpp::features::test_support::replay_backend;
    use ghub_hidpp::provider::{LogitechHidppProvider, ProviderOptions};

    struct TestPlatform {
        foreground: Mutex<Option<ForegroundApp>>,
        held: Mutex<Vec<u16>>,
        launched: Mutex<Vec<String>>,
    }

    impl TestPlatform {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                foreground: Mutex::new(None),
                held: Mutex::new(Vec::new()),
                launched: Mutex::new(Vec::new()),
            })
        }
    }

    impl PlatformServices for TestPlatform {
        fn foreground_app(&self) -> Option<ForegroundApp> {
            self.foreground.lock().unwrap().clone()
        }
        fn is_key_held(&self, virtual_key: u16) -> bool {
            self.held.lock().unwrap().contains(&virtual_key)
        }
        fn launch(&self, target: &str, _arguments: Option<&str>) -> Result<(), String> {
            self.launched.lock().unwrap().push(target.to_owned());
            Ok(())
        }
    }

    #[derive(Default)]
    struct NullInjector;

    impl InputInjector for NullInjector {
        fn key_down(&self, _virtual_key: u16) {}
        fn key_up(&self, _virtual_key: u16) {}
        fn mouse_down(&self, _button: MouseButtonKind) {}
        fn mouse_up(&self, _button: MouseButtonKind) {}
        fn mouse_move(&self, _dx: i32, _dy: i32) {}
        fn wheel(&self, _delta: i32, _horizontal: bool) {}
        fn type_text(&self, _text: &str) {}
    }

    struct Harness {
        service: Arc<PeripheralService>,
        platform: Arc<TestPlatform>,
        _directory: tempdir::TempDir,
    }

    fn lighting_rule(name: &str, executable: &str, priority: i32) -> AppLightingRule {
        AppLightingRule {
            id: Id::new(),
            name: name.to_owned(),
            executable_name: executable.to_owned(),
            enabled: true,
            priority,
            devices: HashMap::new(),
        }
    }

    fn harness() -> Harness {
        let directory = tempdir::TempDir::new();
        let store = ProfileStore::new(directory.path(), 5).expect("open the store");
        let backend = Arc::new(replay_backend(0xC33F, &[]));
        let provider = Arc::new(LogitechHidppProvider::new(backend, ProviderOptions::default()));
        let log = Arc::new(DiagnosticsLog::default());
        let manager =
            DeviceManager::new(provider, Arc::clone(&log), ManagerOptions::default());
        let platform = TestPlatform::new();
        let service = PeripheralService::new(
            store,
            manager,
            Arc::new(NullInjector),
            Arc::clone(&platform) as Arc<dyn PlatformServices>,
            log,
            Arc::new(LightingOwnership::new()),
        );
        Harness { service, platform, _directory: directory }
    }

    /// A throwaway directory. `tempfile` is a dependency this crate does not otherwise need, and
    /// the store's own tests already use this shape.
    mod tempdir {
        pub struct TempDir(std::path::PathBuf);

        impl TempDir {
            #[allow(clippy::new_without_default)]
            pub fn new() -> Self {
                let base = std::env::temp_dir().join(format!(
                    "ghub-service-{}-{}",
                    std::process::id(),
                    crate::id::Id::new()
                ));
                std::fs::create_dir_all(&base).expect("temp dir");
                Self(base)
            }

            pub fn path(&self) -> &std::path::Path {
                &self.0
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn a_fresh_service_starts_on_the_default_profile() {
        let h = harness();
        let document = h.service.document();
        assert!(!document.profiles.is_empty());
        assert_eq!(
            h.service.active_profile().id,
            document.default_profile().expect("a document always has one").id
        );
        assert!(!h.service.can_undo());
        assert!(!h.service.can_redo());
    }

    #[test]
    fn slot_names_match_the_csharp_exactly() {
        // These strings are persisted in profile files, so a change here silently unbinds
        // everything a user has saved.
        assert_eq!(gkey_slot(3, 2, false), "gkey:G3:M2");
        assert_eq!(gkey_slot(3, 2, true), "gkey:G3:M2:shift");
        assert_eq!(button_slot("Left", false), "button:Left");
        assert_eq!(button_slot("cid00C3", true), "button:cid00C3:shift");
        assert_eq!(spy_slot(7, false), "button:spy7");
        assert_eq!(spy_slot(7, true), "button:spy7:shift");
    }

    #[test]
    fn an_edit_is_saved_published_and_undoable() {
        let h = harness();
        let before = h.service.document().profiles.len();
        h.service
            .update_document(|d| {
                let mut profile = PeripheralProfile::new("Second");
                profile.order = 1;
                d.profiles.push(profile);
            })
            .expect("the edit should save");
        assert_eq!(h.service.document().profiles.len(), before + 1);
        assert!(h.service.can_undo());

        assert!(h.service.undo());
        assert_eq!(h.service.document().profiles.len(), before);
        assert!(h.service.can_redo());
        assert!(h.service.redo());
        assert_eq!(h.service.document().profiles.len(), before + 1);
    }

    #[test]
    fn a_rejected_edit_leaves_the_live_document_untouched() {
        let h = harness();
        let before = h.service.document();
        let result = h.service.update_document(|d| {
            // An empty profile list is invalid: the store must refuse it.
            d.profiles.clear();
        });
        assert!(result.is_err(), "validation must reject an empty profile list");
        assert_eq!(h.service.document().profiles.len(), before.profiles.len());
        assert!(!h.service.can_undo(), "a refused edit must not land in the history");
    }

    #[test]
    fn the_history_is_bounded() {
        let h = harness();
        for i in 0..HISTORY_LIMIT + 10 {
            h.service
                .update_document(|d| {
                    d.app.low_battery_threshold_percent = (i % 90) as u8 + 1;
                })
                .expect("save");
        }
        assert_eq!(h.service.undo.lock().unwrap().len(), HISTORY_LIMIT);
    }

    #[test]
    fn gshift_selects_the_shifted_binding_and_falls_back_when_it_is_missing() {
        let h = harness();
        let profile_id = h.service.active_profile().id;
        h.service
            .update_document(|d| {
                let profile = d.profiles.iter_mut().find(|p| p.id == profile_id).unwrap();
                let settings = profile.for_device("dev");
                settings.bindings.insert(
                    "gkey:G1:M1".to_owned(),
                    ActionBinding::new(ActionDefinition::Key(crate::action::KeyStroke::new(0x41))),
                );
                settings.bindings.insert(
                    "gkey:G1:M1:shift".to_owned(),
                    ActionBinding::new(ActionDefinition::Key(crate::action::KeyStroke::new(0x42))),
                );
                settings.bindings.insert(
                    "gkey:G2:M1".to_owned(),
                    ActionBinding::new(ActionDefinition::Key(crate::action::KeyStroke::new(0x43))),
                );
            })
            .expect("save");

        let base = h.service.find_binding("dev", "gkey:G1:M1").unwrap();
        assert!(matches!(base.action, ActionDefinition::Key(stroke) if stroke.virtual_key == 0x41));

        h.service.gshift.store(true, Ordering::SeqCst);
        let shifted = h.service.find_binding("dev", "gkey:G1:M1").unwrap();
        assert!(
            matches!(shifted.action, ActionDefinition::Key(stroke) if stroke.virtual_key == 0x42)
        );
        // G2 has no shifted binding, so the base layer must answer rather than nothing.
        let fallback = h.service.find_binding("dev", "gkey:G2:M1").unwrap();
        assert!(
            matches!(fallback.action, ActionDefinition::Key(stroke) if stroke.virtual_key == 0x43)
        );
    }

    #[test]
    fn the_primary_click_is_only_remapped_when_another_control_restores_it() {
        let h = harness();
        let profile_id = h.service.active_profile().id;
        h.service
            .update_document(|d| {
                let profile = d.profiles.iter_mut().find(|p| p.id == profile_id).unwrap();
                profile.for_device("mouse").bindings.insert(
                    button_slot("Left", false),
                    ActionBinding::new(ActionDefinition::Key(crate::action::KeyStroke::new(0x41))),
                );
            })
            .expect("save");

        assert!(
            !h.service.has_primary_click_recovery("mouse"),
            "nothing else produces a left click yet"
        );
        assert!(
            !h.service.on_mouse_button("mouse", MouseButtonKind::Left, true),
            "the press must pass through rather than leaving the user unable to click"
        );

        h.service
            .update_document(|d| {
                let profile = d.profiles.iter_mut().find(|p| p.id == profile_id).unwrap();
                profile.for_device("mouse").bindings.insert(
                    button_slot("XButton1", false),
                    ActionBinding::new(ActionDefinition::MouseButton { button: MouseButtonKind::Left }),
                );
            })
            .expect("save");
        assert!(h.service.has_primary_click_recovery("mouse"));
        assert!(
            h.service.on_mouse_button("mouse", MouseButtonKind::Left, true),
            "with a recovery binding the remap is allowed"
        );
        // The release is always swallowed and always routed, so the press cannot strand.
        assert!(h.service.on_mouse_button("mouse", MouseButtonKind::Left, false));
    }

    #[test]
    fn a_pass_through_binding_does_not_suppress_the_button() {
        let h = harness();
        let profile_id = h.service.active_profile().id;
        h.service
            .update_document(|d| {
                let profile = d.profiles.iter_mut().find(|p| p.id == profile_id).unwrap();
                profile.for_device("mouse").bindings.insert(
                    button_slot("Middle", false),
                    ActionBinding::new(ActionDefinition::PassThrough),
                );
            })
            .expect("save");
        assert!(!h.service.on_mouse_button("mouse", MouseButtonKind::Middle, true));
    }

    #[test]
    fn game_mode_blocks_only_the_listed_usages_of_an_enabled_keyboard() {
        let h = harness();
        let profile_id = h.service.active_profile().id;
        h.service
            .update_document(|d| {
                let profile = d.profiles.iter_mut().find(|p| p.id == profile_id).unwrap();
                let settings = profile.for_device("kb");
                settings.keyboard = Some(KeyboardSettings {
                    game_mode_enabled: true,
                    // 0x0B is HID usage for "h"; 0x29 is Escape.
                    game_mode_disabled_usages: vec![0x0B],
                    ..Default::default()
                });
            })
            .expect("save");

        let h_key = crate::action::virtual_key_from_hid_usage(0x0B).expect("usage 0x0B maps");
        assert!(h.service.is_game_mode_blocked(h_key));
        assert!(!h.service.is_game_mode_blocked(0x41), "an unlisted key is never blocked");

        h.service
            .update_document(|d| {
                let profile = d.profiles.iter_mut().find(|p| p.id == profile_id).unwrap();
                profile.for_device("kb").keyboard.as_mut().unwrap().game_mode_enabled = false;
            })
            .expect("save");
        assert!(
            !h.service.is_game_mode_blocked(h_key),
            "the list must only apply while game mode is on"
        );
    }

    #[test]
    fn injected_input_is_never_routed_back_through_the_hooks() {
        let h = harness();
        let service = Arc::clone(&h.service);
        // Inside an injection scope, both hook entries must be inert, or a macro that presses a
        // bound key re-triggers itself.
        crate::injection::during_injection(|| {
            assert!(!service.on_mouse_button("mouse", MouseButtonKind::Left, true));
            assert!(!service.is_game_mode_blocked(0x41));
            service.on_keyboard_key(0x41, true, false);
        });
        assert!(!h.service.saw_keyboard_hook.load(Ordering::SeqCst));
    }

    #[test]
    fn spy_buttons_below_six_are_never_diverted() {
        let mut settings = DeviceProfileSettings::default();
        for button in 1..=8 {
            settings.bindings.insert(
                spy_slot(button, false),
                ActionBinding::new(ActionDefinition::Key(crate::action::KeyStroke::new(0x41))),
            );
        }
        let bound = PeripheralService::bound_spy_buttons(Some(&settings), 8, &[]);
        assert_eq!(bound, vec![6, 7, 8], "1-5 reach Windows as ordinary mouse buttons");
    }

    #[test]
    fn a_pass_through_spy_binding_does_not_count_as_bound() {
        let mut settings = DeviceProfileSettings::default();
        settings.bindings.insert(
            spy_slot(6, false),
            ActionBinding::new(ActionDefinition::PassThrough),
        );
        settings.bindings.insert(
            spy_slot(7, true),
            ActionBinding::new(ActionDefinition::Key(crate::action::KeyStroke::new(0x41))),
        );
        // 6 is pass-through, so only 7 - bound on the shift layer - is diverted.
        assert_eq!(PeripheralService::bound_spy_buttons(Some(&settings), 8, &[]), vec![7]);
        assert!(PeripheralService::bound_spy_buttons(None, 8, &[]).is_empty());
    }

    #[test]
    fn the_spy_mapping_is_bounded_by_the_devices_button_count() {
        let mut settings = DeviceProfileSettings::default();
        for button in 6..=20 {
            settings.bindings.insert(
                spy_slot(button, false),
                ActionBinding::new(ActionDefinition::Key(crate::action::KeyStroke::new(0x41))),
            );
        }
        assert_eq!(PeripheralService::bound_spy_buttons(Some(&settings), 7, &[]), vec![6, 7]);
    }

    #[test]
    fn the_g903_dpi_buttons_are_handled_by_default_like_g_hub() {
        let defaults = &[(8, DpiCommand::Previous), (9, DpiCommand::Next)];
        // No profile settings at all - the state of a fresh install - still handles them.
        assert_eq!(PeripheralService::bound_spy_buttons(None, 12, defaults), vec![8, 9]);

        // Assigning pass-through hands a button back to the firmware.
        let mut settings = DeviceProfileSettings::default();
        settings
            .bindings
            .insert(spy_slot(9, false), ActionBinding::new(ActionDefinition::PassThrough));
        assert_eq!(PeripheralService::bound_spy_buttons(Some(&settings), 12, defaults), vec![8]);

        // A device with too few buttons never gets a default past its count.
        assert!(PeripheralService::bound_spy_buttons(None, 7, defaults).is_empty());
    }

    #[test]
    fn the_g903_hero_is_given_g_hubs_dpi_button_roles() {
        let mut descriptor = ghub_hidpp::identity::DeviceDescriptor::new(
            ghub_hidpp::identity::DeviceIdentity::new("p", "s"),
            "G903",
        );
        // Receiver-attached: the USB id is the receiver's, the wireless one is the mouse's.
        descriptor.product_id = 0xC539;
        descriptor.wireless_product_id = Some(0x4087);
        assert_eq!(
            default_spy_dpi_buttons(Some(&descriptor)),
            &[(8, DpiCommand::Previous), (9, DpiCommand::Next)]
        );
        descriptor.wireless_product_id = None;
        descriptor.product_id = 0xC33F; // G815
        assert!(default_spy_dpi_buttons(Some(&descriptor)).is_empty());
        assert!(default_spy_dpi_buttons(None).is_empty());
    }

    #[test]
    fn the_first_dpi_press_steps_from_the_nearest_stage() {
        let stages = [800, 1600, 3200];
        assert_eq!(nearest_stage(&stages, 1600), Some(1));
        assert_eq!(nearest_stage(&stages, 1550), Some(1));
        assert_eq!(nearest_stage(&stages, 100), Some(0));
        assert_eq!(nearest_stage(&stages, 25600), Some(2));
        assert_eq!(nearest_stage(&[], 800), None);
    }

    #[test]
    fn the_static_frame_uses_per_key_colours_over_the_default() {
        let h = harness();
        let mut lighting = LightingSettings {
            default_color: "#102030".to_owned(),
            brightness: 1.0,
            ..Default::default()
        };
        let first = layout::keys().iter().find(|k| k.led_id != 0).unwrap();
        lighting
            .static_colors
            .insert(format!("key:{}", first.id), "#FF0000".to_owned());

        let frame = h.service.static_frame(&lighting);
        assert_eq!(frame[&first.led_id], LedColor { r: 255, g: 0, b: 0 });
        let other = layout::keys()
            .iter()
            .find(|k| k.led_id != 0 && k.id != first.id)
            .unwrap();
        assert_eq!(frame[&other.led_id], LedColor { r: 0x10, g: 0x20, b: 0x30 });
        assert_eq!(frame.len(), layout::keys().iter().filter(|k| k.led_id != 0).count());
    }

    #[test]
    fn brightness_scales_the_whole_static_frame() {
        let h = harness();
        let lighting = LightingSettings {
            default_color: "#FFFFFF".to_owned(),
            brightness: 0.5,
            ..Default::default()
        };
        let frame = h.service.static_frame(&lighting);
        assert!(frame.values().all(|c| c.r > 100 && c.r < 160), "expected a halved white");
    }

    #[test]
    fn an_unparseable_colour_becomes_white_rather_than_black() {
        let h = harness();
        let lighting = LightingSettings {
            // A key that goes dark is indistinguishable from a broken LED; white is visibly wrong.
            default_color: "not a colour".to_owned(),
            brightness: 1.0,
            ..Default::default()
        };
        let frame = h.service.static_frame(&lighting);
        assert!(frame.values().all(|c| *c == LedColor { r: 255, g: 255, b: 255 }));
    }

    #[test]
    fn an_app_lighting_rule_merges_over_the_profiles_colours() {
        let h = harness();
        let profile_id = h.service.active_profile().id;
        let mut baseline = LightingSettings {
            default_color: "#111111".to_owned(),
            brightness: 1.0,
            ..Default::default()
        };
        baseline.static_colors.insert("key:Q".to_owned(), "#222222".to_owned());
        baseline.static_colors.insert("key:W".to_owned(), "#333333".to_owned());

        h.service
            .update_document(|d| {
                let profile = d.profiles.iter_mut().find(|p| p.id == profile_id).unwrap();
                profile.for_device("kb").lighting = baseline.clone();
                let mut over = AppLightingOverride {
                    brightness: Some(0.5),
                    ..Default::default()
                };
                over.static_colors.insert("key:W".to_owned(), "#FF0000".to_owned());
                let mut rule = lighting_rule("Game", "game.exe", 0);
                rule.devices.insert("kb".to_owned(), over);
                d.app_lighting.push(rule);
            })
            .expect("save");

        *h.platform.foreground.lock().unwrap() =
            Some(ForegroundApp::new("C:\\games\\game.exe", "Game"));
        h.service.reevaluate(true, None);

        let profile = h.service.active_profile();
        let effective = h.service.effective_lighting(
            "kb",
            profile.devices.get("kb").map(|d| &d.lighting),
            None,
        );
        assert_eq!(effective.brightness, 0.5, "the rule's brightness wins");
        assert_eq!(effective.default_color, "#111111", "unset fields keep the profile's value");
        assert_eq!(
            effective.static_colors.get("key:W").map(String::as_str),
            Some("#FF0000"),
            "the rule recolours the keys it names"
        );
        assert_eq!(
            effective.static_colors.get("key:Q").map(String::as_str),
            Some("#222222"),
            "and leaves the rest of the layout alone"
        );
    }

    #[test]
    fn a_lighting_rule_for_another_program_does_not_match() {
        let h = harness();
        let profile_id = h.service.active_profile().id;
        h.service
            .update_document(|d| {
                let profile = d.profiles.iter_mut().find(|p| p.id == profile_id).unwrap();
                profile.for_device("kb").lighting = LightingSettings::default();
                let mut rule = lighting_rule("Game", "game.exe", 0);
                rule.devices.insert("kb".to_owned(), AppLightingOverride::default());
                d.app_lighting.push(rule);
            })
            .expect("save");
        *h.platform.foreground.lock().unwrap() =
            Some(ForegroundApp::new("C:\\other\\editor.exe", "Editor"));
        h.service.reevaluate(true, None);
        assert!(h.service.matching_app_lighting("kb").is_none());
    }

    #[test]
    fn the_highest_priority_matching_rule_wins_deterministically() {
        let h = harness();
        h.service
            .update_document(|d| {
                for (name, priority, color) in
                    [("low", 1, "#010101"), ("high", 9, "#090909"), ("mid", 5, "#050505")]
                {
                    let mut rule = lighting_rule(name, "game.exe", priority);
                    let over = AppLightingOverride {
                        default_color: Some(color.to_owned()),
                        ..Default::default()
                    };
                    rule.devices.insert("kb".to_owned(), over);
                    d.app_lighting.push(rule);
                }
            })
            .expect("save");
        *h.platform.foreground.lock().unwrap() =
            Some(ForegroundApp::new("C:\\games\\GAME.EXE", "Game"));
        h.service.reevaluate(true, None);
        let (rule, over) = h.service.matching_app_lighting("kb").expect("a rule must match");
        assert_eq!(rule.name, "high");
        assert_eq!(over.default_color.as_deref(), Some("#090909"));
    }

    #[test]
    fn importing_without_replace_remaps_colliding_macro_ids_including_references() {
        let h = harness();
        let shared = Id::new();
        let mut inner = MacroDefinition::new("inner");
        inner.id = shared;
        inner.steps = vec![MacroStep::KeyDown { virtual_key: 0x41 }];
        let mut outer = MacroDefinition::new("outer");
        outer.steps = vec![MacroStep::Action {
            action: ActionDefinition::MacroRef { macro_id: shared },
        }];

        // The local document already owns `shared`, so the import must not reuse it.
        h.service
            .update_document(|d| {
                let mut local = MacroDefinition::new("local");
                local.id = shared;
                d.macros.push(local);
            })
            .expect("save");

        let mut incoming = ProfileDocument::create_default();
        incoming.macros = vec![inner, outer];
        let json = serde_json::to_string(&incoming).unwrap();
        h.service.import_json(&json, false).expect("the import should succeed");

        let document = h.service.document();
        let names: Vec<&str> = document.macros.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"local") && names.contains(&"inner") && names.contains(&"outer"));
        let imported_inner = document.macros.iter().find(|m| m.name == "inner").unwrap();
        assert_ne!(imported_inner.id, shared, "the colliding id must be regenerated");
        let imported_outer = document.macros.iter().find(|m| m.name == "outer").unwrap();
        match &imported_outer.steps[0] {
            MacroStep::Action { action: ActionDefinition::MacroRef { macro_id } } => {
                assert_eq!(
                    *macro_id, imported_inner.id,
                    "the reference must follow the remap, not point at the local macro"
                );
            }
            other => panic!("expected a macro reference, got {other:?}"),
        }
    }

    #[test]
    fn importing_with_replace_drops_the_lock() {
        let h = harness();
        let locked = h.service.active_profile().id;
        h.service.set_lock(Some(locked)).expect("save");
        assert!(h.service.document().locked_profile_id.is_some());

        let incoming = ProfileDocument::create_default();
        let json = serde_json::to_string(&incoming).unwrap();
        h.service.import_json(&json, true).expect("import");
        assert!(
            h.service.document().locked_profile_id.is_none(),
            "a replaced document must not keep a lock pointing at a profile that is gone"
        );
    }

    #[test]
    fn exported_json_round_trips_through_import() {
        let h = harness();
        let exported = h.service.export_json();
        assert!(crate::store::parse(&exported).is_ok(), "our own export must be importable");
    }

    #[test]
    fn the_bank_is_clamped_to_what_the_device_reports() {
        let h = harness();
        // No device is connected, so the fallback maximum of three applies.
        h.service.set_bank("kb", 0);
        h.service.set_bank("kb", 99);
        assert_eq!(h.service.active_bank("kb"), 1, "out-of-range banks are ignored");
        h.service.set_bank("kb", 2);
        assert_eq!(h.service.active_bank("kb"), 2);
    }

    #[test]
    fn a_bank_change_persists_without_entering_the_undo_history() {
        let h = harness();
        h.service.set_bank("kb", 3);
        assert_eq!(h.service.active_bank("kb"), 3);
        assert!(
            !h.service.can_undo(),
            "pressing an M-key is not an edit the user would want to undo"
        );
    }

    #[test]
    fn recording_binds_the_captured_macro_to_the_target_gkey() {
        let h = harness();
        h.service.start_recording_from_ui("kb", 4);
        assert_eq!(h.service.record_state(), MacroRecordState::Recording);
        assert_eq!(h.service.recording_target().unwrap().gkey, 4);

        h.service.on_keyboard_key(0x41, true, false);
        h.service.on_keyboard_key(0x41, false, false);
        let macro_id = h.service.finish_recording().expect("a macro should be saved");

        assert_eq!(h.service.record_state(), MacroRecordState::Idle);
        let document = h.service.document();
        assert!(document.macros.iter().any(|m| m.id == macro_id));
        let bound = document
            .profiles
            .iter()
            .find(|p| p.id == h.service.active_profile().id)
            .and_then(|p| p.devices.get("kb"))
            .and_then(|d| d.bindings.get(&gkey_slot(4, 1, false)))
            .expect("the G-key must be bound");
        assert!(matches!(bound.action, ActionDefinition::MacroRef { macro_id: id } if id == macro_id));
    }

    #[test]
    fn an_empty_recording_is_discarded_rather_than_bound() {
        let h = harness();
        let before = h.service.document().macros.len();
        h.service.start_recording_from_ui("kb", 1);
        assert!(h.service.finish_recording().is_none());
        assert_eq!(h.service.document().macros.len(), before);
        assert_eq!(h.service.record_state(), MacroRecordState::Idle);
    }

    #[test]
    fn cancelling_a_recording_throws_it_away() {
        let h = harness();
        let before = h.service.document().macros.len();
        h.service.start_recording_from_ui("kb", 2);
        h.service.on_keyboard_key(0x41, true, false);
        h.service.cancel_recording("test");
        assert_eq!(h.service.record_state(), MacroRecordState::Idle);
        assert!(h.service.recording_target().is_none());
        assert_eq!(h.service.document().macros.len(), before);
    }

    #[test]
    fn cancelling_when_idle_does_nothing() {
        let h = harness();
        let seen = Arc::new(Mutex::new(0usize));
        let counter = Arc::clone(&seen);
        h.service.on_event(move |event| {
            if matches!(event, ServiceEvent::RecordStateChanged) {
                *counter.lock().unwrap() += 1;
            }
        });
        h.service.cancel_recording("nothing to cancel");
        assert_eq!(*seen.lock().unwrap(), 0);
    }

    #[test]
    fn profile_next_and_previous_wrap_in_order() {
        let h = harness();
        let first = h.service.active_profile().id;
        h.service
            .update_document(|d| {
                for (index, name) in ["Second", "Third"].into_iter().enumerate() {
                    let mut profile = PeripheralProfile::new(name);
                    profile.order = index as i32 + 1;
                    d.profiles.push(profile);
                }
            })
            .expect("save");

        h.service.apply_profile_command(ProfileCommand::Next, None);
        assert_eq!(h.service.active_profile().name, "Second");
        h.service.apply_profile_command(ProfileCommand::Next, None);
        assert_eq!(h.service.active_profile().name, "Third");
        h.service.apply_profile_command(ProfileCommand::Next, None);
        assert_eq!(h.service.active_profile().id, first, "next wraps to the first");
        h.service.apply_profile_command(ProfileCommand::Previous, None);
        assert_eq!(h.service.active_profile().name, "Third", "previous wraps backwards");
    }

    #[test]
    fn toggle_lock_locks_then_unlocks_the_active_profile() {
        let h = harness();
        let active = h.service.active_profile().id;
        h.service.apply_profile_command(ProfileCommand::ToggleLock, None);
        assert_eq!(h.service.document().locked_profile_id, Some(active));
        h.service.apply_profile_command(ProfileCommand::ToggleLock, None);
        assert_eq!(h.service.document().locked_profile_id, None);
    }

    /// Saves a macro that sleeps, starts it through the trigger path and waits for it to be
    /// running.
    ///
    /// `play` is deliberately blocking - the editor's "test this macro" button wants to know the
    /// outcome - so a test that needs a macro *still running* has to start it the way a key press
    /// does, through `on_trigger`.
    fn start_long_macro(service: &Arc<PeripheralService>) -> Id {
        let mut long = MacroDefinition::new("long");
        long.steps = vec![MacroStep::Delay { milliseconds: 30_000 }];
        let macro_id = long.id;
        service.update_document(|d| d.macros.push(long)).expect("save");
        service.runtime.on_trigger(
            "test|G1",
            Some(&ActionDefinition::MacroRef { macro_id }),
            true,
        );
        for _ in 0..200 {
            if !service.runtime.running().is_empty() {
                return macro_id;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the macro never started");
    }

    #[test]
    fn a_profile_change_stops_running_macros() {
        let h = harness();
        h.service
            .update_document(|d| {
                let mut profile = PeripheralProfile::new("Second");
                profile.order = 1;
                d.profiles.push(profile);
            })
            .expect("save");
        let second = h
            .service
            .document()
            .profiles
            .iter()
            .find(|p| p.name == "Second")
            .unwrap()
            .id;

        // A long macro, started and then abandoned by the profile switch.
        start_long_macro(&h.service);

        h.service.set_lock(Some(second)).expect("save");
        assert_eq!(h.service.active_profile().id, second);
        assert!(
            h.service.runtime.running().is_empty(),
            "a profile change must not leave a macro running against bindings that are gone"
        );
    }

    #[test]
    fn the_foreground_watcher_ignores_a_repeated_report() {
        let h = harness();
        let seen = Arc::new(Mutex::new(0usize));
        let counter = Arc::clone(&seen);
        h.service.on_event(move |event| {
            if matches!(event, ServiceEvent::ActiveProfileChanged(_)) {
                *counter.lock().unwrap() += 1;
            }
        });
        let app = Some(ForegroundApp::new("C:\\x\\a.exe", "A"));
        h.service.on_foreground_changed(app.clone());
        h.service.on_foreground_changed(app.clone());
        h.service.on_foreground_changed(app);
        assert!(
            *seen.lock().unwrap() <= 1,
            "the same window reported repeatedly must not re-resolve the profile"
        );
    }

    #[test]
    fn a_rule_switches_profile_when_its_program_comes_to_the_foreground() {
        let h = harness();
        h.service
            .update_document(|d| {
                let mut profile = PeripheralProfile::new("Game");
                profile.order = 1;
                profile.rules.push(crate::profile::AppRule::by_executable("game.exe"));
                d.profiles.push(profile);
            })
            .expect("save");

        h.service
            .on_foreground_changed(Some(ForegroundApp::new("C:\\games\\game.exe", "Game")));
        assert_eq!(h.service.active_profile().name, "Game");

        h.service
            .on_foreground_changed(Some(ForegroundApp::new("C:\\other\\editor.exe", "Editor")));
        assert_ne!(h.service.active_profile().name, "Game");
    }

    #[test]
    fn a_manual_lock_beats_a_matching_rule() {
        let h = harness();
        let default_id = h.service.active_profile().id;
        h.service
            .update_document(|d| {
                let mut profile = PeripheralProfile::new("Game");
                profile.order = 1;
                profile.rules.push(crate::profile::AppRule::by_executable("game.exe"));
                d.profiles.push(profile);
            })
            .expect("save");
        h.service.set_lock(Some(default_id)).expect("save");
        h.service
            .on_foreground_changed(Some(ForegroundApp::new("C:\\games\\game.exe", "Game")));
        assert_eq!(
            h.service.active_profile().id, default_id,
            "a lock is the user saying 'stop switching'"
        );
    }

    #[test]
    fn the_dpi_stage_override_is_cleared_by_a_profile_change() {
        let h = harness();
        h.service
            .dpi_stage_override
            .lock()
            .unwrap()
            .insert("mouse".to_owned(), 3);
        h.service
            .update_document(|d| {
                let mut profile = PeripheralProfile::new("Second");
                profile.order = 1;
                d.profiles.push(profile);
            })
            .expect("save");
        let second = h
            .service
            .document()
            .profiles
            .iter()
            .find(|p| p.name == "Second")
            .unwrap()
            .id;
        h.service.set_lock(Some(second)).expect("save");
        assert!(
            h.service.dpi_stage_override.lock().unwrap().is_empty(),
            "a stage chosen by a button belongs to the profile it was pressed in"
        );
    }

    #[test]
    fn the_current_stage_prefers_the_override_over_the_profile() {
        let h = harness();
        let mouse = MouseSettings {
            active_stage: 1,
            dpi_stages: vec![DpiStage::new(400), DpiStage::new(800), DpiStage::new(1600)],
            ..Default::default()
        };
        assert_eq!(h.service.current_stage("mouse", &mouse), 1);
        h.service
            .dpi_stage_override
            .lock()
            .unwrap()
            .insert("mouse".to_owned(), 2);
        assert_eq!(h.service.current_stage("mouse", &mouse), 2);
    }

    #[test]
    fn the_attempt_wrapper_keeps_the_csharp_result_strings() {
        let h = harness();
        assert_eq!(
            h.service.attempt("DPI 1600", || Ok("read-back 1600".to_owned())),
            "DPI 1600: read-back 1600"
        );
        assert_eq!(
            h.service.attempt("DPI 1600", || Err("feature returned 0x05".to_owned())),
            "DPI 1600: FAILED (feature returned 0x05)"
        );
    }

    #[test]
    fn applying_to_a_disconnected_device_does_nothing() {
        let h = harness();
        let device = ManagedDevice::new(
            ghub_hidpp::identity::DeviceIdentity::new("logitech-hidpp", "test"),
            "logitech-hidpp",
        );
        assert!(h.service.apply_to_device(&device).is_empty());
    }

    #[test]
    fn the_emergency_stop_stops_every_macro() {
        let h = harness();
        start_long_macro(&h.service);
        HookSink::on_emergency_stop(h.service.as_ref());
        assert!(h.service.runtime.running().is_empty());
    }

    #[test]
    fn the_hook_status_is_recorded_and_logged() {
        let h = harness();
        HookSink::on_hooks_installed(
            h.service.as_ref(),
            HookStatus { keyboard: true, mouse: true, foreground: true, ..Default::default() },
        );
        assert!(h.service.hook_status().all_installed());
        assert!(h.service.log.to_text().contains("hooks installed"));

        HookSink::on_hooks_installed(h.service.as_ref(), HookStatus { last_error: 5, ..Default::default() });
        assert!(!h.service.hook_status().all_installed());
        assert!(h.service.log.to_text().contains("no input hooks installed"));
    }

    #[test]
    fn the_action_host_reads_macros_and_the_platform() {
        let h = harness();
        let definition = MacroDefinition::new("findable");
        let id = definition.id;
        h.service
            .update_document(|d| d.macros.push(definition))
            .expect("save");

        let host = ServiceHost { service: Arc::downgrade(&h.service) };
        assert_eq!(host.find_macro(id).map(|m| m.name), Some("findable".to_owned()));
        assert!(host.find_macro(Id::new()).is_none());

        h.platform.held.lock().unwrap().push(0xA0);
        assert!(host.is_user_key_held(0xA0));
        assert!(!host.is_user_key_held(0x41));

        *h.platform.foreground.lock().unwrap() =
            Some(ForegroundApp::new("C:\\games\\game.exe", "Game"));
        assert_eq!(host.foreground_executable_name(), Some("game.exe".to_owned()));

        host.launch("notepad.exe", None);
        assert_eq!(h.platform.launched.lock().unwrap().as_slice(), ["notepad.exe"]);
    }

    #[test]
    fn gshift_is_set_and_cleared_through_the_action_host() {
        let h = harness();
        let host = ServiceHost { service: Arc::downgrade(&h.service) };
        assert!(!h.service.gshift_held());
        host.set_gshift(true);
        assert!(h.service.gshift_held());
        host.set_gshift(false);
        assert!(!h.service.gshift_held());
    }

    #[test]
    fn the_action_host_holds_only_a_weak_reference() {
        // A strong reference here would be a cycle: the service owns the runtime, which owns the
        // host. The service would then never be freed and its threads never stop.
        let h = harness();
        let weak = Arc::downgrade(&h.service);
        assert!(Arc::weak_count(&h.service) >= 1);
        let host = ServiceHost { service: weak };
        assert!(host.service().is_some());
    }

    #[test]
    fn rebuilding_the_arc_from_a_reference_does_not_drop_the_service() {
        let h = harness();
        let before = Arc::strong_count(&h.service);
        for _ in 0..100 {
            let rebuilt = unsafe_self_arc(&h.service);
            assert_eq!(rebuilt.active_profile().id, h.service.active_profile().id);
        }
        assert_eq!(
            Arc::strong_count(&h.service),
            before,
            "every rebuilt Arc must be released again"
        );
    }

    #[test]
    fn shutdown_is_safe_with_nothing_connected() {
        let h = harness();
        h.service.shutdown();
        assert!(h.service.manager.is_shutting_down());
    }

    /// A suspend hands the hardware back without ending the manager, which is what lets the agent
    /// give the devices to the window and take them again afterwards. If this ever shuts the
    /// manager down, the agent resumes holding nothing.
    #[test]
    fn suspend_hands_the_hardware_back_without_ending_the_manager() {
        let h = harness();
        h.service.suspend();
        assert!(h.service.manager.is_suspended());
        assert!(!h.service.manager.is_shutting_down(), "a suspend must be undoable");
    }

    #[test]
    fn an_event_subscriber_that_panics_does_not_stop_the_others() {
        let h = harness();
        let reached = Arc::new(AtomicBool::new(false));
        h.service.on_event(|_| panic!("a closing window's handler"));
        let flag = Arc::clone(&reached);
        h.service.on_event(move |_| flag.store(true, Ordering::SeqCst));
        h.service.status("hello");
        assert!(reached.load(Ordering::SeqCst));
    }
}
