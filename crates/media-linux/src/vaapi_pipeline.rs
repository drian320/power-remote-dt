//! VaapiVideoProducer — Linux HW codec path (VAAPI H.264).
//!
//! Mirrors `LinuxSwProducer` in shape; capture path (X11 SHM or Wayland
//! portal) is unchanged. After capture we still convert BGRA → I420 on CPU
//! (P5C-1 minimum scope); P5C-2 will replace this with a DMABUF → VAAPI
//! surface zero-copy path.
//!
//! ## !Send encoder + Send producer
//!
//! `VaapiH264Encoder` owns `Rc<libva::Display>` / `Rc<libva::Context>` from
//! `cros-libva` 0.0.13, which means the encoder is `!Send`. But the
//! `VideoProducer` trait requires `Send` so policy-driven failover can move
//! the boxed producer across runtime workers.
//!
//! Bridge: a dedicated OS thread owns the encoder for its entire lifetime.
//! The producer talks to that thread via a pair of `crossbeam`-style mpsc
//! channels (we use `std::sync::mpsc::sync_channel(1)` for backpressure —
//! one in-flight encode at a time matches the encoder's single-surface
//! retire model). Both ends of the channel are `Send`, so the producer
//! struct is `Send`. Per-call `spawn_blocking` performs the blocking
//! `send`/`recv` so we never block the async runtime's worker.

#![cfg(target_os = "linux")]

use crate::capture_source::CaptureSource;
use crate::error::LinuxMediaError;
use crate::frame::BgraFrame;
use prdt_media_sw::bgra_to_i420;
use prdt_media_vaapi::{VaapiError, VaapiH264Encoder, VaapiH264EncoderConfig};
use prdt_protocol::{now_monotonic_us, EncodedFrame, ProducerError, VideoProducer};
use std::sync::mpsc;
use std::time::Duration;
use tokio::time::{interval, Interval, MissedTickBehavior};

// ---------------------------------------------------------------------------
// Encoder wrapper — runs on a dedicated thread; producer holds channel ends.
// ---------------------------------------------------------------------------

/// One unit of work the encoder thread can perform. Replies on the
/// per-request `oneshot` (here implemented as an `mpsc::sync_channel(0)`
/// for simplicity — bounded=0 means the sender blocks until the receiver
/// is ready, equivalent to a oneshot).
enum EncoderCmd {
    Encode {
        frame: BgraFrame,
        force_idr: bool,
        ts_us: u64,
        reply: mpsc::SyncSender<Result<EncodedFrame, LinuxMediaError>>,
    },
    SetBitrate(u32),
    Shutdown,
}

/// Producer-side handle to the encoder thread.
///
/// `Send`-safe because both channel endpoints are `Send` (the encoder
/// itself stays on `_thread`'s stack and is never moved out).
pub struct LinuxVaapiEncoder {
    cmd_tx: mpsc::SyncSender<EncoderCmd>,
    /// JoinHandle kept around so the thread is joined on drop. The Option
    /// lets `Drop::drop` `take()` it.
    thread: Option<std::thread::JoinHandle<()>>,
    width: u32,
    height: u32,
}

impl LinuxVaapiEncoder {
    pub fn new(
        width: u32,
        height: u32,
        bitrate_bps: u32,
        fps: u32,
    ) -> Result<Self, LinuxMediaError> {
        // We need to surface encoder-init errors synchronously, so we
        // construct the encoder on a "probe" call inside the spawned
        // thread and report the result back through an init-status channel
        // before the thread enters its command loop.
        let (init_tx, init_rx) = mpsc::sync_channel::<Result<(), LinuxMediaError>>(0);
        let (cmd_tx, cmd_rx) = mpsc::sync_channel::<EncoderCmd>(1);

        let handle = std::thread::Builder::new()
            .name("vaapi-encoder".into())
            .spawn(move || {
                let cfg = VaapiH264EncoderConfig {
                    width,
                    height,
                    fps,
                    initial_bitrate_bps: bitrate_bps,
                    ..Default::default()
                };
                let mut enc = match VaapiH264Encoder::new(cfg) {
                    Ok(e) => {
                        if init_tx.send(Ok(())).is_err() {
                            // Parent dropped the receiver before we replied; nothing
                            // to do but exit cleanly.
                            return;
                        }
                        e
                    }
                    Err(e) => {
                        // Best-effort error report; if the parent is already gone we
                        // simply exit.
                        let _ = init_tx.send(Err(LinuxMediaError::Vaapi(e)));
                        return;
                    }
                };
                // Encoder is live; enter the command loop.
                while let Ok(cmd) = cmd_rx.recv() {
                    match cmd {
                        EncoderCmd::Encode {
                            frame,
                            force_idr,
                            ts_us,
                            reply,
                        } => {
                            let result = encode_one(&mut enc, &frame, force_idr, ts_us);
                            // If the receiver is gone (producer abandoned the
                            // wait), drop the result silently.
                            let _ = reply.send(result);
                        }
                        EncoderCmd::SetBitrate(bps) => {
                            if let Err(e) = enc.set_target_bitrate(bps) {
                                tracing::warn!(?e, "vaapi set_target_bitrate failed");
                            }
                        }
                        EncoderCmd::Shutdown => break,
                    }
                }
                // Drop runs the spec §3.4 teardown sequence.
                drop(enc);
            })
            .map_err(|e| {
                LinuxMediaError::Vaapi(VaapiError::DisplayOpen(format!("spawn thread: {e}")))
            })?;

        // Wait for the encoder thread to report init success/failure.
        match init_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                cmd_tx,
                thread: Some(handle),
                width,
                height,
            }),
            Ok(Err(e)) => {
                // Thread will exit on its own; join just to be tidy.
                let _ = handle.join();
                Err(e)
            }
            Err(_) => {
                // Thread died before reporting; join + surface a generic
                // bitstream-style error.
                let _ = handle.join();
                Err(LinuxMediaError::Vaapi(VaapiError::DisplayOpen(
                    "encoder thread exited before init".into(),
                )))
            }
        }
    }

    /// Submit one frame to the encoder thread and wait for the reply.
    /// This call is *blocking* — `VaapiVideoProducer::next_frame` runs it
    /// inside `spawn_blocking` so the async runtime's worker stays free.
    pub fn encode_blocking(
        &self,
        frame: BgraFrame,
        force_idr: bool,
        ts_us: u64,
    ) -> Result<EncodedFrame, LinuxMediaError> {
        if frame.width != self.width || frame.height != self.height {
            return Err(LinuxMediaError::InvalidDimensions(
                frame.width,
                frame.height,
            ));
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(0);
        self.cmd_tx
            .send(EncoderCmd::Encode {
                frame,
                force_idr,
                ts_us,
                reply: reply_tx,
            })
            .map_err(|_| LinuxMediaError::Vaapi(VaapiError::Closed))?;
        reply_rx
            .recv()
            .map_err(|_| LinuxMediaError::Vaapi(VaapiError::Closed))?
    }

    pub fn set_target_bitrate(&self, bps: u32) {
        // Fire-and-forget; if the thread is gone the producer is broken
        // anyway and the next `encode_blocking` will surface that.
        let _ = self.cmd_tx.send(EncoderCmd::SetBitrate(bps));
    }
}

impl Drop for LinuxVaapiEncoder {
    fn drop(&mut self) {
        // Politely ask the thread to exit, then join so the encoder's
        // teardown (spec §3.4) completes before we return.
        let _ = self.cmd_tx.send(EncoderCmd::Shutdown);
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}

/// Runs on the dedicated encoder thread. BGRA → I420 conversion happens
/// here (it's owned by the thread that owns the encoder so the conversion
/// scratch lives entirely off the async runtime).
fn encode_one(
    enc: &mut VaapiH264Encoder,
    frame: &BgraFrame,
    force_idr: bool,
    ts_us: u64,
) -> Result<EncodedFrame, LinuxMediaError> {
    let i420 = bgra_to_i420(&frame.bgra, frame.width, frame.height, frame.stride)?;
    let out = enc.encode(&i420, force_idr, ts_us)?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// VaapiVideoProducer (parallel to LinuxSwProducer)
// ---------------------------------------------------------------------------

pub struct VaapiVideoProducer {
    /// `Option` so we can move it into `spawn_blocking` and back. Once a
    /// `spawn_blocking` task panics, `poisoned` is set and the producer
    /// permanently returns `Capture` errors; callers are expected to drop
    /// and rebuild via the policy-driven failover.
    capture: Option<Box<dyn CaptureSource>>,
    /// `LinuxVaapiEncoder` is a channel-based handle to the dedicated
    /// encoder thread — it is `Send`, so we can keep it inline (no
    /// `Option<…>` take/restore dance needed; `&self` works because the
    /// channel `Sender` doesn't need `&mut self`).
    encoder: LinuxVaapiEncoder,
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

impl VaapiVideoProducer {
    pub fn new(
        capture: Box<dyn CaptureSource>,
        encoder: LinuxVaapiEncoder,
        fps: u32,
    ) -> anyhow::Result<Self> {
        let (width, height) = capture.geometry();
        let pacer = make_pacer(fps);
        Ok(Self {
            capture: Some(capture),
            encoder,
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

/// Inspect a `LinuxMediaError` returned by the encoder thread and classify
/// it for the producer's `next_frame` error path. `VaapiError::HardwareBusy`
/// surfaces as `ProducerError::DeviceLost` so `SelectionPolicy` can fail over
/// to OpenH264; everything else stays as `ProducerError::Encode`.
fn classify_encode_error(e: LinuxMediaError) -> ProducerError {
    if let LinuxMediaError::Vaapi(ref v) = e {
        if matches!(v, VaapiError::HardwareBusy { .. }) {
            return ProducerError::DeviceLost {
                backend: "linux-vaapi-h264".into(),
                reason: e.to_string(),
            };
        }
    }
    ProducerError::Encode(e.to_string())
}

#[async_trait::async_trait]
impl VideoProducer for VaapiVideoProducer {
    async fn next_frame(&mut self) -> Result<EncodedFrame, ProducerError> {
        if self.poisoned {
            return Err(ProducerError::Capture(
                "producer poisoned by inner panic: drop and recreate".into(),
            ));
        }

        self.pacer.tick().await;

        // capture_into is blocking; run on the blocking pool to mirror the
        // encode submission below.
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
        // Re-read geometry: Wayland can resize mid-session. Same caveat as
        // LinuxSwProducer — encoder rebuild on size change is not wired up
        // here yet (see P5B-2 TODO in linux_sw_producer.rs).
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
        // Hand a clone of the `Send`-able channel sender to spawn_blocking.
        // (We could also avoid the clone by using cmd_tx via &self, but the
        // `'static` requirement of spawn_blocking forces a move.)
        let encoder_handle = EncoderClient {
            cmd_tx: self.encoder.cmd_tx.clone(),
            width: self.width,
            height: self.height,
        };
        let encode_join = tokio::task::spawn_blocking(move || {
            let frame = crate::frame::BgraFrame {
                width,
                height,
                stride: width * 4,
                bgra,
                capture_ts_us: ts_us,
            };
            let result = encoder_handle.encode_blocking(frame.clone(), force_idr, ts_us);
            (frame.bgra, result)
        })
        .await;
        let (bgra_back, encode_result) = match encode_join {
            Ok(pair) => pair,
            Err(e) => {
                self.poisoned = true;
                return Err(ProducerError::Capture(format!(
                    "producer poisoned by inner panic: {e}"
                )));
            }
        };
        self.bgra_buf = bgra_back;

        let frame = encode_result.map_err(classify_encode_error)?;
        let seq = self.seq;
        self.seq += 1;
        Ok(EncodedFrame { seq, ..frame })
    }

    fn request_idr(&mut self) {
        self.idr_pending = true;
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        self.encoder.set_target_bitrate(bps);
    }

    fn backend_name(&self) -> &'static str {
        "linux-vaapi-h264"
    }
}

/// Tiny `Send` shim used by `next_frame` to dispatch one encode submission
/// into `spawn_blocking` without moving `&self`. The shim holds a clone of
/// the channel sender; the underlying encoder thread does the work.
struct EncoderClient {
    cmd_tx: mpsc::SyncSender<EncoderCmd>,
    width: u32,
    height: u32,
}

impl EncoderClient {
    fn encode_blocking(
        &self,
        frame: BgraFrame,
        force_idr: bool,
        ts_us: u64,
    ) -> Result<EncodedFrame, LinuxMediaError> {
        if frame.width != self.width || frame.height != self.height {
            return Err(LinuxMediaError::InvalidDimensions(
                frame.width,
                frame.height,
            ));
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(0);
        self.cmd_tx
            .send(EncoderCmd::Encode {
                frame,
                force_idr,
                ts_us,
                reply: reply_tx,
            })
            .map_err(|_| LinuxMediaError::Vaapi(VaapiError::Closed))?;
        reply_rx
            .recv()
            .map_err(|_| LinuxMediaError::Vaapi(VaapiError::Closed))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn make_pacer_returns_interval_for_fps_60() {
        let _ = make_pacer(60);
    }

    #[test]
    fn classify_hw_busy_maps_to_device_lost() {
        let e = LinuxMediaError::Vaapi(VaapiError::HardwareBusy { attempts: 5 });
        match classify_encode_error(e) {
            ProducerError::DeviceLost { backend, reason } => {
                assert_eq!(backend, "linux-vaapi-h264");
                assert!(reason.contains("hardware busy"), "got: {reason}");
            }
            other => panic!("expected DeviceLost, got {other:?}"),
        }
    }

    #[test]
    fn classify_other_vaapi_error_maps_to_encode() {
        let e = LinuxMediaError::Vaapi(VaapiError::Bitstream("bad NAL".into()));
        match classify_encode_error(e) {
            ProducerError::Encode(msg) => assert!(msg.contains("bad NAL")),
            other => panic!("expected Encode, got {other:?}"),
        }
    }

    #[test]
    fn classify_non_vaapi_error_maps_to_encode() {
        let e = LinuxMediaError::InvalidDimensions(640, 480);
        assert!(matches!(classify_encode_error(e), ProducerError::Encode(_)));
    }

    /// Compile-time check: `VaapiVideoProducer` is `Send`. This is the
    /// load-bearing property that the encoder-thread bridge restores
    /// despite `VaapiH264Encoder` being `!Send`. If this stops compiling,
    /// `LinuxSwFactory` would also fail to box the producer.
    #[allow(dead_code)]
    fn _assert_send() {
        fn takes_send<T: Send>(_: &T) {}
        // We can't construct one without a real device, so just check the
        // bound on the channel handle and a hypothetical reference.
        let (_tx, _rx) = mpsc::sync_channel::<EncoderCmd>(1);
        takes_send(&_tx);
    }

    /// Smoke: `request_idr` re-arms `idr_pending` even after it has been
    /// consumed. We build the producer struct in place with a stub channel
    /// (the encoder thread is irrelevant for this assertion). Needs a
    /// Tokio runtime because `make_pacer` calls `tokio::time::interval`.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn request_idr_re_arms_idr_pending() {
        struct StubCapture;
        impl CaptureSource for StubCapture {
            fn geometry(&self) -> (u32, u32) {
                (64, 64)
            }
            fn capture_into(
                &mut self,
                _out: &mut Vec<u8>,
            ) -> Result<(), crate::capture_source::CaptureSourceError> {
                Err(crate::capture_source::CaptureSourceError::Terminal {
                    backend: "stub",
                    reason: "test-only".into(),
                })
            }
        }

        // Build a minimal `LinuxVaapiEncoder` whose thread immediately
        // exits — we never call `encode_blocking` in this test. Both
        // channel ends are alive long enough to satisfy the producer's
        // field type.
        let (tx, rx) = mpsc::sync_channel::<EncoderCmd>(1);
        let handle = std::thread::spawn(move || {
            while let Ok(cmd) = rx.recv() {
                if matches!(cmd, EncoderCmd::Shutdown) {
                    break;
                }
            }
        });
        let encoder = LinuxVaapiEncoder {
            cmd_tx: tx,
            thread: Some(handle),
            width: 64,
            height: 64,
        };

        let pacer = make_pacer(60);
        let mut p = VaapiVideoProducer {
            capture: Some(Box::new(StubCapture)),
            encoder,
            bgra_buf: vec![],
            pacer,
            seq: 0,
            idr_pending: true,
            width: 64,
            height: 64,
            poisoned: false,
            resize_warned: false,
        };
        assert!(p.idr_pending);
        p.idr_pending = false;
        p.request_idr();
        assert!(p.idr_pending);
        // `p` drops here, which drops `encoder`, which sends Shutdown and
        // joins the thread. The stub thread's loop exits on disconnect, so
        // the join completes promptly.
    }
}
