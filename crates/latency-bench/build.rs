fn main() {
    // Mirror the cfg gate from prdt-media-win/build.rs so the bench's
    // #[cfg(prdt_nvdec_bindings)] guards are known to rustc/clippy and
    // the unexpected_cfgs lint doesn't fire.
    println!("cargo::rustc-check-cfg=cfg(prdt_nvdec_bindings)");
}
