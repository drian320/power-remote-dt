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
    /// `Option` so we can move it into `spawn_blocking` and back. Once a
    /// `spawn_blocking` task panics, `poisoned` is set and the producer
    /// permanently returns `Terminal`; callers are expected to drop and rebuild
    /// via the policy-driven failover.
    capture: Option<Box<dyn CaptureSource>>,
    encoder: Option<LinuxSwEncoder>,
    bgra_buf: Vec<u8>,
    pacer: Interval,
    seq: u64,
    idr_pending: bool,
    width: u32,
    height: u32,
    /// Set when a `spawn_blocking` task panics. Once `true`, every subsequent
    /// `next_frame()` returns immediately without panicking. Caller must drop
    /// and recreate the producer.
    poisoned: bool,
    /// Set after the first geometry-change warning so we don't spam the log.
    resize_warned: bool,
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
            poisoned: false,
            resize_warned: false,
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
        if self.poisoned {
            return Err(ProducerError::Capture(
                "producer poisoned by inner panic: drop and recreate".into(),
            ));
        }

        self.pacer.tick().await;

        // capture_into is blocking (Wayland path blocks on rx.recv from the
        // PipeWire thread; X11 path blocks on the XCB reply). Run on the
        // blocking pool — mirrors the existing encoder.spawn_blocking.
        let mut bgra = std::mem::take(&mut self.bgra_buf);
        let mut capture = self
            .capture
            .take()
            .expect("capture taken twice; producer state corrupted");
        let capture_join = tokio::task::spawn_blocking(move || {
            let r = capture.capture_into(&mut bgra);
            (bgra, capture, r)
        })
        .await;
        let (bgra, capture, capture_result) = match capture_join {
            Ok(triple) => triple,
            Err(e) => {
                self.poisoned = true;
                return Err(ProducerError::Capture(format!(
                    "producer poisoned by inner panic: {e}"
                )));
            }
        };
        self.bgra_buf = bgra;
        // Re-read geometry: Wayland can resize mid-session. T1 only records the
        // new values on the producer; an actual encoder rebuild on size change is
        // NOT wired up here yet — see follow-up TODO below.
        //
        // TODO(P5B-2 / P5C): when `geometry()` changes mid-session, tear down and
        // rebuild the encoder before the next encode. Until then, a Wayland resize
        // will silently produce a size-mismatch in the encoder. The X11 path never
        // resizes (root window is fixed at startup) so the X11 smoke path is
        // unaffected.
        let (w, h) = capture.geometry();
        if (w != self.width || h != self.height) && !self.resize_warned {
            self.resize_warned = true;
            tracing::warn!(
                old_w = self.width,
                old_h = self.height,
                new_w = w,
                new_h = h,
                "capture geometry changed mid-session; encoder rebuild not yet wired (P5B-2)"
            );
        }
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
        let encode_join = tokio::task::spawn_blocking(move || {
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
        .await;
        let (enc_back, bgra_back, encode_result) = match encode_join {
            Ok(triple) => triple,
            Err(e) => {
                self.poisoned = true;
                return Err(ProducerError::Capture(format!(
                    "producer poisoned by inner panic: {e}"
                )));
            }
        };
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
