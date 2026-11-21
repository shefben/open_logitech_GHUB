//! Recorded-conversation HID backend. Port of `Artemis.Peripherals/Hid/ReplayHidBackend.cs`.
//!
//! A transcript is a list of frames: each request followed by the device's responses. Replaying one
//! exercises the whole stack above the wire without hardware present.
//!
//! Evidence rule carried over from the C#: a replay pass is protocol-only evidence and must be
//! reported as **Not hardware-verified**. It proves the parser and the command format agree with a
//! recording; it does not prove a device accepted anything.

use std::collections::{HashMap, VecDeque};
use std::io::{Error, ErrorKind, Result as IoResult};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;

use crate::hid::{HidBackend, HidConnection, HidInterfaceInfo, LOGITECH_VID, group_key};
use crate::report::constants::{LONG_LENGTH, SHORT_LENGTH, SHORT_REPORT_ID};
use crate::report::from_hex;

/// A hook that may answer a request itself. `None` falls through to the next source.
pub type Hook = Box<dyn Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptFrame {
    pub outgoing: bool,
    pub data: Vec<u8>,
}

/// A recorded HID++ conversation.
#[derive(Debug, Clone, Default)]
pub struct HidTranscript {
    pub device: String,
    pub frames: Vec<TranscriptFrame>,
}

#[derive(Deserialize)]
struct RawTranscript {
    #[serde(default)]
    device: String,
    frames: Vec<RawFrame>,
}

#[derive(Deserialize)]
struct RawFrame {
    d: String,
    hex: String,
}

impl HidTranscript {
    pub fn parse(json: &str) -> IoResult<Self> {
        let raw: RawTranscript =
            serde_json::from_str(json).map_err(|e| Error::new(ErrorKind::InvalidData, e))?;
        let frames = raw
            .frames
            .into_iter()
            .map(|f| {
                let data = from_hex(&f.hex).ok_or_else(|| {
                    Error::new(ErrorKind::InvalidData, format!("frame hex is not valid: {}", f.hex))
                })?;
                Ok(TranscriptFrame { outgoing: f.d == "out", data })
            })
            .collect::<IoResult<Vec<_>>>()?;
        Ok(Self { device: raw.device, frames })
    }

    pub fn load(path: impl AsRef<std::path::Path>) -> IoResult<Self> {
        Self::parse(&std::fs::read_to_string(path)?)
    }
}

/// Matching key for a request: the report id is dropped (the same command may be framed short or
/// long), the software-id nibble is masked off (it is ours, not the device's), and trailing zero
/// padding is trimmed (a short command padded into a long report is the same command).
pub fn request_key(request: &[u8]) -> String {
    if request.len() < 4 {
        return hex(request);
    }
    let mut copy = request[1..].to_vec();
    copy[2] &= 0xF0;
    let mut end = copy.len();
    while end > 3 && copy[end - 1] == 0 {
        end -= 1;
    }
    hex(&copy[..end])
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

struct OpenConnection {
    tx: Sender<Vec<u8>>,
    /// Short collection takes 7-byte reports, long takes the rest.
    is_short: bool,
    dead: Arc<AtomicBool>,
}

#[derive(Default)]
struct Inner {
    responses: HashMap<String, VecDeque<Vec<u8>>>,
    last_response: HashMap<String, Vec<u8>>,
    open: Vec<OpenConnection>,
    all_requests: Vec<Vec<u8>>,
    /// Requests the transcript has no answer for - assert this is empty to prove no unexpected
    /// write reached the device.
    unanswered: Vec<Vec<u8>>,
}

#[derive(Default)]
struct Hooks {
    /// Consulted before the transcript: simulated device state in tests.
    override_hook: Option<Hook>,
    /// Consulted after the transcript: synthesises answers the recording lacks (write echoes).
    fallback_hook: Option<Hook>,
}

/// Held by the backend and by every connection it opens. Hooks live under their own lock so a
/// hook can read backend state without deadlocking against `inner`.
struct Shared {
    inner: Mutex<Inner>,
    hooks: Mutex<Hooks>,
    has_short_collection: bool,
}

impl Shared {
    fn answer(&self, request: &[u8]) -> Option<Vec<u8>> {
        // Hook lock is taken and released around the call, never held across `inner`.
        let overridden = {
            let hooks = self.hooks.lock().unwrap();
            hooks.override_hook.as_ref().and_then(|hook| hook(request))
        };
        if let Some(answer) = overridden {
            self.inner.lock().unwrap().all_requests.push(request.to_vec());
            return Some(answer);
        }

        let key = request_key(request);
        {
            let mut inner = self.inner.lock().unwrap();
            inner.all_requests.push(request.to_vec());

            if let Some(response) = inner.responses.get_mut(&key).and_then(VecDeque::pop_front) {
                inner.last_response.insert(key, response.clone());
                return Some(patch(&response, request));
            }
            // A repeated read answers the way the capture did the first time, so a feature probe
            // that polls twice does not fall off the end of the transcript.
            if let Some(last) = inner.last_response.get(&key) {
                return Some(patch(last, request));
            }
        }

        let synthetic = {
            let hooks = self.hooks.lock().unwrap();
            hooks.fallback_hook.as_ref().and_then(|hook| hook(request))
        };
        match synthetic {
            Some(answer) => Some(answer),
            None => {
                self.inner.lock().unwrap().unanswered.push(request.to_vec());
                None
            }
        }
    }

    /// Delivers a report to the collection that carries its length. With no short collection
    /// present, short reports arrive on the long one.
    fn deliver(&self, report: &[u8]) {
        let is_short = report.first() == Some(&SHORT_REPORT_ID);
        let inner = self.inner.lock().unwrap();
        for connection in &inner.open {
            let takes_it =
                connection.is_short == is_short || (!self.has_short_collection && is_short);
            if takes_it {
                let _ = connection.tx.send(report.to_vec());
            }
        }
    }
}

pub struct ReplayHidBackend {
    shared: Arc<Shared>,
    product_id: u16,
}

impl ReplayHidBackend {
    pub fn new(transcript: &HidTranscript, product_id: u16, has_short_collection: bool) -> Self {
        let mut responses: HashMap<String, VecDeque<Vec<u8>>> = HashMap::new();
        let mut key: Option<String> = None;
        for frame in &transcript.frames {
            if frame.outgoing {
                key = Some(request_key(&frame.data));
                continue;
            }
            // An inbound frame before any request is an unsolicited notification in the
            // recording, not an answer, so it has nothing to key off.
            if let Some(k) = key.take() {
                responses.entry(k).or_default().push_back(frame.data.clone());
            }
        }

        Self {
            shared: Arc::new(Shared {
                inner: Mutex::new(Inner { responses, ..Inner::default() }),
                hooks: Mutex::new(Hooks::default()),
                has_short_collection,
            }),
            product_id,
        }
    }

    /// An empty backend: every request falls through to the hooks. For tests that simulate device
    /// state outright rather than replay a capture.
    pub fn empty(product_id: u16, has_short_collection: bool) -> Self {
        Self::new(&HidTranscript::default(), product_id, has_short_collection)
    }

    pub fn product_id(&self) -> u16 {
        self.product_id
    }

    pub fn set_override_hook(&self, hook: Hook) {
        self.shared.hooks.lock().unwrap().override_hook = Some(hook);
    }

    pub fn set_fallback_hook(&self, hook: Hook) {
        self.shared.hooks.lock().unwrap().fallback_hook = Some(hook);
    }

    pub fn all_requests(&self) -> Vec<Vec<u8>> {
        self.shared.inner.lock().unwrap().all_requests.clone()
    }

    pub fn unanswered_requests(&self) -> Vec<Vec<u8>> {
        self.shared.inner.lock().unwrap().unanswered.clone()
    }

    /// Injects an unsolicited device report (a notification) on every open connection.
    pub fn inject_notification(&self, report: &[u8]) {
        self.shared.deliver(report);
    }

    /// Simulates unplug: every open connection starts failing.
    pub fn disconnect(&self) {
        let inner = self.shared.inner.lock().unwrap();
        for connection in &inner.open {
            connection.dead.store(true, Ordering::SeqCst);
        }
    }
}

/// Replaces the recorded software id with the one this request actually used, so correlation in
/// [`crate::channel`] succeeds. Error reports carry no software id to patch.
fn patch(response: &[u8], request: &[u8]) -> Vec<u8> {
    let mut copy = response.to_vec();
    if copy.len() > 3 && request.len() > 3 && copy[2] != 0xFF && copy[2] != 0x8F {
        copy[3] = (copy[3] & 0xF0) | (request[3] & 0x0F);
    }
    copy
}

impl HidBackend for ReplayHidBackend {
    fn enumerate(&self, _vendor_id: u16) -> IoResult<Vec<HidInterfaceInfo>> {
        let mut list = Vec::new();
        if self.shared.has_short_collection {
            list.push(info(self.product_id, 1, SHORT_LENGTH, 1));
        }
        list.push(info(self.product_id, 2, LONG_LENGTH, 2));
        Ok(list)
    }

    fn open(&self, info: &HidInterfaceInfo) -> IoResult<Arc<dyn HidConnection + Sync>> {
        let (tx, rx) = channel();
        let dead = Arc::new(AtomicBool::new(false));
        let is_short = info.max_output_report_length == SHORT_LENGTH;

        self.shared.inner.lock().unwrap().open.push(OpenConnection {
            tx,
            is_short,
            dead: Arc::clone(&dead),
        });

        Ok(Arc::new(ReplayConnection {
            info: info.clone(),
            inbox: Mutex::new(rx),
            dead,
            shared: Arc::clone(&self.shared),
        }))
    }
}

/// Builds a collection with a Windows-shaped HID path, so [`group_key`] folds the short and long
/// collections back into one interface exactly as it does for a real device. A synthetic path that
/// did not fold would have the provider open two channels on what is one receiver, and each
/// would get its own reader racing for the same reports.
fn info(product_id: u16, collection: u8, length: usize, usage: u16) -> HidInterfaceInfo {
    let path = format!(
        r"\\?\hid#vid_046d&pid_{product_id:04x}&mi_02&col{collection:02}#8&replay&0&{:04}#{{4d1e55b2-f16f-11cf-88cb-001111000030}}",
        collection - 1
    );
    HidInterfaceInfo {
        group_key: group_key(&path),
        path,
        vendor_id: LOGITECH_VID,
        product_id,
        product_name: None,
        serial_number: None,
        max_input_report_length: length,
        max_output_report_length: length,
        usage_page: 0xFF00,
        usage,
        interface_number: Some(2),
    }
}

struct ReplayConnection {
    info: HidInterfaceInfo,
    inbox: Mutex<Receiver<Vec<u8>>>,
    dead: Arc<AtomicBool>,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for ReplayConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ReplayConnection({})", self.info.path)
    }
}

impl HidConnection for ReplayConnection {
    fn info(&self) -> &HidInterfaceInfo {
        &self.info
    }

    fn write(&self, report: &[u8]) -> IoResult<()> {
        if self.dead.load(Ordering::SeqCst) {
            return Err(Error::new(ErrorKind::BrokenPipe, "replay device disconnected"));
        }
        // An unanswered request is not a write failure: the device may legitimately stay silent,
        // and the test asserts on `unanswered_requests()` instead.
        if let Some(response) = self.shared.answer(report) {
            self.shared.deliver(&response);
        }
        Ok(())
    }

    fn read(&self, buffer: &mut [u8], timeout_ms: u32) -> IoResult<usize> {
        if self.dead.load(Ordering::SeqCst) {
            return Err(Error::new(ErrorKind::BrokenPipe, "replay device disconnected"));
        }
        let inbox = self.inbox.lock().unwrap();
        match inbox.recv_timeout(Duration::from_millis(timeout_ms as u64)) {
            Ok(report) => {
                let n = report.len().min(buffer.len());
                buffer[..n].copy_from_slice(&report[..n]);
                Ok(n)
            }
            Err(RecvTimeoutError::Timeout) => {
                if self.dead.load(Ordering::SeqCst) {
                    Err(Error::new(ErrorKind::BrokenPipe, "replay device disconnected"))
                } else {
                    Ok(0)
                }
            }
            // The backend was dropped; nothing more will ever arrive.
            Err(RecvTimeoutError::Disconnected) => {
                Err(Error::new(ErrorKind::BrokenPipe, "replay backend dropped"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{HidppChannel, RequestOptions};
    use crate::error::HidppError;
    use crate::report::constants::WIRED_DEVICE_INDEX;

    /// Opens every collection the backend offers and wires a channel over them.
    fn channel(backend: &ReplayHidBackend) -> (HidppChannel, Receiver<crate::HidppReport>) {
        let connections = backend
            .enumerate(LOGITECH_VID)
            .unwrap()
            .iter()
            .map(|i| backend.open(i).unwrap())
            .collect();
        HidppChannel::new(connections, "replay").unwrap()
    }

    /// Asking a feature for its version: `0x11 FF 00 1x 0001 ...` answers with the feature index.
    const ROOT_GET_FEATURE_REQUEST: &str = "11FF000A00010000000000000000000000000000";
    const ROOT_GET_FEATURE_REPLY: &str = "11FF000A08000000000000000000000000000000";

    fn transcript(pairs: &[(&str, &str)]) -> HidTranscript {
        let frames = pairs
            .iter()
            .flat_map(|(out, reply)| {
                [
                    TranscriptFrame { outgoing: true, data: from_hex(out).unwrap() },
                    TranscriptFrame { outgoing: false, data: from_hex(reply).unwrap() },
                ]
            })
            .collect();
        HidTranscript { device: "test".into(), frames }
    }

    #[test]
    fn request_key_ignores_report_id_software_id_and_padding() {
        // Same command framed short vs long, with different software ids, is one key.
        let short = from_hex("10FF000A0001").unwrap();
        let long = from_hex("11FF000B00010000000000000000000000000000").unwrap();
        assert_eq!(request_key(&short), request_key(&long));
        // The feature index is part of the key, so a different feature is a different key.
        let other = from_hex("11FF010A00010000000000000000000000000000").unwrap();
        assert_ne!(request_key(&short), request_key(&other));
    }

    #[test]
    fn replays_a_recorded_answer_and_correlates_it() {
        let backend = ReplayHidBackend::new(
            &transcript(&[(ROOT_GET_FEATURE_REQUEST, ROOT_GET_FEATURE_REPLY)]),
            0xC539,
            false,
        );
        let (channel, _notifications) = channel(&backend);

        let response = channel
            .request(WIRED_DEVICE_INDEX, 0x00, 0, &[0x00, 0x01], RequestOptions::long())
            .expect("transcript answers this");

        assert_eq!(response.param(0), 0x08, "feature lives at index 8");
        assert!(backend.unanswered_requests().is_empty());
        assert_eq!(backend.all_requests().len(), 1);
    }

    #[test]
    fn a_recorded_software_id_is_patched_so_correlation_still_succeeds() {
        // The capture was taken while G HUB was talking to the device, so its replies carry G
        // HUB's software id (1 here), not ours (0x0A). Without the patch the channel would never
        // match the reply and every replayed read would time out.
        let backend = ReplayHidBackend::new(
            &transcript(&[(ROOT_GET_FEATURE_REQUEST, "11FF00010800000000000000000000000000000000")]),
            0xC539,
            false,
        );
        let (mut channel, _notifications) = channel(&backend);
        channel.timeout = Duration::from_millis(200);

        let response = channel
            .request(WIRED_DEVICE_INDEX, 0x00, 0, &[0x00, 0x01], RequestOptions::long())
            .expect("the patched reply correlates");
        assert_eq!(response.software_id(), crate::report::constants::SOFTWARE_ID);
        assert_eq!(response.param(0), 0x08);
    }

    #[test]
    fn an_unanswered_request_times_out_and_is_recorded() {
        let backend = ReplayHidBackend::empty(0xC539, false);
        let (mut channel, _notifications) = channel(&backend);
        channel.timeout = Duration::from_millis(30);
        channel.retries = 1;

        let error = channel
            .request(WIRED_DEVICE_INDEX, 0x00, 0, &[0x00, 0x01], RequestOptions::long())
            .expect_err("nothing answers");

        match error {
            HidppError::Timeout { attempts, .. } => assert_eq!(attempts, 2, "one retry"),
            other => panic!("expected a timeout, got {other}"),
        }
        // Both attempts reached the device and both went unanswered.
        assert_eq!(backend.unanswered_requests().len(), 2);
    }

    #[test]
    fn non_idempotent_requests_are_not_retried() {
        let backend = ReplayHidBackend::empty(0xC539, false);
        let (mut channel, _notifications) = channel(&backend);
        channel.timeout = Duration::from_millis(30);
        channel.retries = 2;

        let error = channel
            .request(
                WIRED_DEVICE_INDEX,
                0x08,
                1,
                &[0x00],
                RequestOptions::non_idempotent().with_timeout(Duration::from_millis(30)),
            )
            .expect_err("nothing answers");

        assert!(matches!(error, HidppError::Timeout { attempts: 1, .. }), "got {error}");
        assert_eq!(
            backend.all_requests().len(),
            1,
            "an onboard write must reach the device exactly once"
        );
    }

    #[test]
    fn a_protocol_error_is_classified_not_returned_as_a_response() {
        // HID++ 2.0 error for feature index 0x0B, function 0, code 0x09 (Unsupported).
        let backend = ReplayHidBackend::empty(0xC539, false);
        backend.set_fallback_hook(Box::new(|request| {
            let mut error = vec![0u8; LONG_LENGTH];
            error[0] = 0x11;
            error[1] = request[1];
            error[2] = 0xFF; // error sub id
            error[3] = request[2]; // the feature index it refers to
            error[4] = request[3]; // the function|swid it refers to
            error[5] = 0x09; // Unsupported
            Some(error)
        }));
        let (channel, _notifications) = channel(&backend);

        let error = channel
            .request(WIRED_DEVICE_INDEX, 0x0B, 0, &[0x00], RequestOptions::long())
            .expect_err("the device refuses");

        let protocol = error.as_protocol().expect("classified as a protocol error");
        assert!(protocol.is_hidpp20);
        assert_eq!(protocol.code, 0x09);
        assert!(!protocol.is_transient(), "Unsupported must not be retried");
    }

    #[test]
    fn a_busy_answer_is_retried_then_succeeds() {
        let attempts = Arc::new(AtomicBool::new(false));
        let backend = ReplayHidBackend::empty(0xC539, false);
        let seen = Arc::clone(&attempts);
        backend.set_fallback_hook(Box::new(move |request| {
            let mut reply = vec![0u8; LONG_LENGTH];
            reply[0] = 0x11;
            reply[1] = request[1];
            if seen.swap(true, Ordering::SeqCst) {
                // Second attempt: a real answer.
                reply[2] = request[2];
                reply[3] = request[3];
                reply[4] = 0x42;
            } else {
                // First attempt: Busy, which the channel must retry rather than surface.
                reply[2] = 0xFF;
                reply[3] = request[2];
                reply[4] = request[3];
                reply[5] = 0x08;
            }
            Some(reply)
        }));
        let (channel, _notifications) = channel(&backend);

        let response = channel
            .request(WIRED_DEVICE_INDEX, 0x0B, 0, &[0x00], RequestOptions::long())
            .expect("retry after Busy succeeds");
        assert_eq!(response.param(0), 0x42);
    }

    #[test]
    fn device_notifications_reach_the_subscriber_and_requests_do_not() {
        let backend = ReplayHidBackend::empty(0xC539, false);
        let (_channel, notifications) = channel(&backend);

        // Software id 0 marks a device-originated report. 0x0B here is a feature index.
        let mut event = vec![0u8; LONG_LENGTH];
        event[0] = 0x11;
        event[1] = 0x01;
        event[2] = 0x0B;
        event[3] = 0x00;
        event[4] = 0x55;
        backend.inject_notification(&event);

        let received = notifications
            .recv_timeout(Duration::from_millis(500))
            .expect("notification delivered");
        assert_eq!(received.param(0), 0x55);
        assert_eq!(received.software_id(), 0);
    }

    #[test]
    fn unplug_surfaces_as_disconnected_not_as_a_timeout() {
        let backend = ReplayHidBackend::empty(0xC539, false);
        let (mut channel, _notifications) = channel(&backend);
        channel.timeout = Duration::from_millis(200);
        backend.disconnect();

        let error = channel
            .request(WIRED_DEVICE_INDEX, 0x00, 0, &[0x00, 0x01], RequestOptions::long())
            .expect_err("the device is gone");
        assert!(error.is_disconnected(), "got {error}");
        assert!(!channel.is_connected());
    }
}
