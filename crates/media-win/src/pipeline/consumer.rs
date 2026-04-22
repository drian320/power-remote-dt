//! MfD3d11Consumer - Media Foundation H.265 decode. Stores the latest
//! decoded NV12 bytes in a mutex for test/inspection; Plan 3's viewer bin
//! will replace this with a D3D11 swapchain present.

use std::sync::{Arc, Mutex};

use prdt_protocol::{ConsumerError, EncodedFrame, VideoConsumer};

use crate::d3d11::D3d11Device;
use crate::error::MediaError;
use crate::mf::H265Decoder;

pub struct MfD3d11Consumer {
    decoder: H265Decoder,
    latest_output: Arc<Mutex<Option<Vec<u8>>>>,
    needs_idr: bool,
}

impl MfD3d11Consumer {
    pub fn new(dev: &D3d11Device, width: u32, height: u32) -> Result<Self, MediaError> {
        let decoder = H265Decoder::new(dev, width, height)?;
        Ok(Self {
            decoder,
            latest_output: Default::default(),
            needs_idr: true,
        })
    }

    /// Consume the latest decoded NV12 frame (takes ownership, leaves None).
    /// Returns None if no frame has been decoded yet or if the previous one
    /// was already consumed.
    pub fn take_latest_frame(&self) -> Option<Vec<u8>> {
        self.latest_output.lock().unwrap().take()
    }
}

// H265Decoder holds an IMFTransform (COM, !Send by default in the
// `windows` crate). MFTs are thread-agnostic as long as we don't drive them
// concurrently from multiple threads — which we don't: submit() takes
// &mut self. Mark Send so we satisfy the `VideoConsumer: Send` bound.
unsafe impl Send for MfD3d11Consumer {}

#[async_trait::async_trait]
impl VideoConsumer for MfD3d11Consumer {
    async fn submit(&mut self, frame: EncodedFrame) -> Result<(), ConsumerError> {
        // MF expects timestamps in 100ns units. We use frame.timestamp_host_us * 10.
        let ts_hns = (frame.timestamp_host_us as i64).saturating_mul(10);
        self.decoder
            .process_input(&frame.nal_units, ts_hns)
            .map_err(|e| ConsumerError::Decode(e.to_string()))?;

        // Drain available outputs.
        for _ in 0..5 {
            match self
                .decoder
                .process_output()
                .map_err(|e| ConsumerError::Decode(e.to_string()))?
            {
                Some(bytes) => {
                    *self.latest_output.lock().unwrap() = Some(bytes);
                    self.needs_idr = false;
                }
                None => break,
            }
        }
        Ok(())
    }

    fn needs_idr(&self) -> bool {
        self.needs_idr || self.decoder.needs_idr()
    }
}
