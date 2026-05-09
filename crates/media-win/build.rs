use std::env;
use std::path::PathBuf;

fn main() {
    // Only run on Windows.
    let target = env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        return;
    }

    println!("cargo:rerun-if-env-changed=NV_CODEC_SDK_PATH");
    println!("cargo:rerun-if-env-changed=CUDA_PATH");
    println!("cargo:rerun-if-changed=build.rs");
    // Register the custom cfg we emit from generate_nvdec_bindings so the
    // unexpected_cfgs lint doesn't complain on consumers.
    println!("cargo::rustc-check-cfg=cfg(prdt_nvdec_bindings)");

    #[cfg(target_os = "windows")]
    {
        generate_nvenc_bindings();
        generate_nvdec_bindings();
    }
}

#[cfg(target_os = "windows")]
fn generate_nvenc_bindings() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let out_path = PathBuf::from(&out_dir).join("nvenc_bindings.rs");

    let sdk_path = match env::var("NV_CODEC_SDK_PATH") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            println!(
                "cargo:warning=NV_CODEC_SDK_PATH is not set; NVENC bindings will be empty stubs. \
                 Set to your NVIDIA Video Codec SDK root (e.g. C:\\SDK\\Video_Codec_SDK_12.2.72) \
                 and rebuild."
            );
            std::fs::write(
                &out_path,
                "// NVENC SDK not available; bindings are empty.\n",
            )
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
        .default_enum_style(bindgen::EnumVariation::Rust {
            non_exhaustive: false,
        })
        .derive_default(true)
        .derive_debug(false)
        .layout_tests(false)
        .generate()
        .expect("bindgen failed");

    bindings.write_to_file(&out_path).expect("write bindings");
}

/// Plan 2d: generate NVDEC (cuvid) bindings. Requires the NVIDIA Video
/// Codec SDK (`NV_CODEC_SDK_PATH`) for cuviddec.h + nvcuvid.h AND the CUDA
/// Toolkit (`CUDA_PATH`) for the cuda.h that cuviddec.h transitively
/// includes. When CUDA_PATH is unset we emit empty stubs and the nvdec
/// module compiles to a `NotAvailable` error path rather than failing
/// the build outright — the host/viewer still build against MF decode.
#[cfg(target_os = "windows")]
fn generate_nvdec_bindings() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let out_path = PathBuf::from(&out_dir).join("nvdec_bindings.rs");

    let sdk_path = match env::var("NV_CODEC_SDK_PATH") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            std::fs::write(
                &out_path,
                "// NVENC/NVDEC SDK not available; NVDEC bindings are empty.\n",
            )
            .unwrap();
            return;
        }
    };
    let cuda_path = match env::var("CUDA_PATH") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            println!(
                "cargo:warning=CUDA_PATH is not set; NVDEC (Plan 2d) bindings will be empty \
                 stubs. Install the CUDA Toolkit (2-3 GB) from https://developer.nvidia.com/\
                 cuda-downloads and set CUDA_PATH=C:\\Program Files\\NVIDIA GPU Computing \
                 Toolkit\\CUDA\\v13.x to enable the direct-NVDEC decode path."
            );
            std::fs::write(
                &out_path,
                "// CUDA Toolkit not available; NVDEC bindings are empty.\n",
            )
            .unwrap();
            return;
        }
    };

    let nvcuvid = sdk_path.join("Interface").join("nvcuvid.h");
    let cuviddec = sdk_path.join("Interface").join("cuviddec.h");
    let cuda_h = cuda_path.join("include").join("cuda.h");
    let cuda_d3d11 = cuda_path.join("include").join("cudaD3D11.h");
    for h in [&nvcuvid, &cuviddec, &cuda_h, &cuda_d3d11] {
        if !h.exists() {
            panic!("missing NVDEC dependency header: {}", h.display());
        }
        println!("cargo:rerun-if-changed={}", h.display());
    }

    // Bindgen needs merged bindings for nvcuvid + the CUDA-D3D11 interop
    // functions. cudaD3D11.h references ID3D11Device / IDXGIAdapter /
    // ID3D11Resource from the Windows SDK d3d11.h, which we don't want to
    // pull into clang here (it's a huge dependency and we treat D3D11
    // handles as opaque pointers at the CUDA boundary anyway). Forward-
    // declare the few types cudaD3D11.h references so bindgen sees them
    // as opaque structs. The Rust-side wrapper casts `*mut c_void` from
    // the `windows` crate's COM types.
    let umbrella = PathBuf::from(&out_dir).join("nvdec_umbrella.h");
    std::fs::write(
        &umbrella,
        "typedef struct ID3D11Device ID3D11Device;\n\
         typedef struct ID3D11Resource ID3D11Resource;\n\
         typedef struct IDXGIAdapter IDXGIAdapter;\n\
         #include <nvcuvid.h>\n\
         #include <cudaD3D11.h>\n",
    )
    .expect("write umbrella header");

    let bindings = bindgen::Builder::default()
        .header(umbrella.to_string_lossy())
        .clang_arg(format!("-I{}", sdk_path.join("Interface").display()))
        .clang_arg(format!("-I{}", cuda_path.join("include").display()))
        // nvcuvid/cuviddec types we actually use.
        .allowlist_type("CUVID.*")
        .allowlist_type("CUvideo.*")
        .allowlist_type("cudaVideo.*")
        .allowlist_function("cuvid.*")
        .allowlist_var("CUVID.*")
        // CUDA Driver API subset needed by cuvid function signatures.
        .allowlist_type("CU[a-z].*")
        .allowlist_type("CUresult")
        .allowlist_type("CUcontext")
        .allowlist_type("CUdevice")
        .allowlist_type("CUstream")
        .allowlist_type("CUdeviceptr")
        .allowlist_type("CUmemorytype")
        .allowlist_function("cuInit")
        .allowlist_function("cuDeviceGet")
        .allowlist_function("cuDeviceGetCount")
        .allowlist_function("cuCtx.*")
        .allowlist_function("cuStream.*")
        .allowlist_function("cuMemAlloc")
        .allowlist_function("cuMemFree")
        .allowlist_function("cuMemcpy.*")
        .allowlist_function("cuGraphicsD3D11.*")
        .allowlist_function("cuGraphics.*")
        .default_enum_style(bindgen::EnumVariation::Rust {
            non_exhaustive: false,
        })
        .derive_default(true)
        .derive_debug(false)
        .layout_tests(false)
        .generate()
        .expect("nvdec bindgen failed");

    bindings
        .write_to_file(&out_path)
        .expect("write nvdec bindings");

    // Tell cargo where to find the import libraries (nvcuvid.lib + cuda.lib)
    // so the NvdecD3d11Consumer can link when the feature path is taken.
    let sdk_lib = sdk_path.join("Lib").join("win").join("x64");
    let cuda_lib = cuda_path.join("lib").join("x64");
    println!("cargo:rustc-link-search=native={}", sdk_lib.display());
    println!("cargo:rustc-link-search=native={}", cuda_lib.display());
    // Link the CUDA Driver API + nvcuvid import libs so the Rust FFI
    // extern blocks resolve. These are implicit imports from the
    // bindgen-generated bindings; if we don't emit them, the linker
    // fails on `unresolved external symbol cuInit` etc.
    println!("cargo:rustc-link-lib=cuda");
    println!("cargo:rustc-link-lib=nvcuvid");
    // Tell Rust code the CUDA-capable code paths are compiled in.
    println!("cargo:rustc-cfg=prdt_nvdec_bindings");
}
