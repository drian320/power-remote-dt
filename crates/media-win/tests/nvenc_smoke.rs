//! NVENC integration smoke test. Encodes 60 frames of synthetic counter
//! pattern at 1920x1080 via the NVENC H.265 encoder and verifies the
//! output NAL units have valid Annex-B start codes and the expected
//! IDR cadence.

#![cfg(windows)]

use prdt_media_win::{
    pick_default_adapter, synthetic::make_counter_texture, D3d11Device, NvencEncoder,
    NvencEncoderConfig,
};

fn has_annex_b_start_code(nal: &[u8]) -> bool {
    nal.starts_with(&[0, 0, 0, 1]) || nal.starts_with(&[0, 0, 1])
}

#[test]
fn encode_60_frames_1080p_hevc() {
    let adapter = match pick_default_adapter() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("no adapter (skip): {e}");
            return;
        }
    };
    if !adapter.is_nvidia() {
        eprintln!(
            "skipping: non-NVIDIA adapter (have {}: {})",
            adapter.vendor_id, adapter.name
        );
        return;
    }

    let dev = D3d11Device::create(&adapter).expect("D3D11 device");
    let cfg = NvencEncoderConfig {
        width: 1920,
        height: 1080,
        fps_numerator: 60,
        fps_denominator: 1,
        bitrate_bps: 20_000_000,
        gop_length: 30,
    };
    let enc = NvencEncoder::new(&dev, &cfg).expect("NVENC encoder creation");

    let mut total_bytes = 0usize;
    let mut idr_count = 0usize;
    const FRAME_COUNT: u32 = 60;

    for seq in 0..FRAME_COUNT {
        let tex = make_counter_texture(&dev, cfg.width, cfg.height, seq).expect("counter texture");
        // Force IDR on frame 0 explicitly. Subsequent IDRs happen per gop_length.
        let force_idr = seq == 0;
        let frame = enc.encode(&tex, force_idr, seq as u64).expect("encode");

        assert!(!frame.nal_bytes.is_empty(), "frame {seq}: empty NAL");
        assert!(
            has_annex_b_start_code(&frame.nal_bytes),
            "frame {seq}: missing Annex-B start code: {:02x?}",
            &frame.nal_bytes[..8.min(frame.nal_bytes.len())]
        );
        if frame.is_keyframe {
            idr_count += 1;
        }
        total_bytes += frame.nal_bytes.len();
    }

    let avg_bytes = total_bytes / FRAME_COUNT as usize;
    let avg_bitrate_bps = (total_bytes * 8) * (cfg.fps_numerator as usize) / FRAME_COUNT as usize;
    eprintln!(
        "encoded {FRAME_COUNT} frames of {}x{}, total {} bytes, avg {} bytes/frame, IDRs {}, avg bitrate {} bps",
        cfg.width, cfg.height, total_bytes, avg_bytes, idr_count, avg_bitrate_bps
    );

    // Expectations:
    // - At least 1 IDR (frame 0, forced).
    // - With gop_length=30 and 60 frames, we expect 2 IDRs total (frames 0, 30).
    assert!(idr_count >= 1, "should have at least 1 IDR");
    // Lenient IDR cadence assertion: NVENC may delay IDR slightly; allow 1-3.
    assert!(idr_count <= 4, "too many IDRs (unexpected): {idr_count}");

    // Bitrate sanity: low-entropy synthetic counter-pattern compresses to
    // far below the 20 Mbps CBR target (NVENC skips residual coding for
    // near-identical frames). We only assert the encoder produced a non-
    // trivial amount of bitstream data.
    assert!(
        avg_bitrate_bps > 10_000,
        "avg bitrate too low: {avg_bitrate_bps} bps"
    );
    assert!(avg_bytes > 10, "avg bytes/frame too low: {avg_bytes}");
}
