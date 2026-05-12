# P5B-1: Wayland Portal Foundation (Capture via xdg-desktop-portal + PipeWire + CPU readback)

**Status:** Design
**Created:** 2026-05-12
**Predecessors:** P5A (`prdt-media-policy` SelectionPolicy + ProducerFactory), P6 (auth UX)
**Successors:** P5B-2 (DMABUF zero-copy + multi-compositor smoke), P5C (Linux HW encoders)

---

## 1. Goal & DoD

### 1.1 Goal

Add a `WaylandPortal` capture backend to the Linux host so that on a Wayland session it can capture the screen via `xdg-desktop-portal`'s `ScreenCast` interface and consume the PipeWire stream the portal hands back. The output is a stream of `RawFrame` (CPU-side BGRA/BGRx) suitable for the existing `prdt_media_sw::bgra_to_i420` → OpenH264 path. Zero-copy DMABUF, multi-compositor matrix smoke, and Wayland-side input/clipboard/audio are deferred to P5B-2 / P5C.

The new path lives behind a runtime capability probe so an X11 session, WSLg, or any environment without `org.freedesktop.portal.Desktop` keeps using the existing `x11_capture` path with no regression.

### 1.2 Definition of Done

1. On a Wayland session with a working `xdg-desktop-portal-*` (verified target: GNOME 45+ on Ubuntu 24.04), invoking `prdt host` (or `prdt-client`'s "Start Listener") produces a portal authorization dialog the first time, persists the `restore_token`, and on subsequent launches re-uses the token without re-prompting.
2. The PipeWire stream produced by the portal yields BGRA/BGRx frames that round-trip through `bgra_to_i420` → `Openh264Encoder` and are visible on a connected viewer (frames advance, no decode errors).
3. The legacy X11 path is unchanged: WSLg smoke (the existing L2/L3/L4/P5A walkthrough) still works and is selected automatically by the same `LinuxSwFactory`.
4. `--capture-backend {auto, x11, wayland}` CLI flag overrides auto-detection on `prdt-client host`.
5. Linux `cargo build --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace --lib` are green. ≥10 new automated tests.
6. Capability probe correctly falls back to X11 when D-Bus session bus is absent OR `org.freedesktop.portal.Desktop` is missing OR `CreateSession` fails.
7. Ship strategy: P5A/P6 同等 — automated test evidence + 2-stage code review acceptance, real-machine GNOME/KDE smoke deferred to a follow-up session as a documented walkthrough (no DoD blocker).

### 1.3 Out of Scope (deferred)

- **DMABUF zero-copy.** All P5B-1 frames flow through `SPA_DATA_MemFd` (or DmaBuf+mmap if advertised — see §4.4); explicit `sync_file`, multi-plane modifiers, and EGL import are P5B-2.
- **Multi-compositor smoke matrix.** GNOME-only smoke target. KDE / Sway / Hyprland are P5B-2.
- **Cursor mode = Metadata (4).** P5B-1 uses Embedded (2) — same UX as the X11 path; no protocol extension needed.
- **Multi-monitor.** Portal's standard UI hands the user a single monitor pick; we honour that and do not enumerate / merge / cycle.
- **Wayland input dispatch.** The current `prdt-input-linux` (XTest) path keeps working under XWayland; native libei is P5B-2 / future.
- **HW encoder on Linux.** Openh264 SW only — P5C handles VAAPI / NVENC-Linux.
- **GUI Settings UI for "Disconnect Portal" / "Revoke Token".** Tokens can be removed by deleting the persistence file or via the compositor's own Settings panel; in-app revoke UI is future.

---

## 2. Background

### 2.1 Why portal-based capture

Wayland deliberately forbids unprivileged clients from reading other clients' framebuffers — the screen-capture authority lives in the compositor. The cross-compositor standard for asking the compositor to share the screen is `org.freedesktop.portal.ScreenCast` (part of `xdg-desktop-portal`). Compositors back the portal API with their own implementation (`mutter` for GNOME, `kwin` for KDE, `xdg-desktop-portal-wlr` for Sway, `xdg-desktop-portal-hyprland` for Hyprland). The portal returns a PipeWire node id; the client connects to PipeWire's session bus and consumes a video stream from that node.

This is the documented path used by OBS Studio (`plugins/linux-pipewire/`), GNOME Remote Desktop, Sunshine (Linux Wayland host), and rustdesk's recent Wayland work. WLR-screencopy (the wlroots-specific direct path) bypasses the portal and is faster but only works on wlroots-based compositors; portal is the only path that works on GNOME and KDE.

### 2.2 Pre-existing path

`crates/media-linux/src/x11_capture.rs` uses `x11rb` with the MIT-SHM extension to call `XGetImage` on the root window. `LinuxSwProducer` wraps the capturer, runs BGRA→I420 (`prdt_media_sw::bgra_to_i420`) on every frame, and feeds the I420 into an `Openh264Encoder`. The producer is constructed by `crates/media-linux/src/policy.rs::LinuxSwFactory::create` and is the only `BackendKind::Openh264` Linux path that exists today. On WSLg this works because WSLg ships an X11 server; on a real Wayland session, `x11rb::connect` would either fail (no `$DISPLAY`) or attach to XWayland which only sees the calling app's own windows (not the desktop).

### 2.3 Constraints

- **MSRV 1.85.** Pins `ashpd <= 0.12.x` (≥ 0.13 requires Rust 1.87+) and `pipewire = 0.9` (MSRV 1.77, compatible).
- **No new workspace-wide deps.** Both `ashpd` and `pipewire` are Linux-only — declared inside the existing `[target.'cfg(target_os = "linux")']` table on `prdt-media-linux`.
- **Single source of truth for capture choice.** `LinuxSwProbe` already knows it's on Linux; it must learn how to pick between X11 and Wayland deterministically.
- **No regression of WSLg.** The L2/L3/L4/P5A WSLg walkthroughs must keep working. P5B-1 adds detection that picks X11 on WSLg.
- **Producer trait is fixed.** `VideoProducer` was settled in P5A (`async fn next_frame() -> EncodedFrame`, `request_idr`, `set_target_bitrate`, `backend_name`). The Wayland path implements the same trait. The encoder reconfigure plumbing (L4) and the policy-driven swap layer (P5A) plug in unchanged.

---

## 3. Architecture

### 3.1 Component diagram

```
                  ┌──────────────────────────────┐
                  │      prdt-media-policy       │
                  │  (P5A: SelectionPolicy +     │
                  │   ProducerFactory + Health)  │
                  └──────────────┬───────────────┘
                                 │ create(kind, cfg)
                                 ▼
              ┌──────────────────────────────────────┐
              │  LinuxSwFactory (extended)           │
              │   ├─ detect_capture_backend() ──────┐│
              │   │   1. WAYLAND_DISPLAY env        ││
              │   │   2. zbus::Connection::session  ││
              │   │   3. portal.Desktop NameHasOwner││
              │   │   4. CLI --capture-backend     ││
              │   └─ build(CaptureBackend, cfg)    ││
              └────────────┬──────────────┬─────────┘│
                           │              │           │
                ┌──────────▼──┐    ┌──────▼──────────┐│
                │ X11ShmCap   │    │ WaylandPortalCap││ ← new
                │ (existing)  │    │   (P5B-1)       ││
                └──────┬──────┘    └──────┬──────────┘│
                       │                  │            │
                       └────────┬─────────┘            │
                                ▼                       │
                  ┌──────────────────────────────┐     │
                  │ LinuxSwProducer (existing)   │◀────┘
                  │  trait CaptureSource impl    │
                  │  → BGRA→I420 → Openh264      │
                  └──────────────────────────────┘
```

The key insight: BGRA frame production is the common interface. Today `LinuxSwProducer` calls `X11ShmCapturer::capture_into(&mut Vec<u8>)` once per `next_frame()`. We abstract that into a `trait CaptureSource` and let either `X11ShmCapturer` or a new `WaylandPortalCapturer` implement it.

### 3.2 New module layout

All new code lives under `crates/media-linux/src/wayland_portal/`:

| File | Responsibility |
|---|---|
| `wayland_portal/mod.rs` | Public re-exports (`WaylandPortalCapturer`, `WaylandPortalError`, `detect_portal_available`). |
| `wayland_portal/session.rs` | `ashpd::desktop::screencast` session lifecycle: create_session → select_sources → start → open_pipewire_remote. RestoreToken handling. |
| `wayland_portal/stream.rs` | PipeWire mainloop thread + Stream connection + frame callback. Owns the dedicated `std::thread` and the `mpsc::Sender<RawFrame>`. |
| `wayland_portal/capturer.rs` | `WaylandPortalCapturer` — implements `CaptureSource`. Wraps session + stream + frame receiver. |
| `wayland_portal/token.rs` | RestoreToken persistence to `~/.config/prdt/portal-session.toml`. |

Plus refactors to existing files:

| File | Change |
|---|---|
| `crates/media-linux/src/lib.rs` | `pub mod wayland_portal;` + `pub trait CaptureSource`. |
| `crates/media-linux/src/x11_capture.rs` | Add `impl CaptureSource for X11ShmCapturer`. |
| `crates/media-linux/src/linux_sw_producer.rs` | Hold `Box<dyn CaptureSource>` instead of `X11ShmCapturer` directly. |
| `crates/media-linux/src/policy.rs` | `LinuxSwFactory::create` calls `detect_capture_backend()` and selects between the two `CaptureSource` impls. |
| `crates/host/src/lib.rs` (Linux path) | New CLI flag `--capture-backend {auto, x11, wayland}` plumbed into `HostConfig` and into the Linux factory probe context. |

No new crates. `crates/media-linux/Cargo.toml` gains `ashpd = "0.12"` and `pipewire = "0.9"` in its existing Linux deps.

### 3.3 Capture source trait

```rust
// crates/media-linux/src/capture_source.rs (new)
pub trait CaptureSource: Send {
    /// Return the (width, height) the next call to `capture_into` will fill.
    /// Wayland sources can change geometry mid-session if the user resized
    /// the captured monitor; the trait reports the latest known size and the
    /// producer reconfigures the encoder via the existing L4 reconfigure path.
    fn geometry(&self) -> (u32, u32);

    /// Block until a new frame is available, fill `out` with `width * height *
    /// 4` bytes of BGRA (or BGRx — alpha is ignored downstream), and return.
    ///
    /// Returns `Err` only on terminal failure; transient empty-frame /
    /// retry conditions are absorbed internally with a max-N-attempt cap and
    /// surface as `WouldBlock` errors that the producer converts to a "tick"
    /// (no new frame this period).
    fn capture_into(&mut self, out: &mut Vec<u8>) -> Result<(), CaptureSourceError>;
}
```

The error type is shared across both backends so the producer doesn't have to know which it owns.

### 3.4 Probe / fallback flow

`LinuxSwProbe` (currently a stub in `policy.rs`) gets a richer detection:

```rust
fn detect_capture_backend(cli_override: Option<CaptureBackendChoice>) -> CaptureBackend {
    if let Some(forced) = cli_override {
        return match forced {
            CaptureBackendChoice::X11 => CaptureBackend::X11Shm,
            CaptureBackendChoice::Wayland => CaptureBackend::WaylandPortal,
            CaptureBackendChoice::Auto => unreachable!("Auto is the default; handled below"),
        };
    }
    // 1. WAYLAND_DISPLAY env var → strong signal we're on Wayland.
    let on_wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    if !on_wayland {
        return CaptureBackend::X11Shm;
    }
    // 2. Session D-Bus + portal.Desktop NameHasOwner — short-circuit if either fails.
    //    Synchronous probe with a 1s timeout so we don't hang startup.
    //    Failures get logged with tracing::warn and the X11 backend is selected
    //    so e.g. a missing xdg-desktop-portal-* daemon doesn't kill the host.
    match portal_runtime_available_blocking(Duration::from_secs(1)) {
        Ok(true) => CaptureBackend::WaylandPortal,
        Ok(false) => {
            tracing::warn!("WAYLAND_DISPLAY set but xdg-desktop-portal unreachable; falling back to X11");
            CaptureBackend::X11Shm
        }
        Err(e) => {
            tracing::warn!(?e, "portal probe failed; falling back to X11");
            CaptureBackend::X11Shm
        }
    }
}
```

`portal_runtime_available_blocking` opens a `zbus::Connection::session()`, calls `DBus.NameHasOwner("org.freedesktop.portal.Desktop")`, and returns `Ok(true)` only if both succeed. It does **not** call `CreateSession` (that would put up the consent dialog every time we probe). The portal authorization dialog only fires inside `WaylandPortalCapturer::new`, when we actually intend to capture.

The probe is **synchronous** to keep `LinuxSwFactory::create` synchronous; `ashpd`'s normal API is async but the probe doesn't need ashpd — it's a single `NameHasOwner` call that we drive on a small `tokio::runtime::Runtime::new_current_thread` block_on inside the probe. The first real `WaylandPortalCapturer::new` then runs inside the host's existing tokio runtime.

---

## 4. Portal session & PipeWire stream

### 4.1 ashpd session lifecycle

The session walks the standard ScreenCast pattern from the XDG portal spec:

```rust
// Inside session.rs::establish_session(opts) -> Result<SessionHandle, PortalError>
let proxy = ScreenCast::new().await?;
let session = proxy.create_session().await?;          // returns Session

// First-launch: no restore_token. Subsequent: pass it.
let mut opts = SelectSourcesOptions::default()
    .types(SourceType::Monitor.into())
    .cursor_mode(CursorMode::Embedded.into())
    .multiple(false)
    .persist_mode(PersistMode::ExplicitlyRevoked);   // strongest persistence
if let Some(token) = opts.restore_token() {
    opts = opts.restore_token(token);
}
proxy.select_sources(&session, opts).await?;

let response = proxy.start(&session, None).await?;
// response.streams() → Vec<Stream { pipewire_node_id: u32, .. }>
// response.restore_token() → Option<String>
let fd = proxy.open_pipewire_remote(&session).await?; // OwnedFd
```

`Session` does **not** auto-close on Drop in ashpd 0.12 — Codex flagged this explicitly. We call `session.close().await` in `WaylandPortalCapturer::shutdown` (driven from the producer's Drop).

### 4.2 RestoreToken persistence

The token is opaque to us; we treat it as a printable string blob.

```toml
# ~/.config/prdt/portal-session.toml
restore_token = "<opaque base64 from portal>"
saved_at = "2026-05-12T10:34:21Z"
compositor_hint = "GNOME 47.1"   # informational only
```

Atomic write pattern matches the rest of the project (`format!("toml.tmp.{}", pid)` + rename, sorted output deterministic).

On startup, if the file exists, the token is read into memory and passed to `select_sources` via `RestoreToken`. If `proxy.start` returns the portal-specific "invalid token" reply (`Response::Other(_)` with the documented error code, or `select_sources` itself returns an error), we delete the file and re-call `start` with no token — the user gets the consent dialog as if it were a first launch.

Concrete error codes we recognise (from `xdg-desktop-portal` source `dbus-spec.md`):

- ScreenCast `Start` `response` field == 0 → success.
- == 1 → user cancelled → propagate as `PortalError::UserCancelled`; the host logs and continues with whatever previous source was in use (or stops if first launch).
- == 2 → generic error → propagate, discard token.

Token rotation: the portal may issue a new `restore_token` even when we passed one in. We always overwrite the stored token with whatever `start` returns (if it returns a fresh one).

### 4.3 PipeWire stream and the dedicated thread

PipeWire's `MainLoop` is thread-affine and the `pipewire-rs` types are deliberately `!Send + !Sync`. We follow Codex's recommended pattern:

```
            host tokio runtime                  PipeWire-owned thread
            ───────────────────                 ─────────────────────
WaylandPortalCapturer::new()  ───spawn───►   loop_thread_main:
                                                 init MainLoop + Context + Core
                                                 build Stream, register listener
                                                 stream.connect(MAP_BUFFERS|RT_PROCESS)
                                                 mainloop.run()        ◄── blocks here
                                                       │
                              ◄────try_send(frame)─────┤  per process() callback:
                              tokio::sync::mpsc(cap=2) │    dequeue_buffer →
                                                       │    copy chunk → Vec<u8>
                                                       │    tx.try_send (drop on Full)
                                                       │    buffer auto-queue on Drop
                                                       ▼
on shutdown:
    stop.store(true) + mainloop.quit_channel.send()      mainloop.run() returns
    join thread                                          stream/loop/context drop in
                                                         the thread that built them
```

Key correctness points:

- **No `blocking_send` from the callback.** `tx.try_send(frame)` with the "drop on Full" policy gives the latest-only semantics the X11 path already implements via the same channel cap.
- **Channel cap = 2.** Matches the X11 path. Cap=1 risks a single-frame stall; cap=2 lets the producer be 1 frame behind without dropping.
- **MainLoop quit channel.** `pipewire::channel::channel()` is the canonical way to signal the mainloop from another thread. The capture thread keeps the Receiver, the producer holds the Sender, and `Sender::send(LoopCommand::Shutdown)` wakes the loop.
- **Drop order**: producer.drop → set `stop` flag → send `Shutdown` on the loop channel → `thread.join()` → at this point the thread has fully destroyed the Stream/Context/MainLoop *on its own thread*, satisfying thread-affinity.

The Frame structure handed across the channel:

```rust
pub struct RawFrame {
    pub data: Vec<u8>,        // BGRA, length == stride * height
    pub width: u32,
    pub height: u32,
    pub stride: u32,          // may exceed width*4 — Wayland compositors often pad
    pub ts_us: u64,           // monotonic capture-side timestamp for latency stats
}
```

The capture thread copies the chunk into the `Vec<u8>` (no zero-copy — that's P5B-2). Allocation is amortised: the thread keeps a `VecDeque<Vec<u8>>` pool of 2 reusable buffers and recycles them as `RawFrame` instances are consumed downstream.

### 4.4 Pixel format negotiation

The Stream `params` advertise the formats the producer is willing to consume. P5B-1 advertises **`BGRA` and `BGRx`** only — both are 4-byte interleaved, no plane separation, and `bgra_to_i420` already handles them (BGRx is BGRA with the alpha byte ignored, which the converter does anyway).

We do **not** advertise YUV formats in P5B-1. Some compositors (Mutter on Intel iGPU) prefer to hand back YUV-NV12 to save bandwidth, but consuming NV12 requires a separate conversion path. P5B-2 can extend the params to opt into NV12; for now we ask only for BGRA/BGRx, and accept whatever the negotiation lands on.

Stride handling: `pipewire`'s `chunk.stride()` is the per-row byte count. When it differs from `width*4` (typical on Intel iGPU where stride is aligned to 64 bytes), the capture-thread copy must walk row-by-row. The receiving `bgra_to_i420` already accepts a stride parameter.

### 4.5 Frame timing & cursor

`cursor_mode = Embedded` means the cursor is baked into each frame by the compositor. The viewer sees the cursor as part of the screen content — same behaviour as the X11 path with `XFixesGetCursorImage`-merged composition (though the X11 path is actually un-composited; remote viewers have always seen "cursor as part of the picture"). No protocol changes.

Frame rate: PipeWire negotiates a maximum framerate as part of the format. We request 60 fps (no minimum); the compositor pushes frames only when the content changes (most do — GNOME's mutter is event-driven). The producer's `next_frame()` is whatever PipeWire produces, so the encoder sees adaptive frame rate. This matches the existing X11 path which polls but produces no encoded frame if nothing changed (the encoder coalesces).

---

## 5. Producer integration

### 5.1 LinuxSwProducer changes

```rust
// crates/media-linux/src/linux_sw_producer.rs (modified)
pub struct LinuxSwProducer {
    capture: Box<dyn CaptureSource>,    // was: X11ShmCapturer
    encoder: Openh264Encoder,
    buf_bgra: Vec<u8>,
    buf_i420: I420Frame,
    cfg: ProducerConfig,
}

impl LinuxSwProducer {
    pub fn new(
        capture: Box<dyn CaptureSource>,
        cfg: ProducerConfig,
    ) -> Result<Self, MediaError> { ... }
}

#[async_trait]
impl VideoProducer for LinuxSwProducer {
    async fn next_frame(&mut self) -> Result<EncodedFrame, ProducerError> {
        let (w, h) = self.capture.geometry();
        // L4 reconfigure path: if geometry changed since last frame, rebuild encoder.
        // (Existing logic; just driven by the new geometry() trait method.)

        // Capture: blocking is offloaded to spawn_blocking so the tokio thread
        // can service other I/O. The Wayland path's capture_into actually
        // blocks on rx.recv() from the dedicated thread, so spawn_blocking is
        // the correct primitive.
        let bgra = std::mem::take(&mut self.buf_bgra);
        let mut capture = std::mem::replace(&mut self.capture, dummy_capture());
        let (bgra, capture) = tokio::task::spawn_blocking(move || {
            capture.capture_into(&mut bgra).map(|_| (bgra, capture))
        }).await.map_err(|e| ProducerError::Other(e.to_string()))??;
        self.buf_bgra = bgra;
        self.capture = capture;

        // Existing BGRA→I420 → encode pipeline, unchanged.
        prdt_media_sw::bgra_to_i420(&self.buf_bgra, w, h, &mut self.buf_i420)?;
        self.encoder.encode(&self.buf_i420)
    }
    // request_idr / set_target_bitrate / backend_name unchanged.
}
```

`dummy_capture()` is a private no-op capture that exists only to bridge the `&mut self` / `spawn_blocking` ownership window. (Alternative considered: hold the capture in `Option<Box<dyn CaptureSource>>` and `take()` it — same shape, simpler. We pick whichever the implementer prefers — both compile.)

### 5.2 Factory selection

```rust
// crates/media-linux/src/policy.rs (modified)
impl ProducerFactory for LinuxSwFactory {
    fn create(
        &self,
        kind: BackendKind,
        cfg: &ProducerConfig,
    ) -> Result<Box<dyn VideoProducer>, FactoryError> {
        if kind != BackendKind::Openh264 {
            return Err(FactoryError::Unavailable(kind, "linux supports openh264 only".into()));
        }
        let capture: Box<dyn CaptureSource> = match self.capture_backend {
            CaptureBackend::X11Shm => {
                let cap = X11ShmCapturer::new()
                    .map_err(|e| FactoryError::Unavailable(kind, format!("X11ShmCapturer::new: {e}")))?;
                Box::new(cap)
            }
            CaptureBackend::WaylandPortal => {
                // tokio block_on inside a thread already running on a tokio runtime
                // is fine because we use a fresh current-thread runtime.
                let cap = WaylandPortalCapturer::new(cfg)
                    .map_err(|e| FactoryError::Unavailable(kind, format!("WaylandPortalCapturer::new: {e}")))?;
                Box::new(cap)
            }
        };
        let prod = LinuxSwProducer::new(capture, cfg.clone())
            .map_err(|e| FactoryError::Unavailable(kind, e.to_string()))?;
        Ok(Box::new(prod))
    }
}
```

The factory takes its `capture_backend: CaptureBackend` at construction time. `host/src/lib.rs` constructs the factory with the choice resolved from the CLI flag + probe:

```rust
let capture_backend = detect_capture_backend(args.capture_backend);
let factory = LinuxSwFactory::new(capture_backend);
```

### 5.3 SelectionPolicy interaction

P5A's policy currently picks an *encoder* backend (`BackendKind`). Capture is implicit (always X11). In P5B-1 we **do not split** the policy into `(CaptureBackend, EncoderBackend)` axes — the existing single-axis `BackendKind::Openh264` policy entry already covers the Linux case, and capture choice is a purely-local decision (no failover, no scoring). We only thread `CaptureBackend` through the factory.

P5C will revisit the policy split when VAAPI / NVENC-Linux land and capture × encoder combinations actually have multiple valid pairings.

---

## 6. CLI / config / host wiring

### 6.1 CLI

`prdt-client host` (and the underlying `prdt-host`) get one new flag:

```
--capture-backend <auto|x11|wayland>   [default: auto]
```

`auto` runs the probe described in §3.4. `x11` and `wayland` force the choice (error if the forced backend can't construct).

When `wayland` is forced but no Wayland session is detected (`WAYLAND_DISPLAY` unset, portal unreachable, etc.), the error propagates as a hard `FactoryError::Unavailable` and the host fails to start — this is the "I told you Wayland, you better have Wayland" semantics.

### 6.2 Persistence path

| OS | Token path |
|---|---|
| Linux | `$XDG_CONFIG_HOME/prdt/portal-session.toml` (fallback `~/.config/prdt/portal-session.toml`) |
| WSLg | same — file is created but never used (WSLg falls back to X11 in the probe) |
| Windows | n/a — file is Linux-only |

Same `dirs::config_dir()` resolution used by `host-auth.toml` and `host-peers.toml`.

### 6.3 GUI surface

P5B-1 adds **no new GUI**. The portal authorization dialog is rendered by the compositor itself (mutter / kwin / etc.) when the host first calls `start_session`. The host logs `"requesting screen-cast authorization via portal"` at info level; the operator sees the OS-native dialog and clicks Allow. On subsequent runs the token re-uses the existing grant silently.

If the user revokes the grant via the compositor's Settings panel, the next `start_session` returns an error; the producer surfaces it as `ProducerError::DeviceLost { backend: "wayland-portal", reason: "portal grant revoked" }`. The existing policy-driven health monitor (P5A) handles this as a backend failure — but since Linux has no fallback encoder backend, the host logs and stops the session loop. (P5C lifts this restriction; for P5B-1, "user revoked portal grant" === "host stops listening" is acceptable behaviour.)

P5B-2 / future may add a "Portal grant status" line in Settings; not in this spec.

---

## 7. Testing strategy

### 7.1 Unit tests (run on Linux CI)

- `wayland_portal::token::tests::round_trip` — write + read TOML round-trip.
- `wayland_portal::token::tests::atomic_save_pid_suffix` — concurrent saves don't truncate.
- `wayland_portal::token::tests::missing_file_returns_default` — `load_or_default` for no file.
- `wayland_portal::token::tests::corrupt_file_returns_default_with_warn` — invalid TOML.
- `policy::tests::auto_picks_x11_when_wayland_display_unset` — env probe.
- `policy::tests::auto_picks_x11_when_portal_unavailable` — D-Bus mock or env-controlled flag.
- `policy::tests::cli_override_forces_choice` — explicit `Wayland` / `X11` honoured even when probe says otherwise.
- `wayland_portal::stream::tests::raw_frame_with_padded_stride_validates` — Stride > width*4 frame validates correctly (no out-of-bounds during downstream conversion).
- `wayland_portal::stream::tests::pool_recycles_two_buffers` — buffer pool doesn't grow unbounded under sustained capture.

### 7.2 Integration tests

- `crates/media-linux/tests/wayland_portal_smoke.rs` — `#[ignore]` test that spawns a fake PipeWire node via `pipewire-loopback` (if available on the CI runner) and verifies the producer surfaces ≥ 1 frame. The test is `#[ignore]` because GitHub Actions Linux runners do **not** have PipeWire by default; it runs manually on dev machines. Documented in the test header.
- `crates/host/tests/capture_backend_cli.rs` — end-to-end CLI flag parsing: `--capture-backend wayland` propagates correctly through to factory construction.

### 7.3 Capture-source trait property tests

- `tests/capture_source_contract.rs` — generic test that any `CaptureSource` impl: returns `(w, h)` with `w >= 1 && h >= 1`; `capture_into` returns `Ok` AND writes `>= w*h*4` bytes OR returns a transient `WouldBlock`-class error never panics. Driven by both `X11ShmCapturer` (where the test must be `#[ignore]` because it needs an X server) and a stub `MockCaptureSource` that always returns a checkerboard.

### 7.4 Smoke walkthrough (deferred to follow-up)

Documented in `docs/superpowers/p5b1-smoke-walkthrough.md` for the next session:

- **Section A: GNOME smoke.** Fresh Ubuntu 24.04 GNOME 47, no prior `portal-session.toml`. Run `prdt host`, expect compositor dialog, click "Share entire screen", verify ≥ 30s of frames on the viewer. Verify the token file is created. Re-run, expect no dialog.
- **Section B: WSLg X11 regression.** Existing L4 smoke walkthrough, no change expected.
- **Section C: probe priority.** With `WAYLAND_DISPLAY` unset, verify info log shows "x11 capture backend selected".

P5B-2 will add KDE / Sway / Hyprland sections.

### 7.5 Regression bar

- Linux `cargo build --workspace --target x86_64-unknown-linux-gnu` green.
- Linux `cargo clippy --workspace --all-targets -- -D warnings` green.
- Linux `cargo test --workspace --lib` green; ≥ 10 new tests; no pre-existing test regresses.
- Windows builds via GitHub Actions (PR validation) green — this is a Linux-only change but the workspace must keep building on Windows.

---

## 8. Risks & mitigations

| # | Risk | Severity | Mitigation |
|---|---|---|---|
| 1 | `pipewire 0.9` API drift from Codex's `0.8` example | MEDIUM | Pin exact version; verify the example compiles on the chosen version during T1 before going further. |
| 2 | Portal dialog blocks the test runner on dev machines that don't have a desktop session | LOW | All real portal interaction is gated by `#[ignore]` on the integration test; CI never sees a dialog. |
| 3 | `ashpd` 0.12 has `Session` without `Drop::close` and a forgotten `close().await` leaks the session in the compositor (it eventually times out, but the operator may see "you're still being recorded" indicators for minutes) | MEDIUM | Implement an explicit `WaylandPortalCapturer::shutdown(self)` consuming method + a `Drop` impl that schedules `close` on the tokio handle. Validated by a unit test that checks shutdown was called (a flag on a test stub). |
| 4 | Compositor returns a stream that advertises a format we didn't ask for (some old KDE versions) | LOW | The Stream `param_changed` callback validates the negotiated format is BGRA/BGRx; on mismatch, log and tear down (the producer surfaces it as `DeviceLost` and the host stops). |
| 5 | RT-process callback violates tokio assumptions if mistakenly run inside the tokio runtime | HIGH | The PipeWire thread is created via raw `std::thread::spawn` in `WaylandPortalCapturer::new`. Document that it is NOT a tokio task; nothing inside it ever awaits a tokio future. The mpsc `Sender::try_send` is `Send + Sync` and can be called from any thread. |
| 6 | Token file is world-readable by default and the compositor's grant lets anyone with the token re-attach to your screen | LOW | The portal grant is per-user (uid-keyed) inside the compositor; the token alone is useless without your user-session D-Bus. We set the file to `0600` on write nevertheless. |
| 7 | Existing `LinuxSwProducer` refactor (`Box<dyn CaptureSource>`) accidentally regresses WSLg X11 path | HIGH | The X11 path keeps a comprehensive automated test in `wayland_portal_smoke.rs::existing_x11_path_unchanged` (no Wayland deps touched). WSLg smoke walkthrough run in the follow-up session catches anything the unit tests miss. |
| 8 | The 3-step portal probe takes too long on a slow machine (1s timeout × failures = startup hitch) | LOW | Probe runs on a dedicated tokio current-thread runtime with a 1s wall-clock deadline; on timeout we fall back to X11 with a tracing::warn. |

---

## 9. Implementation outline

This is not a TDD task breakdown — that's the plan's job. This section is the rough shape of the work so the plan author knows what they're sizing.

1. **T1 — Trait extraction.** Introduce `CaptureSource` trait, refactor `X11ShmCapturer` and `LinuxSwProducer` to use it. No new deps. WSLg smoke still works. (Touches ~3 files.)
2. **T2 — Capture-backend probe.** Add `CaptureBackend` enum, `detect_capture_backend()` function with the 3-step probe, CLI flag plumbing through `Args` → `HostConfig` → `LinuxSwFactory::new`. (Touches ~4 files.)
3. **T3 — Token persistence.** New `wayland_portal/token.rs` module with `load_or_default` / `save` / atomic-write. Pure data + tests, no portal interaction. (Touches 1 new file + 1 unit test.)
4. **T4 — ashpd session.** Add `ashpd = "0.12"` to `crates/media-linux/Cargo.toml`. Implement `wayland_portal/session.rs::PortalSession` (`new`, `start_with_token_opt`, `close`). At this stage `WaylandPortalCapturer::new` returns a "TODO not connected to PipeWire" error, but the portal dialog mechanically fires correctly. (Touches 2 new files + Cargo.toml.)
5. **T5 — PipeWire stream + thread.** Add `pipewire = "0.9"` dep. Implement `wayland_portal/stream.rs` with the dedicated thread + mpsc bridge. Build a minimal `cargo run --example pw_smoke_stub` (under a feature flag) that connects to a node id and prints a frame, used to verify the dep works before integrating. (Touches 2 new files + Cargo.toml.)
6. **T6 — Capturer glue.** `WaylandPortalCapturer` wires session + stream + token. Implements `CaptureSource`. Drop impl shuts down the thread + closes the session. (Touches 2 new files.)
7. **T7 — Factory + integration.** `LinuxSwFactory` selects between X11 / Wayland based on probe. End-to-end build green; cargo test green. Code review pass 1. (Touches policy.rs + linux_sw_producer.rs.)
8. **T8 — STATUS + smoke walkthrough doc + tag.** Update `docs/superpowers/STATUS.md`, write `docs/superpowers/p5b1-smoke-walkthrough.md`, create phase tag `phase-p5b1-wayland-portal-foundation-complete` after PR merge.

Estimated 8 tasks, ~2-3 sessions of subagent-driven-development.

---

## 10. References

- xdg-desktop-portal ScreenCast spec: https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html
- ashpd 0.12 ScreenCast source: https://docs.rs/crate/ashpd/0.12.3/source/src/desktop/screencast.rs
- pipewire 0.9.2 Stream docs: https://docs.rs/pipewire/0.9.2/pipewire/stream/struct.Stream.html
- OBS Studio Linux PipeWire integration: https://github.com/obsproject/obs-studio/blob/master/plugins/linux-pipewire/pipewire.c
- GNOME Remote Desktop reference impl: https://gitlab.gnome.org/GNOME/gnome-remote-desktop/-/blob/master/src/grd-session.c
- Sunshine Wayland host capture: https://github.com/LizardByte/Sunshine/blob/master/src/platform/linux/wayland.cpp
- CCG synthesis artifacts (this session):
  - `.omc/artifacts/ask/codex-p5b-wayland-portal-pipewire-capture-backend-rust-workspace-f-2026-05-12T00-48-45-914Z.md`
  - `.omc/artifacts/ask/gemini-p5b-wayland-portal-capture-backend-ux-oss-review-context-pow-2026-05-12T00-45-12-727Z.md`
- Roadmap §3 P5B (parent design): `docs/superpowers/specs/2026-05-11-final-goal-roadmap.md`
- P5A capability/policy design (predecessor): `docs/superpowers/specs/2026-05-11-p5a-capability-policy-design.md`

---

## 11. Open questions (for the plan author)

- **`pipewire 0.9` example freshness.** Codex's example targets 0.8; the import paths in 0.9 may differ slightly (`pw::main_loop` vs `pw::MainLoop`). T5 must verify against the actual 0.9.2 docs before locking in the skeleton.
- **`tokio::task::spawn_blocking` vs explicit thread for the capture-into bridge.** §5.1 sketches `spawn_blocking`. If under load this consumes too much of the blocking pool, switch to a dedicated long-lived `std::thread::spawn` consumer that owns the `rx.recv()` blocking call — same shape, different host. Plan author picks based on what reads cleanest in the existing producer.
- **Probe timing.** §3.4 uses 1s timeout for the D-Bus probe. If GNOME's portal is slow to wake (cold-start on a fresh login), 1s may be too tight. Plan author can dial to 3s if T2's smoke shows false negatives.
