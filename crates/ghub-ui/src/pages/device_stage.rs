//! The keyboard stage: the device render, the per-key overlays drawn over it, and the pointer
//! gestures that edit them. Port of `KeyViewModel`, the four overlay `ItemsControl`s in
//! `KeyboardTabView.axaml`, and the pointer handlers in `KeyboardTabView.axaml.cs`.
//!
//! G HUB draws the keyboard as a photograph with coloured light bleeding out of the printed
//! legends, not as a grid of painted keycaps. That is reproduced literally: the render is the
//! image, and a lit key is a translucent wash of its colour over the cap the render already shows.
//! Painting the cap itself would hide the legend and stop looking like the hardware.
//!
//! Two coordinate spaces are in play. *Canvas* space is what the key geometry is expressed in -
//! either the render's own pixels (when artwork is shipped) or key units times [`UNIT`] (the
//! vector fallback). *Screen* space is where the pointer is. [`Projection`] is the only thing that
//! converts between them, so a hit test and the drawing it is testing against cannot drift apart.

use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Ui, Vec2, pos2};
use ghub_core::layout::{KeyDefinition, KeyGroup};
use ghub_core::{DeviceImageMetadata, ImageRect};

use crate::artwork::DeviceArtwork;
use crate::icons::{Icon, IconCache};
use crate::theme as t;
use crate::widgets as w;

/// `KeyViewModel.Unit` - one key unit in the vector fallback.
pub const UNIT: f64 = 38.0;

/// `KeyViewModel.UnlitLegend` - what an unlit legend is drawn in.
pub const UNLIT_LEGEND: Color32 = Color32::from_rgb(0x5A, 0x5C, 0x60);

/// `<Viewbox MaxWidth="1090" MaxHeight="420">` - the stage never grows past the size the captures
/// show, so a wide window gets margins rather than a four-foot keyboard.
pub const STAGE_MAX: Vec2 = Vec2::new(1090.0, 420.0);

/// `GameModeKeyState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GameModeKeyState {
    /// Not lockable at all. G HUB: "Keys cannot be modified".
    #[default]
    Locked,
    /// Lockable and currently enabled.
    Enabled,
    /// Disabled by the user: solid accent fill with a slash.
    DisabledByUser,
    /// Disabled out of the box: hatched fill. Both Windows keys on this model.
    DisabledByDefault,
}

impl GameModeKeyState {
    pub fn hatched(self) -> bool {
        self == Self::DisabledByDefault
    }

    pub fn filled(self) -> bool {
        self == Self::DisabledByUser
    }
}

/// One key on the stage. `KeyViewModel`.
#[derive(Debug, Clone)]
pub struct KeyView {
    pub key: &'static KeyDefinition,
    /// Position on the device render, when the artwork ships geometry for this key.
    pub bounds: Option<ImageRect>,
    pub selected: bool,
    /// The key's lighting colour, or `None` for a key that is not individually lit.
    pub colour: Option<Color32>,
    pub game_state: GameModeKeyState,
    /// Set for keys with an enabled press reaction, in the reaction's colour.
    pub reaction: Option<Color32>,
}

impl KeyView {
    pub fn new(key: &'static KeyDefinition, bounds: Option<ImageRect>) -> Self {
        Self {
            key,
            bounds,
            selected: false,
            colour: None,
            game_state: GameModeKeyState::default(),
            reaction: None,
        }
    }

    /// `KeyViewModel.Lit`.
    pub fn lit(&self) -> bool {
        self.key.led_id != 0
    }

    /// `KeyViewModel.Label` - the space bar gets a bar rather than the word.
    pub fn label(&self) -> String {
        if self.key.id == "Space" {
            "\u{2501}\u{2501}\u{2501}".to_owned()
        } else {
            self.key.label.to_uppercase()
        }
    }

    /// `KeyViewModel.FontSize`.
    pub fn font_size(&self) -> f32 {
        if self.key.label.chars().count() > 3 { 8.0 } else { 10.0 }
    }

    /// The key's rectangle in canvas space, or `None` when it cannot be drawn in this mode.
    ///
    /// `KeyAt` required `(k.Bounds != null) == HasArtwork`: with the render on screen only keys the
    /// artwork places are addressable, and without it only the vector grid is. Mixing the two put
    /// a unit-grid rectangle on top of a photograph.
    pub fn canvas_rect(&self, has_artwork: bool) -> Option<Rect> {
        match (self.bounds, has_artwork) {
            (Some(r), true) => Some(Rect::from_min_size(
                pos2(r.x as f32, r.y as f32),
                Vec2::new(r.width as f32, r.height as f32),
            )),
            (None, false) => Some(Rect::from_min_size(
                pos2((self.key.x * UNIT) as f32, (self.key.y * UNIT) as f32),
                // `Key.W * Unit - 3`: the 3px is the gap between caps in the vector fallback.
                Vec2::new((self.key.w * UNIT - 3.0) as f32, (self.key.h * UNIT - 3.0) as f32),
            )),
            _ => None,
        }
    }

    /// `KeyViewModel.Tip` - the hover text, including the LED evidence so a derived id is never
    /// presented as a confirmed one.
    pub fn tip(&self) -> String {
        let usage = if self.key.hid_usage != 0 {
            format!(" (HID 0x{:02X})", self.key.hid_usage)
        } else {
            String::new()
        };
        let led = if self.lit() {
            format!(
                ", LED 0x{:02X} ({})",
                self.key.led_id,
                self.key.led_evidence().label()
            )
        } else {
            ", not individually lit".to_owned()
        };
        format!("{}{usage}{led}", self.key.id)
    }
}

/// `BoundsFor` - where a key sits on the device render.
///
/// Each family resolves through its own table, and that separation is the point. Media keys use the
/// HID *consumer* page, where 226 is Mute, while 226 on the keyboard page is Left Alt; searching
/// one table for both put Mute on top of Left Alt and left the real Mute key unclickable.
pub fn bounds_for(key: &KeyDefinition, metadata: Option<&DeviceImageMetadata>) -> Option<ImageRect> {
    let metadata = metadata?;

    if key.group == KeyGroup::Logo {
        return metadata.logo_bounds();
    }
    if key.group == KeyGroup::GKey {
        let number: i32 = key.id.get(1..)?.parse().ok()?;
        return metadata.gkey_bounds(number);
    }
    if key.group == KeyGroup::Media {
        return media_consumer_usage(key.id).and_then(|c| metadata.consumer_bounds(c));
    }
    if key.id == "Brightness" {
        return metadata.indicator_bounds();
    }
    if key.id == "GameMode" {
        // G HUB draws the game-mode key as a circular hot area rather than a lit component, so its
        // click target is that circle's bounding box.
        let hot = metadata.hot_area("gameMode")?;
        if hot.radius <= 0.0 {
            return None;
        }
        return Some(ImageRect {
            x: hot.center_x - hot.radius,
            y: hot.center_y - hot.radius,
            width: hot.radius * 2.0,
            height: hot.radius * 2.0,
        });
    }
    if key.hid_usage != 0 {
        return metadata.key_bounds(key.hid_usage);
    }
    // The M-keys have no lit component and therefore no bounds.
    None
}

/// `MediaConsumerUsage` - the render places the media row by consumer usage, which the layout does
/// not carry.
pub fn media_consumer_usage(key_id: &str) -> Option<u8> {
    match key_id {
        "MediaNext" => Some(181),
        "MediaPrevious" => Some(182),
        "MediaPlayPause" => Some(205),
        "MediaMute" => Some(226),
        _ => None,
    }
}

/// Builds the stage's key list for a layout, resolving each key's render geometry.
pub fn build_keys(metadata: Option<&DeviceImageMetadata>) -> Vec<KeyView> {
    ghub_core::layout::keys()
        .iter()
        .map(|key| KeyView::new(key, bounds_for(key, metadata)))
        .collect()
}

/// The canvas size: the render's own coordinate space, or the layout's extent in key units.
///
/// `CanvasWidth` / `CanvasHeight`.
pub fn canvas_size(artwork: Option<&DeviceArtwork>) -> Vec2 {
    match artwork {
        Some(art) => Vec2::new(art.width(), art.height()),
        None => {
            let (w, h) = ghub_core::layout::extent();
            Vec2::new((w * UNIT) as f32, (h * UNIT) as f32)
        }
    }
}

/// The uniform letterboxed map between canvas space and screen space.
///
/// This is Avalonia's `<Viewbox Stretch="Uniform">` written out. It exists as a value rather than
/// as two functions so the drawing pass and the hit test provably share one scale: when they did
/// not, a click landed one key left of where the pointer was.
#[derive(Debug, Clone, Copy)]
pub struct Projection {
    pub origin: Pos2,
    pub scale: f32,
    pub canvas: Vec2,
}

impl Projection {
    pub fn new(into: Rect, canvas: Vec2) -> Self {
        let scale = (into.width() / canvas.x).min(into.height() / canvas.y).max(f32::EPSILON);
        let drawn = canvas * scale;
        Self { origin: into.min + (into.size() - drawn) / 2.0, scale, canvas }
    }

    /// The rect the whole canvas occupies on screen.
    pub fn viewport(&self) -> Rect {
        Rect::from_min_size(self.origin, self.canvas * self.scale)
    }

    pub fn project(&self, canvas: Rect) -> Rect {
        Rect::from_min_size(
            self.origin + canvas.min.to_vec2() * self.scale,
            canvas.size() * self.scale,
        )
    }

    /// Screen to canvas. `None` when the point is outside the canvas, which is what makes a click
    /// on the margin a click on nothing rather than on the nearest edge key.
    pub fn unproject(&self, screen: Pos2) -> Option<Pos2> {
        let viewport = self.viewport();
        if !viewport.contains(screen) {
            return None;
        }
        Some(pos2(
            (screen.x - self.origin.x) / self.scale,
            (screen.y - self.origin.y) / self.scale,
        ))
    }
}

/// Which overlay the stage draws, which is also what a click does. The four `ItemsControl`s in the
/// view, each with its own `IsVisible`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    /// Lighting: the glow wash plus the selection ring. Editable only on FREESTYLE.
    Lighting { editable: bool },
    /// Game mode: hatch for the defaults, accent fill plus a slash for the user's.
    GameMode,
    /// Press reactions: a 55% wash and a 3px ring in the reaction colour.
    Reactions,
}

/// `KeyAt(x, y, requireLit)` - the first key whose canvas rect contains the point.
///
/// First, not nearest: the overlays are drawn in list order and the topmost one is the one the
/// pointer is over, so the first match is the one the user sees themselves clicking.
pub fn key_at(
    keys: &[KeyView],
    has_artwork: bool,
    at: Pos2,
    require_lit: bool,
) -> Option<usize> {
    keys.iter().position(|key| {
        (key.lit() || !require_lit)
            && key
                .canvas_rect(has_artwork)
                .is_some_and(|rect| rect.contains(at))
    })
}

/// `SelectRectangle`'s hit set: every lit key the marquee overlaps, in canvas space.
pub fn keys_in_rect(keys: &[KeyView], has_artwork: bool, marquee: Rect) -> Vec<usize> {
    keys.iter()
        .enumerate()
        .filter(|(_, key)| {
            key.lit()
                && key
                    .canvas_rect(has_artwork)
                    // `k.Left < right && k.Left + k.Width > left && ...`: overlap, not containment,
                    // so a marquee that clips a key's edge still takes it.
                    .is_some_and(|rect| rect.intersects(marquee))
        })
        .map(|(index, _)| index)
        .collect()
}

/// What a frame's pointer gestures amounted to, handed back to the page to act on.
///
/// Collected into one value rather than applied in place because every one of these ends in a
/// profile write, and the page owns that.
#[derive(Debug, Clone, Copy, Default)]
pub struct StageAction {
    /// A plain click, in canvas space, with Ctrl or Shift held.
    pub clicked: Option<(Pos2, bool)>,
    /// A completed marquee, in canvas space, with Ctrl or Shift held.
    pub marquee: Option<(Rect, bool)>,
    /// The brush passed over this point. Reported per frame while the stroke continues.
    pub painted: Option<Pos2>,
    /// The frame a brush stroke began, which is where the undo step is pushed.
    pub stroke_began: bool,
    /// The eyedropper was clicked here.
    pub sampled: Option<Pos2>,
}

impl StageAction {
    pub fn any(&self) -> bool {
        self.clicked.is_some()
            || self.marquee.is_some()
            || self.painted.is_some()
            || self.stroke_began
            || self.sampled.is_some()
    }
}

/// The gesture state that has to survive between frames: where a drag started and what kind it is.
///
/// Avalonia got this from pointer capture; egui has no capture, so the stage keeps the anchor
/// itself and reads `pointer.primary_down` to know the gesture is still live. The practical
/// difference is that a drag which leaves the stage keeps working here too, because the anchor is
/// not tied to the widget being hovered.
#[derive(Default)]
pub struct StageInput {
    /// Where the press landed, in canvas space.
    anchor: Option<Pos2>,
    /// True once the pointer has moved past the marquee threshold.
    marquee_active: bool,
    brushing: bool,
}

impl StageInput {
    /// `Math.Abs(dx) + Math.Abs(dy) < 8` - the Manhattan distance a press has to travel before it
    /// becomes a marquee rather than a click.
    pub const MARQUEE_THRESHOLD: f32 = 8.0;

    pub fn cancel(&mut self) {
        self.anchor = None;
        self.marquee_active = false;
        self.brushing = false;
    }
}

/// Draws the stage and runs its gestures.
///
/// `brush` picks the brush tool over marquee selection, `eyedropper` over both. Returns what the
/// pointer did.
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    icons: &mut IconCache,
    input: &mut StageInput,
    keys: &[KeyView],
    artwork: Option<&DeviceArtwork>,
    overlay: Overlay,
    brush: bool,
    eyedropper: bool,
) -> StageAction {
    let canvas = canvas_size(artwork);

    // `MaxWidth="1090" MaxHeight="420"`, centred in whatever the panel left over.
    let available = ui.available_size();
    let box_size = Vec2::new(
        available.x.clamp(1.0, STAGE_MAX.x),
        available.y.clamp(1.0, STAGE_MAX.y),
    );
    let (frame, response) = ui.allocate_exact_size(box_size, Sense::click_and_drag());
    let projection = Projection::new(frame, canvas);
    let has_artwork = artwork.is_some();

    if let Some(art) = artwork {
        crate::artwork::draw(ui, art, frame, &[]);
    }

    let hovered = response
        .hover_pos()
        .and_then(|screen| projection.unproject(screen))
        .and_then(|at| key_at(keys, has_artwork, at, !matches!(overlay, Overlay::GameMode)));

    draw_overlay(ui, icons, &projection, keys, has_artwork, overlay, hovered);

    // The tooltip is the stage's only way to show a key's LED id and its evidence, so it follows
    // the pointer rather than living in a panel.
    if let Some(index) = hovered {
        response.clone().on_hover_text(keys[index].tip());
    }

    let mut action = StageAction::default();
    let interactive = match overlay {
        Overlay::Lighting { editable } => editable,
        // Game mode and reactions are click-only: a drag over them does nothing, as in G HUB.
        Overlay::GameMode | Overlay::Reactions => true,
    };
    if !interactive {
        input.cancel();
        return action;
    }

    let pointer_down = ui.input(|i| i.pointer.primary_down());
    let additive = ui.input(|i| i.modifiers.ctrl || i.modifiers.shift);

    // Click-only pages first: no marquee, no brush, no undo step.
    if matches!(overlay, Overlay::GameMode | Overlay::Reactions) {
        if response.clicked() {
            if let Some(at) = response.interact_pointer_pos().and_then(|p| projection.unproject(p))
            {
                action.clicked = Some((at, additive));
            }
        }
        return action;
    }

    if eyedropper {
        if response.clicked() {
            action.sampled =
                response.interact_pointer_pos().and_then(|p| projection.unproject(p));
        }
        input.cancel();
        return action;
    }

    let pointer_canvas = ui
        .input(|i| i.pointer.interact_pos())
        .and_then(|p| projection.unproject(p));

    if response.drag_started() || (response.is_pointer_button_down_on() && input.anchor.is_none()) {
        if let Some(at) = pointer_canvas {
            input.anchor = Some(at);
            input.marquee_active = false;
            input.brushing = brush;
            if brush {
                // One undo step per stroke, pushed as the stroke begins.
                action.stroke_began = true;
                action.painted = Some(at);
            }
        }
    }

    if input.brushing {
        if pointer_down {
            if let Some(at) = pointer_canvas {
                action.painted = Some(at);
            }
        } else {
            input.cancel();
        }
        return action;
    }

    if let Some(anchor) = input.anchor {
        let now = pointer_canvas.unwrap_or(anchor);
        let travelled = (now.x - anchor.x).abs() + (now.y - anchor.y).abs();
        if !input.marquee_active && travelled * projection.scale >= StageInput::MARQUEE_THRESHOLD {
            input.marquee_active = true;
        }

        if input.marquee_active {
            let marquee = Rect::from_two_pos(anchor, now);
            // `BorderBrush="#00B4F0" BorderThickness="1.5" Background="#2200B4F0"`.
            let on_screen = projection.project(marquee);
            ui.painter().rect_filled(
                on_screen,
                CornerRadius::ZERO,
                Color32::from_rgba_unmultiplied(0x00, 0xB4, 0xF0, 0x22),
            );
            ui.painter().rect_stroke(
                on_screen,
                CornerRadius::ZERO,
                Stroke::new(1.5, Color32::from_rgb(0x00, 0xB4, 0xF0)),
                egui::StrokeKind::Inside,
            );
        }

        if !pointer_down {
            if input.marquee_active {
                action.marquee = Some((Rect::from_two_pos(anchor, now), additive));
            } else {
                action.clicked = Some((anchor, additive));
            }
            input.cancel();
        }
    }

    action
}

fn draw_overlay(
    ui: &Ui,
    icons: &mut IconCache,
    projection: &Projection,
    keys: &[KeyView],
    has_artwork: bool,
    overlay: Overlay,
    hovered: Option<usize>,
) {
    for (index, key) in keys.iter().enumerate() {
        let Some(canvas) = key.canvas_rect(has_artwork) else { continue };
        let rect = projection.project(canvas);
        // The selection ring is `stroke-width: 10` in the render's own space, so it scales with
        // the render rather than staying a fixed pixel width.
        let radius = CornerRadius::same((4.0 * projection.scale).clamp(1.0, 8.0) as u8);

        match overlay {
            Overlay::Lighting { .. } => {
                if !has_artwork {
                    // The vector fallback has no printed legend, so it draws a keycap and the
                    // legend itself.
                    w::keycap(ui, rect, key.colour, key.selected, hovered == Some(index));
                    let colour = key.colour.unwrap_or(UNLIT_LEGEND);
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        key.label(),
                        egui::FontId::new(key.font_size(), t::family(t::BODY_BOLD)),
                        colour,
                    );
                } else if let Some(colour) = key.colour.filter(|_| key.lit()) {
                    // `Opacity="0.38"` over the render: backlight bleeding out of the legend, not
                    // a painted cap.
                    ui.painter().rect_filled(rect, radius, colour.gamma_multiply(0.38));
                }
                if key.selected {
                    w::selection_ring(ui, rect);
                } else if hovered == Some(index) && has_artwork {
                    ui.painter().rect_stroke(
                        rect,
                        radius,
                        Stroke::new(1.0, t::KEYCAP_BORDER_HOVER),
                        egui::StrokeKind::Inside,
                    );
                }
            }
            Overlay::GameMode => {
                if key.game_state.hatched() {
                    w::hatch(ui, rect);
                } else if key.game_state.filled() {
                    ui.painter().rect_filled(rect, radius, t::ACCENT);
                    // `MaterialIcon Kind="CancelOutline"`, which in G HUB's own set is the
                    // disabled-keys glyph.
                    let glyph = (rect.height() * 0.6).clamp(8.0, 18.0);
                    icons.draw(ui, Icon::Blocked, rect, glyph, t::TEXT);
                }
                if hovered == Some(index) && key.game_state != GameModeKeyState::Locked {
                    ui.painter().rect_stroke(
                        rect,
                        radius,
                        Stroke::new(1.0, t::ACCENT_BRIGHT),
                        egui::StrokeKind::Inside,
                    );
                }
            }
            Overlay::Reactions => {
                if let Some(colour) = key.reaction {
                    // `Opacity="0.55"` plus a 3px ring, both in the reaction colour.
                    ui.painter().rect_filled(rect, radius, colour.gamma_multiply(0.55));
                    ui.painter().rect_stroke(
                        rect,
                        radius,
                        Stroke::new(3.0, colour),
                        egui::StrokeKind::Outside,
                    );
                } else if hovered == Some(index) && key.lit() {
                    ui.painter().rect_stroke(
                        rect,
                        radius,
                        Stroke::new(1.0, t::ACCENT_BRIGHT),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vector_keys() -> Vec<KeyView> {
        build_keys(None)
    }

    #[test]
    fn the_vector_canvas_covers_the_whole_layout() {
        let size = canvas_size(None);
        let (w, h) = ghub_core::layout::extent();
        assert_eq!(size.x, (w * UNIT) as f32);
        assert_eq!(size.y, (h * UNIT) as f32);
        // Every key has to fit inside it, or the stage clips the edge of the board.
        for key in vector_keys() {
            let rect = key.canvas_rect(false).expect("no artwork means every key is on the grid");
            assert!(rect.max.x <= size.x + 0.01, "{} runs off the right", key.key.id);
            assert!(rect.max.y <= size.y + 0.01, "{} runs off the bottom", key.key.id);
        }
    }

    #[test]
    fn a_key_is_hit_at_its_own_centre_and_nowhere_else() {
        let keys = vector_keys();
        let escape = keys.iter().position(|k| k.key.id == "Escape").unwrap();
        let rect = keys[escape].canvas_rect(false).unwrap();
        assert_eq!(key_at(&keys, false, rect.center(), true), Some(escape));

        // The 3px gap between caps belongs to no key.
        let gap = pos2(rect.right() + 1.5, rect.center().y);
        assert_ne!(key_at(&keys, false, gap, true), Some(escape));
    }

    /// The M-keys are not lit, so the lighting page must not select them while game mode still can.
    #[test]
    fn require_lit_decides_whether_an_unlit_key_is_addressable() {
        let keys = vector_keys();
        let m1 = keys.iter().position(|k| k.key.id == "M1").unwrap();
        assert!(!keys[m1].lit());
        let at = keys[m1].canvas_rect(false).unwrap().center();
        assert_eq!(key_at(&keys, false, at, true), None);
        assert_eq!(key_at(&keys, false, at, false), Some(m1));
    }

    #[test]
    fn with_artwork_only_keys_the_render_places_are_addressable() {
        // No metadata, so nothing has bounds; asking for the artwork path must then find nothing
        // rather than falling back to the unit grid on top of a photograph.
        let keys = vector_keys();
        for key in &keys {
            assert!(key.canvas_rect(true).is_none());
        }
        assert_eq!(key_at(&keys, true, pos2(10.0, 10.0), false), None);
    }

    #[test]
    fn the_marquee_takes_keys_it_merely_clips() {
        let keys = vector_keys();
        let w_key = keys.iter().position(|k| k.key.id == "W").unwrap();
        let rect = keys[w_key].canvas_rect(false).unwrap();
        // A one-pixel band over the key's top edge: overlap, not containment.
        let band = Rect::from_min_max(
            pos2(rect.left() + 2.0, rect.top() - 1.0),
            pos2(rect.left() + 3.0, rect.top() + 1.0),
        );
        assert!(keys_in_rect(&keys, false, band).contains(&w_key));
    }

    #[test]
    fn projection_round_trips_a_point() {
        let into = Rect::from_min_size(pos2(50.0, 20.0), Vec2::new(800.0, 300.0));
        let projection = Projection::new(into, Vec2::new(1600.0, 500.0));
        let canvas = pos2(400.0, 250.0);
        let screen = projection.project(Rect::from_min_size(canvas, Vec2::ZERO)).min;
        let back = projection.unproject(screen).expect("inside the viewport");
        assert!((back.x - canvas.x).abs() < 0.01);
        assert!((back.y - canvas.y).abs() < 0.01);
    }

    #[test]
    fn projection_letterboxes_rather_than_stretching() {
        // A 2:1 canvas in a 1:1 box: the drawn area keeps its aspect and is centred vertically.
        let into = Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0));
        let projection = Projection::new(into, Vec2::new(800.0, 400.0));
        let viewport = projection.viewport();
        assert_eq!(viewport.width(), 400.0);
        assert_eq!(viewport.height(), 200.0);
        assert_eq!(viewport.center(), into.center());
        // A point in the margin is outside the canvas, so a click there hits nothing.
        assert!(projection.unproject(pos2(200.0, 10.0)).is_none());
    }

    #[test]
    fn a_media_key_resolves_through_the_consumer_page_not_the_keyboard_one() {
        // 226 is Mute on the consumer page and Left Alt on the keyboard page. If these shared a
        // table, Mute would be drawn over Left Alt.
        assert_eq!(media_consumer_usage("MediaMute"), Some(226));
        let left_alt = ghub_core::layout::by_id("LeftAlt").expect("the layout has Left Alt");
        assert_eq!(left_alt.hid_usage, 0xE2);
        assert_eq!(media_consumer_usage("LeftAlt"), None);
    }

    #[test]
    fn the_space_bar_is_drawn_as_a_bar() {
        let keys = vector_keys();
        let space = keys.iter().find(|k| k.key.id == "Space").unwrap();
        assert_eq!(space.label(), "\u{2501}\u{2501}\u{2501}");
    }

    #[test]
    fn the_tip_states_the_evidence_for_a_derived_led_id() {
        let keys = vector_keys();
        let g1 = keys.iter().find(|k| k.key.id == "G1").unwrap();
        let tip = g1.tip();
        // The G-key ids follow OpenRGB's zone transform but are in no key table, so the tooltip has
        // to say "derived" rather than letting the number read as confirmed.
        assert!(tip.contains("derived"), "{tip}");

        let escape = keys.iter().find(|k| k.key.id == "Escape").unwrap();
        assert!(escape.tip().contains("reference"), "{}", escape.tip());

        let m1 = keys.iter().find(|k| k.key.id == "M1").unwrap();
        assert!(m1.tip().contains("not individually lit"), "{}", m1.tip());
    }

    #[test]
    fn game_state_maps_onto_exactly_one_fill() {
        for (state, hatched, filled) in [
            (GameModeKeyState::Locked, false, false),
            (GameModeKeyState::Enabled, false, false),
            (GameModeKeyState::DisabledByUser, false, true),
            (GameModeKeyState::DisabledByDefault, true, false),
        ] {
            assert_eq!(state.hatched(), hatched, "{state:?}");
            assert_eq!(state.filled(), filled, "{state:?}");
        }
    }
}
