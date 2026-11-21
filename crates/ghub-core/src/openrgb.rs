//! Minimal OpenRGB SDK client: the port of `OpenRgb/OpenRgbClient.cs`.
//!
//! Protocol reference: <https://github.com/CalcProgrammer1/OpenRGB/blob/master/Documentation/OpenRGBSDK.md>.
//! Every value on the wire is **little-endian**, which is the one thing this file has in common
//! with nothing else in the project - HID++ is big-endian throughout, so the two must not share a
//! reader.
//!
//! Three properties carried over from the C# because each of them is a decision, not an accident:
//!
//! * **Non-loopback hosts are refused unless the user opts in.** The SDK has no authentication:
//!   anything that can reach the port can read and write every LED on the machine. Connecting
//!   outward to an arbitrary host is a thing the user has to ask for in words.
//! * **`min(client, server)` version negotiation, and the controller block is parsed *for that
//!   version*.** The block's layout gained fields in protocol 3, 4 and 5. Parsing a v4 block as
//!   v1 does not fail, it silently reads zone counts out of the middle of a mode descriptor.
//! * **Every failure leaves the client disconnected and never takes the host down.** OpenRGB is
//!   an optional integration; a user who does not run it must not see errors.
//!
//! ## Divergence from the C#
//!
//! The C# was async over `NetworkStream`. Here it is blocking with socket read and write
//! timeouts, which is both simpler and strictly better behaved: the C#'s `CancelAfter` left the
//! socket in an unknown state on timeout and relied on `DisconnectAsync` to clean up, while a
//! read timeout here is reported and the caller decides.
//!
//! The protocol-version request is also handled differently. A protocol-0 server never answers
//! it, so the C# waited for the full two-second timeout on every connect to such a server. Here
//! that one read uses a short timeout ([`VERSION_PROBE`]), because "no answer" is the expected
//! outcome, not a failure.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ghub_hidpp::color::LedColor;
use ghub_hidpp::identity::PeripheralKind;

use crate::diagnostics::DiagnosticsLog;

/// Highest protocol version this client parses.
pub const CLIENT_PROTOCOL_VERSION: u32 = 4;

const MAGIC: &[u8; 4] = b"ORGB";
const HEADER_LENGTH: usize = 16;

/// Refuse a payload larger than this rather than allocating what a hostile peer asks for.
const MAX_PAYLOAD: u32 = 16 * 1024 * 1024;

/// Default read and write timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(2000);

/// Timeout for the protocol-version reply only. A protocol-0 server never sends one, and that is
/// a supported server, so waiting the full timeout on every connect would be a self-inflicted
/// two-second stall.
pub const VERSION_PROBE: Duration = Duration::from_millis(300);

/// Controller count is bounded: the field is a `u32` from the network.
const MAX_CONTROLLERS: u32 = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum PacketId {
    RequestControllerCount = 0,
    RequestControllerData = 1,
    Ack = 10,
    RequestProtocolVersion = 40,
    SetClientName = 50,
    DeviceListUpdated = 100,
    UpdateLeds = 1050,
    UpdateZoneLeds = 1051,
    SetCustomMode = 1100,
}

impl PacketId {
    fn from_u32(value: u32) -> Option<Self> {
        Some(match value {
            0 => Self::RequestControllerCount,
            1 => Self::RequestControllerData,
            10 => Self::Ack,
            40 => Self::RequestProtocolVersion,
            50 => Self::SetClientName,
            100 => Self::DeviceListUpdated,
            1050 => Self::UpdateLeds,
            1051 => Self::UpdateZoneLeds,
            1100 => Self::SetCustomMode,
            _ => return None,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OpenRgbError {
    #[error("OpenRGB is not connected")]
    NotConnected,
    #[error("OpenRGB host '{0}' is not local; enable remote OpenRGB explicitly to connect")]
    RemoteHostRefused(String),
    #[error("OpenRGB did not answer {0:?}")]
    Timeout(PacketId),
    #[error("malformed OpenRGB data: {0}")]
    Protocol(String),
    #[error("OpenRGB connection failed: {0}")]
    Io(String),
}

pub type Result<T> = std::result::Result<T, OpenRgbError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Zone {
    pub index: usize,
    pub name: String,
    pub zone_type: i32,
    pub led_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Controller {
    pub index: usize,
    pub controller_type: i32,
    pub name: String,
    pub vendor: String,
    pub description: String,
    pub version: String,
    pub serial: String,
    pub location: String,
    pub zones: Vec<Zone>,
    pub leds: Vec<String>,
}

impl Controller {
    /// OpenRGB's `device_type` enum: 5 is a keyboard, 6 a mouse.
    pub fn kind(&self) -> PeripheralKind {
        match self.controller_type {
            5 => PeripheralKind::Keyboard,
            6 => PeripheralKind::Mouse,
            _ => PeripheralKind::Other,
        }
    }

    /// Whether this controller is a Logitech device, so the UI can warn that OpenRGB and the
    /// native path would be driving the same LEDs.
    pub fn is_logitech(&self) -> bool {
        let needle = "logitech";
        self.vendor.to_ascii_lowercase().contains(needle)
            || self.name.to_ascii_lowercase().contains(needle)
    }

    pub fn total_leds(&self) -> usize {
        self.leds.len()
    }
}

struct Connection {
    stream: TcpStream,
}

pub struct OpenRgbClient {
    log: Arc<DiagnosticsLog>,
    connection: Mutex<Option<Connection>>,
    state: Mutex<ClientState>,
    timeout: Mutex<Duration>,
    device_list_dirty: std::sync::atomic::AtomicBool,
}

#[derive(Debug, Default, Clone)]
struct ClientState {
    server_protocol_version: u32,
    negotiated_protocol_version: u32,
    controllers: Vec<Controller>,
    endpoint: Option<String>,
}

impl std::fmt::Debug for OpenRgbClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenRgbClient")
            .field("connected", &self.is_connected())
            .field(
                "protocol",
                &self.state.lock().map(|s| s.negotiated_protocol_version).unwrap_or(0),
            )
            .finish_non_exhaustive()
    }
}

impl OpenRgbClient {
    pub fn new(log: Arc<DiagnosticsLog>) -> Self {
        Self {
            log,
            connection: Mutex::new(None),
            state: Mutex::new(ClientState::default()),
            timeout: Mutex::new(DEFAULT_TIMEOUT),
            device_list_dirty: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn set_timeout(&self, timeout: Duration) {
        if let Ok(mut slot) = self.timeout.lock() {
            *slot = timeout;
        }
    }

    fn timeout(&self) -> Duration {
        self.timeout.lock().map(|t| *t).unwrap_or(DEFAULT_TIMEOUT)
    }

    pub fn is_connected(&self) -> bool {
        self.connection.lock().map(|c| c.is_some()).unwrap_or(false)
    }

    pub fn server_protocol_version(&self) -> u32 {
        self.state.lock().map(|s| s.server_protocol_version).unwrap_or(0)
    }

    pub fn negotiated_protocol_version(&self) -> u32 {
        self.state.lock().map(|s| s.negotiated_protocol_version).unwrap_or(0)
    }

    pub fn controllers(&self) -> Vec<Controller> {
        self.state.lock().map(|s| s.controllers.clone()).unwrap_or_default()
    }

    pub fn endpoint(&self) -> Option<String> {
        self.state.lock().ok()?.endpoint.clone()
    }

    /// Whether the server has announced a device-list change since this was last asked.
    ///
    /// The C# raised an event from inside the read loop. A flag the caller drains is better here:
    /// the notice arrives while another request is being read, and running a subscriber at that
    /// point would re-enter the client while it holds the socket.
    pub fn take_device_list_updated(&self) -> bool {
        self.device_list_dirty
            .swap(false, std::sync::atomic::Ordering::SeqCst)
    }

    /// Whether a host is loopback, and so safe to connect to without an explicit opt-in.
    pub fn is_loopback(host: &str) -> bool {
        if host.eq_ignore_ascii_case("localhost") {
            return true;
        }
        host.parse::<IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
    }

    /// Connects, negotiates the protocol version and reads the controller list.
    pub fn connect(&self, host: &str, port: u16, allow_remote: bool) -> Result<()> {
        self.disconnect();
        if !allow_remote && !Self::is_loopback(host) {
            return Err(OpenRgbError::RemoteHostRefused(host.to_owned()));
        }

        // OpenRGB listens on IPv4. Resolving "localhost" ourselves stops an IPv6-first resolver
        // from stalling the connect on an address nothing is listening on.
        let resolved: &str = if host.eq_ignore_ascii_case("localhost") { "127.0.0.1" } else { host };
        let address = resolve(resolved, port)?;
        let timeout = self.timeout();
        let stream = TcpStream::connect_timeout(&address, timeout)
            .map_err(|e| OpenRgbError::Io(e.to_string()))?;
        stream.set_nodelay(true).ok();
        stream
            .set_read_timeout(Some(timeout))
            .and_then(|()| stream.set_write_timeout(Some(timeout)))
            .map_err(|e| OpenRgbError::Io(e.to_string()))?;

        if let Ok(mut slot) = self.connection.lock() {
            *slot = Some(Connection { stream });
        }

        let outcome = self.handshake(resolved, port);
        if outcome.is_err() {
            self.disconnect();
        }
        outcome
    }

    fn handshake(&self, host: &str, port: u16) -> Result<()> {
        // The trailing NUL is part of the protocol, not C# string habit: the server reads the
        // name as a NUL-terminated string.
        self.send(0, PacketId::SetClientName, b"ghub-rs\0")?;
        self.send(
            0,
            PacketId::RequestProtocolVersion,
            &CLIENT_PROTOCOL_VERSION.to_le_bytes(),
        )?;
        let reply = self.receive_with(PacketId::RequestProtocolVersion, VERSION_PROBE, true)?;
        let server = reply
            .filter(|payload| payload.len() >= 4)
            .map(|payload| u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]))
            .unwrap_or(0);
        let negotiated = server.min(CLIENT_PROTOCOL_VERSION);
        if let Ok(mut state) = self.state.lock() {
            state.server_protocol_version = server;
            state.negotiated_protocol_version = negotiated;
            state.endpoint = Some(format!("{host}:{port}"));
        }
        self.log.info_with(
            "OpenRGB",
            format!("Connected to {host}:{port}"),
            format!("server protocol {server}, using {negotiated}"),
        );
        self.refresh_controllers()
    }

    /// Re-reads the controller list.
    pub fn refresh_controllers(&self) -> Result<()> {
        let negotiated = self.negotiated_protocol_version();
        self.send(0, PacketId::RequestControllerCount, &[])?;
        let payload = self
            .receive(PacketId::RequestControllerCount)?
            .ok_or(OpenRgbError::Timeout(PacketId::RequestControllerCount))?;
        if payload.len() < 4 {
            return Err(OpenRgbError::Protocol("controller count was truncated".to_owned()));
        }
        let count = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]])
            .min(MAX_CONTROLLERS);

        let mut controllers = Vec::with_capacity(count as usize);
        for index in 0..count {
            // Protocol 0 takes no version argument; sending one would be read as controller data.
            let request: Vec<u8> = if negotiated > 0 {
                negotiated.to_le_bytes().to_vec()
            } else {
                Vec::new()
            };
            self.send(index, PacketId::RequestControllerData, &request)?;
            let data = self
                .receive(PacketId::RequestControllerData)?
                .ok_or(OpenRgbError::Timeout(PacketId::RequestControllerData))?;
            controllers.push(parse_controller(index as usize, &data, negotiated)?);
        }

        if let Ok(mut state) = self.state.lock() {
            state.controllers = controllers;
        }
        Ok(())
    }

    /// Puts a controller in direct mode, so software colours are not overwritten by its own
    /// effect on the next tick.
    pub fn set_custom_mode(&self, controller: usize) -> Result<()> {
        self.send(controller as u32, PacketId::SetCustomMode, &[])
    }

    /// Sets every LED of a controller.
    pub fn update_leds(&self, controller: usize, colors: &[LedColor]) -> Result<()> {
        let mut payload = vec![0u8; 4 + 2 + 4 * colors.len()];
        let length = payload.len() as u32;
        payload[0..4].copy_from_slice(&length.to_le_bytes());
        payload[4..6].copy_from_slice(&(colors.len() as u16).to_le_bytes());
        for (i, color) in colors.iter().enumerate() {
            payload[6 + 4 * i] = color.r;
            payload[7 + 4 * i] = color.g;
            payload[8 + 4 * i] = color.b;
            // The fourth byte is padding in OpenRGB's RGBColor and stays zero.
        }
        self.send(controller as u32, PacketId::UpdateLeds, &payload)
    }

    /// Sets the LEDs of one zone.
    pub fn update_zone_leds(
        &self,
        controller: usize,
        zone: usize,
        colors: &[LedColor],
    ) -> Result<()> {
        let mut payload = vec![0u8; 4 + 4 + 2 + 4 * colors.len()];
        let length = payload.len() as u32;
        payload[0..4].copy_from_slice(&length.to_le_bytes());
        payload[4..8].copy_from_slice(&(zone as u32).to_le_bytes());
        payload[8..10].copy_from_slice(&(colors.len() as u16).to_le_bytes());
        for (i, color) in colors.iter().enumerate() {
            payload[10 + 4 * i] = color.r;
            payload[11 + 4 * i] = color.g;
            payload[12 + 4 * i] = color.b;
        }
        self.send(controller as u32, PacketId::UpdateZoneLeds, &payload)
    }

    fn send(&self, device_id: u32, id: PacketId, payload: &[u8]) -> Result<()> {
        let mut packet = Vec::with_capacity(HEADER_LENGTH + payload.len());
        packet.extend_from_slice(MAGIC);
        packet.extend_from_slice(&device_id.to_le_bytes());
        packet.extend_from_slice(&(id as u32).to_le_bytes());
        packet.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        packet.extend_from_slice(payload);

        let mut guard = self
            .connection
            .lock()
            .map_err(|_| OpenRgbError::Io("the OpenRGB lock is poisoned".to_owned()))?;
        let Some(connection) = guard.as_mut() else {
            return Err(OpenRgbError::NotConnected);
        };
        match connection.stream.write_all(&packet).and_then(|()| connection.stream.flush()) {
            Ok(()) => Ok(()),
            Err(e) => {
                // A write failure means the peer is gone. Drop the socket here, while the lock is
                // held, so no later call can use it.
                *guard = None;
                Err(OpenRgbError::Io(e.to_string()))
            }
        }
    }

    fn receive(&self, expected: PacketId) -> Result<Option<Vec<u8>>> {
        self.receive_with(expected, self.timeout(), false)
    }

    /// Reads until `expected` arrives.
    ///
    /// Device-list notices and acks arrive interleaved with replies, so they are consumed here
    /// rather than confusing the caller. With `allow_timeout`, a timeout returns `Ok(None)`
    /// instead of disconnecting - used only for the version probe.
    fn receive_with(
        &self,
        expected: PacketId,
        timeout: Duration,
        allow_timeout: bool,
    ) -> Result<Option<Vec<u8>>> {
        let mut guard = self
            .connection
            .lock()
            .map_err(|_| OpenRgbError::Io("the OpenRGB lock is poisoned".to_owned()))?;
        let Some(connection) = guard.as_mut() else {
            return Err(OpenRgbError::NotConnected);
        };
        let restore = connection.stream.read_timeout().ok().flatten();
        connection.stream.set_read_timeout(Some(timeout)).ok();

        let outcome = loop {
            let mut header = [0u8; HEADER_LENGTH];
            match read_exact(&mut connection.stream, &mut header) {
                Ok(()) => {}
                Err(ReadError::Timeout) if allow_timeout => break Ok(None),
                Err(e) => break Err(e.into_error(expected)),
            }
            if &header[0..4] != MAGIC {
                break Err(OpenRgbError::Protocol("bad packet magic".to_owned()));
            }
            let id = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
            let size = u32::from_le_bytes([header[12], header[13], header[14], header[15]]);
            if size > MAX_PAYLOAD {
                break Err(OpenRgbError::Protocol(format!("packet of {size} bytes is too large")));
            }
            let mut payload = vec![0u8; size as usize];
            if let Err(e) = read_exact(&mut connection.stream, &mut payload) {
                break Err(e.into_error(expected));
            }

            match PacketId::from_u32(id) {
                Some(PacketId::DeviceListUpdated) => {
                    self.device_list_dirty
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    continue;
                }
                Some(found) if found == expected => break Ok(Some(payload)),
                // Acks and anything else asynchronous are ignored, as in the C#.
                _ => continue,
            }
        };

        connection.stream.set_read_timeout(restore).ok();
        match outcome {
            Ok(value) => Ok(value),
            Err(e) => {
                // Any read failure other than an allowed timeout leaves the connection unusable:
                // we no longer know where we are in the stream.
                *guard = None;
                Err(e)
            }
        }
    }

    /// Whether the connection is still open, without sending anything.
    ///
    /// The supervising service polls this once a second, so it must not cost a round trip: a
    /// request-reply probe every second against a server that is simply idle would be traffic
    /// invented purely to ask a question the socket already answers. This is the C#'s
    /// `Poll(SelectRead) && Available == 0` - *readable with nothing to read* means the peer
    /// closed - written with a non-blocking peek, which is the same test the `Poll` call makes.
    ///
    /// A closed connection is dropped here rather than reported and left, because every other
    /// method would then have to handle a connection that is known-dead.
    pub fn check_alive(&self) -> bool {
        let alive = {
            let Ok(guard) = self.connection.lock() else { return false };
            let Some(connection) = guard.as_ref() else { return false };
            let stream = &connection.stream;
            if stream.set_nonblocking(true).is_err() {
                return false;
            }
            let mut byte = [0u8; 1];
            let probe = stream.peek(&mut byte);
            // Put back before the result is looked at. Leaving the socket non-blocking would
            // turn the next real read into an immediate spurious timeout, so a failure to
            // restore it is itself a reason to treat the connection as unusable.
            let restored = stream.set_nonblocking(false).is_ok();
            match (restored, probe) {
                (false, _) => false,
                // Readable and zero bytes: the peer shut the connection down.
                (true, Ok(0)) => false,
                // Bytes waiting - an unsolicited `DeviceListUpdated`, which is a sign of life.
                (true, Ok(_)) => true,
                // Nothing to read and still open: the healthy idle case.
                (true, Err(e)) => e.kind() == std::io::ErrorKind::WouldBlock,
            }
        };
        if !alive {
            self.disconnect();
        }
        alive
    }

    pub fn disconnect(&self) {
        if let Ok(mut guard) = self.connection.lock() {
            if let Some(connection) = guard.take() {
                let _ = connection.stream.shutdown(std::net::Shutdown::Both);
            }
        }
        if let Ok(mut state) = self.state.lock() {
            *state = ClientState::default();
        }
    }
}

impl Drop for OpenRgbClient {
    fn drop(&mut self) {
        self.disconnect();
    }
}

fn resolve(host: &str, port: u16) -> Result<SocketAddr> {
    (host, port)
        .to_socket_addrs()
        .map_err(|e| OpenRgbError::Io(e.to_string()))?
        // IPv4 first: OpenRGB binds IPv4, and an IPv6 address for the same name would connect to
        // nothing while the IPv4 one works.
        .min_by_key(|address| u8::from(address.is_ipv6()))
        .ok_or_else(|| OpenRgbError::Io(format!("{host} did not resolve to any address")))
}

enum ReadError {
    Timeout,
    Closed,
    Io(String),
}

impl ReadError {
    fn into_error(self, expected: PacketId) -> OpenRgbError {
        match self {
            Self::Timeout => OpenRgbError::Timeout(expected),
            Self::Closed => OpenRgbError::Io("OpenRGB closed the connection".to_owned()),
            Self::Io(message) => OpenRgbError::Io(message),
        }
    }
}

fn read_exact(stream: &mut TcpStream, buffer: &mut [u8]) -> std::result::Result<(), ReadError> {
    let mut read = 0;
    while read < buffer.len() {
        match stream.read(&mut buffer[read..]) {
            Ok(0) => return Err(ReadError::Closed),
            Ok(n) => read += n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err(ReadError::Timeout)
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(ReadError::Io(e.to_string())),
        }
    }
    Ok(())
}

/// Reads a controller data block for a negotiated protocol version.
///
/// Public so the parser can be tested against captured blocks without a server. Returns an error
/// on truncation rather than guessing: a short block means the version was negotiated wrongly,
/// and carrying on would attribute a mode's bytes to a zone.
pub fn parse_controller(index: usize, data: &[u8], protocol: u32) -> Result<Controller> {
    let mut r = Reader::new(data);
    r.u32()?; // data_size, which we do not trust over the actual length
    let controller_type = r.i32()?;
    let name = r.string()?;
    // Vendor appeared in protocol 1. Reading it from a v0 block would consume the description.
    let vendor = if protocol >= 1 { r.string()? } else { String::new() };
    let description = r.string()?;
    let version = r.string()?;
    let serial = r.string()?;
    let location = r.string()?;

    let modes = r.u16()?;
    r.i32()?; // active mode
    for _ in 0..modes {
        r.string()?; // name
        r.i32()?; // value
        r.u32()?; // flags
        r.u32()?; // speed min
        r.u32()?; // speed max
        if protocol >= 3 {
            r.u32()?; // brightness min
            r.u32()?; // brightness max
        }
        r.u32()?; // colors min
        r.u32()?; // colors max
        r.u32()?; // speed
        if protocol >= 3 {
            r.u32()?; // brightness
        }
        r.u32()?; // direction
        r.u32()?; // color mode
        let colors = r.u16()?;
        r.skip(4 * usize::from(colors))?;
    }

    let zone_count = r.u16()?;
    let mut zones = Vec::with_capacity(usize::from(zone_count));
    for zone_index in 0..zone_count {
        let name = r.string()?;
        let zone_type = r.i32()?;
        r.u32()?; // leds min
        r.u32()?; // leds max
        let led_count = r.u32()? as usize;
        let matrix_length = r.u16()?;
        r.skip(usize::from(matrix_length))?;
        if protocol >= 4 {
            let segments = r.u16()?;
            for _ in 0..segments {
                r.string()?; // name
                r.i32()?; // type
                r.u32()?; // start index
                r.u32()?; // led count
            }
        }
        if protocol >= 5 {
            r.u32()?; // zone flags
        }
        zones.push(Zone { index: usize::from(zone_index), name, zone_type, led_count });
    }

    let led_total = r.u16()?;
    let mut leds = Vec::with_capacity(usize::from(led_total));
    for _ in 0..led_total {
        leds.push(r.string()?);
        r.u32()?; // led value
    }

    let color_count = r.u16()?;
    r.skip(4 * usize::from(color_count))?;

    Ok(Controller {
        index,
        controller_type,
        name,
        vendor,
        description,
        version,
        serial,
        location,
        zones,
        leds,
    })
}

struct Reader<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.position.checked_add(n).ok_or_else(|| {
            OpenRgbError::Protocol("controller data length overflowed".to_owned())
        })?;
        if end > self.data.len() {
            return Err(OpenRgbError::Protocol(format!(
                "truncated controller data: wanted {n} bytes at offset {}, only {} remain",
                self.position,
                self.data.len().saturating_sub(self.position)
            )));
        }
        let slice = &self.data[self.position..end];
        self.position = end;
        Ok(slice)
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }

    /// A length-prefixed string. The length *includes* the NUL the server writes.
    fn string(&mut self) -> Result<String> {
        let length = usize::from(self.u16()?);
        let bytes = self.take(length)?;
        // Lossy, not strict: a device name with a stray byte must not fail the whole parse.
        Ok(String::from_utf8_lossy(bytes).trim_end_matches('\0').to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a controller data block for a given protocol version, the way a server does.
    struct BlockBuilder {
        bytes: Vec<u8>,
    }

    impl BlockBuilder {
        fn new() -> Self {
            Self { bytes: Vec::new() }
        }

        fn u32(mut self, value: u32) -> Self {
            self.bytes.extend_from_slice(&value.to_le_bytes());
            self
        }

        fn u16(mut self, value: u16) -> Self {
            self.bytes.extend_from_slice(&value.to_le_bytes());
            self
        }

        fn string(mut self, value: &str) -> Self {
            let mut bytes = value.as_bytes().to_vec();
            bytes.push(0);
            self.bytes.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
            self.bytes.extend_from_slice(&bytes);
            self
        }

        fn finish(self) -> Vec<u8> {
            // The real block starts with its own length; prepend it now that it is known.
            let total = (self.bytes.len() + 4) as u32;
            let mut out = total.to_le_bytes().to_vec();
            out.extend_from_slice(&self.bytes);
            out
        }
    }

    /// A keyboard with one mode, two zones and three LEDs, serialised for `protocol`.
    fn keyboard_block(protocol: u32) -> Vec<u8> {
        let mut b = BlockBuilder::new().u32(5); // device_type: keyboard
        b = b.string("G815 RGB MECHANICAL");
        if protocol >= 1 {
            b = b.string("Logitech");
        }
        b = b.string("A keyboard").string("1.0").string("SERIAL123").string("USB: 1-2");

        b = b.u16(1).u32(0); // one mode, active mode 0
        b = b.string("Direct").u32(0).u32(0).u32(0).u32(0); // name, value, flags, speed min/max
        if protocol >= 3 {
            b = b.u32(0).u32(100); // brightness min, max
        }
        b = b.u32(0).u32(0).u32(0); // colors min, colors max, speed
        if protocol >= 3 {
            b = b.u32(100); // brightness
        }
        b = b.u32(0).u32(0).u16(0); // direction, color mode, color count

        b = b.u16(2); // two zones
        for (name, leds) in [("Keyboard", 2u32), ("Logo", 1)] {
            b = b.string(name).u32(1).u32(0).u32(leds).u32(leds).u16(0);
            if protocol >= 4 {
                b = b.u16(0); // no segments
            }
            if protocol >= 5 {
                b = b.u32(0); // zone flags
            }
        }

        b = b.u16(3); // three LEDs
        for name in ["Key: A", "Key: B", "Logo"] {
            b = b.string(name).u32(0);
        }
        b = b.u16(0); // no colours
        b.finish()
    }

    #[test]
    fn a_protocol_4_block_parses_completely() {
        let controller = parse_controller(0, &keyboard_block(4), 4).expect("parse");
        assert_eq!(controller.name, "G815 RGB MECHANICAL");
        assert_eq!(controller.vendor, "Logitech");
        assert_eq!(controller.description, "A keyboard");
        assert_eq!(controller.serial, "SERIAL123");
        assert_eq!(controller.location, "USB: 1-2");
        assert_eq!(controller.kind(), PeripheralKind::Keyboard);
        assert!(controller.is_logitech());
        assert_eq!(controller.zones.len(), 2);
        assert_eq!(controller.zones[0].name, "Keyboard");
        assert_eq!(controller.zones[0].led_count, 2);
        assert_eq!(controller.zones[1].name, "Logo");
        assert_eq!(controller.leds, ["Key: A", "Key: B", "Logo"]);
        assert_eq!(controller.total_leds(), 3);
    }

    #[test]
    fn every_supported_protocol_version_round_trips() {
        for protocol in 0..=5 {
            let controller = parse_controller(0, &keyboard_block(protocol), protocol)
                .unwrap_or_else(|e| panic!("protocol {protocol} failed: {e}"));
            assert_eq!(controller.zones.len(), 2, "protocol {protocol}");
            assert_eq!(controller.leds.len(), 3, "protocol {protocol}");
            // Vendor only exists from protocol 1.
            if protocol >= 1 {
                assert_eq!(controller.vendor, "Logitech", "protocol {protocol}");
            } else {
                assert!(controller.vendor.is_empty());
            }
        }
    }

    #[test]
    fn parsing_a_newer_block_as_an_older_version_is_rejected_not_misread() {
        // This is the whole reason the version is negotiated. A v4 block read as v1 walks off the
        // end rather than silently reporting nonsense zones.
        let error = parse_controller(0, &keyboard_block(4), 1).unwrap_err();
        assert!(
            matches!(error, OpenRgbError::Protocol(_)),
            "expected a protocol error, got {error}"
        );
    }

    #[test]
    fn a_truncated_block_is_an_error_rather_than_a_partial_controller() {
        let full = keyboard_block(4);
        for cut in [0, 4, 12, full.len() / 2, full.len() - 1] {
            let error = parse_controller(0, &full[..cut], 4).unwrap_err();
            assert!(
                matches!(error, OpenRgbError::Protocol(_)),
                "cut at {cut} gave {error}"
            );
        }
    }

    #[test]
    fn a_non_logitech_controller_is_not_claimed() {
        let mut data = BlockBuilder::new().u32(6); // mouse
        data = data
            .string("Generic Mouse")
            .string("SomeVendor")
            .string("")
            .string("")
            .string("")
            .string("");
        data = data.u16(0).u32(0); // no modes
        data = data.u16(0); // no zones
        data = data.u16(0); // no LEDs
        data = data.u16(0); // no colours
        let controller = parse_controller(3, &data.finish(), 1).expect("parse");
        assert_eq!(controller.index, 3);
        assert_eq!(controller.kind(), PeripheralKind::Mouse);
        assert!(!controller.is_logitech());
        assert!(controller.zones.is_empty());
    }

    #[test]
    fn an_unknown_device_type_is_other() {
        let mut data = BlockBuilder::new().u32(99);
        data = data.string("Thing").string("V").string("").string("").string("").string("");
        data = data.u16(0).u32(0).u16(0).u16(0).u16(0);
        let controller = parse_controller(0, &data.finish(), 1).expect("parse");
        assert_eq!(controller.kind(), PeripheralKind::Other);
    }

    #[test]
    fn loopback_detection_accepts_only_local_addresses() {
        for host in ["localhost", "LOCALHOST", "127.0.0.1", "127.1.2.3", "::1"] {
            assert!(OpenRgbClient::is_loopback(host), "{host} should be loopback");
        }
        for host in ["192.168.1.10", "10.0.0.1", "example.com", "0.0.0.0", ""] {
            assert!(!OpenRgbClient::is_loopback(host), "{host} must not be loopback");
        }
    }

    #[test]
    fn a_remote_host_is_refused_without_an_explicit_opt_in() {
        let client = OpenRgbClient::new(Arc::new(DiagnosticsLog::default()));
        let error = client.connect("192.168.1.50", 6742, false).unwrap_err();
        assert!(
            matches!(error, OpenRgbError::RemoteHostRefused(ref host) if host == "192.168.1.50"),
            "got {error}"
        );
        assert!(!client.is_connected());
    }

    #[test]
    fn calls_before_connecting_report_that_rather_than_panicking() {
        let client = OpenRgbClient::new(Arc::new(DiagnosticsLog::default()));
        assert!(matches!(client.set_custom_mode(0), Err(OpenRgbError::NotConnected)));
        assert!(matches!(
            client.update_leds(0, &[LedColor { r: 1, g: 2, b: 3 }]),
            Err(OpenRgbError::NotConnected)
        ));
        assert!(matches!(client.refresh_controllers(), Err(OpenRgbError::NotConnected)));
        assert!(client.controllers().is_empty());
        assert_eq!(client.negotiated_protocol_version(), 0);
        assert_eq!(client.endpoint(), None);
        // Disconnecting when never connected must be harmless.
        client.disconnect();
    }

    #[test]
    fn the_device_list_flag_is_a_one_shot() {
        let client = OpenRgbClient::new(Arc::new(DiagnosticsLog::default()));
        assert!(!client.take_device_list_updated());
        client
            .device_list_dirty
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(client.take_device_list_updated());
        assert!(!client.take_device_list_updated(), "draining must clear it");
    }

    #[test]
    fn a_connection_to_a_closed_port_fails_without_hanging() {
        let client = OpenRgbClient::new(Arc::new(DiagnosticsLog::default()));
        client.set_timeout(Duration::from_millis(200));
        // Port 1 on loopback: nothing listens, so this is a refusal rather than a timeout.
        let error = client.connect("127.0.0.1", 1, false).unwrap_err();
        assert!(matches!(error, OpenRgbError::Io(_)), "got {error}");
        assert!(!client.is_connected());
    }

    /// Drives the client against a socket we control, so the framing is tested end to end.
    #[test]
    fn the_client_speaks_the_framing_a_server_expects() {
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut received: Vec<(u32, u32, Vec<u8>)> = Vec::new();

            let read_packet = |stream: &mut TcpStream| {
                let mut header = [0u8; HEADER_LENGTH];
                std::io::Read::read_exact(stream, &mut header).expect("header");
                assert_eq!(&header[0..4], MAGIC, "the client must send the ORGB magic");
                let device = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
                let id = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
                let size =
                    u32::from_le_bytes([header[12], header[13], header[14], header[15]]) as usize;
                let mut payload = vec![0u8; size];
                std::io::Read::read_exact(stream, &mut payload).expect("payload");
                (device, id, payload)
            };

            let write_packet = |stream: &mut TcpStream, device: u32, id: u32, payload: &[u8]| {
                let mut packet = MAGIC.to_vec();
                packet.extend_from_slice(&device.to_le_bytes());
                packet.extend_from_slice(&id.to_le_bytes());
                packet.extend_from_slice(&(payload.len() as u32).to_le_bytes());
                packet.extend_from_slice(payload);
                std::io::Write::write_all(stream, &packet).expect("write");
            };

            // SetClientName, then the version request.
            received.push(read_packet(&mut stream));
            received.push(read_packet(&mut stream));
            write_packet(&mut stream, 0, PacketId::RequestProtocolVersion as u32, &4u32.to_le_bytes());

            // An unsolicited device-list notice, which the client must swallow.
            write_packet(&mut stream, 0, PacketId::DeviceListUpdated as u32, &[]);

            // Controller count, then the one controller's data.
            received.push(read_packet(&mut stream));
            write_packet(
                &mut stream,
                0,
                PacketId::RequestControllerCount as u32,
                &1u32.to_le_bytes(),
            );
            received.push(read_packet(&mut stream));
            write_packet(
                &mut stream,
                0,
                PacketId::RequestControllerData as u32,
                &keyboard_block(4),
            );

            // Whatever the client sends next: the LED update.
            received.push(read_packet(&mut stream));
            received
        });

        let client = OpenRgbClient::new(Arc::new(DiagnosticsLog::default()));
        client.set_timeout(Duration::from_secs(5));
        client.connect("127.0.0.1", port, false).expect("connect");

        assert_eq!(client.server_protocol_version(), 4);
        assert_eq!(client.negotiated_protocol_version(), 4);
        assert_eq!(client.endpoint().as_deref(), Some(&format!("127.0.0.1:{port}")[..]));
        let controllers = client.controllers();
        assert_eq!(controllers.len(), 1);
        assert_eq!(controllers[0].name, "G815 RGB MECHANICAL");
        assert!(client.take_device_list_updated(), "the notice must have been seen");

        client
            .update_leds(0, &[LedColor { r: 0x11, g: 0x22, b: 0x33 }])
            .expect("update");

        let received = server.join().expect("the server thread");
        assert_eq!(received[0].1, PacketId::SetClientName as u32);
        assert_eq!(received[0].2, b"ghub-rs\0", "the name is NUL terminated on the wire");
        assert_eq!(received[1].1, PacketId::RequestProtocolVersion as u32);
        assert_eq!(received[1].2, 4u32.to_le_bytes());
        assert_eq!(received[2].1, PacketId::RequestControllerCount as u32);
        assert_eq!(received[3].1, PacketId::RequestControllerData as u32);
        assert_eq!(
            received[3].2,
            4u32.to_le_bytes(),
            "the controller request carries the negotiated version"
        );

        let (device, id, payload) = &received[4];
        assert_eq!(*id, PacketId::UpdateLeds as u32);
        assert_eq!(*device, 0);
        // 4 bytes of length, 2 of count, 4 per colour.
        assert_eq!(payload.len(), 10);
        assert_eq!(u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]), 10);
        assert_eq!(u16::from_le_bytes([payload[4], payload[5]]), 1);
        assert_eq!(&payload[6..10], &[0x11, 0x22, 0x33, 0x00]);
    }

    #[test]
    fn a_server_that_never_answers_the_version_request_is_treated_as_protocol_zero() {
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            // A protocol-0 server has never heard of packet 40, so it reads the request and says
            // nothing. It does answer the controller count.
            let read_packet = |stream: &mut TcpStream| {
                let mut header = [0u8; HEADER_LENGTH];
                std::io::Read::read_exact(stream, &mut header).expect("header");
                let size =
                    u32::from_le_bytes([header[12], header[13], header[14], header[15]]) as usize;
                let mut payload = vec![0u8; size];
                std::io::Read::read_exact(stream, &mut payload).expect("payload");
                u32::from_le_bytes([header[8], header[9], header[10], header[11]])
            };

            assert_eq!(read_packet(&mut stream), PacketId::SetClientName as u32);
            assert_eq!(read_packet(&mut stream), PacketId::RequestProtocolVersion as u32);
            // Deliberately no answer here: that is what makes this a protocol-0 server.
            assert_eq!(read_packet(&mut stream), PacketId::RequestControllerCount as u32);

            let mut packet = MAGIC.to_vec();
            packet.extend_from_slice(&0u32.to_le_bytes());
            packet.extend_from_slice(&(PacketId::RequestControllerCount as u32).to_le_bytes());
            packet.extend_from_slice(&4u32.to_le_bytes());
            packet.extend_from_slice(&0u32.to_le_bytes()); // zero controllers
            std::io::Write::write_all(&mut stream, &packet).expect("write");
            std::thread::sleep(Duration::from_millis(50));
        });

        let client = OpenRgbClient::new(Arc::new(DiagnosticsLog::default()));
        client.set_timeout(Duration::from_secs(2));
        let started = std::time::Instant::now();
        let outcome = client.connect("127.0.0.1", port, false);
        let elapsed = started.elapsed();
        let _ = server.join();

        assert!(outcome.is_ok(), "a protocol-0 server must still connect: {outcome:?}");
        assert_eq!(client.negotiated_protocol_version(), 0);
        assert!(client.controllers().is_empty());
        // The whole point of the short version probe: this must not have cost the full timeout.
        assert!(
            elapsed < Duration::from_millis(1500),
            "the version probe waited {elapsed:?}, which is the C#'s two-second stall"
        );
    }
}
