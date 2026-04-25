//! Phase 4 G4 — embed Windows version resource + icon into prdt-viewer-overlay.exe.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(target_os = "windows")]
    {
        let icon = std::path::PathBuf::from("../gui-host/resources/prdt-icon.ico");
        let mut res = winres::WindowsResource::new();
        res.set("FileDescription", "Power Remote Desktop (Overlay)");
        res.set("ProductName", "Power Remote Desktop");
        if icon.exists() {
            res.set_icon(icon.to_str().expect("ascii icon path"));
        } else {
            println!(
                "cargo:warning=prdt-viewer-overlay: {} missing; building without icon",
                icon.display()
            );
        }
        if let Err(e) = res.compile() {
            println!("cargo:warning=winres compile failed: {e}");
        }
    }
}
