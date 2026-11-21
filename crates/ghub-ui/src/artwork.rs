//! The device render and its per-key geometry. Port of
//! `Artemis.UI/Screens/Peripherals/DeviceArtwork.cs`.
//!
//! Both are shipped as application assets so the peripherals pages draw the real hardware without
//! G HUB installed. Unlike the fonts these are not embedded - the two renders are 4.9MB of PNG
//! between them, which belongs next to the executable rather than inside it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use egui::{Rect, TextureHandle, TextureOptions, Vec2, pos2};
use ghub_core::{DeviceImageMetadata, ImageRect};

/// A removable cover drawn over the device render, with the bitmap already resolved.
pub struct DeviceCover {
    pub id: String,
    pub texture: TextureHandle,
    pub position: ImageRect,
}

pub struct DeviceArtwork {
    pub texture: TextureHandle,
    pub metadata: DeviceImageMetadata,
    /// Removable covers (the G903's magnetic side-button plates), already positioned in the
    /// render's own coordinate space. Empty for devices that have none.
    pub covers: Vec<DeviceCover>,
}

impl DeviceArtwork {
    /// Width of the coordinate space every rectangle in [`Self::metadata`] is expressed in.
    pub fn width(&self) -> f32 {
        self.metadata.origin_width as f32
    }

    /// Height of that coordinate space.
    pub fn height(&self) -> f32 {
        self.metadata.origin_height as f32
    }

    /// Maps a rectangle in the render's coordinate space onto the screen, given the rect the whole
    /// render was drawn into. Scale is uniform, so a single factor suffices.
    pub fn project(&self, into: Rect, r: ImageRect) -> Rect {
        let scale = (into.width() / self.width()).min(into.height() / self.height());
        let drawn = Vec2::new(self.width() * scale, self.height() * scale);
        let origin = into.min + (into.size() - drawn) / 2.0;
        Rect::from_min_size(
            origin + Vec2::new(r.x as f32 * scale, r.y as f32 * scale),
            Vec2::new(r.width as f32 * scale, r.height as f32 * scale),
        )
    }

    /// The rect the render itself occupies inside `into`, letterboxed and centred.
    pub fn image_rect(&self, into: Rect) -> Rect {
        let scale = (into.width() / self.width()).min(into.height() / self.height());
        let drawn = Vec2::new(self.width() * scale, self.height() * scale);
        let origin = into.min + (into.size() - drawn) / 2.0;
        Rect::from_min_size(origin, drawn)
    }

}

/// Loads the artwork for an asset folder (for example `g815_us`), or `None` when it is not
/// shipped. Callers fall back to drawing keycaps from the layout's own coordinates.
///
/// Textures are cached for the life of the process: a render is up to 3600px wide and decoding one
/// is not something to do per frame.
#[derive(Default)]
pub struct ArtworkCache {
    loaded: HashMap<String, Option<DeviceArtwork>>,
    root: PathBuf,
}

impl ArtworkCache {
    /// `assets/devices` beside the executable, falling back to the source tree so the app runs
    /// from `cargo run` without an install step.
    pub fn new() -> Self {
        Self { loaded: HashMap::new(), root: asset_root() }
    }

    pub fn get(&mut self, ctx: &egui::Context, folder: &str) -> Option<&DeviceArtwork> {
        if !self.loaded.contains_key(folder) {
            let artwork = self.load(ctx, folder);
            if artwork.is_none() {
                log::warn!("no device artwork for {folder} under {}", self.root.display());
            }
            self.loaded.insert(folder.to_owned(), artwork);
        }
        self.loaded.get(folder).and_then(Option::as_ref)
    }

    fn load(&self, ctx: &egui::Context, folder: &str) -> Option<DeviceArtwork> {
        let dir = self.root.join(folder);
        let metadata = DeviceImageMetadata::parse(&std::fs::read_to_string(dir.join("metadata.json")).ok()?)
            .map_err(|e| log::warn!("{folder}/metadata.json: {e}"))
            .ok()?;

        let texture = load_texture(ctx, &dir.join("front.png"), &format!("{folder}/front"))?;

        // G HUB keys a cover's bitmap by name ("left_cover"), not by path.
        let covers = metadata
            .covers
            .iter()
            .filter_map(|cover| {
                let file = dir.join(format!("{}.png", cover.cover_image_key));
                let texture = load_texture(ctx, &file, &format!("{folder}/{}", cover.cover_image_key))?;
                Some(DeviceCover { id: cover.id.clone(), texture, position: cover.position })
            })
            .collect();

        Some(DeviceArtwork { texture, metadata, covers })
    }
}

fn load_texture(ctx: &egui::Context, path: &Path, name: &str) -> Option<TextureHandle> {
    let bytes = std::fs::read(path).ok()?;
    let decoded = image::load_from_memory(&bytes)
        .map_err(|e| log::warn!("{}: {e}", path.display()))
        .ok()?
        .to_rgba8();
    let size = [decoded.width() as usize, decoded.height() as usize];
    let image = egui::ColorImage::from_rgba_unmultiplied(size, decoded.as_raw());
    // LINEAR: the renders are drawn well below their native size, so nearest sampling would alias
    // the keycap edges badly.
    Some(ctx.load_texture(name, image, TextureOptions::LINEAR))
}

fn asset_root() -> PathBuf {
    let beside_exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("assets/devices")));

    if let Some(dir) = beside_exe.filter(|d| d.is_dir()) {
        return dir;
    }
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/devices"))
}

/// Draws a render into `into`, letterboxed, with any fitted covers on top. Returns the rect the
/// render occupies so callers can project key geometry onto it.
pub fn draw(ui: &egui::Ui, artwork: &DeviceArtwork, into: Rect, fitted: &[&str]) -> Rect {
    let image = artwork.image_rect(into);
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    ui.painter().image(artwork.texture.id(), image, uv, egui::Color32::WHITE);

    for cover in artwork.covers.iter().filter(|c| fitted.contains(&c.id.as_str())) {
        let at = artwork.project(into, cover.position);
        ui.painter().image(cover.texture.id(), at, uv, egui::Color32::WHITE);
    }
    image
}
