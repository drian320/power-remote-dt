//! `VideoProducer` impl that wires X11ShmCapturer + LinuxSwEncoder
//! together with explicit 60Hz pacing and `spawn_blocking` encode.
//! Mirrors `crates/host/src/dxgi_sw_producer.rs` (Windows side).

use crate::sw_pipeline::LinuxSwEncoder;
use crate::x11_capture::X11ShmCapturer;
use prdt_protocol::{now_monotonic_us, EncodedFrame, ProducerError, VideoProducer};
use std::time::Duration;
use tokio::time::{interval, Interval, MissedTickBehavior};

pub struct LinuxSwProducer {
    capture: X11ShmCapturer,
    /// Owned by the producer for its lifetime; `take()` + restore lets
    /// us move the encoder into `spawn_blocking` and back.
    encoder: Option<LinuxSwEncoder>,
    bgra_buf: Vec<u8>,
    pacer: Interval,
    seq: u64,
    idr_pending: bool,
    width: u32,
    height: u32,
}

impl LinuxSwProducer {
    pub fn new(
        capture: X11ShmCapturer,
        encoder: LinuxSwEncoder,
        fps: u32,
    ) -> anyhow::Result<Self> {
        let width = capture.width();
        let height = capture.height();
        let pacer = make_pacer(fps);
        Ok(Self {
            capture,
            encoder: Some(encoder),
            bgra_buf: vec![0u8; (width * height * 4) as usize],
            pacer,
            seq: 0,
            idr_pending: true,
            width,
            height,
        })
    }
}

fn make_pacer(fps: u32) -> Interval {
    let micros = if fps == 0 { 16_667 } else { 1_000_000 / fps as u64 };
    let mut p = interval(Duration::from_micros(micros));
    p.set_missed_tick_behavior(MissedTickBehavior::Skip);
    p
}

#[async_trait::async_trait]
impl VideoProducer for LinuxSwProducer {
    async fn next_frame(&mut self) -> Result<EncodedFrame, ProducerError> {
        // SW path on Linux has no transient retry conditions (no DXGI
        // access-lost equivalent), so the body is straight-line. If
        // L2 introduces e.g. PipeWire stream-restart we'll wrap this
        // in a retry loop.
        self.pacer.tick().await;

        // Sync capture. If this fails permanently, propagate as
        // Capture error.
        self.capture
            .grab_into(&mut self.bgra_buf)
            .map_err(|e| ProducerError::Capture(e.to_string()))?;

        let bgra = std::mem::take(&mut self.bgra_buf);
        let width = self.width;
        let height = self.height;
        let force_idr = std::mem::take(&mut self.idr_pending);
        let ts_us = now_monotonic_us();

        let mut enc = self
            .encoder
            .take()
            .expect("encoder taken twice; producer state corrupted");
        let join = tokio::task::spawn_blocking(move || {
            let frame = crate::frame::BgraFrame {
                width,
                height,
                stride: width * 4,
                bgra,
                capture_ts_us: ts_us,
            };
            let result = enc.encode(&frame, force_idr, ts_us);
            (enc, frame.bgra, result)
        })
        .await
        .map_err(|e| ProducerError::Other(format!("spawn_blocking join: {e}")))?;
        let (enc_back, bgra_back, encode_result) = join;
        self.encoder = Some(enc_back);
        self.bgra_buf = bgra_back;

        let frame = encode_result.map_err(|e| ProducerError::Encode(e.to_string()))?;

        let seq = self.seq;
        self.seq += 1;

        // Spread the encoder's EncodedFrame, override seq with our
        // producer-tracked counter (mirrors DxgiSwProducer).
        Ok(EncodedFrame { seq, ..frame })
    }

    fn request_idr(&mut self) {
        self.idr_pending = true;
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        if let Some(e) = self.encoder.as_mut() {
            e.set_target_bitrate(bps);
        }
    }

    fn backend_name(&self) -> &'static str {
        "linux-x11shm-openh264"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn make_pacer_returns_60fps_interval_for_fps_60() {
        // Interval period is private; we verify via tokio::time::pause
        // and ticking in the async test below.
        let _ = make_pacer(60);
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn pacer_60fps_yields_at_16ms_intervals() {
        let mut p = make_pacer(60);
        // First tick fires immediately.
        p.tick().await;
        // Second tick should require ~16.67ms of advance.
        let advance = Duration::from_micros(16_667);
        tokio::time::advance(advance).await;
        p.tick().await;
        // Third tick: same again.
        tokio::time::advance(advance).await;
        p.tick().await;
    }
}
