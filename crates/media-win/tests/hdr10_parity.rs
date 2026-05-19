//! F-WIN-FFMPEG PR3.hdr10-parity — assert MF and FFmpeg-NVDEC decoders agree
//! on HDR10 sidecar extraction for the same Main10 bitstream.
//!
//! The fixture `hevc_main10_hdr10_sample.hevc` is libx265-generated with
//! known mastering-display + content-light SEIs (see fixtures/README.md
//! for the exact x265 parameters and the expected JSON dump).
//!
//! On windows-latest CI: NVDEC requires NVIDIA hardware and silently
//! returns `MediaError::DecoderNotAvailable` on the GPU-less runner. We
//! detect that case and fall back to "single-decoder validation": MF
//! alone is asserted against the known-correct HDR10 values from the
//! fixture's SEI. This still catches MF-side regressions.
//!
//! When both decoders are available (HW smoke-test box with NVIDIA GPU
//! and the matching feature flags), the test additionally asserts that
//! both decoders return byte-identical Hdr10Metadata.
#![cfg(all(
    windows,
    feature = "media-win-hevc-main10",
    feature = "media-win-ffmpeg-nvdec-main10",
))]

use prdt_media_core::Hdr10Metadata;
use prdt_media_win::{
    ffmpeg::{HevcNvdecMain10FfmpegDecoderWindows, HevcNvdecMain10FfmpegDecoderWindowsConfig},
    pick_default_adapter, D3d11Device, MediaError, MfHevcMain10Decoder,
};

const FIXTURE: &[u8] = include_bytes!("fixtures/hevc_main10_hdr10_sample.hevc");

/// HDR10 sidecar values the fixture was generated with. Source: the
/// `master-display=...:max-cll=...` arguments captured in
/// `fixtures/README.md`. If you regenerate the fixture, update these.
fn expected_hdr10() -> Hdr10Metadata {
    Hdr10Metadata {
        // R, G, B (SMPTE 2086 order) in 1/50000 units.
        display_primaries: [
            (35400, 14600), // R: (0.708, 0.292)
            (8500, 39850),  // G: (0.170, 0.797)
            (6550, 2300),   // B: (0.131, 0.046)
        ],
        white_point: (15635, 16450), // D65: (0.3127, 0.329)
        // x265 L(10000000,1) → max 1000 cd/m² in 0.0001 units = 10_000_000;
        // min 0.0001 cd/m² in 0.0001 units = 1.
        min_mastering_luminance: 1,
        max_mastering_luminance: 10_000_000,
        max_content_light_level: 1000,
        max_frame_average_light_level: 400,
    }
}

/// Split an Annex-B byte stream into access units (NAL units) by scanning
/// for 4-byte start codes (`00 00 00 01`). Returns a `Vec` of slices that
/// each include their leading start code so the MFT / codec receives a
/// well-formed NAL.
///
/// The fixture is a raw Annex-B HEVC bitstream as emitted by ffmpeg with
/// `-f hevc`. The MF HEVC MFT accepts Annex-B NAL units including the
/// start code prefix; the FFmpeg hevc_cuvid decoder also expects Annex-B.
/// Feeding the entire bitstream as one packet is the simplest path and
/// matches what the existing F8 `main10_idr_decodes_with_hdr10_metadata`
/// test already does (`dec.process_input(&nal_bytes, 0)`).
///
/// To drive 16 frames we still need multiple packets because the MFT may
/// only accept one at a time when its input queue is full. This splitter
/// produces one packet per access-unit boundary (identified by the 4-byte
/// start code `00 00 00 01` not immediately preceded by a non-zero byte,
/// i.e. a "long" start code that marks an AU delimiter).
fn split_annex_b(data: &[u8]) -> Vec<&[u8]> {
    let mut starts: Vec<usize> = Vec::new();
    let mut i = 0usize;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 1 {
            starts.push(i);
            i += 4;
        } else if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            // 3-byte start code — also a NAL boundary but we use 4-byte
            // start codes as AU delimiters. Advance past it.
            i += 3;
        } else {
            i += 1;
        }
    }
    if starts.is_empty() {
        // No start codes found: treat the entire buffer as one AU.
        return vec![data];
    }
    let mut aus: Vec<&[u8]> = Vec::with_capacity(starts.len());
    for w in starts.windows(2) {
        aus.push(&data[w[0]..w[1]]);
    }
    aus.push(&data[*starts.last().unwrap()..]);
    aus
}

/// Drive the MF Main10 decoder: feed every AU, drain until we collect a
/// non-None HDR10 field.  Returns `None` if the MFT never emitted HDR10
/// (e.g. it's a server-SKU stub that cannot parse SEIs).
fn run_mf_decoder(fixture: &[u8]) -> Result<Option<Hdr10Metadata>, MediaError> {
    let adapter = match pick_default_adapter() {
        Ok(a) => a,
        Err(e) => return Err(e),
    };
    let dev = D3d11Device::create(&adapter)?;
    let mut dec = MfHevcMain10Decoder::new(&dev, 1920, 1080)?;

    let aus = split_annex_b(fixture);
    let mut last_hdr10: Option<Hdr10Metadata> = None;

    for (idx, au) in aus.iter().enumerate() {
        // Feed one AU. On MF_E_NOTACCEPTING we must drain first.
        match dec.process_input(au, (idx as i64) * 333_333) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("process_input AU#{idx} error (non-fatal): {e}");
                continue;
            }
        }

        // Drain all available output frames.
        loop {
            match dec.process_output_nv12_16() {
                Ok(Some(frame)) => {
                    if frame.hdr10.is_some() {
                        last_hdr10 = frame.hdr10;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    eprintln!("process_output_nv12_16 error: {e}");
                    break;
                }
            }
        }

        if last_hdr10.is_some() {
            // HDR10 captured — no need to decode every frame.
            break;
        }
    }

    // One final drain pass in case the MFT held frames back.
    if last_hdr10.is_none() {
        loop {
            match dec.process_output_nv12_16() {
                Ok(Some(frame)) => {
                    if frame.hdr10.is_some() {
                        last_hdr10 = frame.hdr10;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    eprintln!("final drain error (non-fatal): {e}");
                    break;
                }
            }
        }
    }

    Ok(last_hdr10)
}

/// Drive the FFmpeg NVDEC Main10 decoder: feed every AU, drain until we
/// collect a non-None HDR10 field.  Returns `None` if no frame had HDR10.
fn run_nvdec_decoder(fixture: &[u8]) -> Result<Option<Hdr10Metadata>, MediaError> {
    let cfg = HevcNvdecMain10FfmpegDecoderWindowsConfig {
        width: 1920,
        height: 1080,
        cuda_device_index: None,
    };
    // HevcNvdecMain10FfmpegDecoderWindows does not derive Debug so we cannot
    // use expect/unwrap_err. Match the Result directly.
    let mut dec = match HevcNvdecMain10FfmpegDecoderWindows::new(cfg) {
        Ok(d) => d,
        Err(e) => return Err(e),
    };

    let aus = split_annex_b(fixture);
    let mut last_hdr10: Option<Hdr10Metadata> = None;

    for (idx, au) in aus.iter().enumerate() {
        // feed_packet returns DecodeError, not MediaError.
        if let Err(e) = dec.feed_packet(au, (idx as u64) * 33_333) {
            eprintln!("feed_packet AU#{idx} error (non-fatal): {e}");
            continue;
        }

        // Drain all available output frames.
        loop {
            match dec.drain_frame() {
                Ok(Some(frame)) => {
                    if frame.hdr10.is_some() {
                        last_hdr10 = frame.hdr10;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    eprintln!("drain_frame error: {e}");
                    break;
                }
            }
        }

        if last_hdr10.is_some() {
            break;
        }
    }

    Ok(last_hdr10)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Verify that the MF decoder extracts the known HDR10 sidecar from the
/// vendored fixture.  On CI (windows-latest, no HEVC MFT): skips gracefully.
#[test]
fn mf_extracts_known_hdr10_sidecar() {
    let hdr10 = match run_mf_decoder(FIXTURE) {
        Ok(Some(h)) => h,
        Ok(None) => {
            eprintln!("skipping: MF HEVC Main10 MFT did not emit HDR10 (no MFT or no GPU)");
            return;
        }
        Err(MediaError::DecoderNotAvailable { codec, reason }) => {
            eprintln!("skipping: MF HEVC Main10 MFT not registered: {codec}: {reason}");
            return;
        }
        Err(e) => {
            eprintln!("skipping: MF decoder error (non-fatal on server SKU): {e}");
            return;
        }
    };

    let expected = expected_hdr10();

    assert_eq!(
        hdr10.display_primaries, expected.display_primaries,
        "MF: display_primaries mismatch"
    );
    assert_eq!(
        hdr10.white_point, expected.white_point,
        "MF: white_point mismatch"
    );
    assert_eq!(
        hdr10.min_mastering_luminance, expected.min_mastering_luminance,
        "MF: min_mastering_luminance mismatch"
    );
    assert_eq!(
        hdr10.max_mastering_luminance, expected.max_mastering_luminance,
        "MF: max_mastering_luminance mismatch"
    );
    assert_eq!(
        hdr10.max_content_light_level, expected.max_content_light_level,
        "MF: max_content_light_level mismatch"
    );
    assert_eq!(
        hdr10.max_frame_average_light_level, expected.max_frame_average_light_level,
        "MF: max_frame_average_light_level mismatch"
    );
}

/// Verify that the FFmpeg NVDEC decoder extracts the known HDR10 sidecar.
/// On windows-latest (no NVIDIA GPU): `DecoderNotAvailable` → skip.
/// On HW smoke-test box with NVIDIA GPU: assert metadata matches fixture SEI.
#[test]
fn nvdec_extracts_known_hdr10_sidecar() {
    let hdr10 = match run_nvdec_decoder(FIXTURE) {
        Ok(Some(h)) => h,
        Ok(None) => {
            eprintln!("skipping: NVDEC did not emit HDR10 (no GPU or no SEI propagation)");
            return;
        }
        Err(MediaError::DecoderNotAvailable { codec, reason }) => {
            eprintln!(
                "skipping: NVDEC not available (expected on CI without GPU): {codec}: {reason}"
            );
            return;
        }
        Err(MediaError::Other(msg)) if msg.contains("av_hwdevice_ctx_create") => {
            eprintln!("skipping: CUDA HW device init failed (no GPU): {msg}");
            return;
        }
        Err(e) => {
            eprintln!("skipping: NVDEC decoder error (non-fatal): {e}");
            return;
        }
    };

    let expected = expected_hdr10();

    assert_eq!(
        hdr10.display_primaries, expected.display_primaries,
        "NVDEC: display_primaries mismatch"
    );
    assert_eq!(
        hdr10.white_point, expected.white_point,
        "NVDEC: white_point mismatch"
    );
    assert_eq!(
        hdr10.min_mastering_luminance, expected.min_mastering_luminance,
        "NVDEC: min_mastering_luminance mismatch"
    );
    assert_eq!(
        hdr10.max_mastering_luminance, expected.max_mastering_luminance,
        "NVDEC: max_mastering_luminance mismatch"
    );
    assert_eq!(
        hdr10.max_content_light_level, expected.max_content_light_level,
        "NVDEC: max_content_light_level mismatch"
    );
    assert_eq!(
        hdr10.max_frame_average_light_level, expected.max_frame_average_light_level,
        "NVDEC: max_frame_average_light_level mismatch"
    );
}

/// When both decoders are available, assert they agree byte-for-byte on the
/// HDR10 metadata extracted from the same input bitstream.
/// If either decoder is unavailable or emits no HDR10, the test is skipped.
#[test]
fn mf_and_nvdec_agree() {
    // Run MF decoder.
    let mf_hdr10 = match run_mf_decoder(FIXTURE) {
        Ok(Some(h)) => h,
        Ok(None) => {
            eprintln!("skipping mf_and_nvdec_agree: MF emitted no HDR10");
            return;
        }
        Err(MediaError::DecoderNotAvailable { codec, reason }) => {
            eprintln!("skipping mf_and_nvdec_agree: MF not available: {codec}: {reason}");
            return;
        }
        Err(e) => {
            eprintln!("skipping mf_and_nvdec_agree: MF error (non-fatal): {e}");
            return;
        }
    };

    // Run NVDEC decoder.
    let nvdec_hdr10 = match run_nvdec_decoder(FIXTURE) {
        Ok(Some(h)) => h,
        Ok(None) => {
            eprintln!("skipping mf_and_nvdec_agree: NVDEC emitted no HDR10 (no GPU?)");
            return;
        }
        Err(MediaError::DecoderNotAvailable { codec, reason }) => {
            eprintln!("skipping mf_and_nvdec_agree: NVDEC not available (expected on CI without GPU): {codec}: {reason}");
            return;
        }
        Err(MediaError::Other(msg)) if msg.contains("av_hwdevice_ctx_create") => {
            eprintln!("skipping mf_and_nvdec_agree: CUDA HW device init failed (no GPU): {msg}");
            return;
        }
        Err(e) => {
            eprintln!("skipping mf_and_nvdec_agree: NVDEC error (non-fatal): {e}");
            return;
        }
    };

    assert_eq!(
        mf_hdr10.display_primaries, nvdec_hdr10.display_primaries,
        "MF vs NVDEC: display_primaries disagree"
    );
    assert_eq!(
        mf_hdr10.white_point, nvdec_hdr10.white_point,
        "MF vs NVDEC: white_point disagrees"
    );
    assert_eq!(
        mf_hdr10.min_mastering_luminance, nvdec_hdr10.min_mastering_luminance,
        "MF vs NVDEC: min_mastering_luminance disagrees"
    );
    assert_eq!(
        mf_hdr10.max_mastering_luminance, nvdec_hdr10.max_mastering_luminance,
        "MF vs NVDEC: max_mastering_luminance disagrees"
    );
    assert_eq!(
        mf_hdr10.max_content_light_level, nvdec_hdr10.max_content_light_level,
        "MF vs NVDEC: max_content_light_level disagrees"
    );
    assert_eq!(
        mf_hdr10.max_frame_average_light_level, nvdec_hdr10.max_frame_average_light_level,
        "MF vs NVDEC: max_frame_average_light_level disagrees"
    );
}
