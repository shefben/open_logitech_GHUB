//! Per-key geometry for a device render, parsed from the `metadata.json` that ships beside the
//! device image. Port of `Artemis.Peripherals/Logitech/DeviceImageMetadata.cs`.
//!
//! This is G HUB's own metadata, copied verbatim out of its install, so the schema is G HUB's and
//! the quirks below are its quirks. Coordinates are in the image's own pixel space
//! (`origin_width` x `origin_height`), so a view only has to scale the whole image uniformly.

use serde::Deserialize;

/// A rectangle in device-image space (origin pixels, top-left anchored).
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct ImageRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl ImageRect {
    pub fn center_x(&self) -> f64 {
        self.x + self.width / 2.0
    }
    pub fn center_y(&self) -> f64 {
        self.y + self.height / 2.0
    }
}

/// A circular hot area in device-image space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageCircle {
    pub center_x: f64,
    pub center_y: f64,
    pub radius: f64,
}

/// One lightable component of a zone. `id` is zone-scoped: for `PERKEY_KEYBOARD` and
/// `PERKEY_CONSUMER` it is the HID usage, for `PERKEY_GKEY` the 1-based G-key number, and for
/// `PERKEY_INDICATOR` / `PERKEY_BRANDING` it is always 1.
#[derive(Debug, Clone, Copy)]
pub struct ZoneComponent {
    pub id: i32,
    pub bounds: ImageRect,
}

/// A named lighting zone. `disableable` marks zones game mode may lock out.
#[derive(Debug, Clone)]
pub struct DeviceZone {
    pub id: String,
    pub disableable: bool,
    pub components: Vec<ZoneComponent>,
}

impl DeviceZone {
    pub fn by_component_id(&self, id: i32) -> Option<&ZoneComponent> {
        self.components.iter().find(|c| c.id == id)
    }
}

/// A G-key assignment marker position, with the label anchor G HUB draws the callout at.
#[derive(Debug, Clone)]
pub struct AssignmentMarker {
    pub slot_id: String,
    pub x: f64,
    pub y: f64,
    pub label_x: f64,
    pub label_y: f64,
}

/// A removable cover (the G903's magnetic side-button plates). G HUB draws the cover image over
/// the bare device render at `position` when the cover is fitted.
#[derive(Debug, Clone)]
pub struct MaskableZone {
    pub id: String,
    pub cover_image_key: String,
    pub position: ImageRect,
}

/// Well-known zone ids.
pub mod zones {
    pub const PER_KEY_KEYBOARD: &str = "PERKEY_KEYBOARD";
    pub const PER_KEY_INDICATOR: &str = "PERKEY_INDICATOR";
    pub const PER_KEY_CONSUMER: &str = "PERKEY_CONSUMER";
    pub const PER_KEY_BRANDING: &str = "PERKEY_BRANDING";
    pub const PER_KEY_GKEY: &str = "PERKEY_GKEY";

    /// Mouse logo / palm LED (G903: the "G" on the palm rest). One component, id 1.
    pub const BRANDING: &str = "ZONE_BRANDING";

    /// Mouse primary LED (G903: the DPI strip under the wheel). One component, id 1.
    pub const PRIMARY: &str = "ZONE_PRIMARY";
}

#[derive(Debug, Clone)]
pub struct DeviceImageMetadata {
    pub origin_width: i32,
    pub origin_height: i32,
    pub zones: Vec<DeviceZone>,
    pub assignments: Vec<AssignmentMarker>,
    pub hot_areas: Vec<(String, ImageCircle)>,
    /// Removable covers drawn over the render (G903 side plates); empty for devices without them.
    pub covers: Vec<MaskableZone>,
}

impl DeviceImageMetadata {
    /// Every component across every zone. For the G815 this is 115 rectangles.
    pub fn component_count(&self) -> usize {
        self.zones.iter().map(|z| z.components.len()).sum()
    }

    pub fn zone(&self, id: &str) -> Option<&DeviceZone> {
        self.zones.iter().find(|z| z.id == id)
    }

    fn component_bounds(&self, zone: &str, id: i32) -> Option<ImageRect> {
        self.zone(zone)?.by_component_id(id).map(|c| c.bounds)
    }

    /// Bounds of a keyboard key by HID *keyboard* usage, or `None` when the usage is not lit
    /// individually. Consumer (media) usages overlap the keyboard page numerically - 226 is both
    /// Left Alt and Mute - so they resolve through [`Self::consumer_bounds`] instead and are
    /// deliberately not searched here.
    pub fn key_bounds(&self, hid_usage: u8) -> Option<ImageRect> {
        self.component_bounds(zones::PER_KEY_KEYBOARD, hid_usage as i32)
    }

    /// Bounds of a media key by HID *consumer* usage (mute = 226), or `None`.
    pub fn consumer_bounds(&self, consumer_usage: u8) -> Option<ImageRect> {
        self.component_bounds(zones::PER_KEY_CONSUMER, consumer_usage as i32)
    }

    /// Bounds of a G-key by its 1-based number (G1 = 1).
    pub fn gkey_bounds(&self, number: i32) -> Option<ImageRect> {
        self.component_bounds(zones::PER_KEY_GKEY, number)
    }

    /// Bounds of the branding (logo) LED: the keyboard logo strip or the mouse palm LED.
    pub fn logo_bounds(&self) -> Option<ImageRect> {
        self.component_bounds(zones::PER_KEY_BRANDING, 1)
            .or_else(|| self.component_bounds(zones::BRANDING, 1))
    }

    /// Bounds of the indicator LED (G815: the brightness key's lighting indicator).
    pub fn indicator_bounds(&self) -> Option<ImageRect> {
        self.component_bounds(zones::PER_KEY_INDICATOR, 1)
    }

    /// Bounds of the mouse's primary LED zone (G903: the DPI strip), or `None`.
    pub fn primary_bounds(&self) -> Option<ImageRect> {
        self.component_bounds(zones::PRIMARY, 1)
    }

    /// Marker position of an assignment slot (G HUB slot id, e.g. `g903hero_g4_m1`).
    pub fn assignment(&self, slot_id: &str) -> Option<&AssignmentMarker> {
        self.assignments.iter().find(|a| a.slot_id == slot_id)
    }

    pub fn hot_area(&self, id: &str) -> Option<ImageCircle> {
        self.hot_areas
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(id))
            .map(|(_, c)| *c)
    }

    pub fn parse(json: &str) -> Result<Self, MetadataError> {
        let raw: RawRoot = serde_json::from_str(json)?;
        let image = raw.images.into_iter().next().ok_or(MetadataError::NoImages)?;

        let zones = image
            .zones
            .into_iter()
            .map(|z| DeviceZone {
                id: z.id,
                disableable: z.disableable,
                components: z
                    .components
                    .into_iter()
                    .enumerate()
                    .filter_map(|(i, c)| {
                        // Single-component zones (the mouse's ZONE_BRANDING / ZONE_PRIMARY) carry
                        // no id in G HUB's metadata; they are addressed by 1-based position.
                        let bounds = first_rect(&c.renderings)?;
                        Some(ZoneComponent { id: c.id.unwrap_or(i as i32 + 1), bounds })
                    })
                    .collect(),
            })
            .collect();

        let assignments = image
            .assignments
            .into_iter()
            .map(|a| AssignmentMarker {
                slot_id: a.slot_id,
                x: a.marker.x,
                y: a.marker.y,
                label_x: a.label.as_ref().map_or(0.0, |l| l.x),
                label_y: a.label.as_ref().map_or(0.0, |l| l.y),
            })
            .collect();

        let hot_areas = image
            .hot_areas
            .into_iter()
            .filter_map(|h| {
                let circle = h.renderings.iter().find_map(|r| r.circle.as_ref())?;
                Some((
                    h.id,
                    ImageCircle { center_x: circle.cx, center_y: circle.cy, radius: circle.r },
                ))
            })
            .collect();

        let covers = image
            .maskable_zones
            .into_iter()
            .filter_map(|m| {
                let cover = m.cover_image?;
                Some(MaskableZone { id: m.id, cover_image_key: cover.key, position: cover.position })
            })
            .collect();

        Ok(Self {
            origin_width: image.origin.width,
            origin_height: image.origin.height,
            zones,
            assignments,
            hot_areas,
            covers,
        })
    }
}

/// The first rectangular rendering of a component; components may in principle carry several.
fn first_rect(renderings: &[RawRendering]) -> Option<ImageRect> {
    renderings.iter().find_map(|r| r.rect)
}

#[derive(Debug, thiserror::Error)]
pub enum MetadataError {
    #[error("device image metadata has no 'images' array")]
    NoImages,
    #[error("device image metadata is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

// ------------------------------------------------------------------ wire shapes

#[derive(Deserialize)]
struct RawRoot {
    images: Vec<RawImage>,
}

#[derive(Deserialize)]
struct RawImage {
    origin: RawOrigin,
    #[serde(default)]
    zones: Vec<RawZone>,
    #[serde(default)]
    assignments: Vec<RawAssignment>,
    #[serde(default, rename = "hotAreas")]
    hot_areas: Vec<RawHotArea>,
    #[serde(default, rename = "maskableZones")]
    maskable_zones: Vec<RawMaskableZone>,
}

#[derive(Deserialize)]
struct RawOrigin {
    width: i32,
    height: i32,
}

#[derive(Deserialize)]
struct RawZone {
    id: String,
    #[serde(default)]
    disableable: bool,
    #[serde(default)]
    components: Vec<RawComponent>,
}

#[derive(Deserialize)]
struct RawComponent {
    #[serde(default)]
    id: Option<i32>,
    #[serde(default)]
    renderings: Vec<RawRendering>,
}

#[derive(Deserialize)]
struct RawRendering {
    #[serde(default)]
    rect: Option<ImageRect>,
    #[serde(default)]
    circle: Option<RawCircle>,
}

#[derive(Deserialize)]
struct RawCircle {
    cx: f64,
    cy: f64,
    r: f64,
}

#[derive(Deserialize)]
struct RawAssignment {
    #[serde(rename = "slotId")]
    slot_id: String,
    marker: RawPoint,
    #[serde(default)]
    label: Option<RawPoint>,
}

#[derive(Deserialize)]
struct RawPoint {
    x: f64,
    y: f64,
}

#[derive(Deserialize)]
struct RawHotArea {
    id: String,
    #[serde(default)]
    renderings: Vec<RawRendering>,
}

#[derive(Deserialize)]
struct RawMaskableZone {
    id: String,
    #[serde(default, rename = "cover_image")]
    cover_image: Option<RawCoverImage>,
}

#[derive(Deserialize)]
struct RawCoverImage {
    key: String,
    position: ImageRect,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(folder: &str) -> DeviceImageMetadata {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/devices/");
        let json = std::fs::read_to_string(format!("{path}{folder}/metadata.json")).unwrap();
        DeviceImageMetadata::parse(&json).unwrap()
    }

    #[test]
    fn g815_geometry_matches_the_shipped_render() {
        let m = load("g815_us");
        assert_eq!((m.origin_width, m.origin_height), (3600, 1374));
        // The per-key zone has to cover the whole keyboard or the FREESTYLE page cannot paint it.
        assert!(m.zone(zones::PER_KEY_KEYBOARD).is_some());
        assert!(m.component_count() >= 100, "only {} components", m.component_count());
        // One marker per physical G-key. G HUB's metadata is geometry only, so the `_m1` suffix
        // is vestigial: the 15 bank positions (G1-G5 x M1-M3) are profile state, and all three
        // banks draw their callout at the same five markers.
        for g in 1..=5 {
            let slot = format!("g815_g{g}_m1");
            assert!(m.assignment(&slot).is_some(), "missing marker for {slot}");
        }
        assert_eq!(m.assignments.len(), 5, "G815 has five assignable G-keys");
        // G-keys are lit individually, so they must also be in PERKEY_GKEY.
        for g in 1..=5 {
            assert!(m.gkey_bounds(g).is_some(), "G{g} has no lighting rect");
        }
    }

    #[test]
    fn g903_has_both_led_zones_and_two_removable_covers() {
        let m = load("g903_hero");
        assert_eq!((m.origin_width, m.origin_height), (1440, 2858));
        assert!(m.logo_bounds().is_some(), "palm logo LED missing");
        assert!(m.primary_bounds().is_some(), "DPI strip LED missing");
        assert_eq!(m.covers.len(), 2, "expected the two magnetic side plates");
        // Eleven assignable buttons, which is what the Mouse page has to draw callouts for.
        assert_eq!(m.assignments.len(), 11);
        for g in 1..=11 {
            let slot = format!("g903hero_g{g}_m1");
            assert!(m.assignment(&slot).is_some(), "missing marker for {slot}");
        }
    }
}
