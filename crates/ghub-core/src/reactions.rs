//! Press reactions over the static per-key layout. Port of
//! `Artemis.Peripherals/Lighting/KeyReactionEngine.cs`.
//!
//! A configured key takes its reaction colour while it is held ([`KeyReactionMode::Momentary`]) or
//! until it is pressed again ([`KeyReactionMode::Toggle`]), and shows the colour the layout gives
//! it otherwise.
//!
//! The engine owns no device. The C# queued its own writes onto the thread pool to keep the
//! keyboard hook from blocking; here every method instead *returns* the frames to write and the
//! caller decides when to send them. The hook still never blocks, the engine is a pure state
//! machine, and a test can assert the exact LED and colour a press produces rather than watching
//! for a side effect.

use std::collections::{HashMap, HashSet};

use ghub_hidpp::LedColor;

use crate::action::virtual_key_from_hid_usage;
use crate::effects::scale;
use crate::layout;
use crate::profile::{KeyReaction, KeyReactionMode, parse_color};

/// A per-key frame: LED id to colour.
pub type Frame = HashMap<u8, LedColor>;

/// Frames to write, one per device.
pub type Writes = Vec<(String, Frame)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Reaction {
    led: u8,
    color: LedColor,
    mode: KeyReactionMode,
}

#[derive(Debug, Default)]
struct DeviceReactions {
    /// Colour each reacting LED returns to, taken from the frame last written.
    base: Frame,
    reactions: HashMap<String, Reaction>,
    held: HashSet<String>,
    latched: HashSet<String>,
}

impl DeviceReactions {
    fn is_lit(&self, key_id: &str) -> bool {
        self.held.contains(key_id) || self.latched.contains(key_id)
    }

    /// Every reacting LED at the colour it should show right now.
    fn full_frame(&self) -> Frame {
        self.reactions
            .iter()
            .map(|(key_id, reaction)| {
                let color = if self.is_lit(key_id) {
                    reaction.color
                } else {
                    self.base.get(&reaction.led).copied().unwrap_or(LedColor::BLACK)
                };
                (reaction.led, color)
            })
            .collect()
    }
}

#[derive(Debug, Default)]
pub struct ReactionEngine {
    devices: HashMap<String, DeviceReactions>,
}

impl ReactionEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// True when at least one device has reactions configured, which lets the caller skip the
    /// whole input path - including installing the keyboard hook at all.
    pub fn is_active(&self) -> bool {
        !self.devices.is_empty()
    }

    pub fn device_count(&self) -> usize {
        self.devices.len()
    }

    /// Installs one device's reactions over the static frame that was just written.
    ///
    /// Returns the frame to write, which re-asserts whatever is still held or latched. Latched
    /// keys survive a re-apply as long as they still have a reaction, so saving an unrelated
    /// setting does not switch off what the user toggled on.
    pub fn configure(
        &mut self,
        device_key: &str,
        base_frame: &Frame,
        reactions: &[KeyReaction],
        brightness: f64,
    ) -> Option<Frame> {
        let resolved: HashMap<String, Reaction> = reactions
            .iter()
            .filter(|r| r.enabled)
            .filter_map(|r| {
                // A key with no addressable LED cannot react, and a colour we cannot parse would
                // otherwise become black - a key that goes dark on press rather than lighting up.
                let key = layout::by_id(&r.key_id).filter(|k| k.led_id != 0)?;
                let color = parse_color(&r.color)?;
                Some((
                    r.key_id.clone(),
                    Reaction { led: key.led_id, color: scale(color, brightness), mode: r.mode },
                ))
            })
            .collect();

        if resolved.is_empty() {
            self.forget(device_key);
            return None;
        }

        let state = self.devices.entry(device_key.to_owned()).or_default();
        state.base = base_frame.clone();
        state.held.retain(|k| resolved.contains_key(k));
        state.latched.retain(|k| resolved.contains_key(k));
        state.reactions = resolved;
        Some(state.full_frame()).filter(|f| !f.is_empty())
    }

    /// Drops a device's reactions: lighting handed back, mode changed, or device gone.
    pub fn forget(&mut self, device_key: &str) {
        self.devices.remove(device_key);
    }

    /// Keyboard-hook entry. `extended` separates the two keys that share a virtual key.
    pub fn on_key(&mut self, virtual_key: u16, down: bool, extended: bool) -> Writes {
        if !self.is_active() {
            return Writes::new();
        }
        let mut writes = Writes::new();
        for key_id in key_ids_for(virtual_key, extended) {
            writes.extend(self.apply(key_id, down));
        }
        writes
    }

    /// HID++ 0x8010 entry. The G-keys have no HID usage, so they never reach the keyboard hook.
    pub fn on_gkey(&mut self, gkey: usize, pressed: bool) -> Writes {
        self.apply(&format!("G{gkey}"), pressed)
    }

    /// Puts the reactions that are lit right now on top of an animation frame, and takes that
    /// frame as the new base.
    ///
    /// Taking the new base is the point: a key released during an animation then returns to
    /// whatever the animation is painting there, rather than to a stale static colour.
    pub fn overlay(&mut self, device_key: &str, frame: &mut Frame) {
        let Some(state) = self.devices.get_mut(device_key) else { return };
        state.base = frame.clone();
        for (key_id, reaction) in &state.reactions {
            if state.held.contains(key_id) || state.latched.contains(key_id) {
                frame.insert(reaction.led, reaction.color);
            }
        }
    }

    /// The colour a key currently shows, for the UI's "what the device looks like now" preview.
    /// `None` when the key has no reaction or is not lit.
    pub fn current_color(&self, device_key: &str, key_id: &str) -> Option<LedColor> {
        let state = self.devices.get(device_key)?;
        let reaction = state.reactions.get(key_id)?;
        state.is_lit(key_id).then_some(reaction.color)
    }

    fn apply(&mut self, key_id: &str, down: bool) -> Writes {
        let mut writes = Writes::new();
        for (device_key, state) in self.devices.iter_mut() {
            let Some(reaction) = state.reactions.get(key_id).copied() else { continue };
            let was_lit = state.is_lit(key_id);

            match (reaction.mode, down) {
                (KeyReactionMode::Momentary, true) => {
                    state.held.insert(key_id.to_owned());
                }
                (KeyReactionMode::Momentary, false) => {
                    state.held.remove(key_id);
                }
                (KeyReactionMode::Toggle, true) => {
                    if !state.latched.remove(key_id) {
                        state.latched.insert(key_id.to_owned());
                    }
                }
                // A toggle ignores the release entirely; acting on it would make the key
                // momentary with extra steps.
                (KeyReactionMode::Toggle, false) => continue,
            }

            let lit = state.is_lit(key_id);
            if lit == was_lit {
                continue;
            }
            let color = if lit {
                reaction.color
            } else {
                state.base.get(&reaction.led).copied().unwrap_or(LedColor::BLACK)
            };
            writes.push((device_key.clone(), HashMap::from([(reaction.led, color)])));
        }
        writes
    }
}

// ------------------------------------------------------------------ virtual key -> layout key

/// Layout keys the keyboard hook reports with the extended-key flag set.
///
/// Only needed to separate keys that share a virtual key. On this layout that is Enter and the
/// numpad's Enter; the rest are listed because the flag is what tells them apart from a key that
/// might be added later, and getting it wrong lights the wrong LED.
static EXTENDED_KEYS: &[&str] = &[
    "NumEnter",
    "NumDivide",
    "RightAlt",
    "RightControl",
    "Insert",
    "Delete",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Up",
    "Down",
    "Left",
    "Right",
    "LeftWindows",
    "RightWindows",
    "Menu",
    "NumLock",
    "PrintScreen",
];

/// Media keys reach the hook as their own virtual keys rather than as HID keyboard usages, so
/// they are mapped here instead of through [`virtual_key_from_hid_usage`].
static MEDIA_KEYS: &[(u16, &str)] = &[
    (0xAD, "MediaMute"),
    (0xB0, "MediaNext"),
    (0xB1, "MediaPrevious"),
    (0xB3, "MediaPlayPause"),
];

fn by_virtual_key() -> &'static HashMap<u16, Vec<&'static str>> {
    use std::sync::OnceLock;
    static MAP: OnceLock<HashMap<u16, Vec<&'static str>>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map: HashMap<u16, Vec<&'static str>> = HashMap::new();
        for key in layout::keys().iter().filter(|k| k.hid_usage != 0) {
            if let Some(vk) = virtual_key_from_hid_usage(key.hid_usage) {
                map.entry(vk).or_default().push(key.id);
            }
        }
        for (vk, id) in MEDIA_KEYS {
            map.entry(*vk).or_default().push(id);
        }
        // Sorted so a virtual key that maps to two layout keys resolves in a fixed order.
        for ids in map.values_mut() {
            ids.sort_unstable();
        }
        map
    })
}

/// Layout keys a virtual key maps to, honouring the extended flag.
///
/// When the flag matches nothing, every candidate is returned rather than none: a hook that does
/// not set the flag the way we expect should light the wrong key at worst, not no key at all.
pub fn key_ids_for(virtual_key: u16, extended: bool) -> Vec<&'static str> {
    let Some(ids) = by_virtual_key().get(&virtual_key) else { return Vec::new() };
    if ids.len() == 1 {
        return ids.clone();
    }
    let exact: Vec<&'static str> = ids
        .iter()
        .copied()
        .filter(|id| EXTENDED_KEYS.contains(id) == extended)
        .collect();
    if exact.is_empty() { ids.clone() } else { exact }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: &str = "#FF0000";
    const DEVICE: &str = "logitech-hidpp:u1";

    fn reaction(key_id: &str, mode: KeyReactionMode) -> KeyReaction {
        KeyReaction { key_id: key_id.to_owned(), color: RED.to_owned(), mode, enabled: true }
    }

    /// A static layout frame where every lightable key is dim blue.
    fn base() -> Frame {
        layout::led_ids()
            .into_iter()
            .map(|id| (id, LedColor { r: 0, g: 0, b: 32 }))
            .collect()
    }

    fn led_of(key_id: &str) -> u8 {
        layout::by_id(key_id).unwrap().led_id
    }

    #[test]
    fn an_engine_with_no_reactions_does_nothing_at_all() {
        let mut engine = ReactionEngine::new();
        assert!(!engine.is_active());
        // The input path must be skippable entirely, so a press produces no writes.
        assert!(engine.on_key(0x41, true, false).is_empty());
        assert!(engine.on_gkey(1, true).is_empty());
    }

    #[test]
    fn configuring_no_usable_reactions_forgets_the_device() {
        let mut engine = ReactionEngine::new();
        assert!(engine.configure(DEVICE, &base(), &[], 1.0).is_none());
        assert!(!engine.is_active());

        // A disabled reaction, a key with no LED, and an unparseable colour are all unusable.
        let mut disabled = reaction("W", KeyReactionMode::Momentary);
        disabled.enabled = false;
        let no_led = reaction("GameMode", KeyReactionMode::Momentary);
        let bad_color =
            KeyReaction { color: "not a colour".into(), ..reaction("A", KeyReactionMode::Toggle) };
        assert!(engine
            .configure(DEVICE, &base(), &[disabled, no_led, bad_color], 1.0)
            .is_none());
        assert!(!engine.is_active(), "none of those could ever be rendered");
    }

    #[test]
    fn a_momentary_key_lights_on_press_and_returns_to_the_layout_on_release() {
        let mut engine = ReactionEngine::new();
        let base = base();
        engine.configure(DEVICE, &base, &[reaction("W", KeyReactionMode::Momentary)], 1.0);

        // 'W' is virtual key 0x57.
        let down = engine.on_key(0x57, true, false);
        assert_eq!(down.len(), 1);
        assert_eq!(down[0].0, DEVICE);
        assert_eq!(down[0].1, HashMap::from([(led_of("W"), LedColor { r: 255, g: 0, b: 0 })]));

        let up = engine.on_key(0x57, false, false);
        assert_eq!(up[0].1, HashMap::from([(led_of("W"), LedColor { r: 0, g: 0, b: 32 })]));
        assert_eq!(engine.current_color(DEVICE, "W"), None);
    }

    #[test]
    fn a_repeated_key_down_does_not_rewrite_the_same_colour() {
        // Windows delivers auto-repeat as a stream of key-downs. Each one must not cost a packet.
        let mut engine = ReactionEngine::new();
        engine.configure(DEVICE, &base(), &[reaction("W", KeyReactionMode::Momentary)], 1.0);
        assert_eq!(engine.on_key(0x57, true, false).len(), 1);
        for _ in 0..50 {
            assert!(engine.on_key(0x57, true, false).is_empty(), "auto-repeat must be free");
        }
        assert_eq!(engine.on_key(0x57, false, false).len(), 1);
    }

    #[test]
    fn a_toggle_key_latches_on_press_and_ignores_the_release() {
        let mut engine = ReactionEngine::new();
        engine.configure(DEVICE, &base(), &[reaction("W", KeyReactionMode::Toggle)], 1.0);

        assert_eq!(engine.on_key(0x57, true, false).len(), 1, "press latches it on");
        assert!(engine.on_key(0x57, false, false).is_empty(), "the release does nothing");
        assert_eq!(engine.current_color(DEVICE, "W"), Some(LedColor { r: 255, g: 0, b: 0 }));

        let second = engine.on_key(0x57, true, false);
        assert_eq!(second[0].1[&led_of("W")], LedColor { r: 0, g: 0, b: 32 }, "second press unlatches");
        assert!(engine.on_key(0x57, false, false).is_empty());
        assert_eq!(engine.current_color(DEVICE, "W"), None);
    }

    #[test]
    fn a_latched_key_survives_a_reconfigure_but_a_removed_one_does_not() {
        let mut engine = ReactionEngine::new();
        let base = base();
        engine.configure(
            DEVICE,
            &base,
            &[reaction("W", KeyReactionMode::Toggle), reaction("A", KeyReactionMode::Toggle)],
            1.0,
        );
        engine.on_key(0x57, true, false);
        engine.on_key(0x41, true, false);
        assert!(engine.current_color(DEVICE, "W").is_some());
        assert!(engine.current_color(DEVICE, "A").is_some());

        // Saving an unrelated setting re-applies the configuration. 'W' must stay lit; 'A', whose
        // reaction was removed, must not.
        let frame = engine
            .configure(DEVICE, &base, &[reaction("W", KeyReactionMode::Toggle)], 1.0)
            .expect("one reaction remains");
        assert_eq!(frame[&led_of("W")], LedColor { r: 255, g: 0, b: 0 }, "still latched");
        assert!(engine.current_color(DEVICE, "A").is_none());
        assert!(!frame.contains_key(&led_of("A")), "a removed reaction is not in the frame");
    }

    #[test]
    fn brightness_scales_the_reaction_colour() {
        let mut engine = ReactionEngine::new();
        engine.configure(DEVICE, &base(), &[reaction("W", KeyReactionMode::Momentary)], 0.5);
        let down = engine.on_key(0x57, true, false);
        assert_eq!(down[0].1[&led_of("W")], LedColor { r: 127, g: 0, b: 0 });
    }

    #[test]
    fn a_g_key_reaches_the_engine_through_its_own_entry_point() {
        // G-keys have no HID usage, so a keyboard hook never sees them.
        let mut engine = ReactionEngine::new();
        engine.configure(DEVICE, &base(), &[reaction("G3", KeyReactionMode::Momentary)], 1.0);
        assert!(key_ids_for(0, false).is_empty(), "there is no virtual key for a G-key");

        let down = engine.on_gkey(3, true);
        assert_eq!(down.len(), 1);
        assert_eq!(down[0].1, HashMap::from([(led_of("G3"), LedColor { r: 255, g: 0, b: 0 })]));
        // A G-key with no reaction produces nothing.
        assert!(engine.on_gkey(5, true).is_empty());
    }

    #[test]
    fn the_extended_flag_separates_enter_from_the_numpads_enter() {
        // Both are virtual key 0x0D; only the numpad's carries the extended flag.
        let plain = key_ids_for(0x0D, false);
        let extended = key_ids_for(0x0D, true);
        assert_eq!(plain, vec!["Enter"]);
        assert_eq!(extended, vec!["NumEnter"]);
        assert_ne!(led_of("Enter"), led_of("NumEnter"));
    }

    #[test]
    fn a_virtual_key_the_layout_does_not_have_resolves_to_nothing() {
        assert!(key_ids_for(0xFE, false).is_empty());
        // And a press of it must not reach any device.
        let mut engine = ReactionEngine::new();
        engine.configure(DEVICE, &base(), &[reaction("W", KeyReactionMode::Momentary)], 1.0);
        assert!(engine.on_key(0xFE, true, false).is_empty());
    }

    #[test]
    fn the_media_keys_map_through_their_own_virtual_keys() {
        assert_eq!(key_ids_for(0xB3, false), vec!["MediaPlayPause"]);
        assert_eq!(key_ids_for(0xAD, false), vec!["MediaMute"]);
        // They are not HID keyboard usages, so the usage table must not also claim them.
        for (vk, _) in MEDIA_KEYS {
            assert_eq!(key_ids_for(*vk, false).len(), 1, "vk 0x{vk:02X} is ambiguous");
        }
    }

    #[test]
    fn every_layout_key_with_a_usage_is_reachable_from_some_virtual_key() {
        // A key that no virtual key maps to can be given a reaction in the UI and then never
        // light up, which looks like a bug in the device.
        let unreachable: Vec<&str> = layout::keys()
            .iter()
            .filter(|k| k.hid_usage != 0 && k.led_id != 0)
            .filter(|k| {
                let Some(vk) = virtual_key_from_hid_usage(k.hid_usage) else { return true };
                !key_ids_for(vk, true).contains(&k.id) && !key_ids_for(vk, false).contains(&k.id)
            })
            .map(|k| k.id)
            .collect();
        assert!(unreachable.is_empty(), "unreachable keys: {unreachable:?}");
    }

    #[test]
    fn an_overlay_takes_the_animation_frame_as_the_new_base() {
        let mut engine = ReactionEngine::new();
        engine.configure(DEVICE, &base(), &[reaction("W", KeyReactionMode::Momentary)], 1.0);
        engine.on_key(0x57, true, false);

        // An animation frame paints everything green. The held key must come back red, and the
        // rest of the frame must be left alone.
        let mut frame: Frame = layout::led_ids()
            .into_iter()
            .map(|id| (id, LedColor { r: 0, g: 255, b: 0 }))
            .collect();
        engine.overlay(DEVICE, &mut frame);
        assert_eq!(frame[&led_of("W")], LedColor { r: 255, g: 0, b: 0 });
        assert_eq!(frame[&led_of("A")], LedColor { r: 0, g: 255, b: 0 });

        // Releasing now returns the key to the animation's colour, not to the old static one.
        let up = engine.on_key(0x57, false, false);
        assert_eq!(up[0].1[&led_of("W")], LedColor { r: 0, g: 255, b: 0 });
    }

    #[test]
    fn an_overlay_for_an_unconfigured_device_leaves_the_frame_untouched() {
        let mut engine = ReactionEngine::new();
        let mut frame: Frame = HashMap::from([(1, LedColor { r: 1, g: 2, b: 3 })]);
        let before = frame.clone();
        engine.overlay("nobody", &mut frame);
        assert_eq!(frame, before);
    }

    #[test]
    fn two_devices_with_the_same_reaction_both_get_a_write() {
        let mut engine = ReactionEngine::new();
        let base = base();
        engine.configure("device-a", &base, &[reaction("W", KeyReactionMode::Momentary)], 1.0);
        engine.configure("device-b", &base, &[reaction("W", KeyReactionMode::Momentary)], 1.0);
        let mut writes = engine.on_key(0x57, true, false);
        writes.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0].0, "device-a");
        assert_eq!(writes[1].0, "device-b");

        engine.forget("device-a");
        assert_eq!(engine.on_key(0x57, false, false).len(), 1);
        assert_eq!(engine.device_count(), 1);
    }

    #[test]
    fn a_reaction_on_a_key_with_no_base_colour_falls_back_to_black() {
        // An empty base frame is what a device under a device-resident effect looks like: we have
        // no idea what colour the firmware is showing, so releasing goes to black rather than to
        // a colour we invented.
        let mut engine = ReactionEngine::new();
        engine.configure(DEVICE, &Frame::new(), &[reaction("W", KeyReactionMode::Momentary)], 1.0);
        engine.on_key(0x57, true, false);
        let up = engine.on_key(0x57, false, false);
        assert_eq!(up[0].1[&led_of("W")], LedColor::BLACK);
    }
}
