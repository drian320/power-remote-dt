//! One-shot helper: convert the green "tray-listening" PNG (G3 build
//! artifact) into a multi-resolution Windows ICO file used by:
//! - winres (build.rs) on the three GUI binaries
//! - WiX MSI's ARPPRODUCTICON
//!
//! Run from the repo root:
//!     cargo run -p prdt-gui-host --bin mkicon
//!
//! Output: crates/gui-host/resources/prdt-icon.ico

use std::path::Path;

fn main() {
    let src = Path::new("crates/gui-host/assets/tray-listening.png");
    let img = image::open(src).expect("read source PNG");
    let rgba = img.to_rgba8();

    let sizes = [16u32, 32, 48, 64, 128, 256];
    let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);
    for &size in &sizes {
        let resized = image::imageops::resize(
            &rgba,
            size,
            size,
            image::imageops::FilterType::Lanczos3,
        );
        let entry = ico::IconImage::from_rgba_data(size, size, resized.into_raw());
        let encoded = ico::IconDirEntry::encode(&entry).expect("ICO encode");
        icon_dir.add_entry(encoded);
    }
    let out = Path::new("crates/gui-host/resources/prdt-icon.ico");
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).expect("create parent dir");
    }
    let file = std::fs::File::create(out).expect("create out file");
    icon_dir.write(file).expect("write ICO");
    eprintln!("wrote {}", out.display());
}
