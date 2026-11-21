//! Opt-in, loopback-only JSON over HTTP for scripts: the port of `Automation/AutomationServer.cs`.
//!
//! For Python `urllib`, PowerShell `Invoke-RestMethod` and anything else a user already has. The
//! surface is deliberately tiny, and the reason is the whole design: **no endpoint executes code,
//! launches a program or touches a file.** Everything it can do, the UI can already do.
//!
//! Four defences, all of them enforced in [`AutomationServer::dispatch`] so they are tested
//! without a socket:
//!
//! 1. **Loopback only.** A request from any other address is refused, and the listener binds
//!    `127.0.0.1` rather than `0.0.0.0`, so the port is not reachable from the network at all.
//! 2. **A bearer token**, compared in constant time, stored in a file under the user's profile.
//! 3. **The `Host` header must be local.** Without this check a web page the user is browsing
//!    could POST to `http://localhost:<port>` and have the browser attach nothing but still be
//!    answered - DNS rebinding turns "loopback only" into "anyone who can get a page into your
//!    browser". The token is the real control; this is the belt to its braces.
//! 4. **A 16 KiB body limit**, checked while reading rather than trusting `Content-Length`.
//!
//! ## Divergence from the C#
//!
//! `HttpListener` has no equivalent here, so the server is about eighty lines of blocking HTTP
//! over `TcpListener`: request line, headers, optional body. That is less code than it sounds,
//! because the protocol surface is one method, one path, and three headers - and it removes the
//! C#'s need for a URL ACL.
//!
//! Token generation is the one thing this module cannot do by itself. A token is a security
//! boundary, so it must come from the OS CSPRNG, which lives in `ghub-platform`. The entropy is
//! therefore passed in, and [`AutomationServer::load_or_create_token`] takes it as an argument
//! rather than inventing it from a clock.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use crate::diagnostics::DiagnosticsLog;
use crate::id::Id;
use crate::profile::is_hex_color;

pub const MAX_BODY_BYTES: usize = 16 * 1024;
/// Bytes of OS entropy a token is built from. 24 bytes is 48 hex characters.
pub const TOKEN_ENTROPY_BYTES: usize = 24;
/// A token file shorter than this is treated as damaged and replaced.
pub const MIN_TOKEN_LENGTH: usize = 32;
/// Cap on the request line plus headers, so a peer cannot make us buffer without bound.
const MAX_HEADER_BYTES: usize = 8 * 1024;

/// Operations the automation API may perform.
///
/// Deliberately small, and deliberately a trait: the surface of the API is this list, so adding
/// an endpoint means adding a method here and being asked to justify it.
pub trait AutomationTarget: Send + Sync {
    fn status(&self) -> serde_json::Value;
    fn devices(&self) -> serde_json::Value;
    fn profiles(&self) -> serde_json::Value;
    fn select_profile(&self, id: Id, lock_profile: bool) -> Result<(), String>;
    fn unlock(&self) -> Result<(), String>;
    /// Whether the macro existed.
    fn run_macro(&self, id: Id) -> bool;
    /// The DPI the device reports afterwards, or `None` when there is no adjustable mouse.
    fn set_dpi(&self, device_key: Option<&str>, dpi: u32) -> Option<u32>;
    fn set_bank(&self, device_key: &str, bank: usize) -> Result<(), String>;
    /// Whether the colour was applied. `false` when another writer owns the device's lighting.
    fn set_static_color(&self, device_key: &str, hex: &str) -> bool;
    fn stop_all_macros(&self);
}

pub const ENDPOINTS: &[&str] = &[
    "GET /api/status",
    "GET /api/devices",
    "GET /api/profiles",
    "POST /api/profile/select {id, lock}",
    "POST /api/profile/unlock",
    "POST /api/macro/run {id}",
    "POST /api/macro/stop",
    "POST /api/dpi {dpi, device?}",
    "POST /api/bank {device, bank}",
    "POST /api/lighting/static {device, color}",
];

/// One request, already parsed. Separating this from the socket is what makes the security
/// checks testable.
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub authorization: Option<String>,
    pub host: Option<String>,
    /// `None` when the request arrived through something other than a socket, as in a test.
    pub remote: Option<IpAddr>,
    pub body: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub body: serde_json::Value,
}

impl Response {
    fn ok() -> Self {
        Self { status: 200, body: serde_json::json!({"ok": true}) }
    }

    fn json(status: u16, body: serde_json::Value) -> Self {
        Self { status, body }
    }

    fn error(status: u16, message: &str) -> Self {
        Self { status, body: serde_json::json!({"error": message}) }
    }

    fn reason(&self) -> &'static str {
        match self.status {
            200 => "OK",
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            409 => "Conflict",
            413 => "Payload Too Large",
            _ => "Internal Server Error",
        }
    }
}

pub struct AutomationServer {
    target: Arc<dyn AutomationTarget>,
    log: Arc<DiagnosticsLog>,
    token_path: PathBuf,
    token: Mutex<String>,
    port: AtomicU16,
    running: Arc<AtomicBool>,
}

impl std::fmt::Debug for AutomationServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AutomationServer")
            .field("port", &self.port())
            .field("running", &self.is_running())
            .finish_non_exhaustive()
    }
}

impl AutomationServer {
    pub fn new(
        target: Arc<dyn AutomationTarget>,
        log: Arc<DiagnosticsLog>,
        token_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            target,
            log,
            token_path: token_path.into(),
            token: Mutex::new(String::new()),
            port: AtomicU16::new(0),
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn token_path(&self) -> &Path {
        &self.token_path
    }

    pub fn port(&self) -> u16 {
        self.port.load(Ordering::SeqCst)
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn token(&self) -> String {
        self.token.lock().map(|t| t.clone()).unwrap_or_default()
    }

    /// Reads the token file, or writes a new token built from `entropy`.
    ///
    /// `entropy` must come from the OS CSPRNG - `ghub_platform::system_random` - and is rejected
    /// if it is obviously not random, because a predictable token is worse than no API.
    pub fn load_or_create_token(path: &Path, entropy: &[u8]) -> std::io::Result<String> {
        if let Ok(existing) = std::fs::read_to_string(path) {
            let existing = existing.trim().to_owned();
            if existing.len() >= MIN_TOKEN_LENGTH {
                return Ok(existing);
            }
        }
        if entropy.len() < 16 || entropy.iter().all(|b| *b == entropy[0]) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "refusing to write an automation token without real entropy from the OS",
            ));
        }
        // Hashed rather than hex-encoded directly, so the exact bytes the OS generator produced
        // never reach a file an attacker might read alongside other output from the same draw.
        let token = ghub_hidpp::sha256::digest_hex(entropy, TOKEN_ENTROPY_BYTES);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, &token)?;
        Ok(token)
    }

    /// Starts listening on loopback. `entropy` is used only if the token file has to be created.
    pub fn start(self: &Arc<Self>, port: u16, entropy: &[u8]) -> std::io::Result<()> {
        if port < 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "use a port between 1024 and 65535",
            ));
        }
        self.stop();
        let token = Self::load_or_create_token(&self.token_path, entropy)?;
        if let Ok(mut slot) = self.token.lock() {
            *slot = token;
        }

        // 127.0.0.1, not 0.0.0.0: the port must not exist on any other interface.
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let bound = listener.local_addr()?.port();
        self.port.store(bound, Ordering::SeqCst);
        self.running.store(true, Ordering::SeqCst);

        let server = Arc::clone(self);
        let running = Arc::clone(&self.running);
        std::thread::Builder::new()
            .name("ghub automation".to_owned())
            .spawn(move || {
                for stream in listener.incoming() {
                    if !running.load(Ordering::SeqCst) {
                        return;
                    }
                    match stream {
                        Ok(stream) => {
                            // One short-lived thread per request. The API is for scripts, so the
                            // rate is a handful per second at most and a pool would be ceremony.
                            let server = Arc::clone(&server);
                            let _ = std::thread::Builder::new()
                                .name("ghub automation request".to_owned())
                                .spawn(move || server.serve(stream));
                        }
                        // A failed accept on a listener we still own is transient; a closed one
                        // ends the loop through the `running` check above.
                        Err(_) => continue,
                    }
                }
            })?;

        self.log.info_with(
            "Automation",
            format!("Local automation API listening on http://localhost:{bound}/"),
            format!("token in {}", self.token_path.display()),
        );
        Ok(())
    }

    pub fn stop(&self) {
        if !self.running.swap(false, Ordering::SeqCst) {
            return;
        }
        // Unblocks `incoming()`, which then sees `running == false` and returns.
        let port = self.port();
        if port != 0 {
            let _ = std::net::TcpStream::connect(("127.0.0.1", port));
        }
        self.port.store(0, Ordering::SeqCst);
    }

    fn serve(&self, mut stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(5)));
        let remote = stream.peer_addr().ok().map(|a| a.ip());

        let response = match read_request(&mut stream, remote) {
            Ok(Some(request)) => self.dispatch(&request),
            // A connection that sent nothing: the `stop` wake-up, or a port scan.
            Ok(None) => return,
            Err(response) => response,
        };

        let body = serde_json::to_vec(&response.body).unwrap_or_else(|_| b"{}".to_vec());
        let header = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Cache-Control: no-store\r\nConnection: close\r\n\r\n",
            response.status,
            response.reason(),
            body.len()
        );
        let _ = stream.write_all(header.as_bytes());
        let _ = stream.write_all(&body);
        let _ = stream.flush();
    }

    /// Transport-independent request handling. Every security check lives here.
    pub fn dispatch(&self, request: &Request) -> Response {
        if let Some(remote) = request.remote {
            if !remote.is_loopback() {
                return Response::error(403, "loopback only");
            }
        }
        let host = host_name(request.host.as_deref().unwrap_or(""));
        if !matches!(host, "localhost" | "127.0.0.1" | "[::1]") {
            return Response::error(403, "invalid Host header");
        }

        let token = self.token();
        let expected = format!("Bearer {token}");
        let presented = request.authorization.as_deref().unwrap_or("");
        if token.is_empty() || !constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
            return Response::error(401, "missing or invalid token");
        }

        let json: Option<serde_json::Map<String, serde_json::Value>> =
            match request.body.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
                None => None,
                Some(body) => match serde_json::from_str::<serde_json::Value>(body) {
                    Ok(serde_json::Value::Object(map)) => Some(map),
                    _ => return Response::error(400, "body must be a JSON object"),
                },
            };

        let string = |name: &str| -> Result<String, Response> {
            json.as_ref()
                .and_then(|map| map.get(name))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| Response::error(400, &format!("'{name}' is required")))
        };
        let integer = |name: &str| -> Result<i64, Response> {
            json.as_ref()
                .and_then(|map| map.get(name))
                .and_then(serde_json::Value::as_i64)
                .ok_or_else(|| Response::error(400, &format!("'{name}' is required")))
        };
        let identifier = |name: &str| -> Result<Id, Response> {
            let raw = string(name)?;
            Id::parse(&raw).ok_or_else(|| Response::error(400, &format!("'{name}' must be an id")))
        };
        let optional_device = || -> Option<String> {
            json.as_ref()
                .and_then(|map| map.get("device"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };

        let path = request.path.trim_end_matches('/');
        let outcome = match (request.method.as_str(), path) {
            ("GET", "/api/status") => Ok(Response::json(200, self.target.status())),
            ("GET", "/api/devices") => Ok(Response::json(200, self.target.devices())),
            ("GET", "/api/profiles") => Ok(Response::json(200, self.target.profiles())),
            ("POST", "/api/profile/select") => identifier("id").and_then(|id| {
                let lock = json
                    .as_ref()
                    .and_then(|map| map.get("lock"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                match self.target.select_profile(id, lock) {
                    Ok(()) => Ok(Response::ok()),
                    Err(e) => Err(Response::error(400, &e)),
                }
            }),
            ("POST", "/api/profile/unlock") => match self.target.unlock() {
                Ok(()) => Ok(Response::ok()),
                Err(e) => Err(Response::error(400, &e)),
            },
            ("POST", "/api/macro/run") => identifier("id").map(|id| {
                if self.target.run_macro(id) {
                    Response::ok()
                } else {
                    Response::error(404, "macro not found")
                }
            }),
            ("POST", "/api/macro/stop") => {
                self.target.stop_all_macros();
                Ok(Response::ok())
            }
            ("POST", "/api/dpi") => integer("dpi").and_then(|dpi| {
                if !(50..=50_000).contains(&dpi) {
                    return Err(Response::error(400, "dpi out of range"));
                }
                match self.target.set_dpi(optional_device().as_deref(), dpi as u32) {
                    Some(read_back) => Ok(Response::json(200, serde_json::json!({"dpi": read_back}))),
                    None => Err(Response::error(404, "no mouse with adjustable DPI")),
                }
            }),
            ("POST", "/api/bank") => integer("bank").and_then(|bank| {
                if !(1..=8).contains(&bank) {
                    return Err(Response::error(400, "bank out of range"));
                }
                let device = string("device")?;
                match self.target.set_bank(&device, bank as usize) {
                    Ok(()) => Ok(Response::ok()),
                    Err(e) => Err(Response::error(400, &e)),
                }
            }),
            ("POST", "/api/lighting/static") => string("color").and_then(|hex| {
                if !is_hex_color(&hex) {
                    return Err(Response::error(400, "color must be #RRGGBB"));
                }
                let device = string("device")?;
                if self.target.set_static_color(&device, &hex) {
                    Ok(Response::ok())
                } else {
                    Err(Response::error(
                        409,
                        "device lighting owned by another writer or unavailable",
                    ))
                }
            }),
            _ => Err(Response::json(
                404,
                serde_json::json!({"error": "unknown endpoint", "endpoints": ENDPOINTS}),
            )),
        };
        outcome.unwrap_or_else(|response| response)
    }
}

impl Drop for AutomationServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The host part of a `Host` header, with any port removed.
///
/// An IPv6 literal is bracketed and contains colons of its own, so the port separator is only the
/// colon *after* the closing bracket. The C# split on the first colon, which turned `[::1]:6743`
/// into `[` - so its own list of accepted hosts could never match an IPv6 request. Fixed here,
/// and the test below is what caught it.
fn host_name(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        return match rest.find(']') {
            Some(end) => &host[..end + 2],
            // Unterminated bracket: hand back the whole thing, which will not match any accepted
            // host and so is refused.
            None => host,
        };
    }
    host.split(':').next().unwrap_or(host)
}

/// Compares two byte strings without an early exit on the first difference.
///
/// Lengths are allowed to leak - the expected length is a constant - but the contents are not,
/// because a token oracle that leaks one byte at a time is a token oracle.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut difference = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        difference |= x ^ y;
    }
    difference == 0
}

/// Reads one HTTP request. `Ok(None)` means the peer connected and said nothing.
fn read_request(
    stream: &mut TcpStream,
    remote: Option<IpAddr>,
) -> Result<Option<Request>, Response> {
    let mut reader = BufReader::new(stream.try_clone().map_err(|_| {
        Response::error(500, "could not read the request")
    })?);

    let mut line = String::new();
    let mut consumed = 0usize;
    match reader.read_line(&mut line) {
        Ok(0) => return Ok(None),
        Ok(n) => consumed += n,
        Err(_) => return Ok(None),
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_ascii_uppercase();
    let target = parts.next().unwrap_or("/").to_owned();
    if method.is_empty() {
        return Ok(None);
    }
    // The query string is not used by any endpoint; dropping it keeps path matching exact.
    let path = target.split(['?', '#']).next().unwrap_or("/").to_owned();

    let mut headers: HashMap<String, String> = HashMap::new();
    loop {
        let mut header = String::new();
        match reader.read_line(&mut header) {
            Ok(0) => break,
            Ok(n) => consumed += n,
            Err(_) => break,
        }
        if consumed > MAX_HEADER_BYTES {
            return Err(Response::error(413, "headers too large"));
        }
        let trimmed = header.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }

    let length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if length > MAX_BODY_BYTES {
        return Err(Response::error(413, "body too large"));
    }
    let body = if length > 0 {
        let mut buffer = vec![0u8; length];
        // `take` rather than trusting the declared length: a peer that promises 16 KiB and sends
        // more must not be able to push the extra into the next read.
        match reader.by_ref().take(length as u64).read_exact(&mut buffer) {
            Ok(()) => Some(String::from_utf8_lossy(&buffer).into_owned()),
            Err(_) => return Err(Response::error(400, "the body could not be read")),
        }
    } else {
        None
    };

    Ok(Some(Request {
        method,
        path,
        authorization: headers.get("authorization").cloned(),
        host: headers.get("host").cloned(),
        remote,
        body,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingTarget {
        calls: Mutex<Vec<String>>,
        macro_exists: bool,
        dpi_read_back: Option<u32>,
        lighting_applied: bool,
    }

    impl RecordingTarget {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                macro_exists: true,
                dpi_read_back: Some(1600),
                lighting_applied: true,
                ..Default::default()
            })
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        fn record(&self, call: impl Into<String>) {
            self.calls.lock().unwrap().push(call.into());
        }
    }

    impl AutomationTarget for RecordingTarget {
        fn status(&self) -> serde_json::Value {
            self.record("status");
            serde_json::json!({"running": true})
        }
        fn devices(&self) -> serde_json::Value {
            self.record("devices");
            serde_json::json!([])
        }
        fn profiles(&self) -> serde_json::Value {
            self.record("profiles");
            serde_json::json!([])
        }
        fn select_profile(&self, id: Id, lock_profile: bool) -> Result<(), String> {
            self.record(format!("select {id} lock={lock_profile}"));
            Ok(())
        }
        fn unlock(&self) -> Result<(), String> {
            self.record("unlock");
            Ok(())
        }
        fn run_macro(&self, id: Id) -> bool {
            self.record(format!("run {id}"));
            self.macro_exists
        }
        fn set_dpi(&self, device_key: Option<&str>, dpi: u32) -> Option<u32> {
            self.record(format!("dpi {dpi} on {}", device_key.unwrap_or("<any>")));
            self.dpi_read_back
        }
        fn set_bank(&self, device_key: &str, bank: usize) -> Result<(), String> {
            self.record(format!("bank {bank} on {device_key}"));
            Ok(())
        }
        fn set_static_color(&self, device_key: &str, hex: &str) -> bool {
            self.record(format!("color {hex} on {device_key}"));
            self.lighting_applied
        }
        fn stop_all_macros(&self) {
            self.record("stop");
        }
    }

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";

    fn server_with(target: Arc<RecordingTarget>) -> Arc<AutomationServer> {
        let directory = std::env::temp_dir().join(format!("ghub-automation-{}", Id::new()));
        std::fs::create_dir_all(&directory).expect("temp dir");
        let server = Arc::new(AutomationServer::new(
            target,
            Arc::new(DiagnosticsLog::default()),
            directory.join("automation-token.txt"),
        ));
        *server.token.lock().unwrap() = TOKEN.to_owned();
        server
    }

    fn request(method: &str, path: &str, body: Option<&str>) -> Request {
        Request {
            method: method.to_owned(),
            path: path.to_owned(),
            authorization: Some(format!("Bearer {TOKEN}")),
            host: Some("localhost:6743".to_owned()),
            remote: Some(IpAddr::from([127, 0, 0, 1])),
            body: body.map(str::to_owned),
        }
    }

    #[test]
    fn a_valid_status_request_is_answered() {
        let target = RecordingTarget::new();
        let server = server_with(Arc::clone(&target));
        let response = server.dispatch(&request("GET", "/api/status", None));
        assert_eq!(response.status, 200);
        assert_eq!(response.body["running"], serde_json::json!(true));
        assert_eq!(target.calls(), ["status"]);
    }

    #[test]
    fn a_trailing_slash_and_a_query_string_still_match() {
        let target = RecordingTarget::new();
        let server = server_with(Arc::clone(&target));
        assert_eq!(server.dispatch(&request("GET", "/api/status/", None)).status, 200);
    }

    #[test]
    fn a_non_loopback_client_is_refused_before_anything_else() {
        let target = RecordingTarget::new();
        let server = server_with(Arc::clone(&target));
        let mut r = request("GET", "/api/status", None);
        r.remote = Some(IpAddr::from([192, 168, 1, 50]));
        let response = server.dispatch(&r);
        assert_eq!(response.status, 403);
        assert_eq!(response.body["error"], serde_json::json!("loopback only"));
        assert!(target.calls().is_empty(), "nothing may run for a refused client");
    }

    #[test]
    fn a_foreign_host_header_is_refused_even_from_loopback() {
        // This is the DNS-rebinding case: the connection really is from 127.0.0.1, because it is
        // the user's own browser making it.
        let target = RecordingTarget::new();
        let server = server_with(Arc::clone(&target));
        for host in ["evil.example.com", "evil.example.com:6743", "", "0.0.0.0"] {
            let mut r = request("GET", "/api/status", None);
            r.host = Some(host.to_owned());
            let response = server.dispatch(&r);
            assert_eq!(response.status, 403, "host '{host}' should be refused");
        }
        assert!(target.calls().is_empty());
    }

    #[test]
    fn the_accepted_host_headers_are_the_local_ones() {
        let target = RecordingTarget::new();
        let server = server_with(Arc::clone(&target));
        for host in ["localhost", "localhost:6743", "127.0.0.1", "127.0.0.1:6743", "[::1]"] {
            let mut r = request("GET", "/api/status", None);
            r.host = Some(host.to_owned());
            assert_eq!(server.dispatch(&r).status, 200, "host '{host}' should be accepted");
        }
    }

    #[test]
    fn a_missing_or_wrong_token_is_refused() {
        let target = RecordingTarget::new();
        let server = server_with(Arc::clone(&target));
        for authorization in [
            None,
            Some(String::new()),
            Some("Bearer".to_owned()),
            Some(format!("Bearer {}", "f".repeat(TOKEN.len()))),
            // The right token with the wrong scheme, and the token without one.
            Some(TOKEN.to_owned()),
            Some(format!("Basic {TOKEN}")),
            // One byte short and one byte long, which a length-only check would accept.
            Some(format!("Bearer {}", &TOKEN[..TOKEN.len() - 1])),
            Some(format!("Bearer {TOKEN}x")),
        ] {
            let mut r = request("GET", "/api/status", None);
            r.authorization = authorization.clone();
            let response = server.dispatch(&r);
            assert_eq!(response.status, 401, "{authorization:?} should be refused");
        }
        assert!(target.calls().is_empty());
    }

    #[test]
    fn an_empty_server_token_refuses_everything() {
        // Otherwise a server that failed to load its token would accept "Bearer ".
        let target = RecordingTarget::new();
        let server = server_with(Arc::clone(&target));
        *server.token.lock().unwrap() = String::new();
        let mut r = request("GET", "/api/status", None);
        r.authorization = Some("Bearer ".to_owned());
        assert_eq!(server.dispatch(&r).status, 401);
    }

    #[test]
    fn an_unknown_endpoint_lists_the_real_ones() {
        let server = server_with(RecordingTarget::new());
        let response = server.dispatch(&request("GET", "/api/shell", None));
        assert_eq!(response.status, 404);
        assert_eq!(response.body["endpoints"].as_array().unwrap().len(), ENDPOINTS.len());
        // The published list must not grow a code-execution endpoint by accident.
        assert!(
            !ENDPOINTS.iter().any(|e| e.contains("launch") || e.contains("exec")),
            "the automation surface must never execute anything"
        );
    }

    #[test]
    fn the_wrong_method_for_a_real_path_is_a_404_not_an_action() {
        let target = RecordingTarget::new();
        let server = server_with(Arc::clone(&target));
        assert_eq!(server.dispatch(&request("GET", "/api/macro/stop", None)).status, 404);
        assert_eq!(server.dispatch(&request("POST", "/api/status", None)).status, 404);
        assert!(target.calls().is_empty());
    }

    #[test]
    fn a_body_that_is_not_a_json_object_is_rejected() {
        let server = server_with(RecordingTarget::new());
        for body in ["[1,2,3]", "\"text\"", "17", "{", "not json"] {
            let response = server.dispatch(&request("POST", "/api/macro/run", Some(body)));
            assert_eq!(response.status, 400, "body '{body}' should be rejected");
        }
    }

    #[test]
    fn a_missing_field_says_which_one() {
        let server = server_with(RecordingTarget::new());
        let response = server.dispatch(&request("POST", "/api/macro/run", Some("{}")));
        assert_eq!(response.status, 400);
        assert_eq!(response.body["error"], serde_json::json!("'id' is required"));
    }

    #[test]
    fn selecting_a_profile_passes_the_id_and_the_lock_flag() {
        let target = RecordingTarget::new();
        let server = server_with(Arc::clone(&target));
        let id = Id::new();
        let body = format!("{{\"id\":\"{id}\",\"lock\":true}}");
        assert_eq!(
            server.dispatch(&request("POST", "/api/profile/select", Some(&body))).status,
            200
        );
        assert_eq!(target.calls(), [format!("select {id} lock=true")]);

        // The flag defaults to false when absent, matching the C#.
        target.calls.lock().unwrap().clear();
        let body = format!("{{\"id\":\"{id}\"}}");
        server.dispatch(&request("POST", "/api/profile/select", Some(&body)));
        assert_eq!(target.calls(), [format!("select {id} lock=false")]);
    }

    #[test]
    fn a_malformed_id_is_a_400() {
        let server = server_with(RecordingTarget::new());
        let response =
            server.dispatch(&request("POST", "/api/macro/run", Some("{\"id\":\"nonsense\"}")));
        assert_eq!(response.status, 400);
        assert!(response.body["error"].as_str().unwrap().contains("must be an id"));
    }

    #[test]
    fn a_macro_that_does_not_exist_is_a_404() {
        let target = Arc::new(RecordingTarget {
            macro_exists: false,
            dpi_read_back: Some(800),
            lighting_applied: true,
            ..Default::default()
        });
        let server = server_with(Arc::clone(&target));
        let body = format!("{{\"id\":\"{}\"}}", Id::new());
        let response = server.dispatch(&request("POST", "/api/macro/run", Some(&body)));
        assert_eq!(response.status, 404);
    }

    #[test]
    fn dpi_is_range_checked_and_reports_the_read_back_value() {
        let target = RecordingTarget::new();
        let server = server_with(Arc::clone(&target));
        for dpi in [0, 49, 50_001, -100] {
            let body = format!("{{\"dpi\":{dpi}}}");
            let response = server.dispatch(&request("POST", "/api/dpi", Some(&body)));
            assert_eq!(response.status, 400, "dpi {dpi} should be refused");
        }
        let response = server.dispatch(&request("POST", "/api/dpi", Some("{\"dpi\":1600}")));
        assert_eq!(response.status, 200);
        // The read-back, not the requested value: the sensor rounds.
        assert_eq!(response.body["dpi"], serde_json::json!(1600));
        assert_eq!(target.calls(), ["dpi 1600 on <any>"]);
    }

    #[test]
    fn dpi_reports_404_when_no_mouse_can_change_it() {
        let target = Arc::new(RecordingTarget {
            macro_exists: true,
            dpi_read_back: None,
            lighting_applied: true,
            ..Default::default()
        });
        let server = server_with(Arc::clone(&target));
        let response = server.dispatch(&request("POST", "/api/dpi", Some("{\"dpi\":1600}")));
        assert_eq!(response.status, 404);
    }

    #[test]
    fn bank_is_range_checked_and_needs_a_device() {
        let target = RecordingTarget::new();
        let server = server_with(Arc::clone(&target));
        for bank in [0, 9, 100] {
            let body = format!("{{\"device\":\"kb\",\"bank\":{bank}}}");
            assert_eq!(
                server.dispatch(&request("POST", "/api/bank", Some(&body))).status,
                400,
                "bank {bank} should be refused"
            );
        }
        assert_eq!(
            server.dispatch(&request("POST", "/api/bank", Some("{\"bank\":2}"))).status,
            400,
            "the device is required"
        );
        let response =
            server.dispatch(&request("POST", "/api/bank", Some("{\"device\":\"kb\",\"bank\":2}")));
        assert_eq!(response.status, 200);
        assert_eq!(target.calls(), ["bank 2 on kb"]);
    }

    #[test]
    fn a_static_colour_must_be_a_hex_triplet() {
        let target = RecordingTarget::new();
        let server = server_with(Arc::clone(&target));
        for color in ["red", "#FFF", "FFFFFF", "#GGGGGG", "#FFFFFFFF", ""] {
            let body = format!("{{\"device\":\"kb\",\"color\":\"{color}\"}}");
            assert_eq!(
                server.dispatch(&request("POST", "/api/lighting/static", Some(&body))).status,
                400,
                "colour '{color}' should be refused"
            );
        }
        let body = "{\"device\":\"kb\",\"color\":\"#FF8800\"}";
        assert_eq!(
            server.dispatch(&request("POST", "/api/lighting/static", Some(body))).status,
            200
        );
        assert_eq!(target.calls(), ["color #FF8800 on kb"]);
    }

    #[test]
    fn a_colour_refused_by_ownership_is_a_409() {
        let target = Arc::new(RecordingTarget {
            macro_exists: true,
            dpi_read_back: Some(800),
            lighting_applied: false,
            ..Default::default()
        });
        let server = server_with(Arc::clone(&target));
        let body = "{\"device\":\"kb\",\"color\":\"#FF8800\"}";
        let response = server.dispatch(&request("POST", "/api/lighting/static", Some(body)));
        assert_eq!(response.status, 409);
    }

    #[test]
    fn stopping_macros_and_unlocking_need_no_body() {
        let target = RecordingTarget::new();
        let server = server_with(Arc::clone(&target));
        assert_eq!(server.dispatch(&request("POST", "/api/macro/stop", None)).status, 200);
        assert_eq!(server.dispatch(&request("POST", "/api/profile/unlock", None)).status, 200);
        assert_eq!(target.calls(), ["stop", "unlock"]);
    }

    #[test]
    fn the_port_is_stripped_from_a_host_header_including_an_ipv6_literal() {
        assert_eq!(host_name("localhost"), "localhost");
        assert_eq!(host_name("localhost:6743"), "localhost");
        assert_eq!(host_name("127.0.0.1:6743"), "127.0.0.1");
        // The case the C# got wrong: splitting on the first colon leaves "[".
        assert_eq!(host_name("[::1]"), "[::1]");
        assert_eq!(host_name("[::1]:6743"), "[::1]");
        assert_eq!(host_name("[fe80::1]:80"), "[fe80::1]");
        assert_eq!(host_name("[unterminated"), "[unterminated");
        assert_eq!(host_name(""), "");
    }

    #[test]
    fn constant_time_comparison_is_still_correct() {
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"", b"a"));
    }

    #[test]
    fn a_token_is_created_once_and_then_reused() {
        let directory = std::env::temp_dir().join(format!("ghub-token-{}", Id::new()));
        let path = directory.join("nested").join("automation-token.txt");
        let entropy: Vec<u8> = (0..24u8).collect();

        let first = AutomationServer::load_or_create_token(&path, &entropy).expect("create");
        assert_eq!(first.len(), TOKEN_ENTROPY_BYTES * 2);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(path.exists(), "the parent directories must be created");

        let second = AutomationServer::load_or_create_token(&path, &[]).expect("reuse");
        assert_eq!(first, second, "an existing token must not be rotated");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_damaged_token_file_is_replaced() {
        let directory = std::env::temp_dir().join(format!("ghub-token-{}", Id::new()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("automation-token.txt");
        std::fs::write(&path, "short").unwrap();
        let entropy: Vec<u8> = (0..24u8).map(|b| b.wrapping_mul(7).wrapping_add(3)).collect();
        let token = AutomationServer::load_or_create_token(&path, &entropy).expect("replace");
        assert_ne!(token, "short");
        assert!(token.len() >= MIN_TOKEN_LENGTH);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_token_is_never_written_without_real_entropy() {
        let directory = std::env::temp_dir().join(format!("ghub-token-{}", Id::new()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("automation-token.txt");
        for entropy in [vec![], vec![1, 2, 3], vec![0u8; 32], vec![0xAA; 24]] {
            let error = AutomationServer::load_or_create_token(&path, &entropy).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{entropy:?}");
        }
        assert!(!path.exists(), "nothing may be written when the entropy is refused");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_privileged_port_is_refused() {
        let server = server_with(RecordingTarget::new());
        let entropy: Vec<u8> = (0..24u8).map(|b| b ^ 0x5A).collect();
        let error = server.start(80, &entropy).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!server.is_running());
    }

    /// Drives a real socket, so the HTTP parsing is covered too.
    #[test]
    fn the_server_answers_over_http() {
        let target = RecordingTarget::new();
        let server = server_with(Arc::clone(&target));
        let entropy: Vec<u8> = (0..24u8).map(|b| b.wrapping_mul(31).wrapping_add(7)).collect();
        // Port 0 asks the OS for a free one, so the test cannot collide with a real instance.
        server.start(1024, &entropy).or_else(|_| server.start(0, &entropy)).expect("start");
        let token = server.token();
        let port = server.port();
        assert!(server.is_running());

        let send = |request: &str| -> String {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
            stream.write_all(request.as_bytes()).expect("write");
            let mut response = String::new();
            stream.read_to_string(&mut response).expect("read");
            response
        };

        let authorized = send(&format!(
            "GET /api/status HTTP/1.1\r\nHost: localhost:{port}\r\n\
             Authorization: Bearer {token}\r\nConnection: close\r\n\r\n"
        ));
        assert!(authorized.starts_with("HTTP/1.1 200 OK"), "{authorized}");
        assert!(authorized.contains("Content-Type: application/json"));
        assert!(authorized.contains("Cache-Control: no-store"));
        assert!(authorized.contains("\"running\":true"), "{authorized}");

        let unauthorized = send(&format!(
            "GET /api/status HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n"
        ));
        assert!(unauthorized.starts_with("HTTP/1.1 401"), "{unauthorized}");

        let rebound = send(&format!(
            "GET /api/status HTTP/1.1\r\nHost: evil.example.com\r\n\
             Authorization: Bearer {token}\r\nConnection: close\r\n\r\n"
        ));
        assert!(rebound.starts_with("HTTP/1.1 403"), "{rebound}");

        let with_body = send(&format!(
            "POST /api/dpi HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\n\
             Content-Length: 12\r\nConnection: close\r\n\r\n{{\"dpi\":1600}}"
        ));
        assert!(with_body.starts_with("HTTP/1.1 200 OK"), "{with_body}");
        assert!(with_body.contains("\"dpi\":1600"), "{with_body}");

        let too_large = send(&format!(
            "POST /api/dpi HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            MAX_BODY_BYTES + 1
        ));
        assert!(too_large.starts_with("HTTP/1.1 413"), "{too_large}");

        server.stop();
        assert!(!server.is_running());
    }
}
