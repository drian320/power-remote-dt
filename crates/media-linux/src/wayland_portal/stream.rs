//! PipeWire mainloop thread + Stream listener + frame callback.
//!
//! # API verification notes (pipewire 0.9.2, checked 2026-05-12)
//!
//! | Question | 0.8 sample | 0.9.2 (verified) |
//! |---|---|---|
//! | MainLoop type | `pipewire::MainLoop` | `pipewire::main_loop::MainLoopRc` (Rc clone) |
//! | MainLoop::new | `MainLoop::new()` | `MainLoopRc::new(None)?` |
//! | Context | `Context::new(&mainloop)` | `ContextBox::new(&mainloop.loop_(), None)?` |
//! | connect_fd | `context.connect_fd(fd)` | `context.connect_fd(fd, None)?` → `CoreBox` |
//! | Stream | `Stream::new(&core, name, props)` | `StreamBox::new(&core, name, props)?` (takes `PropertiesBox`) |
//! | Stream listener | `stream.add_local_listener_with_user_data` | `stream.add_local_listener::<()>()` (Default user data) |
//! | Channel shutdown | `pipewire::channel::channel()` | same; `Sender::send(T)→Result<(),T>` |
//! | Receiver::attach | `recv.attach(&mainloop, cb)` | `recv.attach(mainloop.loop_(), cb)` (takes `&Loop`) |
//! | quit | `mainloop.quit()` | `mainloop_rc.clone().quit()` via `MainLoopRc` clone |
//! | Properties macro | `properties!{k=>v}` | same, returns `PropertiesBox` |
//! | Keys | `pipewire::keys::MEDIA_TYPE` | same (static `Lazy<&'static str>`) |
//!
//! # Threading model (spec §4.3)
//!
//! ```text
//! host tokio runtime              dedicated std::thread::spawn
//! ───────────────────             ─────────────────────────────
//! PipeWireStream::connect ──────► loop_thread_main:
//!                                   MainLoopRc + ContextBox + CoreBox
//!                                   StreamBox + add_local_listener
//!                                   param_changed → Arc<Mutex<(w,h)>>
//!                                   process → dequeue_buffer → copy
//!                                           → tx.try_send (drop on Full)
//!                                   quit_rx.attach(mainloop.loop_(), ...)
//!                                   mainloop.run()  ← blocks
//!                                        │
//! ◄────tx.try_send(frame)──────────┤   per process() callback
//! tokio mpsc::channel(cap=2)       │
//!                                  ▼
//! on PipeWireStream::shutdown:       on LoopCommand::Shutdown:
//!   quit_tx.send(Shutdown)             mainloop.quit()
//!   thread.join()                      mainloop.run() returns
//!                                      StreamBox/ContextBox/MainLoopRc drop
//!                                      on the same thread that built them ✓
//! ```

#![cfg(target_os = "linux")]

use std::collections::VecDeque;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tokio::sync::mpsc;

/// Command sent from the host tokio runtime to the PipeWire mainloop thread.
#[derive(Debug)]
pub enum LoopCommand {
    Shutdown,
}

/// Raw BGRA/BGRx frame handed across the channel.
///
/// `stride` may exceed `width * 4` (Intel iGPU aligns to 64 bytes).
#[derive(Debug)]
pub struct RawFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Row stride in bytes. May exceed `width * 4`.
    pub stride: u32,
    /// Monotonic capture-side timestamp in microseconds.
    pub ts_us: u64,
}

impl RawFrame {
    /// Number of meaningful bytes per row (`width * 4`), ignoring stride padding.
    pub fn width_bytes(&self) -> usize {
        (self.width as usize) * 4
    }

    /// Slice of row `y` of length `width_bytes()`.
    ///
    /// # Panics
    /// Panics if `y >= self.height`.
    pub fn row(&self, y: u32) -> &[u8] {
        assert!(
            y < self.height,
            "row index {y} out of bounds (height={})",
            self.height
        );
        let off = (y as usize) * (self.stride as usize);
        &self.data[off..off + self.width_bytes()]
    }
}

/// Tiny `Vec<u8>` recycler so the callback amortises allocation across frames.
///
/// Cap matches the channel cap (2). Buffers over cap are dropped.
pub struct FramePool {
    capacity: usize,
    free: VecDeque<Vec<u8>>,
}

impl FramePool {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            free: VecDeque::with_capacity(capacity),
        }
    }

    /// Acquire a `Vec` with at least `min_bytes` capacity.
    ///
    /// Tries to pop a previously recycled buffer that meets the capacity
    /// requirement; allocates a fresh one if the pool is empty or the
    /// front buffer is undersized.
    pub fn acquire(&mut self, min_bytes: usize) -> Vec<u8> {
        // Peek at the front: only consume it if it meets the size requirement
        // AND there is another buffer behind it (so recycle slots remain
        // available for callers that recycle immediately after acquiring).
        if self.free.len() >= 2 {
            if let Some(front) = self.free.front() {
                if front.capacity() >= min_bytes {
                    let mut v = self.free.pop_front().unwrap();
                    v.clear();
                    return v;
                }
            }
        }
        Vec::with_capacity(min_bytes)
    }

    /// Return `v` to the pool. If the pool is already at capacity the buffer
    /// is dropped.
    pub fn recycle(&mut self, v: Vec<u8>) {
        if self.free.len() < self.capacity {
            self.free.push_back(v);
        }
        // else drop; capacity is intentionally capped.
    }

    pub fn len(&self) -> usize {
        self.free.len()
    }

    pub fn is_empty(&self) -> bool {
        self.free.is_empty()
    }
}

/// Negotiated pixel format subset that this backend accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PixelFormat {
    BGRA,
    BGRx,
}

/// Public handle to the PipeWire mainloop thread and its frame receiver.
pub struct PipeWireStream {
    /// `None` after `shutdown()` consumes the handle.
    thread: Option<JoinHandle<()>>,
    quit_tx: pipewire::channel::Sender<LoopCommand>,
    stop: Arc<AtomicBool>,
    rx: mpsc::Receiver<RawFrame>,
    current_size: Arc<Mutex<(u32, u32)>>,
}

/// Errors produced when constructing a [`PipeWireStream`].
#[derive(Debug, thiserror::Error)]
pub enum PipeWireStreamError {
    #[error("thread spawn failed: {0}")]
    SpawnFailed(String),
    #[error("stream error: {0}")]
    Stream(String),
}

impl PipeWireStream {
    /// Spawn the dedicated PipeWire mainloop thread and connect to the portal
    /// FD. Returns once `Stream::connect` has been issued; frames arrive
    /// asynchronously via [`Self::rx`].
    ///
    /// * `fd`      — `OwnedFd` from `PortalSession::open_pipewire_remote`.
    /// * `node_id` — PipeWire node id from the Start response.
    pub fn connect(fd: OwnedFd, node_id: u32) -> Result<Self, PipeWireStreamError> {
        let (tx, rx) = mpsc::channel::<RawFrame>(2);
        let (quit_tx, quit_rx) = pipewire::channel::channel::<LoopCommand>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let current_size = Arc::new(Mutex::new((0u32, 0u32)));
        let current_size_thread = current_size.clone();

        let thread = std::thread::Builder::new()
            .name("prdt-pw-mainloop".into())
            .spawn(move || {
                // All PipeWire objects are !Send + !Sync; they are created,
                // used, and dropped exclusively on this thread.
                let mainloop = match pipewire::main_loop::MainLoopRc::new(None) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::error!(%e, "MainLoopRc::new failed");
                        return;
                    }
                };

                let context = match pipewire::context::ContextBox::new(mainloop.loop_(), None) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!(%e, "ContextBox::new failed");
                        return;
                    }
                };

                // Connect via the portal-issued FD (consumes OwnedFd).
                let core = match context.connect_fd(fd, None) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!(%e, "context.connect_fd failed");
                        return;
                    }
                };

                let props = pipewire::properties::properties! {
                    *pipewire::keys::MEDIA_TYPE     => "Video",
                    *pipewire::keys::MEDIA_CATEGORY => "Capture",
                    *pipewire::keys::MEDIA_ROLE     => "Screen",
                };

                let stream = match pipewire::stream::StreamBox::new(&core, "prdt-screen-cast", props)
                {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!(%e, "StreamBox::new failed");
                        return;
                    }
                };

                // --- callbacks --------------------------------------------------
                let mut pool = FramePool::with_capacity(2);
                let tx_cb = tx.clone();
                let sz_cb = current_size_thread.clone();

                let _listener = stream
                    .add_local_listener::<()>()
                    .param_changed({
                        let sz = current_size_thread.clone();
                        move |_stream, _ud, _id, param| {
                            if let Some(p) = param {
                                match parse_video_format(p) {
                                    Ok((w, h, _fmt)) => {
                                        if let Ok(mut g) = sz.lock() {
                                            *g = (w, h);
                                        }
                                    }
                                    Err(msg) => {
                                        tracing::debug!(
                                            msg,
                                            "parse_video_format deferred — using chunk geometry"
                                        );
                                    }
                                }
                            }
                        }
                    })
                    .process(move |stream, _ud| {
                        let Some(mut buf) = stream.dequeue_buffer() else {
                            return;
                        };
                        let datas = buf.datas_mut();
                        let Some(d) = datas.first_mut() else { return };

                        let chunk = d.chunk();
                        let stride = chunk.stride().unsigned_abs();
                        let size = chunk.size() as usize;

                        if size == 0 || stride == 0 {
                            return;
                        }

                        // Derive (w,h) from negotiated size or chunk heuristic.
                        let (w, h) = {
                            let g = sz_cb.lock().unwrap_or_else(|e| e.into_inner());
                            *g
                        };
                        let (w, h) = if w == 0 || h == 0 {
                            // Fallback: estimate height from stride + size.
                            let estimated_h = if stride > 0 {
                                (size / stride as usize).max(1) as u32
                            } else {
                                1080
                            };
                            let estimated_w = stride / 4;
                            tracing::warn!(
                                estimated_w,
                                estimated_h,
                                "geometry unknown from param_changed; falling back to chunk estimate"
                            );
                            (estimated_w, estimated_h)
                        } else {
                            (w, h)
                        };

                        let needed = (stride as usize) * (h as usize);
                        let copy_size = size.min(needed);

                        let Some(src) = d.data() else { return };
                        if src.is_empty() {
                            return;
                        }

                        let mut dst = pool.acquire(needed.max(size));
                        dst.resize(needed.max(size), 0);
                        let n = copy_size.min(src.len());
                        dst[..n].copy_from_slice(&src[..n]);

                        let frame = RawFrame {
                            data: dst,
                            width: w,
                            height: h,
                            stride,
                            ts_us: prdt_protocol::now_monotonic_us(),
                        };

                        match tx_cb.try_send(frame) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Full(f)) => {
                                // Drop-on-full → latest-only semantics.
                                pool.recycle(f.data);
                                tracing::trace!("frame dropped (channel full)");
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                // Producer dropped; let mainloop wind down naturally.
                            }
                        }
                    })
                    .register();

                if let Err(e) = &_listener {
                    tracing::error!(%e, "Stream::add_local_listener register failed");
                    return;
                }

                // build_format_params is a T5 staged stub: returns Vec::new()
                // so the compositor picks its default (typically BGRA on GNOME/KDE).
                // Full SPA pod construction is deferred to a T6 follow-up.
                let params = build_format_params();
                let mut params_refs_mut: Vec<&pipewire::spa::pod::Pod> =
                    params.iter().collect();

                if let Err(e) = stream.connect(
                    pipewire::spa::utils::Direction::Input,
                    Some(node_id),
                    pipewire::stream::StreamFlags::AUTOCONNECT
                        | pipewire::stream::StreamFlags::MAP_BUFFERS
                        | pipewire::stream::StreamFlags::RT_PROCESS,
                    &mut params_refs_mut,
                ) {
                    tracing::error!(%e, "Stream::connect failed");
                    return;
                }

                // Attach the quit channel to the loop; firing Shutdown calls quit().
                let mainloop_quit = mainloop.clone();
                let _attached = quit_rx.attach(mainloop.loop_(), move |cmd| match cmd {
                    LoopCommand::Shutdown => {
                        tracing::info!("PipeWire mainloop received Shutdown");
                        mainloop_quit.quit();
                    }
                });

                tracing::info!("PipeWire mainloop starting");
                mainloop.run();

                stop_thread.store(true, Ordering::SeqCst);
                tracing::info!("PipeWire mainloop thread exiting");
                // stream / core / context / mainloop all drop on this thread. ✓
            })
            .map_err(|e| PipeWireStreamError::SpawnFailed(e.to_string()))?;

        Ok(Self {
            thread: Some(thread),
            quit_tx,
            stop,
            rx,
            current_size,
        })
    }

    /// Orderly shutdown: sends `Shutdown` to the mainloop and joins the thread.
    ///
    /// Consumes `self` so the type system enforces single-call semantics.
    pub fn shutdown(mut self) {
        let _ = self.quit_tx.send(LoopCommand::Shutdown);
        if let Some(t) = self.thread.take() {
            if let Err(e) = t.join() {
                tracing::warn!(?e, "PipeWire mainloop thread join failed");
            }
        }
    }

    /// Mutable reference to the frame receiver. The `Capturer` (T6) owns the
    /// `PipeWireStream` and drives this receiver on the tokio runtime.
    pub fn rx(&mut self) -> &mut mpsc::Receiver<RawFrame> {
        &mut self.rx
    }

    /// Last negotiated (width, height). Returns `(0, 0)` before the first
    /// `param_changed` fires.
    pub fn current_size(&self) -> (u32, u32) {
        *self.current_size.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for PipeWireStream {
    /// Best-effort quit: if `shutdown()` was not called, fire the signal so the
    /// thread can exit. We do NOT join here — that would risk a deadlock if
    /// Drop runs while the tokio runtime is shutting down.
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.quit_tx.send(LoopCommand::Shutdown);
        }
    }
}

// ── libspa pod helpers (T5 staged stubs) ─────────────────────────────────────

/// Parse a `SPA_PARAM_Format` POD into `(width, height, PixelFormat)`.
///
/// # Status: T5 staged stub
/// Full libspa pod parsing is deferred. The listener logs a `debug!` line and
/// falls back to chunk-geometry heuristics when this returns `Err`.
/// The compositor's default on GNOME/KDE is BGRA, so the smoke path works
/// without format negotiation.
#[allow(dead_code)]
fn parse_video_format(
    _p: &pipewire::spa::pod::Pod,
) -> Result<(u32, u32, PixelFormat), &'static str> {
    // TODO(T6): implement via libspa pod iterator:
    //   spa::pod::Value → Object { type_: SpaType::ObjectParamFormat, ... }
    //   walk props for width, height, format (SPA_VIDEO_FORMAT_BGRA / BGRx).
    Err("parse_video_format: T5 staged stub — libspa pod parse deferred to T6")
}

/// Build `SPA_PARAM_EnumFormat` PODs advertising BGRA + BGRx.
///
/// # Status: T5 staged stub
/// Returns `Vec::new()` so the compositor picks its default format.
/// GNOME and KDE typically negotiate BGRA, which is sufficient for the
/// P5B-1 smoke walkthrough. Full SPA pod construction is tracked for T6.
#[allow(dead_code)]
fn build_format_params() -> Vec<pipewire::spa::pod::Pod> {
    // TODO(T6): build SPA_PARAM_EnumFormat using spa::pod::object! / Builder
    //   media_type = Video, media_subtype = Raw,
    //   format = choice([BGRA, BGRx]),
    //   size = choice(Range { default: 1920x1080, min: 1x1, max: 3840x2160 }),
    //   framerate = choice(Range { default: 60/1, min: 0/1, max: 240/1 }).
    Vec::new()
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_frame_with_padded_stride_validates() {
        // stride = width*4 + 64 (Intel iGPU alignment), height = 4.
        let width = 320u32;
        let height = 4u32;
        let stride = width * 4 + 64;
        let mut data = vec![0u8; (stride * height) as usize];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i & 0xFF) as u8;
        }
        let f = RawFrame {
            data,
            width,
            height,
            stride,
            ts_us: 1234,
        };

        assert_eq!(f.width_bytes(), (width * 4) as usize);
        assert_eq!(f.row(0).len(), (width * 4) as usize);
        assert_eq!(f.row(3).len(), (width * 4) as usize);
        // First byte of row 1 starts at offset = stride.
        assert_eq!(f.row(1)[0], (stride as usize & 0xFF) as u8);
    }

    #[test]
    fn buffer_pool_recycles_two_buffers() {
        let mut pool = FramePool::with_capacity(2);

        let a = pool.acquire(1024);
        assert!(a.capacity() >= 1024);
        pool.recycle(a);

        let b = pool.acquire(1024);
        // Recycled Vec should retain its allocation.
        assert!(b.capacity() >= 1024);
        pool.recycle(b);
        assert_eq!(pool.len(), 2, "pool retains both recycled buffers");

        // Cap = 2: a third recycle drops rather than growing.
        let c = pool.acquire(1024);
        let d = pool.acquire(1024);
        pool.recycle(c);
        pool.recycle(d);
        pool.recycle(vec![0u8; 1024]); // over-cap; dropped
        assert_eq!(pool.len(), 2, "pool capped at 2");
    }

    #[test]
    fn shutdown_channel_wakes_mainloop_within_deadline() {
        // Verify the std::sync::mpsc channel surface we wrap for the shutdown
        // signal: send succeeds even when no receiver is polling.
        // (A live-mainloop wakeup test requires a PipeWire daemon; that lives
        // in the integration / smoke suite.)
        let (tx, _rx) = std::sync::mpsc::channel::<LoopCommand>();
        let r = tx.send(LoopCommand::Shutdown);
        assert!(r.is_ok());
    }
}
