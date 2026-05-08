//! Phase 4 G3 — generate placeholder tray icons (16×16 + 32×32 PNGs)
//! at build time. G4 / G5 will replace these with proper artwork.

use std::path::Path;

fn main() {
    let assets_dir = Path::new("assets");
    if !assets_dir.exists() {
        std::fs::create_dir_all(assets_dir).expect("create assets dir");
    }

    write_solid_color(assets_dir, "tray-idle.png", [128, 128, 128, 255]); // gray
    write_solid_color(assets_dir, "tray-listening.png", [40, 180, 80, 255]); // green
    write_solid_color(assets_dir, "tray-error.png", [200, 60, 60, 255]); // red

    println!("cargo:rerun-if-changed=build.rs");

    // Note: Windows version resource embedding moved to crates/client/build.rs
    // (the prdt.exe bin) so that linking the gui-host lib into multiple bins
    // (prdt-host, prdt) doesn't produce CVT1100 duplicate-VERSION errors.
}

/// Write a 32×32 RGBA PNG of the given color to `assets/<name>`.
fn write_solid_color(dir: &Path, name: &str, rgba: [u8; 4]) {
    use image::{ImageBuffer, Rgba};
    let img = ImageBuffer::from_pixel(32, 32, Rgba(rgba));
    let path = dir.join(name);
    img.save(&path)
        .unwrap_or_else(|e| panic!("save {}: {e}", path.display()));
}
