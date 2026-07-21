//! Shared visual theme for every power-remote-dt egui surface.
//!
//! GUI modernization P4 → Hallmark redesign: all GUI binaries (the unified
//! launcher, the host operator window, the connect launcher, the in-session
//! overlay) depend on `gui-common`, so applying one theme here gives the whole
//! product a single, consistent look across Windows and Linux (egui is
//! custom-drawn, so the result is pixel-identical on both).
//!
//! Design system (Hallmark · genre: modern-minimal/technical · dark):
//! · Neutrals — an OKLCH-even cool ink ramp with a faint blue undertone
//!   (charcoal → surface → hairline), so elevation steps read as evenly spaced
//!   rather than ad-hoc.
//! · Accent — a single refined "Performance Cyan" reserved for the primary
//!   action (Connect / Start sharing), the active nav route, and live/connected
//!   indicators. Never decorative; restraint is the point.
//! · Semantics — crimson (destructive), green (healthy), amber (caution).
//! · Type — an intentional TextStyle scale; the 9-digit device ID is the hero,
//!   rendered large in tabular monospace.
//! Apply once at startup via [`install_theme`].
//!
//! Hallmark · component-system: egui theme · states: inactive · hover · active ·
//! open · selection · disabled · contrast: pass

use egui::{CornerRadius, FontFamily, FontId, Margin, Stroke, Style, TextStyle, Visuals};

/// Design tokens. Kept `pub` so individual screens can reuse the exact same
/// colors for bespoke widgets (e.g. a red Disconnect button, a cyan "live"
/// dot) instead of hardcoding hex values at call sites.
pub mod tokens {
    use egui::Color32;

    // --- Neutral ink ramp (dark, faint blue undertone; OKLCH-even steps) ---
    /// Deepest background (app gutter / behind everything).
    pub const BG_DEEP: Color32 = Color32::from_rgb(0x0E, 0x11, 0x16);
    /// Default window / panel fill.
    pub const BG: Color32 = Color32::from_rgb(0x14, 0x18, 0x1E);
    /// Raised surface (cards, group boxes, inactive widgets).
    pub const SURFACE: Color32 = Color32::from_rgb(0x1B, 0x20, 0x27);
    /// Surface on hover (one step brighter).
    pub const SURFACE_HOVER: Color32 = Color32::from_rgb(0x23, 0x2A, 0x33);
    /// Hairline borders / separators.
    pub const BORDER: Color32 = Color32::from_rgb(0x2C, 0x33, 0x3D);
    /// Stronger border for hover emphasis (still neutral — accent is reserved).
    pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x3A, 0x42, 0x4E);

    // --- Accent (brand "Performance Cyan", refined) ---
    /// Primary accent. Reserve for the primary action (Connect / Start),
    /// the active nav route, and live/connected indicators.
    pub const ACCENT: Color32 = Color32::from_rgb(0x22, 0xD3, 0xEE);
    /// Brighter accent for hover on accent-filled controls.
    pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x4D, 0xDD, 0xF2);
    /// Dimmed accent for subtle strokes.
    pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x14, 0x80, 0x8F);
    /// Very dark accent tint — selected nav fill, faint accent surfaces.
    pub const ACCENT_WEAK: Color32 = Color32::from_rgb(0x10, 0x2A, 0x31);

    // --- Semantic ---
    /// Destructive / warning — disconnect buttons, security alerts.
    pub const DESTRUCTIVE: Color32 = Color32::from_rgb(0xFB, 0x5B, 0x5B);
    /// Success / healthy indicator (latency OK, connected, sharing on).
    pub const OK: Color32 = Color32::from_rgb(0x4A, 0xDE, 0x80);
    /// Caution indicator (latency rising, unprovisioned).
    pub const WARN: Color32 = Color32::from_rgb(0xE6, 0xB5, 0x4A);

    // --- Text ---
    /// Primary text.
    pub const TEXT: Color32 = Color32::from_rgb(0xE6, 0xEA, 0xF0);
    /// Secondary / dimmed text (values, body detail).
    pub const TEXT_DIM: Color32 = Color32::from_rgb(0x9A, 0xA3, 0xB0);
    /// Faint text — eyebrow section labels, hints.
    pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x6B, 0x74, 0x80);

    /// Corner radius for interactive controls (buttons, inputs) — px.
    pub const RADIUS: u8 = 8;
    /// Corner radius for cards / windows — px (softer than controls).
    pub const RADIUS_CARD: u8 = 10;
}

/// The rounded corner used for interactive controls.
fn radius() -> CornerRadius {
    CornerRadius::same(tokens::RADIUS)
}

/// The rounded corner used for cards / group frames / windows.
fn card_radius() -> CornerRadius {
    CornerRadius::same(tokens::RADIUS_CARD)
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

    v.window_corner_radius = card_radius();
    v.window_stroke = Stroke::new(1.0_f32, tokens::BORDER);
    v.menu_corner_radius = card_radius();

    // Selection (text selection, selected combo entries) uses the accent.
    v.selection.bg_fill = tokens::ACCENT.gamma_multiply(0.30);
    v.selection.stroke = Stroke::new(1.0_f32, tokens::ACCENT);

    // Widget states — accent is reserved; hover stays neutral (a brighter
    // surface + a stronger hairline), and only the pressed/focused state earns
    // an accent stroke. This is the restraint move: no cyan on every hover.
    let w = &mut v.widgets;
    // Non-interactive: labels, separators, group/card frames.
    w.noninteractive.bg_fill = tokens::SURFACE;
    w.noninteractive.weak_bg_fill = tokens::SURFACE;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, tokens::BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, tokens::TEXT_DIM);
    w.noninteractive.corner_radius = card_radius();

    // Inactive: buttons / combos / inputs at rest.
    w.inactive.bg_fill = tokens::SURFACE;
    w.inactive.weak_bg_fill = tokens::SURFACE;
    w.inactive.bg_stroke = Stroke::new(1.0_f32, tokens::BORDER);
    w.inactive.fg_stroke = Stroke::new(1.0_f32, tokens::TEXT);
    w.inactive.corner_radius = radius();

    // Hovered — neutral brighten, no accent.
    w.hovered.bg_fill = tokens::SURFACE_HOVER;
    w.hovered.weak_bg_fill = tokens::SURFACE_HOVER;
    w.hovered.bg_stroke = Stroke::new(1.0_f32, tokens::BORDER_STRONG);
    w.hovered.fg_stroke = Stroke::new(1.0_f32, tokens::TEXT);
    w.hovered.corner_radius = radius();
    w.hovered.expansion = 1.0;

    // Active (pressed / keyboard-focused) — the one place the accent shows on
    // an ordinary control, so focus is unmistakable.
    w.active.bg_fill = tokens::SURFACE_HOVER;
    w.active.weak_bg_fill = tokens::SURFACE_HOVER;
    w.active.bg_stroke = Stroke::new(1.5_f32, tokens::ACCENT);
    w.active.fg_stroke = Stroke::new(1.5_f32, tokens::TEXT);
    w.active.corner_radius = radius();
    w.active.expansion = 1.0;

    // Open (combo box / menu expanded).
    w.open.bg_fill = tokens::SURFACE;
    w.open.weak_bg_fill = tokens::SURFACE;
    w.open.bg_stroke = Stroke::new(1.0_f32, tokens::BORDER_STRONG);
    w.open.fg_stroke = Stroke::new(1.0_f32, tokens::TEXT);
    w.open.corner_radius = radius();

    // Intentional type scale — a clear step between heading, body, and the
    // small eyebrow labels; monospace is the "data" voice (IDs, PIN, addrs).
    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(20.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
        (
            TextStyle::Monospace,
            FontId::new(13.5, FontFamily::Monospace),
        ),
        (
            TextStyle::Button,
            FontId::new(14.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(11.0, FontFamily::Proportional),
        ),
    ]
    .into();

    // 4-pt spacing scale — a little more air than egui's default so the
    // prosumer surfaces read calm rather than cramped.
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 8.0);
    style.spacing.interact_size.y = 32.0;
    style.spacing.menu_margin = Margin::same(8);
    style.spacing.window_margin = Margin::same(16);

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
    fn cards_use_card_radius_and_controls_use_control_radius() {
        let s = dark_style();
        // Cards / group frames (noninteractive) use the softer card radius…
        assert_eq!(
            s.visuals.widgets.noninteractive.corner_radius,
            card_radius()
        );
        // …while interactive controls use the tighter control radius.
        assert_eq!(s.visuals.widgets.inactive.corner_radius, radius());
        assert_eq!(s.visuals.window_corner_radius, card_radius());
    }

    #[test]
    fn hover_stays_neutral_focus_earns_accent() {
        let s = dark_style();
        // Restraint: hover must not paint the accent…
        assert_eq!(
            s.visuals.widgets.hovered.bg_stroke.color,
            tokens::BORDER_STRONG
        );
        // …but the pressed/focused state does, so focus is unmistakable.
        assert_eq!(s.visuals.widgets.active.bg_stroke.color, tokens::ACCENT);
    }

    #[test]
    fn type_scale_is_intentional() {
        let s = dark_style();
        let heading = &s.text_styles[&TextStyle::Heading];
        let body = &s.text_styles[&TextStyle::Body];
        assert!(heading.size > body.size, "heading must out-size body");
    }
}
