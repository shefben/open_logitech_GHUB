//! Buttons, checkboxes, sliders, numeric fields, dropdowns and switches, drawn to G HUB's own
//! metrics.
//!
//! The Avalonia build used stock Fluent controls for all of these - `Button`, `CheckBox`,
//! `ComboBox`, `Slider`, `NumericUpDown`, `TextBox`, `ToggleSwitch` - with only `ComboBox` given a
//! `gh-combo` restyle. That was the one place the port visibly diverged from G HUB, because
//! restyling the rest would have meant writing a second stylesheet by eye.
//!
//! The metrics are no longer guesswork: they come from G HUB's own `app.min.css`. A `.dls-button`
//! is 48px tall with BrownPro-Bold at 14px and 0.5px letter spacing, over `11px` of padding and a
//! `4rem` minimum width; `--dls-border-radius` is `0.5rem`; a contained-primary button is a pill
//! (`border-radius: 56px`) in the primary colour with **black** text, and an outlined one is 40px
//! tall with a 2px `#666` border. State is a `:before` overlay whose opacity is 0 at rest, which
//! is how one fill colour serves rest, hover and press.

use egui::{Color32, CornerRadius, FontId, Rect, Response, Sense, Stroke, StrokeKind, TextFormat, Ui, Vec2};

use crate::icons::{Icon, IconCache};
use crate::theme as t;
use crate::widgets::{row_inner, text};

/// `.dls-button` - `height: 3rem`.
const BUTTON_HEIGHT: f32 = 48.0;
/// `.dls-button-container` - `min-width: 4rem`.
const BUTTON_MIN_WIDTH: f32 = 64.0;
/// `--dls-button-padding: 11px`.
const BUTTON_PAD_X: f32 = 11.0;
/// `.dls-button__contained-primary { border-radius: 56px }` - a pill at any height.
const BUTTON_RADIUS_PILL: u8 = 56;
/// `.dls-button__outlined-primary` - `height: 40px; border: 2px solid #666`.
const OUTLINED_HEIGHT: f32 = 40.0;
const OUTLINED_BORDER: Color32 = Color32::from_rgb(0x66, 0x66, 0x66);
/// `.dls-button:before` opacity while hovered and while pressed.
const OVERLAY_HOVER: f32 = 0.1;
const OVERLAY_PRESSED: f32 = 0.2;
/// `--dls-color-primary-disabled: #1196ff66` - the primary at 40% alpha.
const DISABLED_ALPHA: f32 = 0.4;

/// `.dls-button` text: BrownPro-Bold 14, 0.5px letter spacing.
fn button_text() -> TextFormat {
    TextFormat {
        font_id: FontId::new(14.0, t::family(t::BODY_BOLD)),
        color: t::TEXT,
        extra_letter_spacing: 0.5,
        ..Default::default()
    }
}

/// How prominent a button is, matching G HUB's three `.dls-button__*` families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    /// `.dls-button__contained-primary` - an accent pill with black text. A page's main action.
    Primary,
    /// `.dls-button__outlined-primary` - transparent behind a 2px grey border.
    Outlined,
    /// `.dls-button__text-primary` - no fill and no border.
    Text,
}

impl ButtonStyle {
    fn height(self) -> f32 {
        match self {
            ButtonStyle::Primary => BUTTON_HEIGHT,
            ButtonStyle::Outlined | ButtonStyle::Text => OUTLINED_HEIGHT,
        }
    }
}

/// A G HUB button, sized to its label.
///
/// `enabled` false draws the disabled appearance and reports no clicks, which is what every page's
/// `IsEnabled="{CompiledBinding !Busy}"` did.
pub fn button(ui: &mut Ui, label: &str, style: ButtonStyle, enabled: bool) -> Response {
    let galley = text(ui, label, button_text());
    let width = (galley.size().x + BUTTON_PAD_X * 2.0).max(BUTTON_MIN_WIDTH);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, style.height()), sense);
    paint_button(ui, rect, galley, style, enabled, &response);
    response
}

/// The same button stretched across the width available, which is what
/// `HorizontalAlignment="Stretch"` gave the Save buttons.
pub fn button_wide(ui: &mut Ui, label: &str, style: ButtonStyle, enabled: bool) -> Response {
    let galley = text(ui, label, button_text());
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), style.height()), sense);
    paint_button(ui, rect, galley, style, enabled, &response);
    response
}

fn paint_button(
    ui: &Ui,
    rect: Rect,
    galley: std::sync::Arc<egui::Galley>,
    style: ButtonStyle,
    enabled: bool,
    response: &Response,
) {
    let overlay = if !enabled {
        0.0
    } else if response.is_pointer_button_down_on() {
        OVERLAY_PRESSED
    } else if response.hovered() {
        OVERLAY_HOVER
    } else {
        0.0
    };

    let radius = CornerRadius::same(BUTTON_RADIUS_PILL);
    let painter = ui.painter();
    match style {
        ButtonStyle::Primary => {
            let fill = if enabled { t::ACCENT } else { t::ACCENT.gamma_multiply(DISABLED_ALPHA) };
            painter.rect_filled(rect, radius, fill);
            if overlay > 0.0 {
                // On a light fill the state overlay is the mono (black) one.
                painter.rect_filled(rect, radius, Color32::BLACK.gamma_multiply(overlay));
            }
        }
        ButtonStyle::Outlined => {
            let border =
                if enabled { OUTLINED_BORDER } else { OUTLINED_BORDER.gamma_multiply(DISABLED_ALPHA) };
            painter.rect_stroke(rect, radius, Stroke::new(2.0, border), StrokeKind::Inside);
            if overlay > 0.0 {
                painter.rect_filled(rect, radius, Color32::WHITE.gamma_multiply(overlay));
            }
        }
        ButtonStyle::Text => {
            if overlay > 0.0 {
                painter.rect_filled(
                    rect,
                    CornerRadius::same(t::RADIUS_PILL),
                    Color32::WHITE.gamma_multiply(overlay),
                );
            }
        }
    }

    // `.dls-button__contained-primary { color: #000 }` - black on the accent, white elsewhere.
    let colour = match (style, enabled) {
        (ButtonStyle::Primary, true) => Color32::BLACK,
        (ButtonStyle::Primary, false) => Color32::from_black_alpha(0x66),
        (_, true) => t::TEXT,
        (_, false) => t::DISABLED,
    };
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, colour);
}

/// `.checkbox-label` - the box glyph followed by its caption, with the whole row as the hit
/// target, because G HUB makes the caption a `<label>` bound to the input.
///
/// Returns true on the frame it was toggled.
pub fn checkbox(
    ui: &mut Ui,
    icons: &mut IconCache,
    checked: &mut bool,
    label: &str,
    enabled: bool,
) -> bool {
    /// The glyph is authored at 14x14.
    const BOX: f32 = 14.0;
    const GAP: f32 = 8.0;

    let galley = text(ui, label, TextFormat { color: t::TEXT_ROW, ..t::row() });
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(BOX + GAP + galley.size().x, galley.size().y.max(BOX)), sense);

    let glyph = Rect::from_center_size(
        egui::pos2(rect.left() + BOX / 2.0, rect.center().y),
        Vec2::splat(BOX),
    );
    let icon = if *checked { Icon::CheckboxChecked } else { Icon::Checkbox };
    icons.draw(ui, icon, glyph, BOX, if enabled { t::TEXT_LABEL } else { t::DISABLED });

    let colour = if !enabled {
        t::DISABLED
    } else if response.hovered() {
        t::TEXT
    } else {
        t::TEXT_ROW
    };
    let size = galley.size();
    ui.painter().galley(
        egui::pos2(rect.left() + BOX + GAP, rect.center().y - size.y / 2.0),
        galley,
        colour,
    );

    if response.clicked() {
        *checked = !*checked;
        return true;
    }
    false
}

/// `.dls-switch` - a track that takes the primary colour when checked.
pub fn toggle_switch(ui: &mut Ui, on: &mut bool, enabled: bool) -> bool {
    const WIDTH: f32 = 40.0;
    const HEIGHT: f32 = 20.0;

    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(WIDTH, HEIGHT), sense);

    let track = match (enabled, *on) {
        (true, true) => t::ACCENT,
        (true, false) => t::SLIDER_TRACK,
        (false, _) => t::SLIDER_TRACK.gamma_multiply(DISABLED_ALPHA),
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same((HEIGHT / 2.0) as u8), track);

    let travel = WIDTH - HEIGHT;
    let centre = egui::pos2(
        rect.left() + HEIGHT / 2.0 + if *on { travel } else { 0.0 },
        rect.center().y,
    );
    painter.circle_filled(
        centre,
        HEIGHT / 2.0 - 3.0,
        if enabled { Color32::WHITE } else { t::DISABLED },
    );

    if response.clicked() {
        *on = !*on;
        return true;
    }
    false
}

/// A switch with a caption either side, which is how the Assignments page shows
/// `DEFAULT [switch] G-SHIFT` and the device page shows `ON-BOARD MEMORY MODE`.
pub fn toggle_switch_labelled(
    ui: &mut Ui,
    on: &mut bool,
    off_label: &str,
    on_label: &str,
    enabled: bool,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        crate::widgets::label(ui, off_label);
        ui.add_space(10.0);
        changed = toggle_switch(ui, on, enabled);
        ui.add_space(10.0);
        crate::widgets::label(ui, on_label);
    });
    changed
}

/// `.dls-slider` - a 4px track with the filled portion in the accent and a round thumb.
///
/// Returns true on any frame the value moved, which is what drives the keyboard page's debounced
/// save: a drag reports many times and the debounce collapses them into one profile write.
pub fn slider(
    ui: &mut Ui,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    enabled: bool,
) -> bool {
    const HEIGHT: f32 = 20.0;
    const TRACK: f32 = 4.0;
    const THUMB: f32 = 7.0;

    let sense = if enabled { Sense::click_and_drag() } else { Sense::hover() };
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), HEIGHT), sense);

    let (low, high) = (*range.start(), *range.end());
    let span = (high - low).max(f64::EPSILON);
    let mut changed = false;
    if enabled && (response.is_pointer_button_down_on() || response.dragged()) {
        if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
            // The thumb has a radius, so the usable track is inset by it at both ends - without
            // that, the last few pixels of travel are unreachable.
            let usable = (rect.width() - THUMB * 2.0).max(1.0);
            let fraction = ((pos.x - rect.left() - THUMB) / usable).clamp(0.0, 1.0) as f64;
            let next = low + fraction * span;
            if (next - *value).abs() > f64::EPSILON {
                *value = next;
                changed = true;
            }
        }
    }

    let fraction = ((*value - low) / span).clamp(0.0, 1.0) as f32;
    let track = Rect::from_center_size(rect.center(), Vec2::new(rect.width() - THUMB * 2.0, TRACK))
        .translate(Vec2::ZERO);
    let painter = ui.painter();
    let track_colour =
        if enabled { t::SLIDER_TRACK } else { t::SLIDER_TRACK.gamma_multiply(DISABLED_ALPHA) };
    painter.rect_filled(track, CornerRadius::same((TRACK / 2.0) as u8), track_colour);

    let filled = Rect::from_min_size(track.min, Vec2::new(track.width() * fraction, TRACK));
    let accent = if enabled { t::ACCENT } else { t::ACCENT.gamma_multiply(DISABLED_ALPHA) };
    painter.rect_filled(filled, CornerRadius::same((TRACK / 2.0) as u8), accent);

    let thumb = egui::pos2(track.left() + track.width() * fraction, rect.center().y);
    painter.circle_filled(thumb, THUMB, if enabled { Color32::WHITE } else { t::DISABLED });
    changed
}

/// `NumericUpDown`: a typed value with `.stepper-control` arrows beside it.
///
/// `buffer` is the text being edited and is the caller's state, because a numeric field cannot be
/// driven from its value alone: rewriting the text from the number on every frame makes anything
/// longer than one digit impossible to type. `step` is one arrow press. Returns true when the
/// committed value changed.
pub fn numeric(
    ui: &mut Ui,
    icons: &mut IconCache,
    buffer: &mut String,
    value: &mut i64,
    range: std::ops::RangeInclusive<i64>,
    step: i64,
    enabled: bool,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let response = ui.add_enabled(
            enabled,
            egui::TextEdit::singleline(buffer)
                .desired_width(64.0)
                .font(FontId::new(13.0, t::family(t::BODY_BOLD)))
                .text_color(t::TEXT)
                .frame(egui::Frame::NONE),
        );
        if response.lost_focus() {
            // Unparseable or out of range reverts, which is what `NumericUpDown` did when its
            // text failed to bind. Clamping instead would silently accept 999999 as the maximum.
            match buffer.trim().parse::<i64>() {
                Ok(parsed) if range.contains(&parsed) => {
                    if parsed != *value {
                        *value = parsed;
                        changed = true;
                    }
                }
                _ => *buffer = value.to_string(),
            }
        }

        ui.vertical(|ui| {
            let tint = if enabled { t::TEXT_LABEL } else { t::DISABLED };
            for (icon, delta) in [(Icon::StepperUp, step), (Icon::StepperDown, -step)] {
                let (rect, response) = ui.allocate_exact_size(
                    Vec2::new(24.0, t::STEPPER_GLYPH_HEIGHT + 4.0),
                    if enabled { Sense::click() } else { Sense::hover() },
                );
                // `.stepper-control:hover { opacity: .8 }`.
                let opacity = if response.hovered() { 0.8 } else { 1.0 };
                icons.draw(ui, icon, rect, t::STEPPER_GLYPH_WIDTH, tint.gamma_multiply(opacity));
                if response.clicked() {
                    let next = (*value + delta).clamp(*range.start(), *range.end());
                    if next != *value {
                        *value = next;
                        *buffer = next.to_string();
                        changed = true;
                    }
                }
            }
        });
    });
    changed
}

/// `ComboBox.gh-combo` - transparent, borderless, BrownPro-Bold 13, with the disclosure chevron
/// on the right.
///
/// Generic over the item type so each page can use its own, with `label` rendering one.
pub fn combo<T: PartialEq + Clone>(
    ui: &mut Ui,
    icons: &mut IconCache,
    id: impl std::hash::Hash + std::fmt::Debug,
    selected: &mut T,
    items: &[T],
    label: impl Fn(&T) -> String,
    enabled: bool,
) -> bool {
    const CHEVRON: f32 = 18.0;

    let mut changed = false;
    let galley = text(ui, &label(selected), TextFormat { color: t::TEXT, ..t::row() });
    let height = galley.size().y.max(CHEVRON) + 8.0;
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), sense);

    let colour = if !enabled {
        t::DISABLED
    } else if response.hovered() {
        t::TEXT
    } else {
        t::TEXT_ROW
    };
    let size = galley.size();
    ui.painter()
        .galley(egui::pos2(rect.left(), rect.center().y - size.y / 2.0), galley, colour);
    let chevron = Rect::from_center_size(
        egui::pos2(rect.right() - CHEVRON / 2.0, rect.center().y),
        Vec2::splat(CHEVRON),
    );
    icons.draw(ui, Icon::Chevron, chevron, CHEVRON, colour);

    let popup = ui.scope_id().with("ghub combo").with(id);
    if response.clicked() {
        let open = ui.memory(|m| m.data.get_temp::<bool>(popup).unwrap_or(false));
        ui.memory_mut(|m| m.data.insert_temp(popup, !open));
    }

    if ui.memory(|m| m.data.get_temp::<bool>(popup).unwrap_or(false)) {
        let shown = egui::Area::new(popup)
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(rect.left(), rect.bottom() + 2.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(t::PANEL)
                    .stroke(Stroke::new(1.0, t::PANEL_BORDER))
                    .corner_radius(CornerRadius::same(t::RADIUS_PILL))
                    .inner_margin(egui::Margin::same(4))
                    .show(ui, |ui| {
                        ui.set_min_width(rect.width());
                        let mut picked = None;
                        for item in items {
                            if row_inner(
                                ui,
                                &label(item),
                                t::ANIM_ROW_PAD,
                                Color32::TRANSPARENT,
                                item == selected,
                            )
                            .clicked()
                            {
                                picked = Some(item.clone());
                            }
                        }
                        picked
                    })
                    .inner
            });

        if let Some(picked) = shown.inner {
            if picked != *selected {
                *selected = picked;
                changed = true;
            }
            ui.memory_mut(|m| m.data.insert_temp(popup, false));
        } else if !response.clicked() && shown.response.clicked_elsewhere() {
            // The click that opened the list lands outside it, so it must not close it again
            // on the same frame.
            ui.memory_mut(|m| m.data.insert_temp(popup, false));
        }
    }
    changed
}

/// A single-line text field in the G HUB body style.
///
/// Returns true when editing finished *and* the value changed, not on every keystroke: the pages
/// persist on commit, and saving per keystroke would be a profile write per character.
pub fn text_field(ui: &mut Ui, buffer: &mut String, width: f32, enabled: bool) -> bool {
    let before = buffer.clone();
    let response = ui.add_enabled(
        enabled,
        egui::TextEdit::singleline(buffer)
            .desired_width(width)
            .font(FontId::new(13.0, t::family(t::BODY)))
            .text_color(t::TEXT)
            .frame(egui::Frame::NONE),
    );
    response.lost_focus() && *buffer != before
}

/// A multi-line field, for the macro editor and the automation page.
pub fn text_area(ui: &mut Ui, buffer: &mut String, rows: usize, enabled: bool) -> bool {
    let before = buffer.clone();
    let response = ui.add_enabled(
        enabled,
        egui::TextEdit::multiline(buffer)
            .desired_rows(rows)
            .desired_width(ui.available_width())
            .font(FontId::new(13.0, t::family(t::MONO)))
            .text_color(t::TEXT)
            .frame(egui::Frame::NONE),
    );
    response.lost_focus() && *buffer != before
}

/// `<Border Height="1" Background="{DynamicResource GHubPanelBorder}" Margin="0 4" />` - the rule
/// the panels divide their sections with.
pub fn separator(ui: &mut Ui) {
    ui.add_space(4.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, t::PANEL_BORDER);
    ui.add_space(4.0);
}

/// A caption/control row: a label on the left and the control pushed to the right, which is the
/// `ColumnDefinitions="*,Auto"` grid the panels use over and over.
pub fn field_row<R>(ui: &mut Ui, caption: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    let mut result = None;
    ui.horizontal(|ui| {
        crate::widgets::body(ui, caption);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            result = Some(add(ui));
        });
    });
    result.expect("the right-to-left layout always runs its body")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pill radius has to exceed half the tallest button, or a `Primary` button comes out as a
    /// rounded rectangle rather than the pill G HUB draws.
    #[test]
    fn the_pill_radius_fully_rounds_the_tallest_button() {
        assert!(f32::from(BUTTON_RADIUS_PILL) >= BUTTON_HEIGHT / 2.0);
        assert!(f32::from(BUTTON_RADIUS_PILL) >= OUTLINED_HEIGHT / 2.0);
    }

    /// Reproduces G HUB's own numbers, so a future tidy-up cannot quietly change the look.
    #[test]
    fn the_button_metrics_are_the_ones_g_hub_publishes() {
        // `height: 3rem`, `min-width: 4rem` at the 16px root font size.
        assert_eq!(BUTTON_HEIGHT, 48.0);
        assert_eq!(BUTTON_MIN_WIDTH, 64.0);
        assert_eq!(BUTTON_PAD_X, 11.0);
        assert_eq!(OUTLINED_HEIGHT, 40.0);
        assert_eq!(ButtonStyle::Primary.height(), 48.0);
        assert_eq!(ButtonStyle::Outlined.height(), 40.0);
        assert_eq!(ButtonStyle::Text.height(), 40.0);
    }

    /// A compile-time check: a pressed button must read as further from rest than a hovered one,
    /// or the press gives no feedback. `const` because both are constants, which is also what
    /// keeps clippy from flagging the assertion as trivially true.
    const _OVERLAY_ORDER: () = {
        assert!(OVERLAY_PRESSED > OVERLAY_HOVER);
        assert!(OVERLAY_HOVER > 0.0);
    };
}
