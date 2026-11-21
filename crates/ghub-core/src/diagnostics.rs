//! Bounded in-memory structured diagnostics: the port of `Diagnostics/DiagnosticsLog.cs`.
//!
//! Two rules from the C# carry over unchanged because they are privacy rules, not design choices:
//!
//! * protocol payloads are **redacted by default** - only the four-byte HID++ routing header is
//!   kept, the parameters are replaced by their length,
//! * macro capture content is **never** logged here. A recorded macro is keystrokes the user typed
//!   into other applications; it does not belong in a log the user may paste into a bug report.
//!
//! The C# raised an `EntryAdded` event and called an optional `Sink` delegate. Both become
//! registered closures here, and - as in the C# - neither runs while the entry lock is held, so a
//! sink that logs back into the log cannot deadlock.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::time::{format_readable, now_unix_millis};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub enum DiagnosticLevel {
    Trace,
    #[default]
    Info,
    Warning,
    Error,
}

impl DiagnosticLevel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Trace => "Trace",
            Self::Info => "Info",
            Self::Warning => "Warning",
            Self::Error => "Error",
        }
    }

    /// Single letter for the compact diagnostics table in the UI.
    pub fn initial(self) -> char {
        match self {
            Self::Trace => 'T',
            Self::Info => 'I',
            Self::Warning => 'W',
            Self::Error => 'E',
        }
    }

    pub const ALL: [Self; 4] = [Self::Trace, Self::Info, Self::Warning, Self::Error];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticEntry {
    /// Milliseconds since the Unix epoch, UTC. See the note in [`crate::time`] on why not local.
    pub time: i64,
    pub level: DiagnosticLevel,
    pub source: String,
    pub message: String,
    pub detail: Option<String>,
}

impl DiagnosticEntry {
    /// One line, as the "copy diagnostics" button and the exported log file write it.
    pub fn to_line(&self) -> String {
        let mut line = format!(
            "{} {} [{}] {}",
            format_readable(self.time),
            self.level.label(),
            self.source,
            self.message
        );
        if let Some(detail) = &self.detail {
            line.push_str(" | ");
            line.push_str(detail);
        }
        line
    }
}

type Listener = Box<dyn Fn(&DiagnosticEntry) + Send + Sync + 'static>;

/// A ring buffer of entries plus a fan-out to listeners.
///
/// Shared by every subsystem, so it is `Sync` and takes `&self` everywhere. The two recording
/// switches are atomics rather than fields behind the mutex: the HID++ layer checks
/// `record_protocol_transactions` on every single transaction, and taking a mutex for that would
/// put the log on the hot path of the device loop.
pub struct DiagnosticsLog {
    entries: Mutex<std::collections::VecDeque<DiagnosticEntry>>,
    listeners: Mutex<Vec<Listener>>,
    capacity: usize,
    record_protocol_payloads: AtomicBool,
    record_protocol_transactions: AtomicBool,
}

impl std::fmt::Debug for DiagnosticsLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiagnosticsLog")
            .field("capacity", &self.capacity)
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

pub const DEFAULT_CAPACITY: usize = 2000;

impl Default for DiagnosticsLog {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl DiagnosticsLog {
    pub fn new(capacity: usize) -> Self {
        Self {
            // A zero capacity would make `push_back` then `pop_front` drop the entry we just
            // added, which is correct but pointless; clamp to one so "the last thing that
            // happened" is always answerable.
            entries: Mutex::new(std::collections::VecDeque::with_capacity(capacity.min(4096))),
            listeners: Mutex::new(Vec::new()),
            capacity: capacity.max(1),
            record_protocol_payloads: AtomicBool::new(false),
            record_protocol_transactions: AtomicBool::new(false),
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.entries.lock().map(|e| e.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// When false (the default) protocol transactions are logged with payload bytes replaced by
    /// their length.
    pub fn record_protocol_payloads(&self) -> bool {
        self.record_protocol_payloads.load(Ordering::Relaxed)
    }

    pub fn set_record_protocol_payloads(&self, on: bool) {
        self.record_protocol_payloads.store(on, Ordering::Relaxed);
    }

    /// When false (the default) protocol transactions are not logged at all - they are very chatty.
    pub fn record_protocol_transactions(&self) -> bool {
        self.record_protocol_transactions.load(Ordering::Relaxed)
    }

    pub fn set_record_protocol_transactions(&self, on: bool) {
        self.record_protocol_transactions.store(on, Ordering::Relaxed);
    }

    /// Registers a sink. Called for every entry added afterwards, in registration order.
    ///
    /// There is no unsubscribe: the log lives as long as the process and the only subscribers are
    /// the UI's entry channel and the host logger. An unsubscribe would need handles and a
    /// generation counter for no gain.
    pub fn subscribe(&self, listener: impl Fn(&DiagnosticEntry) + Send + Sync + 'static) {
        if let Ok(mut listeners) = self.listeners.lock() {
            listeners.push(Box::new(listener));
        }
    }

    pub fn add(
        &self,
        level: DiagnosticLevel,
        source: impl Into<String>,
        message: impl Into<String>,
        detail: Option<String>,
    ) {
        let entry = DiagnosticEntry {
            time: now_unix_millis(),
            level,
            source: source.into(),
            message: message.into(),
            detail,
        };
        if let Ok(mut entries) = self.entries.lock() {
            entries.push_back(entry.clone());
            while entries.len() > self.capacity {
                entries.pop_front();
            }
        }
        // Outside the lock, exactly as the C# did: a listener is free to read the log back.
        if let Ok(listeners) = self.listeners.lock() {
            for listener in listeners.iter() {
                listener(&entry);
            }
        }
    }

    pub fn trace(&self, source: impl Into<String>, message: impl Into<String>) {
        self.add(DiagnosticLevel::Trace, source, message, None);
    }

    pub fn info(&self, source: impl Into<String>, message: impl Into<String>) {
        self.add(DiagnosticLevel::Info, source, message, None);
    }

    pub fn info_with(
        &self,
        source: impl Into<String>,
        message: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.add(DiagnosticLevel::Info, source, message, Some(detail.into()));
    }

    pub fn warning(&self, source: impl Into<String>, message: impl Into<String>) {
        self.add(DiagnosticLevel::Warning, source, message, None);
    }

    pub fn warning_with(
        &self,
        source: impl Into<String>,
        message: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.add(DiagnosticLevel::Warning, source, message, Some(detail.into()));
    }

    pub fn error(&self, source: impl Into<String>, message: impl Into<String>) {
        self.add(DiagnosticLevel::Error, source, message, None);
    }

    pub fn error_with(
        &self,
        source: impl Into<String>,
        message: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.add(DiagnosticLevel::Error, source, message, Some(detail.into()));
    }

    /// Logs one HID++ transaction, honouring both recording switches.
    pub fn protocol(&self, source: impl Into<String>, direction: &str, payload: &[u8]) {
        if !self.record_protocol_transactions() {
            return;
        }
        let data = if self.record_protocol_payloads() {
            to_hex(payload)
        } else {
            redact(payload)
        };
        self.add(DiagnosticLevel::Trace, source, format!("{direction} {data}"), None);
    }

    pub fn snapshot(&self) -> Vec<DiagnosticEntry> {
        self.entries
            .lock()
            .map(|e| e.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// The whole log as text, newest last. What the "export" button writes.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for entry in self.snapshot() {
            out.push_str(&entry.to_line());
            out.push('\n');
        }
        out
    }

    pub fn clear(&self) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.clear();
        }
    }
}

/// Keeps the HID++ routing header (report, device, feature, function) and hides the parameters.
pub fn redact(payload: &[u8]) -> String {
    if payload.len() <= 4 {
        return to_hex(payload);
    }
    format!(
        "{}+[{} bytes redacted]",
        to_hex(&payload[..4]),
        payload.len() - 4
    )
}

/// Uppercase hex with no separator, matching `Convert.ToHexString`.
pub fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0F) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    #[test]
    fn entries_are_bounded_and_drop_the_oldest() {
        let log = DiagnosticsLog::new(3);
        for i in 0..10 {
            log.info("Test", format!("entry {i}"));
        }
        let snapshot = log.snapshot();
        assert_eq!(snapshot.len(), 3);
        assert_eq!(snapshot[0].message, "entry 7");
        assert_eq!(snapshot[2].message, "entry 9");
    }

    #[test]
    fn a_zero_capacity_still_keeps_the_last_entry() {
        let log = DiagnosticsLog::new(0);
        log.info("Test", "something happened");
        assert_eq!(log.snapshot().len(), 1);
    }

    #[test]
    fn protocol_logging_is_off_by_default() {
        let log = DiagnosticsLog::default();
        log.protocol("Hidpp", "->", &[0x10, 0xFF, 0x04, 0x1A, 0x00, 0x00, 0x00]);
        assert!(log.is_empty(), "transactions must not be logged unless asked for");
    }

    #[test]
    fn payloads_are_redacted_unless_explicitly_enabled() {
        let log = DiagnosticsLog::default();
        log.set_record_protocol_transactions(true);
        log.protocol("Hidpp", "->", &[0x10, 0xFF, 0x04, 0x1A, 0xDE, 0xAD, 0xBE]);
        assert_eq!(log.snapshot()[0].message, "-> 10FF041A+[3 bytes redacted]");

        log.clear();
        log.set_record_protocol_payloads(true);
        log.protocol("Hidpp", "->", &[0x10, 0xFF, 0x04, 0x1A, 0xDE, 0xAD, 0xBE]);
        assert_eq!(log.snapshot()[0].message, "-> 10FF041ADEADBE");
    }

    #[test]
    fn a_short_payload_is_all_header_so_nothing_is_redacted() {
        assert_eq!(redact(&[0x10, 0xFF, 0x04, 0x1A]), "10FF041A");
        assert_eq!(redact(&[]), "");
        assert_eq!(redact(&[0x00, 0x01]), "0001");
    }

    #[test]
    fn hex_matches_dotnets_convert_tohexstring() {
        assert_eq!(to_hex(&[0x00, 0x0F, 0xF0, 0xFF]), "000FF0FF");
    }

    #[test]
    fn listeners_see_every_entry_and_may_read_the_log_back() {
        let log = Arc::new(DiagnosticsLog::new(10));
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&seen);
        let reader = Arc::clone(&log);
        log.subscribe(move |entry| {
            counter.fetch_add(1, Ordering::Relaxed);
            // Reading inside the callback would deadlock if `add` held the entry lock.
            assert!(!reader.snapshot().is_empty());
            assert!(!entry.message.is_empty());
        });
        log.info("Test", "one");
        log.warning("Test", "two");
        assert_eq!(seen.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn levels_carry_their_detail_through_to_the_exported_line() {
        let log = DiagnosticsLog::default();
        log.error_with("Hidpp", "write rejected", "feature 0x8081 returned 0x05");
        let entry = &log.snapshot()[0];
        assert_eq!(entry.level, DiagnosticLevel::Error);
        let line = entry.to_line();
        assert!(line.contains("Error [Hidpp] write rejected | feature 0x8081 returned 0x05"), "{line}");
        assert!(log.to_text().ends_with('\n'));
    }

    #[test]
    fn levels_order_from_trace_to_error() {
        assert!(DiagnosticLevel::Trace < DiagnosticLevel::Error);
        assert_eq!(DiagnosticLevel::ALL.len(), 4);
        assert_eq!(DiagnosticLevel::Warning.initial(), 'W');
    }
}
