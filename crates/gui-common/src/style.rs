//! egui style + Japanese font setup.
//!
//! Embeds Noto Sans CJK JP (subset) so Japanese strings render without
//! relying on the user's installed fonts. Apply once at startup with
//! `install_jp_font(&ctx)`.

use egui::{FontData, FontDefinitions, FontFamily};

const JP_FONT: &[u8] = include_bytes!("../assets/NotoSansJP-Reduced.ttf");

/// Install the bundled JP font alongside egui's default fonts. The font is
/// added as the highest-priority Proportional fallback so JP glyphs render
/// while ASCII keeps egui's default look.
pub fn install_jp_font(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    // egui 0.32: font_data values are Arc<FontData> so definitions can be
    // shared cheaply across contexts.
    fonts.font_data.insert(
        "noto_jp".into(),
        std::sync::Arc::new(FontData::from_static(JP_FONT)),
    );

    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "noto_jp".into());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .push("noto_jp".into());

    ctx.set_fonts(fonts);
}
