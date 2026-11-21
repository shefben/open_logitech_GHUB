//! Serialised HID++ request/response channel.
//! Port of `Artemis.Peripherals/Logitech/Hidpp/HidppChannel.cs`.
//!
//! One request is in flight at a time, because receivers and keyboards misbehave with pipelined
//! requests, and responses are correlated on device index, feature index, function and software
//! id, so traffic belonging to another HID++ client (G HUB) is ignored rather than mistaken for
//! ours.
//!
//! The C# original is `async`; this is blocking on purpose. Every caller already runs on a worker
//! thread, the device answers in single-digit milliseconds, and a blocking channel keeps an async
//! runtime out of a binary whose whole point is being small.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::error::{HidppError, ProtocolError, Result};
use crate::hid::HidConnection;
use crate::report::constants::*;
use crate::report::HidppReport;

/// How a response is recognised as the answer to the request in flight.
type Matcher = Box<dyn Fn(&HidppReport) -> bool + Send + Sync>;

struct Pending {
    matches: Matcher,
    reply: SyncSender<Result<HidppReport>>,
}

/// Shared between the request path and the reader threads.
struct Shared {
    name: String,
    pending: Mutex<Option<Pending>>,
    notifications: SyncSender<HidppReport>,
    shutdown: AtomicBool,
    disconnected: AtomicBool,
}

impl Shared {
    /// Hands a report to the request in flight if it answers it, otherwise treats it as a
    /// device-originated notification.
    fn dispatch(&self, report: HidppReport) {
        {
            let mut slot = self.pending.lock().unwrap();
            if slot.as_ref().is_some_and(|p| (p.matches)(&report)) {
                let pending = slot.take().expect("just checked");
                // A full or dropped channel means the waiter already timed out and left; the
                // report is then genuinely stale, so dropping it is correct.
                let _ = pending.reply.try_send(Ok(report));
                return;
            }
        }

        // Errors belong to whoever asked, and a response to another HID++ client carries that
        // client's own non-zero software id. Neither is a notification. HID++ 1.0 notifications
        // are the exception: they occupy sub ids 0x40..0x80 and carry no software id of ours.
        let is_hidpp10_notification = (0x40..0x80).contains(&report.feature_index());
        if report.is_error() || (report.software_id() != 0 && !is_hidpp10_notification) {
            return;
        }

        // Dropped when nobody is listening or the queue is backed up: notifications are status,
        // and a stale one is worth less than blocking the reader thread.
        let _ = self.notifications.try_send(report);
    }

    fn mark_disconnected(&self, reason: &str) {
        if self.disconnected.swap(true, Ordering::SeqCst) {
            return;
        }
        log::warn!("{}: HID++ channel disconnected ({reason})", self.name);
        if let Some(pending) = self.pending.lock().unwrap().take() {
            let _ = pending.reply.try_send(Err(HidppError::Disconnected {
                channel: self.name.clone(),
                reason: reason.to_owned(),
            }));
        }
    }
}

pub struct HidppChannel {
    shared: Arc<Shared>,
    connections: Vec<Arc<dyn HidConnection + Sync>>,
    short: Option<Arc<dyn HidConnection + Sync>>,
    long: Option<Arc<dyn HidConnection + Sync>>,
    /// Serialises requests. Held for the whole retry loop, so a retry cannot interleave with
    /// another caller's request.
    request_lock: Mutex<()>,
    readers: Vec<JoinHandle<()>>,
    pub timeout: Duration,
    pub retries: u32,
}

impl HidppChannel {
    /// Spawns a reader thread per collection and returns the channel with the receiver for
    /// device-originated notifications. Dropping the receiver is fine: notifications are then
    /// discarded.
    pub fn new(
        connections: Vec<Arc<dyn HidConnection + Sync>>,
        name: impl Into<String>,
    ) -> Result<(Self, Receiver<HidppReport>)> {
        let name = name.into();
        let short = connections
            .iter()
            .find(|c| c.info().max_output_report_length == SHORT_LENGTH)
            .cloned();
        let long = connections
            .iter()
            .find(|c| c.info().max_output_report_length == LONG_LENGTH)
            .cloned();
        if short.is_none() && long.is_none() {
            return Err(HidppError::Unsupported(format!(
                "{name}: no HID++ short or long collection among the supplied connections"
            )));
        }

        let (notifications, inbox) = sync_channel(64);
        let shared = Arc::new(Shared {
            name,
            pending: Mutex::new(None),
            notifications,
            shutdown: AtomicBool::new(false),
            disconnected: AtomicBool::new(false),
        });

        let readers = connections
            .iter()
            .cloned()
            .map(|connection| {
                let shared = Arc::clone(&shared);
                std::thread::Builder::new()
                    .name(format!("hidpp-reader-{}", shared.name))
                    .spawn(move || read_loop(&shared, connection.as_ref()))
                    .expect("spawn HID++ reader thread")
            })
            .collect();

        Ok((
            Self {
                shared,
                connections,
                short,
                long,
                request_lock: Mutex::new(()),
                readers,
                timeout: Duration::from_millis(1000),
                retries: 2,
            },
            inbox,
        ))
    }

    pub fn name(&self) -> &str {
        &self.shared.name
    }

    pub fn supports_short(&self) -> bool {
        self.short.is_some()
    }

    pub fn supports_long(&self) -> bool {
        self.long.is_some()
    }

    pub fn is_connected(&self) -> bool {
        !self.shared.disconnected.load(Ordering::SeqCst)
    }

    /// Sends a HID++ 2.0 feature request and waits for the matching response.
    ///
    /// `retry_on_timeout` must be false for non-idempotent requests (onboard memory writes): a
    /// lost reply does not mean the command was not applied, and resending could apply it twice.
    pub fn request(
        &self,
        device_index: u8,
        feature_index: u8,
        function: u8,
        parameters: &[u8],
        options: RequestOptions,
    ) -> Result<HidppReport> {
        let report_id = self.choose_report_id(parameters.len(), options.force_long)?;
        let request = HidppReport::create(
            device_index,
            feature_index,
            function,
            SOFTWARE_ID,
            parameters,
            report_id,
        )
        .map_err(|e| HidppError::Unsupported(e.to_string()))?;

        let address = request.address();
        self.send(
            request,
            Box::new(move |r| matches_feature_response(r, device_index, feature_index, address)),
            options,
        )
    }

    /// Sends a HID++ 1.0 register request, used for receiver registers.
    pub fn register_request(
        &self,
        device_index: u8,
        sub_id: u8,
        address: u8,
        parameters: &[u8],
        long_report: bool,
    ) -> Result<HidppReport> {
        let mut report_id = if long_report || parameters.len() > 3 {
            LONG_REPORT_ID
        } else {
            SHORT_REPORT_ID
        };
        if report_id == SHORT_REPORT_ID && self.short.is_none() {
            report_id = LONG_REPORT_ID;
        }
        let request = HidppReport::create_register(device_index, sub_id, address, parameters, report_id)
            .map_err(|e| HidppError::Unsupported(e.to_string()))?;

        self.send(
            request,
            Box::new(move |r| {
                r.device_index() == device_index
                    && ((r.feature_index() == sub_id && r.address() == address)
                        || (r.is_hidpp10_error()
                            && r.error_target_index() == sub_id
                            && r.error_target_address() == address))
            }),
            RequestOptions::default(),
        )
    }

    fn choose_report_id(&self, parameter_length: usize, force_long: bool) -> Result<u8> {
        let fits_short = parameter_length <= SHORT_LENGTH - 4;
        if !force_long && fits_short && self.short.is_some() {
            return Ok(SHORT_REPORT_ID);
        }
        if self.long.is_some() {
            return Ok(LONG_REPORT_ID);
        }
        if fits_short {
            return Ok(SHORT_REPORT_ID);
        }
        Err(HidppError::Unsupported(format!(
            "{}: no long HID++ collection; request does not fit a short report",
            self.shared.name
        )))
    }

    fn send(&self, request: HidppReport, matches: Matcher, options: RequestOptions) -> Result<HidppReport> {
        if !self.is_connected() {
            return Err(HidppError::Disconnected {
                channel: self.shared.name.clone(),
                reason: "channel already marked disconnected".into(),
            });
        }

        let timeout = options.timeout.unwrap_or(self.timeout);
        let _serialised = self.request_lock.lock().unwrap();

        // The matcher is shared across attempts; each attempt gets a fresh reply channel.
        let matches = Arc::new(matches);

        for attempt in 0..=self.retries {
            let (reply, wait) = sync_channel(1);
            {
                // Registered before the write, or a fast device could answer into an empty slot.
                let matches = Arc::clone(&matches);
                *self.shared.pending.lock().unwrap() =
                    Some(Pending { matches: Box::new(move |r| matches(r)), reply });
            }

            if let Err(e) = self.write(&request) {
                self.clear_pending();
                return Err(e);
            }

            let outcome = wait.recv_timeout(timeout);
            self.clear_pending();

            match outcome {
                Ok(Ok(response)) if response.is_error() => {
                    let error = ProtocolError::from_report(&response);
                    if error.is_transient() && attempt < self.retries {
                        std::thread::sleep(Duration::from_millis(25));
                        continue;
                    }
                    return Err(HidppError::Protocol(error));
                }
                Ok(Ok(response)) => return Ok(response),
                Ok(Err(e)) => return Err(e),
                Err(RecvTimeoutError::Timeout) => {
                    if options.retry_on_timeout && attempt < self.retries {
                        continue;
                    }
                    return Err(HidppError::Timeout {
                        channel: self.shared.name.clone(),
                        request: request.to_string(),
                        attempts: attempt + 1,
                    });
                }
                // The sender was dropped, which only happens when a reader thread tore the
                // pending slot down - i.e. the transport went away.
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(HidppError::Disconnected {
                        channel: self.shared.name.clone(),
                        reason: "reader thread ended".into(),
                    });
                }
            }
        }

        unreachable!("the loop returns on its last attempt")
    }

    fn clear_pending(&self) {
        *self.shared.pending.lock().unwrap() = None;
    }

    fn write(&self, request: &HidppReport) -> Result<()> {
        let connection = if request.report_id() == SHORT_REPORT_ID {
            self.short.as_ref()
        } else {
            self.long.as_ref()
        }
        .ok_or_else(|| {
            HidppError::Unsupported(format!(
                "{}: no collection for report id 0x{:02X}",
                self.shared.name,
                request.report_id()
            ))
        })?;

        log::trace!("{} > {request}", self.shared.name);
        connection.write(request.raw()).map_err(|e| {
            self.shared.mark_disconnected(&e.to_string());
            HidppError::Disconnected {
                channel: self.shared.name.clone(),
                reason: format!("write failed: {e}"),
            }
        })
    }
}

impl Drop for HidppChannel {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        // Readers wake from their 100ms poll and see the flag. Joining bounds that wait; the
        // threads are detached-by-drop if one is wedged in a driver call.
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
        self.connections.clear();
        self.shared.disconnected.store(true, Ordering::SeqCst);
    }
}

fn read_loop(shared: &Shared, connection: &(dyn HidConnection + Sync)) {
    let mut buffer = vec![0u8; connection.info().max_input_report_length.max(64)];

    while !shared.shutdown.load(Ordering::SeqCst) {
        // Short poll rather than a blocking read: it is what lets Drop join promptly.
        match connection.read(&mut buffer, 100) {
            Ok(0) => continue,
            Ok(count) => {
                if let Some(report) = HidppReport::try_parse(&buffer[..count]) {
                    log::trace!("{} < {report}", shared.name);
                    shared.dispatch(report);
                }
            }
            Err(e) => {
                if !shared.shutdown.load(Ordering::SeqCst) {
                    shared.mark_disconnected(&e.to_string());
                }
                return;
            }
        }
    }
}

/// Per-request knobs. `Default` is the ordinary idempotent read.
#[derive(Debug, Clone, Copy)]
pub struct RequestOptions {
    pub timeout: Option<Duration>,
    /// Force a long report even when the parameters would fit a short one. Some features answer
    /// only on the long collection.
    pub force_long: bool,
    /// False for non-idempotent requests: a lost reply does not mean the command was not applied.
    pub retry_on_timeout: bool,
}

impl Default for RequestOptions {
    fn default() -> Self {
        Self { timeout: None, force_long: false, retry_on_timeout: true }
    }
}

impl RequestOptions {
    /// For onboard-memory writes and anything else that must not be sent twice.
    pub fn non_idempotent() -> Self {
        Self { retry_on_timeout: false, ..Self::default() }
    }

    pub fn long() -> Self {
        Self { force_long: true, ..Self::default() }
    }

    /// Chainable form of [`Self::long`], for combining with [`Self::non_idempotent`].
    pub fn forcing_long(self) -> Self {
        Self { force_long: true, ..self }
    }

    pub fn with_timeout(self, timeout: Duration) -> Self {
        Self { timeout: Some(timeout), ..self }
    }
}

/// Correlates a response with the request in flight.
fn matches_feature_response(
    r: &HidppReport,
    device_index: u8,
    feature_index: u8,
    address: u8,
) -> bool {
    if r.device_index() != device_index {
        return false;
    }
    if r.feature_index() == feature_index && r.address() == address {
        return true;
    }
    if r.is_hidpp20_error() && r.error_target_index() == feature_index && r.error_target_address() == address
    {
        return true;
    }
    // A receiver answers for an absent or asleep paired device with a HID++ 1.0 error.
    r.is_hidpp10_error() && r.error_target_index() == feature_index && r.error_target_address() == address
}
