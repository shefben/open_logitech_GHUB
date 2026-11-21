//! Stable identifiers for profiles, macros and automation rules.
//!
//! The C# used `System.Guid`. A UUID crate would work, but all this needs is "a 128-bit value that
//! will not collide with another one made on this machine", and these ids go into profile JSON
//! that a user may hand-edit, so they are printed as plain hex.
//!
//! Not a cryptographic identifier and not a v4 UUID: it is a counter and a clock reading mixed
//! with SplitMix64. Nothing here depends on an id being unguessable.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

static COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Id(pub u128);

impl Id {
    /// The nil id, which no generated id ever equals. Used for "unset" in imported data.
    pub const NIL: Self = Self(0);

    pub fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        // Two independently mixed halves: the clock alone repeats under a coarse timer, and the
        // counter alone repeats across restarts.
        let high = splitmix64(nanos ^ 0x9E37_79B9_7F4A_7C15);
        let low = splitmix64(seq.wrapping_mul(0xBF58_476D_1CE4_E5B9) ^ nanos);
        let value = ((high as u128) << 64) | low as u128;
        // Never hand out NIL, which callers use to mean "unset".
        Self(if value == 0 { 1 } else { value })
    }

    pub fn is_nil(&self) -> bool {
        self.0 == 0
    }

    pub fn parse(text: &str) -> Option<Self> {
        let cleaned: String = text.chars().filter(|c| *c != '-').collect();
        u128::from_str_radix(&cleaned, 16).ok().map(Self)
    }
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

impl Serialize for Id {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).ok_or_else(|| serde::de::Error::custom(format!("not an id: {text}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_do_not_repeat_even_in_a_tight_loop() {
        // A clock-only id collides here, because the system clock does not advance between two
        // adjacent calls on Windows.
        let ids: HashSet<Id> = (0..10_000).map(|_| Id::new()).collect();
        assert_eq!(ids.len(), 10_000);
        assert!(!ids.contains(&Id::NIL), "NIL means unset and must never be generated");
    }

    #[test]
    fn ids_round_trip_through_json_as_hex() {
        let id = Id::new();
        let json = serde_json::to_string(&id).unwrap();
        assert!(json.starts_with('"'), "an id is a JSON string, not a number: {json}");
        assert_eq!(serde_json::from_str::<Id>(&json).unwrap(), id);
        assert_eq!(id.to_string().len(), 32);
    }

    #[test]
    fn a_hyphenated_uuid_from_an_imported_file_still_parses() {
        // G HUB and the Avalonia build both wrote hyphenated GUIDs. Importing one must not fail
        // just because this build prints them differently.
        let dashed = "123e4567-e89b-12d3-a456-426614174000";
        assert_eq!(Id::parse(dashed).unwrap().to_string(), "123e4567e89b12d3a456426614174000");
        assert!(Id::parse("not an id").is_none());
    }

    #[test]
    fn ids_from_several_threads_do_not_collide() {
        let handles: Vec<_> = (0..8)
            .map(|_| std::thread::spawn(|| (0..2_000).map(|_| Id::new()).collect::<Vec<_>>()))
            .collect();
        let all: HashSet<Id> =
            handles.into_iter().flat_map(|h| h.join().unwrap()).collect();
        assert_eq!(all.len(), 16_000);
    }
}
