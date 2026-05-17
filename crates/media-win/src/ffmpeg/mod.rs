//! Windows FFmpeg coexistence path (`media-win-ffmpeg` feature).
//!
//! PR1: skeleton module tree only. Encoder and decoder bodies land in PR2 / PR3.
//! This file exists so `cargo check --features media-win-ffmpeg` proves the
//! build.rs env-var wiring and rusty_ffmpeg dep resolve correctly before any
//! encoder/decoder code depends on them.
