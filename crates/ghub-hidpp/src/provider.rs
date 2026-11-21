//! Device discovery and channel sharing. Port of
//! `Artemis.Peripherals/Logitech/LogitechHidppProvider.cs`.
//!
//! Enumeration groups a device's HID collections back into their USB interface, opens one channel
//! per interface, and probes it: a receiver is asked about each of its slots, a directly attached
//! device about index 0xFF. The channel is shared, because a receiver's slots all talk over the
//! same pair of collections and opening a second channel on them would have two readers racing for
//! the same reports.
//!
//! What a device is *called* and what it can *do* is the session's job. This layer answers one
//! question: which devices are there, and under what stable key.

use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::channel::{HidppChannel, RequestOptions};
use crate::device::{HidppDevice, HidppFeatureInfo, feature};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{HidppError, Result};
use crate::hid::{HidBackend, HidInterfaceInfo, LOGITECH_VID};
use crate::identity::DeviceIdentity;
use crate::model;
use crate::report::HidppReport;
use crate::report::constants::WIRED_DEVICE_INDEX;
use crate::session::{Candidate, LogitechHidppSession, SessionOptions};
use crate::sha256;

pub const PROVIDER_ID: &str = "logitech-hidpp";

#[derive(Debug, Clone)]
pub struct ProviderOptions {
    /// Onboard (flash) profile writes. Off by default.
    pub allow_onboard_writes: bool,
    /// Receiver slots probed for paired devices. A Unifying receiver holds six.
    pub max_receiver_slots: u8,
    /// Ping timeout. Short on purpose: most slots are empty, and an empty slot costs this long.
    pub ping_timeout: Duration,
    /// Per-key LED ids by product id, supplied by the caller because the layouts live above this
    /// crate. A product id that is absent gets zone lighting rather than per-key.
    pub per_key_led_ids: HashMap<u16, Vec<u8>>,
}

impl Default for ProviderOptions {
    fn default() -> Self {
        Self {
            allow_onboard_writes: false,
            max_receiver_slots: 6,
            ping_timeout: Duration::from_millis(400),
            per_key_led_ids: HashMap::new(),
        }
    }
}

/// A device the provider can see but has not opened a session on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredDevice {
    pub identity: DeviceIdentity,
    pub display_name: String,
    /// Opaque handle for [`LogitechHidppProvider::connect`]. Never persisted: it holds a HID path
    /// group key, which Windows changes on reconnect.
    pub candidate: Candidate,
}

/// One open channel plus the collections it was built from, shared by every device behind it.
struct SharedChannel {
    channel: Arc<HidppChannel>,
    interfaces: Vec<HidInterfaceInfo>,
    /// Sessions currently using this channel. The channel outlives them all until it is both
    /// unreferenced and disconnected, because a receiver whose device is asleep is still a
    /// perfectly good channel.
    references: usize,
    /// The channel's notification stream, until the owner takes it.
    ///
    /// Kept here rather than dropped so the device manager can pump it. A dropped receiver makes
    /// the channel discard every notification, which silently disables battery reporting, G-key
    /// bindings and link-loss detection - the three things that are hardest to notice are
    /// missing, because nothing errors.
    notifications: Option<Receiver<HidppReport>>,
}

pub struct LogitechHidppProvider {
    backend: Arc<dyn HidBackend>,
    options: ProviderOptions,
    /// The live value of [`ProviderOptions::allow_onboard_writes`].
    ///
    /// Separate from `options` because this one setting is the user's to change while the
    /// application runs: it is a switch in Options, and requiring a restart to turn flash writes
    /// *off* again would be the wrong way round for a safety setting. Read when a session is
    /// opened, so a session already open keeps the value it was opened with - which is why the
    /// Options page says to scan after changing it.
    allow_onboard_writes: AtomicBool,
    channels: Mutex<HashMap<String, SharedChannel>>,
}

impl LogitechHidppProvider {
    pub fn new(backend: Arc<dyn HidBackend>, options: ProviderOptions) -> Self {
        let allow_onboard_writes = AtomicBool::new(options.allow_onboard_writes);
        Self { backend, options, allow_onboard_writes, channels: Mutex::new(HashMap::new()) }
    }

    /// Whether a session opened from now on may write onboard flash.
    pub fn allow_onboard_writes(&self) -> bool {
        self.allow_onboard_writes.load(Ordering::SeqCst)
    }

    /// Changes that, for sessions opened from now on.
    pub fn set_allow_onboard_writes(&self, allow: bool) {
        self.allow_onboard_writes.store(allow, Ordering::SeqCst);
    }

    pub fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    pub fn display_name(&self) -> &'static str {
        "Logitech HID++ (native)"
    }

    pub fn options(&self) -> &ProviderOptions {
        &self.options
    }

    /// Enumerates every HID++ device reachable right now.
    ///
    /// Never fails for one bad device: an interface that cannot be opened, or a slot that answers
    /// nonsense, is logged and skipped. The only `Err` is a backend-wide enumeration failure.
    pub fn enumerate(&self) -> Result<Vec<DiscoveredDevice>> {
        let interfaces = self.backend.enumerate(LOGITECH_VID).map_err(|e| {
            HidppError::Hid(format!("HID enumeration failed: {e}"))
        })?;

        // Grouped by USB interface, keeping only the collections that actually speak HID++. A
        // vendor-defined collection is not enough: a receiver also exposes its DJ interface and a
        // 2-byte connect-button collection on vendor pages.
        let mut groups: HashMap<String, Vec<HidInterfaceInfo>> = HashMap::new();
        for info in interfaces.iter().filter(|i| i.is_hidpp()) {
            groups.entry(info.group_key.clone()).or_default().push(info.clone());
        }

        let mut channels = self.channels.lock().unwrap();

        // Drop channels whose interface has gone away. Keyed on *all* enumerated collections, not
        // just the HID++ ones: a device whose descriptor we failed to parse this time round is
        // still present, and tearing its channel down would drop the session with it.
        let present: std::collections::HashSet<&str> =
            interfaces.iter().map(|i| i.group_key.as_str()).collect();
        channels.retain(|key, _| {
            let kept = present.contains(key.as_str());
            if !kept {
                log::info!("{PROVIDER_ID}: interface {key} vanished, dropping its channel");
            }
            kept
        });

        // Deterministic order, so the device list does not reshuffle between scans.
        let mut keys: Vec<&String> = groups.keys().collect();
        keys.sort();

        let mut found = Vec::new();
        for key in keys {
            let group = &groups[key];
            let product_id = group[0].product_id;
            let shared = match Self::get_or_open_channel(&mut channels, &*self.backend, key, group)
            {
                Ok(shared) => shared,
                Err(e) => {
                    log::warn!("{PROVIDER_ID}: skipping HID interface {key} ({product_id:04X}): {e}");
                    continue;
                }
            };

            if model::is_receiver(product_id) {
                for slot in 1..=self.options.max_receiver_slots {
                    match self.probe(shared, key, product_id, slot) {
                        Ok(Some(device)) => found.push(device),
                        Ok(None) => {}
                        Err(e) => log::warn!(
                            "{PROVIDER_ID}: receiver {product_id:04X} slot {slot} probe failed: {e}"
                        ),
                    }
                }
            } else {
                match self.probe(shared, key, product_id, WIRED_DEVICE_INDEX) {
                    Ok(Some(device)) => found.push(device),
                    Ok(None) => {}
                    Err(e) => {
                        log::warn!("{PROVIDER_ID}: {product_id:04X} probe failed: {e}")
                    }
                }
            }
        }

        Ok(found)
    }

    fn get_or_open_channel<'c>(
        channels: &'c mut HashMap<String, SharedChannel>,
        backend: &dyn HidBackend,
        group_key: &str,
        interfaces: &[HidInterfaceInfo],
    ) -> Result<&'c SharedChannel> {
        // A channel that is still connected is reused; one that has died is replaced, keeping the
        // reference count so sessions on it still release correctly.
        if channels.get(group_key).is_some_and(|s| s.channel.is_connected()) {
            return Ok(&channels[group_key]);
        }

        let mut connections = Vec::new();
        for info in interfaces {
            match backend.open(info) {
                Ok(connection) => connections.push(connection),
                // One unopenable collection is normal: another HID++ client may hold it, or it may
                // be a collection the OS will not hand over. The channel needs only one.
                Err(e) => log::warn!("{PROVIDER_ID}: cannot open {}: {e}", info.path),
            }
        }
        if connections.is_empty() {
            return Err(HidppError::Hid(
                "no HID++ collection could be opened (held exclusively by another application?)"
                    .into(),
            ));
        }

        let (channel, notifications) = HidppChannel::new(connections, group_key)?;
        let previous = channels.get(group_key).map(|s| s.references).unwrap_or(0);
        channels.insert(
            group_key.to_owned(),
            SharedChannel {
                channel: Arc::new(channel),
                interfaces: interfaces.to_vec(),
                references: previous,
                notifications: Some(notifications),
            },
        );
        Ok(&channels[group_key])
    }

    /// Asks one device index whether anything is there.
    ///
    /// `Ok(None)` is the ordinary answer for an empty or sleeping receiver slot: the receiver
    /// replies with a HID++ 1.0 `UnknownDevice` error, or nothing at all.
    fn probe(
        &self,
        shared: &SharedChannel,
        group_key: &str,
        product_id: u16,
        index: u8,
    ) -> Result<Option<DiscoveredDevice>> {
        let mut device = HidppDevice::new(Arc::clone(&shared.channel), index);
        let options = RequestOptions::default().with_timeout(self.options.ping_timeout);

        match device.ping(options) {
            Ok(_) => {}
            Err(HidppError::Timeout { .. }) => return Ok(None),
            Err(HidppError::Protocol(e)) if e.is_unknown_device() => return Ok(None),
            Err(e) => return Err(e),
        }

        // 0x0003 is how the stable id is built, so it is queried directly rather than through a
        // full feature discovery: that happens once, in the session.
        let info = device.query_feature(feature::DEVICE_INFORMATION)?;
        let (stable_id, name) = match info {
            Some(info) => {
                device.install_features([
                    HidppFeatureInfo { feature_id: feature::ROOT, index: 0, version: 0, flags: 0 },
                    info,
                ]);
                let raw = device.call(
                    feature::DEVICE_INFORMATION,
                    0,
                    &[],
                    RequestOptions::default(),
                )?;
                let model_ids =
                    [raw.param_u16(7), raw.param_u16(9), raw.param_u16(11)];
                let name = model::by_model_ids(&model_ids)
                    .map(|m| m.name.to_owned())
                    .unwrap_or_else(|| format!("Logitech device {product_id:04X}"));
                (stable_id_from_unit_id(&unit_id(&raw), raw.param_u16(7)), name)
            }
            // No 0x0003 means no unit id to key on. Falling back to the HID path group makes the
            // key unstable across reconnects, which is worse than nothing for profile matching -
            // so it is marked, and the profile store treats a `p` key as session-scoped.
            None => (
                format!("p{:04X}-{}-{index:02X}", product_id, sha256::digest_hex(group_key.as_bytes(), 4)),
                format!("Logitech device {product_id:04X}"),
            ),
        };

        Ok(Some(DiscoveredDevice {
            identity: DeviceIdentity::new(PROVIDER_ID, stable_id),
            display_name: name,
            candidate: Candidate {
                group_key: group_key.to_owned(),
                product_id,
                device_index: index,
            },
        }))
    }

    /// Opens a session on a device returned by [`Self::enumerate`].
    pub fn connect(&self, device: &DiscoveredDevice) -> Result<LogitechHidppSession> {
        let channel = {
            let mut channels = self.channels.lock().unwrap();
            let shared = channels.get_mut(&device.candidate.group_key).ok_or_else(|| {
                HidppError::Hid(format!(
                    "interface {} is no longer present",
                    device.candidate.group_key
                ))
            })?;
            // The channel can die between enumeration and connect - a receiver that went to
            // sleep, a cable pulled and pushed back. Reopening from the collections recorded at
            // enumeration is cheap and keeps the device's identity, so the caller is not forced
              // to re-enumerate for what is usually a momentary gap.
            if !shared.channel.is_connected() {
                let interfaces = shared.interfaces.clone();
                let references = shared.references;
                log::info!(
                    "{PROVIDER_ID}: channel {} had gone; reopening its {} collection(s)",
                    device.candidate.group_key,
                    interfaces.len()
                );
                Self::get_or_open_channel(
                    &mut channels,
                    &*self.backend,
                    &device.candidate.group_key,
                    &interfaces,
                )?;
                let shared = channels.get_mut(&device.candidate.group_key).expect("just inserted");
                shared.references = references + 1;
                Arc::clone(&shared.channel)
            } else {
                shared.references += 1;
                Arc::clone(&shared.channel)
            }
        };

        let options = SessionOptions {
            allow_onboard_writes: self.allow_onboard_writes(),
            per_key_led_ids: self
                .options
                .per_key_led_ids
                .get(&device.candidate.product_id)
                .cloned()
                .unwrap_or_default(),
        };

        let result = LogitechHidppSession::open(
            channel,
            device.candidate.clone(),
            device.identity.clone(),
            options,
        );
        if result.is_err() {
            self.release(&device.candidate.group_key);
        }
        result
    }

    /// Gives back a session's share of its channel. Call when a session is dropped.
    pub fn release(&self, group_key: &str) {
        let mut channels = self.channels.lock().unwrap();
        let Some(shared) = channels.get_mut(group_key) else { return };
        shared.references = shared.references.saturating_sub(1);
        // A disconnected channel with no users left is of no further use; a connected one is kept,
        // because its paired device may simply be asleep and will come back on the same channel.
        if shared.references == 0 && !shared.channel.is_connected() {
            channels.remove(group_key);
        }
    }

    /// Takes a channel's notification stream, once.
    ///
    /// The device manager calls this after its first [`Self::connect`] on a group key and pumps
    /// the receiver on its own thread. A second call returns `None`, because a stream has exactly
    /// one consumer; a channel reopened after a disconnect produces a fresh stream, which the
    /// manager must take again.
    pub fn take_notifications(&self, group_key: &str) -> Option<Receiver<HidppReport>> {
        self.channels.lock().unwrap().get_mut(group_key)?.notifications.take()
    }

    /// The channel behind a group key, for callers that need to ask whether it is still alive.
    pub fn channel(&self, group_key: &str) -> Option<Arc<HidppChannel>> {
        self.channels.lock().unwrap().get(group_key).map(|s| Arc::clone(&s.channel))
    }

    pub fn open_channel_count(&self) -> usize {
        self.channels.lock().unwrap().len()
    }
}

/// The four unit-id bytes out of a `0x0003` function 0 reply.
fn unit_id(raw: &HidppReport) -> [u8; 4] {
    [raw.param(1), raw.param(2), raw.param(3), raw.param(4)]
}

/// Builds the profile key from a device's HID++ unit id.
///
/// The unit id is identical whether the device is on its cable or its receiver, which is what
/// prevents one mouse growing two profiles when it is unplugged. It is hashed so the raw id never
/// reaches a log line or a profile file.
///
/// A device that does not fill the unit id in gets the model id mixed in, because an all-zero unit
/// id would otherwise hash every such device to the same key - and they would share one profile.
pub fn stable_id_from_unit_id(unit_id: &[u8], model_id: u16) -> String {
    let all_zero = unit_id.iter().all(|b| *b == 0);
    let input: Vec<u8> = if all_zero {
        let mut v = unit_id.to_vec();
        v.extend_from_slice(&model_id.to_be_bytes());
        v
    } else {
        unit_id.to_vec()
    };
    format!("u{}", sha256::digest_hex(&input, 8))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hid::group_key;
    use crate::replay::{HidTranscript, ReplayHidBackend, TranscriptFrame};
    use crate::features::test_support::long;

    #[test]
    fn the_stable_id_survives_a_change_of_link() {
        // Same unit id, different model id: the same mouse, once wired and once wireless. The key
        // must not change, or unplugging the cable would hand the user a fresh empty profile.
        let wired = stable_id_from_unit_id(&[0x11, 0x22, 0x33, 0x44], 0xC091);
        let wireless = stable_id_from_unit_id(&[0x11, 0x22, 0x33, 0x44], 0x4087);
        assert_eq!(wired, wireless);
        assert!(wired.starts_with('u'));
        assert_eq!(wired.len(), 17, "u plus 8 bytes of hex");
    }

    #[test]
    fn the_raw_unit_id_never_appears_in_the_key() {
        let key = stable_id_from_unit_id(&[0xDE, 0xAD, 0xBE, 0xEF], 0xC091);
        assert!(!key.contains("deadbeef"), "the unit id leaked into {key}");
        assert!(!key.contains("DEADBEEF"));
    }

    #[test]
    fn devices_with_no_unit_id_still_get_distinct_keys() {
        // Firmware that leaves the unit id zero would otherwise hash two different mice to one
        // key, and they would share a profile.
        let a = stable_id_from_unit_id(&[0, 0, 0, 0], 0xC091);
        let b = stable_id_from_unit_id(&[0, 0, 0, 0], 0xC086);
        assert_ne!(a, b);
        // And a real unit id must not collide with the zero case for the same model.
        assert_ne!(a, stable_id_from_unit_id(&[0, 0, 0, 1], 0xC091));
    }

    const RECEIVER_COL01: &str =
        r"\\?\hid#vid_046d&pid_c539&mi_02&col01#8&38ddd24c&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";

    /// A replay backend with one short and one long collection, as a receiver has.
    fn receiver_backend(pairs: &[(&str, &str)]) -> ReplayHidBackend {
        let frames = pairs
            .iter()
            .flat_map(|(out, reply)| {
                [
                    TranscriptFrame { outgoing: true, data: long(out) },
                    TranscriptFrame { outgoing: false, data: long(reply) },
                ]
            })
            .collect();
        ReplayHidBackend::new(
            &HidTranscript { device: "receiver".into(), frames },
            0xC539,
            true,
        )
    }

    #[test]
    fn an_empty_receiver_slot_is_skipped_rather_than_reported() {
        // Slot 1 answers with the HID++ 1.0 UnknownDevice error (0x8F sub id, code 0x09); the
        // other slots answer nothing at all. Neither is a device.
        let backend = Arc::new(receiver_backend(&[("1101001A00005A", "118F011A09")]));
        let provider = LogitechHidppProvider::new(
            backend,
            ProviderOptions {
                max_receiver_slots: 2,
                ping_timeout: Duration::from_millis(30),
                ..ProviderOptions::default()
            },
        );
        let found = provider.enumerate().expect("enumeration must not fail");
        assert!(found.is_empty(), "{found:?}");
        assert_eq!(provider.open_channel_count(), 1, "the channel itself stays open");
    }

    #[test]
    fn a_paired_device_is_discovered_under_a_hashed_key() {
        let backend = Arc::new(receiver_backend(&[
            // Slot 1: ping, then Root.getFeature(0x0003) -> index 2, then getDeviceInfo.
            ("1101001A00005A", "1101001A04055A"),
            ("1101000A0003", "1101000A020001"),
            ("1101020A", "1101020A011122334400044087C0910000"),
            // Slot 2: nothing paired.
            ("1102001A00005A", "118F021A09"),
        ]));
        let provider = LogitechHidppProvider::new(
            backend,
            ProviderOptions {
                max_receiver_slots: 2,
                ping_timeout: Duration::from_millis(30),
                ..ProviderOptions::default()
            },
        );

        let found = provider.enumerate().expect("enumeration must not fail");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].display_name, "G903 LIGHTSPEED HERO");
        assert_eq!(found[0].candidate.device_index, 1);
        assert_eq!(found[0].candidate.product_id, 0xC539, "the channel is the receiver's");
        assert_eq!(found[0].identity.provider_id, PROVIDER_ID);
        assert_eq!(
            found[0].identity.stable_id,
            stable_id_from_unit_id(&[0x11, 0x22, 0x33, 0x44], 0x4087)
        );

        // Enumerating again must return the same key and reuse the same channel, not open a
        // second one that would race the first reader for the same reports.
        let again = provider.enumerate().unwrap();
        assert_eq!(again, found);
        assert_eq!(provider.open_channel_count(), 1);
    }

    #[test]
    fn a_device_with_no_device_information_feature_is_marked_as_having_an_unstable_key() {
        let backend = Arc::new(receiver_backend(&[
            ("1101001A00005A", "1101001A04055A"),
            // Root.getFeature answers index 0, which for anything but Root itself means "absent".
            ("1101000A0003", "1101000A000000"),
        ]));
        let provider = LogitechHidppProvider::new(
            backend,
            ProviderOptions {
                max_receiver_slots: 1,
                ping_timeout: Duration::from_millis(30),
                ..ProviderOptions::default()
            },
        );
        let found = provider.enumerate().unwrap();
        assert_eq!(found.len(), 1);
        assert!(
            found[0].identity.stable_id.starts_with('p'),
            "a path-derived key must be distinguishable from a unit-id one: {}",
            found[0].identity.stable_id
        );
        assert_eq!(found[0].display_name, "Logitech device C539");
    }

    /// Flash writes stay off unless something explicitly turns them on, and the switch has to
    /// work both ways: a safety setting that needs a restart to re-engage is not a safety setting.
    #[test]
    fn onboard_writes_are_off_by_default_and_can_be_switched_both_ways() {
        let provider = LogitechHidppProvider::new(
            Arc::new(receiver_backend(&[])),
            ProviderOptions::default(),
        );
        assert!(!provider.allow_onboard_writes());
        provider.set_allow_onboard_writes(true);
        assert!(provider.allow_onboard_writes());
        provider.set_allow_onboard_writes(false);
        assert!(!provider.allow_onboard_writes());
    }

    #[test]
    fn only_hidpp_collections_are_grouped_into_a_channel() {
        // The replay backend hands out one short and one long collection, which is what a
        // receiver looks like; group_key must fold them into one interface.
        let backend = receiver_backend(&[]);
        let interfaces = backend.enumerate(LOGITECH_VID).unwrap();
        assert!(interfaces.len() >= 2);
        let keys: std::collections::HashSet<&str> =
            interfaces.iter().map(|i| i.group_key.as_str()).collect();
        assert_eq!(keys.len(), 1, "both collections belong to one interface: {keys:?}");
        assert!(interfaces.iter().all(|i| i.is_hidpp()));
        // Sanity-check the key builder against a real receiver path.
        assert_eq!(group_key(RECEIVER_COL01), group_key(&RECEIVER_COL01.to_uppercase()));
    }
}
