//! The G HUB control vocabulary as egui widgets, one per style selector in `GHubStyles.axaml`.
//!
//! These are painted rather than themed. G HUB's controls are flat fills with state-dependent
//! colour and no chrome, which is more work to coax out of a styled widget than to draw directly,
//! and drawing keeps every metric in one place next to the selector it came from.

// `callout`, `field`, `status` and `value_right` are ported G HUB styles with no current caller:
// the pages that show that shape build it inline because they need the rect back. Kept for the
// same reason as the theme tokens - the vocabulary is worth having whole - rather than deleted
// and re-derived by the next page that needs one.
#![allow(dead_code)]

use egui::{
    Align, Color32, CornerRadius, FontId, Rect, Response, Sense, Stroke, StrokeKind, TextFormat, Ui,
    Vec2, text::LayoutJob,
};

use crate::icons::{Icon, IconCache};
use crate::theme as t;

// ------------------------------------------------------------------ text

/// Lays out a single run in one of the `gh-*` text styles.
pub fn text(ui: &Ui, s: &str, format: TextFormat) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::default();
    job.append(s, 0.0, format);
    ui.painter().layout_job(job)
}

/// Same, wrapped to `width` - `TextBlock.gh-body` sets `TextWrapping="Wrap"`.
pub fn wrapped(ui: &Ui, s: &str, format: TextFormat, width: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::default();
    job.append(s, 0.0, format);
    job.wrap.max_width = width;
    ui.painter().layout_job(job)
}

fn put(ui: &mut Ui, galley: std::sync::Arc<egui::Galley>) -> Response {
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::hover());
    ui.painter().galley(rect.min, galley, t::TEXT);
    response
}

/// `TextBlock.gh-wordmark` - the "G HUB" wordmark, 32 display bold.
pub fn wordmark(ui: &mut Ui, s: &str) -> Response {
    let g = text(ui, s, t::wordmark());
    put(ui, g)
}

/// `TextBlock.gh-title` - a page title, 22 display with 0.5 letter-spacing.
pub fn title(ui: &mut Ui, s: &str) -> Response {
    let g = text(ui, s, t::title());
    put(ui, g)
}

/// `TextBlock.gh-label` - a section caption. G HUB sets these in caps in the markup rather than
/// via a text transform, so the caller passes the cased string.
pub fn label(ui: &mut Ui, s: &str) -> Response {
    let g = text(ui, s, t::label());
    put(ui, g)
}

/// `TextBlock.gh-body` - wrapping body copy on a 19px line.
pub fn body(ui: &mut Ui, s: &str) -> Response {
    let w = ui.available_width();
    let g = wrapped(ui, s, t::body(), w);
    put(ui, g)
}

// ------------------------------------------------------------------ shell

/// `Border.gh-panel` - the fixed 307px settings panel: flat `GHubPanel` fill, 8px radius,
/// 20x18 padding, 8px outer margin, left-aligned.
pub fn panel<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(t::PANEL)
        .corner_radius(CornerRadius::same(t::RADIUS_PANEL))
        .inner_margin(t::PANEL_PAD)
        .outer_margin(egui::Margin::same(t::PANEL_MARGIN as i8))
        .show(ui, |ui| {
            ui.set_width(t::PANEL_WIDTH - t::PANEL_PAD.sum().x);
            add(ui)
        })
        .inner
}

/// `Border.gh-stage` - the black device stage the render and key grid sit on.
pub fn stage<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new().fill(t::BG).inner_margin(t::STAGE_PAD).show(ui, add).inner
}

/// `Border.gh-pill` - the profile selector / onboard-memory pill.
pub fn pill<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(t::PANEL)
        .corner_radius(CornerRadius::same(t::RADIUS_PILL))
        .inner_margin(t::PILL_PAD)
        .show(ui, |ui| {
            ui.set_min_width(t::PILL_MIN_WIDTH - t::PILL_PAD.sum().x);
            add(ui)
        })
        .inner
}

/// `Border.gh-callout` - a button callout on the mouse render. `active` switches the border to
/// `GHubAccentBright`, the way the selected button's chip is called out in G HUB.
pub fn callout<R>(ui: &mut Ui, active: bool, add: impl FnOnce(&mut Ui) -> R) -> R {
    let border = if active { t::ACCENT_BRIGHT } else { t::PANEL_BORDER };
    egui::Frame::new()
        .fill(t::PANEL)
        .stroke(Stroke::new(1.0, border))
        .corner_radius(CornerRadius::same(t::RADIUS_PILL))
        .inner_margin(t::CALLOUT_PAD)
        .show(ui, |ui| {
            ui.set_min_width(t::CALLOUT_MIN_WIDTH - t::CALLOUT_PAD.sum().x);
            add(ui)
        })
        .inner
}

// ------------------------------------------------------------------ buttons

/// `.menu-icon-wrapper` - a 48x48 icon button on the device screen's rail.
///
/// G HUB expresses every state as an opacity on the whole wrapper, and gives the active one the
/// accent as a background fill; it never changes the icon's colour or swaps the asset. Reproduced
/// literally, including the fact that hovering *dims* an inactive icon.
pub fn rail(
    ui: &mut Ui,
    icons: &mut IconCache,
    icon: Icon,
    active: bool,
    enabled: bool,
) -> Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(t::RAIL_SIZE), sense);

    let opacity = if !enabled {
        t::RAIL_OPACITY_DISABLED
    } else if active {
        if response.is_pointer_button_down_on() {
            t::RAIL_OPACITY_ACTIVE_PRESSED
        } else if response.hovered() {
            t::RAIL_OPACITY_ACTIVE_HOVER
        } else {
            t::RAIL_OPACITY_ACTIVE
        }
    } else if response.is_pointer_button_down_on() {
        t::RAIL_OPACITY_PRESSED
    } else if response.hovered() {
        t::RAIL_OPACITY_HOVER
    } else {
        t::RAIL_OPACITY_REST
    };

    if active {
        // The background takes the same opacity as the icon, because in G HUB the opacity is on
        // the wrapper and the background is part of it.
        let fill = t::ACCENT.gamma_multiply(opacity);
        ui.painter().rect_filled(rect, CornerRadius::same(t::RADIUS_RAIL), fill);
    }
    icons.draw(ui, icon, rect, RAIL_GLYPH, t::TEXT.gamma_multiply(opacity));

    ui.add_space(t::RAIL_GAP);
    response
}

/// The glyph inside a rail button. G HUB authors these at 24px and centres them in the 48px
/// wrapper via `background-position: 50%`.
const RAIL_GLYPH: f32 = 24.0;

/// A bare icon button: a hit area with a glyph centred in it, no background in any state.
///
/// This is `.dropdown-item.dropdown-button` and the various `.panel-options-button` variants,
/// which G HUB sizes as a 25px box around a 20px glyph and distinguishes only by opacity.
pub fn icon_button(
    ui: &mut Ui,
    icons: &mut IconCache,
    icon: Icon,
    size: f32,
    glyph: f32,
    colour: Color32,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    // `.stepper-control:hover { opacity: .8 }` is the pattern G HUB uses for all of these.
    let opacity = if response.is_pointer_button_down_on() {
        0.6
    } else if response.hovered() {
        0.8
    } else {
        1.0
    };
    icons.draw(ui, icon, rect, glyph, colour.gamma_multiply(opacity));
    response
}

/// `.info-tooltip` - the 15px help bubble that sits after a label. Hovering it shows `hint`.
pub fn help(ui: &mut Ui, icons: &mut IconCache, hint: &str) -> Response {
    let response = icon_button(
        ui,
        icons,
        Icon::InfoTooltip,
        t::TOOLTIP_ICON,
        t::TOOLTIP_ICON,
        t::TEXT_LABEL,
    );
    response.on_hover_text(hint)
}

/// `Button.gh-tab` - a LIGHTSYNC sub-tab: bold 12 caption over a 2px underline that is only drawn
/// on the active tab, with an 18px gap to the next one.
pub fn subtab(ui: &mut Ui, s: &str, active: bool) -> Response {
    let colour = if active { t::TEXT } else { t::TEXT_LABEL };
    let g = text(ui, s, TextFormat { color: colour, ..t::tab() });
    let pad = t::TAB_PAD;
    let size = g.size() + Vec2::new(0.0, (pad.top + pad.bottom) as f32);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    let colour = if active || response.hovered() { t::TEXT } else { t::TEXT_LABEL };
    ui.painter().galley(rect.min + Vec2::new(0.0, pad.top as f32), g, colour);

    if active {
        let y = rect.bottom() - t::TAB_UNDERLINE / 2.0;
        ui.painter().hline(rect.x_range(), y, Stroke::new(t::TAB_UNDERLINE, t::TEXT));
    }
    ui.add_space(t::TAB_GAP);
    response
}

/// `Button.gh-tool` - a 40x40 square select/brush tool. `#050505` at rest, `#1A1B1E` on hover,
/// accent when selected. Square: G HUB gives these no corner radius.
pub fn tool(ui: &mut Ui, icons: &mut IconCache, icon: Icon, active: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(t::TOOL_SIZE), Sense::click());
    let fill = if active {
        t::ACCENT
    } else if response.hovered() {
        t::RAISED
    } else {
        t::TOOL_BG
    };
    ui.painter().rect_filled(rect, CornerRadius::ZERO, fill);

    let colour = if active || response.hovered() { t::TEXT } else { t::TEXT_LABEL };
    icons.draw(ui, icon, rect, 20.0, colour);
    response
}

/// `Button.gh-row` - a full-width "COLORS ON DEVICE" row: 13 bold, transparent until hovered,
/// then `GHubHover` behind white text.
pub fn row(ui: &mut Ui, s: &str) -> Response {
    row_inner(ui, s, t::ROW_PAD, Color32::TRANSPARENT, false)
}

/// `Button.gh-row.anim` - an animation row. Sits on `#1A1B1E` at rest and goes solid accent while
/// it is the animation that is playing, the way a selected preset is called out in G HUB.
pub fn anim_row(ui: &mut Ui, s: &str, playing: bool) -> Response {
    row_inner(ui, s, t::ANIM_ROW_PAD, t::RAISED, playing)
}

/// Shared body of [`row`] and [`anim_row`]. Also used by the dropdown in [`crate::controls`],
/// because a dropdown item is a `gh-row` in G HUB.
pub(crate) fn row_inner(
    ui: &mut Ui,
    s: &str,
    pad: egui::Margin,
    rest: Color32,
    active: bool,
) -> Response {
    let width = ui.available_width();
    let g = text(ui, s, t::row());
    let height = g.size().y + (pad.top + pad.bottom) as f32;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());

    let fill = if active {
        t::ACCENT
    } else if response.hovered() {
        t::HOVER
    } else {
        rest
    };
    ui.painter().rect_filled(rect, CornerRadius::same(t::RADIUS_ROW), fill);

    let colour = if active || response.hovered() { t::TEXT } else { t::TEXT_ROW };
    ui.painter().galley(rect.min + Vec2::new(pad.left as f32, pad.top as f32), g, colour);
    response
}

/// `Button.gh-link` - a quick-group link under the keyboard. Goes `GHubAccentBright` on hover.
/// `dim` is the `.dim` variant: 11 instead of 12, in label grey.
pub fn link(ui: &mut Ui, s: &str, dim: bool) -> Response {
    let base = if dim { t::link_dim() } else { TextFormat { color: t::TEXT_ROW, ..t::tab() } };
    let g = text(ui, s, base);
    let pad = t::LINK_PAD;
    let size = g.size() + Vec2::new((pad.left + pad.right) as f32, (pad.top + pad.bottom) as f32);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    let colour = if response.hovered() {
        t::ACCENT_BRIGHT
    } else if dim {
        t::TEXT_LABEL
    } else {
        t::TEXT_ROW
    };
    ui.painter().galley(rect.min + Vec2::new(pad.left as f32, pad.top as f32), g, colour);
    response
}

/// `Button.gh-swatch` - a round colour swatch. 14px radius, with a translucent white wash on hover.
pub fn swatch(ui: &mut Ui, colour: Color32, size: f32, selected: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let radius = CornerRadius::same(t::RADIUS_SWATCH);
    if response.hovered() {
        ui.painter().rect_filled(rect, radius, t::SWATCH_HOVER);
    }
    ui.painter().rect_filled(rect.shrink(3.0), radius, colour);
    if selected {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            radius,
            Stroke::new(2.0, t::ACCENT_BRIGHT),
            StrokeKind::Inside,
        );
    }
    response
}

// ------------------------------------------------------------------ device stage

/// `Border.gh-sel` - the 3px accent ring drawn over a selected key on the device render.
pub fn selection_ring(ui: &Ui, rect: Rect) {
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(t::RADIUS_PILL),
        Stroke::new(t::SEL_RING, t::ACCENT_BRIGHT),
        StrokeKind::Outside,
    );
}

/// `Border.keycap` - the fallback vector keycap, used when a device render is not shipped: a
/// vertical `#26272A`-to-`#151618` gradient, 1px `#0B0B0C` border, and a 2px drop shadow below.
pub fn keycap(ui: &Ui, rect: Rect, lit: Option<Color32>, selected: bool, hovered: bool) {
    let painter = ui.painter();
    let radius = CornerRadius::same(t::RADIUS_KEYCAP);

    // BoxShadow="0 2 0 0 #0A0A0A"
    painter.rect_filled(rect.translate(Vec2::new(0.0, 2.0)), radius, t::KEYCAP_SHADOW);

    match lit {
        Some(colour) => {
            painter.rect_filled(rect, radius, colour);
        }
        None => vertical_gradient(painter, rect, radius, t::KEYCAP_TOP, t::KEYCAP_BOTTOM),
    }

    let (stroke, kind) = if selected {
        (Stroke::new(2.0, t::ACCENT_BRIGHT), StrokeKind::Inside)
    } else if hovered {
        (Stroke::new(1.0, t::KEYCAP_BORDER_HOVER), StrokeKind::Inside)
    } else {
        (Stroke::new(1.0, t::KEYCAP_BORDER), StrokeKind::Inside)
    };
    painter.rect_stroke(rect, radius, stroke, kind);
}

/// Avalonia's `LinearGradientBrush` has no direct egui equivalent, so the keycap gradient is drawn
/// as a vertical strip mesh. Cheap: a keycap is at most ~60px tall.
fn vertical_gradient(
    painter: &egui::Painter,
    rect: Rect,
    radius: CornerRadius,
    top: Color32,
    bottom: Color32,
) {
    const STEPS: usize = 8;
    let h = rect.height() / STEPS as f32;
    for i in 0..STEPS {
        let s = Rect::from_min_size(
            rect.min + Vec2::new(0.0, h * i as f32),
            Vec2::new(rect.width(), h + 1.0),
        );
        let f = (i as f32 + 0.5) / STEPS as f32;
        let colour = top.lerp_to_gamma(bottom, f);
        // Only the first and last strips need the rounding.
        let r = if i == 0 {
            CornerRadius { nw: radius.nw, ne: radius.ne, sw: 0, se: 0 }
        } else if i == STEPS - 1 {
            CornerRadius { nw: 0, ne: 0, sw: radius.sw, se: radius.se }
        } else {
            CornerRadius::ZERO
        };
        painter.rect_filled(s.intersect(rect), r, colour);
    }
}

/// `DrawingBrush x:Key="GHubHatch"` - the "keys disabled by default" hatching on the Game Mode
/// page: a 10x10 tile of 1.6px `#D8DCE2` diagonals, clipped to `rect`.
pub fn hatch(ui: &Ui, rect: Rect) {
    let painter = ui.painter().with_clip_rect(rect);
    let stroke = Stroke::new(t::HATCH_PEN, t::HATCH);
    let span = rect.width() + rect.height();
    let mut offset = 0.0;
    while offset <= span {
        // The tile's "M 0,10 L 10,0" diagonal, repeated every 10px across the rect.
        let a = egui::pos2(rect.left() + offset, rect.top());
        let b = egui::pos2(rect.left() + offset - rect.height(), rect.bottom());
        painter.line_segment([a, b], stroke);
        offset += t::HATCH_TILE;
    }
}

// ------------------------------------------------------------------ top nav

/// The `NavigationView PaneDisplayMode="Top"` strip from `PeripheralsView.axaml`: the ten page
/// names in a row, the selected one underlined in accent.
pub fn top_nav(ui: &mut Ui, tabs: &[&str], selected: &mut usize) -> bool {
    let mut changed = false;
    egui::Frame::new()
        .fill(t::BG)
        .inner_margin(egui::Margin { left: 20, right: 20, top: 10, bottom: 0 })
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                for (i, name) in tabs.iter().enumerate() {
                    let active = i == *selected;
                    let g = text(ui, name, TextFormat { color: t::TEXT_ROW, ..t::tab() });
                    let size = g.size() + Vec2::new(0.0, 14.0);
                    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

                    // G HUB brightens a hovered tab to the same full white as the active one; only
                    // an idle, inactive tab is dimmed.
                    let colour = if active || response.hovered() { t::TEXT } else { t::TEXT_LABEL };
                    ui.painter().galley(rect.min, g, colour);
                    if active {
                        ui.painter().hline(
                            rect.x_range(),
                            rect.bottom() - t::TAB_UNDERLINE,
                            Stroke::new(t::TAB_UNDERLINE, t::ACCENT),
                        );
                    }
                    if response.clicked() {
                        *selected = i;
                        changed = true;
                    }
                    ui.add_space(t::TAB_GAP);
                }
            });
        });
    changed
}

/// A caption/value pair, the shape most of the settings panels are built from: an 11px
/// `gh-label` caption with the control directly beneath it.
pub fn field<R>(ui: &mut Ui, caption: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    label(ui, caption);
    ui.add_space(6.0);
    let out = add(ui);
    ui.add_space(14.0);
    out
}

/// A status line, used by every tab's error/status banner (`PeripheralTabBase.Status`).
pub fn status(ui: &mut Ui, message: &str, is_error: bool) {
    let colour = if is_error { Color32::from_rgb(0xE8, 0x5A, 0x5A) } else { t::TEXT_HEAD };
    let g = wrapped(
        ui,
        message,
        TextFormat { color: colour, ..t::body() },
        ui.available_width(),
    );
    let (rect, _) = ui.allocate_exact_size(g.size(), Sense::hover());
    ui.painter().galley(rect.min, g, colour);
}

/// `TextBlock.rgb-label` - the `#`, `R`, `G` and `B` captions in the colour picker.
pub fn rgb_label(ui: &mut Ui, s: &str) {
    let galley = text(ui, s, TextFormat { color: t::TEXT_LABEL, ..t::row() });
    // `Margin="0 0 2 0"` on the caption.
    let (rect, _) = ui.allocate_exact_size(galley.size() + Vec2::new(2.0, 0.0), Sense::hover());
    ui.painter().galley(rect.min, galley, t::TEXT_LABEL);
}

/// Body text at a given size and colour, for the many `Classes="gh-body"` overrides the markup
/// carries (`FontSize="11"`, `Foreground="OrangeRed"`, and so on).
pub fn body_small(ui: &mut Ui, s: &str, colour: Color32) {
    if s.is_empty() {
        return;
    }
    let galley = wrapped(
        ui,
        s,
        TextFormat {
            color: colour,
            font_id: FontId::new(11.0, t::family(t::BODY)),
            line_height: Some(16.0),
            ..Default::default()
        },
        ui.available_width(),
    );
    let (rect, _) = ui.allocate_exact_size(galley.size(), Sense::hover());
    ui.painter().galley(rect.min, galley, colour);
}

/// The amber callout the reactions page uses to say the lighting is not being written: a
/// `#3A2A12` box with an `#A3721A` border.
pub fn callout_warning<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(Color32::from_rgb(0x3A, 0x2A, 0x12))
        .stroke(Stroke::new(1.0, Color32::from_rgb(0xA3, 0x72, 0x1A)))
        .corner_radius(CornerRadius::same(5))
        .inner_margin(egui::Margin { left: 10, right: 10, top: 8, bottom: 8 })
        .show(ui, add)
        .inner
}

/// The `#1A1B1E` card a reaction row, a macro row or a DPI stage sits on.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(t::RAISED)
        .corner_radius(CornerRadius::same(5))
        .inner_margin(egui::Margin { left: 10, right: 10, top: 8, bottom: 8 })
        .show(ui, add)
        .inner
}

/// Right-aligned helper for the "current / pending / saved-on-device" value trio the plan requires
/// every hardware control to show.
pub fn value_right(ui: &mut Ui, s: &str, colour: Color32) {
    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
        let g = text(ui, s, TextFormat { color: colour, font_id: FontId::new(13.0, t::family(t::BODY)), ..Default::default() });
        let (rect, _) = ui.allocate_exact_size(g.size(), Sense::hover());
        ui.painter().galley(rect.min, g, colour);
    });
}
