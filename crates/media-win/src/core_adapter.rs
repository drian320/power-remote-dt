//! Adapter shim: implements `prdt_media_core::Encoder` (cross-platform
//! trait) on top of the existing `Hevc265Encoder` / `HwHevcEncoder`
//! Windows-specific traits.
//!
//! L0 only — host / viewer code is not yet rewired to consume the
//! `prdt_media_core::Encoder` trait. This module exists so the trait
//! surface is exercised on Windows (smoke test below) and so the L1
//! Linux work has a precedent to mirror.

use prdt_media_core::{EncodeError, EncodedPacket, Encoder};

use crate::d3d11::D3d11Texture;
use crate::encoder_trait::{EncodedH265Frame, Hevc265Encoder, HwHevcEncoder};
use crate::error::MediaError;

impl Encoder for HwHevcEncoder {
    type Frame = D3d11Texture;

    fn encode(
        &mut self,
        frame: &Self::Frame,
        force_idr: bool,
        timestamp_us: u64,
    ) -> Result<EncodedPacket, EncodeError> {
        <HwHevcEncoder as Hevc265Encoder>::encode(self, frame, force_idr, timestamp_us)
            .map(into_packet)
            .map_err(map_err)
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        <HwHevcEncoder as Hevc265Encoder>::set_target_bitrate(self, bps);
    }

    fn backend_name(&self) -> &'static str {
        <HwHevcEncoder as Hevc265Encoder>::backend_name(self)
    }
}

fn into_packet(frame: EncodedH265Frame) -> EncodedPacket {
    EncodedPacket {
        nal_bytes: frame.nal_bytes,
        is_keyframe: frame.is_keyframe,
        timestamp_us: frame.timestamp,
    }
}

fn map_err(err: MediaError) -> EncodeError {
    match &err {
        // MediaError::UnsupportedFormat is the one media-win variant
        // that semantically matches EncodeError::FormatMismatch.
        // All other variants (including DeviceRemoved, which is
        // recoverable-but-fatal) currently collapse to Backend(...)
        // because EncodeError has no dedicated DeviceLost variant.
        // A follow-up task should add EncodeError::DeviceLost so the
        // L1 host wiring can distinguish "recreate device" from
        // "transient encode failure".
        MediaError::UnsupportedFormat { .. } => {
            EncodeError::FormatMismatch(err.to_string())
        }
        _ => EncodeError::Backend(err.to_string()),
    }
}
