use std::env;
use std::path::PathBuf;

fn main() {
    // Mirror the cfg gate from prdt-media-win/build.rs so the viewer's
    // #[cfg(prdt_nvdec_bindings)] guards are known to rustc/clippy and
    // the unexpected_cfgs lint doesn't fire.
    println!("cargo::rustc-check-cfg=cfg(prdt_nvdec_bindings)");

    // Mirror prdt-media-win/build.rs: emit `prdt_nvdec_bindings` when both
    // NV_CODEC_SDK_PATH and CUDA_PATH point at usable directories. Without
    // this, viewer's `#[cfg(prdt_nvdec_bindings)]` guards always resolve
    // to the fallback branch — even though media-win itself has the
    // bindings — because cfgs from build.rs only apply to the crate that
    // emits them.
    println!("cargo:rerun-if-env-changed=NV_CODEC_SDK_PATH");
    println!("cargo:rerun-if-env-changed=CUDA_PATH");

    if !env::var("TARGET").unwrap_or_default().contains("windows") {
        return;
    }

    let sdk_ok = env::var("NV_CODEC_SDK_PATH")
        .ok()
        .map(PathBuf::from)
        .and_then(|p| p.join("Interface").join("nvcuvid.h").exists().then_some(()))
        .is_some();
    let cuda_ok = env::var("CUDA_PATH")
        .ok()
        .map(PathBuf::from)
        .and_then(|p| p.join("include").join("cuda.h").exists().then_some(()))
        .is_some();

    if sdk_ok && cuda_ok {
        println!("cargo:rustc-cfg=prdt_nvdec_bindings");
    }
}
