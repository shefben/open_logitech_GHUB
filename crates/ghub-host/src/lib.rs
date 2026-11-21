//! The running stack, with no window attached to it.
//!
//! This is the half of the old `ghub-ui/src/host.rs` that has nothing to do with egui: the profile
//! store, the HID++ provider, the device manager, the service, the low-level input hooks, the
//! automation API and the OpenRGB integration, wired together the way Artemis's dependency
//! injection container wired them and handed them to every tab as `IPeripheralsService`.
//!
//! It lives in a crate of its own because two binaries need it and neither may drift from the
//! other:
//!
//! * `ghub-agent.exe` sits resident, owns the hardware and the notification-area icon, and never
//!   links a GUI toolkit.
//! * `ghub.exe` is the window. It builds the same stack while it is open, wraps it in a worker
//!   thread and a frame loop, and exits completely when closed.
//!
//! Only one of them may hold the hardware at a time, which is what [`Stack::release`] and
//! [`Stack::resume`] are for: the agent hands the devices, the hooks and the automation port over
//! when the window starts, and takes them back when it exits. A copy of this file in each binary
//! would mean the agent and the window could disagree about what a document means - which is
//! exactly the class of bug that cannot be found by looking at either one.

mod live;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ghub_core::automation::AutomationServer;
use ghub_core::automation_target::PeripheralAutomationTarget;
use ghub_core::diagnostics::DiagnosticsLog;
use ghub_core::manager::{DeviceManager, ManagerOptions};
use ghub_core::openrgb_service::OpenRgbService;
use ghub_core::ownership::LightingOwnership;
use ghub_core::platform::PlatformServices;
use ghub_core::service::{PeripheralService, ServiceEvent};
use ghub_core::store::ProfileStore;
use ghub_core::{InputInjector, layout};
use ghub_hidpp::hid::HidBackend;
use ghub_hidpp::provider::{LogitechHidppProvider, ProviderOptions};

/// How many documents back the store keeps. Matches the C#'s `ProfileStore` registration.
const BACKUPS_TO_KEEP: usize = 5;

/// Everything below the UI: the service and the pieces it was built from.
///
/// Public fields rather than accessors because that is how the C# handed these round - the tabs
/// held `IPeripheralsService`, `IDeviceManager` and `ILogitechProvider` directly - and because
/// wrapping twenty getters round them would add a layer that does nothing.
pub struct Stack {
    pub service: Arc<PeripheralService>,
    pub devices: Arc<DeviceManager>,
    pub log: Arc<DiagnosticsLog>,
    /// Service events since the last drain: document edits, profile switches, record-state
    /// changes. Callers reload from these rather than polling the document.
    events: Arc<Mutex<Vec<ServiceEvent>>>,
    /// Set when a device connected, disconnected or was re-read.
    ///
    /// A flag rather than a queue: the pages' response to any of
    /// `DeviceConnected`/`DeviceDisconnected`/`DeviceUpdated` was the same single `Refresh()`
    /// call, and Windows raises several arrival events per composite device, so a queue would
    /// just be collapsed back to one on arrival.
    device_change: Arc<AtomicBool>,
    /// The loopback automation API. Always constructed, started only when the document asks for
    /// it: the Automation page has to be able to show its token path and its state whether or not
    /// it is listening.
    pub automation: Arc<AutomationServer>,
    /// The HID++ provider, kept so Options can change the onboard-write policy without a restart.
    pub provider: Arc<LogitechHidppProvider>,
    /// The optional OpenRGB integration. Constructed always and connected only when the document
    /// asks for it, for the same reason as the API: Diagnostics and Options both report its state,
    /// and "Disabled" is a state they have to be able to show.
    pub open_rgb: Arc<OpenRgbService>,
    /// Why the real HID backend could not be opened, when it could not. The diagnostics page
    /// shows it; the C# had no equivalent because Artemis logged it and carried on with no
    /// devices, which looked identical to having none plugged in.
    pub backend_error: Option<String>,
    /// Live low-level input hooks. Dropping this uninstalls them.
    #[cfg(windows)]
    hooks: Option<ghub_platform::HookHost>,
    /// False while the hardware is held by the other process. Separate from the pieces being
    /// absent, because a released stack is still a stack: its document is still readable and its
    /// diagnostics log still works, which is what lets the agent answer a tray click while the
    /// window owns the devices.
    holding: bool,
    /// Whether the device manager has ever been started, which decides whether taking the
    /// hardware calls `start` (first time, which also spawns the health loop) or `resume`.
    started_devices: bool,
}

impl Stack {
    /// Builds the whole stack. Infallible by design, like `PeripheralService::new`: a missing HID
    /// backend, an unwritable profile directory or an absent device all produce a running
    /// application that says what is wrong, because a peripheral manager that refuses to start
    /// cannot be used to diagnose why it refused.
    ///
    /// `hold_hardware` is false for a process that is starting up while the other one still owns
    /// the devices. The stack is built either way - same store, same document, same log - but it
    /// installs no hooks, opens no automation port and runs no first scan until [`Stack::resume`].
    pub fn new(hold_hardware: bool) -> Self {
        let log = Arc::new(DiagnosticsLog::new(ghub_core::diagnostics::DEFAULT_CAPACITY));
        let (store, store_note) = open_store(&log);
        let (backend, backend_error) = open_backend(&log);

        // Per-key LED ids live above the HID crate because the layouts do; the provider needs them
        // to decide which products get per-key lighting rather than zone lighting. The product ids
        // come from the model table rather than being repeated here, so a device added there picks
        // the layout up without a second edit.
        let mut per_key_led_ids = std::collections::HashMap::new();
        for model in ghub_hidpp::model::KNOWN
            .iter()
            .filter(|m| m.family == ghub_hidpp::model::ModelFamily::G815)
        {
            for pid in model.wired_product_ids.iter().chain(model.wireless_product_ids) {
                per_key_led_ids.insert(*pid, layout::led_ids());
            }
        }

        let provider = Arc::new(LogitechHidppProvider::new(
            backend,
            ProviderOptions {
                // Read before write: onboard flash writes stay off here and are turned on below
                // only if the document says the user asked for them. Seeding it from the document
                // rather than from this literal is what makes the Options switch survive a
                // restart.
                allow_onboard_writes: false,
                per_key_led_ids,
                ..Default::default()
            },
        ));
        let devices =
            DeviceManager::new(Arc::clone(&provider), Arc::clone(&log), ManagerOptions::default());
        let ownership = Arc::new(LightingOwnership::new());

        // Built with the same ownership registry the native writer uses, which is the whole point:
        // an OpenRGB client and a profile's own lighting contend for one device rather than each
        // writing over the other.
        let open_rgb = Arc::new(OpenRgbService::new(Arc::clone(&log), Arc::clone(&ownership)));

        let (injector, platform) = host_services();
        let service = PeripheralService::new(
            store,
            Arc::clone(&devices),
            injector,
            platform,
            Arc::clone(&log),
            ownership,
        );

        // Screen Sampler and Audio Visualizer read through this; it captures only while one plays.
        let live = live::LiveCapture::new();
        service
            .animations()
            .set_live_input(Some(Arc::clone(&live) as Arc<dyn ghub_core::effects::LiveInput>));

        if let Some(note) = store_note {
            log.error("Profiles", note);
        }

        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        service.on_event(move |event| {
            if let Ok(mut sink) = sink.lock() {
                sink.push(event.clone());
            }
        });

        let device_change = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&device_change);
        devices.on_change(move |_| flag.store(true, Ordering::SeqCst));

        // Built after the service, because it is a view onto it. Starting it is a separate
        // decision, taken from the document below, so a document with the API switched off never
        // opens a socket.
        let automation = Arc::new(AutomationServer::new(
            Arc::new(PeripheralAutomationTarget::new(Arc::clone(&service))),
            Arc::clone(&log),
            token_path(),
        ));
        // An OpenRGB controller that is one of the devices this application already drives has to
        // arbitrate under *that* device's key, or the two writers would each own a name of their
        // own for one piece of hardware and both would write to it.
        let for_resolver = Arc::clone(&devices);
        open_rgb.set_native_device_key_resolver(move |controller| {
            if !controller.is_logitech() {
                return None;
            }
            for_resolver
                .devices()
                .into_iter()
                .find(|device| {
                    device.descriptor.as_ref().is_some_and(|descriptor| {
                        // The model word, not the whole display name: OpenRGB calls it
                        // "Logitech G815 RGB Mechanical Gaming Keyboard" and the descriptor says
                        // "G815 LIGHTSYNC RGB", so only the first word is common to both.
                        descriptor.display_name.split(' ').next().is_some_and(|model| {
                            !model.is_empty()
                                && controller.name.to_lowercase().contains(&model.to_lowercase())
                        })
                    })
                })
                .map(|device| device.key())
        });

        let mut stack = Self {
            service,
            devices,
            log,
            events,
            device_change,
            automation,
            provider,
            open_rgb,
            backend_error,
            #[cfg(windows)]
            hooks: None,
            holding: false,
            started_devices: false,
        };
        if hold_hardware {
            stack.resume();
        } else {
            // Still reconciled, because the settings that are not about hardware - the two
            // protocol-logging switches and the onboard-write policy - apply to a released stack
            // too, and the diagnostics page of whichever process is released still shows them.
            stack.apply_app_settings();
        }
        stack
    }

    /// Whether this process currently owns the devices, the hooks and the automation port.
    pub fn holding(&self) -> bool {
        self.holding
    }

    /// Takes the hardware: installs the input hooks, starts the device scan, opens the automation
    /// port if the document asks for it, and connects OpenRGB if the document asks for that.
    ///
    /// Idempotent. Called from [`Stack::new`] for a process that starts as the owner, and from the
    /// agent when the window exits.
    pub fn resume(&mut self) {
        if self.holding {
            return;
        }
        self.holding = true;

        // The other process owned the document while it owned the hardware, so this one's copy is
        // whatever was on disk before the window started. Re-read it before anything is applied:
        // an agent that resumed on its stale copy would apply the profile the user had just
        // renamed and would overwrite the rename on its next save. At start-up this re-reads a
        // file that was loaded a moment ago, which costs one read of one JSON document and keeps
        // taking the hardware down a single path.
        for warning in self.service.reload_from_store() {
            self.log.warning("Profiles", warning);
        }

        #[cfg(windows)]
        {
            // The service implements `HookSink`, so the coercion is explicit rather than
            // inferred through `Arc::clone`.
            let sink: Arc<dyn ghub_core::platform::HookSink> =
                Arc::clone(&self.service) as Arc<dyn ghub_core::platform::HookSink>;
            self.hooks = Some(ghub_platform::HookHost::start(sink));
        }

        // Devices are enumerated on a worker thread, not here: a scan on a machine with a
        // Unifying receiver means six ping timeouts before the window appears.
        //
        // `start` the first time and `resume` afterwards: `start` also spawns the health loop, and
        // a second one of those would have two threads polling the same devices. `resume` keeps
        // the loop that is already running and re-scans.
        let for_scan = Arc::clone(&self.devices);
        let first = !self.started_devices;
        self.started_devices = true;
        let _ = std::thread::Builder::new()
            .name("ghub device scan".to_owned())
            .spawn(move || {
                if first {
                    for_scan.start();
                } else {
                    for_scan.resume();
                }
            });

        let app = self.service.document().app.clone();
        if app.automation_api_enabled {
            start_automation(&self.automation, app.automation_api_port, &self.log);
        }

        // Last, and through the shared reconciler rather than by hand, so taking the hardware back
        // and a later document edit cannot disagree about what the document means.
        self.apply_app_settings();
    }

    /// Hands the hardware to the other process: closes every device session, uninstalls the hooks,
    /// stops the automation listener and disconnects OpenRGB.
    ///
    /// Everything that is *state* survives - the store, the document, the log - so the released
    /// process can still answer a tray click and still show its diagnostics. Only the exclusive
    /// resources are given up, and they are given up in the order that leaves the shortest gap:
    /// the listeners first, the devices last, because a device left open is the one thing the
    /// other process cannot work around.
    pub fn release(&mut self) {
        if !self.holding {
            return;
        }
        self.automation.stop();
        self.open_rgb.stop();
        #[cfg(windows)]
        if let Some(mut hooks) = self.hooks.take() {
            hooks.stop();
        }
        // `suspend`, not `shutdown`. It performs the same hand-back the window does when it
        // closes - spy mapping to identity, G-keys to firmware control, per-key lighting to an
        // onboard effect, so the hardware is its firmware's again between the two processes
        // rather than dark - but it leaves the device manager able to take the hardware back.
        // `shutdown` is terminal: it latches a flag that `scan` checks, so a stack that released
        // with it and then resumed would hold no devices at all while reporting that it did.
        self.service.suspend();
        self.holding = false;
    }

    /// Makes the live state match the document's `app` settings.
    ///
    /// `ApplyAppSettings` in the C#, which called it from start-up *and* from every document
    /// change for the same reason it is shared here: these settings can be changed from the
    /// Options page, by a G HUB import, or by restoring a backup, and only one of those three is
    /// a page that could apply them itself.
    ///
    /// The logging flags are pushed into the log rather than read from the document per event: the
    /// HID++ layer checks them on every transaction, and a document read there would put the
    /// profile store on the device loop's hot path.
    ///
    /// Idempotent, and deliberately cheap when nothing moved - in particular OpenRGB is only
    /// started or stopped when the document and the running state actually disagree, because
    /// restarting it would drop a working connection on every unrelated profile edit.
    pub fn apply_app_settings(&self) {
        let app = self.service.document().app.clone();
        self.log.set_record_protocol_transactions(app.log_protocol_transactions);
        self.log.set_record_protocol_payloads(app.log_protocol_payloads);
        self.provider.set_allow_onboard_writes(app.allow_onboard_writes);

        let running = self.open_rgb.state() != ghub_core::openrgb_service::OpenRgbState::Disabled;
        // A released stack must not open a socket: the process holding the hardware is the one
        // that owns the OpenRGB claim, and two clients negotiating for the same controller would
        // each take a turn writing over the other.
        let wanted = app.open_rgb_enabled && self.holding;
        if wanted && !running {
            self.open_rgb.start(&app.open_rgb_host, app.open_rgb_port, app.open_rgb_allow_remote);
        } else if !wanted && running {
            self.open_rgb.stop();
        }
    }

    /// Takes the service events raised since the last drain.
    pub fn drain_events(&self) -> Vec<ServiceEvent> {
        match self.events.lock() {
            Ok(mut events) => std::mem::take(&mut *events),
            Err(e) => std::mem::take(&mut *e.into_inner()),
        }
    }

    /// True once per burst of device changes, clearing the flag.
    pub fn take_device_change(&self) -> bool {
        self.device_change.swap(false, Ordering::SeqCst)
    }

    /// Hands the hardware back for good. Called when the process is exiting.
    ///
    /// `release` plus the manager's terminal shutdown, which is the difference between this and a
    /// hand-over: nothing is going to take these devices in this process again.
    pub fn shutdown(&mut self) {
        self.release();
        self.devices.shutdown();
    }
}

/// Opens the profile store, falling back to a temporary directory so the application still runs
/// when the real one cannot be written (a roaming profile on a disconnected share is the common
/// case).
fn open_store(log: &DiagnosticsLog) -> (ProfileStore, Option<String>) {
    let directory = profile_directory();
    match ProfileStore::new(&directory, BACKUPS_TO_KEEP) {
        Ok(store) => {
            log.info_with(
                "Profiles",
                "Profile store opened",
                store.directory().display().to_string(),
            );
            (store, None)
        }
        Err(e) => {
            let fallback = std::env::temp_dir().join("ghub-profiles");
            let note = format!(
                "{} could not be opened ({e}). Profiles are being kept in {} for this session \
                 only and will not be found next time.",
                directory.display(),
                fallback.display()
            );
            // The fallback is inside the OS temp directory, which is writable by definition; if
            // even that fails there is nowhere left to put a document, so the panic is honest.
            let store = ProfileStore::new(&fallback, BACKUPS_TO_KEEP)
                .expect("the temporary directory is writable");
            (store, Some(note))
        }
    }
}

/// Where the automation API's bearer token lives.
///
/// Beside the profiles rather than inside them: the token is a credential for this machine, and a
/// profile document is something users copy between machines and paste into bug reports.
pub fn token_path() -> std::path::PathBuf {
    data_directory().join("automation-token")
}

/// `%APPDATA%\ghub-rs\profiles`, matching where the Artemis build kept them relative to its own
/// data directory.
pub fn profile_directory() -> std::path::PathBuf {
    data_directory().join("profiles")
}

fn data_directory() -> std::path::PathBuf {
    std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("ghub-rs")
}

/// Starts the API, reporting a refusal to the diagnostics log rather than to nobody.
///
/// The entropy comes from the OS CSPRNG and from nowhere else; the server rejects anything that
/// does not look random, so a failure here leaves the API off rather than listening with a
/// guessable token.
pub fn start_automation(server: &Arc<AutomationServer>, port: u16, log: &DiagnosticsLog) {
    let entropy = match ghub_platform::system_random(ghub_core::automation::TOKEN_ENTROPY_BYTES) {
        Ok(entropy) => entropy,
        Err(e) => {
            log.error("Automation", format!("no OS entropy for the API token: {e}"));
            return;
        }
    };
    if let Err(e) = server.start(port, &entropy) {
        log.error("Automation", format!("the automation API did not start: {e}"));
    }
}

/// The real HID backend, or a backend that sees nothing plus the reason why.
fn open_backend(log: &DiagnosticsLog) -> (Arc<dyn HidBackend>, Option<String>) {
    match ghub_hidpp::hidapi_backend::HidApiBackend::new() {
        Ok(backend) => (Arc::new(backend), None),
        Err(e) => {
            let reason = format!("HID could not be opened: {e}");
            log.error("Devices", reason.clone());
            // An empty replay backend enumerates nothing, which is exactly the behaviour wanted:
            // every page shows "no device" rather than erroring on each call.
            (
                Arc::new(ghub_hidpp::replay::ReplayHidBackend::new(
                    &ghub_hidpp::replay::HidTranscript::default(),
                    0,
                    false,
                )),
                Some(reason),
            )
        }
    }
}

#[cfg(windows)]
fn host_services() -> (Arc<dyn InputInjector>, Arc<dyn PlatformServices>) {
    (
        Arc::new(ghub_platform::WindowsInjector::new()),
        Arc::new(ghub_platform::WindowsPlatform::new()),
    )
}

/// Off Windows the application still builds and every page renders, which is what makes the layout
/// workable without a Windows machine. Nothing is injected and no hook is installed.
#[cfg(not(windows))]
fn host_services() -> (Arc<dyn InputInjector>, Arc<dyn PlatformServices>) {
    let stub = Arc::new(ghub_platform::stub::StubPlatform::new());
    (stub.clone(), stub)
}
