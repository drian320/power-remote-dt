//! Embed the Windows version resource + icon into prdt.exe.
//!
//! This was previously split across crates/gui-host/build.rs and
//! crates/gui-viewer/build.rs (each targeting their own bin). When the
//! unified `prdt` bin started linking both libs, CVTRES emitted CVT1100
//! "duplicate resource" because each lib brought its own VERSION block.
//! Now the resource lives in exactly one place: this bin.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(target_os = "windows")]
    {
        let icon = std::path::PathBuf::from("../gui-host/resources/prdt-icon.ico");
        let mut res = winres::WindowsResource::new();
        res.set("FileDescription", "Power Remote Desktop");
        res.set("ProductName", "Power Remote Desktop");
        if icon.exists() {
            res.set_icon(icon.to_str().expect("ascii icon path"));
        } else {
            println!(
                "cargo:warning=prdt: {} missing; building without icon",
                icon.display()
            );
        }
        if let Err(e) = res.compile() {
            println!("cargo:warning=winres compile failed: {e}");
        }
    }
}
