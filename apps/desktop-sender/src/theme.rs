//! The sender's visual style, matched to the web app's tokens so the two
//! senders read as one product.
//!
//! One rule constrains everything here: **nothing styles the QR code.** The
//! stage paints pure black modules on pure white at an integer number of
//! device pixels per module with nearest-neighbour filtering. A gradient, a
//! tint, a rounded module or a fractional scale each costs decode margin on
//! the far side of a camera, and the receiver has no way to tell you that is
//! why it is struggling. The design lives in the chrome around the code.
//!
//! The chrome is dark for the same reason the receiver's is: it sits next to a
//! large white rectangle a camera has to expose for, and a bright UI around it
//! makes that exposure harder.

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, Sense, Stroke, StrokeKind, TextStyle,
    Vec2,
};

pub const BG: Color32 = Color32::from_rgb(0x0a, 0x0c, 0x0f);
pub const SURFACE: Color32 = Color32::from_rgb(0x11, 0x14, 0x18);
pub const RAISED: Color32 = Color32::from_rgb(0x17, 0x1b, 0x21);
pub const HOVER: Color32 = Color32::from_rgb(0x1d, 0x22, 0x2a);
pub const CELL: Color32 = Color32::from_rgb(0x1f, 0x25, 0x2d);
pub const LINE: Color32 = Color32::from_rgb(0x24, 0x2a, 0x32);
pub const LINE_STRONG: Color32 = Color32::from_rgb(0x32, 0x39, 0x44);

pub const TEXT: Color32 = Color32::from_rgb(0xe9, 0xed, 0xf2);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8d, 0x97, 0xa5);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x5f, 0x68, 0x75);

pub const ACCENT: Color32 = Color32::from_rgb(0x4f, 0xe3, 0xc1);
pub const ACCENT_INK: Color32 = Color32::from_rgb(0x04, 0x24, 0x1d);
pub const WARN: Color32 = Color32::from_rgb(0xf4, 0xc1, 0x52);
pub const DANGER: Color32 = Color32::from_rgb(0xff, 0x6b, 0x6b);

pub const HEADING: f32 = 15.0;
pub const BODY: f32 = 14.0;
pub const SMALL: f32 = 12.5;
pub const LABEL: f32 = 11.0;
pub const RADIUS: u8 = 12;

pub fn apply(ctx: &egui::Context) {
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
            FontId::new(SMALL, FontFamily::Monospace),
        ),
    ]
    .into();

    let spacing = &mut style.spacing;
    spacing.item_spacing = Vec2::new(8.0, 8.0);
    spacing.button_padding = Vec2::new(14.0, 7.0);
    spacing.window_margin = Margin::same(14);
    spacing.menu_margin = Margin::same(6);
    spacing.interact_size = Vec2::new(40.0, 30.0);
    spacing.combo_width = 130.0;

    let visuals = &mut style.visuals;
    visuals.dark_mode = true;
    visuals.panel_fill = BG;
    visuals.window_fill = SURFACE;
    visuals.window_stroke = Stroke::new(1.0, LINE);
    visuals.window_corner_radius = CornerRadius::same(10);
    visuals.extreme_bg_color = Color32::from_rgb(0x0d, 0x10, 0x14);
    visuals.faint_bg_color = RAISED;
    visuals.override_text_color = Some(TEXT);
    visuals.hyperlink_color = ACCENT;
    visuals.selection.bg_fill = ACCENT.linear_multiply(0.28);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);

    let radius = CornerRadius::same(9);
    let widgets = &mut visuals.widgets;
    for (state, fill, stroke, text) in [
        (&mut widgets.noninteractive, SURFACE, LINE, TEXT_DIM),
        (&mut widgets.inactive, RAISED, LINE_STRONG, TEXT),
        (
            &mut widgets.hovered,
            HOVER,
            Color32::from_rgb(0x3d, 0x46, 0x53),
            TEXT,
        ),
        (&mut widgets.active, CELL, ACCENT.linear_multiply(0.7), TEXT),
        (&mut widgets.open, HOVER, LINE_STRONG, TEXT),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = Stroke::new(1.0, stroke);
        state.fg_stroke = Stroke::new(1.0, text);
        state.corner_radius = radius;
        state.expansion = 0.0;
    }

    let style = std::sync::Arc::new(style);
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
}

/// A bordered card with a title row. `aside` goes at the right of the title.
pub fn card<R>(
    ui: &mut egui::Ui,
    title: &str,
    aside: impl FnOnce(&mut egui::Ui),
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(title).size(HEADING).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), aside);
            });
            ui.add_space(6.0);
            add(ui)
        })
        .inner
}

/// A small uppercase label above a group of values.
pub fn section(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .size(LABEL)
            .color(TEXT_FAINT)
            .strong(),
    );
}

/// A filled pill for state the eye must not skip, like the security warning.
/// Text must stay inside the default font's coverage: two earlier versions of
/// this UI used block glyphs that rendered as empty boxes.
pub fn pill(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    egui::Frame::new()
        .fill(color.linear_multiply(0.12))
        .stroke(Stroke::new(1.0, color.linear_multiply(0.6)))
        .corner_radius(CornerRadius::same(u8::MAX))
        .inner_margin(Margin::symmetric(10, 3))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(SMALL).color(color).strong());
        })
        .response
}

/// Two or more mutually exclusive options in one rounded track.
pub fn segmented<T: PartialEq + Copy>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)]) {
    egui::Frame::new()
        .fill(BG)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(CornerRadius::same(u8::MAX))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(|ui| {
                for (option, label) in options {
                    let selected = *value == *option;
                    let text = egui::RichText::new(*label)
                        .size(SMALL + 0.5)
                        .strong()
                        .color(if selected { TEXT } else { TEXT_DIM });
                    let button = egui::Button::new(text)
                        .fill(if selected {
                            RAISED
                        } else {
                            Color32::TRANSPARENT
                        })
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(u8::MAX))
                        .min_size(Vec2::new(64.0, 26.0));
                    if ui.add(button).clicked() {
                        *value = *option;
                    }
                }
            });
        });
}

/// A selectable tile: a name and the numbers behind it.
pub fn tile(
    ui: &mut egui::Ui,
    selected: bool,
    title: &str,
    detail: &str,
    width: f32,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 54.0), Sense::click());
    let painter = ui.painter();
    let hovered = response.hovered() && ui.is_enabled();
    let fill = if selected {
        ACCENT.linear_multiply(0.09)
    } else if hovered {
        HOVER
    } else {
        RAISED
    };
    let stroke = if selected { ACCENT } else { LINE };
    painter.rect(
        rect,
        CornerRadius::same(10),
        fill,
        Stroke::new(1.0, stroke),
        StrokeKind::Inside,
    );
    let faded = |color: Color32| {
        if ui.is_enabled() {
            color
        } else {
            color.linear_multiply(0.5)
        }
    };
    painter.text(
        rect.left_top() + Vec2::new(12.0, 10.0),
        egui::Align2::LEFT_TOP,
        title,
        FontId::new(BODY, FontFamily::Proportional),
        faded(TEXT),
    );
    painter.text(
        rect.left_top() + Vec2::new(12.0, 31.0),
        egui::Align2::LEFT_TOP,
        detail,
        FontId::new(SMALL - 0.5, FontFamily::Proportional),
        faded(if selected { ACCENT } else { TEXT_DIM }),
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// One `label  value` row with a hairline under it, value in tabular mono.
pub fn stat(ui: &mut egui::Ui, label: &str, value: &str, color: Color32, width: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 26.0), Sense::hover());
    let painter = ui.painter();
    painter.text(
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::new(SMALL, FontFamily::Proportional),
        TEXT_DIM,
    );
    painter.text(
        rect.right_center(),
        egui::Align2::RIGHT_CENTER,
        value,
        FontId::new(SMALL, FontFamily::Monospace),
        color,
    );
    painter.hline(rect.x_range(), rect.bottom(), Stroke::new(1.0, LINE));
}

/// The one button that should look like the thing to press.
pub fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let label = egui::RichText::new(text)
        .color(ACCENT_INK)
        .size(BODY + 1.0)
        .strong();
    ui.add_sized(
        [ui.available_width(), 42.0],
        egui::Button::new(label)
            .fill(ACCENT)
            .corner_radius(CornerRadius::same(10))
            .stroke(Stroke::NONE),
    )
}

/// A full-width secondary button.
pub fn wide_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add_sized(
        [ui.available_width(), 38.0],
        egui::Button::new(egui::RichText::new(text).size(BODY).strong())
            .corner_radius(CornerRadius::same(10)),
    )
}

/// A thin progress track.
pub fn track(ui: &mut egui::Ui, fraction: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 4.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(2), CELL);
    let mut done = rect;
    done.set_width(rect.width() * fraction.clamp(0.0, 1.0));
    painter.rect_filled(done, CornerRadius::same(2), color);
}

/// Three finder patterns, painted rather than typed: a glyph is only as
/// portable as the font behind it.
pub fn qr_motif(ui: &mut egui::Ui, size: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_motif(ui.painter(), rect, color);
}

pub fn paint_motif(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let unit = rect.width() / 7.0;
    let radius = CornerRadius::same((unit * 0.35) as u8);
    for (column, row) in [(0.0, 0.0), (4.0, 0.0), (0.0, 4.0)] {
        let origin = rect.min + Vec2::new(column * unit, row * unit);
        let outer = egui::Rect::from_min_size(origin, Vec2::splat(unit * 3.0));
        painter.rect_stroke(
            outer,
            radius,
            Stroke::new(unit * 0.42, color),
            StrokeKind::Inside,
        );
        let inner = egui::Rect::from_center_size(outer.center(), Vec2::splat(unit * 0.9));
        painter.rect_filled(inner, CornerRadius::same(1), color);
    }
    let small = egui::Rect::from_min_size(
        rect.min + Vec2::new(4.6 * unit, 4.6 * unit),
        Vec2::splat(unit),
    );
    painter.rect_filled(small, CornerRadius::same(1), color);
    painter.rect_filled(
        small.translate(Vec2::splat(unit * 1.4)),
        CornerRadius::same(1),
        color,
    );
}
