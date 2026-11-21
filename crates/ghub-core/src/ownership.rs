//! Exactly one writer per device zone. Port of
//! `Artemis.Peripherals/Lighting/LightingOwnershipService.cs`.
//!
//! Several things in this application can want to write the same LEDs: the animation engine, the
//! static painter, the OpenRGB adapter, the per-application lighting override. Without an arbiter
//! two of them write alternate frames and the keyboard flickers between two pictures, which looks
//! like broken hardware.
//!
//! Higher priority wins on request; **equal priority keeps the incumbent**, which is what stops
//! two equal writers handing the zone back and forth every frame. Frames are also throttled per
//! zone, because the HID link is the scarce resource, not the renderer.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

/// A component that wants to write LEDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LightingWriter {
    pub id: String,
    pub display_name: String,
    pub priority: i32,
}

impl LightingWriter {
    pub fn new(id: impl Into<String>, display_name: impl Into<String>, priority: i32) -> Self {
        Self { id: id.into(), display_name: display_name.into(), priority }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnershipChange {
    pub device_key: String,
    pub zone: String,
    pub previous: Option<LightingWriter>,
    pub current: Option<LightingWriter>,
}

/// The zone name meaning "the whole device".
pub const ALL_ZONES: &str = "*";

type Key = (String, String);

struct Inner {
    owners: HashMap<Key, LightingWriter>,
    last_frame: HashMap<Key, Instant>,
    /// Changes since the caller last drained them. The C# raised an event; collecting them means
    /// the caller is never called back while holding this lock.
    changes: Vec<OwnershipChange>,
}

pub struct LightingOwnership {
    inner: Mutex<Inner>,
    /// Maximum frames per second per zone, protecting the HID link's bandwidth.
    max_frames_per_second: Mutex<u32>,
}

impl Default for LightingOwnership {
    fn default() -> Self {
        Self::new()
    }
}

impl LightingOwnership {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                owners: HashMap::new(),
                last_frame: HashMap::new(),
                changes: Vec::new(),
            }),
            max_frames_per_second: Mutex::new(30),
        }
    }

    pub fn max_frames_per_second(&self) -> u32 {
        *self.max_frames_per_second.lock().unwrap()
    }

    pub fn set_max_frames_per_second(&self, fps: u32) {
        *self.max_frames_per_second.lock().unwrap() = fps.max(1);
    }

    /// Requests ownership. True when `writer` now owns the zone.
    ///
    /// A whole-device claim conflicts with every zone claim of that device, and the other way
    /// round: a writer painting the whole keyboard and one painting just its logo cannot both be
    /// right about what the logo shows.
    pub fn request(&self, device_key: &str, writer: &LightingWriter, zone: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let key = (device_key.to_owned(), zone.to_owned());
        let existing_here = inner.owners.get(&key).cloned();

        let conflicts: Vec<Key> = inner
            .owners
            .keys()
            .filter(|(device, owned_zone)| {
                device == device_key
                    && (zone == ALL_ZONES || owned_zone == ALL_ZONES || owned_zone == zone)
            })
            .cloned()
            .collect();

        let mut others: Vec<LightingWriter> = conflicts
            .iter()
            .map(|key| inner.owners[key].clone())
            .filter(|owner| owner.id != writer.id)
            .collect();

        // Strictly greater: an equal-priority newcomer leaves the incumbent alone, which is the
        // whole anti-flapping rule.
        if !others.iter().all(|o| writer.priority > o.priority) {
            return false;
        }

        others.sort_by_key(|o| -o.priority);
        // Who held this exact zone before, or failing that the strongest writer displaced by the
        // claim. Looking at `others` alone - as the C# did - loses the fact that the requester
        // already owned the zone, so re-requesting raised a spurious change every frame.
        let previous = existing_here.or_else(|| others.first().cloned());
        for conflict in conflicts {
            inner.owners.remove(&conflict);
        }
        inner.owners.insert(key, writer.clone());

        if previous.as_ref().map(|p| p.id.as_str()) != Some(writer.id.as_str()) {
            inner.changes.push(OwnershipChange {
                device_key: device_key.to_owned(),
                zone: zone.to_owned(),
                previous,
                current: Some(writer.clone()),
            });
        }
        true
    }

    /// Releases a zone, if this writer still owns it.
    pub fn release(&self, device_key: &str, writer: &LightingWriter, zone: &str) {
        let mut inner = self.inner.lock().unwrap();
        let key = (device_key.to_owned(), zone.to_owned());
        if inner.owners.get(&key).is_some_and(|owner| owner.id == writer.id) {
            inner.owners.remove(&key);
            inner.last_frame.remove(&key);
            inner.changes.push(OwnershipChange {
                device_key: device_key.to_owned(),
                zone: zone.to_owned(),
                previous: Some(writer.clone()),
                current: None,
            });
        }
    }

    /// Releases everything a writer owns, for a writer that was disconnected or switched off.
    pub fn release_all(&self, writer: &LightingWriter) {
        let keys: Vec<Key> = {
            let inner = self.inner.lock().unwrap();
            inner
                .owners
                .iter()
                .filter(|(_, owner)| owner.id == writer.id)
                .map(|(key, _)| key.clone())
                .collect()
        };
        for (device, zone) in keys {
            self.release(&device, writer, &zone);
        }
    }

    /// Who owns a zone: its own owner, or the whole-device owner.
    pub fn owner_of(&self, device_key: &str, zone: &str) -> Option<LightingWriter> {
        let inner = self.inner.lock().unwrap();
        inner
            .owners
            .get(&(device_key.to_owned(), zone.to_owned()))
            .or_else(|| inner.owners.get(&(device_key.to_owned(), ALL_ZONES.to_owned())))
            .cloned()
    }

    pub fn can_write(&self, device_key: &str, writer: &LightingWriter, zone: &str) -> bool {
        self.owner_of(device_key, zone).is_some_and(|owner| owner.id == writer.id)
    }

    /// Ownership plus throttle. False means the frame must be skipped.
    pub fn try_begin_frame(
        &self,
        device_key: &str,
        writer: &LightingWriter,
        zone: &str,
    ) -> bool {
        if !self.can_write(device_key, writer, zone) {
            return false;
        }
        let fps = self.max_frames_per_second();
        let min_interval = std::time::Duration::from_secs_f64(1.0 / fps as f64);
        let now = Instant::now();

        let mut inner = self.inner.lock().unwrap();
        let key = (device_key.to_owned(), zone.to_owned());
        if let Some(last) = inner.last_frame.get(&key) {
            if now.duration_since(*last) < min_interval {
                return false;
            }
        }
        inner.last_frame.insert(key, now);
        true
    }

    /// Ownership changes since the last call. Drained rather than raised as a callback, so no
    /// handler can run while the lock is held.
    pub fn take_changes(&self) -> Vec<OwnershipChange> {
        std::mem::take(&mut self.inner.lock().unwrap().changes)
    }

    /// Everything currently owned, for the diagnostics page.
    pub fn snapshot(&self) -> Vec<(String, String, LightingWriter)> {
        let inner = self.inner.lock().unwrap();
        let mut all: Vec<(String, String, LightingWriter)> = inner
            .owners
            .iter()
            .map(|((device, zone), owner)| (device.clone(), zone.clone(), owner.clone()))
            .collect();
        all.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICE: &str = "logitech-hidpp:u1";

    fn writer(id: &str, priority: i32) -> LightingWriter {
        LightingWriter::new(id, id, priority)
    }

    #[test]
    fn the_first_writer_gets_the_zone() {
        let ownership = LightingOwnership::new();
        let native = writer("native", 10);
        assert!(ownership.request(DEVICE, &native, ALL_ZONES));
        assert!(ownership.can_write(DEVICE, &native, ALL_ZONES));
        assert_eq!(ownership.owner_of(DEVICE, ALL_ZONES).unwrap().id, "native");

        let changes = ownership.take_changes();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].previous, None);
        assert_eq!(changes[0].current.as_ref().unwrap().id, "native");
        assert!(ownership.take_changes().is_empty(), "changes are drained");
    }

    #[test]
    fn higher_priority_takes_over_and_lower_priority_is_refused() {
        let ownership = LightingOwnership::new();
        let low = writer("low", 1);
        let high = writer("high", 2);

        assert!(ownership.request(DEVICE, &low, ALL_ZONES));
        assert!(ownership.request(DEVICE, &high, ALL_ZONES));
        assert!(!ownership.can_write(DEVICE, &low, ALL_ZONES));
        assert!(ownership.can_write(DEVICE, &high, ALL_ZONES));

        // The low writer cannot take it back.
        assert!(!ownership.request(DEVICE, &low, ALL_ZONES));
        assert_eq!(ownership.owner_of(DEVICE, ALL_ZONES).unwrap().id, "high");

        let changes = ownership.take_changes();
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[1].previous.as_ref().unwrap().id, "low");
        assert_eq!(changes[1].current.as_ref().unwrap().id, "high");
    }

    #[test]
    fn equal_priority_keeps_the_incumbent_so_nothing_flaps() {
        // Two equal writers requesting every frame would otherwise swap the zone back and forth
        // and the keyboard would flicker between two pictures.
        let ownership = LightingOwnership::new();
        let first = writer("first", 5);
        let second = writer("second", 5);
        assert!(ownership.request(DEVICE, &first, ALL_ZONES));
        for _ in 0..100 {
            assert!(!ownership.request(DEVICE, &second, ALL_ZONES));
        }
        assert_eq!(ownership.owner_of(DEVICE, ALL_ZONES).unwrap().id, "first");
        assert_eq!(ownership.take_changes().len(), 1, "only the first grant is a change");
    }

    #[test]
    fn re_requesting_a_zone_you_already_own_is_free_and_silent() {
        let ownership = LightingOwnership::new();
        let w = writer("w", 1);
        assert!(ownership.request(DEVICE, &w, ALL_ZONES));
        ownership.take_changes();
        assert!(ownership.request(DEVICE, &w, ALL_ZONES), "your own claim never conflicts");
        assert!(ownership.take_changes().is_empty(), "no change to report");
    }

    #[test]
    fn a_whole_device_claim_displaces_every_zone_claim() {
        let ownership = LightingOwnership::new();
        let per_zone = writer("per-zone", 1);
        let whole = writer("whole", 2);
        assert!(ownership.request(DEVICE, &per_zone, "zone:0"));
        assert!(ownership.request(DEVICE, &per_zone, "zone:1"));
        assert_eq!(ownership.snapshot().len(), 2);

        assert!(ownership.request(DEVICE, &whole, ALL_ZONES));
        assert_eq!(ownership.snapshot().len(), 1, "both zone claims were displaced");
        assert!(!ownership.can_write(DEVICE, &per_zone, "zone:0"));
        // And a zone with no claim of its own falls through to the whole-device owner.
        assert_eq!(ownership.owner_of(DEVICE, "zone:7").unwrap().id, "whole");
    }

    #[test]
    fn a_zone_claim_cannot_slip_past_a_whole_device_owner() {
        let ownership = LightingOwnership::new();
        let whole = writer("whole", 5);
        let sneak = writer("sneak", 5);
        assert!(ownership.request(DEVICE, &whole, ALL_ZONES));
        assert!(
            !ownership.request(DEVICE, &sneak, "zone:0"),
            "equal priority must not carve a zone out of a whole-device claim"
        );
        let stronger = writer("stronger", 9);
        assert!(ownership.request(DEVICE, &stronger, "zone:0"));
        assert!(!ownership.can_write(DEVICE, &whole, ALL_ZONES), "the whole claim was displaced");
    }

    #[test]
    fn two_devices_are_independent() {
        let ownership = LightingOwnership::new();
        let a = writer("a", 1);
        let b = writer("b", 1);
        assert!(ownership.request("device-a", &a, ALL_ZONES));
        assert!(ownership.request("device-b", &b, ALL_ZONES));
        assert!(ownership.can_write("device-a", &a, ALL_ZONES));
        assert!(ownership.can_write("device-b", &b, ALL_ZONES));
        assert!(!ownership.can_write("device-a", &b, ALL_ZONES));
    }

    #[test]
    fn releasing_only_works_for_the_current_owner() {
        let ownership = LightingOwnership::new();
        let owner = writer("owner", 2);
        let other = writer("other", 1);
        ownership.request(DEVICE, &owner, ALL_ZONES);
        ownership.take_changes();

        ownership.release(DEVICE, &other, ALL_ZONES);
        assert!(ownership.can_write(DEVICE, &owner, ALL_ZONES), "a non-owner cannot release it");
        assert!(ownership.take_changes().is_empty());

        ownership.release(DEVICE, &owner, ALL_ZONES);
        assert!(ownership.owner_of(DEVICE, ALL_ZONES).is_none());
        let changes = ownership.take_changes();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].current, None);
    }

    #[test]
    fn release_all_frees_every_zone_a_writer_held() {
        let ownership = LightingOwnership::new();
        let w = writer("w", 1);
        let other = writer("other", 1);
        ownership.request("device-a", &w, "zone:0");
        ownership.request("device-a", &w, "zone:1");
        ownership.request("device-b", &w, ALL_ZONES);
        ownership.request("device-c", &other, ALL_ZONES);
        assert_eq!(ownership.snapshot().len(), 4);

        ownership.release_all(&w);
        let left = ownership.snapshot();
        assert_eq!(left.len(), 1, "{left:?}");
        assert_eq!(left[0].2.id, "other");
    }

    #[test]
    fn after_a_release_a_previously_refused_writer_can_take_the_zone() {
        let ownership = LightingOwnership::new();
        let high = writer("high", 9);
        let low = writer("low", 1);
        ownership.request(DEVICE, &high, ALL_ZONES);
        assert!(!ownership.request(DEVICE, &low, ALL_ZONES));
        ownership.release(DEVICE, &high, ALL_ZONES);
        assert!(ownership.request(DEVICE, &low, ALL_ZONES));
    }

    #[test]
    fn the_frame_throttle_drops_frames_that_arrive_too_soon() {
        let ownership = LightingOwnership::new();
        ownership.set_max_frames_per_second(20); // one frame per 50 ms
        let w = writer("w", 1);
        ownership.request(DEVICE, &w, ALL_ZONES);

        assert!(ownership.try_begin_frame(DEVICE, &w, ALL_ZONES), "the first frame always passes");
        assert!(!ownership.try_begin_frame(DEVICE, &w, ALL_ZONES), "immediately again is too soon");
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert!(ownership.try_begin_frame(DEVICE, &w, ALL_ZONES));
    }

    #[test]
    fn a_non_owner_never_begins_a_frame() {
        let ownership = LightingOwnership::new();
        let owner = writer("owner", 2);
        let other = writer("other", 1);
        ownership.request(DEVICE, &owner, ALL_ZONES);
        assert!(!ownership.try_begin_frame(DEVICE, &other, ALL_ZONES));
        // And a device nobody owns is not writable either.
        assert!(!ownership.try_begin_frame("nobody", &owner, ALL_ZONES));
    }

    #[test]
    fn the_throttle_is_per_zone_not_per_device() {
        // Two zones of one device each get their own budget; sharing one would halve the frame
        // rate of a two-zone mouse for no reason.
        let ownership = LightingOwnership::new();
        ownership.set_max_frames_per_second(20);
        let w = writer("w", 1);
        ownership.request(DEVICE, &w, "zone:0");
        ownership.request(DEVICE, &w, "zone:1");
        assert!(ownership.try_begin_frame(DEVICE, &w, "zone:0"));
        assert!(ownership.try_begin_frame(DEVICE, &w, "zone:1"));
        assert!(!ownership.try_begin_frame(DEVICE, &w, "zone:0"));
    }

    #[test]
    fn a_zero_frame_rate_is_clamped_rather_than_dividing_by_zero() {
        let ownership = LightingOwnership::new();
        ownership.set_max_frames_per_second(0);
        assert_eq!(ownership.max_frames_per_second(), 1);
        let w = writer("w", 1);
        ownership.request(DEVICE, &w, ALL_ZONES);
        assert!(ownership.try_begin_frame(DEVICE, &w, ALL_ZONES));
    }

    #[test]
    fn the_snapshot_is_ordered_so_the_diagnostics_page_does_not_reshuffle() {
        let ownership = LightingOwnership::new();
        let w = writer("w", 1);
        ownership.request("device-b", &w, "zone:1");
        ownership.request("device-a", &w, "zone:2");
        ownership.request("device-a", &w, "zone:1");
        let snapshot = ownership.snapshot();
        let keys: Vec<(&str, &str)> =
            snapshot.iter().map(|(d, z, _)| (d.as_str(), z.as_str())).collect();
        assert_eq!(keys, vec![("device-a", "zone:1"), ("device-a", "zone:2"), ("device-b", "zone:1")]);
    }
}
