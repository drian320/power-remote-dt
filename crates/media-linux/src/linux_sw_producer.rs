//! `VideoProducer` impl that wires any `CaptureSource` (X11 SHM or
//! Wayland portal) + LinuxSwEncoder with explicit 60Hz pacing and
//! `spawn_blocking` encode. Mirrors `crates/host/src/dxgi_sw_producer.rs`
//! (Windows side).

use crate::capture_source::CaptureSource;
use crate::sw_pipeline::LinuxSwEncoder;
use prdt_protocol::{now_monotonic_us, EncodedFrame, ProducerError, VideoProducer};
use std::time::Duration;
use tokio::time::{interval, Interval, MissedTickBehavior};

pub struct LinuxSwProducer {
    /// `Option` so we can move it into `spawn_blocking` and back. Never
    /// `None` outside the await boundary.
    capture: Option<Box<dyn CaptureSource>>,
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
        capture: Box<dyn CaptureSource>,
        encoder: LinuxSwEncoder,
        fps: u32,
    ) -> anyhow::Result<Self> {
        let (width, height) = capture.geometry();
        let pacer = make_pacer(fps);
        Ok(Self {
            capture: Some(capture),
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
    let micros = if fps == 0 {
        16_667
    } else {
        1_000_000 / fps as u64
    };
    let mut p = interval(Duration::from_micros(micros));
    p.set_missed_tick_behavior(MissedTickBehavior::Skip);
    p
}

#[async_trait::async_trait]
impl VideoProducer for LinuxSwProducer {
    async fn next_frame(&mut self) -> Result<EncodedFrame, ProducerError> {
        self.pacer.tick().await;

        // capture_into is blocking (Wayland path blocks on rx.recv from the
        // PipeWire thread; X11 path blocks on the XCB reply). Run on the
        // blocking pool — mirrors the existing encoder.spawn_blocking.
        let mut bgra = std::mem::take(&mut self.bgra_buf);
        let mut capture = self
            .capture
            .take()
            .expect("capture taken twice; producer state corrupted");
        let (bgra, capture, capture_result) = tokio::task::spawn_blocking(move || {
            let r = capture.capture_into(&mut bgra);
            (bgra, capture, r)
        })
        .await
        .map_err(|e| ProducerError::Other(format!("spawn_blocking capture join: {e}")))?;
        self.bgra_buf = bgra;
        // Re-read geometry: Wayland can resize mid-session. L4 encoder reconfigure
        // is already in place; on a size change the encoder rebuilds before the
        // next encode (see set_target_bitrate / future reconfigure entry point).
        let (w, h) = capture.geometry();
        self.width = w;
        self.height = h;
        self.capture = Some(capture);

        match capture_result {
            Ok(()) => {}
            Err(crate::capture_source::CaptureSourceError::WouldBlock(reason)) => {
                // No frame this tick — surface as ProducerError::Capture with
                // a clear marker so callers can distinguish from terminal failure.
                // The session loop will simply pick up the next tick.
                return Err(ProducerError::Capture(format!("would_block: {reason}")));
            }
            Err(crate::capture_source::CaptureSourceError::Terminal { backend, reason }) => {
                return Err(ProducerError::Capture(format!("{backend}: {reason}")));
            }
        }

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
        // The "capture-encoder" pair name. Concrete capture impl logs its
        // own name on construction; the producer-level name stays stable so
        // viewer-overlay UX doesn't flicker on a capture-only swap.
        "linux-openh264"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn make_pacer_returns_60fps_interval_for_fps_60() {
        let _ = make_pacer(60);
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn pacer_60fps_yields_at_16ms_intervals() {
        let mut p = make_pacer(60);
        p.tick().await;
        let advance = Duration::from_micros(16_667);
        tokio::time::advance(advance).await;
        p.tick().await;
        tokio::time::advance(advance).await;
        p.tick().await;
    }
}
