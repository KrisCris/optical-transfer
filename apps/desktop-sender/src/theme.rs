//! The sender's visual style.
//!
//! One rule constrains everything here: **nothing styles the QR code.** The
//! stage paints pure `Color32::BLACK` modules on pure `Color32::WHITE` at an
//! integer scale with nearest-neighbour filtering. A gradient, a tint, a
//! rounded module, a drop shadow or a fractional scale each costs decode
//! margin on the far side of a camera, and the receiver has no way to tell you
//! that is why it is struggling. The design lives in the chrome around the
//! code.
//!
//! Everything else is dark for the same reason the receiver is: the sender
//! sits next to a large white rectangle the phone's camera has to expose for,
//! and a bright UI around it makes that exposure harder.

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Vec2,
};

pub const BG: Color32 = Color32::from_rgb(0x07, 0x09, 0x0c);
pub const PANEL: Color32 = Color32::from_rgb(0x0e, 0x12, 0x19);
pub const RAISED: Color32 = Color32::from_rgb(0x16, 0x1c, 0x26);
pub const HOVER: Color32 = Color32::from_rgb(0x1d, 0x25, 0x31);
pub const LINE: Color32 = Color32::from_rgb(0x1e, 0x25, 0x31);
pub const LINE_BRIGHT: Color32 = Color32::from_rgb(0x2b, 0x34, 0x44);

pub const TEXT: Color32 = Color32::from_rgb(0xee, 0xf2, 0xf7);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x9a, 0xa7, 0xb8);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x64, 0x70, 0x7f);

pub const ACCENT: Color32 = Color32::from_rgb(0x34, 0xe5, 0xa0);
pub const ACCENT_INK: Color32 = Color32::from_rgb(0x03, 0x20, 0x15);
pub const WARN: Color32 = Color32::from_rgb(0xff, 0xc0, 0x43);
pub const DANGER: Color32 = Color32::from_rgb(0xff, 0x6b, 0x6b);

/// Text styles the app refers to by name, so a size change happens in one place.
pub const HEADING: f32 = 19.0;
pub const BODY: f32 = 14.0;
pub const SMALL: f32 = 12.0;
pub const LABEL: f32 = 11.0;

pub fn apply(ctx: &egui::Context) {
    // The sender is dark in both system themes on purpose: it sits beside a
    // large white rectangle a camera has to expose for, so a light chrome
    // makes the code harder to read, not easier.
    ctx.set_theme(egui::Theme::Dark);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();

    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(HEADING, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(BODY, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(BODY, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(SMALL, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(SMALL + 1.0, FontFamily::Monospace),
        ),
    ]
    .into();

    let spacing = &mut style.spacing;
    spacing.item_spacing = Vec2::new(8.0, 8.0);
    spacing.button_padding = Vec2::new(14.0, 8.0);
    spacing.window_margin = Margin::same(14);
    spacing.menu_margin = Margin::same(8);
    // Controls a mouse can hit without aiming, and a consistent row height so
    // the settings grid does not jitter between widget kinds.
    spacing.interact_size = Vec2::new(44.0, 30.0);
    spacing.combo_width = 120.0;
    spacing.icon_width = 16.0;
    spacing.icon_width_inner = 9.0;

    let visuals = &mut style.visuals;
    visuals.dark_mode = true;
    visuals.panel_fill = BG;
    visuals.window_fill = PANEL;
    visuals.window_stroke = Stroke::new(1.0, LINE);
    visuals.window_corner_radius = CornerRadius::same(12);
    visuals.extreme_bg_color = Color32::from_rgb(0x08, 0x0b, 0x10);
    visuals.faint_bg_color = Color32::from_rgb(0x11, 0x16, 0x1e);
    visuals.override_text_color = Some(TEXT);
    visuals.hyperlink_color = ACCENT;
    visuals.selection.bg_fill = ACCENT.linear_multiply(0.28);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);

    let radius = CornerRadius::same(9);
    let widgets = &mut visuals.widgets;

    widgets.noninteractive.bg_fill = PANEL;
    widgets.noninteractive.weak_bg_fill = PANEL;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_DIM);
    widgets.noninteractive.corner_radius = radius;

    widgets.inactive.bg_fill = RAISED;
    widgets.inactive.weak_bg_fill = RAISED;
    widgets.inactive.bg_stroke = Stroke::new(1.0, LINE_BRIGHT);
    widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.inactive.corner_radius = radius;
    widgets.inactive.expansion = 0.0;

    widgets.hovered.bg_fill = HOVER;
    widgets.hovered.weak_bg_fill = HOVER;
    widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x37, 0x42, 0x5a));
    widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.hovered.corner_radius = radius;
    widgets.hovered.expansion = 0.0;

    widgets.active.bg_fill = Color32::from_rgb(0x23, 0x2c, 0x3a);
    widgets.active.weak_bg_fill = Color32::from_rgb(0x23, 0x2c, 0x3a);
    widgets.active.bg_stroke = Stroke::new(1.0, ACCENT.linear_multiply(0.7));
    widgets.active.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.active.corner_radius = radius;
    widgets.active.expansion = 0.0;

    widgets.open.bg_fill = HOVER;
    widgets.open.weak_bg_fill = HOVER;
    widgets.open.bg_stroke = Stroke::new(1.0, LINE_BRIGHT);
    widgets.open.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.open.corner_radius = radius;

    let style = std::sync::Arc::new(style);
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
}

/// A section heading: small, spaced, and quiet enough to organise without
/// competing with the values underneath it.
pub fn section(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .size(LABEL)
            .color(TEXT_FAINT)
            .strong(),
    );
    ui.add_space(2.0);
}

/// A filled pill. Used for the security warning and for stream state, where a
/// plain line of text reads as decoration and gets skipped.
/// Text here must stay inside the default font's coverage. Two earlier
/// versions of this UI used U+259A and U+25CF and both rendered as an empty
/// box, because eframe's bundled proportional font carries neither.
pub fn pill(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    let painter_color = color.linear_multiply(0.16);
    egui::Frame::new()
        .fill(painter_color)
        .stroke(Stroke::new(1.0, color.linear_multiply(0.45)))
        .corner_radius(CornerRadius::same(u8::MAX))
        .inner_margin(Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(SMALL).color(color).strong());
        })
        .response
}

/// A bordered card. egui's own group frame is a flat grey box; this matches the
/// receiver's panels so the two halves of the product look related.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(PANEL)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(12))
        .show(ui, add)
        .inner
}

/// One `label: value` row of the stream readout, with the value in a tabular
/// monospace so digits do not shift as they change.
pub fn stat(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.label(egui::RichText::new(label).size(SMALL).color(TEXT_FAINT));
    ui.label(
        egui::RichText::new(value)
            .size(SMALL + 1.0)
            .family(FontFamily::Monospace)
            .color(TEXT),
    );
    ui.end_row();
}

/// The one button that should look like the thing to press.
///
/// egui has no notion of a primary action, so this is an explicitly filled
/// button rather than a variant of the default widget visuals — which every
/// other control also uses.
pub fn primary_button(ui: &mut egui::Ui, text: &str, height: f32) -> egui::Response {
    let label = egui::RichText::new(text)
        .color(ACCENT_INK)
        .size(BODY)
        .strong();
    ui.add_sized(
        [ui.available_width(), height],
        egui::Button::new(label)
            .fill(ACCENT)
            .corner_radius(CornerRadius::same(9))
            .stroke(Stroke::NONE),
    )
}

/// Three finder patterns, painted rather than typed.
///
/// The obvious alternative is a glyph, but a glyph is only as portable as the
/// font behind it: the first version of this used U+259A and rendered as an
/// empty box, because nothing in the default stack had it.
pub fn qr_motif(ui: &mut egui::Ui, size: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    let painter = ui.painter();
    let unit = size / 7.0;
    let radius = CornerRadius::same((unit * 0.35) as u8);

    // Outline plus centre, at the three corners a QR code puts them.
    for (column, row) in [(0.0, 0.0), (4.0, 0.0), (0.0, 4.0)] {
        let origin = rect.min + Vec2::new(column * unit, row * unit);
        let outer = egui::Rect::from_min_size(origin, Vec2::splat(unit * 3.0));
        painter.rect_stroke(
            outer,
            radius,
            Stroke::new(unit * 0.42, color),
            egui::StrokeKind::Inside,
        );
        let inner = egui::Rect::from_center_size(outer.center(), Vec2::splat(unit * 0.9));
        painter.rect_filled(inner, CornerRadius::same(1), color);
    }
}
