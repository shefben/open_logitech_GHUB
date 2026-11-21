//! Shared harness for the feature tests: a device with a known feature table, talking to a replay
//! backend that answers only the recorded requests.
//!
//! Installing the feature table rather than discovering it keeps each test's transcript to the
//! requests the feature under test actually makes, so `unanswered_requests()` being empty is a
//! real assertion that nothing extra was sent to the device.

use std::sync::Arc;

use crate::channel::HidppChannel;
use crate::device::{HidppDevice, HidppFeatureInfo, feature};
use crate::hid::{HidBackend, LOGITECH_VID};
use crate::replay::{HidTranscript, ReplayHidBackend, TranscriptFrame};
use crate::report::constants::WIRED_DEVICE_INDEX;
use crate::report::from_hex;

/// Zero-pads hex out to a 20-byte long report, the way firmware pads a short answer.
pub fn long(hex: &str) -> Vec<u8> {
    let mut bytes = from_hex(hex).unwrap_or_else(|| panic!("{hex} is not valid hex"));
    assert!(bytes.len() <= 20, "{hex} is longer than a long report");
    bytes.resize(20, 0);
    bytes
}

/// A device with `features` installed at the given indexes, over a replay backend answering
/// `pairs`. The backend is given no short collection, matching the directly attached G815.
pub fn device_over(
    features: &[(u16, u8)],
    pairs: &[(&str, &str)],
) -> (HidppDevice, ReplayHidBackend) {
    let frames = pairs
        .iter()
        .flat_map(|(out, reply)| {
            [
                TranscriptFrame { outgoing: true, data: long(out) },
                TranscriptFrame { outgoing: false, data: long(reply) },
            ]
        })
        .collect();
    let transcript = HidTranscript { device: "test".into(), frames };
    let backend = ReplayHidBackend::new(&transcript, 0xC33F, false);
    let connections = backend
        .enumerate(LOGITECH_VID)
        .expect("enumerate")
        .iter()
        .map(|i| backend.open(i).expect("open"))
        .collect();
    let (channel, _notifications) = HidppChannel::new(connections, "replay").expect("channel");

    let mut device = HidppDevice::new(Arc::new(channel), WIRED_DEVICE_INDEX);
    let mut table =
        vec![HidppFeatureInfo { feature_id: feature::ROOT, index: 0, version: 0, flags: 0 }];
    table.extend(features.iter().map(|(id, index)| HidppFeatureInfo {
        feature_id: *id,
        index: *index,
        version: 1,
        flags: 0,
    }));
    device.install_features(table);
    (device, backend)
}

/// A whole session over a replay backend: the feature table is *discovered* from the transcript
/// rather than installed, because discovery is part of what the session does.
///
/// The channel gets a 50 ms timeout and no retries. A request the transcript has no answer for is
/// then a fast failure rather than three seconds of waiting, and it lands in
/// `unanswered_requests()` where the test can name it.
pub fn session_over(
    product_id: u16,
    device_index: u8,
    pairs: &[(&str, &str)],
    options: crate::session::SessionOptions,
) -> (crate::error::Result<crate::session::LogitechHidppSession>, ReplayHidBackend) {
    let frames = pairs
        .iter()
        .flat_map(|(out, reply)| {
            [
                TranscriptFrame { outgoing: true, data: long(out) },
                TranscriptFrame { outgoing: false, data: long(reply) },
            ]
        })
        .collect();
    let transcript = HidTranscript { device: "test".into(), frames };
    let backend = ReplayHidBackend::new(&transcript, product_id, false);
    let connections = backend
        .enumerate(LOGITECH_VID)
        .expect("enumerate")
        .iter()
        .map(|i| backend.open(i).expect("open"))
        .collect();
    let (mut channel, _notifications) = HidppChannel::new(connections, "replay").expect("channel");
    channel.timeout = std::time::Duration::from_millis(50);
    channel.retries = 0;

    let candidate = crate::session::Candidate {
        group_key: "replay".into(),
        product_id,
        device_index,
    };
    let identity = crate::identity::DeviceIdentity::new("logitech-hidpp", "test");
    let session = crate::session::LogitechHidppSession::open(
        Arc::new(channel),
        candidate,
        identity,
        options,
    );
    (session, backend)
}


/// A G815-shaped keyboard: five G-keys, three banks, per-key lighting, no DPI, no battery.
///
/// Lifted verbatim from the session tests so the device manager is driven against the same
/// recorded device the session layer is, rather than a second transcript that could drift.
/// Every request is `11 <device index> <feature index> <function<<4 | 0x0A>> <params>`.
pub const G815_FRAMES: &[(&str, &str)] = &[
    // Root.getProtocolVersion, echoing the 0x5A ping byte back.
    ("11FF001A00005A", "11FF001A04055A"),
    // Root.getFeature(0x0001) -> index 1, flags 0, version 1.
    ("11FF000A0001", "11FF000A010001"),
    // FeatureSet.getCount -> 5, then one getFeatureId per index.
    ("11FF010A", "11FF010A05"),
    ("11FF011A01", "11FF011A00010001"),
    ("11FF011A02", "11FF011A00030001"),
    ("11FF011A03", "11FF011A80100002"),
    ("11FF011A04", "11FF011A80200001"),
    ("11FF011A05", "11FF011A80810001"),
    // 0x0003 getDeviceInfo: 2 entities, unit id, transport 0x0002, model id C33F.
    ("11FF020A", "11FF020A02112233440002C33F00000000"),
    // Firmware entities: main application "U1" 03.02.0018, then the bootloader.
    ("11FF021A00", "11FF021A0055310003020018"),
    ("11FF021A01", "11FF021A01424C3101020000"),
    // 0x8010 getCount -> 5 G-keys.
    ("11FF030A", "11FF030A05"),
    // 0x8020 getCount -> 3 banks.
    ("11FF040A", "11FF040A03"),
];

/// A replay backend over `pairs`, for callers that want to build their own provider on it.
pub fn replay_backend(product_id: u16, pairs: &[(&str, &str)]) -> ReplayHidBackend {
    let frames = pairs
        .iter()
        .flat_map(|(out, reply)| {
            [
                TranscriptFrame { outgoing: true, data: long(out) },
                TranscriptFrame { outgoing: false, data: long(reply) },
            ]
        })
        .collect();
    let transcript = HidTranscript { device: "test".into(), frames };
    ReplayHidBackend::new(&transcript, product_id, false)
}
