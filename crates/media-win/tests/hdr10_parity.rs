// hdr10_parity.rs — stub for the PR3 MF-vs-FFmpeg NVDEC HDR10 metadata parity test.
//
// TODO(PR3-followup): implement the full parity test once the vendored fixture
// is committed. The test should:
//   1. Assert the fixture has both "Mastering display metadata" and
//      "Content light level metadata" in frame 0 side_data (via ffprobe).
//   2. Decode 16 frames with `MfHevcMain10Consumer` and collect per-frame
//      `Hdr10Metadata`.
//   3. Decode the same 16 frames with `HevcNvdecMain10FfmpegDecoderWindows`
//      and collect per-frame `Hdr10Metadata`.
//   4. Assert both outputs agree within a small tolerance on all fields.
//
// Cfg gate: only runs when both MF Main10 and FFmpeg NVDEC Main10 are compiled.
// The test is intentionally left as a no-op stub so CI does not gate on it
// until the fixture and implementation are ready.
//
// See plan .omc/plans/2026-05-18-windows-ffmpeg-coexist-mf.md §PR3 item 25.
#![cfg(all(
    windows,
    feature = "media-win-hevc-main10",
    feature = "media-win-ffmpeg-nvdec-main10"
))]

#[test]
#[ignore = "PR3-followup: fixture not yet committed; implement after parity fixture is vendored"]
fn hdr10_metadata_mf_vs_ffmpeg_nvdec_parity() {
    // Placeholder — see module-level TODO comment.
}
