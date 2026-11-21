//! Verified Logitech identifier table. Port of `Artemis.Peripherals/Logitech/LogitechModels.cs`.
//!
//! Every id here is either observed on attached hardware or taken from libratbag's device
//! database. `evidence` records which, and the UI surfaces it: a device matched here is marked a
//! known model, and a device that is not still works - it just drives its capabilities entirely
//! off the discovered feature table rather than off a name.

use crate::capability::LightingZone;
use crate::identity::PeripheralKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModelFamily {
    #[default]
    Unknown,
    G903,
    G903Hero,
    G815,
}

#[derive(Debug, Clone)]
pub struct LogitechModel {
    pub family: ModelFamily,
    pub name: &'static str,
    pub kind: PeripheralKind,
    pub wired_product_ids: &'static [u16],
    pub wireless_product_ids: &'static [u16],
    /// Where the identifiers came from. Shown on the diagnostics page so a claim of
    /// "hardware-verified" can always be traced.
    pub evidence: &'static str,
}

pub static KNOWN: &[LogitechModel] = &[
    LogitechModel {
        family: ModelFamily::G903,
        name: "G903 LIGHTSPEED",
        kind: PeripheralKind::Mouse,
        wired_product_ids: &[0xC086],
        wireless_product_ids: &[0x4067],
        evidence: "libratbag logitech-g903.device (not hardware-verified here)",
    },
    LogitechModel {
        family: ModelFamily::G903Hero,
        name: "G903 LIGHTSPEED HERO",
        kind: PeripheralKind::Mouse,
        wired_product_ids: &[0xC091],
        wireless_product_ids: &[0x4087],
        evidence: "Observed: DeviceInformation model ids 4087,C091 via receiver C539; libratbag logitech-g903-hero.device",
    },
    LogitechModel {
        family: ModelFamily::G815,
        name: "G815 RGB Mechanical Gaming Keyboard",
        kind: PeripheralKind::Keyboard,
        wired_product_ids: &[0xC33F],
        wireless_product_ids: &[],
        evidence: "Observed: USB 046D:C33F, HID++ on MI_01 usage page 0xFF43",
    },
];

/// LIGHTSPEED / Unifying receiver product ids. A receiver is never treated as an end device: it is
/// a channel that paired devices sit behind at device indexes 1..n.
pub static RECEIVER_PRODUCT_IDS: &[u16] = &[
    0xC539, 0xC53A, 0xC53F, 0xC541, 0xC545, 0xC547, 0xC548, 0xC52B, 0xC532, 0xC534,
];

pub fn is_receiver(product_id: u16) -> bool {
    RECEIVER_PRODUCT_IDS.contains(&product_id)
}

pub fn by_wired_pid(pid: u16) -> Option<&'static LogitechModel> {
    KNOWN.iter().find(|m| m.wired_product_ids.contains(&pid))
}

pub fn by_wireless_pid(pid: u16) -> Option<&'static LogitechModel> {
    KNOWN.iter().find(|m| m.wireless_product_ids.contains(&pid))
}

/// Matches on any of the model ids 0x0003 reports. A mouse reports both its wired and its wireless
/// product id regardless of which link it is currently on, so either may be the one that matches.
pub fn by_model_ids(model_ids: &[u16]) -> Option<&'static LogitechModel> {
    KNOWN.iter().find(|m| {
        model_ids
            .iter()
            .any(|id| m.wired_product_ids.contains(id) || m.wireless_product_ids.contains(id))
    })
}

/// The G903's two lighting zones, in the order 0x8071 reports its clusters.
pub fn g903_zones() -> Vec<LightingZone> {
    vec![
        LightingZone { index: 0, name: "Primary (side strip)".into(), led_count: 0 },
        LightingZone { index: 1, name: "Logo".into(), led_count: 0 },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_receiver_is_not_an_end_device() {
        assert!(is_receiver(0xC539), "the LIGHTSPEED receiver on this machine");
        assert!(!is_receiver(0xC33F), "the G815 is a keyboard, not a receiver");
        // A receiver pid must never also appear as a model, or the provider would probe slot
        // 0xFF on it instead of slots 1..n.
        for pid in RECEIVER_PRODUCT_IDS {
            assert!(by_wired_pid(*pid).is_none(), "{pid:04X} is listed as both");
            assert!(by_wireless_pid(*pid).is_none(), "{pid:04X} is listed as both");
        }
    }

    #[test]
    fn a_mouse_is_found_by_either_of_its_product_ids() {
        // The same G903 HERO, once on its cable and once through the receiver.
        assert_eq!(by_wired_pid(0xC091).unwrap().family, ModelFamily::G903Hero);
        assert_eq!(by_wireless_pid(0x4087).unwrap().family, ModelFamily::G903Hero);
        // 0x0003 reports both; whichever order they arrive in, the answer is the same model.
        assert_eq!(by_model_ids(&[0x4087, 0xC091, 0]).unwrap().family, ModelFamily::G903Hero);
        assert_eq!(by_model_ids(&[0, 0xC091, 0x4087]).unwrap().family, ModelFamily::G903Hero);
    }

    #[test]
    fn an_unknown_device_matches_nothing_rather_than_the_first_row() {
        assert!(by_model_ids(&[0x1234]).is_none());
        assert!(by_model_ids(&[]).is_none());
        // All-zero model ids are what a device that does not fill them in reports. They must not
        // match, or every such device would be mistaken for the first row with a zero in it.
        assert!(by_model_ids(&[0, 0, 0]).is_none());
    }

    #[test]
    fn every_row_states_its_evidence() {
        for m in KNOWN {
            assert!(!m.evidence.is_empty(), "{} has no evidence note", m.name);
            assert_ne!(m.kind, PeripheralKind::Unknown, "{} has no kind", m.name);
            assert!(
                !m.wired_product_ids.is_empty() || !m.wireless_product_ids.is_empty(),
                "{} has no product ids and could never match",
                m.name
            );
        }
    }
}
