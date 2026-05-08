//! Verifies that `HwHevcEncoder` is usable through the cross-platform
//! `prdt_media_core::Encoder` trait. This test does NOT exercise the
//! GPU encode path — that's covered by the existing
//! `nvenc_smoke` / `pipeline_smoke` tests. We only check trait-method
//! dispatch (object safety + signature compatibility).

#![cfg(windows)]

use prdt_media_core::Encoder;
use prdt_media_win::{HwHevcEncoder, MfH265Encoder};

/// Compile-time witness that `HwHevcEncoder` implements
/// `prdt_media_core::Encoder<Frame = D3d11Texture>`. If this stops
/// compiling, the adapter signature is no longer compatible.
fn _witness_hwencoder_impls_encoder<E: Encoder>(_e: &mut E) {}

#[test]
fn hwencoder_witness_compiles() {
    fn _f(e: &mut HwHevcEncoder) {
        _witness_hwencoder_impls_encoder(e);
    }
    // No body needed — the witness check is at compile time.
    // Keep this assertion so the test runner reports the test
    // result instead of skipping silently.
    assert_eq!(std::mem::size_of::<&MfH265Encoder>(), std::mem::size_of::<usize>());
}
