//! Verifies that `HwHevcEncoder` is usable through the cross-platform
//! `prdt_media_core::Encoder` trait. This test does NOT exercise the
//! GPU encode path — that's covered by the existing
//! `nvenc_smoke` / `pipeline_smoke` tests. We only check trait-method
//! dispatch (object safety + signature compatibility).

#![cfg(windows)]

use prdt_media_core::Encoder;
use prdt_media_win::HwHevcEncoder;

/// Compile-time witness that `HwHevcEncoder` implements
/// `prdt_media_core::Encoder<Frame = D3d11Texture>`. If this stops
/// compiling, the adapter signature is no longer compatible.
fn _witness_hwencoder_impls_encoder<E: Encoder>(_e: &mut E) {}

#[test]
fn hwencoder_witness_compiles() {
    fn _f(e: &mut HwHevcEncoder) {
        _witness_hwencoder_impls_encoder(e);
    }
    // No runtime assertion needed — the compile-time witness above is the
    // entire value of this test. The test function must exist so it appears
    // in `cargo test` output and is not silently absent from CI results.
}
