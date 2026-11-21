//! G HUB's own icons, taken from the application's asset bundle.
//!
//! The Avalonia build drew these with `Material.Icons.Avalonia`, picking the nearest Material
//! Design glyph for each place G HUB has an icon - a stand-in, because it had no way to embed
//! Logitech's. Here they are the real thing: G HUB ships its UI as an Electron app whose
//! `app.asar` holds 169 SVGs under content-hashed names, and `app.min.css` maps each hash to the
//! class that uses it (`.menu-icon-wrapper.lighting`, `.color-picker .picker .eye-dropper-button`,
//! and so on). The forty-nine the peripheral pages need were resolved through that mapping and
//! rasterised to `assets/icons`, named after the G HUB class rather than after a Material glyph.
//!
//! They are rasterised rather than shipped as SVG on purpose: an SVG renderer able to handle these
//! correctly (even-odd fills, elliptical arcs) is a megabyte or two of dependency to draw
//! forty-nine fixed shapes. Rasterising at 64px - twice the natural size of the largest of them -
//! keeps them crisp at 2x display scaling and costs 202KB of PNG for the whole set.
//!
//! Two storage forms, because G HUB's icons come in two kinds:
//!
//! * **Monochrome** (thirty-six of them): a single fill whose antialiasing lives entirely in the
//!   alpha channel. Stored as an 8-bit grey alpha mask and tinted at draw time, which is what lets
//!   one asset serve the icon rail's three states - G HUB varies a rail icon by opacity, not by
//!   swapping the image.
//! * **Full colour** (thirteen): the warning amber, the error red, the gold star, the red slash on
//!   the no-colour swatch. Stored as RGBA and drawn as-is, because tinting them would throw away
//!   the colour that is the point of them.

use std::collections::HashMap;
use std::path::PathBuf;

use egui::{Color32, Rect, TextureHandle, TextureOptions, Vec2};

/// One of G HUB's icons. The names are the CSS classes `app.min.css` attaches each asset to, so a
/// selector in G HUB's own stylesheet maps onto a variant here without a lookup table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
// The 50 assets resolved out of G HUB's own stylesheet and shipped under `assets/icons`. Some
// have no caller: they belong to G HUB screens this build renders differently (the integration
// star and share buttons, the pairing notices). They stay in the enum and in `ALL` because the
// set's value is being the complete, checkable list of what was extracted and what ships - an
// icon dropped from here is an icon nobody notices is missing.
#[allow(dead_code)]
pub enum Icon {
    // ---- the device-screen icon rail (`.menu-icon-wrapper.*`)
    /// `.menu-icon-wrapper.lighting` - LIGHTSYNC.
    RailLighting,
    /// `.menu-icon-wrapper.assign` - Assignments. G HUB uses the same asset for `.buttons`.
    RailAssign,
    /// `.menu-icon-wrapper.disabled-keys` - Game Mode.
    RailDisabledKeys,
    /// `.menu-icon-wrapper.dpi` - the mouse's DPI page.
    RailDpi,
    /// `.menu-icon-wrapper.scroll-wheel`.
    RailScrollWheel,
    /// `.menu-icon-wrapper.performance` - report rate and power.
    RailPerformance,
    /// `.menu-icon-wrapper.connect` - wireless link.
    RailConnect,
    /// `.menu-icon-wrapper.character-menu` - the per-device settings page.
    RailCharacterMenu,
    /// `.menu-icon-wrapper.sampler` - stands in for the macro pages.
    RailSampler,
    /// `.menu-icon-wrapper.rapid-switches` - the key-switch glyph, which is what the Press
    /// Reactions page uses. That page has no G HUB counterpart, so there is no rail class that
    /// names it; this is the one G HUB authored *about key presses*, which is as close as its own
    /// asset set gets.
    RailRapidSwitches,

    // ---- chrome
    /// `.dropdown-item.dropdown-button.g-icon` - the Logitech G wordmark glyph.
    GLogo,
    /// `.accordion-toggle:after` - the disclosure chevron.
    Chevron,
    /// `.left-arrow,.right-arrow`.
    ArrowSide,
    /// `.stepper-control.up` - a numeric field's increment.
    StepperUp,
    /// `.stepper-control.down`.
    StepperDown,
    /// `.checkbox-label` - an unchecked box.
    Checkbox,
    /// `.checkbox:checked+label.checkbox-label`.
    CheckboxChecked,
    /// `.info-tooltip` - the inline help bubble.
    InfoTooltip,
    /// `.overlay-close`, `.delete-button` - G HUB uses one asset for both.
    Close,
    /// `.panel-options-button.edit`.
    Edit,
    /// `.panel-options-button.copy`.
    Copy,
    /// `.preset-list .preset-add .more-presets-icon` - add.
    Add,
    /// `.action-container-button.add-holder .add:after`.
    AddAction,
    /// `.action-container-button.button-holder .done:after`.
    Done,
    /// `.pairing-checkmark-icon`.
    Checkmark,
    /// `.dfu-warning-icon`, `.info-message-icon.pairing-warning`.
    Warning,
    /// `.info-message-icon.pairing-caution`.
    Caution,
    /// `.integration-error-indicator`.
    Error,
    /// `.rating-star:before`.
    Star,
    /// `.rating-star.selected:before`.
    StarSelected,
    /// `.share-button`.
    Share,
    /// `.notification .action-button.dismiss`.
    Dismiss,
    /// `.lighting-sync-overlay .device-container .sync-lock`.
    SyncLock,

    // ---- lighting tools
    /// `.color-picker .picker .eye-dropper-button`.
    Eyedropper,
    /// `.brush-mode .led` - the freestyle brush.
    Brush,
    /// `.color-picker .preset-circle.no-color`.
    NoColor,
    /// `.color-picker .hue-picker .blocked-picker`.
    Blocked,

    // ---- macro recorder transport (`.media-controls .*`)
    Play,
    PlayDisabled,
    Stop,
    Record,
    RecordDisabled,
    StopRecord,

    // ---- macro repeat modes (`.macro-type-button.*`)
    MacroSimple,
    MacroRepeat,
    MacroToggle,
    MacroSequence,

    // ---- mouse DPI page
    /// `.DpiCanvasAdvancedTableRowItem.lock-icon .linked`.
    LockLinked,
    /// `.DpiCanvasAdvancedTableRowItem.lock-icon .unlinked`.
    LockUnlinked,
    /// `.dpi-assign-button`.
    DpiAssign,

    // ---- G HUB's *system tray* asset set (`LGHUB/system_tray/*_dark.png`)
    //
    // A second, separate set from the web UI's: the tray application is native WinUI and ships
    // its own 24px glyphs beside its executable rather than inside the asar. They are taken from
    // there because they are the icons G HUB itself uses for exactly these meanings - battery
    // level, how a device is attached, and the three severities of a status message.
    //
    // The `_dark` variant of each pair is the one extracted: it is the white-on-transparent form,
    // which is what a coverage mask needs, and this UI is dark.
    /// `status_info_dark.png`.
    StatusInfo,
    /// `status_warning_dark.png`.
    StatusWarning,
    /// `status_error_dark.png`.
    StatusError,
    /// `battery_0_dark.png`.
    Battery0,
    /// `battery_25_dark.png`.
    Battery25,
    /// `battery_50_dark.png`.
    Battery50,
    /// `battery_75_dark.png`.
    Battery75,
    /// `battery_100_dark.png`.
    Battery100,
    /// `battery_charging_dark.png`.
    BatteryCharging,
    /// `connection_usb_dark.png`.
    ConnectionUsb,
    /// `connection_lightspeed_dark.png`.
    ConnectionLightspeed,
    /// `connection_bluetooth_dark.png`.
    ConnectionBluetooth,
    /// `report_rate_dark.png`.
    ReportRate,
}

impl Icon {
    /// The asset's file stem under `assets/icons`.
    pub fn asset(self) -> &'static str {
        match self {
            Icon::RailLighting => "rail-lighting",
            Icon::RailAssign => "rail-assign",
            Icon::RailDisabledKeys => "rail-disabled-keys",
            Icon::RailDpi => "rail-dpi",
            Icon::RailScrollWheel => "rail-scroll-wheel",
            Icon::RailPerformance => "rail-performance",
            Icon::RailConnect => "rail-connect",
            Icon::RailCharacterMenu => "rail-character-menu",
            Icon::RailSampler => "rail-sampler",
            Icon::RailRapidSwitches => "rail-rapid-switches",
            Icon::GLogo => "g-logo",
            Icon::Chevron => "chevron",
            Icon::ArrowSide => "arrow-side",
            Icon::StepperUp => "stepper-up",
            Icon::StepperDown => "stepper-down",
            Icon::Checkbox => "checkbox",
            Icon::CheckboxChecked => "checkbox-checked",
            Icon::InfoTooltip => "info-tooltip",
            Icon::Close => "close",
            Icon::Edit => "edit",
            Icon::Copy => "copy",
            Icon::Add => "add",
            Icon::AddAction => "add-action",
            Icon::Done => "done",
            Icon::Checkmark => "checkmark",
            Icon::Warning => "warning",
            Icon::Caution => "caution",
            Icon::Error => "error",
            Icon::Star => "star",
            Icon::StarSelected => "star-selected",
            Icon::Share => "share",
            Icon::Dismiss => "dismiss",
            Icon::SyncLock => "sync-lock",
            Icon::Eyedropper => "eyedropper",
            Icon::Brush => "brush",
            Icon::NoColor => "no-color",
            Icon::Blocked => "blocked",
            Icon::Play => "play",
            Icon::PlayDisabled => "play-disabled",
            Icon::Stop => "stop",
            Icon::Record => "record",
            Icon::RecordDisabled => "record-disabled",
            Icon::StopRecord => "stop-record",
            Icon::MacroSimple => "macro-simple",
            Icon::MacroRepeat => "macro-repeat",
            Icon::MacroToggle => "macro-toggle",
            Icon::MacroSequence => "macro-sequence",
            Icon::LockLinked => "lock-linked",
            Icon::LockUnlinked => "lock-unlinked",
            Icon::DpiAssign => "dpi-assign",
            Icon::StatusInfo => "status-info",
            Icon::StatusWarning => "status-warning",
            Icon::StatusError => "status-error",
            Icon::Battery0 => "battery-0",
            Icon::Battery25 => "battery-25",
            Icon::Battery50 => "battery-50",
            Icon::Battery75 => "battery-75",
            Icon::Battery100 => "battery-100",
            Icon::BatteryCharging => "battery-charging",
            Icon::ConnectionUsb => "connection-usb",
            Icon::ConnectionLightspeed => "connection-lightspeed",
            Icon::ConnectionBluetooth => "connection-bluetooth",
            Icon::ReportRate => "report-rate",
        }
    }

    /// Whether the asset carries its own colour.
    ///
    /// A full-colour icon ignores the tint it is drawn with, because its colour *is* its meaning:
    /// tinting the amber warning triangle to label grey would turn a warning into decoration.
    pub fn is_full_colour(self) -> bool {
        matches!(
            self,
            Icon::Blocked
                | Icon::Caution
                | Icon::CheckboxChecked
                | Icon::Checkmark
                | Icon::DpiAssign
                | Icon::Error
                | Icon::InfoTooltip
                | Icon::NoColor
                | Icon::Record
                | Icon::Star
                | Icon::StarSelected
                | Icon::StopRecord
                | Icon::Warning
        )
    }

    /// Every icon: the manifest of what `assets/icons` must contain.
    ///
    /// Its one caller is the asset-completeness test, which is also why it has to exist - a
    /// missing asset shows as a silently blank button, so something has to assert the whole set
    /// is present and decodable before a release.
    #[allow(dead_code)]
    pub const ALL: &'static [Icon] = &[
        Icon::RailLighting,
        Icon::RailAssign,
        Icon::RailDisabledKeys,
        Icon::RailDpi,
        Icon::RailScrollWheel,
        Icon::RailPerformance,
        Icon::RailConnect,
        Icon::RailCharacterMenu,
        Icon::RailSampler,
        Icon::RailRapidSwitches,
        Icon::GLogo,
        Icon::Chevron,
        Icon::ArrowSide,
        Icon::StepperUp,
        Icon::StepperDown,
        Icon::Checkbox,
        Icon::CheckboxChecked,
        Icon::InfoTooltip,
        Icon::Close,
        Icon::Edit,
        Icon::Copy,
        Icon::Add,
        Icon::AddAction,
        Icon::Done,
        Icon::Checkmark,
        Icon::Warning,
        Icon::Caution,
        Icon::Error,
        Icon::Star,
        Icon::StarSelected,
        Icon::Share,
        Icon::Dismiss,
        Icon::SyncLock,
        Icon::Eyedropper,
        Icon::Brush,
        Icon::NoColor,
        Icon::Blocked,
        Icon::Play,
        Icon::PlayDisabled,
        Icon::Stop,
        Icon::Record,
        Icon::RecordDisabled,
        Icon::StopRecord,
        Icon::MacroSimple,
        Icon::MacroRepeat,
        Icon::MacroToggle,
        Icon::MacroSequence,
        Icon::LockLinked,
        Icon::LockUnlinked,
        Icon::DpiAssign,
        Icon::StatusInfo,
        Icon::StatusWarning,
        Icon::StatusError,
        Icon::Battery0,
        Icon::Battery25,
        Icon::Battery50,
        Icon::Battery75,
        Icon::Battery100,
        Icon::BatteryCharging,
        Icon::ConnectionUsb,
        Icon::ConnectionLightspeed,
        Icon::ConnectionBluetooth,
        Icon::ReportRate,
    ];
}

/// An icon's pixels as straight RGBA, with no egui context involved.
///
/// The window icon and the notification-area icon are handed to the OS rather than drawn, and the
/// OS has no tint step - so a monochrome mask has to be expanded into `colour` here. `None` when
/// the asset is missing or will not decode, which the caller reports rather than substituting
/// something: an invented tray icon would be exactly the look-alike this asset set exists to
/// avoid.
pub fn rgba(icon: Icon, colour: [u8; 3]) -> Option<(u32, u32, Vec<u8>)> {
    let path = asset_root().join(format!("{}.png", icon.asset()));
    let bytes = std::fs::read(&path).ok()?;
    let decoded =
        image::load_from_memory(&bytes).map_err(|e| log::warn!("{}: {e}", path.display())).ok()?;

    if icon.is_full_colour() {
        let rgba = decoded.to_rgba8();
        Some((rgba.width(), rgba.height(), rgba.into_raw()))
    } else {
        let grey = decoded.to_luma8();
        let pixels = grey
            .as_raw()
            .iter()
            .flat_map(|a| [colour[0], colour[1], colour[2], *a])
            .collect();
        Some((grey.width(), grey.height(), pixels))
    }
}

/// Decoded icon textures, loaded on first use and kept for the life of the process.
pub struct IconCache {
    loaded: HashMap<Icon, Option<TextureHandle>>,
    root: PathBuf,
}

impl Default for IconCache {
    fn default() -> Self {
        Self::new()
    }
}

impl IconCache {
    pub fn new() -> Self {
        Self { loaded: HashMap::new(), root: asset_root() }
    }

    fn texture(&mut self, ctx: &egui::Context, icon: Icon) -> Option<&TextureHandle> {
        if !self.loaded.contains_key(&icon) {
            let loaded = self.load(ctx, icon);
            if loaded.is_none() {
                log::warn!("missing icon asset {} under {}", icon.asset(), self.root.display());
            }
            self.loaded.insert(icon, loaded);
        }
        self.loaded.get(&icon).and_then(Option::as_ref)
    }

    fn load(&self, ctx: &egui::Context, icon: Icon) -> Option<TextureHandle> {
        let path = self.root.join(format!("{}.png", icon.asset()));
        let bytes = std::fs::read(&path).ok()?;
        let decoded =
            image::load_from_memory(&bytes).map_err(|e| log::warn!("{}: {e}", path.display())).ok()?;

        let image = if icon.is_full_colour() {
            let rgba = decoded.to_rgba8();
            egui::ColorImage::from_rgba_unmultiplied(
                [rgba.width() as usize, rgba.height() as usize],
                rgba.as_raw(),
            )
        } else {
            // The mask holds coverage in its grey level. Expanded to white-with-alpha so the draw
            // call's tint colour multiplies through to exactly the wanted colour.
            let grey = decoded.to_luma8();
            let pixels: Vec<u8> = grey.as_raw().iter().flat_map(|a| [255, 255, 255, *a]).collect();
            egui::ColorImage::from_rgba_unmultiplied(
                [grey.width() as usize, grey.height() as usize],
                &pixels,
            )
        };

        // LINEAR: these are 64px assets drawn at 14-32px, so nearest sampling would break up the
        // thin strokes G HUB draws them with.
        Some(ctx.load_texture(icon.asset(), image, TextureOptions::LINEAR))
    }

    /// Draws `icon` centred in `rect` at `size`, tinted with `colour`.
    ///
    /// A full-colour icon ignores `colour` except for its alpha, which is how a disabled control
    /// dims one without recolouring it.
    pub fn draw(&mut self, ui: &egui::Ui, icon: Icon, rect: Rect, size: f32, colour: Color32) {
        let Some(texture) = self.texture(ui.ctx(), icon) else {
            return;
        };
        let into = Rect::from_center_size(rect.center(), Vec2::splat(size));
        let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        let tint = if icon.is_full_colour() {
            // Only the alpha carries over, so the artwork's own colours survive.
            Color32::from_white_alpha(colour.a())
        } else {
            colour
        };
        ui.painter().image(texture.id(), into, uv, tint);
    }

    /// Allocates `size` square in the current layout and draws the icon into it.
    pub fn show(&mut self, ui: &mut egui::Ui, icon: Icon, size: f32, colour: Color32) -> egui::Response {
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
        self.draw(ui, icon, rect, size, colour);
        response
    }
}

/// `assets/icons` beside the executable, falling back to the source tree so `cargo run` works
/// without an install step. Same arrangement as the device renders.
fn asset_root() -> PathBuf {
    let beside_exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("assets/icons")));

    if let Some(dir) = beside_exe.filter(|d| d.is_dir()) {
        return dir;
    }
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/icons"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A missing asset shows as a silently blank button, so the whole set is checked for presence
    /// and decodability rather than trusting the enum.
    #[test]
    fn every_icon_has_an_asset_that_decodes() {
        let root = asset_root();
        for icon in Icon::ALL {
            let path = root.join(format!("{}.png", icon.asset()));
            let bytes = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("{} is missing: {e}", path.display()));
            let decoded = image::load_from_memory(&bytes)
                .unwrap_or_else(|e| panic!("{} did not decode: {e}", path.display()));
            assert!(
                decoded.width() > 0 && decoded.height() > 0,
                "{} decoded to nothing",
                path.display()
            );
            // Rasterised to fit a 64px box, so neither side may exceed it.
            assert!(
                decoded.width() <= 64 && decoded.height() <= 64,
                "{} is {}x{}, larger than the 64px the set is rendered at",
                path.display(),
                decoded.width(),
                decoded.height()
            );
        }
    }

    /// Two variants sharing an asset stem would mean one of them silently drew the other's icon.
    #[test]
    fn asset_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for icon in Icon::ALL {
            assert!(seen.insert(icon.asset()), "{} is used by two variants", icon.asset());
        }
        assert_eq!(seen.len(), Icon::ALL.len());
    }

    /// `ALL` drives the asset test, so a variant left out of it would go unchecked. There is no
    /// reflection to enumerate variants, so the count is pinned instead: adding one without
    /// listing it fails here.
    #[test]
    fn all_lists_every_variant() {
        assert_eq!(Icon::ALL.len(), 63);
    }

    /// The monochrome/full-colour split decides whether an icon can be tinted, and getting it
    /// wrong is a visible bug in one direction (a grey warning triangle) and an invisible icon in
    /// the other. It is derived from the assets themselves: a mask is 8-bit grey, a colour icon is
    /// not.
    #[test]
    fn the_colour_split_matches_how_each_asset_was_stored() {
        let root = asset_root();
        for icon in Icon::ALL {
            let path = root.join(format!("{}.png", icon.asset()));
            let bytes = std::fs::read(&path).expect("the asset exists");
            let decoded = image::load_from_memory(&bytes).expect("the asset decodes");
            let has_colour_channels = !matches!(
                decoded.color(),
                image::ColorType::L8 | image::ColorType::L16
            );
            assert_eq!(
                icon.is_full_colour(),
                has_colour_channels,
                "{} is stored as {:?} but is_full_colour() says {}",
                icon.asset(),
                decoded.color(),
                icon.is_full_colour()
            );
        }
    }

    /// Thirteen of the 63 carry their own colour; the other 50 are coverage masks tinted at draw
    /// time. The tray set added nothing to the colour side: every one of G HUB's own tray glyphs
    /// is monochrome, including the three status severities - it colours the *text* beside them,
    /// not the glyph.
    #[test]
    fn thirteen_of_the_set_carry_their_own_colour() {
        let colour = Icon::ALL.iter().filter(|i| i.is_full_colour()).count();
        assert_eq!(colour, 13);
        assert_eq!(Icon::ALL.len() - colour, 50);
    }

    /// Battery level is picked by threshold, so the five steps have to be exactly G HUB's own
    /// five files - a missing step would silently round a battery to the wrong glyph.
    #[test]
    fn the_battery_set_is_the_five_steps_g_hub_ships() {
        let steps = [
            Icon::Battery0,
            Icon::Battery25,
            Icon::Battery50,
            Icon::Battery75,
            Icon::Battery100,
        ];
        for (index, icon) in steps.iter().enumerate() {
            assert_eq!(icon.asset(), format!("battery-{}", index * 25));
            assert!(!icon.is_full_colour(), "the tray glyphs are masks");
        }
    }

    /// The agent ships the G mark a second time, pre-decoded, because it links no PNG decoder:
    /// one 64x64 mask is not worth `image` and its inflater in a binary whose whole point is to
    /// be small. Two copies of one asset can drift, so this is the tie between them - the decoder
    /// lives here, so the comparison has to as well.
    ///
    /// A re-extracted or re-coloured G mark therefore fails this test rather than quietly leaving
    /// the notification area on the old artwork.
    #[test]
    fn the_agents_pre_decoded_tray_icon_is_the_same_pixels_as_the_png() {
        let (width, height, pixels) = rgba(Icon::GLogo, [255, 255, 255]).expect("the G mark loads");
        let path = asset_root().join("../tray/g-logo.rgba");
        let blob = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{} could not be read: {e}", path.display()));

        assert!(blob.len() > 8, "{} is shorter than its header", path.display());
        assert_eq!(
            u32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]),
            width,
            "the pre-decoded icon's width disagrees with the PNG's"
        );
        assert_eq!(
            u32::from_le_bytes([blob[4], blob[5], blob[6], blob[7]]),
            height,
            "the pre-decoded icon's height disagrees with the PNG's"
        );
        assert_eq!(
            &blob[8..],
            pixels.as_slice(),
            "assets/tray/g-logo.rgba is no longer the decoded g-logo.png; regenerate it"
        );
    }
}
