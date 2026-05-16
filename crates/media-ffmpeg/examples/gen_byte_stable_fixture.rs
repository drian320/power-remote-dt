//! Golden-fixture generator for the A13 byte-stability test.
//!
//! Encodes a deterministic 30-frame I420 sequence through
//! `HevcNvencFfmpegEncoder` (fixed config: 320×240, fps=30, bitrate=4 Mbps,
//! gop=30) and writes the concatenated `EncodedPacket.nal_bytes` blobs to
//! the path passed on the command line.
//!
//! Per P2.5 plan §3 (iter-3 M2), this example must be run **once**, on real
//! NVIDIA hardware, against commit 56a81fc (current master, pre-R6 refactor),
//! to produce the golden fixture file
//! `crates/media-ffmpeg/tests/fixtures/byte_stable_nvenc_h265.bin`. The
//! fixture is then committed in this PR, and the
//! `byte_stable_against_master_fixture` test asserts post-R6 encoder output
//! is byte-equal to the committed golden.
//!
//! The deterministic I420 input is generated in-memory (no separate fixture
//! file needed), so the only blob committed is the encoded golden output.
//!
//! NOTE: encoder output is deterministic given identical SDK + driver +
//! config, but only reproducible on the same minor SDK family. If the
//! smoke-runner driver/SDK changes materially, a new fixture must be
//! re-generated and committed alongside the driver/SDK floor bump.
//!
//! Invocation (one-time author task, on smoke runner with real NVENC):
//!
//! ```sh
//! git checkout 56a81fc   # pre-R6 master
//! ./scripts/dev-container.sh bash -c \
//!   'cargo run -p prdt-media-ffmpeg \
//!      --features ffmpeg-encode-hevc-nvenc-ffmpeg5 \
//!      --example gen_byte_stable_fixture \
//!      --target x86_64-unknown-linux-gnu \
//!      -- crates/media-ffmpeg/tests/fixtures/byte_stable_nvenc_h265.bin'
//! ```
//!
//! Then commit `byte_stable_nvenc_h265.bin` and proceed with the R6 refactor.

#![cfg(all(feature = "ffmpeg-encode-hevc-nvenc-any", target_os = "linux"))]

use std::env;
use std::fs;
use std::process::ExitCode;

use prdt_media_ffmpeg::{HevcNvencFfmpegEncoder, HevcNvencFfmpegEncoderConfig};
use prdt_media_sw::I420Frame;

const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;
const FPS: u32 = 30;
const BITRATE: u32 = 4_000_000;
const GOP: u32 = 30;
const N_FRAMES: u32 = 30;

/// Generate a deterministic I420 frame for `frame_idx`. Y plane is a
/// 256-wide gradient that shifts by `frame_idx` per frame so consecutive
/// frames differ at the bytestream level. UV planes are filled with the
/// neutral 128.
fn generate_frame(frame_idx: u32) -> I420Frame {
    let mut frame = I420Frame::new_packed(WIDTH, HEIGHT).expect("frame alloc");
    let w = WIDTH as usize;
    let h = HEIGHT as usize;
    let shift = (frame_idx & 0xff) as u8;
    for y in 0..h {
        for x in 0..w {
            frame.y[y * w + x] = ((x as u8).wrapping_add(y as u8)).wrapping_add(shift);
        }
    }
    let cw = w / 2;
    let ch = h / 2;
    for i in 0..(cw * ch) {
        frame.u[i] = 128;
        frame.v[i] = 128;
    }
    frame
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        eprintln!(
            "usage: {} <output-path>",
            args.first()
                .map(String::as_str)
                .unwrap_or("gen_byte_stable_fixture")
        );
        return ExitCode::from(2);
    }
    let out_path = &args[1];

    let cfg = HevcNvencFfmpegEncoderConfig {
        width: WIDTH,
        height: HEIGHT,
        fps: FPS,
        initial_bitrate_bps: BITRATE,
        gop_size: GOP,
        cuda_device_index: None,
    };
    let mut enc = match HevcNvencFfmpegEncoder::new(cfg) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("encoder init failed: {e}");
            return ExitCode::from(1);
        }
    };

    let mut blob = Vec::new();
    for i in 0..N_FRAMES {
        let frame = generate_frame(i);
        let force_idr = i == 0;
        let ts_us = (i as u64) * 1_000_000 / (FPS as u64);
        match enc.encode(&frame, force_idr, ts_us) {
            Ok(pkt) => blob.extend_from_slice(&pkt.nal_bytes),
            Err(e) => {
                eprintln!("encode frame {i} failed: {e}");
                return ExitCode::from(1);
            }
        }
    }

    if let Err(e) = fs::write(out_path, &blob) {
        eprintln!("write {out_path} failed: {e}");
        return ExitCode::from(1);
    }
    eprintln!("wrote {} bytes to {out_path}", blob.len());
    ExitCode::SUCCESS
}
