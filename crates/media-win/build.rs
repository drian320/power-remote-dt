use std::env;
use std::path::PathBuf;

fn main() {
    // Only run on Windows.
    let target = env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        return;
    }

    println!("cargo:rerun-if-env-changed=NV_CODEC_SDK_PATH");
    println!("cargo:rerun-if-changed=build.rs");

    let sdk_path = match env::var("NV_CODEC_SDK_PATH") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            println!(
                "cargo:warning=NV_CODEC_SDK_PATH is not set; NVENC bindings will be empty stubs. \
                 Set to your NVIDIA Video Codec SDK root (e.g. C:\\SDK\\Video_Codec_SDK_12.2.72) \
                 and rebuild."
            );
            let out_dir = env::var("OUT_DIR").unwrap();
            let out_path = PathBuf::from(out_dir).join("nvenc_bindings.rs");
            std::fs::write(&out_path, "// NVENC SDK not available; bindings are empty.\n")
                .unwrap();
            return;
        }
    };

    let header = sdk_path.join("Interface").join("nvEncodeAPI.h");
    if !header.exists() {
        panic!(
            "NV_CODEC_SDK_PATH={} does not contain Interface/nvEncodeAPI.h",
            sdk_path.display()
        );
    }

    println!("cargo:rerun-if-changed={}", header.display());

    let bindings = bindgen::Builder::default()
        .header(header.to_string_lossy())
        .clang_arg(format!("-I{}", sdk_path.join("Interface").display()))
        .allowlist_type("NV_ENC.*")
        .allowlist_type("NVENC.*")
        .allowlist_type("GUID")
        .allowlist_function("NvEncodeAPICreateInstance")
        .allowlist_function("NvEncodeAPIGetMaxSupportedVersion")
        .allowlist_var("NV_ENC.*")
        .allowlist_var("NVENC.*")
        .default_enum_style(bindgen::EnumVariation::Rust { non_exhaustive: false })
        .derive_default(true)
        .derive_debug(false)
        .layout_tests(false)
        .generate()
        .expect("bindgen failed");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("nvenc_bindings.rs");
    bindings
        .write_to_file(&out_path)
        .expect("write bindings");
}
