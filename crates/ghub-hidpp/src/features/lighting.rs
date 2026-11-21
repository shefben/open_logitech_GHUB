//! 0x8071 RGB effects and 0x8081 per-key lighting v2.
//!
//! Every write here uses the RAM persistence flag. The onboard (non-volatile) lighting
//! configuration is never touched, so unplugging the device restores whatever the user had stored
//! in it through G HUB.

use std::collections::{HashMap, HashSet};

use crate::capability::LightingZone;
use crate::channel::RequestOptions;
use crate::color::LedColor;
use crate::device::{HidppDevice, feature};
use crate::error::{Hidpp20Error, HidppError, ProtocolError, Result};

/// Well-known 0x8071 effect ids.
pub mod effect_id {
    pub const OFF: u16 = 0x0000;
    pub const STATIC: u16 = 0x0001;
    pub const COLOR_CYCLE: u16 = 0x0003;
    pub const COLOR_WAVE: u16 = 0x0004;
    pub const BREATHING: u16 = 0x000A;
    pub const RIPPLE: u16 = 0x000B;
    pub const STORED: u16 = 0x000C;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbEffectInfo {
    pub index: u8,
    pub effect_id: u16,
    pub capabilities: u16,
    pub period: u16,
}

impl RgbEffectInfo {
    pub fn name(&self) -> String {
        match self.effect_id {
            effect_id::OFF => "Off".into(),
            effect_id::STATIC => "Static".into(),
            effect_id::COLOR_CYCLE => "Color cycle".into(),
            effect_id::COLOR_WAVE => "Color wave".into(),
            effect_id::BREATHING => "Breathing".into(),
            effect_id::RIPPLE => "Ripple".into(),
            effect_id::STORED => "Custom/stored".into(),
            other => format!("Effect 0x{other:04X}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbClusterInfo {
    pub index: u8,
    pub location: u16,
    pub persistency_caps: u8,
    pub effects: Vec<RgbEffectInfo>,
}

impl RgbClusterInfo {
    pub fn location_name(&self) -> String {
        match self.location {
            0x0001 => "Primary".into(),
            0x0002 => "Logo".into(),
            other => format!("Zone 0x{other:04X}"),
        }
    }
}

/// Runtime lighting power timers of 0x8071, in seconds, plus the flags word G HUB sends alongside
/// them. The flags' meaning is undocumented, so they are preserved exactly as read rather than
/// invented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RgbPowerTimers {
    pub idle_seconds: u16,
    pub sleep_seconds: u16,
    pub flags: u16,
}

/// 0x8071: discovers clusters and effects (read-only), then sets cluster effects in RAM.
pub struct RgbEffectsFeature<'d> {
    pub(crate) device: &'d HidppDevice,
    pub clusters: Vec<RgbClusterInfo>,
    pub zones: Vec<LightingZone>,
}

impl<'d> RgbEffectsFeature<'d> {
    pub fn create(device: &'d HidppDevice) -> Result<Option<Self>> {
        if !device.has_feature(feature::RGB_EFFECTS) {
            return Ok(None);
        }
        // Cluster 0xFF, effect 0xFF is the "describe the device" query.
        let info =
            device.call(feature::RGB_EFFECTS, 0, &[0xFF, 0xFF, 0x00], RequestOptions::default())?;
        let cluster_count = (info.param(2) as usize).min(8);

        let mut clusters = Vec::with_capacity(cluster_count);
        for c in 0..cluster_count as u8 {
            let cluster = device.call(
                feature::RGB_EFFECTS,
                0,
                &[c, 0xFF, 0x00],
                RequestOptions::default(),
            )?;
            let effect_count = (cluster.param(4) as usize).min(16);
            let mut effects = Vec::with_capacity(effect_count);
            for e in 0..effect_count as u8 {
                let effect = device.call(
                    feature::RGB_EFFECTS,
                    0,
                    &[c, e, 0x00],
                    RequestOptions::default(),
                )?;
                effects.push(RgbEffectInfo {
                    index: e,
                    effect_id: effect.param_u16(2),
                    capabilities: effect.param_u16(4),
                    period: effect.param_u16(6),
                });
            }
            clusters.push(RgbClusterInfo {
                index: c,
                location: cluster.param_u16(2),
                persistency_caps: cluster.param(5),
                effects,
            });
        }

        let zones = clusters
            .iter()
            .map(|c| LightingZone {
                index: c.index as usize,
                name: c.location_name(),
                led_count: 1,
            })
            .collect();
        Ok(Some(Self { device, clusters, zones }))
    }

    /// The device's own index for an effect id on a cluster. Effect *indexes* are per-firmware;
    /// only the ids are stable, so an index is never assumed.
    pub fn effect_index_for(&self, cluster: usize, effect_id: u16) -> Option<u8> {
        self.clusters
            .get(cluster)?
            .effects
            .iter()
            .find(|e| e.effect_id == effect_id)
            .map(|e| e.index)
    }

    /// Builds the `setRgbClusterEffect` payload. Byte 12 is the persistence selector and is
    /// always 0x01, meaning RAM: 0x02 would write the onboard configuration.
    pub fn build_set_effect(
        cluster: u8,
        effect_index: u8,
        color: LedColor,
        period: u16,
        brightness: u8,
    ) -> [u8; 16] {
        let mut p = [0u8; 16];
        p[0] = cluster;
        p[1] = effect_index;
        p[2] = color.r;
        p[3] = color.g;
        p[4] = color.b;
        p[5] = (period >> 8) as u8;
        p[6] = period as u8;
        p[8] = brightness;
        p[12] = 0x01;
        p
    }

    pub fn set_static_color(&self, zone_index: usize, color: LedColor) -> Result<()> {
        let index = self.effect_index_for(zone_index, effect_id::STATIC).ok_or_else(|| {
            HidppError::Unsupported(format!("cluster {zone_index} has no static effect"))
        })?;
        self.device.call(
            feature::RGB_EFFECTS,
            1,
            &Self::build_set_effect(zone_index as u8, index, color, 0, 100),
            RequestOptions::long(),
        )?;
        Ok(())
    }

    pub fn set_effect(
        &self,
        zone_index: usize,
        wanted: u16,
        color: LedColor,
        period: u16,
        brightness: u8,
    ) -> Result<()> {
        let index = self.effect_index_for(zone_index, wanted).ok_or_else(|| {
            HidppError::Unsupported(format!(
                "cluster {zone_index} does not offer effect 0x{wanted:04X}"
            ))
        })?;
        self.device.call(
            feature::RGB_EFFECTS,
            1,
            &Self::build_set_effect(zone_index as u8, index, color, period, brightness),
            RequestOptions::long(),
        )?;
        Ok(())
    }

    /// Reads the runtime lighting power timers (function 7, sub-function 0 = get).
    pub fn get_power_timers(&self) -> Result<RgbPowerTimers> {
        let r = self.device.call(feature::RGB_EFFECTS, 7, &[0x00], RequestOptions::default())?;
        parse_power_timers(&r)
    }

    /// Sets the power timers. Volatile: the firmware resets them on power cycle.
    ///
    /// `non_idempotent` because a lost reply does not mean the timers were not applied, and
    /// resending could overwrite a value the user changed in between.
    pub fn set_power_timers(&self, timers: RgbPowerTimers) -> Result<RgbPowerTimers> {
        self.device.call(
            feature::RGB_EFFECTS,
            7,
            &build_set_power_timers(timers),
            RequestOptions::non_idempotent().forcing_long(),
        )?;
        self.get_power_timers()
    }

    /// Turns the onboard effect off on every cluster, before host-streamed per-key frames. Without
    /// this the firmware effect fights every frame we send.
    pub fn disable_onboard_effects(&self) -> Result<()> {
        for cluster in &self.clusters {
            if let Some(off) = self.effect_index_for(cluster.index as usize, effect_id::OFF) {
                self.device.call(
                    feature::RGB_EFFECTS,
                    1,
                    &Self::build_set_effect(cluster.index, off, LedColor::BLACK, 0, 100),
                    RequestOptions::long(),
                )?;
            }
        }
        Ok(())
    }

    /// Hands lighting back to a firmware static-white effect. The previous onboard effect is not
    /// readable, so restoring it exactly is impossible; white is what G HUB leaves behind too.
    pub fn restore_static_white(&self) -> Result<()> {
        for cluster in &self.clusters {
            if let Some(index) = self.effect_index_for(cluster.index as usize, effect_id::STATIC) {
                self.device.call(
                    feature::RGB_EFFECTS,
                    1,
                    &Self::build_set_effect(cluster.index, index, LedColor::WHITE, 0, 100),
                    RequestOptions::long(),
                )?;
            }
        }
        Ok(())
    }
}

pub fn parse_power_timers(r: &crate::report::HidppReport) -> Result<RgbPowerTimers> {
    if r.param(0) != 0x00 {
        return Err(HidppError::Malformed(format!(
            "unexpected power-config echo 0x{:02X}",
            r.param(0)
        )));
    }
    Ok(RgbPowerTimers {
        flags: r.param_u16(1),
        idle_seconds: r.param_u16(3),
        sleep_seconds: r.param_u16(5),
    })
}

pub fn build_set_power_timers(timers: RgbPowerTimers) -> [u8; 16] {
    let mut p = [0u8; 16];
    p[0] = 0x01;
    p[1] = (timers.flags >> 8) as u8;
    p[2] = timers.flags as u8;
    p[3] = (timers.idle_seconds >> 8) as u8;
    p[4] = timers.idle_seconds as u8;
    p[5] = (timers.sleep_seconds >> 8) as u8;
    p[6] = timers.sleep_seconds as u8;
    p
}

/// One HID++ packet of a per-key frame: the function id plus its 16 parameter bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FramePacket {
    pub function: u8,
    pub parameters: [u8; 16],
}

/// 0x8081 per-key lighting v2 (G815). Frames are volatile.
pub struct PerKeyLightingFeature {
    /// Colour last accepted per key, so a frame only sends what changed.
    current: HashMap<u8, LedColor>,
    /// Key ids this firmware refused. Left out of every later frame.
    rejected: HashSet<u8>,
    pub key_ids: Vec<u8>,
    pub acquired: bool,
}

impl PerKeyLightingFeature {
    pub fn create(device: &HidppDevice, key_ids: Vec<u8>) -> Option<Self> {
        if !device.has_feature(feature::PER_KEY_LIGHTING_V2) {
            return None;
        }
        Some(Self {
            current: HashMap::new(),
            rejected: HashSet::new(),
            key_ids,
            acquired: false,
        })
    }

    pub fn rejected_key_ids(&self) -> Vec<u8> {
        let mut all: Vec<u8> = self.rejected.iter().copied().collect();
        all.sort_unstable();
        all
    }

    /// Takes over lighting. `effects` is the device's 0x8071 feature when it has one, used to
    /// silence the onboard effect first.
    pub fn acquire(&mut self, effects: Option<&RgbEffectsFeature<'_>>) -> Result<()> {
        if let Some(effects) = effects {
            effects.disable_onboard_effects()?;
        }
        // The cache describes what the device shows. Taking over invalidates it, so the next frame
        // must send every key rather than assume anything.
        self.current.clear();
        self.acquired = true;
        Ok(())
    }

    pub fn release(&mut self, effects: Option<&RgbEffectsFeature<'_>>) -> Result<()> {
        self.acquired = false;
        self.current.clear();
        match effects {
            Some(effects) => effects.restore_static_white(),
            None => Ok(()),
        }
    }

    /// Sends one frame: only the keys whose colour changed, then the frame-end.
    pub fn write_frame(
        &mut self,
        device: &HidppDevice,
        colors: &HashMap<u8, LedColor>,
    ) -> Result<()> {
        let mut changed: HashMap<u8, LedColor> = colors
            .iter()
            .filter(|(key, _)| **key != 0 && !self.rejected.contains(*key))
            .filter(|(key, color)| self.current.get(*key) != Some(*color))
            .map(|(k, c)| (*k, *c))
            .collect();
        if changed.is_empty() {
            return Ok(());
        }

        let mut failed = false;
        for packet in build_frame_packets(&changed) {
            match device.call(
                feature::PER_KEY_LIGHTING_V2,
                packet.function,
                &packet.parameters,
                RequestOptions::long(),
            ) {
                Ok(_) => {}
                Err(HidppError::Protocol(e)) if is_rejection(&e) => {
                    failed = true;
                    break;
                }
                Err(e) => return Err(e),
            }
        }

        if failed {
            // The firmware validates a whole packet, so one unknown key id fails every key sent
            // with it - on the G815 that blanks the keyboard. Find the offenders one at a time,
            // remember them, and keep the rest lit.
            changed = self.quarantine(device, &changed)?;
        }

        device.call(feature::PER_KEY_LIGHTING_V2, 7, &[], RequestOptions::long())?;
        for (key, color) in changed {
            self.current.insert(key, color);
        }
        Ok(())
    }

    /// Writes each key on its own, recording the ones the firmware refuses. Returns the accepted
    /// keys; the caller still sends the frame-end.
    fn quarantine(
        &mut self,
        device: &HidppDevice,
        changed: &HashMap<u8, LedColor>,
    ) -> Result<HashMap<u8, LedColor>> {
        let mut accepted = HashMap::new();
        // Sorted so the order of quarantine - and so the set of ids blamed when several are bad -
        // does not depend on hash iteration order.
        let mut keys: Vec<u8> = changed.keys().copied().collect();
        keys.sort_unstable();

        for key in keys {
            let color = changed[&key];
            let mut parameters = [0u8; 16];
            parameters[0] = key;
            parameters[1] = color.r;
            parameters[2] = color.g;
            parameters[3] = color.b;
            parameters[4] = 0xFF;
            match device.call(
                feature::PER_KEY_LIGHTING_V2,
                1,
                &parameters,
                RequestOptions::long(),
            ) {
                Ok(_) => {
                    accepted.insert(key, color);
                }
                Err(HidppError::Protocol(e)) if is_rejection(&e) => {
                    log::warn!("key id 0x{key:02X} refused by firmware; excluded from later frames");
                    self.rejected.insert(key);
                    self.current.remove(&key);
                }
                Err(e) => return Err(e),
            }
        }
        Ok(accepted)
    }
}

/// Whether a protocol error means "the firmware did not like this key id", as opposed to a
/// transport problem worth surfacing.
fn is_rejection(e: &ProtocolError) -> bool {
    e.is_hidpp20
        && (e.code == Hidpp20Error::InvalidArgument as u8
            || e.code == Hidpp20Error::OutOfRange as u8)
}

/// Groups keys by colour.
///
/// A colour shared by more than four keys goes through function 6 (one colour plus up to 13 key
/// ids, 0xFF-terminated); the rest go through function 1 (up to four id/R/G/B tuples). This is
/// what keeps a whole-keyboard frame inside a few packets instead of 110 of them.
pub fn build_frame_packets(colors: &HashMap<u8, LedColor>) -> Vec<FramePacket> {
    let mut by_color: HashMap<LedColor, Vec<u8>> = HashMap::new();
    for (key, color) in colors {
        by_color.entry(*color).or_default().push(*key);
    }

    let mut packets = Vec::new();
    let mut singles: Vec<(u8, LedColor)> = Vec::new();

    // Sorted by colour so packet order is deterministic; a frame that reorders between runs makes
    // transcripts useless as fixtures.
    let mut groups: Vec<(LedColor, Vec<u8>)> = by_color.into_iter().collect();
    groups.sort_unstable_by_key(|(c, _)| (c.r, c.g, c.b));

    for (color, mut keys) in groups {
        keys.sort_unstable();
        if keys.len() > 4 {
            for chunk in keys.chunks(13) {
                let mut parameters = [0u8; 16];
                parameters[0] = color.r;
                parameters[1] = color.g;
                parameters[2] = color.b;
                parameters[3..3 + chunk.len()].copy_from_slice(chunk);
                if chunk.len() < 13 {
                    parameters[3 + chunk.len()] = 0xFF;
                }
                packets.push(FramePacket { function: 6, parameters });
            }
        } else {
            singles.extend(keys.into_iter().map(|k| (k, color)));
        }
    }

    singles.sort_unstable_by_key(|(k, _)| *k);
    for chunk in singles.chunks(4) {
        let mut parameters = [0u8; 16];
        for (i, (key, color)) in chunk.iter().enumerate() {
            parameters[4 * i] = *key;
            parameters[4 * i + 1] = color.r;
            parameters[4 * i + 2] = color.g;
            parameters[4 * i + 3] = color.b;
        }
        if chunk.len() < 4 {
            parameters[4 * chunk.len()] = 0xFF;
        }
        packets.push(FramePacket { function: 1, parameters });
    }

    packets
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: LedColor = LedColor::new(0xFF, 0, 0);
    const BLUE: LedColor = LedColor::new(0, 0, 0xFF);

    fn frame(pairs: &[(u8, LedColor)]) -> HashMap<u8, LedColor> {
        pairs.iter().copied().collect()
    }

    #[test]
    fn four_or_fewer_keys_of_a_colour_go_in_tuple_packets() {
        let packets = build_frame_packets(&frame(&[(1, RED), (2, RED), (3, BLUE)]));
        assert_eq!(packets.len(), 1, "three keys fit one function-1 packet");
        let p = &packets[0];
        assert_eq!(p.function, 1);
        assert_eq!(&p.parameters[0..4], &[1, 0xFF, 0, 0]);
        assert_eq!(&p.parameters[4..8], &[2, 0xFF, 0, 0]);
        assert_eq!(&p.parameters[8..12], &[3, 0, 0, 0xFF]);
        // A short packet is terminated so the firmware does not read the zero padding as key 0.
        assert_eq!(p.parameters[12], 0xFF);
    }

    #[test]
    fn more_than_four_keys_of_one_colour_use_the_bulk_function() {
        let keys: Vec<(u8, LedColor)> = (1..=5).map(|k| (k, RED)).collect();
        let packets = build_frame_packets(&frame(&keys));
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].function, 6);
        assert_eq!(&packets[0].parameters[0..3], &[0xFF, 0, 0], "colour first");
        assert_eq!(&packets[0].parameters[3..8], &[1, 2, 3, 4, 5]);
        assert_eq!(packets[0].parameters[8], 0xFF, "terminated");
    }

    #[test]
    fn a_bulk_group_is_chunked_at_thirteen_keys() {
        // The whole point of function 6: a 110-key keyboard lit one colour must not take 110
        // packets. 20 keys is two packets, 13 then 7.
        let keys: Vec<(u8, LedColor)> = (1..=20).map(|k| (k, RED)).collect();
        let packets = build_frame_packets(&frame(&keys));
        assert_eq!(packets.len(), 2);
        assert!(packets.iter().all(|p| p.function == 6));
        assert_eq!(&packets[0].parameters[3..16], &(1..=13).collect::<Vec<u8>>()[..]);
        // A full chunk has no room for a terminator, and must not get one.
        assert_eq!(packets[0].parameters[15], 13);
        assert_eq!(&packets[1].parameters[3..10], &[14, 15, 16, 17, 18, 19, 20]);
        assert_eq!(packets[1].parameters[10], 0xFF);
    }

    #[test]
    fn a_full_keyboard_of_one_colour_is_nine_packets_not_a_hundred() {
        // The G815 has 110 addressable components. At 13 per packet that is 9 packets, which is
        // what makes 60 Hz streaming possible over a 20-byte report.
        let keys: Vec<(u8, LedColor)> = (1..=110).map(|k| (k, RED)).collect();
        let packets = build_frame_packets(&frame(&keys));
        assert_eq!(packets.len(), 9);
    }

    #[test]
    fn packet_order_is_deterministic() {
        // Frames are compared against recorded transcripts, so two runs of the same frame must
        // produce byte-identical packets despite HashMap iteration order.
        let f = frame(&[(9, BLUE), (1, RED), (5, BLUE), (3, RED), (7, BLUE), (2, BLUE), (4, BLUE)]);
        let first = build_frame_packets(&f);
        for _ in 0..20 {
            assert_eq!(build_frame_packets(&f), first);
        }
    }

    #[test]
    fn a_power_timer_round_trip_preserves_the_undocumented_flags() {
        // The flags word's meaning is unknown, so it must come back out exactly as it went in
        // rather than be normalised to something we think is right.
        let timers = RgbPowerTimers { idle_seconds: 300, sleep_seconds: 900, flags: 0x1234 };
        let built = build_set_power_timers(timers);
        assert_eq!(built[0], 0x01, "sub-function 1 is set");
        assert_eq!(&built[1..7], &[0x12, 0x34, 0x01, 0x2C, 0x03, 0x84]);

        // The read side answers with sub-function 0 echoed in param 0.
        let mut reply = vec![0x11, 0xFF, 0x0A, 0x7A, 0x00];
        reply.extend_from_slice(&built[1..7]);
        reply.resize(20, 0);
        let parsed = parse_power_timers(&crate::report::HidppReport::try_parse(&reply).unwrap());
        assert_eq!(parsed.unwrap(), timers);
    }

    #[test]
    fn a_wrong_power_config_echo_is_malformed() {
        let mut reply = vec![0x11, 0xFF, 0x0A, 0x7A, 0x01];
        reply.resize(20, 0);
        let r = crate::report::HidppReport::try_parse(&reply).unwrap();
        assert!(matches!(parse_power_timers(&r), Err(HidppError::Malformed(_))));
    }

    #[test]
    fn the_effect_payload_writes_ram_persistence_only() {
        // Byte 12 is the persistence selector. 0x02 there would write onboard flash, which is out
        // of scope for a lighting change the user made with a colour picker.
        let p = RgbEffectsFeature::build_set_effect(1, 3, RED, 0x0100, 80);
        assert_eq!(p[0..5], [1, 3, 0xFF, 0x00, 0x00]);
        assert_eq!(p[5..7], [0x01, 0x00], "period is big-endian");
        assert_eq!(p[8], 80);
        assert_eq!(p[12], 0x01, "RAM, never onboard");
    }

    #[test]
    fn effect_names_cover_the_ids_the_ui_offers() {
        for id in [
            effect_id::OFF,
            effect_id::STATIC,
            effect_id::COLOR_CYCLE,
            effect_id::COLOR_WAVE,
            effect_id::BREATHING,
            effect_id::RIPPLE,
        ] {
            let info = RgbEffectInfo { index: 0, effect_id: id, capabilities: 0, period: 0 };
            assert!(!info.name().starts_with("Effect 0x"), "0x{id:04X} has no name");
        }
    }

    #[test]
    fn cluster_locations_are_named() {
        let primary =
            RgbClusterInfo { index: 0, location: 1, persistency_caps: 0, effects: vec![] };
        assert_eq!(primary.location_name(), "Primary");
        assert_eq!(RgbClusterInfo { location: 2, ..primary.clone() }.location_name(), "Logo");
        assert_eq!(
            RgbClusterInfo { location: 9, ..primary }.location_name(),
            "Zone 0x0009"
        );
    }

    #[test]
    fn only_invalid_argument_and_out_of_range_count_as_a_key_rejection() {
        // Quarantining on a transport error would blacklist perfectly good keys for the rest of
        // the session.
        let rejection = ProtocolError {
            is_hidpp20: true,
            code: Hidpp20Error::InvalidArgument as u8,
            target_index: 0,
        };
        assert!(is_rejection(&rejection));
        assert!(is_rejection(&ProtocolError {
            code: Hidpp20Error::OutOfRange as u8,
            ..rejection
        }));
        assert!(!is_rejection(&ProtocolError { code: Hidpp20Error::Busy as u8, ..rejection }));
        // A HID++ 1.0 error with the same numeric code means something else entirely.
        assert!(!is_rejection(&ProtocolError { is_hidpp20: false, ..rejection }));
    }
}
