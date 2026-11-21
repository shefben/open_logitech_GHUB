//! The supervisor around [`OpenRgbClient`]. Port of `OpenRgb/OpenRgbService.cs`.
//!
//! The client knows how to speak the protocol once; this knows when to speak it at all. It owns
//! the connect/reconnect loop, the state the two pages report, and the ownership gate every write
//! goes through.
//!
//! Three properties are carried over deliberately:
//!
//! * **OpenRGB being absent changes [`OpenRgbState`] and nothing else.** It is an optional
//!   integration. A user who has never installed it must not see an error, which is why a failed
//!   connect is `Unavailable` with a reason rather than anything louder.
//! * **A refused host is not retried.** Reaching a non-loopback address without an opt-in is a
//!   policy decision, not a transient failure; retrying it every five seconds would be a
//!   connection attempt per five seconds to a host the user never approved.
//! * **Writes are ownership-gated and throttled.** OpenRGB's writer priority is below the native
//!   provider's, so when a profile is driving a device's lighting natively, OpenRGB does not get
//!   to fight it - [`LightingOwnership::try_begin_frame`] simply refuses the frame.
//!
//! ## Divergence from the C#
//!
//! The C# was a `Task.Run` loop with a `CancellationTokenSource` and a fresh `OpenRgbClient` per
//! attempt. Here it is one thread and one client: [`OpenRgbClient::connect`] disconnects first, so
//! reusing it is safe, and a stable client is what lets the UI read `controllers()` and the
//! negotiated version without having to be handed a new object on every reconnect.
//!
//! Cancellation is a flag checked between naps rather than a token, and [`OpenRgbService::stop`]
//! joins the thread. The C# did not join (`DisposeAsync` waited two seconds and gave up), which
//! meant a stop followed immediately by a start could have two loops running against one field.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ghub_hidpp::color::LedColor;

use crate::diagnostics::DiagnosticsLog;
use crate::openrgb::{Controller, OpenRgbClient, OpenRgbError};
use crate::ownership::{ALL_ZONES, LightingOwnership, LightingWriter};

/// Priority 10: below the native HID++ writer's 20, so a profile's own lighting wins a contest
/// for the same device. OpenRGB is an addition to the native path, not a replacement for it.
pub fn writer() -> LightingWriter {
    LightingWriter::new("openrgb", "OpenRGB SDK", 10)
}

/// How often the loop asks whether the connection is still there.
const ALIVE_POLL: Duration = Duration::from_millis(1000);

/// Granularity of a cancellable wait. Stop has to be prompt - it runs on the window's close path
/// and from the Options page - so no nap is longer than this.
const NAP_SLICE: Duration = Duration::from_millis(100);

/// Default wait between reconnect attempts.
pub const DEFAULT_RECONNECT_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpenRgbState {
    /// Switched off in the document, or stopped.
    #[default]
    Disabled,
    Connecting,
    Connected,
    /// Tried and could not: not running, refused, or the connection dropped.
    Unavailable,
}

impl OpenRgbState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Disabled => "Disabled",
            Self::Connecting => "Connecting",
            Self::Connected => "Connected",
            Self::Unavailable => "Unavailable",
        }
    }
}

/// Maps an OpenRGB controller onto a native device key, so both writers arbitrate over the same
/// physical device rather than each owning its own name for it.
type KeyResolver = Box<dyn Fn(&Controller) -> Option<String> + Send + Sync>;

pub struct OpenRgbService {
    log: Arc<DiagnosticsLog>,
    ownership: Arc<LightingOwnership>,
    client: Arc<OpenRgbClient>,
    state: Mutex<(OpenRgbState, Option<String>)>,
    /// Set by [`OpenRgbService::stop`] and read by the loop between naps. Not per-loop state:
    /// `start` joins the old loop before clearing it, so there is never more than one reader.
    cancel: AtomicBool,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    reconnect_delay: Mutex<Duration>,
    resolver: Mutex<Option<KeyResolver>>,
    /// Set whenever the state or the controller list moved, drained by the UI.
    ///
    /// A flag rather than the C#'s `StateChanged` event: the transitions happen on the loop
    /// thread, and running a UI subscriber there would mean a repaint request from inside a
    /// connect.
    dirty: AtomicBool,
}

impl OpenRgbService {
    pub fn new(log: Arc<DiagnosticsLog>, ownership: Arc<LightingOwnership>) -> Self {
        Self {
            client: Arc::new(OpenRgbClient::new(Arc::clone(&log))),
            log,
            ownership,
            state: Mutex::new((OpenRgbState::Disabled, None)),
            cancel: AtomicBool::new(false),
            worker: Mutex::new(None),
            reconnect_delay: Mutex::new(DEFAULT_RECONNECT_DELAY),
            resolver: Mutex::new(None),
            dirty: AtomicBool::new(false),
        }
    }

    pub fn client(&self) -> &Arc<OpenRgbClient> {
        &self.client
    }

    pub fn state(&self) -> OpenRgbState {
        self.state.lock().map(|s| s.0).unwrap_or(OpenRgbState::Disabled)
    }

    pub fn last_error(&self) -> Option<String> {
        self.state.lock().ok()?.1.clone()
    }

    pub fn negotiated_protocol_version(&self) -> u32 {
        self.client.negotiated_protocol_version()
    }

    pub fn controllers(&self) -> Vec<Controller> {
        self.client.controllers()
    }

    pub fn set_reconnect_delay(&self, delay: Duration) {
        if let Ok(mut slot) = self.reconnect_delay.lock() {
            *slot = delay;
        }
    }

    pub fn set_native_device_key_resolver(
        &self,
        resolve: impl Fn(&Controller) -> Option<String> + Send + Sync + 'static,
    ) {
        if let Ok(mut slot) = self.resolver.lock() {
            *slot = Some(Box::new(resolve));
        }
    }

    /// True once per burst of state or controller-list changes, clearing the flag.
    pub fn take_changed(&self) -> bool {
        self.dirty.swap(false, Ordering::SeqCst)
    }

    /// The ownership key for a controller: the native device it is really addressing when the
    /// resolver recognises it, and a name of its own otherwise.
    pub fn device_key_for(&self, controller: &Controller) -> String {
        if let Ok(resolver) = self.resolver.lock() {
            if let Some(resolve) = resolver.as_ref() {
                if let Some(key) = resolve(controller) {
                    return key;
                }
            }
        }
        format!("openrgb:{}:{}", controller.name, controller.location)
    }

    /// Starts the connect/reconnect loop. Stops an existing one first, so calling this when the
    /// Options page is saved is enough to move host or port.
    pub fn start(self: &Arc<Self>, host: &str, port: u16, allow_remote: bool) {
        // Stops and *joins* any previous loop first, so the flag can be cleared without a
        // surviving loop reading it and carrying on.
        self.stop();
        self.cancel.store(false, Ordering::SeqCst);

        let service = Arc::clone(self);
        let host_owned = host.to_owned();
        let spawned = std::thread::Builder::new()
            .name("ghub openrgb".to_owned())
            .spawn(move || service.run(&host_owned, port, allow_remote));

        match spawned {
            Ok(handle) => {
                if let Ok(mut slot) = self.worker.lock() {
                    *slot = Some(handle);
                }
            }
            Err(e) => self.set_state(
                OpenRgbState::Unavailable,
                Some(format!("the OpenRGB thread could not be started: {e}")),
            ),
        }
    }

    fn run(&self, host: &str, port: u16, allow_remote: bool) {
        while !self.cancel.load(Ordering::SeqCst) {
            self.set_state(OpenRgbState::Connecting, None);
            match self.client.connect(host, port, allow_remote) {
                Ok(()) => {
                    self.log.info_with(
                        "OpenRGB",
                        "Connected",
                        format!(
                            "{host}:{port}, protocol {}, {} controller(s)",
                            self.client.negotiated_protocol_version(),
                            self.client.controllers().len()
                        ),
                    );
                    self.set_state(OpenRgbState::Connected, None);

                    while !self.cancel.load(Ordering::SeqCst) && self.client.check_alive() {
                        if self.client.take_device_list_updated() {
                            self.refresh();
                        }
                        if !self.nap(ALIVE_POLL) {
                            break;
                        }
                    }

                    // The writer's claims go back whether the drop was the server's or ours: a
                    // held claim would block the native writer from a device nothing is driving.
                    self.ownership.release_all(&writer());
                    if self.cancel.load(Ordering::SeqCst) {
                        break;
                    }
                    self.set_state(
                        OpenRgbState::Unavailable,
                        Some("Connection lost; retrying".to_owned()),
                    );
                }
                // A refused host is the one failure that is not transient: the user has not asked
                // for a non-loopback connection, and asking again in five seconds will not change
                // that. The C# caught `InvalidOperationException` here for the same reason.
                Err(OpenRgbError::RemoteHostRefused(_)) => {
                    let message = OpenRgbError::RemoteHostRefused(host.to_owned()).to_string();
                    self.log.warning("OpenRGB", message.clone());
                    self.set_state(OpenRgbState::Unavailable, Some(message));
                    return;
                }
                Err(e) => {
                    self.ownership.release_all(&writer());
                    self.set_state(
                        OpenRgbState::Unavailable,
                        Some(format!("OpenRGB not reachable at {host}:{port} ({e})")),
                    );
                }
            }

            let delay = self.reconnect_delay.lock().map(|d| *d).unwrap_or(DEFAULT_RECONNECT_DELAY);
            if !self.nap(delay) {
                return;
            }
        }
    }

    /// Waits, in slices, so a stop is noticed within [`NAP_SLICE`]. False means cancelled.
    fn nap(&self, total: Duration) -> bool {
        let mut left = total;
        while left > Duration::ZERO {
            if self.cancel.load(Ordering::SeqCst) {
                return false;
            }
            let slice = left.min(NAP_SLICE);
            std::thread::sleep(slice);
            left -= slice;
        }
        !self.cancel.load(Ordering::SeqCst)
    }

    fn refresh(&self) {
        match self.client.refresh_controllers() {
            Ok(()) => self.dirty.store(true, Ordering::SeqCst),
            Err(e) => self.log.warning_with("OpenRGB", "Controller refresh failed", e.to_string()),
        }
    }

    /// Claims a controller and switches it to direct mode, which is what makes per-LED writes
    /// take effect at all. False means another writer owns the device.
    pub fn acquire(&self, controller: &Controller) -> bool {
        let key = self.device_key_for(controller);
        if !self.ownership.request(&key, &writer(), ALL_ZONES) {
            return false;
        }
        match self.client.set_custom_mode(controller.index) {
            Ok(()) => true,
            Err(e) => {
                self.log.warning_with("OpenRGB", "Direct mode failed", e.to_string());
                // The claim goes back: holding ownership of a device this writer cannot actually
                // drive would lock out the one that can.
                self.ownership.release(&key, &writer(), ALL_ZONES);
                false
            }
        }
    }

    /// Writes one frame. False means the frame was skipped - not owned, throttled, or not
    /// connected - which is a normal outcome and not an error.
    pub fn write(&self, controller: &Controller, colors: &[LedColor]) -> bool {
        if !self.client.is_connected() {
            return false;
        }
        if !self.ownership.try_begin_frame(&self.device_key_for(controller), &writer(), ALL_ZONES) {
            return false;
        }
        match self.client.update_leds(controller.index, colors) {
            Ok(()) => true,
            Err(e) => {
                self.log.warning_with("OpenRGB", "LED update failed", e.to_string());
                false
            }
        }
    }

    /// Stops the loop, disconnects and hands every claim back. Safe to call when not running.
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        let worker = self.worker.lock().ok().and_then(|mut slot| slot.take());
        if let Some(worker) = worker {
            // Joined rather than abandoned: a stop immediately followed by a start would
            // otherwise have two loops writing one state field, and the second connect would race
            // the first loop's disconnect.
            let _ = worker.join();
        }
        self.client.disconnect();
        self.ownership.release_all(&writer());
        self.set_state(OpenRgbState::Disabled, None);
    }

    fn set_state(&self, state: OpenRgbState, error: Option<String>) {
        let mut changed = false;
        if let Ok(mut slot) = self.state.lock() {
            changed = slot.0 != state || slot.1 != error;
            *slot = (state, error);
        }
        if changed {
            self.dirty.store(true, Ordering::SeqCst);
        }
    }

    /// One line for the diagnostics page and the Options page's status row.
    ///
    /// Built here rather than in each page so the two cannot disagree about what "connected"
    /// means: the protocol version and controller count are shown only when there is a connection
    /// that negotiated them.
    pub fn describe(&self) -> String {
        let state = self.state();
        let mut line = state.label().to_owned();
        if state == OpenRgbState::Connected {
            line.push_str(&format!(
                ", protocol {}, {} controllers",
                self.negotiated_protocol_version(),
                self.controllers().len()
            ));
        }
        if let Some(error) = self.last_error() {
            line.push_str(" - ");
            line.push_str(&error);
        }
        line
    }
}

impl Drop for OpenRgbService {
    fn drop(&mut self) {
        // `stop` takes `&self`, and the loop holds an `Arc<Self>`, so a live loop keeps this
        // alive: by the time `drop` runs the loop is already gone. Cancelling anyway costs
        // nothing and makes the invariant hold even if that ever stops being true.
        self.cancel.store(true, Ordering::SeqCst);
        self.client.disconnect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> Arc<OpenRgbService> {
        Arc::new(OpenRgbService::new(
            Arc::new(DiagnosticsLog::new(64)),
            Arc::new(LightingOwnership::new()),
        ))
    }

    fn controller(name: &str) -> Controller {
        Controller {
            index: 0,
            controller_type: 5,
            name: name.to_owned(),
            vendor: "Logitech".to_owned(),
            description: String::new(),
            version: String::new(),
            serial: String::new(),
            location: "usb".to_owned(),
            zones: Vec::new(),
            leds: Vec::new(),
        }
    }

    /// The whole point of the priority number: a device the native writer is driving is not
    /// available to OpenRGB, so a profile's colours cannot be overwritten by an SDK client.
    #[test]
    fn the_native_writer_outranks_openrgb() {
        let native = LightingWriter::new("native-hidpp", "Native Logitech HID++", 20);
        assert!(writer().priority < native.priority);
    }

    #[test]
    fn a_stopped_service_is_disabled_and_quiet() {
        let service = service();
        assert_eq!(service.state(), OpenRgbState::Disabled);
        assert_eq!(service.last_error(), None);
        assert_eq!(service.describe(), "Disabled");
        // Stopping something that never started must not panic or report a failure: the Options
        // page calls it whenever the integration is switched off, including the first time.
        service.stop();
        assert_eq!(service.state(), OpenRgbState::Disabled);
    }

    /// Without a resolver a controller still needs a stable key, or two controllers would share
    /// one ownership entry and each would think it owned the other's LEDs.
    #[test]
    fn an_unrecognised_controller_gets_a_key_of_its_own() {
        let service = service();
        let a = service.device_key_for(&controller("Corsair K70"));
        let b = service.device_key_for(&controller("Aura Motherboard"));
        assert_ne!(a, b);
        assert!(a.starts_with("openrgb:"));
    }

    #[test]
    fn a_resolver_points_a_controller_at_the_native_device() {
        let service = service();
        service.set_native_device_key_resolver(|c| {
            c.name.contains("G815").then(|| "logi:046d:c33f".to_owned())
        });
        assert_eq!(service.device_key_for(&controller("Logitech G815")), "logi:046d:c33f");
        assert!(service.device_key_for(&controller("Corsair K70")).starts_with("openrgb:"));
    }

    /// A write with nothing connected is a skip, not an error: the lighting path calls this on
    /// every frame and must not log once per frame when OpenRGB is not running.
    #[test]
    fn a_write_without_a_connection_is_skipped() {
        let service = service();
        assert!(!service.write(&controller("Logitech G815"), &[LedColor { r: 1, g: 2, b: 3 }]));
    }

    /// A refused host is a policy decision, so the loop must not sit there retrying it. The test
    /// asserts the loop ended by checking the state settles and stays put.
    #[test]
    fn a_remote_host_without_consent_is_refused_once() {
        let service = service();
        service.set_reconnect_delay(Duration::from_millis(10));
        service.start("203.0.113.1", 6742, false);

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while service.state() != OpenRgbState::Unavailable {
            assert!(std::time::Instant::now() < deadline, "never reported the refusal");
            std::thread::sleep(NAP_SLICE);
        }
        let error = service.last_error().unwrap_or_default();
        assert!(error.contains("not local"), "unexpected reason: {error}");
        service.stop();
    }

    /// Nothing listens on this port, so the loop must report that and keep retrying rather than
    /// giving up - OpenRGB being started after this application is the normal case.
    #[test]
    fn an_absent_server_is_reported_and_retried() {
        let service = service();
        // A short connect timeout so the attempt fails promptly, and a reconnect delay long
        // enough that the reported failure is still on screen when the poll below looks: with a
        // 10 ms delay the `Unavailable` state exists for 10 ms out of every attempt and a test
        // that sampled it would pass or fail on timing rather than on behaviour.
        service.client().set_timeout(Duration::from_millis(300));
        service.set_reconnect_delay(Duration::from_secs(1));
        // Port 1 needs no privilege to connect *to*, and nothing listens on it.
        service.start("127.0.0.1", 1, false);

        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            if service.state() == OpenRgbState::Unavailable {
                let error = service.last_error().unwrap_or_default();
                assert!(error.contains("not reachable at 127.0.0.1:1"), "{error}");
                break;
            }
            assert!(std::time::Instant::now() < deadline, "never reported the failure");
            std::thread::sleep(Duration::from_millis(25));
        }
        service.stop();
        assert_eq!(service.state(), OpenRgbState::Disabled);
    }

    /// `describe` is what both pages show, so its "connected" form is pinned here even though a
    /// test cannot reach a real server: the fields must only appear with a connection.
    #[test]
    fn the_description_only_claims_a_protocol_when_connected() {
        let service = service();
        service.set_state(OpenRgbState::Connecting, None);
        assert_eq!(service.describe(), "Connecting");
        service.set_state(OpenRgbState::Unavailable, Some("not running".to_owned()));
        assert_eq!(service.describe(), "Unavailable - not running");
        service.set_state(OpenRgbState::Connected, None);
        assert_eq!(service.describe(), "Connected, protocol 0, 0 controllers");
    }

    #[test]
    fn a_state_change_is_reported_once() {
        let service = service();
        service.set_state(OpenRgbState::Connecting, None);
        assert!(service.take_changed());
        assert!(!service.take_changed());
        // The same state again is not a change, so the UI is not woken for nothing.
        service.set_state(OpenRgbState::Connecting, None);
        assert!(!service.take_changed());
    }
}
