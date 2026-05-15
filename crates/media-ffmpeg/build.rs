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
}
