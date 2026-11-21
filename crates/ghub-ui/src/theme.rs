//! G HUB device-screen look, ported token-for-token from
//! `Artemis.UI/Screens/Peripherals/Tabs/GHubStyles.axaml`.
//!
//! Colours are G HUB's own dls design-system tokens out of its `app.min.css`; metrics are the ones
//! measured from the 1920x1080 captures in `docs/ghub-reference/screenshots`. Nothing here is a
//! fresh design decision - if a value looks arbitrary it is because it matches G HUB.

// Every value here is one of G HUB's own `dls` tokens or a metric measured off its UI, ported as
// a set so each number sits next to the rule it came from. A handful have no caller: the controls
// that use them are drawn with their own local constants, or G HUB uses the token in a place this
// build renders differently. They are kept rather than deleted because the set's value is being
// complete and checkable against the stylesheet - a half-ported token table is one nobody can
// diff against the original.
#![allow(dead_code)]

use std::sync::Arc;

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke, TextFormat,
};

// ------------------------------------------------------------------ colours

/// `--dls-color-background` - the dark theme's fallback is `#000`.
pub const BG: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
/// `--dls-color-surface` - `#212225`. The Avalonia port had `#222326`, a shade off.
pub const PANEL: Color32 = Color32::from_rgb(0x21, 0x22, 0x25);
/// `--dark-tertiary-700` - `#2A2B30`, which is what G HUB rules its panels with.
pub const PANEL_BORDER: Color32 = Color32::from_rgb(0x2A, 0x2B, 0x30);
/// The device screen's accent.
///
/// G HUB carries two primaries: `--dls-color-primary-main` (`#1196FF`, a blue, used by the newer
/// shell chrome) and the legacy Logitech cyan that the *device* pages still use, recorded as
/// `--dls-color-primary-rgb: "0, 168, 224"` and `--dls-color-state-overlay-primary-bg-rgb`. The
/// screenshots show the cyan, so that is what this is. The Avalonia port had `#00A8E8`.
pub const ACCENT: Color32 = Color32::from_rgb(0x00, 0xA8, 0xE0);
/// `.lighting-key.selected` stroke - `#00A9E0`, a hair brighter than the fill accent.
pub const ACCENT_BRIGHT: Color32 = Color32::from_rgb(0x00, 0xA9, 0xE0);
/// `--dls-color-onsurface-high-xl` - `#FFF`.
pub const TEXT: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
/// `.freestyle-quick-color` and `.checkbox-label`'s own fill - `#AFB1B4`.
pub const TEXT_LABEL: Color32 = Color32::from_rgb(0xAF, 0xB1, 0xB4);
/// `--neutral-400` - `#A7A7A8`, G HUB's row text.
pub const TEXT_ROW: Color32 = Color32::from_rgb(0xA7, 0xA7, 0xA8);
/// `--dark-tertiary-500` - `#656870`, the dimmest readable label.
pub const TEXT_HEAD: Color32 = Color32::from_rgb(0x65, 0x68, 0x70);
/// `--dls-color-surface-overlay` - `#FFFFFF1A` over the surface. Flattened against [`PANEL`] so
/// it can be used as an opaque fill.
pub const HOVER: Color32 = Color32::from_rgb(0x2C, 0x2D, 0x30);
/// `--dls-color-error` - `#E54438`.
pub const ERROR: Color32 = Color32::from_rgb(0xE5, 0x44, 0x38);
/// `--warning-main-500` - `#FF5E01`.
pub const WARNING: Color32 = Color32::from_rgb(0xFF, 0x5E, 0x01);
/// `--warning-secondary-500` - `#FFB401`, the softer caution amber.
pub const CAUTION: Color32 = Color32::from_rgb(0xFF, 0xB4, 0x01);
/// `--dls-disable` - `#919192`.
pub const DISABLED: Color32 = Color32::from_rgb(0x91, 0x91, 0x92);
/// `--dls-color-slider-track` - `#6B6B6B`.
pub const SLIDER_TRACK: Color32 = Color32::from_rgb(0x6B, 0x6B, 0x6B);
/// `--dls-theme-scrollbar-thumb-color` - `#FFFFFF99`.
pub const SCROLL_THUMB: Color32 = Color32::from_rgba_premultiplied(0x99, 0x99, 0x99, 0x99);

/// `Button.gh-rail:pointerover`, `Button.gh-tool:pointerover`, `Button.gh-row.anim` resting fill.
pub const RAISED: Color32 = Color32::from_rgb(0x1A, 0x1B, 0x1E);
/// `Button.gh-tool` resting fill.
pub const TOOL_BG: Color32 = Color32::from_rgb(0x05, 0x05, 0x05);
/// `Button.gh-swatch:pointerover` - #30FFFFFF.
pub const SWATCH_HOVER: Color32 = Color32::from_rgba_premultiplied(0x30, 0x30, 0x30, 0x30);
/// `Border.keycap` border and drop shadow.
pub const KEYCAP_BORDER: Color32 = Color32::from_rgb(0x0B, 0x0B, 0x0C);
pub const KEYCAP_SHADOW: Color32 = Color32::from_rgb(0x0A, 0x0A, 0x0A);
pub const KEYCAP_BORDER_HOVER: Color32 = Color32::from_rgb(0x6A, 0x6D, 0x73);
/// `Border.keycap` vertical gradient, top then bottom.
pub const KEYCAP_TOP: Color32 = Color32::from_rgb(0x26, 0x27, 0x2A);
pub const KEYCAP_BOTTOM: Color32 = Color32::from_rgb(0x15, 0x16, 0x18);
/// Pen of the "keys disabled by default" hatch on the Game Mode page.
pub const HATCH: Color32 = Color32::from_rgb(0xD8, 0xDC, 0xE2);

// ------------------------------------------------------------------ metrics

/// `Border.gh-root`, `Border.gh-panel` - CornerRadius 8.
pub const RADIUS_PANEL: u8 = 8;
/// `Button.gh-rail` - CornerRadius 12.
pub const RADIUS_RAIL: u8 = 12;
/// `Border.gh-pill`, `Border.gh-sel`, `Border.gh-callout` - CornerRadius 6.
pub const RADIUS_PILL: u8 = 6;
/// `Button.gh-swatch` - CornerRadius 14.
pub const RADIUS_SWATCH: u8 = 14;
/// `Border.keycap` - CornerRadius 4.
pub const RADIUS_KEYCAP: u8 = 4;
/// `Button.gh-row` - CornerRadius 3.
pub const RADIUS_ROW: u8 = 3;

/// `.menu-icon-wrapper` - 48x48 with a 12px radius.
///
/// The Avalonia port used 52x52, measured off a screenshot. G HUB's own stylesheet says 48.
pub const RAIL_SIZE: f32 = 48.0;
pub const RAIL_GAP: f32 = 10.0;

/// `.menu-icon-wrapper` opacity by state: `.6` at rest, `.4` on hover, `.2` while pressed, and
/// `1` when active (where it also takes the accent as a background).
///
/// Note the direction: G HUB *dims* a hovered rail icon rather than brightening it. Both the
/// Avalonia port and the first draft of this one had it backwards, brightening on hover, which
/// makes the active item look less emphasised than the one under the pointer.
pub const RAIL_OPACITY_REST: f32 = 0.6;
pub const RAIL_OPACITY_HOVER: f32 = 0.4;
pub const RAIL_OPACITY_PRESSED: f32 = 0.2;
pub const RAIL_OPACITY_ACTIVE: f32 = 1.0;
/// `.menu-icon-wrapper.active:hover` - `.9`, and `.8` while pressed.
pub const RAIL_OPACITY_ACTIVE_HOVER: f32 = 0.9;
pub const RAIL_OPACITY_ACTIVE_PRESSED: f32 = 0.8;
/// `.menu-icon-wrapper.disabled` - `.4`, with pointer events off.
pub const RAIL_OPACITY_DISABLED: f32 = 0.4;

/// `.info-tooltip` - 15x15 beside a label.
pub const TOOLTIP_ICON: f32 = 15.0;
/// `.stepper-control` - a 50x20 hit area with a 16x8 glyph in it.
pub const STEPPER_WIDTH: f32 = 50.0;
pub const STEPPER_HEIGHT: f32 = 20.0;
pub const STEPPER_GLYPH_WIDTH: f32 = 16.0;
pub const STEPPER_GLYPH_HEIGHT: f32 = 8.0;
/// `.dropdown-item.dropdown-button` - a 25px hit area with a 20px glyph.
pub const ROW_ICON_BUTTON: f32 = 25.0;
pub const ROW_ICON_GLYPH: f32 = 20.0;
/// `.lighting-key.selected` - `stroke-width: 10` in the render's own coordinate space, which is
/// why the selection ring is projected rather than drawn at a fixed pixel width.
pub const LIGHTING_KEY_STROKE: f32 = 10.0;
/// `Button.gh-tool` - 40x40 square, no radius.
pub const TOOL_SIZE: f32 = 40.0;
/// `Border.gh-panel` - fixed 307px wide, left-aligned, 8px margin.
pub const PANEL_WIDTH: f32 = 307.0;
pub const PANEL_MARGIN: f32 = 8.0;
/// `Border.gh-pill` - MinWidth 260.
pub const PILL_MIN_WIDTH: f32 = 260.0;
/// `Border.gh-callout` - MinWidth 190.
pub const CALLOUT_MIN_WIDTH: f32 = 190.0;
/// `Button.gh-tab` - 2px bottom border, 18px gap to the next tab.
pub const TAB_UNDERLINE: f32 = 2.0;
pub const TAB_GAP: f32 = 18.0;
/// `Border.gh-sel` - 3px accent ring over a key on the device render.
pub const SEL_RING: f32 = 3.0;
/// Hatch tile is 10x10 with a 1.6px pen.
pub const HATCH_TILE: f32 = 10.0;
pub const HATCH_PEN: f32 = 1.6;

/// `Border.gh-panel` - Padding 20 18.
pub const PANEL_PAD: Margin = Margin { left: 20, right: 20, top: 18, bottom: 18 };
/// `Border.gh-stage` - Padding 24 18.
pub const STAGE_PAD: Margin = Margin { left: 24, right: 24, top: 18, bottom: 18 };
/// `Border.gh-pill` - Padding 14 10.
pub const PILL_PAD: Margin = Margin { left: 14, right: 14, top: 10, bottom: 10 };
/// `Border.gh-callout` - Padding 12 8.
pub const CALLOUT_PAD: Margin = Margin { left: 12, right: 12, top: 8, bottom: 8 };
/// `Button.gh-row` - Padding 0 5 6 5.
pub const ROW_PAD: Margin = Margin { left: 0, right: 6, top: 5, bottom: 5 };
/// `Button.gh-row.anim` - Padding 10 7.
pub const ANIM_ROW_PAD: Margin = Margin { left: 10, right: 10, top: 7, bottom: 7 };
/// `Button.gh-link` - Padding 0 4 30 4.
pub const LINK_PAD: Margin = Margin { left: 0, right: 30, top: 4, bottom: 4 };
/// `Button.gh-tab` - Padding 0 2 0 5.
pub const TAB_PAD: Margin = Margin { left: 0, right: 0, top: 2, bottom: 5 };
/// `Frame` host padding in `PeripheralsView.axaml`.
pub const CONTENT_PAD: Margin = Margin { left: 20, right: 20, top: 20, bottom: 20 };

/// Fluent paints its scrollbar over the content, so every scrolling panel keeps a bar's width of
/// viewport clear on the right. The same reservation here keeps the colour swatch and the spectrum
/// picker on FREESTYLE off the bar.
pub const SCROLLBAR_RESERVE: f32 = 14.0;

// ------------------------------------------------------------------ fonts

/// `GHubFont` - avares://Artemis.UI/Assets/Fonts#BrownPro.
pub const BODY: &str = "BrownPro";
pub const BODY_BOLD: &str = "BrownPro-Bold";
/// `GHubDisplayFont` - avares://Artemis.UI/Assets/Fonts#Brown Logitech Pan.
pub const DISPLAY: &str = "BrownLogitechPan";
pub const DISPLAY_BOLD: &str = "BrownLogitechPan-Bold";
/// Diagnostics / HID++ trace view.
pub const MONO: &str = "RobotoMono";

pub fn family(name: &str) -> FontFamily {
    FontFamily::Name(Arc::from(name))
}

/// Logitech's own faces. Layout depends on them, so unlike the device renders these are embedded.
pub fn font_definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::empty();

    let faces: [(&str, &[u8]); 5] = [
        (BODY, include_bytes!("../../../assets/fonts/BrownPro-Regular.otf")),
        (BODY_BOLD, include_bytes!("../../../assets/fonts/BrownPro-Bold.otf")),
        (DISPLAY, include_bytes!("../../../assets/fonts/BrownLogitechPan-Regular.otf")),
        (DISPLAY_BOLD, include_bytes!("../../../assets/fonts/BrownLogitechPan-Bold.otf")),
        (MONO, include_bytes!("../../../assets/fonts/RobotoMono-Regular.ttf")),
    ];

    for (name, bytes) in faces {
        fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
        fonts.families.insert(family(name), vec![name.to_owned()]);
    }

    // egui still resolves Proportional/Monospace for anything that does not name a family.
    fonts.families.insert(FontFamily::Proportional, vec![BODY.to_owned()]);
    fonts.families.insert(FontFamily::Monospace, vec![MONO.to_owned()]);
    fonts
}

// ------------------------------------------------------------------ text styles

/// `TextBlock.gh-wordmark` - display font, 32, bold.
pub fn wordmark() -> TextFormat {
    TextFormat { font_id: FontId::new(32.0, family(DISPLAY_BOLD)), color: TEXT, ..Default::default() }
}

/// `TextBlock.gh-title` - display font, 22, LetterSpacing 0.5.
pub fn title() -> TextFormat {
    TextFormat {
        font_id: FontId::new(22.0, family(DISPLAY)),
        color: TEXT,
        extra_letter_spacing: 0.5,
        ..Default::default()
    }
}

/// `TextBlock.gh-label` - the "EFFECT" / "SELECTION MODE" / "COLORS ON DEVICE" captions:
/// 11, bold, LetterSpacing 0.6, label grey.
pub fn label() -> TextFormat {
    TextFormat {
        font_id: FontId::new(11.0, family(BODY_BOLD)),
        color: TEXT_LABEL,
        extra_letter_spacing: 0.6,
        ..Default::default()
    }
}

/// `TextBlock.gh-body` - 13 on a 19px line.
pub fn body() -> TextFormat {
    TextFormat {
        font_id: FontId::new(13.0, family(BODY)),
        color: TEXT_ROW,
        line_height: Some(19.0),
        ..Default::default()
    }
}

/// `Button.gh-row` / `ComboBox.gh-combo` - 13 bold.
pub fn row() -> TextFormat {
    TextFormat { font_id: FontId::new(13.0, family(BODY_BOLD)), color: TEXT_ROW, ..Default::default() }
}

/// `Button.gh-tab` / `Button.gh-link` - 12 bold.
pub fn tab() -> TextFormat {
    TextFormat { font_id: FontId::new(12.0, family(BODY_BOLD)), color: TEXT_LABEL, ..Default::default() }
}

/// `Button.gh-link.dim` - 11 bold, label grey.
pub fn link_dim() -> TextFormat {
    TextFormat { font_id: FontId::new(11.0, family(BODY_BOLD)), color: TEXT_LABEL, ..Default::default() }
}

// ------------------------------------------------------------------ install

/// Applies the G HUB look to a context: fonts, flat dark visuals, and none of the widget chrome
/// egui would otherwise draw. Panels, rails and tabs are painted explicitly in `widgets`, so the
/// base visuals only have to stay out of the way.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());

    // G HUB has exactly one appearance, so both of egui's themes get it: a viewer whose OS is in
    // light mode still gets the dark device screen rather than an unstyled one.
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.all_styles_mut(apply);
}

fn apply(style: &mut egui::Style) {
    style.visuals.dark_mode = true;
    style.visuals.panel_fill = BG;
    style.visuals.window_fill = PANEL;
    style.visuals.extreme_bg_color = TOOL_BG;
    style.visuals.faint_bg_color = RAISED;
    style.visuals.override_text_color = Some(TEXT_ROW);
    style.visuals.selection.bg_fill = ACCENT;
    style.visuals.selection.stroke = Stroke::new(1.0, TEXT);
    style.visuals.hyperlink_color = ACCENT_BRIGHT;
    style.visuals.window_corner_radius = CornerRadius::same(RADIUS_PANEL);
    style.visuals.window_stroke = Stroke::new(1.0, PANEL_BORDER);
    style.visuals.popup_shadow = egui::epaint::Shadow::NONE;
    style.visuals.window_shadow = egui::epaint::Shadow::NONE;

    // G HUB has no raised or bevelled controls: every surface is flat and state is shown by fill.
    for w in [
        &mut style.visuals.widgets.noninteractive,
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(RADIUS_PILL);
        w.bg_stroke = Stroke::NONE;
        w.expansion = 0.0;
    }
    style.visuals.widgets.noninteractive.bg_fill = PANEL;
    style.visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_ROW);
    style.visuals.widgets.inactive.bg_fill = RAISED;
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_ROW);
    style.visuals.widgets.hovered.bg_fill = HOVER;
    style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    style.visuals.widgets.active.bg_fill = ACCENT;
    style.visuals.widgets.active.fg_stroke = Stroke::new(1.0, TEXT);
    style.visuals.widgets.open.bg_fill = PANEL;

    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.window_margin = PANEL_PAD;
    style.spacing.button_padding = egui::vec2(10.0, 7.0);
    style.spacing.scroll.bar_width = SCROLLBAR_RESERVE;
}
