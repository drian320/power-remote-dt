fn main() {
    if std::env::var_os("FFMPEG_INCLUDE_DIR").is_none() {
        println!("cargo:rustc-env=FFMPEG_INCLUDE_DIR=/usr/include/x86_64-linux-gnu");
    }
    if std::env::var_os("FFMPEG_DLL_PATH").is_none() {
        println!("cargo:rustc-env=FFMPEG_DLL_PATH=/usr/lib/x86_64-linux-gnu/libavcodec.so");
    }
    println!("cargo:rerun-if-env-changed=FFMPEG_INCLUDE_DIR");
    println!("cargo:rerun-if-env-changed=FFMPEG_DLL_PATH");

    // rusty_ffmpeg only links -lavcodec via FFMPEG_DLL_PATH. The FFI calls in
    // this crate also use symbols from libavutil (av_buffer_unref, av_dict_*,
    // av_hwdevice_ctx_create, av_hwframe_ctx_*, av_frame_*, av_packet_*) and
    // libavformat. Link them explicitly here so the test binary resolves them.
    println!("cargo:rustc-link-lib=dylib=avutil");
    println!("cargo:rustc-link-lib=dylib=avformat");
    println!("cargo:rustc-link-search=native=/usr/lib/x86_64-linux-gnu");

    // P3 Main10 encoders (hevc_vaapi_main10_encoder.rs, hevc_nvenc_main10_encoder.rs)
    // call sws_getContext / sws_scale / sws_freeContext for the CPU-side BGRA8 →
    // P010LE conversion path. rusty_ffmpeg's FFMPEG_DLL_PATH only resolves
    // libavcodec; libswscale must be linked explicitly here. The Linux smoke
    // runner installs libswscale-dev as part of the FFmpeg dev packages, so
    // /usr/lib/x86_64-linux-gnu/libswscale.so is on the link search path above.
    if std::env::var("CARGO_FEATURE_FFMPEG_ENCODE_HEVC_VAAPI_MAIN10_ANY").is_ok()
        || std::env::var("CARGO_FEATURE_FFMPEG_ENCODE_HEVC_NVENC_MAIN10_ANY").is_ok()
    {
        println!("cargo:rustc-link-lib=dylib=swscale");
        println!("cargo:rerun-if-env-changed=CARGO_FEATURE_FFMPEG_ENCODE_HEVC_VAAPI_MAIN10_ANY");
        println!("cargo:rerun-if-env-changed=CARGO_FEATURE_FFMPEG_ENCODE_HEVC_NVENC_MAIN10_ANY");
    }

    // P2.5: NPP feature pulls in CUDA + NPP runtime libs. Step 0 Dockerfile
    // installs the official NVIDIA CUDA repo packages cuda-cudart-dev-12-4 +
    // libnpp-dev-12-4 at /usr/local/cuda-12.4/{lib64,include}. On non-CUDA
    // build hosts (CI ubuntu-latest without the NVIDIA repo), `cargo check`
    // is link-free and skips this; `cargo test` needs the libs or the stub
    // .so at tests/fixtures/libnppicc-stub.so via RUSTFLAGS='-L ...'.
    if std::env::var("CARGO_FEATURE_FFMPEG_ENCODE_HEVC_NVENC_NPP_ANY").is_ok() {
        println!("cargo:rustc-link-search=native=/usr/local/cuda-12.4/lib64");
        println!("cargo:rustc-link-search=native=/usr/local/cuda/lib64");
        println!("cargo:rustc-link-lib=dylib=cudart");
        println!("cargo:rustc-link-lib=dylib=nppicc");
        println!("cargo:rerun-if-env-changed=CARGO_FEATURE_FFMPEG_ENCODE_HEVC_NVENC_NPP_ANY");
    }
}
