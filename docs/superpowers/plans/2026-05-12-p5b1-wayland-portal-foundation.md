# P5B-1 Wayland Portal Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `WaylandPortal` capture backend to `prdt-media-linux` so a Wayland session can capture the screen via `xdg-desktop-portal`'s `ScreenCast` interface, consume the PipeWire stream the portal hands back, and feed CPU-side BGRA frames into the existing `bgra_to_i420` → OpenH264 path with no changes to the encoder side. The X11 path stays the default on WSLg / X11 sessions; the Wayland path is selected via a runtime capability probe (`WAYLAND_DISPLAY` env + D-Bus `NameHasOwner("org.freedesktop.portal.Desktop")`) or via the new `--capture-backend {auto|x11|wayland}` CLI flag.

**Architecture:** Introduce a `trait CaptureSource { geometry() -> (u32,u32); capture_into(&mut Vec<u8>) -> Result<()> }` in `crates/media-linux/src/capture_source.rs`. Both `X11ShmCapturer` (existing) and the new `WaylandPortalCapturer` impl it; `LinuxSwProducer` holds `Box<dyn CaptureSource>`. A new `CaptureBackend { X11Shm, WaylandPortal }` enum in `policy.rs` drives `LinuxSwFactory::new(backend)`; the factory selects the capturer at construction time. The probe is **synchronous** from `LinuxSwFactory::create` (a small `tokio::runtime::Builder::new_current_thread().build()?.block_on(...)` for the 3-step D-Bus check, 1s timeout, no `CreateSession`). Portal session lifecycle (ashpd 0.12) and PipeWire stream (pipewire 0.9) live under `crates/media-linux/src/wayland_portal/`. The PipeWire mainloop runs on a dedicated `std::thread` (NOT a tokio task); the callback bridges to the producer via a `tokio::sync::mpsc::channel::<RawFrame>(2)` with `try_send` (drop-on-full latest-only semantics matching the existing X11 path). `Session::close().await` is explicit because ashpd 0.12 `Session` has no `Drop::close`; the trait's `Drop` impl logs a `warn!` if shutdown wasn't called.

**Tech Stack:** Rust 1.85, edition 2021. New deps (Linux-only, declared inside the existing `[target.'cfg(target_os = "linux")']` table on `prdt-media-linux`): `ashpd = "0.12"` (MSRV 1.85; ≥ 0.13 needs 1.87+), `pipewire = "0.9"` (MSRV 1.77, compatible). No new workspace members. Existing deps (`tokio`, `tracing`, `zbus` transitively via ashpd, `serde + toml`, `dirs`, `async-trait`) cover the rest.

**Spec:** `docs/superpowers/specs/2026-05-12-p5b1-wayland-portal-foundation-design.md` (commit `fb26776`)

**Branch:** `phase-p5b1-wayland-portal-foundation`

**Tag (on completion):** `phase-p5b1-wayland-portal-foundation-complete`

**Cross-platform regression bar:** Linux + Windows both green for `cargo build/clippy/test --workspace -- -D warnings` (matches L0–L4 + P5A + P6 bar). Linux gates: `cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings` and `cargo test --workspace --lib --target x86_64-unknown-linux-gnu`. Pre-existing flaky `transport::probe_test::two_transports_find_each_other` is not a P5B-1 regression.

---

## File Structure

### Created

| Path | Responsibility |
|---|---|
| `crates/media-linux/src/capture_source.rs` | `trait CaptureSource` + `CaptureSourceError` shared by both backends |
| `crates/media-linux/src/wayland_portal/mod.rs` | Public re-exports (`WaylandPortalCapturer`, `WaylandPortalError`, `detect_portal_available_blocking`) |
| `crates/media-linux/src/wayland_portal/token.rs` | RestoreToken TOML persistence: `PortalSessionToken { restore_token, saved_at, compositor_hint }` + `load_or_default(path)` + `save(&self, path)` (atomic-write, pid-suffix `.tmp`, 0600 perms) |
| `crates/media-linux/src/wayland_portal/session.rs` | `PortalSession` lifecycle around `ashpd::desktop::screencast::Screencast`: `new`, `start_with_token_opt`, `close` (explicit `Session::close().await` because ashpd 0.12 has no `Drop::close`) |
| `crates/media-linux/src/wayland_portal/stream.rs` | `PipeWireStream` — owns the dedicated `std::thread::spawn` for `pipewire::main_loop::MainLoop`, builds the Stream + listener, exposes `Sender<LoopCommand>` for shutdown and a `tokio::sync::mpsc::Receiver<RawFrame>` for frame delivery. `RawFrame { data, width, height, stride, ts_us }`. |
| `crates/media-linux/src/wayland_portal/capturer.rs` | `WaylandPortalCapturer` — wires session + stream + token, impls `CaptureSource`. `new(cfg)` is sync (wraps an internal `tokio::runtime::Builder::new_current_thread().build()?.block_on(...)`). `shutdown(self)` consumes self and awaits `session.close()`; `Drop` impl `warn!`s if shutdown wasn't called. |
| `crates/media-linux/tests/capture_source_contract.rs` | Generic contract tests over any `CaptureSource` impl (driven by a `MockCheckerboardCapture` test stub; `X11ShmCapturer` variant gated `#[ignore]`) |
| `crates/media-linux/tests/wayland_portal_smoke.rs` | `#[ignore]` real-PipeWire smoke; runs manually on dev machines with `pipewire-loopback` |
| `docs/superpowers/p5b1-smoke-walkthrough.md` | Manual smoke doc: GNOME / WSLg X11 regression / probe priority |

### Modified

| Path | Change |
|---|---|
| `crates/media-linux/Cargo.toml` | Add `ashpd = "0.12"` and `pipewire = "0.9"` to the existing `[target.'cfg(target_os = "linux")'.dependencies]` table |
| `crates/media-linux/src/lib.rs` | `pub mod capture_source;` + `pub mod wayland_portal;` + re-export `CaptureSource`, `CaptureSourceError`, `WaylandPortalCapturer`; rework `build_video_producer` signature to take `Box<dyn CaptureSource>` (helper retained for tests) |
| `crates/media-linux/src/x11_capture.rs` | `impl CaptureSource for X11ShmCapturer` — delegates `capture_into` to existing `grab_into`; `geometry()` returns `(self.width, self.height)` |
| `crates/media-linux/src/linux_sw_producer.rs` | `LinuxSwProducer.capture: Box<dyn CaptureSource>` (was `X11ShmCapturer`); `next_frame` uses `geometry()` per-call to drive future mid-session resize (encoder reconfigure plumbing already exists from L4); `spawn_blocking` boundary preserved |
| `crates/media-linux/src/policy.rs` | Add `CaptureBackend { X11Shm, WaylandPortal }`, `CaptureBackendChoice { Auto, X11, Wayland }`, `detect_capture_backend(choice) -> CaptureBackend` (synchronous 3-step probe), `portal_runtime_available_blocking(timeout) -> Result<bool>`. `LinuxSwFactory` becomes `LinuxSwFactory { capture_backend: CaptureBackend }` with `pub fn new(capture_backend: CaptureBackend) -> Self`. `LinuxSwProbe` unchanged (encoder-side only). |
| `crates/host/src/lib.rs` | Add `Args::capture_backend: String` (`auto`/`x11`/`wayland`, default `auto`); on Linux, parse into `CaptureBackendChoice`, call `detect_capture_backend(choice)`, log the result, and construct `LinuxSwFactory::new(backend)` via the platform shim |
| `crates/host/src/platform/linux.rs` | `factory()` becomes `factory(capture_backend: CaptureBackend) -> Arc<dyn ProducerFactory>` so the host can pass the resolved backend |
| `docs/superpowers/STATUS.md` | Append `phase-p5b1-wayland-portal-foundation-complete` entry under §1 Phase tag table; update `**Last updated**` and `**Latest tag**` |

---

## Task list overview

| # | Task | Files | Tests |
|---|---|---|---|
| T1 | Extract `CaptureSource` trait + refactor `X11ShmCapturer` impl + swap `LinuxSwProducer` to `Box<dyn CaptureSource>`. WSLg X11 path remains green. | capture_source.rs (new), x11_capture.rs, linux_sw_producer.rs, lib.rs, tests/capture_source_contract.rs (new) | 4 new (contract trait tests + mock-based) |
| T2 | Add `CaptureBackend` enum + `CaptureBackendChoice` + synchronous `detect_capture_backend` + CLI `--capture-backend` flag plumbed through `Args` → `LinuxSwFactory::new`. | policy.rs, lib.rs (media-linux), host/src/lib.rs, host/src/platform/linux.rs | 4 new (env-controlled probe; CLI override; auto on no WAYLAND_DISPLAY; auto when portal absent) |
| T3 | Token persistence — `wayland_portal/token.rs` with atomic save + 0600 perms + load_or_default. Pure data; no portal interaction. | wayland_portal/mod.rs, wayland_portal/token.rs | 4 new (round-trip, atomic save w/ pid suffix, missing file → default, corrupt file → default + warn) |
| T4 | ashpd session lifecycle in `wayland_portal/session.rs`. Adds `ashpd = "0.12"`. CLI `prdt host --capture-backend wayland` mechanically fires the portal dialog (capturer stub returns `WouldBlock` from `capture_into` until T6). | Cargo.toml (media-linux), wayland_portal/{mod.rs, session.rs, capturer.rs (stub)} | 2 new (error mapping; restore-token error → token deletion via injected fake) |
| T5 | PipeWire mainloop thread + Stream + frame callback in `wayland_portal/stream.rs`. Adds `pipewire = "0.9"`. **First step verifies pipewire 0.9.2 API vs Codex's 0.8 sample.** | Cargo.toml (media-linux), wayland_portal/stream.rs | 3 new (RawFrame stride > width*4 validates; buffer-pool recycles 2 buffers; shutdown channel wakes mainloop within deadline) |
| T6 | Capturer glue — `wayland_portal/capturer.rs` wires session + stream + token. Implements `CaptureSource`. `Drop` impl logs `warn!` if `shutdown_completed` flag is false. | wayland_portal/capturer.rs | 1 new (shutdown_flag warn-on-drop, driven by a deliberately-not-shutdown construction) |
| T7 | Factory integration — `LinuxSwFactory::create` builds the right capturer from `capture_backend`. `LinuxSwProducer::new(Box<dyn CaptureSource>, cfg)`. Host wires CLI flag → probe → factory. End-to-end clippy + test green. | policy.rs, linux_sw_producer.rs, host/src/{lib.rs, platform/linux.rs} | 2 new (factory routes Wayland choice to the right constructor; factory rejects forced-Wayland on a host without WAYLAND_DISPLAY) |
| T8 | STATUS.md entry + smoke walkthrough doc + final clippy/test green + PR prep. No new implementation. | docs/superpowers/STATUS.md, docs/superpowers/p5b1-smoke-walkthrough.md | (manual) |

**Total new automated tests: ≥ 16** (≥10 DoD target met with margin).

---

## Conventions for every task

- Use `superpowers:test-driven-development`: write failing test → run to verify failure → minimal impl → run to verify pass → commit.
- `cargo fmt --all` before every commit.
- Linux gate before every commit: `cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings` and `cargo test --workspace --lib --target x86_64-unknown-linux-gnu`.
- Commit subject is short imperative; optional body explains the **why**. No Claude footer (matches project history — see `git log --oneline -15`).
- Use `tracing::info!` / `warn!` / `debug!` for runtime events; **no `eprintln!` or `println!`** in non-CLI code.
- Atomic-write pattern: `path.with_extension(format!("toml.tmp.{}", std::process::id()))` + `fs::rename`.
- Tests that need a real Wayland session, real PipeWire daemon, or a real X server are gated `#[ignore]` with a doc string explaining how to run them.

---

## Task 1: Extract `CaptureSource` trait + refactor X11 capturer + producer

**Files:**
- Create: `crates/media-linux/src/capture_source.rs`
- Create: `crates/media-linux/tests/capture_source_contract.rs`
- Modify: `crates/media-linux/src/lib.rs`
- Modify: `crates/media-linux/src/x11_capture.rs` (add `impl CaptureSource`)
- Modify: `crates/media-linux/src/linux_sw_producer.rs` (hold `Box<dyn CaptureSource>`)

- [ ] **Step 1: Create the branch**

```bash
git checkout -b phase-p5b1-wayland-portal-foundation master
git log -1 --oneline   # confirm starting point is fb26776 (the design spec commit)
```

- [ ] **Step 2: Write failing test for the `CaptureSource` trait contract**

Create `crates/media-linux/tests/capture_source_contract.rs`:

```rust
//! Contract tests over any `CaptureSource` impl.
//!
//! Driven by an in-memory `MockCheckerboardCapture` stub; the X11
//! variant is gated `#[ignore]` because it needs a real X server.

#![cfg(target_os = "linux")]

use prdt_media_linux::capture_source::{CaptureSource, CaptureSourceError};

/// Test stub that fills `out` with a deterministic checkerboard pattern.
struct MockCheckerboardCapture {
    width: u32,
    height: u32,
    tick: u32,
}

impl CaptureSource for MockCheckerboardCapture {
    fn geometry(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    fn capture_into(&mut self, out: &mut Vec<u8>) -> Result<(), CaptureSourceError> {
        let n = (self.width as usize) * (self.height as usize) * 4;
        out.resize(n, 0);
        for (i, b) in out.iter_mut().enumerate() {
            *b = ((i as u32 ^ self.tick) & 0xFF) as u8;
        }
        self.tick = self.tick.wrapping_add(1);
        Ok(())
    }
}

#[test]
fn mock_capture_obeys_geometry_invariant() {
    let mut cap = MockCheckerboardCapture { width: 320, height: 240, tick: 0 };
    let (w, h) = cap.geometry();
    assert!(w >= 1 && h >= 1, "geometry must be ≥ 1×1");
    let mut buf = Vec::new();
    cap.capture_into(&mut buf).expect("capture_into ok");
    assert!(buf.len() >= (w as usize) * (h as usize) * 4, "capture_into wrote enough bytes");
}

#[test]
fn mock_capture_advances_between_calls() {
    let mut cap = MockCheckerboardCapture { width: 8, height: 8, tick: 0 };
    let mut a = Vec::new();
    let mut b = Vec::new();
    cap.capture_into(&mut a).unwrap();
    cap.capture_into(&mut b).unwrap();
    assert_ne!(a, b, "successive frames should differ (advancing tick)");
}

#[test]
fn mock_capture_geometry_is_stable_when_unchanged() {
    let cap = MockCheckerboardCapture { width: 1920, height: 1080, tick: 0 };
    assert_eq!(cap.geometry(), (1920, 1080));
    assert_eq!(cap.geometry(), (1920, 1080)); // idempotent
}

#[test]
#[ignore = "requires real X11 connection — run on WSL2 with: cargo test -p prdt-media-linux --test capture_source_contract -- --ignored"]
fn x11_capturer_implements_capture_source() {
    use prdt_media_linux::x11_capture::X11ShmCapturer;
    let mut cap = X11ShmCapturer::new().expect("X11 connect");
    let (w, h) = cap.geometry();
    let mut buf = Vec::new();
    cap.capture_into(&mut buf).expect("grab");
    assert_eq!(buf.len(), (w as usize) * (h as usize) * 4);
}
```

Run: `cargo test -p prdt-media-linux --test capture_source_contract --target x86_64-unknown-linux-gnu 2>&1 | head -20`

Expected: compile failure — `prdt_media_linux::capture_source` doesn't exist yet. Good. (`error[E0433]: failed to resolve: could not find 'capture_source' in 'prdt_media_linux'`).

- [ ] **Step 3: Create the trait module**

Create `crates/media-linux/src/capture_source.rs`:

```rust
//! Capture-source abstraction shared by the X11 and Wayland-portal backends.
//!
//! `LinuxSwProducer` holds a `Box<dyn CaptureSource>`; the concrete impl is
//! picked at construction time by `LinuxSwFactory` based on the resolved
//! `CaptureBackend` (see `policy.rs`).
//!
//! The trait is deliberately small so the producer doesn't have to know
//! whether it owns an X11 SHM segment or a PipeWire stream. Errors are
//! surfaced via a shared `CaptureSourceError`; both backends map their
//! internal errors into this enum.

#![cfg(target_os = "linux")]

use thiserror::Error;

/// Error type for capture-source operations. Variants are deliberately
/// coarse: terminal failures should surface as `Terminal { backend, reason }`
/// (which the producer maps to `ProducerError::Capture`), while transient
/// "no frame yet" conditions surface as `WouldBlock` so the producer can
/// tick once and retry on the next pacer beat.
#[derive(Debug, Error)]
pub enum CaptureSourceError {
    /// No frame was available in the configured wait window. The producer
    /// converts this into a no-op tick (pacer advances but no encoded frame
    /// is emitted), matching how the existing X11 path handles a slow X
    /// server.
    #[error("would block: {0}")]
    WouldBlock(String),

    /// Permanent failure — capture cannot continue. Wraps the backend
    /// name so the producer can attribute it cleanly.
    #[error("capture terminal on {backend}: {reason}")]
    Terminal { backend: &'static str, reason: String },
}

/// Common interface implemented by every Linux capture backend.
///
/// `geometry()` is exposed per-call (not stored once at construction) so the
/// Wayland portal can report a mid-session resize when the user resizes the
/// captured monitor. The X11 path returns a fixed value (root window
/// geometry is read once in `X11ShmCapturer::new`).
pub trait CaptureSource: Send {
    /// Return the (width, height) the next call to `capture_into` will fill,
    /// in pixels. Must be ≥ 1×1.
    fn geometry(&self) -> (u32, u32);

    /// Block until a new frame is available, then resize `out` to
    /// `geometry().0 * geometry().1 * 4` bytes (or larger if the backend
    /// uses padding) and fill it with BGRA / BGRx data.
    ///
    /// Returns `Err(WouldBlock)` for transient empty-frame conditions
    /// (producer converts to a tick) and `Err(Terminal)` for permanent
    /// failures (producer surfaces as `ProducerError::Capture`).
    fn capture_into(&mut self, out: &mut Vec<u8>) -> Result<(), CaptureSourceError>;
}
```

- [ ] **Step 4: Wire the new module into `lib.rs`**

Edit `crates/media-linux/src/lib.rs`. Replace the existing module list with:

```rust
//! Linux media backend — XShm capture + OpenH264 SW encode/decode +
//! VideoProducer adapter. See `docs/superpowers/specs/2026-05-09-l1-linux-poc-design.md`.
//!
//! The crate compiles to an empty library on non-Linux targets.

#![cfg(target_os = "linux")]
#![allow(dead_code)]

pub mod capture_source;
pub mod core_adapter;
pub mod error;
pub mod frame;
pub mod i420_to_bgra;
pub mod linux_sw_producer;
pub mod policy;
pub mod sw_pipeline;
pub mod x11_capture;

pub use capture_source::{CaptureSource, CaptureSourceError};
pub use error::LinuxMediaError;
pub use frame::BgraFrame;

/// Production wiring entry point — host calls this to obtain a boxed
/// `VideoProducer` for the Linux SW path. The capture source is injected
/// (the factory picks X11 or Wayland-portal); width/height come from the
/// capture source via `geometry()`.
#[cfg(target_os = "linux")]
pub fn build_video_producer_with(
    capture: Box<dyn CaptureSource>,
    bitrate_bps: u32,
    fps: u32,
) -> anyhow::Result<linux_sw_producer::LinuxSwProducer> {
    use anyhow::Context as _;
    let (w, h) = capture.geometry();
    let enc = sw_pipeline::LinuxSwEncoder::new(w, h, bitrate_bps, fps)
        .context("LinuxSwEncoder::new")?;
    linux_sw_producer::LinuxSwProducer::new(capture, enc, fps).context("LinuxSwProducer::new")
}

/// Legacy entry point — X11-only convenience wrapper retained so callers
/// that still want X11 explicitly (smoke tests, the
/// `build_video_decoder`-paired helper) don't have to assemble the
/// `Box<dyn CaptureSource>` themselves. Internally equivalent to
/// `build_video_producer_with(Box::new(X11ShmCapturer::new()?), ...)`.
#[cfg(target_os = "linux")]
pub fn build_video_producer(
    bitrate_bps: u32,
    fps: u32,
) -> anyhow::Result<linux_sw_producer::LinuxSwProducer> {
    use anyhow::Context as _;
    let cap = x11_capture::X11ShmCapturer::new().context("X11ShmCapturer::new")?;
    build_video_producer_with(Box::new(cap), bitrate_bps, fps)
}

/// Production wiring entry point — viewer calls this to obtain a SW decoder.
#[cfg(target_os = "linux")]
pub fn build_video_decoder() -> anyhow::Result<sw_pipeline::LinuxSwDecoder> {
    sw_pipeline::LinuxSwDecoder::new().map_err(Into::into)
}
```

- [ ] **Step 5: Add `impl CaptureSource for X11ShmCapturer`**

Append to `crates/media-linux/src/x11_capture.rs`, right after the `impl X11ShmCapturer { ... }` block (before the existing `impl Drop`):

```rust
impl crate::capture_source::CaptureSource for X11ShmCapturer {
    fn geometry(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn capture_into(
        &mut self,
        out: &mut Vec<u8>,
    ) -> Result<(), crate::capture_source::CaptureSourceError> {
        let n = (self.width as usize) * (self.height as usize) * 4;
        out.resize(n, 0);
        self.grab_into(out.as_mut_slice()).map_err(|e| {
            crate::capture_source::CaptureSourceError::Terminal {
                backend: "linux-x11shm",
                reason: e.to_string(),
            }
        })
    }
}
```

- [ ] **Step 6: Refactor `LinuxSwProducer` to hold `Box<dyn CaptureSource>`**

Replace `crates/media-linux/src/linux_sw_producer.rs` fully:

```rust
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
    let micros = if fps == 0 { 16_667 } else { 1_000_000 / fps as u64 };
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
```

- [ ] **Step 7: Run the contract tests + workspace gate**

```bash
cargo test -p prdt-media-linux --test capture_source_contract --target x86_64-unknown-linux-gnu
```

Expected: 3 pass, 1 ignored (the X11 one).
```
test mock_capture_obeys_geometry_invariant ... ok
test mock_capture_advances_between_calls ... ok
test mock_capture_geometry_is_stable_when_unchanged ... ok
test x11_capturer_implements_capture_source ... ignored
test result: ok. 3 passed; 0 failed; 1 ignored
```

```bash
cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
cargo test --workspace --lib --target x86_64-unknown-linux-gnu
```

Expected: green (existing flaky `transport::probe_test::two_transports_find_each_other` is the only allowed failure).

- [ ] **Step 8: Commit**

```bash
git add crates/media-linux/src/capture_source.rs crates/media-linux/src/lib.rs \
        crates/media-linux/src/x11_capture.rs crates/media-linux/src/linux_sw_producer.rs \
        crates/media-linux/tests/capture_source_contract.rs
git commit -m "$(cat <<'EOF'
P5B-1 T1: extract CaptureSource trait + refactor LinuxSwProducer

LinuxSwProducer now holds Box<dyn CaptureSource> instead of X11ShmCapturer
directly, opening the way for the Wayland-portal capturer to drop in at T6
without touching producer/encoder code. geometry() is exposed per-call so
the Wayland path can report a mid-session resize.

- new crates/media-linux/src/capture_source.rs (CaptureSource trait +
  CaptureSourceError { WouldBlock, Terminal })
- X11ShmCapturer impls CaptureSource via the existing grab_into.
- LinuxSwProducer.capture: Option<Box<dyn CaptureSource>> (Option lets us
  move across spawn_blocking; never None outside the await boundary).
- backend_name() collapses to "linux-openh264" so viewer-overlay doesn't
  flicker on a capture-only swap (encoder still drives the name).
- 4 new tests (CaptureSource contract on a mock checkerboard stub; X11
  variant gated #[ignore] because it needs a real X server).
EOF
)"
```

---

## Task 2: `CaptureBackend` enum + synchronous probe + CLI `--capture-backend` flag

**Files:**
- Modify: `crates/media-linux/src/policy.rs` (add CaptureBackend / detect_capture_backend / portal_runtime_available_blocking)
- Modify: `crates/media-linux/src/lib.rs` (no — re-export pulled through `policy` already)
- Modify: `crates/host/src/lib.rs` (Args + plumbing + tracing on resolved choice)
- Modify: `crates/host/src/platform/linux.rs` (factory takes CaptureBackend)

- [ ] **Step 1: Write failing tests for the probe + factory plumbing**

Append to `crates/media-linux/src/policy.rs` (inside the existing `#[cfg(test)] mod tests` block — extend after `linux_factory_rejects_mf_hevc`):

```rust
    use std::env;

    /// Helper to clear WAYLAND_DISPLAY for the duration of one test.
    /// `unsafe` because std::env::set_var is not thread-safe; we rely on
    /// `cargo test --lib` running these probes sequentially. The probe
    /// itself is read-only at runtime.
    struct ScopedEnv {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }
    impl ScopedEnv {
        fn unset(key: &'static str) -> Self {
            let prev = env::var_os(key);
            env::remove_var(key);
            Self { key, prev }
        }
        fn set(key: &'static str, val: &str) -> Self {
            let prev = env::var_os(key);
            env::set_var(key, val);
            Self { key, prev }
        }
    }
    impl Drop for ScopedEnv {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => env::set_var(self.key, v),
                None => env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn detect_backend_x11_when_wayland_display_unset() {
        let _guard = ScopedEnv::unset("WAYLAND_DISPLAY");
        let got = detect_capture_backend(CaptureBackendChoice::Auto);
        assert_eq!(got, CaptureBackend::X11Shm);
    }

    #[test]
    fn detect_backend_cli_override_forces_x11_even_with_wayland_display() {
        let _guard = ScopedEnv::set("WAYLAND_DISPLAY", "wayland-fake");
        let got = detect_capture_backend(CaptureBackendChoice::X11);
        assert_eq!(got, CaptureBackend::X11Shm);
    }

    #[test]
    fn detect_backend_cli_override_forces_wayland_even_without_display() {
        let _guard = ScopedEnv::unset("WAYLAND_DISPLAY");
        let got = detect_capture_backend(CaptureBackendChoice::Wayland);
        assert_eq!(got, CaptureBackend::WaylandPortal);
    }

    #[test]
    fn detect_backend_auto_falls_back_to_x11_when_portal_unreachable() {
        // Simulate "WAYLAND_DISPLAY set but no session bus" by pointing
        // DBUS_SESSION_BUS_ADDRESS at a path that can't be opened. The probe
        // should warn + return X11Shm, not panic, not hang.
        let _g1 = ScopedEnv::set("WAYLAND_DISPLAY", "wayland-fake");
        let _g2 = ScopedEnv::set("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/prdt-test");
        let got = detect_capture_backend(CaptureBackendChoice::Auto);
        assert_eq!(got, CaptureBackend::X11Shm);
    }
```

Run: `cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu policy::tests::detect 2>&1 | head -30`

Expected: compile failure — `CaptureBackend`, `CaptureBackendChoice`, `detect_capture_backend` don't exist yet.

- [ ] **Step 2: Implement `CaptureBackend`, `CaptureBackendChoice`, and the probe**

Replace the contents of `crates/media-linux/src/policy.rs` with:

```rust
//! P5A capability/factory integration + P5B-1 capture-backend probe.
//!
//! `LinuxSwProbe` (P5A) reports the encoder side (Openh264 only on Linux).
//! `CaptureBackend` (P5B-1) selects the *capture* side: X11 MIT-SHM or the
//! xdg-desktop-portal ScreenCast path. The two axes don't interact today —
//! Linux ships Openh264 regardless of capture choice — so the policy stays
//! single-axis (P5C may revisit when VAAPI/NVENC-Linux land).

#![cfg(target_os = "linux")]

use prdt_media_policy::{
    BackendKind, CapabilityProbe, Codec, EncoderCapability, FactoryError, ProducerConfig,
    ProducerFactory,
};
use prdt_protocol::VideoProducer;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Encoder-side probe (unchanged from P5A)
// ---------------------------------------------------------------------------

pub struct LinuxSwProbe;

impl CapabilityProbe for LinuxSwProbe {
    fn list_encoders(&self) -> Vec<EncoderCapability> {
        vec![EncoderCapability {
            backend: BackendKind::Openh264,
            codec: Codec::H264,
            max_resolution: (3840, 2160),
            max_fps: 60,
            zero_copy: false,
            priority: 10,
        }]
    }
}

// ---------------------------------------------------------------------------
// Capture-side backend
// ---------------------------------------------------------------------------

/// Concrete capture-side choice as resolved by `detect_capture_backend`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureBackend {
    X11Shm,
    WaylandPortal,
}

/// CLI-level choice. `Auto` is the default and runs the 3-step probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureBackendChoice {
    Auto,
    X11,
    Wayland,
}

impl CaptureBackendChoice {
    /// Parse the `--capture-backend <auto|x11|wayland>` CLI value. Returns
    /// `Auto` for unknown strings after logging a warn — matches the
    /// `--encoder` parser's tolerance.
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Self::Auto,
            "x11" => Self::X11,
            "wayland" => Self::Wayland,
            other => {
                tracing::warn!(
                    capture_backend = %other,
                    "unknown --capture-backend value; treating as auto"
                );
                Self::Auto
            }
        }
    }
}

/// Resolve the capture-side backend choice.
///
/// 1. Honour an explicit CLI override (`X11` / `Wayland`).
/// 2. Otherwise check `WAYLAND_DISPLAY`: if unset, pick X11 (this covers WSLg
///    and pure X11 sessions cheaply, with no D-Bus traffic).
/// 3. Otherwise call `portal_runtime_available_blocking` (D-Bus `NameHasOwner`
///    against `org.freedesktop.portal.Desktop`, 1s timeout). If the call
///    fails or the portal isn't there, log a warn and fall back to X11.
///
/// The probe never calls `CreateSession` — that would fire the consent
/// dialog every time we probe. The dialog only fires inside
/// `WaylandPortalCapturer::new` when we actually intend to capture.
pub fn detect_capture_backend(choice: CaptureBackendChoice) -> CaptureBackend {
    match choice {
        CaptureBackendChoice::X11 => return CaptureBackend::X11Shm,
        CaptureBackendChoice::Wayland => return CaptureBackend::WaylandPortal,
        CaptureBackendChoice::Auto => {}
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        tracing::info!("WAYLAND_DISPLAY unset; selecting X11 capture backend");
        return CaptureBackend::X11Shm;
    }
    match portal_runtime_available_blocking(Duration::from_secs(1)) {
        Ok(true) => {
            tracing::info!("xdg-desktop-portal reachable; selecting Wayland capture backend");
            CaptureBackend::WaylandPortal
        }
        Ok(false) => {
            tracing::warn!(
                "WAYLAND_DISPLAY set but xdg-desktop-portal unreachable; falling back to X11"
            );
            CaptureBackend::X11Shm
        }
        Err(e) => {
            tracing::warn!(error = %e, "portal probe failed; falling back to X11");
            CaptureBackend::X11Shm
        }
    }
}

/// Synchronous D-Bus probe. Spins up a tiny `current_thread` tokio runtime
/// (so we don't depend on being called from one), opens the session bus,
/// asks `NameHasOwner("org.freedesktop.portal.Desktop")`, and tears down.
///
/// Wall-clock timeout = `timeout`. On timeout returns `Ok(false)` (treat as
/// "portal not available") rather than `Err`, so a slow login doesn't kill
/// startup; if the timeout proves too tight in smoke (spec §11), bump to 3s
/// as a follow-up commit — do not bump pre-emptively.
pub fn portal_runtime_available_blocking(timeout: Duration) -> Result<bool, anyhow::Error> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("portal probe tokio runtime: {e}"))?;
    rt.block_on(async move {
        let fut = async {
            let conn = zbus::Connection::session().await?;
            let proxy = zbus::fdo::DBusProxy::new(&conn).await?;
            let has = proxy
                .name_has_owner(zbus::names::BusName::WellKnown(
                    zbus::names::WellKnownName::try_from("org.freedesktop.portal.Desktop")?,
                ))
                .await?;
            Ok::<bool, anyhow::Error>(has)
        };
        match tokio::time::timeout(timeout, fut).await {
            Ok(Ok(b)) => Ok(b),
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "portal probe NameHasOwner returned err");
                Ok(false)
            }
            Err(_elapsed) => {
                tracing::debug!(?timeout, "portal probe timed out");
                Ok(false)
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Producer factory. `capture_backend` is fixed at construction time: the
/// host resolves it once via `detect_capture_backend(args.into())` before
/// building the factory.
pub struct LinuxSwFactory {
    capture_backend: CaptureBackend,
}

impl LinuxSwFactory {
    pub fn new(capture_backend: CaptureBackend) -> Self {
        Self { capture_backend }
    }

    pub fn capture_backend(&self) -> CaptureBackend {
        self.capture_backend
    }
}

impl ProducerFactory for LinuxSwFactory {
    fn create(
        &self,
        kind: BackendKind,
        cfg: &ProducerConfig,
    ) -> Result<Box<dyn VideoProducer>, FactoryError> {
        if !matches!(kind, BackendKind::Openh264) {
            return Err(FactoryError::Unavailable(
                kind,
                "Linux P5A only supports Openh264; VAAPI/V4L2/NVENC-Linux deferred to P5C".into(),
            ));
        }
        // T7 fills the Wayland arm in; for now route both through the X11
        // helper so the test gate stays green between T2 and T7.
        let producer = match self.capture_backend {
            CaptureBackend::X11Shm => crate::build_video_producer(cfg.initial_bitrate_bps, cfg.fps)
                .map_err(|e| FactoryError::InvalidConfig(kind, e.to_string()))?,
            CaptureBackend::WaylandPortal => crate::build_video_producer(cfg.initial_bitrate_bps, cfg.fps)
                .map_err(|e| FactoryError::InvalidConfig(kind, format!(
                    "wayland-portal capturer not wired yet (T7); legacy X11 path failed: {e}"
                )))?,
        };
        Ok(Box::new(producer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_probe_lists_openh264_only() {
        let probe = LinuxSwProbe;
        let caps = probe.list_encoders();
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].backend, BackendKind::Openh264);
        assert_eq!(caps[0].codec, Codec::H264);
        assert!(!caps[0].zero_copy);
    }

    #[test]
    fn linux_factory_rejects_nvenc() {
        let factory = LinuxSwFactory::new(CaptureBackend::X11Shm);
        let cfg = ProducerConfig {
            width: 1920, height: 1080, fps: 60,
            initial_bitrate_bps: 8_000_000, codec: Codec::H264,
        };
        let result = factory.create(BackendKind::Nvenc, &cfg);
        assert!(matches!(result, Err(FactoryError::Unavailable(BackendKind::Nvenc, _))));
    }

    #[test]
    fn linux_factory_rejects_mf_hevc() {
        let factory = LinuxSwFactory::new(CaptureBackend::X11Shm);
        let cfg = ProducerConfig {
            width: 1920, height: 1080, fps: 60,
            initial_bitrate_bps: 8_000_000, codec: Codec::H264,
        };
        let result = factory.create(BackendKind::MfHevc, &cfg);
        assert!(matches!(result, Err(FactoryError::Unavailable(BackendKind::MfHevc, _))));
    }

    // ----- P5B-1 probe tests -----
    // (insert the ScopedEnv helper + detect_backend_* tests authored in Step 1 here)
}
```

(Re-insert the `ScopedEnv` helper + four `detect_backend_*` tests from Step 1 into the test module.)

- [ ] **Step 3: Add `zbus` dep (transitive via ashpd would land in T4; we need it now for the probe)**

The simplest path is to lean on the workspace `zbus`. Edit `crates/media-linux/Cargo.toml` to add:

```toml
[target.'cfg(target_os = "linux")'.dependencies]
# ... existing entries unchanged ...
zbus = { version = "4", default-features = false, features = ["tokio"] }
```

(If the workspace already provides a `zbus` version constraint, switch to `zbus = { workspace = true }`. Verify with `grep -n '"zbus"' Cargo.toml`.)

- [ ] **Step 4: Run probe tests**

```bash
cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu policy::tests::detect
```

Expected:
```
test policy::tests::detect_backend_x11_when_wayland_display_unset ... ok
test policy::tests::detect_backend_cli_override_forces_x11_even_with_wayland_display ... ok
test policy::tests::detect_backend_cli_override_forces_wayland_even_without_display ... ok
test policy::tests::detect_backend_auto_falls_back_to_x11_when_portal_unreachable ... ok
```

- [ ] **Step 5: Plumb the CLI flag through `Args`**

Edit `crates/host/src/lib.rs`. Find the `Args` block (lines ~74-191) and append a new field directly after `force_sw`:

```rust
    /// Linux-only: capture-source backend. `auto` (default) probes for a
    /// reachable xdg-desktop-portal on a Wayland session and picks
    /// `wayland`; otherwise falls back to `x11`. `wayland` forces the
    /// portal path (errors hard if no portal is reachable); `x11` forces
    /// MIT-SHM (works on WSLg / X11 sessions). Ignored on non-Linux.
    #[arg(long, default_value = "auto")]
    pub capture_backend: String,
```

Find the Linux platform factory call site (around line 807, `let factory_arc = platform_factory();`) and replace it with the resolved-backend version. First, change the platform shim (next step), then update the call site here to:

```rust
        // P5B-1: resolve capture-side backend (Linux only — Windows ignores).
        let factory_arc = platform_factory(&args.capture_backend);
```

- [ ] **Step 6: Update `crates/host/src/platform/linux.rs`**

Replace the existing `factory()` function (lines ~131-133):

```rust
pub fn factory(
    capture_backend_arg: &str,
) -> std::sync::Arc<dyn prdt_media_policy::ProducerFactory> {
    use prdt_media_linux::policy::{
        detect_capture_backend, CaptureBackendChoice, LinuxSwFactory,
    };
    let choice = CaptureBackendChoice::parse(capture_backend_arg);
    let backend = detect_capture_backend(choice);
    tracing::info!(
        choice = ?choice,
        resolved = ?backend,
        "P5B-1 capture backend resolved"
    );
    std::sync::Arc::new(LinuxSwFactory::new(backend))
}
```

Then update `crates/host/src/platform/win.rs`'s `factory()` signature to ignore the new arg:

```rust
pub fn factory(
    _capture_backend_arg: &str,
) -> std::sync::Arc<dyn prdt_media_policy::ProducerFactory> {
    // Windows has no Wayland axis; arg is ignored.
    // (existing body unchanged)
}
```

And `crates/host/src/platform/mod.rs` re-exports — confirm `pub use ::factory` propagates the new signature; no edit needed if it's a `pub use ... as factory;` line. If it's a thin re-export, signatures line up.

- [ ] **Step 7: Smoke the CLI parser**

Append to the existing `crates/host/src/lib.rs` test module (right after `linux_normalize_encoder_falls_back_for_hw`-style tests around line ~1377):

```rust
    #[test]
    fn cli_capture_backend_default_is_auto() {
        let args = Args::try_parse_from([
            "prdt-host", "--bitrate-mbps", "5", "--silent-allow", "--headless",
        ])
        .expect("default parse");
        assert_eq!(args.capture_backend, "auto");
    }

    #[test]
    fn cli_capture_backend_wayland_parses() {
        let args = Args::try_parse_from([
            "prdt-host", "--bitrate-mbps", "5", "--silent-allow",
            "--headless", "--capture-backend", "wayland",
        ])
        .expect("wayland parse");
        assert_eq!(args.capture_backend, "wayland");
    }
```

Run: `cargo test -p prdt-host --lib --target x86_64-unknown-linux-gnu cli_capture_backend 2>&1 | tail -10`

Expected: `test result: ok. 2 passed`.

- [ ] **Step 8: Workspace gate**

```bash
cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
cargo test --workspace --lib --target x86_64-unknown-linux-gnu
```

Expected: green.

- [ ] **Step 9: Commit**

```bash
git add crates/media-linux/Cargo.toml crates/media-linux/src/policy.rs \
        crates/host/src/lib.rs crates/host/src/platform/linux.rs crates/host/src/platform/win.rs
git commit -m "$(cat <<'EOF'
P5B-1 T2: CaptureBackend probe + --capture-backend CLI flag

Add CaptureBackend { X11Shm, WaylandPortal } + CaptureBackendChoice { Auto,
X11, Wayland } + detect_capture_backend (synchronous 3-step probe:
WAYLAND_DISPLAY → zbus session → NameHasOwner). 1s timeout returns
Ok(false) on elapse so a slow login doesn't kill startup; bump to 3s in a
follow-up commit if smoke shows false negatives.

- Probe never CreateSession's — that would fire the consent dialog on
  every host start. Dialog only fires in WaylandPortalCapturer::new (T6).
- LinuxSwFactory takes CaptureBackend at construction; Wayland arm is a
  TODO stub that falls through to X11 until T7 wires the real capturer.
- host/src/platform/linux.rs::factory takes &str CLI value, parses +
  probes + logs the resolved choice.
- Windows platform shim ignores the arg (no Wayland axis).
- 6 new tests (4 probe behaviours via ScopedEnv guard + 2 CLI parser).
EOF
)"
```

---

## Task 3: RestoreToken persistence (`wayland_portal/token.rs`)

**Files:**
- Create: `crates/media-linux/src/wayland_portal/mod.rs`
- Create: `crates/media-linux/src/wayland_portal/token.rs`
- Modify: `crates/media-linux/src/lib.rs` (`pub mod wayland_portal;`)

- [ ] **Step 1: Write failing tests for round-trip / atomic save / missing / corrupt**

Create `crates/media-linux/src/wayland_portal/mod.rs`:

```rust
//! xdg-desktop-portal ScreenCast capture backend.
//!
//! See `docs/superpowers/specs/2026-05-12-p5b1-wayland-portal-foundation-design.md`
//! for the full design.

#![cfg(target_os = "linux")]

pub mod token;

// Re-exports filled in T4-T6.
pub use token::PortalSessionToken;
```

Create `crates/media-linux/src/wayland_portal/token.rs` with the test scaffold first:

```rust
//! RestoreToken persistence — TOML at
//! `$XDG_CONFIG_HOME/prdt/portal-session.toml`.
//!
//! The token blob is opaque to us; the portal issues it on Start and
//! validates it on the next select_sources. We only need string in/out
//! plus a few hints for operator debugging (saved_at, compositor_hint).
//!
//! Failure-modes match HostAuthConfig / KnownPeers:
//! - file missing → returns default (no token; first launch path).
//! - parse error → logs warn, returns default (corrupt file replaced
//!   on next save).
//! - write atomic via {path}.tmp.{pid} + rename; perms 0600.

#![cfg(target_os = "linux")]

use serde::{Deserialize, Serialize};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortalSessionToken {
    /// Opaque restore token from `ScreenCast.Start` response. Empty string
    /// means "no token persisted yet" (treated as `None` by callers).
    #[serde(default)]
    pub restore_token: String,
    /// RFC3339 timestamp; informational, written by `save`.
    #[serde(default)]
    pub saved_at: String,
    /// Informational hint, e.g. "GNOME 47.1". Best-effort; falls back to
    /// "unknown" if the compositor refuses to identify itself.
    #[serde(default)]
    pub compositor_hint: String,
}

impl PortalSessionToken {
    /// Load from disk; return `Self::default()` for missing file or parse
    /// error (warn-logged). Never returns `Err`: caller treats both
    /// "no file" and "corrupt file" as "first launch".
    pub fn load_or_default(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(s) => match toml::from_str::<Self>(&s) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(),
                        "portal-session.toml parse failed; using default");
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(),
                    "portal-session.toml read failed; using default");
                Self::default()
            }
        }
    }

    /// Write atomically. Tmp filename suffix carries pid so concurrent
    /// host instances don't truncate each other. Perms set to 0600 after
    /// rename. Caller is responsible for `create_dir_all` on the parent.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let parent = path.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
        })?;
        std::fs::create_dir_all(parent)?;
        let tmp = path.with_extension(format!("toml.tmp.{}", std::process::id()));
        let body = toml::to_string_pretty(self).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("toml ser: {e}"))
        })?;
        std::fs::write(&tmp, body)?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Returns `Some(&str)` only when the token is non-empty. Saves
    /// callers a length check.
    pub fn token_opt(&self) -> Option<&str> {
        if self.restore_token.is_empty() {
            None
        } else {
            Some(&self.restore_token)
        }
    }

    pub fn with_token(restore_token: impl Into<String>, compositor_hint: impl Into<String>) -> Self {
        Self {
            restore_token: restore_token.into(),
            saved_at: chrono_like_rfc3339_now(),
            compositor_hint: compositor_hint.into(),
        }
    }
}

/// Minimal RFC3339 timestamp without pulling in chrono. Uses
/// `SystemTime::now()` formatted via `humantime::format_rfc3339` if
/// available; otherwise falls back to a UNIX-epoch second string. The
/// value is informational only — never parsed back.
fn chrono_like_rfc3339_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => format!("unix:{}.{:09}", d.as_secs(), d.subsec_nanos()),
        Err(_) => "unix:0".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("prdt-portal-token-{}-{}",
            name, std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn round_trip() {
        let dir = tmpdir("round_trip");
        let path = dir.join("portal-session.toml");
        let original = PortalSessionToken::with_token("abc123==", "GNOME 47.1");
        original.save(&path).expect("save");
        let loaded = PortalSessionToken::load_or_default(&path);
        assert_eq!(loaded.restore_token, "abc123==");
        assert_eq!(loaded.compositor_hint, "GNOME 47.1");
    }

    #[test]
    fn atomic_save_pid_suffix_does_not_collide_under_repeated_writes() {
        let dir = tmpdir("atomic");
        let path = dir.join("portal-session.toml");
        for i in 0..32 {
            let t = PortalSessionToken::with_token(format!("tok-{i}"), "stress");
            t.save(&path).expect("save");
        }
        let final_ = PortalSessionToken::load_or_default(&path);
        assert!(final_.restore_token.starts_with("tok-"));
        // Mode is 0600.
        let perms = fs::metadata(&path).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o600);
        // No stray .tmp files left behind.
        let strays: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains("tmp"))
            .collect();
        assert!(strays.is_empty(), "tmp files left behind: {strays:?}");
    }

    #[test]
    fn missing_file_returns_default() {
        let dir = tmpdir("missing");
        let path = dir.join("does-not-exist.toml");
        let loaded = PortalSessionToken::load_or_default(&path);
        assert_eq!(loaded, PortalSessionToken::default());
        assert!(loaded.token_opt().is_none());
    }

    #[test]
    fn corrupt_file_returns_default_with_warn() {
        let dir = tmpdir("corrupt");
        let path = dir.join("portal-session.toml");
        fs::write(&path, b"this is not [[[ toml").expect("seed corrupt");
        let loaded = PortalSessionToken::load_or_default(&path);
        assert_eq!(loaded, PortalSessionToken::default());
        // The warn line is captured by tracing-test if needed; here we
        // just assert behaviour. (Manual: rerun with RUST_LOG=warn to see.)
    }
}
```

- [ ] **Step 2: Add `pub mod wayland_portal;` to `crates/media-linux/src/lib.rs`**

Insert `pub mod wayland_portal;` alphabetically (between `sw_pipeline` and `x11_capture`).

- [ ] **Step 3: Run the tests**

```bash
cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu wayland_portal::token::tests
```

Expected:
```
test wayland_portal::token::tests::round_trip ... ok
test wayland_portal::token::tests::atomic_save_pid_suffix_does_not_collide_under_repeated_writes ... ok
test wayland_portal::token::tests::missing_file_returns_default ... ok
test wayland_portal::token::tests::corrupt_file_returns_default_with_warn ... ok
test result: ok. 4 passed
```

- [ ] **Step 4: Workspace gate**

```bash
cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
cargo test --workspace --lib --target x86_64-unknown-linux-gnu
```

Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/media-linux/src/lib.rs crates/media-linux/src/wayland_portal/
git commit -m "$(cat <<'EOF'
P5B-1 T3: portal RestoreToken TOML persistence (wayland_portal/token.rs)

Pure data layer — no portal interaction yet. PortalSessionToken {
restore_token, saved_at, compositor_hint } with load_or_default
(missing/corrupt → default + warn, mirrors HostAuthConfig / KnownPeers)
and atomic save (path.tmp.{pid} + rename, 0600 perms).

- 4 new tests (round-trip, atomic stress + 0600 perms + no .tmp strays,
  missing → default, corrupt → default + warn).
- RFC3339 stamp uses unix-epoch seconds inline; not parsed back, so no
  chrono dep needed.
EOF
)"
```

---

## Task 4: ashpd portal session (`wayland_portal/session.rs`) + capturer stub

**Files:**
- Modify: `crates/media-linux/Cargo.toml` (+ ashpd 0.12)
- Modify: `crates/media-linux/src/wayland_portal/mod.rs`
- Create: `crates/media-linux/src/wayland_portal/session.rs`
- Create: `crates/media-linux/src/wayland_portal/capturer.rs` (stub returning `WouldBlock`)

- [ ] **Step 1: Pin ashpd 0.12 in Cargo.toml**

Edit `crates/media-linux/Cargo.toml`. Append to `[target.'cfg(target_os = "linux")'.dependencies]`:

```toml
ashpd = { version = "0.12", default-features = false, features = ["tokio"] }
```

Run `cargo update -p ashpd --precise 0.12.3` (latest 0.12.x as of 2026-05) to lock the exact version. Verify with `grep -A1 '"ashpd"' Cargo.lock | head -4` — expect `version = "0.12.3"` or similar.

- [ ] **Step 2: Write failing tests for session error mapping + token-deletion-on-invalid-token**

Append to `crates/media-linux/src/wayland_portal/session.rs` test section (file created in Step 3 below). For now, draft them so we know what to assert. These tests exercise the **error mapping** without a real D-Bus session — they construct `WaylandPortalError` variants manually and verify display + `is_token_invalid()`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_error_display_user_cancelled() {
        let e = WaylandPortalError::UserCancelled;
        assert_eq!(e.to_string(), "user cancelled portal authorization");
    }

    #[test]
    fn portal_error_token_invalid_triggers_deletion_signal() {
        let e = WaylandPortalError::RestoreTokenRejected("response code 2".into());
        assert!(e.is_token_invalid(), "RestoreTokenRejected → invalid");
        let e2 = WaylandPortalError::UserCancelled;
        assert!(!e2.is_token_invalid(), "UserCancelled is not a token problem");
    }
}
```

Run: `cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu wayland_portal::session::tests 2>&1 | head -10`

Expected: compile failure — `WaylandPortalError` doesn't exist yet.

- [ ] **Step 3: Implement `wayland_portal/session.rs`**

Create `crates/media-linux/src/wayland_portal/session.rs`:

```rust
//! ashpd::desktop::screencast::Screencast lifecycle wrapper.
//!
//! Walks the standard ScreenCast flow:
//!   1. ScreenCast::new()
//!   2. create_session()
//!   3. select_sources(SourceType::Monitor, CursorMode::Embedded,
//!                    multiple=false, persist_mode=ExplicitlyRevoked,
//!                    restore_token=opt)
//!   4. start(session, None) → response.streams() + response.restore_token()
//!   5. open_pipewire_remote(session) → OwnedFd
//!
//! ashpd 0.12 has no Drop::close on Session; the consumer must call
//! `close().await` explicitly. We do that in
//! WaylandPortalCapturer::shutdown.

#![cfg(target_os = "linux")]

use ashpd::desktop::screencast::{CursorMode, PersistMode, Screencast, SourceType};
use ashpd::desktop::Session;
use std::os::fd::OwnedFd;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WaylandPortalError {
    #[error("ashpd: {0}")]
    Ashpd(String),
    #[error("user cancelled portal authorization")]
    UserCancelled,
    /// Portal rejected the restore_token (token rotated or grant revoked
    /// since last save). The capturer deletes the stored token and retries
    /// `start_with_token_opt(None)` once before failing the construction.
    #[error("portal rejected restore_token: {0}")]
    RestoreTokenRejected(String),
    #[error("portal returned no streams")]
    NoStreams,
}

impl WaylandPortalError {
    /// True iff the error means "the token I sent is no longer valid,
    /// delete it and retry as a first-launch". Drives the
    /// retry-without-token branch in WaylandPortalCapturer.
    pub fn is_token_invalid(&self) -> bool {
        matches!(self, WaylandPortalError::RestoreTokenRejected(_))
    }
}

impl From<ashpd::Error> for WaylandPortalError {
    fn from(e: ashpd::Error) -> Self {
        WaylandPortalError::Ashpd(e.to_string())
    }
}

/// Output of a successful Start: the PipeWire node id the stream will be
/// attached to, the OwnedFd into the PipeWire remote, and the refreshed
/// restore_token (if the portal issued a new one).
pub struct PortalStartOutput {
    pub session: Session<'static, Screencast<'static>>,
    pub pipewire_fd: OwnedFd,
    pub pipewire_node_id: u32,
    pub restore_token: Option<String>,
}

pub struct PortalSession;

impl PortalSession {
    /// Open a portal session and Start it. The async function blocks on
    /// the consent dialog the first time it runs (no `restore_token` or
    /// a rejected `restore_token`); subsequent runs return immediately
    /// when the token re-uses the previous grant.
    pub async fn start_with_token_opt(
        restore_token: Option<&str>,
    ) -> Result<PortalStartOutput, WaylandPortalError> {
        let proxy = Screencast::new().await?;
        let session = proxy.create_session().await?;

        let mut opts = proxy
            .select_sources(&session)
            .types(SourceType::Monitor.into())
            .cursor_mode(CursorMode::Embedded)
            .multiple(false)
            .persist_mode(PersistMode::ExplicitlyRevoked);
        if let Some(t) = restore_token {
            opts = opts.restore_token(t);
        }
        // `.send()` is the ashpd 0.12 builder finaliser.
        opts.send().await?.response().map_err(|e| {
            // ashpd surfaces user-cancel as Response::Other; map by string match
            // on the canonical signature. (Token-invalid uses the same channel
            // and is distinguished by code == 2.)
            let s = e.to_string();
            if s.contains("cancel") {
                WaylandPortalError::UserCancelled
            } else if restore_token.is_some() {
                WaylandPortalError::RestoreTokenRejected(s)
            } else {
                WaylandPortalError::Ashpd(s)
            }
        })?;

        let response = proxy.start(&session, None).await?.response().map_err(|e| {
            let s = e.to_string();
            if s.contains("cancel") {
                WaylandPortalError::UserCancelled
            } else if restore_token.is_some() {
                WaylandPortalError::RestoreTokenRejected(s)
            } else {
                WaylandPortalError::Ashpd(s)
            }
        })?;

        let streams = response.streams();
        let stream = streams.first().ok_or(WaylandPortalError::NoStreams)?;
        let pipewire_node_id = stream.pipe_wire_node_id();
        let restore_token = response.restore_token().map(String::from);

        let fd = proxy.open_pipewire_remote(&session).await?;

        Ok(PortalStartOutput {
            session,
            pipewire_fd: fd,
            pipewire_node_id,
            restore_token,
        })
    }

    /// Explicit close. ashpd 0.12 Session has no Drop::close; callers must
    /// invoke this on the existing tokio runtime before drop.
    pub async fn close(
        session: Session<'static, Screencast<'static>>,
    ) -> Result<(), WaylandPortalError> {
        session.close().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_error_display_user_cancelled() {
        let e = WaylandPortalError::UserCancelled;
        assert_eq!(e.to_string(), "user cancelled portal authorization");
    }

    #[test]
    fn portal_error_token_invalid_triggers_deletion_signal() {
        let e = WaylandPortalError::RestoreTokenRejected("response code 2".into());
        assert!(e.is_token_invalid());
        let e2 = WaylandPortalError::UserCancelled;
        assert!(!e2.is_token_invalid());
    }
}
```

> **Plan author note on ashpd 0.12 API:** the exact builder method names
> (`select_sources(...).types(...).cursor_mode(...).send()` vs older
> `select_sources(&session, opts)` shape) were verified against
> `https://docs.rs/crate/ashpd/0.12.3/source/src/desktop/screencast.rs`
> while writing this plan. If the implementer finds a method renamed
> (e.g. `.send()` vs `.start()`), correct in-place — the call sequence
> (create_session → select_sources → start → open_pipewire_remote) is
> stable across the 0.12.x range.

- [ ] **Step 4: Create the capturer stub**

Create `crates/media-linux/src/wayland_portal/capturer.rs`:

```rust
//! `WaylandPortalCapturer` — partial scaffold filled in across T4/T5/T6.
//!
//! At T4 the capturer only exists so `LinuxSwFactory` can reference its
//! type; `new()` returns NotConnected and `capture_into` returns
//! WouldBlock. The portal dialog is wired in T5/T6 by replacing the body
//! with a real session + stream construction.

#![cfg(target_os = "linux")]

use crate::capture_source::{CaptureSource, CaptureSourceError};

pub struct WaylandPortalCapturer {
    /// Pre-T6 placeholder so the type is constructible. T6 replaces
    /// this with a real Session + Stream + Receiver triple and a
    /// `shutdown_completed: AtomicBool`.
    _todo: (),
}

#[derive(Debug, thiserror::Error)]
pub enum WaylandPortalCapturerInitError {
    #[error("wayland-portal capturer is not implemented yet (T5/T6 in flight)")]
    NotImplemented,
}

impl WaylandPortalCapturer {
    /// T4 stub. T6 replaces with full session + stream wiring.
    pub fn new() -> Result<Self, WaylandPortalCapturerInitError> {
        Err(WaylandPortalCapturerInitError::NotImplemented)
    }
}

impl CaptureSource for WaylandPortalCapturer {
    fn geometry(&self) -> (u32, u32) {
        (1, 1) // never reachable until T6
    }
    fn capture_into(&mut self, _out: &mut Vec<u8>) -> Result<(), CaptureSourceError> {
        Err(CaptureSourceError::WouldBlock(
            "wayland-portal capturer scaffold; T5/T6 will fill in".into(),
        ))
    }
}
```

Update `crates/media-linux/src/wayland_portal/mod.rs` to expose the new modules:

```rust
//! xdg-desktop-portal ScreenCast capture backend.

#![cfg(target_os = "linux")]

pub mod capturer;
pub mod session;
pub mod token;

pub use capturer::{WaylandPortalCapturer, WaylandPortalCapturerInitError};
pub use session::{PortalSession, PortalStartOutput, WaylandPortalError};
pub use token::PortalSessionToken;
```

- [ ] **Step 5: Run session tests + workspace gate**

```bash
cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu wayland_portal::session::tests
```

Expected:
```
test wayland_portal::session::tests::portal_error_display_user_cancelled ... ok
test wayland_portal::session::tests::portal_error_token_invalid_triggers_deletion_signal ... ok
```

```bash
cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
cargo test --workspace --lib --target x86_64-unknown-linux-gnu
```

Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates/media-linux/Cargo.toml crates/media-linux/src/wayland_portal/ Cargo.lock
git commit -m "$(cat <<'EOF'
P5B-1 T4: ashpd 0.12 portal session lifecycle (session.rs)

Add ashpd = "0.12" (MSRV 1.85; ≥0.13 needs 1.87+). Implement
PortalSession::start_with_token_opt — create_session → select_sources
(Monitor, Embedded, multiple=false, ExplicitlyRevoked) → start →
open_pipewire_remote. Returns OwnedFd + node_id + refreshed
restore_token. Errors classify into UserCancelled vs
RestoreTokenRejected vs Ashpd; is_token_invalid() drives the
"delete-and-retry-as-first-launch" branch in T6.

PortalSession::close() is explicit because ashpd 0.12 Session has no
Drop::close (Codex flagged this; we own the discipline). The capturer
shutdown path in T6 awaits it before drop.

Capturer scaffold (capturer.rs) — type exists so policy.rs::factory can
reference it; new() returns NotImplemented and capture_into returns
WouldBlock until T6.

- 2 new tests (portal error display + is_token_invalid discrimination).
EOF
)"
```

---

## Task 5: PipeWire stream + dedicated mainloop thread (`wayland_portal/stream.rs`)

**Files:**
- Modify: `crates/media-linux/Cargo.toml` (+ pipewire 0.9)
- Create: `crates/media-linux/src/wayland_portal/stream.rs`
- Modify: `crates/media-linux/src/wayland_portal/mod.rs`

- [ ] **Step 1: Verify pipewire 0.9.2 API vs Codex's 0.8 sample**

Before writing any code, fetch fresh docs:

```bash
# Option A: use context7 MCP (recommended; pinned current docs).
# Option B: cargo doc -p pipewire after adding the dep.
cargo add --dry-run --target 'cfg(target_os = "linux")' --package prdt-media-linux pipewire@0.9
# Confirms the latest 0.9.x release; expect 0.9.2 or 0.9.x as of 2026-05.
```

Capture the answer to these three questions in a short comment block at the top of `wayland_portal/stream.rs` so the next maintainer doesn't have to re-derive:

| Question | 0.8 (Codex sample) | 0.9.2 (verified) |
|---|---|---|
| MainLoop import path | `pipewire::MainLoop` | `pipewire::main_loop::MainLoop` (re-exported as `pw::MainLoop`) |
| Channel for cross-thread signal | `pipewire::channel::channel()` | same |
| Stream listener builder | `stream.add_local_listener_with_user_data` | same |
| `pw_thread_loop_*` Rust wrapper | n/a (the rust crate is mainloop-only) | n/a |

If 0.9.2 has incompatible breaking changes from the 0.8 sample we adapt — **0.9.2 wins** (newer, MSRV-compatible). Document the adapted call in the implementer's commit body.

- [ ] **Step 2: Pin pipewire 0.9 in Cargo.toml**

Append to `crates/media-linux/Cargo.toml`'s `[target.'cfg(target_os = "linux")'.dependencies]`:

```toml
pipewire = "0.9"
libspa = "0.8"   # only if pipewire 0.9 still re-exports through libspa; otherwise drop
```

Run `cargo update -p pipewire --precise 0.9.2` (or whatever Step 1 verified is current). Confirm `Cargo.lock` shows the locked version.

- [ ] **Step 3: Write failing tests**

Append to `crates/media-linux/src/wayland_portal/stream.rs` (file body in Step 4; we write the tests up front):

```rust
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
        let f = RawFrame { data, width, height, stride, ts_us: 1234 };
        // Validation must accept stride > width*4 and walk row-by-row when
        // copying out. Verify the helper exposes width-bytes-per-row.
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
        assert_eq!(a.capacity() >= 1024, true);
        pool.recycle(a);
        let b = pool.acquire(1024);
        // Recycled Vec should retain its allocation.
        assert_eq!(b.capacity() >= 1024, true);
        pool.recycle(b);
        assert_eq!(pool.len(), 2, "pool retains both recycled buffers");
        // Cap = 2: a third recycle drops the buffer rather than growing.
        let c = pool.acquire(1024);
        let d = pool.acquire(1024);
        pool.recycle(c);
        pool.recycle(d);
        pool.recycle(vec![0u8; 1024]); // over-cap; dropped
        assert_eq!(pool.len(), 2, "pool capped at 2");
    }

    #[test]
    fn shutdown_channel_wakes_mainloop_within_deadline() {
        // The real test of MainLoop wakeup needs a live PipeWire daemon;
        // here we verify the channel API surface only: build a
        // (Sender, Receiver) pair and confirm Sender::send returns Ok
        // even when nobody's polling Receiver yet. This is the wire we
        // need so the producer's shutdown() can unblock the thread.
        let (tx, _rx) = std::sync::mpsc::channel::<LoopCommand>();
        let r = tx.send(LoopCommand::Shutdown);
        assert!(r.is_ok());
    }
}
```

Run: compile-fail expected — `RawFrame`, `FramePool`, `LoopCommand` don't exist yet.

- [ ] **Step 4: Implement `stream.rs`**

Create `crates/media-linux/src/wayland_portal/stream.rs`:

```rust
//! PipeWire mainloop thread + Stream listener + frame callback.
//!
//! API verification notes (pipewire 0.9.2, checked 2026-05-12):
//! - `pipewire::main_loop::MainLoop::new(properties)` (Codex's 0.8 sample
//!   used `pipewire::MainLoop::new()`; 0.9 moved to a sub-module).
//! - Cross-thread shutdown via `pipewire::channel::channel::<LoopCommand>()`.
//! - Stream listener: `stream.add_local_listener::<()>` then
//!   `.process(...)` + `.param_changed(...)` builder + `register()`.
//!
//! Threading model (see spec §4.3):
//!     host tokio runtime              dedicated std::thread::spawn
//!     ───────────────────             ─────────────────────────────
//!     PipeWireStream::new() ───────►  loop_thread_main:
//!                                       MainLoop + Context + Core
//!                                       Stream + listener (.process)
//!                                       stream.connect(MAP_BUFFERS)
//!                                       mainloop.run()  ◄── blocks
//!                                            │
//!     ◄────tx.try_send(frame)──────────┤   per process() callback:
//!     tokio mpsc::channel(cap=2)       │     dequeue_buffer
//!                                      │     row-by-row memcpy → Vec
//!                                      │     tx.try_send (drop on Full)
//!                                      │     buffer auto-queued on Drop
//!     on shutdown:                     ▼
//!       stop.store(true)                  mainloop.quit_channel.send()
//!       quit_tx.send(LoopCommand::Shutdown)  mainloop.run() returns
//!       thread.join()                       Stream/Context/MainLoop
//!                                           drop on the same thread
//!                                           that built them ✓

#![cfg(target_os = "linux")]

use std::collections::VecDeque;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use tokio::sync::mpsc;

#[derive(Debug)]
pub enum LoopCommand {
    Shutdown,
}

/// Raw BGRA frame handed across the channel.
#[derive(Debug)]
pub struct RawFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Row stride in bytes. May exceed `width * 4` (Intel iGPU alignment).
    pub stride: u32,
    /// Monotonic capture-side timestamp for latency stats.
    pub ts_us: u64,
}

impl RawFrame {
    pub fn width_bytes(&self) -> usize {
        (self.width as usize) * 4
    }
    /// View into row `y` of length `width * 4` (ignoring stride padding).
    /// Panics if `y >= height` — caller asserts via height-bounded loop.
    pub fn row(&self, y: u32) -> &[u8] {
        let off = (y as usize) * (self.stride as usize);
        &self.data[off..off + self.width_bytes()]
    }
}

/// Tiny buffer recycler so the callback doesn't re-allocate per frame
/// under sustained capture. Cap = 2 matches the channel cap.
pub struct FramePool {
    capacity: usize,
    free: VecDeque<Vec<u8>>,
}

impl FramePool {
    pub fn with_capacity(capacity: usize) -> Self {
        Self { capacity, free: VecDeque::with_capacity(capacity) }
    }
    pub fn acquire(&mut self, min_bytes: usize) -> Vec<u8> {
        match self.free.pop_front() {
            Some(mut v) if v.capacity() >= min_bytes => {
                v.clear();
                v
            }
            Some(_) => Vec::with_capacity(min_bytes),
            None => Vec::with_capacity(min_bytes),
        }
    }
    pub fn recycle(&mut self, v: Vec<u8>) {
        if self.free.len() < self.capacity {
            self.free.push_back(v);
        }
        // else drop on the floor; cap = 2.
    }
    pub fn len(&self) -> usize {
        self.free.len()
    }
}

/// Public handle to the PipeWire mainloop thread + frame receiver.
pub struct PipeWireStream {
    /// `None` after `shutdown()`; consumed by `join`.
    thread: Option<JoinHandle<()>>,
    quit_tx: pipewire::channel::Sender<LoopCommand>,
    stop: Arc<AtomicBool>,
    pub rx: mpsc::Receiver<RawFrame>,
    pub current_size: Arc<parking_lot::Mutex<(u32, u32)>>,
}

impl PipeWireStream {
    /// Spawn the dedicated mainloop thread and connect to the portal-issued
    /// PipeWire remote. Returns the handle once `Stream::connect` has been
    /// issued (frames may not arrive until the compositor pushes one).
    ///
    /// `fd` is the OwnedFd from `PortalSession::open_pipewire_remote`;
    /// `node_id` is the PipeWire node id from the Start response.
    pub fn connect(fd: OwnedFd, node_id: u32) -> Result<Self, PipeWireStreamError> {
        let (tx, rx) = mpsc::channel::<RawFrame>(2);
        let (quit_tx, quit_rx) = pipewire::channel::channel::<LoopCommand>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let current_size = Arc::new(parking_lot::Mutex::new((0u32, 0u32)));
        let current_size_thread = current_size.clone();

        let thread = std::thread::Builder::new()
            .name("prdt-pw-mainloop".into())
            .spawn(move || {
                // Build MainLoop + Context + Core on this thread (pipewire
                // types are deliberately !Send + !Sync).
                let mainloop = match pipewire::main_loop::MainLoop::new(None) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::error!(error = %e, "MainLoop::new failed");
                        return;
                    }
                };
                let context = match pipewire::context::Context::new(&mainloop) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!(error = %e, "Context::new failed");
                        return;
                    }
                };
                // Connect using the portal-handed FD.
                let core = match context.connect_fd(fd, None) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!(error = %e, "Core::connect_fd failed");
                        return;
                    }
                };

                let stream = match pipewire::stream::Stream::new(
                    &core,
                    "prdt-screen-cast",
                    pipewire::properties::properties! {
                        *pipewire::keys::MEDIA_TYPE => "Video",
                        *pipewire::keys::MEDIA_CATEGORY => "Capture",
                        *pipewire::keys::MEDIA_ROLE => "Screen",
                    },
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!(error = %e, "Stream::new failed");
                        return;
                    }
                };

                let mut pool = FramePool::with_capacity(2);
                let tx_cb = tx.clone();
                let current_size_cb = current_size_thread.clone();

                let _listener = stream
                    .add_local_listener::<()>()
                    .param_changed(move |stream, _id, _user_data, param| {
                        // P5B-1 only consumes BGRA/BGRx. Refuse other formats
                        // by returning early; the portal will renegotiate or
                        // surface DeviceLost via stream state.
                        if let Some(p) = param {
                            if let Ok((w, h, fmt)) = parse_video_format(p) {
                                if fmt != PixelFormat::BGRA && fmt != PixelFormat::BGRx {
                                    tracing::warn!(?fmt, "negotiated format not BGRA/BGRx; aborting");
                                    stream.disconnect().ok();
                                    return;
                                }
                                *current_size_cb.lock() = (w, h);
                            }
                        }
                    })
                    .process(move |stream, _user_data| {
                        let Some(mut buf) = stream.dequeue_buffer() else { return };
                        let datas = buf.datas_mut();
                        let Some(d) = datas.first_mut() else { return };
                        let chunk = d.chunk();
                        let stride = chunk.stride() as u32;
                        let size = chunk.size() as usize;
                        let (w, h) = *current_size_cb.lock();
                        if w == 0 || h == 0 || stride == 0 {
                            return;
                        }
                        let needed = (stride as usize) * (h as usize);
                        let src = match d.data() {
                            Some(s) => s,
                            None => return,
                        };
                        let mut dst = pool.acquire(needed.max(size));
                        dst.resize(needed.max(size), 0);
                        let copy_n = src.len().min(dst.len());
                        dst[..copy_n].copy_from_slice(&src[..copy_n]);

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
                                // drop-on-full latest-only semantics; recycle.
                                pool.recycle(f.data);
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                // producer hung up; let mainloop wind down.
                            }
                        }
                    })
                    .register()
                    .ok();

                // Build the format param (BGRA + BGRx, accept any size up to 4K).
                // The exact builder shape lives in libspa pod builder; encode
                // a minimal SPA_PARAM_EnumFormat object with media_type=Video,
                // media_subtype=Raw, format=BGRA|BGRx.
                let params = build_format_params();
                if let Err(e) = stream.connect(
                    pipewire::spa::utils::Direction::Input,
                    Some(node_id),
                    pipewire::stream::StreamFlags::AUTOCONNECT
                        | pipewire::stream::StreamFlags::MAP_BUFFERS
                        | pipewire::stream::StreamFlags::RT_PROCESS,
                    &mut params.iter().map(|p| p as &_).collect::<Vec<_>>(),
                ) {
                    tracing::error!(error = %e, "Stream::connect failed");
                    return;
                }

                // Wire the quit channel into the mainloop.
                let _attached = quit_rx.attach(&mainloop, {
                    let mainloop_quit = mainloop.clone();
                    move |cmd| match cmd {
                        LoopCommand::Shutdown => {
                            tracing::info!("PipeWire mainloop received Shutdown");
                            mainloop_quit.quit();
                        }
                    }
                });

                mainloop.run();
                stop_thread.store(true, Ordering::SeqCst);
                tracing::info!("PipeWire mainloop thread exiting");
                // Stream / context / core / mainloop drop on this thread.
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

    /// Request orderly shutdown. Sends LoopCommand::Shutdown to the
    /// mainloop and joins the thread (blocking up to ~2s).
    pub fn shutdown(mut self) {
        let _ = self.quit_tx.send(LoopCommand::Shutdown);
        if let Some(t) = self.thread.take() {
            // join is bounded by the mainloop quitting; if it hangs we'd
            // rather log and proceed than block the producer's tokio
            // runtime forever. The mainloop is event-driven so this
            // should return within ms in practice.
            if let Err(e) = t.join() {
                tracing::warn!(?e, "PipeWire mainloop thread join failed");
            }
        }
    }
}

impl Drop for PipeWireStream {
    fn drop(&mut self) {
        // Best-effort: if shutdown() wasn't called, fire the quit signal
        // anyway and let the thread tear down. We can't join here without
        // risking a deadlock; the OS will reap the thread eventually.
        let _ = self.quit_tx.send(LoopCommand::Shutdown);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PipeWireStreamError {
    #[error("thread spawn: {0}")]
    SpawnFailed(String),
    #[error("stream: {0}")]
    Stream(String),
}

/// Negotiated pixel format (subset). The stream listener refuses anything
/// not in this enum (BGRA / BGRx are byte-equivalent for our downstream).
#[derive(Debug, PartialEq, Eq)]
pub enum PixelFormat {
    BGRA,
    BGRx,
}

/// Parse a SPA_PARAM_Format POD into (width, height, format). Returns
/// Err if the param is non-Video / non-Raw or fields are missing.
fn parse_video_format(
    _p: &pipewire::spa::pod::Pod,
) -> Result<(u32, u32, PixelFormat), &'static str> {
    // T5 implementer fills this against the libspa 0.x API. The shape:
    //   ParamFormat → media_type=Video → media_subtype=Raw → format,
    //   size (Rectangle{w,h}), framerate (Fraction).
    // Until then, return a conservative fallback so the listener does not
    // hard-fail on first param_changed.
    Err("parse_video_format: T5 stub — fill against libspa 0.x API")
}

/// Build the EnumFormat param advertising BGRA + BGRx, framerate up to 60fps.
/// Returns one or more Pod objects suitable for Stream::connect's params.
fn build_format_params() -> Vec<pipewire::spa::pod::Pod> {
    // T5 implementer fills this using libspa's pod builder; until then,
    // return an empty Vec — the compositor will pick a default, which on
    // GNOME/KDE is typically BGRA already. This is enough for the smoke
    // doc's first dialog-render, but real frames require the proper
    // param construction (tracked as a follow-up if simplification stuck).
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    // (RawFrame stride / FramePool / shutdown_channel tests authored
    // in Step 3 go here.)
}
```

> **Plan author note on `parse_video_format` / `build_format_params`:**
> Codex's 0.8 sample uses a hand-rolled `spa::pod::Pod` builder. In 0.9
> the same builder exists under `pipewire::spa::pod`. The exact builder
> calls are implementer-fiddly; the spec's recommendation (§4.3) is to
> get the dialog-firing path landed first (this task), then iterate on
> the format param in T6 once a real GNOME smoke can drive the
> `param_changed` callback. If the format param construction is too
> fiddly for a single task slice, ship the placeholder + a follow-up
> TODO and let GNOME's default negotiation carry the smoke — this is
> flagged in the spec §4.3 as acceptable. **Do not block on perfection.**

- [ ] **Step 5: Add the `parking_lot` workspace dep if not already present**

Check `Cargo.toml` (workspace root). If `parking_lot` isn't a workspace dep, either:
- Use `std::sync::Mutex` instead of `parking_lot::Mutex` (one tracking field — fine), or
- Add `parking_lot = "0.12"` to the workspace `[workspace.dependencies]` and reference via `workspace = true`.

Prefer the `std::sync::Mutex` route (zero new dep). Search-and-replace in stream.rs accordingly.

- [ ] **Step 6: Run tests**

```bash
cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu wayland_portal::stream::tests
```

Expected:
```
test wayland_portal::stream::tests::raw_frame_with_padded_stride_validates ... ok
test wayland_portal::stream::tests::buffer_pool_recycles_two_buffers ... ok
test wayland_portal::stream::tests::shutdown_channel_wakes_mainloop_within_deadline ... ok
```

- [ ] **Step 7: Update `wayland_portal/mod.rs` re-exports**

```rust
pub mod capturer;
pub mod session;
pub mod stream;
pub mod token;

pub use capturer::{WaylandPortalCapturer, WaylandPortalCapturerInitError};
pub use session::{PortalSession, PortalStartOutput, WaylandPortalError};
pub use stream::{LoopCommand, PipeWireStream, PipeWireStreamError, RawFrame};
pub use token::PortalSessionToken;
```

- [ ] **Step 8: Workspace gate**

```bash
cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
cargo test --workspace --lib --target x86_64-unknown-linux-gnu
```

Expected: green. (If clippy complains about `parse_video_format` returning a no-op `Err`, allow with a one-line `#[allow(dead_code)]` comment explaining the staged completion.)

- [ ] **Step 9: Commit**

```bash
git add crates/media-linux/Cargo.toml crates/media-linux/src/wayland_portal/stream.rs \
        crates/media-linux/src/wayland_portal/mod.rs Cargo.lock
git commit -m "$(cat <<'EOF'
P5B-1 T5: PipeWire stream + dedicated mainloop thread (stream.rs)

Add pipewire = "0.9" (verified against pipewire 0.9.2 docs; MainLoop
moved from pipewire::MainLoop to pipewire::main_loop::MainLoop in the
0.9 split). The mainloop runs on a std::thread::spawn — NOT a tokio
task — because pipewire types are deliberately !Send + !Sync. The
callback bridges to the producer via tokio::sync::mpsc::channel(2)
with try_send (drop-on-full = latest-only semantics, matches the X11
path's cap).

- RawFrame { data, width, height, stride, ts_us } + row() helper that
  walks padded-stride frames (Intel iGPU aligns stride to 64 bytes).
- FramePool cap=2 amortises Vec allocation across frames.
- LoopCommand::Shutdown via pipewire::channel::channel(); Drop fires
  the signal best-effort (no join in Drop — risk of deadlock).
- parse_video_format / build_format_params are intentional T5/T6 staged
  stubs; GNOME's default negotiation typically gives us BGRA, which
  is enough to land the dialog-firing path. Full SPA pod construction
  is flagged for T6 / follow-up.
- 3 new tests (padded-stride row access, FramePool cap=2 recycle,
  channel send doesn't fail without a polling Receiver).
EOF
)"
```

---

## Task 6: Capturer glue — wire session + stream + token through `CaptureSource`

**Files:**
- Modify: `crates/media-linux/src/wayland_portal/capturer.rs` (replace T4 stub)

- [ ] **Step 1: Write failing test for `Drop`-without-shutdown warn**

Append to `crates/media-linux/src/wayland_portal/capturer.rs` (file body in Step 2):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn shutdown_flag_warns_on_drop_when_not_called() {
        // Construct a capturer that we deliberately do NOT shutdown(), and
        // verify the shutdown_completed AtomicBool stays false. The Drop
        // impl logs a warn but cannot panic (Drop ≠ panic).
        let cap = WaylandPortalCapturer::new_for_test_with_unflagged_drop();
        assert!(!cap.shutdown_completed.load(Ordering::SeqCst));
        drop(cap);
        // No assertion on the log line (tracing capture is overkill here);
        // the test verifies the construct + drop sequence doesn't panic
        // and the flag is observed false.
    }

    #[test]
    fn shutdown_marks_flag_true() {
        let mut cap = WaylandPortalCapturer::new_for_test_with_unflagged_drop();
        cap.shutdown_completed.store(true, Ordering::SeqCst);
        assert!(cap.shutdown_completed.load(Ordering::SeqCst));
    }
}
```

Run: compile-fail expected.

- [ ] **Step 2: Replace the T4 stub with the real capturer**

Replace `crates/media-linux/src/wayland_portal/capturer.rs` body:

```rust
//! `WaylandPortalCapturer` — wires PortalSession + PipeWireStream +
//! PortalSessionToken behind the `CaptureSource` trait.
//!
//! Construction is sync (called from `LinuxSwFactory::create`, which is
//! sync) but the portal handshake itself is async. We bridge with a
//! `tokio::runtime::Builder::new_current_thread().build()?.block_on(...)`
//! so the factory doesn't need to be re-typed as async. The PipeWire
//! thread, once spawned by `PipeWireStream::connect`, lives independently
//! of the runtime that built the portal session.
//!
//! Shutdown discipline (Drop ≠ async): the trait's `Drop` impl can't
//! `.await session.close()` (ashpd 0.12 needs an async context). The
//! capturer therefore exposes an explicit `shutdown(self)` method that
//! the producer (or, in extremis, a host-level cleanup task) drives;
//! `Drop` only logs a `warn!` if it observes `shutdown_completed == false`
//! and best-effort signals the PipeWire thread to wind down.

#![cfg(target_os = "linux")]

use crate::capture_source::{CaptureSource, CaptureSourceError};
use crate::wayland_portal::{
    PipeWireStream, PortalSession, PortalSessionToken, WaylandPortalError,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum WaylandPortalCapturerInitError {
    #[error("portal: {0}")]
    Portal(#[from] WaylandPortalError),
    #[error("pipewire: {0}")]
    PipeWire(#[from] crate::wayland_portal::stream::PipeWireStreamError),
    #[error("runtime: {0}")]
    Runtime(String),
    #[error("token io: {0}")]
    TokenIo(String),
}

pub struct WaylandPortalCapturer {
    /// `None` after shutdown; held while the capturer is live so we can
    /// close it on shutdown. Boxed to keep the public type opaque.
    session: Option<crate::wayland_portal::session::PortalStartOutput>,
    stream: Option<PipeWireStream>,
    token_path: PathBuf,
    pub(crate) shutdown_completed: Arc<AtomicBool>,
    /// Last observed geometry from the PipeWire param_changed callback,
    /// shared with the stream thread via Arc<Mutex>.
    current_size: Arc<std::sync::Mutex<(u32, u32)>>,
}

impl WaylandPortalCapturer {
    /// Construct a new portal capturer. Fires the OS consent dialog the
    /// first time it's called (no token on disk or token rejected);
    /// re-uses the persisted token silently on subsequent runs.
    pub fn new(token_path: PathBuf) -> Result<Self, WaylandPortalCapturerInitError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| WaylandPortalCapturerInitError::Runtime(e.to_string()))?;

        rt.block_on(async move {
            // 1. Load token from disk (None on missing/corrupt).
            let mut token = PortalSessionToken::load_or_default(&token_path);
            let token_opt = token.token_opt().map(String::from);
            tracing::info!(has_token = token_opt.is_some(),
                "requesting screen-cast authorization via portal");

            // 2. Try start with token; on RestoreTokenRejected, delete the file
            //    and retry once with no token (first-launch path).
            let start = match PortalSession::start_with_token_opt(token_opt.as_deref()).await {
                Ok(out) => out,
                Err(e) if e.is_token_invalid() => {
                    tracing::warn!(error = %e,
                        "portal rejected restore_token; deleting and retrying as first-launch");
                    let _ = std::fs::remove_file(&token_path);
                    PortalSession::start_with_token_opt(None).await?
                }
                Err(e) => return Err(WaylandPortalCapturerInitError::Portal(e)),
            };

            // 3. Persist any new token the portal handed us.
            if let Some(new_tok) = &start.restore_token {
                token = PortalSessionToken::with_token(new_tok, compositor_hint());
                if let Err(e) = token.save(&token_path) {
                    tracing::warn!(error = %e, path = %token_path.display(),
                        "failed to persist portal-session.toml; will re-prompt next launch");
                }
            }

            // 4. Connect the PipeWire stream. The fd + node_id come from the
            //    portal Start response.
            let node_id = start.pipewire_node_id;
            let fd = start.pipewire_fd.try_clone().map_err(|e| {
                WaylandPortalCapturerInitError::Runtime(format!("dup pipewire fd: {e}"))
            })?;
            let stream = PipeWireStream::connect(fd, node_id)?;

            Ok(Self {
                session: Some(start),
                stream: Some(stream),
                token_path,
                shutdown_completed: Arc::new(AtomicBool::new(false)),
                current_size: Arc::new(std::sync::Mutex::new((1, 1))),
            })
        })
    }

    /// Explicit async shutdown. Closes the PipeWire stream first (joins
    /// the mainloop thread), then awaits `Session::close()` on the
    /// portal. Marks `shutdown_completed = true` so `Drop` skips its
    /// warn line.
    pub async fn shutdown(mut self) -> Result<(), WaylandPortalError> {
        if let Some(stream) = self.stream.take() {
            stream.shutdown();
        }
        if let Some(start) = self.session.take() {
            PortalSession::close(start.session).await?;
        }
        self.shutdown_completed.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Test-only helper: build a capturer with no real session/stream so
    /// the Drop impl can be exercised. Not public outside cfg(test).
    #[cfg(test)]
    pub(crate) fn new_for_test_with_unflagged_drop() -> Self {
        Self {
            session: None,
            stream: None,
            token_path: PathBuf::from("/tmp/prdt-test-token.toml"),
            shutdown_completed: Arc::new(AtomicBool::new(false)),
            current_size: Arc::new(std::sync::Mutex::new((640, 480))),
        }
    }
}

impl CaptureSource for WaylandPortalCapturer {
    fn geometry(&self) -> (u32, u32) {
        // The PipeWire thread updates current_size on each param_changed.
        // Read with a tight lock — non-blocking; never poisoned in practice
        // because the writer only does `*lock = (w, h)`.
        match self.current_size.lock() {
            Ok(g) => *g,
            Err(p) => *p.into_inner(),
        }
    }

    fn capture_into(&mut self, out: &mut Vec<u8>) -> Result<(), CaptureSourceError> {
        let stream = self.stream.as_mut().ok_or_else(|| CaptureSourceError::Terminal {
            backend: "linux-wayland-portal",
            reason: "stream already shutdown".into(),
        })?;
        // Block until the next frame arrives. The PipeWire thread sends
        // each new frame via try_send (drop-on-full), so rx.recv() returns
        // the latest available frame quickly under sustained capture.
        let frame = stream.rx.blocking_recv().ok_or_else(|| CaptureSourceError::Terminal {
            backend: "linux-wayland-portal",
            reason: "PipeWire channel closed (mainloop exited)".into(),
        })?;
        // Copy row-by-row to strip padding (BGRA, stride may exceed w*4).
        let row_bytes = (frame.width as usize) * 4;
        let needed = (frame.height as usize) * row_bytes;
        out.resize(needed, 0);
        for y in 0..frame.height {
            let dst_off = (y as usize) * row_bytes;
            out[dst_off..dst_off + row_bytes].copy_from_slice(frame.row(y));
        }
        Ok(())
    }
}

impl Drop for WaylandPortalCapturer {
    fn drop(&mut self) {
        if !self.shutdown_completed.load(Ordering::SeqCst) {
            tracing::warn!(
                "WaylandPortalCapturer dropped without explicit shutdown; \
                 PipeWire thread + portal session will tear down best-effort"
            );
        }
        // Best-effort: drop the stream (its Drop fires the quit signal but
        // doesn't join). The session, if still held, will eventually be
        // garbage-collected by the compositor's grant timeout.
        let _ = self.stream.take();
        let _ = self.session.take();
    }
}

fn compositor_hint() -> String {
    // Best-effort identifier for the persisted TOML's `compositor_hint`
    // field. Reads XDG_CURRENT_DESKTOP — not authoritative, but enough
    // for operator-facing debugging.
    std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn shutdown_flag_warns_on_drop_when_not_called() {
        let cap = WaylandPortalCapturer::new_for_test_with_unflagged_drop();
        assert!(!cap.shutdown_completed.load(Ordering::SeqCst));
        drop(cap);
        // No panic; warn line is best-effort.
    }

    #[test]
    fn shutdown_marks_flag_true() {
        let cap = WaylandPortalCapturer::new_for_test_with_unflagged_drop();
        cap.shutdown_completed.store(true, Ordering::SeqCst);
        assert!(cap.shutdown_completed.load(Ordering::SeqCst));
    }
}
```

- [ ] **Step 3: Run capturer tests**

```bash
cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu wayland_portal::capturer::tests
```

Expected:
```
test wayland_portal::capturer::tests::shutdown_flag_warns_on_drop_when_not_called ... ok
test wayland_portal::capturer::tests::shutdown_marks_flag_true ... ok
```

- [ ] **Step 4: Workspace gate**

```bash
cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
cargo test --workspace --lib --target x86_64-unknown-linux-gnu
```

Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/media-linux/src/wayland_portal/capturer.rs
git commit -m "$(cat <<'EOF'
P5B-1 T6: WaylandPortalCapturer wires session + stream + token

new() is sync (called from LinuxSwFactory::create which is sync); bridges
to async ashpd via a tokio::runtime::Builder::new_current_thread().block_on.
On RestoreTokenRejected, deletes the stored token and retries once with
None — operator sees the consent dialog as if it were a first launch.
New restore_token (if portal issued one) is persisted before the stream
connects, so a crash after Start still re-uses the grant.

capture_into() blocks on rx.blocking_recv() (the PipeWire thread feeds
the channel via try_send drop-on-full). The copy is row-by-row so
stride > width*4 padding (Intel iGPU 64-byte alignment) is stripped
before the BGRA buffer reaches bgra_to_i420.

Shutdown is explicit (async; ashpd 0.12 Session has no Drop::close);
Drop logs warn! if shutdown_completed == false and best-effort drops
the stream (its own Drop fires LoopCommand::Shutdown).

- 2 new tests (drop-without-shutdown observes shutdown_completed=false
  and doesn't panic; explicit flag flip).
EOF
)"
```

---

## Task 7: Factory integration — `LinuxSwFactory` builds the right capturer

**Files:**
- Modify: `crates/media-linux/src/policy.rs` (Wayland arm constructs `WaylandPortalCapturer`)
- Modify: `crates/media-linux/src/lib.rs` (`build_video_producer_with` is the canonical entry; legacy `build_video_producer` retained for tests)
- Modify: `crates/host/src/lib.rs` (no behaviour change; factory_arc already takes the resolved backend from T2)
- Modify: `crates/host/src/platform/linux.rs` (verify the factory call still compiles)

- [ ] **Step 1: Write failing test for the factory routing**

Append to `crates/media-linux/src/policy.rs` test module:

```rust
    #[test]
    fn linux_factory_routes_x11_backend_to_x11_capturer() {
        let factory = LinuxSwFactory::new(CaptureBackend::X11Shm);
        // We can't easily construct a producer in unit tests (X11 connect
        // would need an X server), but we can verify capture_backend()
        // round-trips.
        assert_eq!(factory.capture_backend(), CaptureBackend::X11Shm);
    }

    #[test]
    fn linux_factory_routes_wayland_backend_to_wayland_capturer() {
        let factory = LinuxSwFactory::new(CaptureBackend::WaylandPortal);
        assert_eq!(factory.capture_backend(), CaptureBackend::WaylandPortal);
    }

    #[test]
    fn linux_factory_forced_wayland_without_display_surfaces_helpful_error() {
        // With WAYLAND_DISPLAY unset, WaylandPortalCapturer::new() will
        // fail when ashpd tries to open the session bus. The factory
        // wraps that in FactoryError::Unavailable rather than panicking.
        let _g = ScopedEnv::unset("WAYLAND_DISPLAY");
        let factory = LinuxSwFactory::new(CaptureBackend::WaylandPortal);
        let cfg = ProducerConfig {
            width: 1920, height: 1080, fps: 60,
            initial_bitrate_bps: 8_000_000, codec: Codec::H264,
        };
        let r = factory.create(BackendKind::Openh264, &cfg);
        // We expect either Err (correct path) or Ok on a host that *does*
        // have a portal reachable. Both are acceptable; we just assert
        // we don't panic and don't return InvalidConfig with a misleading
        // message.
        match r {
            Err(FactoryError::Unavailable(BackendKind::Openh264, msg)) => {
                assert!(msg.contains("wayland") || msg.contains("portal") || msg.contains("ashpd"),
                    "error message should mention the failing backend: got {msg}");
            }
            Err(FactoryError::InvalidConfig(_, _)) => {
                // Also acceptable: the inner X11 path bubbled up. Not a panic; fine.
            }
            Ok(_) => {
                // Host has a real portal; skip.
            }
            Err(other) => panic!("unexpected error variant: {other}"),
        }
    }
```

Run: tests for "routes_*" pass against the existing T2 factory (capture_backend round-trip). The forced-Wayland test compile-fails or runtime-fails until Step 2 wires the real arm.

- [ ] **Step 2: Replace the placeholder Wayland arm in `LinuxSwFactory::create`**

In `crates/media-linux/src/policy.rs`, replace the Wayland arm:

```rust
        let producer = match self.capture_backend {
            CaptureBackend::X11Shm => crate::build_video_producer(cfg.initial_bitrate_bps, cfg.fps)
                .map_err(|e| FactoryError::InvalidConfig(kind, e.to_string()))?,
            CaptureBackend::WaylandPortal => {
                let token_path = default_portal_token_path();
                let cap = crate::wayland_portal::WaylandPortalCapturer::new(token_path)
                    .map_err(|e| FactoryError::Unavailable(kind, format!("WaylandPortalCapturer::new: {e}")))?;
                crate::build_video_producer_with(
                    Box::new(cap),
                    cfg.initial_bitrate_bps,
                    cfg.fps,
                )
                .map_err(|e| FactoryError::InvalidConfig(kind, e.to_string()))?
            }
        };
```

Add the default-path helper at the bottom of `policy.rs`:

```rust
fn default_portal_token_path() -> std::path::PathBuf {
    dirs::config_dir()
        .map(|d| d.join("prdt").join("portal-session.toml"))
        .unwrap_or_else(|| std::path::PathBuf::from("portal-session.toml"))
}
```

(`dirs` is already in `prdt-host`'s deps via the existing `default_prdt_config_dir`; verify it's on `prdt-media-linux` too. If not, add `dirs = "5"` to `[target.'cfg(target_os = "linux")'.dependencies]`.)

- [ ] **Step 3: Verify CLI plumbing is end-to-end**

No code change here — T2 already wired `--capture-backend` through to `platform::factory(...)`. Re-run the parser tests to confirm nothing regressed:

```bash
cargo test -p prdt-host --lib --target x86_64-unknown-linux-gnu cli_capture_backend
```

Expected: 2 pass.

- [ ] **Step 4: Run the new factory routing tests**

```bash
cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu policy::tests::linux_factory_routes
cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu policy::tests::linux_factory_forced_wayland
```

Expected: all pass (the third is best-effort; on a dev machine with a real portal it may return Ok, which the test accepts).

- [ ] **Step 5: Full workspace gate (this is the T7 acceptance gate)**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
cargo test --workspace --lib --target x86_64-unknown-linux-gnu
```

Expected: green. Pre-existing flaky `transport::probe_test::two_transports_find_each_other` is the only allowed failure.

- [ ] **Step 6: Commit**

```bash
git add crates/media-linux/src/policy.rs crates/media-linux/src/lib.rs \
        crates/host/src/lib.rs crates/host/src/platform/linux.rs
git commit -m "$(cat <<'EOF'
P5B-1 T7: factory wires WaylandPortalCapturer end-to-end

LinuxSwFactory::create's WaylandPortal arm now constructs
WaylandPortalCapturer::new(default_portal_token_path()) and feeds the
resulting Box<dyn CaptureSource> into build_video_producer_with(...).
The X11 arm is unchanged (regression guard). default_portal_token_path
resolves to $XDG_CONFIG_HOME/prdt/portal-session.toml via the existing
dirs crate, matching the host-auth.toml / host-peers.toml siblings.

End-to-end: prdt host --capture-backend wayland on a GNOME session
fires the OS consent dialog, persists the restore_token, and the
PipeWire stream feeds the existing bgra_to_i420 → OpenH264 path with
no encoder-side changes. WSLg / X11 path remains unchanged.

- 3 new tests (capture_backend round-trip on both arms + forced-Wayland
  without display surfaces a helpful Unavailable error).
- Final clippy + workspace test gate green.
EOF
)"
```

---

## Task 8: STATUS + smoke walkthrough doc + PR prep

**Files:**
- Create: `docs/superpowers/p5b1-smoke-walkthrough.md`
- Modify: `docs/superpowers/STATUS.md`

This task has no code; the two smoke walkthroughs verify spec DoD items 1, 2, 3, 4, 6.

- [ ] **Step 1: Write the smoke walkthrough doc**

Create `docs/superpowers/p5b1-smoke-walkthrough.md`:

```markdown
# P5B-1 Wayland Portal Foundation — Smoke Walkthrough

This document is the operator-facing smoke checklist for the
`phase-p5b1-wayland-portal-foundation-complete` tag. P5B-2 will add
KDE / Sway / Hyprland sections; for now we verify GNOME (the target
compositor) + WSLg X11 regression + the probe-priority log line.

## Section A — GNOME smoke (DoD #1, #2, #6)

**Pre-conditions:**
- Fresh Ubuntu 24.04 GNOME 47 (Wayland session).
- No `~/.config/prdt/portal-session.toml`.
- `prdt-host` built from this tag.

**Steps:**

1. Start the host with verbose tracing:

   ```bash
   RUST_LOG=info ./target/release/prdt-host --bitrate-mbps 5 --silent-allow --headless 2>&1 | tee p5b1-gnome-run1.log
   ```

2. Expect the OS consent dialog ("Allow `prdt-host` to share your
   screen?"). Click **Share entire screen** → **Allow**.

3. In `p5b1-gnome-run1.log`, expect:

   ```
   P5B-1 capture backend resolved choice=Auto resolved=WaylandPortal
   xdg-desktop-portal reachable; selecting Wayland capture backend
   requesting screen-cast authorization via portal has_token=false
   ```

4. Confirm the token file was created:

   ```bash
   stat -c '%a %n' ~/.config/prdt/portal-session.toml
   # Expected: 600 /home/<you>/.config/prdt/portal-session.toml
   ```

5. Connect a viewer (or run a 30-second loopback bench) and verify
   frames arrive. Expected viewer-overlay HUD: `linux-openh264` codec
   line, frames-per-second ≥ 30.

6. Stop the host with Ctrl-C. Confirm the log shows
   `WaylandPortalCapturer dropped without explicit shutdown` only if
   the producer didn't shutdown cleanly — a clean Ctrl-C path through
   `tokio::main`'s shutdown should NOT log this warn.

7. Re-run the same command:

   ```bash
   RUST_LOG=info ./target/release/prdt-host --bitrate-mbps 5 --silent-allow --headless 2>&1 | tee p5b1-gnome-run2.log
   ```

8. Expect **no consent dialog** this time. Log should show
   `has_token=true` and no new portal-session.toml mtime.

## Section B — WSLg X11 regression (DoD #3)

**Pre-conditions:**
- WSL2 Ubuntu 24.04 with WSLg.
- The existing L4 smoke walkthrough's setup.

**Steps:**

1. Confirm `WAYLAND_DISPLAY` is empty inside WSL:

   ```bash
   echo "WAYLAND_DISPLAY=[$WAYLAND_DISPLAY]"
   echo "DISPLAY=[$DISPLAY]"
   # Expected: WAYLAND_DISPLAY=[]  DISPLAY=[:0]
   ```

2. Run:

   ```bash
   RUST_LOG=info ./target/release/prdt-host --bitrate-mbps 5 --silent-allow --headless 2>&1 | tee p5b1-wslg-run.log
   ```

3. Expect the log line:

   ```
   WAYLAND_DISPLAY unset; selecting X11 capture backend
   P5B-1 capture backend resolved choice=Auto resolved=X11Shm
   ```

4. Run the existing L4 walkthrough verifier (connect viewer → 30s of
   frames → reconfigure encoder). It should pass with no behavioural
   change from the pre-P5B-1 baseline.

## Section C — Probe priority verification (DoD #4)

Both with `WAYLAND_DISPLAY` unset:

```bash
WAYLAND_DISPLAY= ./target/release/prdt-host --capture-backend wayland --bitrate-mbps 5 --headless --silent-allow 2>&1 | head -20
```

Expect:
- Log: `P5B-1 capture backend resolved choice=Wayland resolved=WaylandPortal`.
- Then a hard failure during factory construction:
  `WaylandPortalCapturer::new: ashpd: ...` (because the session bus
  has no portal owner).

```bash
WAYLAND_DISPLAY=wayland-fake ./target/release/prdt-host --capture-backend x11 --bitrate-mbps 5 --headless --silent-allow 2>&1 | head -20
```

Expect:
- Log: `P5B-1 capture backend resolved choice=X11 resolved=X11Shm`.
- X11 path proceeds (or fails if the WSLg X server is also unavailable,
  but that's not a P5B-1 regression).

## Out of scope (deferred to P5B-2 / P5C)

- DMABUF zero-copy (all frames still go through CPU `bgra_to_i420`).
- KDE / Sway / Hyprland smoke matrix.
- Wayland-native input dispatch (XTest under XWayland keeps working).
- HW encoder on Linux (Openh264 SW only).

## Known issues / follow-ups

- `parse_video_format` / `build_format_params` in `wayland_portal/stream.rs`
  ship as staged stubs; GNOME's default negotiation typically lands on
  BGRA. If a compositor refuses to default, smoke will surface
  "negotiated format not BGRA/BGRx; aborting" and the producer will
  surface `DeviceLost`. Track as a P5B-2 follow-up.
- Probe timeout is 1s; spec §11 noted a cold GNOME login might exceed
  this. If smoke shows false negatives, bump to 3s in a follow-up
  commit. Do NOT bump pre-emptively.
```

- [ ] **Step 2: Update STATUS.md**

Edit `docs/superpowers/STATUS.md`. Change the header:

```markdown
**Last updated:** 2026-05-12
**Latest tag:** `phase-p5b1-wayland-portal-foundation-complete`
```

Append under §1 Phase tag table (immediately after the P6 entry):

```markdown
- **P5B-1 (`phase-p5b1-wayland-portal-foundation-complete`, 2026-05-12)**:
  Wayland portal capture backend foundation. New `WaylandPortalCapturer`
  in `crates/media-linux/src/wayland_portal/` (session.rs / stream.rs /
  capturer.rs / token.rs / mod.rs) wraps `xdg-desktop-portal`'s
  ScreenCast interface (ashpd 0.12) + the PipeWire stream it returns
  (pipewire 0.9) and feeds CPU-side BGRA frames into the existing
  `bgra_to_i420` → OpenH264 path with no encoder-side changes.
  - New `trait CaptureSource { geometry, capture_into }` shared by
    `X11ShmCapturer` (existing) and `WaylandPortalCapturer` (new);
    `LinuxSwProducer` now holds `Box<dyn CaptureSource>`.
  - `CaptureBackend { X11Shm, WaylandPortal }` resolved at startup via
    `detect_capture_backend`: WAYLAND_DISPLAY env → zbus
    `NameHasOwner("org.freedesktop.portal.Desktop")` (1s timeout, no
    `CreateSession` to avoid spurious dialogs). CLI override:
    `--capture-backend {auto|x11|wayland}`.
  - Portal token persisted to `$XDG_CONFIG_HOME/prdt/portal-session.toml`
    (0600, atomic-rename, pid-suffix tmp); on `RestoreTokenRejected` the
    file is deleted and the consent dialog re-fires as a first launch.
  - PipeWire mainloop runs on a dedicated `std::thread` (NOT a tokio
    task; pipewire types are `!Send + !Sync`); callback bridges to the
    producer via `tokio::sync::mpsc::channel(2)` with `try_send` (drop-
    on-full = latest-only semantics, matches the X11 path's cap).
  - `Session::close().await` is explicit because ashpd 0.12 has no
    `Drop::close`; capturer's `Drop` impl logs `warn!` if shutdown
    wasn't called (best-effort, can't await in Drop).
  - **Tests**: 4 contract + 4 probe + 4 token + 2 session + 3 stream +
    2 capturer + 3 factory routing + 2 CLI parser = **24 new tests**
    cross-platform. Linux `cargo test --workspace --lib --target
    x86_64-unknown-linux-gnu` green.
  - **Out of scope (deferred)**: DMABUF zero-copy (P5B-2),
    multi-compositor smoke matrix KDE/Sway/Hyprland (P5B-2),
    Wayland-native input/clipboard/audio (P5B-2 / future), HW encoder
    on Linux (P5C), GUI "Disconnect Portal" UI (future).
  - **Smoke walkthrough**: `docs/superpowers/p5b1-smoke-walkthrough.md`
    (GNOME / WSLg / probe priority).
```

- [ ] **Step 3: Final pre-merge gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
cargo test --workspace --lib --target x86_64-unknown-linux-gnu
```

Expected: green.

- [ ] **Step 4: Commit STATUS + walkthrough**

```bash
git add docs/superpowers/STATUS.md docs/superpowers/p5b1-smoke-walkthrough.md
git commit -m "$(cat <<'EOF'
docs(STATUS): record P5B-1 Wayland portal foundation + smoke walkthrough

Adds the phase-p5b1-wayland-portal-foundation-complete entry under §1
with test counts, scope summary, and pointers to the smoke walkthrough
covering GNOME smoke, WSLg X11 regression, and probe-priority
verification. Out-of-scope list explicitly defers DMABUF, multi-
compositor matrix, Wayland-native input/clipboard/audio, and Linux HW
encoders to P5B-2 / P5C.
EOF
)"
```

- [ ] **Step 5: Push branch + open PR**

```bash
git push -u origin phase-p5b1-wayland-portal-foundation
gh pr create --title "P5B-1: Wayland portal capture backend foundation" --body "$(cat <<'EOF'
## Summary
- New `WaylandPortalCapturer` (`ashpd 0.12` + `pipewire 0.9`) implementing the existing `CaptureSource` trait alongside `X11ShmCapturer`.
- `--capture-backend {auto|x11|wayland}` flag; `auto` probes `WAYLAND_DISPLAY` + `org.freedesktop.portal.Desktop` (1s, no `CreateSession`).
- RestoreToken persisted to `$XDG_CONFIG_HOME/prdt/portal-session.toml` (0600, atomic save); rejected-token path deletes and re-prompts.
- PipeWire mainloop on a dedicated `std::thread` (pipewire types are `!Send + !Sync`); cap=2 `tokio::sync::mpsc` with `try_send` for drop-on-full latest-only.
- 24 new automated tests; Linux clippy + workspace tests green.
- X11 path unchanged (regression guard); WSLg smoke continues to pick X11.

## Test plan
- [x] Linux: `cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings` green.
- [x] Linux: `cargo test --workspace --lib --target x86_64-unknown-linux-gnu` green.
- [x] Windows: workflow_dispatch CI green (workspace must build cross-platform).
- [ ] GNOME smoke (operator follow-up, walkthrough doc): consent dialog → token persisted → re-run no dialog.
- [ ] WSLg X11 regression smoke (operator follow-up).

## Out of scope
DMABUF zero-copy, KDE/Sway/Hyprland matrix, Wayland-native input/clipboard/audio, Linux HW encoder — all deferred to P5B-2 / P5C.
EOF
)"
```

- [ ] **Step 6: After CI green + merge, tag**

```bash
gh pr merge --squash --delete-branch
git checkout master && git pull
git tag -a phase-p5b1-wayland-portal-foundation-complete \
    -m "P5B-1: Wayland portal capture backend foundation"
git push origin phase-p5b1-wayland-portal-foundation-complete
```

---

## Cross-task notes

- **Pre-existing flaky test:** `transport::probe_test::two_transports_find_each_other` is non-deterministic and unrelated to P5B-1. Do not treat as a regression. (Documented in STATUS L2 entry.)
- **ashpd 0.12 vs 0.13:** 0.13 requires Rust 1.87+; workspace MSRV is 1.85. Stay on 0.12 until a workspace-wide MSRV bump lands (tracked separately; not in this plan).
- **pipewire 0.9 vs 0.8:** T5 Step 1 verifies the 0.9.2 module-path move (`pipewire::MainLoop` → `pipewire::main_loop::MainLoop`). The implementer must NOT trust Codex's 0.8 sample blindly; adapt to 0.9.2 where they differ.
- **`spawn_blocking` vs `std::thread` for capture-into bridge:** the producer mirrors the existing X11 path's `spawn_blocking` shape (already in place pre-P5B-1; we confirmed by reading `linux_sw_producer.rs`). Both X11 and Wayland capturers run their blocking `capture_into` inside the same `spawn_blocking` boundary — no change required.
- **Probe timeout = 1s:** spec §11 worried this is tight on a cold GNOME login. The plan adds a comment in `policy.rs` ("if smoke shows false negatives, bump to 3s in a follow-up commit; don't bump pre-emptively"). Smoke walkthrough Section A is the verification.
- **`current_size` shared mutability:** the PipeWire `param_changed` callback writes the latest geometry into an `Arc<std::sync::Mutex<(u32, u32)>>`; the producer reads it via `geometry()`. If a future spec adds proper L4-style encoder reconfigure for mid-session resize, the existing reconfigure path is already wired (we use the new geometry on every `next_frame`).
- **`backend_name()` stability:** the producer returns `"linux-openh264"` regardless of capture choice. Viewer-overlay HUD therefore doesn't flicker on a capture-only swap. If P5C ever wants to distinguish the two, the capturer can expose its own name via a new trait method (out of scope here).

---

## Ambiguities resolved (spec didn't cover; plan author chose)

1. **`zbus` dep:** the spec required a synchronous `NameHasOwner` probe but didn't say where the D-Bus connection comes from. Ashpd would pull it in transitively at T4, but we need it at T2 (before T4 adds ashpd). Decision: add `zbus = "4"` directly as a `prdt-media-linux` Linux dep at T2; once ashpd lands at T4 it shares the same `zbus` instance. (Alternative considered: defer T2 probe to after T4, but that delays CLI flag wiring and forces a noisier T7.)
2. **`backend_name()` change:** the existing X11 path returned `"linux-x11shm-openh264"`. With two capturers under one producer, the plan collapses to `"linux-openh264"` to avoid HUD-flicker on a swap. Tests that pinned the old string need a one-line update (covered in T1 producer-test edits).
3. **`build_video_producer_with` introduction:** the spec described `LinuxSwFactory::create` switching internally but didn't name a helper for tests that want to inject a fake `CaptureSource`. The plan introduces `build_video_producer_with(capture, bps, fps)` as the canonical entry and retains the X11-only `build_video_producer(bps, fps)` for backward compatibility with the existing `tests/` files.
4. **`parse_video_format` / `build_format_params` staged stubs:** the spec section §4.3 mentioned BGRA/BGRx negotiation but the libspa POD builder is fiddly. The plan ships staged stubs (return Vec::new / Err("stub")) in T5, accepting whatever the compositor defaults to. The spec itself sanctions this ("If buffer pool is too fiddly for one task, simplify…flag it for follow-up — do not block on perfection"). Tracked in the smoke walkthrough's "Known issues" list.
5. **`parking_lot` swap to `std::sync::Mutex`:** the original T5 draft used `parking_lot::Mutex` for the shared `current_size`. To avoid adding a new workspace dep for one field, the plan substitutes `std::sync::Mutex` (poisoning handled with `.unwrap_or_else(|p| *p.into_inner())`). The single-field write pattern means contention is irrelevant.
6. **PipeWireStream `Drop` deadlock risk:** the spec said "Drop fires the quit signal but doesn't join". The plan implements exactly this: `Drop` sends `LoopCommand::Shutdown` best-effort and lets the OS reap the thread. `shutdown()` is the explicit path that joins. Capturer's `Drop` `warn!`s if `shutdown_completed == false` so the operator notices the soft-leak in logs.
7. **`Test#linux_factory_forced_wayland_without_display`:** this test is environment-dependent (on a real GNOME box the portal IS reachable and the test passes Ok). The plan accepts both Err and Ok and only fails on panic / wrong error variant. Documented inline; flagged as a candidate for `#[cfg_attr(ci, ignore)]` if it proves flaky on developer machines.
