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

    // Phase 4 G4: embed Windows version resource + icon into prdt-host.exe.
    #[cfg(target_os = "windows")]
    {
        let icon = std::path::Path::new("resources/prdt-icon.ico");
        let mut res = winres::WindowsResource::new();
        res.set("FileDescription", "Power Remote Desktop (Host)");
        res.set("ProductName", "Power Remote Desktop");
        if icon.exists() {
            res.set_icon(icon.to_str().expect("ascii icon path"));
        } else {
            println!(
                "cargo:warning=prdt-host: {} missing; building without icon (Task 5 generates it)",
                icon.display()
            );
        }
        if let Err(e) = res.compile() {
            println!("cargo:warning=winres compile failed: {e}");
        }
    }
}

/// Write a 32×32 RGBA PNG of the given color to `assets/<name>`.
fn write_solid_color(dir: &Path, name: &str, rgba: [u8; 4]) {
    use image::{ImageBuffer, Rgba};
    let img = ImageBuffer::from_pixel(32, 32, Rgba(rgba));
    let path = dir.join(name);
    img.save(&path)
        .unwrap_or_else(|e| panic!("save {}: {e}", path.display()));
}
