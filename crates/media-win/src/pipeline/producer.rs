//! DxgiNvencProducer - DXGI Desktop Duplication capture to NVENC H.265 encode.

use std::time::{Duration, Instant};

use bytes::Bytes;
use prdt_protocol::{EncodedFrame, ProducerError, VideoProducer};

use crate::d3d11::D3d11Device;
use crate::dxgi::{AcquiredFrame, DesktopDuplication, OutputInfo};
use crate::error::MediaError;
use crate::nvenc::{NvencEncoder, NvencEncoderConfig};

pub struct DxgiNvencProducer {
    dev: D3d11Device,
    output: OutputInfo,
    dup: DesktopDuplication,
    encoder: NvencEncoder,
    seq: u64,
    epoch: Instant,
    idr_pending: bool,
    width: u32,
    height: u32,
}

impl DxgiNvencProducer {
    /// Create a producer for the given monitor. `bitrate_bps` is the NVENC
    /// target CBR bitrate.
    pub fn new(
        dev: &D3d11Device,
        output: &OutputInfo,
        bitrate_bps: u32,
    ) -> Result<Self, MediaError> {
        let dup = DesktopDuplication::new(dev, output)?;
        let width = dup.width();
        let height = dup.height();
        let cfg = NvencEncoderConfig {
            width,
            height,
            fps_numerator: 60,
            fps_denominator: 1,
            bitrate_bps,
            gop_length: 60,
        };
        let encoder = NvencEncoder::new(dev, &cfg)?;
        Ok(Self {
            dev: dev.clone(),
            output: output.clone(),
            dup,
            encoder,
            seq: 0,
            epoch: Instant::now(),
            idr_pending: true,
            width,
            height,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
}

// DesktopDuplication holds an IDXGIOutputDuplication (COM, !Send by default
// in the `windows` crate). NvencEncoder already declares `unsafe impl Send`;
// the DXGI duplication object is safe to move between threads provided we
// don't touch it concurrently (we don't: &mut self on next_frame).
unsafe impl Send for DxgiNvencProducer {}

/// Classify an error from `DesktopDuplication::acquire_next_frame` as a
/// "duplication lost" condition that we should try to recover from by
/// re-creating the duplication. These HRESULTs show up when Windows takes
/// the duplication context away from us (UAC secure-desktop prompt, screen
/// lock, Ctrl+Alt+Del, fullscreen exclusive app, resolution change, etc.).
fn is_access_lost(e: &MediaError) -> bool {
    match e {
        MediaError::Dxgi { hresult, .. } => {
            // DXGI error HRESULTs we treat as recoverable.
            const DXGI_ERROR_ACCESS_LOST: u32 = 0x887A_0026;
            const DXGI_ERROR_ACCESS_DENIED: u32 = 0x887A_0027;
            // After access-lost, subsequent calls on the stale duplication
            // often come back as INVALID_CALL; treat that as recoverable too.
            const DXGI_ERROR_INVALID_CALL: u32 = 0x887A_0001;
            *hresult == DXGI_ERROR_ACCESS_LOST
                || *hresult == DXGI_ERROR_ACCESS_DENIED
                || *hresult == DXGI_ERROR_INVALID_CALL
        }
        _ => false,
    }
}

#[async_trait::async_trait]
impl VideoProducer for DxgiNvencProducer {
    async fn next_frame(&mut self) -> Result<EncodedFrame, ProducerError> {
        loop {
            let acquired = match self.dup.acquire_next_frame(Duration::from_millis(16)) {
                Ok(a) => a,
                Err(e) => {
                    if is_access_lost(&e) {
                        tracing::warn!(error = %e, "DXGI access lost; re-acquiring duplication");
                        match DesktopDuplication::new(&self.dev, &self.output) {
                            Ok(new_dup) => {
                                self.dup = new_dup;
                                // Viewer state is invalid after the gap; force the
                                // next encoded frame to be an IDR.
                                self.idr_pending = true;
                                tokio::time::sleep(Duration::from_millis(50)).await;
                                continue;
                            }
                            Err(re_err) => {
                                // The OS is still holding the duplication away
                                // from us (e.g. UAC prompt still up). Back off
                                // and try again on the next loop iteration.
                                tracing::warn!(
                                    error = %re_err,
                                    "re-acquiring DXGI duplication failed; backing off"
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
            let ts_us = self.epoch.elapsed().as_micros() as u64;
            let force_idr = std::mem::take(&mut self.idr_pending);
            let encoded = self
                .encoder
                .encode(&texture, force_idr, ts_us)
                .map_err(|e| ProducerError::Encode(e.to_string()))?;
            let seq = self.seq;
            self.seq += 1;
            return Ok(EncodedFrame::new_h265(
                seq,
                ts_us,
                encoded.is_keyframe,
                Bytes::from(encoded.nal_bytes),
                self.width,
                self.height,
            ));
        }
    }

    fn request_idr(&mut self) {
        self.idr_pending = true;
    }

    fn set_target_bitrate(&mut self, _bps: u32) {
        // Phase 0 Plan 2c: bitrate is fixed at construction time. Reconfigure
        // via NvencEncoder::reconfigure will be wired in Plan 3+.
    }
}
