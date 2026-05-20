//! QR code generation for displaying host pubkey/host_id strings.

use egui::ColorImage;
use qrcode::{Color, QrCode};

#[derive(thiserror::Error, Debug)]
pub enum QrError {
    #[error("qrcode: {0}")]
    QrCode(#[from] qrcode::types::QrError),
}

/// Render `text` as a black-on-white QR code at integer pixel `scale`.
/// Returns an `egui::ColorImage` of size `(modules*scale)x(modules*scale)`.
pub fn generate(text: &str, scale: usize) -> Result<ColorImage, QrError> {
    let code = QrCode::new(text.as_bytes())?;
    let modules = code.width();
    let pixel_w = modules * scale;
    let mut pixels = vec![egui::Color32::WHITE; pixel_w * pixel_w];
    let bools: Vec<bool> = code
        .to_colors()
        .into_iter()
        .map(|c| c == Color::Dark)
        .collect();
    for my in 0..modules {
        for mx in 0..modules {
            if bools[my * modules + mx] {
                for sy in 0..scale {
                    for sx in 0..scale {
                        let px = mx * scale + sx;
                        let py = my * scale + sy;
                        pixels[py * pixel_w + px] = egui::Color32::BLACK;
                    }
                }
            }
        }
    }
    Ok(ColorImage {
        size: [pixel_w, pixel_w],
        // egui 0.32: ColorImage tracks the original (pre-scale) source size
        // for DPI-aware sampling. Our QR pixels ARE the final raster, so
        // source_size == size.
        source_size: egui::Vec2::new(pixel_w as f32, pixel_w as f32),
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonempty_input_produces_image() {
        let img = generate("hello", 4).unwrap();
        assert!(img.size[0] >= 4 * 21);
        assert_eq!(img.size[0], img.size[1]);
    }

    #[test]
    fn larger_payload_grows() {
        let small = generate("a", 2).unwrap();
        let large = generate(&"a".repeat(100), 2).unwrap();
        assert!(large.size[0] > small.size[0]);
    }
}
