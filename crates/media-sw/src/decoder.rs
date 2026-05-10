//! OpenH264-backed software H.264 decoder. Returns I420 frames so the
//! consumer side can either present directly (Linux / future) or
//! convert to NV12 via `i420_to_nv12` for upload into a D3D11 NV12
//! texture (the chosen renderer reuse path — see plan §Phase 3).

use openh264::decoder::{Decoder, DecoderConfig};
use openh264::formats::YUVSource;
use openh264::OpenH264API;

use crate::error::{MediaSwError, Result};
use crate::nv12::I420Frame;
use crate::traits::SwH264Decoder;

pub struct Openh264Decoder {
    inner: Decoder,
}

impl Openh264Decoder {
    pub fn new() -> Result<Self> {
        let api = OpenH264API::from_source();
        let cfg = DecoderConfig::new();
        let inner = Decoder::with_api_config(api, cfg)
            .map_err(|e| MediaSwError::openh264("Decoder::with_api_config", e))?;
        Ok(Self { inner })
    }
}

impl SwH264Decoder for Openh264Decoder {
    fn decode(&mut self, nal_units: &[u8]) -> std::result::Result<Option<I420Frame>, MediaSwError> {
        // OpenH264's Decoder::decode accepts the entire access unit at
        // once; we don't need to split into individual NALs.
        let yuv = match self
            .inner
            .decode(nal_units)
            .map_err(|e| MediaSwError::openh264("Decoder::decode", e))?
        {
            Some(v) => v,
            None => return Ok(None),
        };

        let (w, h) = yuv.dimensions();
        let strides = yuv.strides();

        // Copy out into owned buffers so the caller can keep the frame
        // past the next decode call. The decoder returns a borrowed
        // view into its internal buffer, which is invalidated on the
        // next call.
        let y = yuv.y().to_vec();
        let u = yuv.u().to_vec();
        let v = yuv.v().to_vec();

        Ok(Some(I420Frame {
            width: w as u32,
            height: h as u32,
            y,
            u,
            v,
            stride_y: strides.0 as u32,
            stride_uv: strides.1 as u32,
        }))
    }

    fn backend_name(&self) -> &'static str {
        "openh264"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::{Openh264Encoder, Openh264EncoderConfig};
    use crate::nv12::I420Frame;
    use crate::traits::SwH264Encoder;

    fn make_test_frame(width: u32, height: u32) -> I420Frame {
        let mut f = I420Frame::new_packed(width, height).unwrap();
        // Simple horizontal gradient on Y so encode/decode produces
        // structured-but-cheap content the encoder can compress.
        let stride_y = f.stride_y as usize;
        for row in 0..(height as usize) {
            for col in 0..(width as usize) {
                f.y[row * stride_y + col] = ((col + row) & 0xFF) as u8;
            }
        }
        for b in f.u.iter_mut() {
            *b = 128;
        }
        for b in f.v.iter_mut() {
            *b = 128;
        }
        f
    }

    #[test]
    fn openh264_decoder_accepts_self_encoded_stream() {
        let w = 320u32;
        let h = 240u32;
        let cfg = Openh264EncoderConfig {
            width: w,
            height: h,
            target_bitrate_bps: 1_000_000,
            max_fps: 30.0,
        };
        let mut enc = Openh264Encoder::new(cfg).unwrap();
        let mut dec = Openh264Decoder::new().unwrap();

        // Push a few frames so the decoder has time to produce output.
        // The first IDR usually decodes synchronously on OpenH264, but
        // be tolerant of one frame of latency.
        let mut decoded: Option<I420Frame> = None;
        for i in 0..3u64 {
            let frame = make_test_frame(w, h);
            let ef = enc.encode(&frame, i == 0, i * 33_000).expect("encode");
            if let Some(f) = dec.decode(&ef.nal_units).expect("decode") {
                decoded = Some(f);
                break;
            }
        }

        let f = decoded.expect("decoder produced no frame after 3 inputs");
        assert_eq!(f.width, w);
        assert_eq!(f.height, h);
        assert_eq!(f.y.len(), (f.stride_y as usize) * (h as usize));
        // U and V planes are quarter-area
        assert_eq!(f.u.len(), (f.stride_uv as usize) * (h as usize / 2));
        assert_eq!(f.v.len(), (f.stride_uv as usize) * (h as usize / 2));
    }
}
