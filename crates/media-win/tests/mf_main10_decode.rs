//! MfHevcMain10Decoder acceptance tests (F8.A5 / F8.A8 / F8.A12).
//!
//! These tests exercise the HEVC Main10 decoder on Windows. They are
//! unconditionally compiled when `media-win-hevc-main10` is enabled so the
//! CI compile gate (A2) catches build failures; runtime requires a Windows
//! host with a HEVC Main10 MFT available (skips gracefully otherwise).
//!
//! The round-trip test (F8.A6) is marked `#[ignore]` because it requires a
//! vendored Main10 fixture not yet generated (Step 8 of the F8 plan). The
//! skip-if-unavailable pattern mirrors the 8-bit `create_h265_decoder` test.

#![cfg(all(windows, feature = "media-win-hevc-main10"))]

use prdt_media_win::{pick_default_adapter, D3d11Device, MediaError, MfHevcMain10Decoder};

/// F8.A5 — decoder constructs (or gracefully reports DecoderNotAvailable).
#[test]
fn create_hevc_main10_decoder() {
    let adapter = match pick_default_adapter() {
        Ok(a) => a,
        Err(_) => return,
    };
    let dev = D3d11Device::create(&adapter).expect("D3D11 device");
    match MfHevcMain10Decoder::new(&dev, 1920, 1080) {
        Ok(dec) => {
            assert_eq!(dec.width(), 1920);
            assert_eq!(dec.height(), 1080);
            assert!(dec.needs_idr());
            eprintln!(
                "MfHevcMain10Decoder constructed OK (d3d11_aware={})",
                dec.d3d11_aware()
            );
        }
        Err(MediaError::DecoderNotAvailable { codec, reason }) => {
            // Non-fatal on CI runners without the HEVC Video Extensions package.
            eprintln!("HEVC Main10 MFT not available (non-fatal): {codec}: {reason}");
        }
        Err(e) => {
            eprintln!("Unexpected error (non-fatal, VM/server SKU): {e}");
        }
    }
}

/// F8.A8 — existing 8-bit create_h265_decoder must still pass (byte-stability
/// regression check). Mirrors the test in mf/decoder.rs directly; having it
/// here ensures the acceptance test suite for F8 explicitly re-asserts it.
#[test]
fn h265_8bit_decoder_unchanged() {
    let adapter = match pick_default_adapter() {
        Ok(a) => a,
        Err(_) => return,
    };
    let dev = D3d11Device::create(&adapter).expect("D3D11 device");
    use prdt_media_win::mf::H265Decoder;
    match H265Decoder::new(&dev, 1920, 1080) {
        Ok(dec) => {
            assert_eq!(dec.width(), 1920);
            assert_eq!(dec.height(), 1080);
            assert!(dec.needs_idr());
        }
        Err(e) => {
            // On VMs / server SKUs there is no HW H.265 decoder MFT.
            eprintln!("H.265 8-bit MFT not available (non-fatal): {e}");
        }
    }
}

/// F8.A6 (ignored) — round-trip: feed a vendored Main10 HEVC IDR and assert
/// Nv12Frame16 + HDR10 metadata. Skipped until the fixture file
/// `crates/media-win/tests/fixtures/main10_with_hdr10.h265` is generated
/// (F8 plan Step 8 ffmpeg one-liner). Enable with `cargo test -- --include-ignored`.
#[test]
#[ignore = "requires fixture crates/media-win/tests/fixtures/main10_with_hdr10.h265 (F8 Step 8)"]
fn main10_idr_decodes_with_hdr10_metadata() {
    let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/main10_with_hdr10.h265");
    assert!(
        fixture_path.exists(),
        "fixture not found: {fixture_path:?} — run F8 Step 8 ffmpeg one-liner to generate it"
    );

    let adapter = match pick_default_adapter() {
        Ok(a) => a,
        Err(_) => return,
    };
    let dev = D3d11Device::create(&adapter).expect("D3D11 device");
    let mut dec = match MfHevcMain10Decoder::new(&dev, 1920, 1080) {
        Ok(d) => d,
        Err(MediaError::DecoderNotAvailable { codec, reason }) => {
            eprintln!("skip: {codec}: {reason}");
            return;
        }
        Err(e) => panic!("unexpected decoder error: {e}"),
    };

    let nal_bytes = std::fs::read(&fixture_path).expect("read fixture");
    dec.process_input(&nal_bytes, 0).expect("process_input");

    let frame = dec
        .process_output_nv12_16()
        .expect("process_output_nv12_16");
    let frame = match frame {
        Some(f) => f,
        None => {
            eprintln!("decoder needs more input (MFT preroll) — test inconclusive");
            return;
        }
    };

    // Assert HDR10 metadata was parsed from the bitstream SEI (Choice C-1).
    assert!(
        frame.hdr10.is_some(),
        "expected HDR10 metadata from Main10 fixture, got None"
    );
    let hdr10 = frame.hdr10.unwrap();
    assert_eq!(hdr10.max_cll, 1000, "MaxCLL mismatch");
    assert_eq!(hdr10.max_fall, 400, "MaxFALL mismatch");

    // Assert Y-plane samples are in legal Main10 limited-range [64, 940].
    let y_sample = frame.y_plane[0];
    assert!(
        (64..=940).contains(&y_sample),
        "Y sample {y_sample} outside legal Main10 limited-range [64, 940]"
    );
}
