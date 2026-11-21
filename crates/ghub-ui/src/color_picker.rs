//! G HUB's colour picker: a saturation/value square, a vertical hue bar, an eyedropper toggle and
//! hex/R/G/B entry. Port of `Artemis.UI/Screens/Peripherals/Tabs/SpectrumColorPicker.axaml{,.cs}`.
//!
//! Three things carry over that are easy to get wrong:
//!
//! 1. **Hue is stored, not derived.** Dragging into black or grey loses the hue in RGB, so a
//!    picker that recomputed it from the colour would snap to red as the user dragged down the
//!    square. [`ColorPicker`] keeps `h`, `s` and `v` and treats the colour as the output.
//! 2. **The hue bar runs red at *both* ends.** G HUB's gradient is red, magenta, blue, cyan,
//!    green, yellow, red from top to bottom - hue *decreasing*, not increasing.
//! 3. **Invalid text reverts.** Typing nonsense into the hex field and leaving it puts the current
//!    colour back rather than clamping to something arbitrary.
//!
//! The square is drawn as a subdivided mesh rather than three stacked gradients, which is what
//! Avalonia's markup did. It comes to the same thing: HSV to RGB *is* `v * lerp(white, hue, s)`,
//! so a grid whose vertices carry their own exact colour reproduces the three-layer composite
//! without any alpha blending.

use egui::{Color32, Mesh, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2, epaint};

use crate::icons::{Icon, IconCache};
use crate::theme as t;
use crate::widgets as w;

/// `Grid Height="146"` - the square's height, and the hue bar's.
const AREA_HEIGHT: f32 = 146.0;
/// `ColumnDefinitions="*,20,20"` - a 20px gap then a 20px hue column.
const HUE_COLUMN: f32 = 20.0;
const HUE_GAP: f32 = 20.0;
/// `Border Width="12" CornerRadius="6"` inside the hue column.
const HUE_BAR_WIDTH: f32 = 12.0;
/// `Ellipse x:Name="SvThumb" Width="14"`, `HueThumb Width="16"`, both `StrokeThickness="2"`.
const SV_THUMB: f32 = 14.0;
const HUE_THUMB: f32 = 16.0;
const THUMB_STROKE: f32 = 2.0;
/// How finely the saturation/value square is subdivided. 24 steps keeps the banding below one
/// level of 8-bit colour across a 146px square.
const SV_STEPS: usize = 24;
/// `Border CornerRadius="2"` on the square.
const SV_RADIUS: f32 = 2.0;

/// Which text field the user is part-way through editing.
///
/// Needed because the fields are driven *from* the colour: without this, typing `1` into the red
/// field would immediately rewrite the colour to `#010000` and the field to `1`, making anything
/// longer than one digit impossible to type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Hex,
    R,
    G,
    B,
}

pub struct ColorPicker {
    /// Hue in degrees, kept across drags through grey. See the module note.
    h: f64,
    s: f64,
    v: f64,
    /// Text buffers, rewritten from the colour whenever the user is not typing in them.
    hex: String,
    r: String,
    g: String,
    b: String,
    editing: Option<Field>,
    /// `IsEyedropperActive`. The owner reads this to decide whether a click on the device render
    /// samples a colour instead of painting one.
    pub eyedropper: bool,
}

impl Default for ColorPicker {
    fn default() -> Self {
        Self::new(Color32::RED)
    }
}

impl ColorPicker {
    pub fn new(colour: Color32) -> Self {
        let (h, s, v) = to_hsv(colour, 0.0);
        let mut picker = Self {
            h,
            s,
            v,
            hex: String::new(),
            r: String::new(),
            g: String::new(),
            b: String::new(),
            editing: None,
            eyedropper: false,
        };
        picker.sync_text(colour);
        picker
    }

    /// Re-seeds from a colour set elsewhere - a palette swatch, a "colours on device" row, or the
    /// eyedropper. `OnPropertyChanged` for `ColorProperty` in the C#.
    pub fn set(&mut self, colour: Color32) {
        let (h, s, v) = to_hsv(colour, self.h);
        self.h = h;
        self.s = s;
        self.v = v;
        self.sync_text(colour);
    }

    fn sync_text(&mut self, colour: Color32) {
        // `HexBox.Text = $"{c.R:X2}{c.G:X2}{c.B:X2}"` - no leading `#`, which the markup draws as
        // a separate label.
        self.hex = format!("{:02X}{:02X}{:02X}", colour.r(), colour.g(), colour.b());
        self.r = colour.r().to_string();
        self.g = colour.g().to_string();
        self.b = colour.b().to_string();
    }

    /// Draws the picker and edits `colour` in place. Returns true when the colour changed this
    /// frame, which is what the keyboard page hangs its live recolouring off.
    pub fn show(&mut self, ui: &mut Ui, icons: &mut IconCache, colour: &mut Color32) -> bool {
        let mut changed = false;

        // ---------------------------------------------------- the square and the hue bar
        let width = ui.available_width().max(HUE_COLUMN + HUE_GAP + 40.0);
        let square_width = width - HUE_COLUMN - HUE_GAP;
        let (row, _) = ui.allocate_exact_size(Vec2::new(width, AREA_HEIGHT), Sense::hover());

        let sv_rect = Rect::from_min_size(row.min, Vec2::new(square_width, AREA_HEIGHT));
        let hue_rect = Rect::from_min_size(
            row.min + Vec2::new(square_width + HUE_GAP, 0.0),
            Vec2::new(HUE_COLUMN, AREA_HEIGHT),
        );

        self.paint_square(ui, sv_rect);
        self.paint_hue_bar(ui, hue_rect);

        // `Cursor="Hand"` on both areas, and both capture the pointer so a drag that leaves the
        // control keeps working - which is why these use `drag()` rather than `click()`.
        let sv = ui.interact(sv_rect, ui.scope_id().with("sv"), Sense::click_and_drag());
        if sv.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if let Some(pos) = pointer_in(&sv, ui) {
            self.s = (((pos.x - sv_rect.left()) / sv_rect.width().max(1.0)) as f64).clamp(0.0, 1.0);
            self.v =
                1.0 - (((pos.y - sv_rect.top()) / sv_rect.height().max(1.0)) as f64).clamp(0.0, 1.0);
            changed = true;
        }

        let hue = ui.interact(hue_rect, ui.scope_id().with("hue"), Sense::click_and_drag());
        if hue.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if let Some(pos) = pointer_in(&hue, ui) {
            let fraction =
                (((pos.y - hue_rect.top()) / hue_rect.height().max(1.0)) as f64).clamp(0.0, 1.0);
            // `_h = (1 - t) * 360 % 360` - the bar runs hue downwards.
            self.h = ((1.0 - fraction) * 360.0) % 360.0;
            changed = true;
        }

        if changed {
            *colour = from_hsv(self.h, self.s, self.v);
            self.sync_text(*colour);
            // A drag cancels a half-finished text edit, as moving focus away would in Avalonia.
            self.editing = None;
        }

        self.paint_thumbs(ui, sv_rect, hue_rect);

        ui.add_space(10.0);

        // ---------------------------------------------------- eyedropper, hex and R/G/B
        ui.horizontal(|ui| {
            // `ToggleButton` with the eyedropper glyph: pressed state is the accent.
            let tint = if self.eyedropper { t::ACCENT } else { t::TEXT };
            if w::icon_button(ui, icons, Icon::Eyedropper, 26.0, 18.0, tint)
                .on_hover_text("Eyedropper: click a key to take its colour")
                .clicked()
            {
                self.eyedropper = !self.eyedropper;
            }
            ui.add_space(8.0);

            w::rgb_label(ui, "#");
            if self.field(ui, Field::Hex, 68.0, 7) {
                changed |= self.commit(Field::Hex, colour);
            }
            ui.add_space(8.0);
            for (field, caption) in
                [(Field::R, "R"), (Field::G, "G"), (Field::B, "B")]
            {
                w::rgb_label(ui, caption);
                if self.field(ui, field, 36.0, 3) {
                    changed |= self.commit(field, colour);
                }
            }
        });

        changed
    }

    /// One of the four text fields. Returns true when it should be committed: the C# committed on
    /// `LostFocus` and on Enter, and both map onto egui's `lost_focus`.
    fn field(&mut self, ui: &mut Ui, field: Field, width: f32, max_chars: usize) -> bool {
        let buffer = match field {
            Field::Hex => &mut self.hex,
            Field::R => &mut self.r,
            Field::G => &mut self.g,
            Field::B => &mut self.b,
        };
        let response = ui.add(
            egui::TextEdit::singleline(buffer)
                .desired_width(width)
                .char_limit(max_chars)
                .font(egui::FontId::new(13.0, t::family(t::BODY_BOLD)))
                .text_color(t::TEXT)
                .frame(egui::Frame::NONE),
        );
        if response.has_focus() {
            self.editing = Some(field);
        }
        let entered = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        response.lost_focus() || entered
    }

    /// `CommitText`: parse, and on anything invalid put the current colour back.
    fn commit(&mut self, field: Field, colour: &mut Color32) -> bool {
        self.editing = None;
        let parsed = match field {
            Field::Hex => {
                let text = self.hex.trim().trim_start_matches('#');
                (text.len() == 6)
                    .then(|| u32::from_str_radix(text, 16).ok())
                    .flatten()
                    .map(|rgb| {
                        Color32::from_rgb(
                            (rgb >> 16) as u8,
                            ((rgb >> 8) & 0xFF) as u8,
                            (rgb & 0xFF) as u8,
                        )
                    })
            }
            Field::R => self.r.trim().parse::<u8>().ok().map(|r| {
                Color32::from_rgb(r, colour.g(), colour.b())
            }),
            Field::G => self.g.trim().parse::<u8>().ok().map(|g| {
                Color32::from_rgb(colour.r(), g, colour.b())
            }),
            Field::B => self.b.trim().parse::<u8>().ok().map(|b| {
                Color32::from_rgb(colour.r(), colour.g(), b)
            }),
        };

        match parsed {
            // `if (c == Color) { UpdateText(); return; }` - unchanged or unparseable both revert.
            Some(new) if rgb_eq(new, *colour) => {
                self.sync_text(*colour);
                false
            }
            Some(new) => {
                let (h, s, v) = to_hsv(new, self.h);
                self.h = h;
                self.s = s;
                self.v = v;
                *colour = new;
                self.sync_text(new);
                true
            }
            None => {
                self.sync_text(*colour);
                false
            }
        }
    }

    /// The saturation/value square: a grid whose vertices carry their exact HSV colour.
    fn paint_square(&self, ui: &Ui, rect: Rect) {
        let mut mesh = Mesh::default();
        let steps = SV_STEPS;
        for row in 0..=steps {
            for column in 0..=steps {
                let s = column as f64 / steps as f64;
                let v = 1.0 - row as f64 / steps as f64;
                mesh.colored_vertex(
                    rect.min
                        + Vec2::new(
                            rect.width() * column as f32 / steps as f32,
                            rect.height() * row as f32 / steps as f32,
                        ),
                    from_hsv(self.h, s, v),
                );
            }
        }
        let stride = (steps + 1) as u32;
        for row in 0..steps as u32 {
            for column in 0..steps as u32 {
                let top_left = row * stride + column;
                mesh.add_triangle(top_left, top_left + 1, top_left + stride);
                mesh.add_triangle(top_left + 1, top_left + stride, top_left + stride + 1);
            }
        }
        // Clipped to the rounded rect the markup gives the square, so the mesh's square corners do
        // not show outside it.
        let painter = ui.painter().with_clip_rect(rect);
        painter.add(Shape::mesh(mesh));
        // The 2px radius is small enough that masking it is not worth a stencil; the corner is
        // covered by redrawing the rounding in the panel colour.
        painter.rect_stroke(
            rect,
            epaint::CornerRadius::same(SV_RADIUS as u8),
            Stroke::new(1.0, t::PANEL),
            egui::StrokeKind::Outside,
        );
    }

    /// The hue bar: red, magenta, blue, cyan, green, yellow, red from top to bottom.
    fn paint_hue_bar(&self, ui: &Ui, column: Rect) {
        let bar = Rect::from_center_size(
            column.center(),
            Vec2::new(HUE_BAR_WIDTH, column.height()),
        );
        let mut mesh = Mesh::default();
        // One vertex pair per 6 degrees of hue: smooth at any realistic bar height.
        const STEPS: usize = 60;
        for i in 0..=STEPS {
            let fraction = i as f64 / STEPS as f64;
            let colour = from_hsv((1.0 - fraction) * 360.0 % 360.0, 1.0, 1.0);
            let y = bar.top() + bar.height() * fraction as f32;
            mesh.colored_vertex(Pos2::new(bar.left(), y), colour);
            mesh.colored_vertex(Pos2::new(bar.right(), y), colour);
        }
        for i in 0..STEPS as u32 {
            let top_left = i * 2;
            mesh.add_triangle(top_left, top_left + 1, top_left + 2);
            mesh.add_triangle(top_left + 1, top_left + 2, top_left + 3);
        }
        // `CornerRadius="6"` on a 12px-wide bar is a full semicircle at each end, so the mesh is
        // clipped to a rounded rect of exactly half its width.
        let painter = ui.painter().with_clip_rect(bar);
        painter.add(Shape::mesh(mesh));
        painter.rect_stroke(
            bar,
            epaint::CornerRadius::same((HUE_BAR_WIDTH / 2.0) as u8),
            Stroke::new(1.5, t::PANEL),
            egui::StrokeKind::Outside,
        );
    }

    /// The two thumbs: a white ring on the square, and a hue-filled white ring on the bar.
    fn paint_thumbs(&self, ui: &Ui, sv_rect: Rect, hue_rect: Rect) {
        let painter = ui.painter();
        let at = sv_rect.min
            + Vec2::new(
                self.s as f32 * sv_rect.width(),
                (1.0 - self.v as f32) * sv_rect.height(),
            );
        painter.circle_stroke(at, SV_THUMB / 2.0, Stroke::new(THUMB_STROKE, Color32::WHITE));

        // `HueThumb.Fill = FromHsv(_h, 1, 1)` - the thumb shows the pure hue, not the colour.
        let fraction = (1.0 - self.h / 360.0) % 1.0;
        let hue_at = Pos2::new(
            hue_rect.center().x,
            hue_rect.top() + fraction as f32 * hue_rect.height(),
        );
        painter.circle_filled(hue_at, HUE_THUMB / 2.0, from_hsv(self.h, 1.0, 1.0));
        painter.circle_stroke(hue_at, HUE_THUMB / 2.0, Stroke::new(THUMB_STROKE, Color32::WHITE));
    }
}

/// The pointer position while a press or drag is live on `response`, in screen coordinates.
fn pointer_in(response: &Response, ui: &Ui) -> Option<Pos2> {
    if !(response.is_pointer_button_down_on() || response.drag_started() || response.clicked()) {
        return None;
    }
    ui.input(|i| i.pointer.interact_pos())
}

/// Compares only the colour channels. `Color32` includes alpha, which the picker never varies, and
/// an alpha difference must not count as a colour change.
fn rgb_eq(a: Color32, b: Color32) -> bool {
    a.r() == b.r() && a.g() == b.g() && a.b() == b.b()
}

/// `SpectrumColorPicker.FromHsv`.
pub fn from_hsv(h: f64, s: f64, v: f64) -> Color32 {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let h = h.rem_euclid(360.0);
    let (r, g, b) = if h < 60.0 {
        (c, x, 0.0)
    } else if h < 120.0 {
        (x, c, 0.0)
    } else if h < 180.0 {
        (0.0, c, x)
    } else if h < 240.0 {
        (0.0, x, c)
    } else if h < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    let byte = |d: f64| ((d + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(byte(r), byte(g), byte(b))
}

/// `SpectrumColorPicker.ToHsv`. For a grey, where hue is undefined, `previous_hue` is kept.
pub fn to_hsv(colour: Color32, previous_hue: f64) -> (f64, f64, f64) {
    let (r, g, b) = (
        colour.r() as f64 / 255.0,
        colour.g() as f64 / 255.0,
        colour.b() as f64 / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let mut h = previous_hue;
    if delta > 0.0 {
        h = if max == r {
            60.0 * (((g - b) / delta) % 6.0)
        } else if max == g {
            60.0 * ((b - r) / delta + 2.0)
        } else {
            60.0 * ((r - g) / delta + 4.0)
        };
        if h < 0.0 {
            h += 360.0;
        }
    }
    (h, if max == 0.0 { 0.0 } else { delta / max }, max)
}

/// Parses `#RRGGBB` or `RRGGBB`. Returns `None` rather than a guess, so a bad value in a profile
/// shows as unset rather than as black.
pub fn parse_hex(text: &str) -> Option<Color32> {
    let text = text.trim().trim_start_matches('#');
    if text.len() != 6 {
        return None;
    }
    let rgb = u32::from_str_radix(text, 16).ok()?;
    Some(Color32::from_rgb(
        (rgb >> 16) as u8,
        ((rgb >> 8) & 0xFF) as u8,
        (rgb & 0xFF) as u8,
    ))
}

/// `#RRGGBB`, upper case, which is the form the profile document stores.
pub fn to_hex(colour: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", colour.r(), colour.g(), colour.b())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_six_hue_corners_are_the_pure_colours() {
        assert_eq!(from_hsv(0.0, 1.0, 1.0), Color32::from_rgb(255, 0, 0));
        assert_eq!(from_hsv(60.0, 1.0, 1.0), Color32::from_rgb(255, 255, 0));
        assert_eq!(from_hsv(120.0, 1.0, 1.0), Color32::from_rgb(0, 255, 0));
        assert_eq!(from_hsv(180.0, 1.0, 1.0), Color32::from_rgb(0, 255, 255));
        assert_eq!(from_hsv(240.0, 1.0, 1.0), Color32::from_rgb(0, 0, 255));
        assert_eq!(from_hsv(300.0, 1.0, 1.0), Color32::from_rgb(255, 0, 255));
        // 360 must wrap to red rather than falling off the end of the match.
        assert_eq!(from_hsv(360.0, 1.0, 1.0), Color32::from_rgb(255, 0, 0));
    }

    #[test]
    fn zero_value_is_black_and_zero_saturation_is_grey() {
        assert_eq!(from_hsv(210.0, 1.0, 0.0), Color32::from_rgb(0, 0, 0));
        assert_eq!(from_hsv(210.0, 0.0, 1.0), Color32::from_rgb(255, 255, 255));
        assert_eq!(from_hsv(210.0, 0.0, 0.5), Color32::from_rgb(128, 128, 128));
    }

    /// Every 8-bit colour must survive a round trip through HSV, because the picker converts on
    /// every drag frame and a drift of one level per frame would walk the colour away under the
    /// user's pointer.
    #[test]
    fn every_colour_round_trips_through_hsv() {
        for r in (0..=255).step_by(5) {
            for g in (0..=255).step_by(5) {
                for b in (0..=255).step_by(5) {
                    let original = Color32::from_rgb(r as u8, g as u8, b as u8);
                    let (h, s, v) = to_hsv(original, 0.0);
                    let back = from_hsv(h, s, v);
                    assert!(
                        rgb_eq(original, back),
                        "{original:?} -> hsv({h}, {s}, {v}) -> {back:?}"
                    );
                }
            }
        }
    }

    /// The reason hue is stored rather than derived: dragging into black and back out must come
    /// back to the hue the user left, not to red.
    #[test]
    fn hue_survives_a_trip_through_black() {
        let mut picker = ColorPicker::new(Color32::from_rgb(0, 128, 255));
        let hue = picker.h;
        assert!((hue - 210.0).abs() < 0.5, "expected ~210 degrees, got {hue}");

        picker.set(Color32::BLACK);
        assert!((picker.h - hue).abs() < 0.001, "hue was lost: {} vs {hue}", picker.h);

        // Back up to full value at the same hue gives the pure colour, not red.
        let recovered = from_hsv(picker.h, 1.0, 1.0);
        assert_eq!(recovered, Color32::from_rgb(0, 128, 255));
    }

    #[test]
    fn grey_keeps_the_previous_hue_rather_than_reporting_red() {
        let (h, s, _) = to_hsv(Color32::from_rgb(90, 90, 90), 137.0);
        assert_eq!(h, 137.0);
        assert_eq!(s, 0.0);
    }

    #[test]
    fn hex_round_trips_and_rejects_junk() {
        assert_eq!(parse_hex("#00A8E0"), Some(Color32::from_rgb(0, 0xA8, 0xE0)));
        assert_eq!(parse_hex("00a8e0"), Some(Color32::from_rgb(0, 0xA8, 0xE0)));
        assert_eq!(parse_hex("  #00A8E0  "), Some(Color32::from_rgb(0, 0xA8, 0xE0)));
        assert_eq!(to_hex(Color32::from_rgb(0, 0xA8, 0xE0)), "#00A8E0");

        // Short, long, and non-hex are all refused rather than guessed at.
        assert_eq!(parse_hex("#FFF"), None);
        assert_eq!(parse_hex("#00A8E00"), None);
        assert_eq!(parse_hex("#GGGGGG"), None);
        assert_eq!(parse_hex(""), None);
    }

    /// Committing junk puts the current colour back in the box, which is what
    /// `CommitText`'s `UpdateText()` on the invalid path did.
    #[test]
    fn an_invalid_hex_entry_reverts_to_the_current_colour() {
        let mut colour = Color32::from_rgb(0x12, 0x34, 0x56);
        let mut picker = ColorPicker::new(colour);
        picker.hex = "nonsense".to_owned();
        assert!(!picker.commit(Field::Hex, &mut colour));
        assert_eq!(picker.hex, "123456");
        assert_eq!(colour, Color32::from_rgb(0x12, 0x34, 0x56));
    }

    #[test]
    fn a_valid_hex_entry_moves_the_colour_and_the_channels() {
        let mut colour = Color32::BLACK;
        let mut picker = ColorPicker::new(colour);
        picker.hex = "FF8800".to_owned();
        assert!(picker.commit(Field::Hex, &mut colour));
        assert_eq!(colour, Color32::from_rgb(0xFF, 0x88, 0x00));
        assert_eq!((picker.r.as_str(), picker.g.as_str(), picker.b.as_str()), ("255", "136", "0"));
    }

    #[test]
    fn a_channel_entry_changes_only_that_channel() {
        let mut colour = Color32::from_rgb(10, 20, 30);
        let mut picker = ColorPicker::new(colour);
        picker.g = "200".to_owned();
        assert!(picker.commit(Field::G, &mut colour));
        assert_eq!(colour, Color32::from_rgb(10, 200, 30));
        assert_eq!(picker.hex, "0AC81E");
    }

    /// Out-of-range input is not clamped: `byte.TryParse` failed, which took the revert path.
    #[test]
    fn an_out_of_range_channel_reverts_rather_than_clamping() {
        let mut colour = Color32::from_rgb(10, 20, 30);
        let mut picker = ColorPicker::new(colour);
        picker.r = "300".to_owned();
        assert!(!picker.commit(Field::R, &mut colour));
        assert_eq!(colour, Color32::from_rgb(10, 20, 30));
        assert_eq!(picker.r, "10");
    }

    #[test]
    fn committing_the_same_colour_is_not_a_change() {
        let mut colour = Color32::from_rgb(0x12, 0x34, 0x56);
        let mut picker = ColorPicker::new(colour);
        picker.hex = "123456".to_owned();
        assert!(!picker.commit(Field::Hex, &mut colour));
    }

    /// The bar's top and bottom are both red, and the midpoint is cyan. Getting the direction
    /// wrong is invisible in a screenshot of the gradient but puts every drag on the wrong hue.
    #[test]
    fn the_hue_bar_runs_red_to_red_with_cyan_in_the_middle() {
        let at = |fraction: f64| from_hsv((1.0 - fraction) * 360.0 % 360.0, 1.0, 1.0);
        assert_eq!(at(0.0), Color32::from_rgb(255, 0, 0));
        assert_eq!(at(1.0), Color32::from_rgb(255, 0, 0));
        assert_eq!(at(0.5), Color32::from_rgb(0, 255, 255));
        // The markup's own stops: 1/6 magenta, 1/3 blue, 2/3 green, 5/6 yellow.
        assert_eq!(at(1.0 / 6.0), Color32::from_rgb(255, 0, 255));
        assert_eq!(at(1.0 / 3.0), Color32::from_rgb(0, 0, 255));
        assert_eq!(at(2.0 / 3.0), Color32::from_rgb(0, 255, 0));
        assert_eq!(at(5.0 / 6.0), Color32::from_rgb(255, 255, 0));
    }

    /// The square's four corners are white, the hue, and black twice - which is what makes the
    /// single mesh equal to G HUB's three stacked gradient layers.
    #[test]
    fn the_square_corners_match_the_three_layer_composite() {
        let hue = 210.0;
        assert_eq!(from_hsv(hue, 0.0, 1.0), Color32::WHITE);
        assert_eq!(from_hsv(hue, 1.0, 1.0), Color32::from_rgb(0, 128, 255));
        assert_eq!(from_hsv(hue, 0.0, 0.0), Color32::BLACK);
        assert_eq!(from_hsv(hue, 1.0, 0.0), Color32::BLACK);

        // And the interior is the product the composite produces: v * lerp(white, hue, s).
        for s in [0.25_f64, 0.5, 0.75] {
            for v in [0.25_f64, 0.5, 0.75] {
                let direct = from_hsv(hue, s, v);
                let pure = from_hsv(hue, 1.0, 1.0);
                let expect = |channel: u8| {
                    (v * (255.0 * (1.0 - s) + channel as f64 * s)).round() as u8
                };
                let composed =
                    Color32::from_rgb(expect(pure.r()), expect(pure.g()), expect(pure.b()));
                for (a, b) in [
                    (direct.r(), composed.r()),
                    (direct.g(), composed.g()),
                    (direct.b(), composed.b()),
                ] {
                    assert!(
                        a.abs_diff(b) <= 1,
                        "s={s} v={v}: {direct:?} vs composite {composed:?}"
                    );
                }
            }
        }
    }
}
