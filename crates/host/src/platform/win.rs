//! Windows host backend. Receives encoder dispatch + DXGI SW producer
//! contents (Task 3) + cross-platform factory wrappers (Task 4).
//! All items are `cfg(windows)`-gated at the file level.

#![cfg(windows)]

// === Moved from encoder_dispatch.rs ===

use std::time::Duration;

use anyhow::Context as _;
use prdt_media_sw::{bgra_to_i420, Openh264Encoder, Openh264EncoderConfig, SwH264Encoder};
use prdt_media_win::{
    AcquiredFrame, D3d11Device, D3d11Texture, DesktopDuplication, HwHevcEncoder, MediaError,
    OutputInfo, TextureFormat,
};
use prdt_protocol::{now_monotonic_us, EncodedFrame, ProducerError, VideoProducer};

/// Runtime-dispatched video encoder used to construct the right producer
/// in `run_host`. Phase 4 will use the `is_h264()` discriminator to fork
/// producer construction; Phase 2 just wires up the type.
pub enum VideoEncoderBackend {
    Hw(HwHevcEncoder),
    SwH264(Box<Openh264Encoder>),
}

impl VideoEncoderBackend {
    pub fn backend_name(&self) -> &'static str {
        match self {
            Self::Hw(e) => e.backend_name(),
            Self::SwH264(_) => "openh264",
        }
    }

    #[allow(dead_code)]
    pub fn is_h264(&self) -> bool {
        matches!(self, Self::SwH264(_))
    }

    /// Best-effort target-bitrate update. For OpenH264 the new value is
    /// stashed in cfg and takes effect on encoder reinit (see media-sw
    /// `Openh264Encoder::set_target_bitrate` doc).
    #[allow(dead_code)]
    pub fn set_target_bitrate(&mut self, bps: u32) {
        match self {
            Self::Hw(e) => e.set_target_bitrate(bps),
            Self::SwH264(e) => {
                e.set_target_bitrate(bps);
            }
        }
    }
}

#[cfg(test)]
mod encoder_dispatch_tests {
    use super::*;

    #[test]
    fn sw_backend_name_is_openh264() {
        let cfg = Openh264EncoderConfig {
            width: 320,
            height: 240,
            target_bitrate_bps: 1_000_000,
            max_fps: 30.0,
        };
        let enc = Openh264Encoder::new(cfg).expect("encoder");
        let backend = VideoEncoderBackend::SwH264(Box::new(enc));
        assert_eq!(backend.backend_name(), "openh264");
        assert!(backend.is_h264());
    }
}

// === Moved from dxgi_sw_producer.rs ===

pub struct DxgiSwProducer {
    dev: D3d11Device,
    output: OutputInfo,
    dup: DesktopDuplication,
    /// Owned by the producer for the loop's lifetime. `take()` + restore
    /// pattern lets us move the encoder into `spawn_blocking` and back
    /// without an `Arc<Mutex<>>` around the hot path.
    encoder: Option<Openh264Encoder>,
    staging: D3d11Texture,
    seq: u64,
    idr_pending: bool,
    width: u32,
    height: u32,
}

impl DxgiSwProducer {
    /// Create a producer for the given monitor with a pre-built encoder.
    /// Mirrors `DxgiNvencProducer::with_encoder` so the host main fn can
    /// fork on `VideoEncoderBackend` without producer-vendor branching.
    pub fn with_encoder(
        dev: &D3d11Device,
        output: &OutputInfo,
        encoder: Openh264Encoder,
    ) -> anyhow::Result<Self> {
        let dup = DesktopDuplication::new(dev, output).context("DesktopDuplication::new")?;
        let width = dup.width();
        let height = dup.height();
        let staging = D3d11Texture::new_staging(dev, width, height, TextureFormat::Bgra8)
            .context("staging texture")?;
        Ok(Self {
            dev: dev.clone(),
            output: output.clone(),
            dup,
            encoder: Some(encoder),
            staging,
            seq: 0,
            idr_pending: true,
            width,
            height,
        })
    }

    #[allow(dead_code)]
    pub fn width(&self) -> u32 {
        self.width
    }
    #[allow(dead_code)]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Copy `tex` → cached staging tex, then map and tight-pack into the
    /// caller-provided `Vec<u8>` (length `width * height * 4`).
    fn readback_bgra(&self, tex: &D3d11Texture, out: &mut [u8]) -> Result<(), MediaError> {
        tex.read_back_bgra_into(&self.dev, &self.staging, out)
    }
}

// Same Send rationale as DxgiNvencProducer: DesktopDuplication holds a !Send
// IDXGIOutputDuplication, but we serialise access via &mut self in
// `next_frame`, never touching it concurrently. Openh264Encoder is Send.
unsafe impl Send for DxgiSwProducer {}

fn is_access_lost(e: &MediaError) -> bool {
    match e {
        MediaError::Dxgi { hresult, .. } => {
            const DXGI_ERROR_ACCESS_LOST: u32 = 0x887A_0026;
            const DXGI_ERROR_ACCESS_DENIED: u32 = 0x887A_0027;
            const DXGI_ERROR_INVALID_CALL: u32 = 0x887A_0001;
            *hresult == DXGI_ERROR_ACCESS_LOST
                || *hresult == DXGI_ERROR_ACCESS_DENIED
                || *hresult == DXGI_ERROR_INVALID_CALL
        }
        _ => false,
    }
}

#[async_trait::async_trait]
impl VideoProducer for DxgiSwProducer {
    async fn next_frame(&mut self) -> Result<EncodedFrame, ProducerError> {
        loop {
            let acquired = match self.dup.acquire_next_frame(Duration::from_millis(16)) {
                Ok(a) => a,
                Err(e) if e.is_device_removed() => {
                    tracing::error!(
                        error = %e,
                        "D3D11 device removed in sw producer — fatal; \
                         restart the host process to recover",
                    );
                    return Err(ProducerError::Capture(format!("device removed: {e}")));
                }
                Err(e) => {
                    if is_access_lost(&e) {
                        tracing::warn!(
                            error = %e,
                            "DXGI access lost (sw producer); re-acquiring duplication"
                        );
                        match DesktopDuplication::new(&self.dev, &self.output) {
                            Ok(new_dup) => {
                                self.dup = new_dup;
                                self.idr_pending = true;
                                tokio::time::sleep(Duration::from_millis(50)).await;
                                continue;
                            }
                            Err(re_err) => {
                                tracing::warn!(
                                    error = %re_err,
                                    "re-acquiring DXGI duplication failed (sw producer); backing off"
                                );
                                tokio::time::sleep(Duration::from_millis(250)).await;
                                continue;
                            }
                        }
                    } else {
                        return Err(ProducerError::Capture(e.to_string()));
                    }
                }
            };
            let texture = match acquired {
                AcquiredFrame::Frame { texture, .. } => texture,
                AcquiredFrame::Timeout => continue,
            };

            // CPU readback (cached staging tex) — synchronous, sub-ms at 1080p.
            let row_bytes = (self.width as usize) * 4;
            let mut bgra = vec![0u8; row_bytes * (self.height as usize)];
            self.readback_bgra(&texture, &mut bgra)
                .map_err(|e| ProducerError::Capture(format!("readback: {e}")))?;

            let width = self.width;
            let height = self.height;
            let bgra_stride = width * 4;
            let i420 = bgra_to_i420(&bgra, width, height, bgra_stride)
                .map_err(|e| ProducerError::Other(format!("bgra_to_i420: {e}")))?;

            let ts_us = now_monotonic_us();
            let force_idr = std::mem::take(&mut self.idr_pending);

            // Move encoder into the blocking pool, run encode, move it back.
            // This keeps the single-threaded OpenH264 call off the tokio
            // reactor (pre-mortem #2 mitigation).
            let mut enc = self
                .encoder
                .take()
                .expect("encoder was taken twice; producer state corrupted");
            let join = tokio::task::spawn_blocking(move || {
                let result = enc.encode(&i420, force_idr, ts_us);
                (enc, result)
            })
            .await
            .map_err(|e| ProducerError::Other(format!("spawn_blocking join: {e}")))?;
            let (enc_back, encode_result) = join;
            self.encoder = Some(enc_back);

            let frame = encode_result.map_err(|e| ProducerError::Encode(e.to_string()))?;

            // Openh264Encoder already returns a fully-formed EncodedFrame
            // with codec=H264. Override seq with the producer-tracked
            // counter so the wire seq matches our producer ordering
            // (encoder's internal seq is independent and resets on
            // reinit).
            let seq = self.seq;
            self.seq += 1;
            return Ok(EncodedFrame {
                seq,
                ..frame
            });
        }
    }

    fn request_idr(&mut self) {
        self.idr_pending = true;
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        if let Some(enc) = self.encoder.as_mut() {
            enc.set_target_bitrate(bps);
        }
    }

    fn backend_name(&self) -> &'static str {
        "openh264-sw"
    }
}
