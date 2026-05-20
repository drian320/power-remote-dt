//! Shared visual theme for every power-remote-dt egui surface.
//!
//! GUI modernization P4: all GUI binaries (the unified launcher, the host
//! operator window, the connect launcher, the in-session overlay) depend on
//! `gui-common`, so applying one theme here gives the whole product a single,
//! consistent look across Windows and Linux (egui is custom-drawn, so the
//! result is pixel-identical on both — the "strict identity" decision from the
//! design doc §6).
//!
//! Aesthetic: dark-first "cyber-minimalism" / high-performance prosumer.
//! Charcoal/OLED backgrounds, a single "Performance Cyan" accent reserved for
//! primary actions / live indicators, and a crimson reserved for destructive
//! actions. Apply once at startup via [`apply_theme`] (or [`install_theme`],
//! which also installs the bundled JP font).

use egui::{CornerRadius, Stroke, Style, Visuals};

/// Design tokens. Kept `pub` so individual screens can reuse the exact same
/// colors for bespoke widgets (e.g. a red Disconnect button, a cyan "live"
/// dot) instead of hardcoding hex values at call sites.
pub mod tokens {
    use egui::Color32;

    /// Deepest background (app gutter / behind everything).
    pub const BG_DEEP: Color32 = Color32::from_rgb(0x12, 0x12, 0x14);
    /// Default window / panel fill.
    pub const BG: Color32 = Color32::from_rgb(0x1A, 0x1A, 0x1E);
    /// Raised surface (cards, group boxes, inactive widgets).
    pub const SURFACE: Color32 = Color32::from_rgb(0x25, 0x25, 0x29);
    /// Surface on hover (one step brighter).
    pub const SURFACE_HOVER: Color32 = Color32::from_rgb(0x30, 0x30, 0x36);
    /// Hairline borders / separators.
    pub const BORDER: Color32 = Color32::from_rgb(0x3A, 0x3A, 0x40);
    /// Primary accent — "Performance Cyan". Reserve for the primary action
    /// (Connect), active toggles, and live/connected indicators.
    pub const ACCENT: Color32 = Color32::from_rgb(0x00, 0xE5, 0xFF);
    /// Dimmed accent for hover strokes.
    pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x0A, 0x80, 0x90);
    /// Destructive / warning — disconnect buttons, security alerts.
    pub const DESTRUCTIVE: Color32 = Color32::from_rgb(0xFF, 0x45, 0x3A);
    /// Primary text.
    pub const TEXT: Color32 = Color32::from_rgb(0xE8, 0xE8, 0xEC);
    /// Secondary / dimmed text.
    pub const TEXT_DIM: Color32 = Color32::from_rgb(0x9A, 0x9A, 0xA2);
    /// Success / healthy indicator (latency OK).
    pub const OK: Color32 = Color32::from_rgb(0x32, 0xD7, 0x4B);
    /// Caution indicator (latency rising).
    pub const WARN: Color32 = Color32::from_rgb(0xE2, 0xC5, 0x41);

    /// Corner radius for surfaces/buttons (px).
    pub const RADIUS: u8 = 8;
}

/// The standard rounded corner used across the UI.
fn radius() -> CornerRadius {
    CornerRadius::same(tokens::RADIUS)
}

/// Build the power-remote-dt dark [`Style`]. Exposed for tests / previews;
/// most callers want [`apply_theme`].
pub fn dark_style() -> Style {
    let mut style = Style::default();
    let mut v = Visuals::dark();

    v.dark_mode = true;
    v.override_text_color = Some(tokens::TEXT);
    v.panel_fill = tokens::BG;
    v.window_fill = tokens::BG;
    // Text-edit / sunken backgrounds.
    v.extreme_bg_color = tokens::BG_DEEP;
    // Alternating row / faint highlight.
    v.faint_bg_color = tokens::SURFACE;
    v.hyperlink_color = tokens::ACCENT;

    v.window_corner_radius = radius();
    v.window_stroke = Stroke::new(1.0, tokens::BORDER);
    v.menu_corner_radius = radius();

    // Selection (text selection, selected combo entries) uses the accent.
    v.selection.bg_fill = tokens::ACCENT.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, tokens::ACCENT);

    // Widget states.
    let w = &mut v.widgets;
    // Non-interactive: labels, separators, group frames.
    w.noninteractive.bg_fill = tokens::BG;
    w.noninteractive.weak_bg_fill = tokens::BG;
    w.noninteractive.bg_stroke = Stroke::new(1.0, tokens::BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, tokens::TEXT_DIM);
    w.noninteractive.corner_radius = radius();

    // Inactive: buttons / combos at rest.
    w.inactive.bg_fill = tokens::SURFACE;
    w.inactive.weak_bg_fill = tokens::SURFACE;
    w.inactive.bg_stroke = Stroke::new(1.0, tokens::BORDER);
    w.inactive.fg_stroke = Stroke::new(1.0, tokens::TEXT);
    w.inactive.corner_radius = radius();

    // Hovered.
    w.hovered.bg_fill = tokens::SURFACE_HOVER;
    w.hovered.weak_bg_fill = tokens::SURFACE_HOVER;
    w.hovered.bg_stroke = Stroke::new(1.0, tokens::ACCENT_DIM);
    w.hovered.fg_stroke = Stroke::new(1.0, tokens::TEXT);
    w.hovered.corner_radius = radius();
    w.hovered.expansion = 1.0;

    // Active (pressed / focused).
    w.active.bg_fill = tokens::SURFACE_HOVER;
    w.active.weak_bg_fill = tokens::SURFACE_HOVER;
    w.active.bg_stroke = Stroke::new(1.0, tokens::ACCENT);
    w.active.fg_stroke = Stroke::new(1.5, tokens::TEXT);
    w.active.corner_radius = radius();
    w.active.expansion = 1.0;

    // Open (combo box / menu expanded).
    w.open.bg_fill = tokens::SURFACE;
    w.open.weak_bg_fill = tokens::SURFACE;
    w.open.bg_stroke = Stroke::new(1.0, tokens::BORDER);
    w.open.fg_stroke = Stroke::new(1.0, tokens::TEXT);
    w.open.corner_radius = radius();

    // Comfortable spacing — performance tools read better with a little air.
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    style.spacing.menu_margin = egui::Margin::same(6);
    style.spacing.window_margin = egui::Margin::same(12);

    style.visuals = v;
    style
}

/// Apply the power-remote-dt dark theme to an egui context. Idempotent.
pub fn apply_theme(ctx: &egui::Context) {
    ctx.set_style(dark_style());
}

/// Convenience: apply the theme AND install the bundled JP font in one call.
/// This is what GUI entry points should use at startup.
pub fn install_theme(ctx: &egui::Context) {
    crate::style::install_jp_font(ctx);
    apply_theme(ctx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_style_uses_accent_for_selection_and_links() {
        let s = dark_style();
        assert_eq!(s.visuals.hyperlink_color, tokens::ACCENT);
        assert_eq!(s.visuals.selection.stroke.color, tokens::ACCENT);
        assert!(s.visuals.dark_mode);
        assert_eq!(s.visuals.panel_fill, tokens::BG);
    }

    #[test]
    fn widgets_use_rounded_corners() {
        let s = dark_style();
        assert_eq!(s.visuals.widgets.inactive.corner_radius, radius());
        assert_eq!(s.visuals.window_corner_radius, radius());
    }
}
