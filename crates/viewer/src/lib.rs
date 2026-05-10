use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use prdt_audio::{AudioPlayback, OpusDecoder};
use prdt_crypto::{KeyPair, KnownHosts, PubKey};
use prdt_filetransfer::{send_file, TransferReceiver, DEFAULT_MAX_TRANSFER_BYTES};
#[cfg(windows)]
use prdt_protocol::VideoConsumer;
use prdt_protocol::{frame::Codec, ControlMessage, InputEvent, MonitorRect};

#[cfg(windows)]
#[allow(dead_code)] // wired into ViewerApp in Task 3
mod overlay_ipc;
#[cfg(windows)]
#[allow(dead_code)] // wired into ViewerApp in Task 3
mod overlay_supervisor;

mod latency;
use latency::LatencyProbe;

mod platform;
use platform::{
    build_consumer, build_render, clipboard_sequence_number, map_winit_mouse_button,
    physical_key_to_scancode, present_frame, read_clipboard_text, resize_renderer,
    write_clipboard_text, PlatformConsumer, PlatformFrame, PlatformRender, RenderError,
    MAX_CLIPBOARD_BYTES,
};

use prdt_transport::{
    viewer_handshake, CustomUdpTransport, HelloRequest, ReceivedMessage, Transport,
    UdpTransportConfig, DEFAULT_HANDSHAKE_TIMEOUT, DEFAULT_HELLO_RETRIES, DEFAULT_HELLO_TIMEOUT,
};
use tokio::sync::mpsc;
use tracing::{info, warn};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

/// Tracks whether an IDR frame has been requested from the host encoder,
/// with a 250 ms rate-limit to avoid flooding the encode loop.
///
/// Two trigger paths:
///   1. `FrameAssembler::purge()` returns a non-empty `Vec<u64>` (fragment loss).
///   2. Decoder returns `Err(_)` on a frame (reference frame missing / corrupt).
struct IdrRequester {
    needs_idr_pending: bool,
    last_request_at: Option<std::time::Instant>,
}

impl IdrRequester {
    fn new() -> Self {
        Self {
            needs_idr_pending: false,
            last_request_at: None,
        }
    }

    /// Signal that an IDR is needed (called on decode error or assembler purge).
    fn mark(&mut self) {
        self.needs_idr_pending = true;
    }

    /// If a request is pending and the cooldown has elapsed, clear the flag
    /// and return `true` (caller should send `RequestIdr`). Otherwise `false`.
    fn try_take(&mut self, now: std::time::Instant, cooldown: std::time::Duration) -> bool {
        if !self.needs_idr_pending {
            return false;
        }
        if let Some(t) = self.last_request_at {
            if now.duration_since(t) < cooldown {
                return false;
            }
        }
        self.needs_idr_pending = false;
        self.last_request_at = Some(now);
        true
    }
}

pub fn default_viewer_key_path() -> std::path::PathBuf {
    if let Some(base) = dirs::data_local_dir() {
        let dir = base.join("prdt");
        let _ = std::fs::create_dir_all(&dir);
        return dir.join("viewer-key.bin");
    }
    std::path::PathBuf::from("viewer-key.bin")
}

#[derive(Parser, Debug)]
#[command(name = "prdt-viewer", about = "power-remote-dt viewer")]
pub struct Args {
    /// Host address, e.g. 192.168.1.5:9000 or 127.0.0.1:9000.
    #[arg(long)]
    host: Option<SocketAddr>,

    /// Requested resolution; currently just used for the window size hint and
    /// as the viewer's request in the Hello. The actual resolution is decided
    /// by the host.
    #[arg(long, default_value = "1920x1080")]
    resolution: String,

    /// Requested FPS.
    #[arg(long, default_value_t = 60u32)]
    fps: u32,

    /// Host's public key in base64 (shown on host startup). If absent,
    /// --known-hosts is consulted. Required for Noise handshake.
    #[arg(long)]
    host_pubkey: Option<String>,

    /// Path to a known-hosts file mapping host addresses to pubkeys.
    /// Ignored if --host-pubkey is set.
    #[arg(long)]
    known_hosts: Option<std::path::PathBuf>,

    /// Directory into which files streamed from the host land. Created on
    /// demand; collisions get a `-N` suffix so nothing is overwritten.
    #[arg(long, default_value = "prdt-received")]
    recv_dir: std::path::PathBuf,

    /// Decoder backend. `nvdec` (default) uses the Plan 2d direct
    /// nvcuvid.dll path with the dual R8/R8G8 zero-copy optimization
    /// (`plan2d-zerocopy-complete`). The `prdt-bench-matrix` 60-config
    /// sweep (2026-04-26) showed NVDEC wins e2e_p50 against MF in
    /// every paired (resolution, bitrate, fps) cell -- median ratio
    /// 0.83 (17% faster), with lower jitter (CV 0.286 vs 0.309) and
    /// half the loss rate. The encode pipeline runs faster under the
    /// NVDEC consumer because back-pressure from the host side is
    /// lower; per-frame decode is actually slower than MF (~1.5 ms
    /// vs 0.22 ms p50) but encode wins the e2e race.
    ///
    /// `mf` falls back to Media Foundation's H.265 MFT via the HEVC
    /// Video Extensions store app. Per-frame decode is ~7x faster than
    /// NVDEC (0.22 ms p50 at 1080p60) but encode-side back-pressure
    /// makes overall e2e slower. Use `mf` for legacy reasons or when
    /// the CUDA Toolkit isn't installed at build time -- in that case
    /// `nvdec` falls back to `mf` with a warning.
    ///
    /// `openh264` selects the cross-platform OpenH264 software decoder
    /// from `prdt-media-sw`. It is the only decoder that consumes
    /// H.264 streams (negotiated when the host advertises only
    /// `[H264]`). Use it together with `--codec h264` against an
    /// `--encoder openh264` host, or together with `--codec auto` to
    /// silently downgrade.
    ///
    /// `auto` picks NVDEC when available (with the same MF fallback)
    /// for H.265 streams, and OpenH264 for H.264 streams; the actual
    /// choice is made after the Hello handshake using the host's
    /// negotiated codec.
    #[arg(
        long,
        default_value = "nvdec",
        value_parser = ["mf", "nvdec", "openh264", "auto"],
    )]
    decoder: String,

    /// Codec preference sent in Hello. `auto` (default) sends `H265`
    /// (the historical default) and accepts whatever the host
    /// negotiates; this is the only path that performs an implicit
    /// codec downgrade. `h265` and `h264` send the explicit codec and
    /// error out if the host responds with anything else (including a
    /// `HelloReject` from a host that does not support the requested
    /// codec). The interaction with `--decoder` is checked after
    /// handshake; mismatches print a precise error and exit non-zero.
    #[arg(
        long,
        default_value = "auto",
        value_parser = ["auto", "h265", "h264"],
    )]
    codec: String,

    /// Rendezvous via a signaling server instead of direct host address.
    #[arg(long)]
    signaling_url: Option<url::Url>,

    /// Opaque host identifier to look up on the signaling server.
    /// Required when --signaling-url is set.
    #[arg(long)]
    host_id: Option<String>,

    /// Rendezvous overall timeout in seconds.
    #[arg(long, default_value_t = 10)]
    signaling_timeout: u64,

    /// Path to the host_id-indexed known-hosts file (TOFU store for signaling mode).
    #[arg(long, default_value = "known-host-ids")]
    known_host_ids: std::path::PathBuf,

    /// Proceed even when the signaling-learned pubkey mismatches a previously recorded one.
    #[arg(long)]
    force_tofu: bool,

    /// STUN server URL (e.g. stun://stun.l.google.com:19302). Optional.
    /// When set together with --signaling-url, the viewer learns its public
    /// addr and sends it alongside the LAN Host candidate.
    #[arg(long)]
    stun_url: Option<url::Url>,

    /// TURN server URL (turn://user:pass@host:port). Optional. When set,
    /// transport is built via bind_with_relay (TURN relay mode).
    #[arg(long)]
    turn_url: Option<url::Url>,

    /// Local UDP bind address. Default in signaling mode is `0.0.0.0:0`
    /// (ephemeral, any interface) so cross-LAN probing works; set explicitly
    /// to a specific interface IP (e.g. `192.168.1.20:0`) when you need the
    /// Host candidate to carry that interface's address.
    #[arg(long, default_value = "0.0.0.0:0")]
    bind: SocketAddr,

    /// Run in CLI-only mode without launching the GUI launcher. Required for headless / CI.
    #[arg(long)]
    headless: bool,

    /// Override the GUI config file location (default: %APPDATA%/prdt/config.toml).
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Path to the viewer's long-term identity key. Generated on first use if
    /// missing. The host uses the matching pubkey to identify this viewer.
    #[arg(long, default_value_os_t = default_viewer_key_path())]
    pub viewer_key_file: std::path::PathBuf,

    /// Disable the L3 viewer-side adaptive bitrate controller. When set,
    /// the viewer will not send `ControlMessage::SetBitrate` to the host
    /// and the host's encoder will run at its CLI-configured bitrate for
    /// the entire session. Use for A/B regression comparisons.
    #[arg(long, default_value_t = false)]
    pub no_adaptive_bitrate: bool,

    /// Hint to the controller about the host's max bitrate, in Mbps. Used
    /// as the upper clamp for AIMD. If you don't know it, leave the
    /// default — the controller will start at this value and never exceed
    /// it. Should match the host's `--bitrate-mbps`.
    #[arg(long, default_value_t = 30u32)]
    pub bitrate_mbps: u32,
}

/// Normalize a user-supplied host_id for signaling: 9-digit numeric inputs
/// get the standard `XXX-XXX-XXX` dashed form; all other inputs are returned
/// verbatim.
fn normalize_host_id_input(input: &str) -> String {
    let stripped: String = input.chars().filter(|c| *c != '-').collect();
    if stripped.len() == 9 && stripped.chars().all(|c| c.is_ascii_digit()) {
        format!(
            "{}-{}-{}",
            &stripped[0..3],
            &stripped[3..6],
            &stripped[6..9]
        )
    } else {
        input.to_string()
    }
}

/// Load the viewer's long-term Noise static key from `path`, generating a new
/// keypair and persisting the private bytes if the file does not yet exist.
/// A wrong-size file is treated as an error rather than silently overwritten,
/// so an admin's misplaced file is not destroyed.
fn load_or_create_viewer_key(path: &std::path::Path) -> Result<KeyPair> {
    match std::fs::read(path) {
        Ok(bytes) => {
            if bytes.len() != 32 {
                anyhow::bail!(
                    "viewer key file {} has wrong size: expected 32 bytes, got {}",
                    path.display(),
                    bytes.len()
                );
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            Ok(KeyPair::from_private(arr))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let kp = KeyPair::generate();
            std::fs::write(path, kp.private.0)
                .with_context(|| format!("write new viewer key to {}", path.display()))?;
            info!(
                path = ?path,
                pubkey = %kp.public.to_base64(),
                "generated new viewer key"
            );
            Ok(kp)
        }
        Err(e) => {
            Err(anyhow::Error::from(e).context(format!("read viewer key {}", path.display())))
        }
    }
}

fn parse_resolution(s: &str) -> Result<(u32, u32)> {
    let (w, h) = s
        .split_once('x')
        .with_context(|| format!("bad --resolution {s:?}, expected WIDTHxHEIGHT"))?;
    let w: u32 = w.parse().with_context(|| format!("bad width in {s:?}"))?;
    let h: u32 = h.parse().with_context(|| format!("bad height in {s:?}"))?;
    Ok((w, h))
}

/// User-supplied `--codec` flag in its parsed form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodecArg {
    Auto,
    H265,
    H264,
}

impl CodecArg {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "auto" => Ok(Self::Auto),
            "h265" => Ok(Self::H265),
            "h264" => Ok(Self::H264),
            other => anyhow::bail!("bad --codec {other:?}, expected one of: auto, h265, h264"),
        }
    }

    /// The codec the viewer advertises in Hello. `auto` resolves to
    /// `H265` (historical default) so a non-upgraded host that ignores
    /// the new wire fields still gets a sensible request.
    fn hello_codec(self) -> Codec {
        match self {
            Self::Auto => Codec::H265,
            Self::H265 => Codec::H265,
            Self::H264 => Codec::H264,
        }
    }
}

/// Concrete decoder picked after handshake, after the negotiation
/// guard has approved the (decoder, negotiated_codec, codec) triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecoderChoice {
    Nvdec,
    Mf,
    Openh264,
}

/// Decide the concrete decoder backend given `--decoder`, the
/// host-negotiated codec from HelloAck, and the user's `--codec`
/// preference. Returns the verbatim error string from plan §Phase 3
/// on mismatch; the caller prints it and exits non-zero.
///
/// Pure function over `&str` so this matrix can be exhaustively
/// unit-tested without spinning up a transport.
fn choose_decoder(
    decoder_arg: &str,
    negotiated: Codec,
    codec_arg: CodecArg,
) -> std::result::Result<DecoderChoice, String> {
    // Step 1: enforce explicit --codec mismatches first. An explicit
    // --codec h264 against a negotiated H265 (or vice-versa) is always
    // an error regardless of --decoder, because we already opted out
    // of the implicit-downgrade path.
    match codec_arg {
        CodecArg::H265 if negotiated != Codec::H265 => {
            return Err(format!(
                "codec mismatch: --codec h265 but host negotiated {}",
                negotiated.name()
            ));
        }
        CodecArg::H264 if negotiated != Codec::H264 => {
            return Err(format!(
                "codec mismatch: --codec h264 but host negotiated {}",
                negotiated.name()
            ));
        }
        _ => {}
    }

    // Step 2: enforce --decoder vs negotiated. Plan §Phase 3 specifies
    // exact error strings for the (nvdec|mf, H264) and (openh264, H265)
    // cases; preserve them verbatim.
    match (decoder_arg, negotiated) {
        ("nvdec", Codec::H265) => Ok(DecoderChoice::Nvdec),
        ("mf", Codec::H265) => Ok(DecoderChoice::Mf),
        ("openh264", Codec::H264) => Ok(DecoderChoice::Openh264),
        ("auto", Codec::H265) => Ok(DecoderChoice::Nvdec),
        ("auto", Codec::H264) => Ok(DecoderChoice::Openh264),
        ("nvdec", Codec::H264) => Err("codec mismatch: viewer requested nvdec (H.265) but host \
             negotiated H.264; pass --decoder openh264 or --decoder auto"
            .into()),
        ("mf", Codec::H264) => Err("codec mismatch: viewer requested mf (H.265) but host \
             negotiated H.264; pass --decoder openh264 or --decoder auto"
            .into()),
        ("openh264", Codec::H265) => Err(
            "codec mismatch: viewer requested openh264 (H.264) but host \
             negotiated H.265; pass --decoder {nvdec|mf} or --decoder auto"
                .into(),
        ),
        (_, Codec::Av1) => Err(format!(
            "codec mismatch: host negotiated AV1 but viewer has no AV1 decoder \
             (decoder={decoder_arg})"
        )),
        (other, _) => Err(format!(
            "internal: unhandled (decoder={other}, negotiated={})",
            negotiated.name()
        )),
    }
}

/// Shared state between the winit main thread and the tokio worker thread.
struct ViewerShared {
    /// Latest decoded frame alongside the host capture timestamp (in the
    /// shared monotonic clock). The render thread pops this, presents, and
    /// feeds `host_ts_us` into the LatencyProbe to close the glass-to-glass
    /// measurement loop.
    latest_frame: Arc<Mutex<Option<(PlatformFrame, u64)>>>,
    /// Stream dimensions negotiated from HelloAck (and later refined by the
    /// decoder's reported texture size).
    stream_width: Mutex<u32>,
    stream_height: Mutex<u32>,
    /// Host's captured-monitor rect in virtual-desktop coords (from HelloAck).
    /// Used by `emit_mouse_move` to map window-local coords → virtual-desktop
    /// coords before normalizing to 0..65535 for MOUSEEVENTF_VIRTUALDESK.
    host_monitor_rect: Mutex<MonitorRect>,
    /// Bounding rect of the host's entire virtual desktop.
    host_virtual_desktop_rect: Mutex<MonitorRect>,
    /// Input events captured by winit, drained by the tokio send loop.
    input_tx: mpsc::UnboundedSender<InputEvent>,
    /// Paths of files dropped onto the window, drained by the file-transfer
    /// worker task which streams their bytes to the host.
    file_drop_tx: mpsc::UnboundedSender<std::path::PathBuf>,
    /// M1 latency probe. Written by the recv task (record_recv /
    /// record_decoded) and by the render thread (record_present_for_host_ts).
    latency: Arc<LatencyProbe>,
    /// Formatted status string the winit thread shows in the window title.
    /// The latency task refreshes it once per second; the main thread
    /// applies it in `about_to_wait`. `None` until we have a first value.
    status_title: Mutex<Option<String>>,
}

struct ViewerApp {
    req_w: u32,
    req_h: u32,
    shared: Arc<ViewerShared>,
    render: Option<PlatformRender>,
    decoder: String,
    /// True when invoked with --headless; overlay is suppressed in that mode.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    headless: bool,
    // The tokio runtime running the UDP / decode worker thread; kept alive
    // for the duration of the event loop.
    _runtime: tokio::runtime::Runtime,
    /// Set by `render_frame` when it hits an unrecoverable error (e.g.
    /// D3D11 device removed). `about_to_wait` sees it and calls
    /// `event_loop.exit()` so the next iteration tears down cleanly.
    should_exit: bool,
    /// Phase 4 G2: overlay supervisor. None when --headless or when init failed.
    #[cfg(windows)]
    overlay: Option<overlay_supervisor::OverlaySupervisor>,
    /// Last time we wrote stats.json. Throttled to 1 Hz in about_to_wait.
    #[cfg(windows)]
    last_overlay_tick: std::time::Instant,
    /// Set by overlay control polling when the user clicked Disconnect.
    /// Checked in about_to_wait to call event_loop.exit().
    disconnect_requested: bool,
}

impl ApplicationHandler for ViewerApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        info!("ApplicationHandler::resumed called");
        if self.render.is_some() {
            info!("render already exists, skipping");
            return;
        }

        let attrs = Window::default_attributes()
            .with_title("prdt-viewer")
            .with_inner_size(LogicalSize::new(self.req_w as f64, self.req_h as f64))
            .with_resizable(true);

        info!("creating window");
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                warn!(?e, "failed to create window");
                event_loop.exit();
                return;
            }
        };
        info!("window created ok");

        let size = window.inner_size();
        info!(
            width = size.width,
            height = size.height,
            "creating render state"
        );
        let render = match build_render(Arc::clone(&window), size.width.max(1), size.height.max(1))
        {
            Ok(r) => r,
            Err(e) => {
                warn!(?e, "build_render failed");
                event_loop.exit();
                return;
            }
        };
        self.render = Some(render);
        window.request_redraw();
        info!("resumed done, first redraw requested");

        // Phase 4 G2: spawn overlay supervisor (skipped in --headless mode).
        #[cfg(windows)]
        if !self.headless {
            match overlay_supervisor::OverlaySupervisor::new() {
                Ok(s) => {
                    info!(ipc_dir = %s.ipc_dir().display(), "overlay supervisor ready");
                    self.overlay = Some(s);
                }
                Err(e) => warn!(?e, "overlay supervisor disabled (cache dir error)"),
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                info!("close requested");
                event_loop.exit();
            }
            WindowEvent::Resized(PhysicalSize { width, height }) => {
                if let Some(r) = self.render.as_mut() {
                    if let Err(e) = resize_renderer(r, width.max(1), height.max(1)) {
                        warn!(?e, "resize_renderer failed");
                    }
                    r.window().request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(r) = self.render.as_ref() {
                    let win_size = r.window().inner_size();
                    self.emit_mouse_move(position, win_size.width, win_size.height);
                }
            }
            WindowEvent::MouseInput { button, state, .. } => {
                if let Some(btn) = map_winit_mouse_button(button) {
                    let pressed = matches!(state, ElementState::Pressed);
                    let _ = self.shared.input_tx.send(InputEvent::MouseButton {
                        button: btn,
                        pressed,
                    });
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x as i32, y as i32),
                    MouseScrollDelta::PixelDelta(PhysicalPosition { x, y }) => {
                        // One notch is usually 120 units on Windows; scale pixel
                        // deltas crudely to a line-ish magnitude.
                        ((x / 120.0) as i32, (y / 120.0) as i32)
                    }
                };
                if dx != 0 || dy != 0 {
                    let _ = self.shared.input_tx.send(InputEvent::MouseWheel { dx, dy });
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                // Phase 4 G2: ESC spawns the overlay (and is NOT forwarded to host).
                if event.physical_key
                    == winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::Escape)
                    && event.state == ElementState::Pressed
                {
                    #[cfg(windows)]
                    {
                        if let Some(s) = self.overlay.as_mut() {
                            if let Err(e) = s.spawn_if_idle() {
                                warn!(?e, "overlay spawn failed");
                            }
                        }
                    }
                    #[cfg(target_os = "linux")]
                    {
                        info!("Esc pressed; quick-disconnecting (overlay deferred to L2 on Linux)");
                        event_loop.exit();
                    }
                    return;
                }
                if event.repeat {
                    return;
                }
                if let Some(scan) = physical_key_to_scancode(event.physical_key) {
                    let pressed = matches!(event.state, ElementState::Pressed);
                    let _ = self.shared.input_tx.send(InputEvent::Key {
                        scancode: scan,
                        pressed,
                    });
                }
            }
            WindowEvent::DroppedFile(path) => {
                info!(path = %path.display(), "file dropped on viewer");
                let _ = self.shared.file_drop_tx.send(path);
            }
            WindowEvent::RedrawRequested => {
                self.render_frame();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.should_exit {
            event_loop.exit();
            return;
        }

        // Phase 4 G2: 1 Hz overlay tick — write stats.json + poll control.json.
        #[cfg(windows)]
        if self.last_overlay_tick.elapsed() >= std::time::Duration::from_secs(1) {
            self.last_overlay_tick = std::time::Instant::now();
            if let Some(ref s) = self.overlay {
                let payload = build_stats_payload(self);
                if let Err(e) = s.write_stats(&payload) {
                    warn!(?e, "write_stats failed");
                }
                match s.read_control() {
                    Ok(Some(action)) if action == "disconnect" => {
                        info!("overlay requested disconnect; shutting down");
                        self.disconnect_requested = true;
                    }
                    Ok(_) => {}
                    Err(e) => warn!(?e, "read_control failed"),
                }
            }
        }

        if self.disconnect_requested {
            event_loop.exit();
        }

        // Apply any pending window-title refresh from the latency task.
        // set_title on winit 0.30 is cheap when the string hasn't changed,
        // and the latency task only touches this slot every ~1s.
        if let Some(r) = self.render.as_ref() {
            if let Some(title) = self.shared.status_title.lock().unwrap().take() {
                r.window().set_title(&title);
            }
            r.window().request_redraw();
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        info!("viewer exiting");
    }
}

impl ViewerApp {
    fn emit_mouse_move(&self, position: PhysicalPosition<f64>, win_w: u32, win_h: u32) {
        let window_w = (win_w as f64).max(1.0);
        let window_h = (win_h as f64).max(1.0);
        let monitor = *self.shared.host_monitor_rect.lock().unwrap();
        let vd = *self.shared.host_virtual_desktop_rect.lock().unwrap();
        let (abs_x, abs_y) =
            map_cursor_to_virtual_desktop(position.x, position.y, window_w, window_h, monitor, vd);
        let _ = self.shared.input_tx.send(InputEvent::MouseMove {
            x: abs_x,
            y: abs_y,
            absolute: true,
        });
    }

    fn render_frame(&mut self) {
        let Some(render) = self.render.as_mut() else {
            return;
        };
        let maybe_frame = self.shared.latest_frame.lock().unwrap().take();
        let mut presented_host_ts: Option<u64> = None;
        if let Some((frame, host_ts_us)) = maybe_frame {
            if let Err(e) = present_frame(render, &frame, &self.decoder) {
                if matches!(e, RenderError::DeviceLost(_)) {
                    tracing::error!(?e, "present_frame device-lost; viewer cannot continue");
                    self.should_exit = true;
                }
                warn!(?e, "present_frame failed");
            }
            presented_host_ts = Some(host_ts_us);
        }
        if let Some(ts) = presented_host_ts {
            self.shared.latency.record_present_for_host_ts(ts);
        }
    }

    // Phase 4 G2: label helpers used by build_stats_payload.

    #[cfg(windows)]
    fn host_label_for_overlay(&self) -> String {
        if let Some(id) = self.host_id_for_label() {
            return id;
        }
        if let Some(addr) = self.host_addr_for_label() {
            return addr;
        }
        "(unknown)".to_string()
    }

    #[cfg(windows)]
    fn overlay_decoder_label(&self) -> String {
        self.decoder.clone()
    }

    /// Returns the signaling host_id stored on the struct, if any.
    /// ViewerApp does not carry args directly; host_id was passed into
    /// spawn_worker_tasks and is not retained on the struct. Fall back None.
    #[cfg(windows)]
    fn host_id_for_label(&self) -> Option<String> {
        None // host_id not retained on struct; host_label falls through to addr
    }

    /// Returns the direct-connect host address stored on the struct, if any.
    /// Same situation: not retained. Fall back None.
    #[cfg(windows)]
    fn host_addr_for_label(&self) -> Option<String> {
        None // direct_host not retained on struct; reported as "(unknown)"
    }
}

/// Saturating cast u64 → u32 for telemetry fields. A latency beyond ~71
/// minutes would overflow — treat that as "big", not as wraparound.
fn clamp_u32(v: u64) -> u32 {
    v.try_into().unwrap_or(u32::MAX)
}

/// Format the viewer window title from the latency probe snapshot. Shows
/// "connecting…" until we have samples, then p50 / p95 in milliseconds
/// plus the present-samples count so users can see the window is live.
fn format_status_title(snap: &latency::LatencySnapshot) -> String {
    match snap.present {
        Some(s) => format!(
            "prdt-viewer · lag p50 {:.1}ms · p95 {:.1}ms · {} samples",
            s.p50_us as f64 / 1000.0,
            s.p95_us as f64 / 1000.0,
            s.samples,
        ),
        None => match snap.arrival {
            Some(a) => format!(
                "prdt-viewer · arriving · lag p50 {:.1}ms",
                a.p50_us as f64 / 1000.0,
            ),
            None => "prdt-viewer · connecting…".to_string(),
        },
    }
}

/// Build the IPC stats payload from current viewer state.
#[cfg(windows)]
fn build_stats_payload(app: &ViewerApp) -> overlay_ipc::StatsPayload {
    let snap = app.shared.latency.snapshot();
    let present = snap.present;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let connection_state = if present.as_ref().map(|p| p.samples).unwrap_or(0) > 0 {
        "connected".to_string()
    } else {
        "connecting".to_string()
    };
    let host_label = app.host_label_for_overlay();
    let decoder = app.overlay_decoder_label();
    let latency_us = present.as_ref().map(|p| overlay_ipc::LatencyUs {
        p50: p.p50_us,
        p95: p.p95_us,
        p99: p.p99_us,
        samples: p.samples,
    });
    overlay_ipc::StatsPayload {
        version: 1,
        viewer_pid: std::process::id(),
        updated_at_unix_ms: now_ms,
        connection_state,
        host_label,
        decoder,
        latency_us,
        fps_observed: 0.0, // approximated; refined in G3+
    }
}

/// Map a window-local cursor position into the `(0..=65535, 0..=65535)` range
/// expected by `SendInput` with `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`.
///
/// The viewer window covers exactly the host's captured monitor, so
/// normalized window coords map linearly onto that monitor's rect in the
/// host's virtual desktop; we then scale to the virtual-desktop bounds.
///
/// Degenerate inputs (zero-sized rects) fall back to the legacy "whole-window
/// = 0..65535" mapping so the injector doesn't receive NaNs.
fn map_cursor_to_virtual_desktop(
    win_x: f64,
    win_y: f64,
    window_w: f64,
    window_h: f64,
    monitor: MonitorRect,
    vd: MonitorRect,
) -> (i32, i32) {
    let vd_w = vd.width() as f64;
    let vd_h = vd.height() as f64;
    if vd_w <= 0.0 || vd_h <= 0.0 {
        let x = ((win_x / window_w) * 65535.0).clamp(0.0, 65535.0) as i32;
        let y = ((win_y / window_h) * 65535.0).clamp(0.0, 65535.0) as i32;
        return (x, y);
    }
    let mon_w = monitor.width() as f64;
    let mon_h = monitor.height() as f64;
    let norm_x = (win_x / window_w).clamp(0.0, 1.0);
    let norm_y = (win_y / window_h).clamp(0.0, 1.0);
    let vd_px_x = monitor.left as f64 + norm_x * mon_w;
    let vd_px_y = monitor.top as f64 + norm_y * mon_h;
    let abs_x = (((vd_px_x - vd.left as f64) / vd_w) * 65535.0).clamp(0.0, 65535.0) as i32;
    let abs_y = (((vd_px_y - vd.top as f64) / vd_h) * 65535.0).clamp(0.0, 65535.0) as i32;
    (abs_x, abs_y)
}

#[cfg(windows)]
fn apply_connect_args(args: &mut Args, c: prdt_gui_viewer::ConnectArgs) {
    // Map ConnectArgs into the existing Args fields. Each ConnectArgs
    // value either replaces the corresponding Args field or is ignored
    // when ConnectArgs has no value (e.g. signaling_url is None for
    // direct mode). The existing CLI defaults survive when unset.
    if let Some(url) = c.signaling_url.clone() {
        args.signaling_url = Some(url);
    }
    if let Some(id) = c.host_id.clone() {
        args.host_id = Some(id);
    }
    if let Some(addr) = c.direct_addr {
        args.host = Some(addr);
    }
    if let Some(pk) = c.pubkey.clone() {
        args.host_pubkey = Some(pk);
    }
    args.recv_dir = c.recv_dir.clone();
    args.decoder = c.decoder.clone();
    args.resolution = c.default_resolution.clone();
    args.known_hosts = Some(c.known_hosts_path.clone());
    args.known_host_ids = c.known_host_ids_path.clone();
}

pub fn run_main() -> Result<()> {
    run_with_args(Args::parse())
}

pub fn run_with_args(
    #[cfg_attr(target_os = "linux", allow(unused_mut))] mut args: Args,
) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Phase 4 G5: install crash reporter (writes JSON dump + tracing error).
    #[cfg(windows)]
    prdt_gui_common::install_panic_hook(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));

    #[cfg(windows)]
    if !args.headless {
        match prdt_gui_viewer::run_viewer_launcher(args.config.clone())
            .map_err(|e| anyhow::anyhow!(e))?
        {
            prdt_gui_viewer::LaunchOutcome::Quit => return Ok(()),
            prdt_gui_viewer::LaunchOutcome::Connect(c) => apply_connect_args(&mut args, *c),
        }
    }
    // On Linux there is no GUI launcher path yet (deferred to L2). The
    // CLI is the only entry; --headless is implicitly always-true.
    #[cfg(target_os = "linux")]
    let _ = &args.config; // silence unused-field warning until L2 wires the launcher
    let (req_w, req_h) = parse_resolution(&args.resolution)?;
    // Normalize --host-id: accept 9-digit numeric IDs with or without dashes
    // so both `--host-id 123456789` and `--host-id 123-456-789` resolve to the
    // same DashMap key the server stored at Register time.
    let normalized_host_id: Option<String> =
        args.host_id.as_ref().map(|s| normalize_host_id_input(s));
    // Resolve host address + static pubkey. In signaling mode, both are learned at runtime from the
    // rendezvous server (pubkey via TOFU against --known-host-ids). In direct mode, --host is required
    // and the pubkey comes from --host-pubkey or --known-hosts.
    let direct_host: Option<SocketAddr> = args.host;
    let direct_pubkey: Option<PubKey> = match (&args.host_pubkey, &args.known_hosts, &direct_host) {
        (Some(b64), _, _) => Some(
            PubKey::from_base64(b64).map_err(|e| anyhow::anyhow!("invalid --host-pubkey: {e}"))?,
        ),
        (None, Some(path), Some(host_addr)) => {
            let kh = KnownHosts::load(path)
                .with_context(|| format!("load --known-hosts {}", path.display()))?;
            let host_str = host_addr.to_string();
            Some(kh.get(&host_str).copied().with_context(|| {
                format!(
                    "no entry for {host_str} in known-hosts file ({} entries)",
                    kh.len()
                )
            })?)
        }
        (None, _, None) => None,       // signaling mode will resolve below
        (None, None, Some(_)) => None, // no pubkey source; validated below
    };

    if args.signaling_url.is_some() {
        if args.host_id.is_none() {
            anyhow::bail!("--host-id is required when --signaling-url is set");
        }
    } else {
        if direct_host.is_none() {
            anyhow::bail!("either --host or --signaling-url is required");
        }
        if direct_pubkey.is_none() {
            anyhow::bail!("one of --host-pubkey, --known-hosts, or --signaling-url is required");
        }
    }

    // Load (or generate-and-persist) the viewer's long-term identity key.
    // The IK Noise pattern transmits its pubkey to the host so the host can
    // identify this viewer cryptographically.
    let viewer_kp: KeyPair = load_or_create_viewer_key(&args.viewer_key_file)?;
    info!(viewer_pubkey = %viewer_kp.public.to_base64(), "viewer identity");

    info!(
        host = ?args.host,
        signaling_url = ?args.signaling_url,
        host_id = ?normalized_host_id,
        resolution = %args.resolution,
        fps = args.fps,
        "viewer starting"
    );

    // Input channel: winit main thread → tokio send loop.
    let (input_tx, input_rx) = mpsc::unbounded_channel::<InputEvent>();
    // File-drop channel: winit main thread → tokio file-transfer task.
    let (file_drop_tx, file_drop_rx) = mpsc::unbounded_channel::<std::path::PathBuf>();

    let shared = Arc::new(ViewerShared {
        latest_frame: Arc::new(Mutex::new(None)),
        stream_width: Mutex::new(req_w),
        stream_height: Mutex::new(req_h),
        // Pre-handshake defaults: assume captured monitor == primary ==
        // full virtual desktop starting at origin. `emit_mouse_move` uses
        // these until HelloAck updates them with the host's real geometry.
        host_monitor_rect: Mutex::new(MonitorRect::new(0, 0, req_w as i32, req_h as i32)),
        host_virtual_desktop_rect: Mutex::new(MonitorRect::new(0, 0, req_w as i32, req_h as i32)),
        input_tx,
        file_drop_tx,
        latency: Arc::new(LatencyProbe::new()),
        status_title: Mutex::new(None),
    });

    // Build the tokio runtime on a dedicated worker thread.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .thread_name("prdt-viewer-worker")
        .build()
        .context("tokio runtime")?;

    // Spawn the network + decode tasks.
    spawn_worker_tasks(
        runtime.handle().clone(),
        Arc::clone(&shared),
        input_rx,
        file_drop_rx,
        direct_host,
        direct_pubkey,
        args.signaling_url.clone(),
        normalized_host_id,
        args.signaling_timeout,
        args.stun_url.clone(),
        args.turn_url.clone(),
        args.known_host_ids.clone(),
        args.force_tofu,
        args.bind,
        req_w,
        req_h,
        args.fps,
        args.recv_dir.clone(),
        args.decoder.clone(),
        args.codec.clone(),
        viewer_kp,
        args.no_adaptive_bitrate,
        args.bitrate_mbps.saturating_mul(1_000_000),
    );

    // Build the event loop + app.
    let event_loop = EventLoop::new().context("EventLoop::new")?;
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = ViewerApp {
        req_w,
        req_h,
        shared,
        render: None,
        decoder: args.decoder.clone(),
        headless: args.headless,
        _runtime: runtime,
        should_exit: false,
        #[cfg(windows)]
        overlay: None,
        #[cfg(windows)]
        last_overlay_tick: std::time::Instant::now(),
        disconnect_requested: false,
    };

    info!("event_loop.run_app starting");
    let rc = event_loop.run_app(&mut app);
    info!(?rc, "event_loop.run_app returned");
    rc.context("run_app")?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn spawn_worker_tasks(
    handle: tokio::runtime::Handle,
    shared: Arc<ViewerShared>,
    mut input_rx: mpsc::UnboundedReceiver<InputEvent>,
    mut file_drop_rx: mpsc::UnboundedReceiver<std::path::PathBuf>,
    direct_host: Option<SocketAddr>,
    direct_pubkey: Option<PubKey>,
    signaling_url: Option<url::Url>,
    host_id: Option<String>,
    signaling_timeout_s: u64,
    stun_url: Option<url::Url>,
    turn_url: Option<url::Url>,
    known_host_ids_path: std::path::PathBuf,
    force_tofu: bool,
    cli_bind: SocketAddr,
    req_w: u32,
    req_h: u32,
    req_fps: u32,
    recv_dir: std::path::PathBuf,
    decoder: String,
    codec: String,
    viewer_kp: KeyPair,
    no_adaptive_bitrate: bool,
    max_bitrate_bps: u32,
) {
    handle.clone().spawn(async move {
        // CLI --bind supplies the local UDP bind (default 0.0.0.0:0). We only
        // override to [::]:0 when we're in direct-mode and the host address is
        // IPv6; everywhere else honour the CLI value so cross-LAN probing works.
        let mut bind_addr: SocketAddr = match (&signaling_url, &direct_host) {
            (None, Some(h)) if h.is_ipv6() && cli_bind.is_ipv4() => {
                "[::]:0".parse().unwrap()
            }
            _ => cli_bind,
        };
        // In signaling mode, if the caller left the bind IP as a wildcard
        // (0.0.0.0 / ::), auto-detect the outbound interface by opening a
        // temp UDP socket "connected" to the signaling server. The resulting
        // local IP is the one the OS routes out over, which is also the
        // interface addr we want to advertise as the Host candidate.
        if bind_addr.ip().is_unspecified() {
            if let Some(url) = signaling_url.as_ref() {
                match prdt_signaling_client::discover_outbound_ip(url).await {
                    Ok(ip) => {
                        bind_addr.set_ip(ip);
                        tracing::info!(%bind_addr, "auto-detected LAN bind IP via signaling URL");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "outbound IP discovery failed; falling back to wildcard bind (candidate may be unroutable)");
                    }
                }
            }
        }
        let cfg = UdpTransportConfig::default();
        let transport = match turn_url.clone() {
            Some(url) => {
                let turn_cfg = match prdt_nat_traversal::TurnConfig::from_url(&url).await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!(error = %e, "parse turn URL failed");
                        return;
                    }
                };
                match CustomUdpTransport::bind_with_relay(bind_addr, cfg, turn_cfg).await {
                    Ok(t) => Arc::new(t),
                    Err(e) => {
                        warn!(?e, "bind_with_relay failed");
                        return;
                    }
                }
            }
            None => match CustomUdpTransport::bind(bind_addr, cfg).await {
                Ok(t) => Arc::new(t),
                Err(e) => {
                    warn!(?e, "UDP bind failed");
                    return;
                }
            },
        };
        let local_udp = match transport.local_addr() {
            Ok(a) => a,
            Err(e) => {
                tracing::error!(error = %e, "local_addr failed");
                return;
            }
        };

        // Resolve (host_addr, pubkey) now. Signaling mode pulls both at runtime; direct mode uses
        // CLI-supplied values.
        let (host_addr, pubkey) = if let Some(url) = signaling_url.clone() {
            let host_id = host_id.clone().expect("clap-checked");
            let outcome = match prdt_signaling_client::rendezvous_as_viewer(
                prdt_signaling_client::RendezvousConfig {
                    url,
                    host_id: host_id.clone(),
                    timeout: std::time::Duration::from_secs(signaling_timeout_s),
                    stun_url: stun_url.clone(),
                    turn_url: turn_url.clone(),
                    aggregation_window: prdt_signaling_client::RendezvousConfig::DEFAULT_AGGREGATION_WINDOW,
                },
                local_udp,
            ).await {
                Ok(o) => o,
                Err(e) => {
                    tracing::error!(error = %e, "signaling rendezvous failed");
                    return;
                }
            };
            let pk_b64 = match outcome.peer_pubkey_b64.as_deref() {
                Some(s) => s,
                None => {
                    tracing::error!("signaling did not return a host pubkey");
                    return;
                }
            };
            let pk = match PubKey::from_base64(pk_b64) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(error = %e, "bad host pubkey from signaling");
                    return;
                }
            };

            use prdt_crypto::TofuVerdict;
            match prdt_crypto::KnownHosts::verify_or_record(&known_host_ids_path, &host_id, &pk) {
                Ok(TofuVerdict::FirstSeen) => {
                    tracing::info!(%host_id, "tofu_first_seen: recorded host pubkey");
                }
                Ok(TofuVerdict::Matched) => {
                    tracing::info!(%host_id, "tofu_matched");
                }
                Ok(TofuVerdict::Mismatch { .. }) if force_tofu => {
                    tracing::warn!(%host_id, "tofu_mismatch forced-through by --force-tofu");
                }
                Ok(TofuVerdict::Mismatch { .. }) => {
                    tracing::error!(%host_id, "TOFU pubkey mismatch. Refusing to connect. Use --force-tofu to override.");
                    return;
                }
                Err(e) => {
                    tracing::error!(error = %e, "known-host-ids error");
                    return;
                }
            }

            let cand_addrs: Vec<std::net::SocketAddr> = outcome
                .peer_candidates
                .iter()
                .filter_map(|c| format!("{}:{}", c.ip, c.port).parse().ok())
                .collect();
            tracing::info!(
                session_id = %outcome.session_id,
                %host_id,
                candidate_count = cand_addrs.len(),
                "signaling_rendezvous_completed"
            );
            let probed = match transport
                .probe_and_commit_peer(&cand_addrs, std::time::Duration::from_secs(10))
                .await
            {
                Ok(a) => a,
                Err(e) => {
                    tracing::error!(error = %e, "probe_and_commit_peer failed");
                    return;
                }
            };
            tracing::info!(peer = %probed, "probe selected winner");

            (probed, pk)
        } else {
            (direct_host.expect("args validated"), direct_pubkey.expect("args validated"))
        };

        // In direct mode we still need an explicit configure_peer; in
        // signaling mode probe_and_commit_peer already committed.
        if signaling_url.is_none() {
            transport.configure_peer(host_addr).await;
        }
        info!(%host_addr, local = ?transport.local_addr().ok(), "viewer transport ready");

        // Noise client handshake first (establishes encrypted channel).
        // Uses DEFAULT_HANDSHAKE_TIMEOUT so a wrong pubkey or unreachable host
        // fails fast instead of hanging the viewer forever.
        if let Err(e) = transport
            .handshake_as_client(&pubkey, &viewer_kp, DEFAULT_HANDSHAKE_TIMEOUT)
            .await
        {
            warn!(?e, "Noise client handshake failed");
            return;
        }
        tracing::info!("Noise handshake complete");

        // Parse --codec into a typed value. Args were already validated
        // by clap's value_parser so this is infallible in the happy path;
        // we still bubble up errors instead of unwrap so the codec_arg
        // helper test exercises the failure branch.
        let codec_arg = match CodecArg::parse(&codec) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "bad --codec");
                std::process::exit(2);
            }
        };

        // Handshake.
        let req = HelloRequest {
            req_width: req_w,
            req_height: req_h,
            req_fps,
            codec: codec_arg.hello_codec(),
        };
        let ack = match viewer_handshake(
            &*transport,
            &req,
            DEFAULT_HELLO_TIMEOUT,
            DEFAULT_HELLO_RETRIES,
        )
        .await
        {
            Ok(a) => a,
            Err(prdt_transport::TransportError::HelloRejected(reason)) => {
                // Plan §Phase 3 acceptance: surface reason verbatim and
                // exit non-zero within 100ms of Hello send. The transport
                // layer's `viewer_handshake` already returns immediately
                // on HelloReject without retrying, so the deadline is
                // bounded by `process::exit` overhead.
                tracing::error!(reason = %reason, "host rejected Hello");
                eprintln!("HelloReject: {reason}");
                std::process::exit(3);
            }
            Err(e) => {
                warn!(?e, "handshake failed");
                return;
            }
        };
        info!(
            session_id = format!("{:#x}", ack.session_id),
            neg = format!("{}x{}@{}", ack.neg_width, ack.neg_height, ack.neg_fps),
            bitrate_bps = ack.neg_bitrate_bps,
            monitor = ?ack.host_monitor_rect,
            virtual_desktop = ?ack.host_virtual_desktop_rect,
            "handshake complete"
        );
        *shared.stream_width.lock().unwrap() = ack.neg_width;
        *shared.stream_height.lock().unwrap() = ack.neg_height;
        *shared.host_monitor_rect.lock().unwrap() = ack.host_monitor_rect;
        *shared.host_virtual_desktop_rect.lock().unwrap() = ack.host_virtual_desktop_rect;

        // Negotiation guard — full matrix per plan §Phase 3. Runs
        // before consumer construction so we can fail fast with a
        // precise message if the host's negotiated codec disagrees with
        // --decoder / --codec.
        let decoder_choice = match choose_decoder(&decoder, ack.negotiated_codec, codec_arg) {
            Ok(c) => c,
            Err(msg) => {
                tracing::error!(error = %msg, "negotiation guard rejected handshake");
                eprintln!("{msg}");
                std::process::exit(4);
            }
        };

        // T7: build consumer through the platform factory. The Windows
        // path needs a D3D11 device; Linux's CPU path takes none.
        #[cfg(windows)]
        let consumer: Arc<tokio::sync::Mutex<PlatformConsumer>> = {
            let adapter = match prdt_media_win::pick_default_adapter() {
                Ok(a) => a,
                Err(e) => {
                    tracing::error!(error = %e, "pick_default_adapter");
                    return;
                }
            };
            let dev = match prdt_media_win::D3d11Device::create(&adapter) {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!(error = %e, "D3d11Device::create");
                    return;
                }
            };
            match build_consumer(&decoder, ack.negotiated_codec, ack.neg_width, ack.neg_height, &dev) {
                Ok(c) => Arc::new(tokio::sync::Mutex::new(c)),
                Err(e) => {
                    tracing::error!(error = %e, "build_consumer");
                    return;
                }
            }
        };
        #[cfg(target_os = "linux")]
        let consumer: Arc<tokio::sync::Mutex<PlatformConsumer>> = match build_consumer(
            &decoder,
            ack.negotiated_codec,
            ack.neg_width,
            ack.neg_height,
        ) {
            Ok(c) => Arc::new(tokio::sync::Mutex::new(c)),
            Err(e) => {
                tracing::error!(error = %e, "build_consumer");
                return;
            }
        };
        info!(
            backend = ?decoder_choice,
            negotiated = ack.negotiated_codec.name(),
            "decoder ready; spawning worker tasks"
        );

        // Shared "last clipboard text received from host" — used by the
        // watcher below to avoid echoing remote updates back.
        let last_remote_clipboard: Arc<tokio::sync::Mutex<Option<String>>> =
            Arc::new(tokio::sync::Mutex::new(None));

        // Audio playback lives on a dedicated OS thread because the cpal
        // `Stream` is `!Send` on Windows (WASAPI binds the stream to the
        // creating thread via COM). The recv_task decodes Opus → PCM and
        // pushes frames via a std::sync::mpsc; the playback thread owns the
        // `AudioPlayback` and calls `enqueue` as frames arrive.
        //
        // If device init fails the `audio_pcm_tx` stays `None`, decoded audio
        // is simply dropped, and video/input keep working.
        let (audio_pcm_tx, audio_pcm_rx) = std::sync::mpsc::channel::<Vec<f32>>();
        std::thread::Builder::new()
            .name("prdt-viewer-audio".into())
            .spawn(move || match AudioPlayback::start() {
                Ok(pb) => {
                    while let Ok(pcm) = audio_pcm_rx.recv() {
                        pb.enqueue(&pcm);
                    }
                }
                Err(e) => {
                    warn!(?e, "audio playback init failed; audio muted");
                    // Drain the channel forever so senders don't block on a
                    // disconnected receiver.
                    while audio_pcm_rx.recv().is_ok() {}
                }
            })
            .expect("spawn audio thread");

        let audio_decoder = Arc::new(tokio::sync::Mutex::new(
            OpusDecoder::new().expect("opus decoder"),
        ));

        // Recv loop: video → consumer; also handle control messages.
        let recv_shared = Arc::clone(&shared);
        let recv_transport = Arc::clone(&transport);
        let recv_consumer = Arc::clone(&consumer);
        let recv_last_remote = Arc::clone(&last_remote_clipboard);
        let recv_decoder = Arc::clone(&audio_decoder);
        let recv_audio_tx = audio_pcm_tx;
        let recv_ft_dir = recv_dir.clone();
        let recv_task = tokio::spawn(async move {
            info!("recv_task started");
            let mut ft_rx = TransferReceiver::new(recv_ft_dir, DEFAULT_MAX_TRANSFER_BYTES);
            let mut frame_count = 0u64;
            let mut tex_count = 0u64;
            let mut control_count = 0u64;
            let mut input_count = 0u64;
            let mut err_count = 0u64;
            let mut last_log = std::time::Instant::now();
            let mut timeouts = 0u64;
            let mut idr_req = IdrRequester::new();
            const IDR_COOLDOWN: std::time::Duration = std::time::Duration::from_millis(250);
            let try_send_idr_request =
                |idr_req: &mut IdrRequester, transport: &Arc<CustomUdpTransport>| {
                    if idr_req.try_take(std::time::Instant::now(), IDR_COOLDOWN) {
                        let ctrl_transport = Arc::clone(transport);
                        tokio::spawn(async move {
                            if let Err(e) = ctrl_transport
                                .send_control(ControlMessage::RequestIdr)
                                .await
                            {
                                tracing::warn!(?e, "send RequestIdr failed");
                            } else {
                                tracing::debug!("viewer sent RequestIdr (loss detected)");
                            }
                        });
                    }
                };
            loop {
                let recv_result = match tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    recv_transport.recv(),
                )
                .await
                {
                    Ok(r) => r,
                    Err(_) => {
                        timeouts += 1;
                        let purged = recv_transport.purge_assembler().await;
                        if !purged.is_empty() {
                            idr_req.mark();
                        }
                        try_send_idr_request(&mut idr_req, &recv_transport);
                        info!(
                            frames_received = frame_count,
                            textures_decoded = tex_count,
                            control_received = control_count,
                            input_received = input_count,
                            recv_errors = err_count,
                            timeouts,
                            "viewer rx stats (recv timeout 1s, no packet)"
                        );
                        continue;
                    }
                };
                if last_log.elapsed() >= std::time::Duration::from_secs(1) {
                    info!(
                        frames_received = frame_count,
                        textures_decoded = tex_count,
                        control_received = control_count,
                        input_received = input_count,
                        recv_errors = err_count,
                        timeouts,
                        "viewer rx stats"
                    );
                    last_log = std::time::Instant::now();
                }
                match recv_result {
                    Ok(ReceivedMessage::Video(frame)) => {
                        frame_count += 1;
                        let seq = frame.seq;
                        let host_ts_us = frame.timestamp_host_us;
                        let is_kf = frame.is_keyframe;
                        let nal_len = frame.nal_units.len();
                        recv_shared.latency.record_recv(seq, host_ts_us);
                        let mut c = recv_consumer.lock().await;

                        #[cfg(windows)]
                        {
                            use prdt_media_sw::SwH264Decoder as _;
                            let submit_result: std::result::Result<(), String> = match &mut *c {
                                PlatformConsumer::Mf(m) => m
                                    .submit(frame)
                                    .await
                                    .map_err(|e| format!("{e:?}")),
                                #[cfg(prdt_nvdec_bindings)]
                                PlatformConsumer::Nvdec(n) => n
                                    .submit(frame)
                                    .await
                                    .map_err(|e| format!("{e:?}")),
                                PlatformConsumer::Openh264 {
                                    decoder,
                                    uploader,
                                    latest_texture,
                                    needs_idr,
                                } => {
                                    // OpenH264 accepts the entire access unit
                                    // at once. If the host hasn't sent a
                                    // keyframe yet (post-reset), the decoder
                                    // will silently return None until the
                                    // next IDR arrives, which is fine.
                                    match decoder.decode(&frame.nal_units) {
                                        Ok(Some(i420)) => match uploader.upload(&i420) {
                                            Ok(tex) => {
                                                *latest_texture = Some(tex);
                                                *needs_idr = false;
                                                Ok(())
                                            }
                                            Err(e) => Err(format!("upload: {e}")),
                                        },
                                        Ok(None) => Ok(()),
                                        Err(e) => Err(format!("decode: {e}")),
                                    }
                                }
                            };
                            if let Err(e) = submit_result {
                                warn!(error = %e, seq, is_kf, nal_len, "consumer.submit error");
                                idr_req.mark();
                                try_send_idr_request(&mut idr_req, &recv_transport);
                                continue;
                            }
                            let frame_opt: Option<PlatformFrame> = match &mut *c {
                                PlatformConsumer::Mf(m) => {
                                    m.take_latest_texture().map(PlatformFrame::Nv12)
                                }
                                #[cfg(prdt_nvdec_bindings)]
                                PlatformConsumer::Nvdec(n) => {
                                    n.take_latest_dual_plane().map(PlatformFrame::DualPlane)
                                }
                                PlatformConsumer::Openh264 { latest_texture, .. } => {
                                    latest_texture.take().map(PlatformFrame::Nv12)
                                }
                            };
                            if let Some(frame) = frame_opt {
                                tex_count += 1;
                                recv_shared.latency.record_decoded(seq);
                                *recv_shared.latest_frame.lock().unwrap() =
                                    Some((frame, host_ts_us));
                            }
                        }

                        #[cfg(target_os = "linux")]
                        {
                            use prdt_media_sw::traits::SwH264Decoder as _;
                            let PlatformConsumer::Openh264 {
                                decoder,
                                latest,
                                needs_idr,
                            } = &mut *c;
                            match decoder.decode(&frame.nal_units) {
                                Ok(Some(i420)) => {
                                    let arc = std::sync::Arc::new(i420);
                                    *latest = Some(std::sync::Arc::clone(&arc));
                                    *needs_idr = false;
                                    tex_count += 1;
                                    recv_shared.latency.record_decoded(seq);
                                    *recv_shared.latest_frame.lock().unwrap() =
                                        Some((PlatformFrame::I420(arc), host_ts_us));
                                }
                                Ok(None) => {
                                    if *needs_idr && !is_kf {
                                        idr_req.mark();
                                    }
                                }
                                Err(e) => {
                                    warn!(error = %e, seq, is_kf, nal_len, "linux openh264 decode failed");
                                    idr_req.mark();
                                }
                            }
                        }

                        // Rate-limited RequestIdr send. Fires when loss detected (purge or decode error).
                        try_send_idr_request(&mut idr_req, &recv_transport);
                    }
                    Ok(ReceivedMessage::Control(ControlMessage::Bye)) => {
                        info!("host sent Bye");
                        break;
                    }
                    Ok(ReceivedMessage::Control(ControlMessage::Pong { .. })) => {
                        control_count += 1;
                    }
                    Ok(ReceivedMessage::Control(ControlMessage::ClipboardText { text })) => {
                        control_count += 1;
                        // Record so the watcher loop doesn't echo it back.
                        *recv_last_remote.lock().await = Some(text.clone());
                        if let Err(e) = write_clipboard_text(&text) {
                            warn!(?e, "write_clipboard_text failed");
                        }
                    }
                    Ok(ReceivedMessage::Control(msg)) => {
                        control_count += 1;
                        let _ = ft_rx.handle(msg);
                    }
                    Ok(ReceivedMessage::Input(_)) => {
                        input_count += 1;
                    }
                    Ok(ReceivedMessage::Audio(pkt)) => {
                        let mut dec = recv_decoder.lock().await;
                        match dec.decode(&pkt.opus_bytes) {
                            Ok(pcm) => {
                                if let Err(e) = recv_audio_tx.send(pcm) {
                                    warn!(?e, seq = pkt.seq, "audio thread disconnected");
                                }
                            }
                            Err(e) => warn!(?e, seq = pkt.seq, "opus decode"),
                        }
                    }
                    Err(e) => {
                        err_count += 1;
                        warn!(?e, "recv error; continuing");
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
        });

        // Send loop: drain input_rx → UDP.
        let send_transport = Arc::clone(&transport);
        let send_task = tokio::spawn(async move {
            while let Some(ev) = input_rx.recv().await {
                if let Err(e) = send_transport.send_input(ev).await {
                    warn!(?e, "send_input error");
                }
            }
        });

        // Ping loop (1Hz).
        let ping_transport = Arc::clone(&transport);
        let ping_task = tokio::spawn(async move {
            let mut seq: u64 = 0;
            let mut ticker = tokio::time::interval(Duration::from_secs(1));
            loop {
                ticker.tick().await;
                seq = seq.wrapping_add(1);
                let msg = ControlMessage::Ping {
                    ping_seq: seq,
                    viewer_ts_us: prdt_transport::now_monotonic_us(),
                };
                if let Err(e) = ping_transport.send_control(msg).await {
                    warn!(?e, "ping send error");
                }
            }
        });

        // File-drop → chunked file transfer worker.
        let ft_transport = Arc::clone(&transport);
        let ft_task = tokio::spawn(async move {
            while let Some(path) = file_drop_rx.recv().await {
                match send_file(&*ft_transport, &path, DEFAULT_MAX_TRANSFER_BYTES).await {
                    Ok(()) => info!(path = %path.display(), "file transfer sent"),
                    Err(e) => warn!(?e, path = %path.display(), "file transfer failed"),
                }
            }
        });

        // Clipboard watcher: poll GetClipboardSequenceNumber at 50ms and
        // only actually read the clipboard when the counter moves. This
        // keeps CPU use low when idle while dropping copy→remote-paste lag
        // from ~500ms to ~50ms.
        let clip_transport = Arc::clone(&transport);
        let clip_last_remote = Arc::clone(&last_remote_clipboard);
        let clip_task = tokio::spawn(async move {
            let mut last_sent: Option<String> = None;
            let mut last_seq = clipboard_sequence_number();
            loop {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let seq = clipboard_sequence_number();
                if seq == last_seq {
                    continue;
                }
                last_seq = seq;
                let current = match read_clipboard_text() {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                if current.len() > MAX_CLIPBOARD_BYTES {
                    continue;
                }
                if last_sent.as_ref() == Some(&current) {
                    continue;
                }
                if clip_last_remote.lock().await.as_ref() == Some(&current) {
                    continue;
                }
                if let Err(e) = clip_transport
                    .send_control(ControlMessage::ClipboardText {
                        text: current.clone(),
                    })
                    .await
                {
                    warn!(?e, "send clipboard failed");
                } else {
                    last_sent = Some(current);
                }
            }
        });

        // M1 latency reporter: every 1s log p50/p95/p99 locally, refresh the
        // viewer's window title with live stats, and every 5s send a
        // `LatencyReport` to the host so the host's logs show what the
        // viewer is actually experiencing (useful for distributed debugging
        // on real LAN). snapshot() is cheap (clones two small VecDeques).
        let latency_probe = Arc::clone(&shared.latency);
        let latency_transport = Arc::clone(&transport);
        let title_shared = Arc::clone(&shared);
        // L3 adaptive bitrate controller — runs inside latency_task at 1 Hz.
        let mut bitrate_controller = {
            let mut cfg = prdt_transport::bitrate_control::BitrateControllerConfig::new_for_max(
                max_bitrate_bps,
            );
            cfg.enabled = !no_adaptive_bitrate;
            prdt_transport::bitrate_control::BitrateController::new(cfg)
        };
        let bitrate_transport = Arc::clone(&transport);
        let latency_task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(1));
            ticker.tick().await; // fire first tick immediately; skip it
            let mut ticks_since_report: u32 = 0;
            // L3: caller-side rolling window state.
            let mut last_total_samples: u64 = 0;
            loop {
                ticker.tick().await;

                // Liveness heartbeat — host's watchdog needs this regardless of
                // whether decode is healthy yet. Crucial for slow-init viewers
                // that have not produced a present sample.
                if let Err(e) = latency_transport
                    .send_control(ControlMessage::KeepAlive)
                    .await
                {
                    warn!(?e, "send KeepAlive failed");
                }

                let snap = latency_probe.snapshot();

                // Window-title refresh: shown on every tick so users get
                // live feedback without tailing the log.
                let new_title = format_status_title(&snap);
                *title_shared.status_title.lock().unwrap() = Some(new_title);
                if let Some(present) = snap.present {
                    info!(
                        samples = present.samples,
                        arrival_p50_us = snap.arrival.map(|s| s.p50_us).unwrap_or(0),
                        arrival_p95_us = snap.arrival.map(|s| s.p95_us).unwrap_or(0),
                        decode_p50_us = snap.decode_done.map(|s| s.p50_us).unwrap_or(0),
                        decode_p95_us = snap.decode_done.map(|s| s.p95_us).unwrap_or(0),
                        present_p50_us = present.p50_us,
                        present_p95_us = present.p95_us,
                        present_p99_us = present.p99_us,
                        "M1 latency (host_capture → viewer_present)",
                    );
                } else if let Some(arrival) = snap.arrival {
                    info!(
                        samples = arrival.samples,
                        arrival_p50_us = arrival.p50_us,
                        arrival_p95_us = arrival.p95_us,
                        "M1 latency (arrival only; no present samples yet)",
                    );
                }

                // L3: adaptive bitrate step.
                let purged = bitrate_transport.purge_assembler().await;
                let lost = purged.len() as u64;
                let curr_total_samples = snap
                    .present
                    .map(|p| p.samples as u64)
                    .unwrap_or(last_total_samples);
                let delta_total = curr_total_samples.saturating_sub(last_total_samples);
                last_total_samples = curr_total_samples;
                let total_window = delta_total.saturating_add(lost);
                bitrate_controller.observe(lost, total_window);
                bitrate_controller.aimd_step(std::time::Instant::now());
                bitrate_controller.reset_window();
                if bitrate_controller.should_send() {
                    let target_bps = bitrate_controller.target_bps();
                    let msg = ControlMessage::SetBitrate { target_bps };
                    match bitrate_transport.send_control(msg).await {
                        Ok(()) => {
                            bitrate_controller.mark_sent();
                            info!(
                                target_bps,
                                lost_in_window = lost,
                                total_in_window = total_window,
                                "L3 sent SetBitrate"
                            );
                        }
                        Err(e) => warn!(?e, "L3 send SetBitrate failed"),
                    }
                }

                ticks_since_report += 1;
                if ticks_since_report >= 5 {
                    ticks_since_report = 0;
                    if let Some(present) = snap.present {
                        let arrival = snap.arrival.unwrap_or_default();
                        let decode = snap.decode_done.unwrap_or_default();
                        let msg = ControlMessage::LatencyReport {
                            samples: present.samples as u32,
                            arrival_p50_us: clamp_u32(arrival.p50_us),
                            arrival_p95_us: clamp_u32(arrival.p95_us),
                            decode_p50_us: clamp_u32(decode.p50_us),
                            decode_p95_us: clamp_u32(decode.p95_us),
                            present_p50_us: clamp_u32(present.p50_us),
                            present_p95_us: clamp_u32(present.p95_us),
                            present_p99_us: clamp_u32(present.p99_us),
                        };
                        if let Err(e) = latency_transport.send_control(msg).await {
                            warn!(?e, "send LatencyReport failed");
                        }
                    }
                }
            }
        });

        // If any loop exits, tear down.
        tokio::select! {
            _ = recv_task => info!("recv task ended"),
            _ = send_task => info!("send task ended"),
            _ = ping_task => info!("ping task ended"),
            _ = clip_task => info!("clipboard task ended"),
            _ = ft_task => info!("file transfer task ended"),
            _ = latency_task => info!("latency task ended"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_on_primary_monitor_in_single_monitor_setup() {
        // One 1920x1080 monitor at origin == full virtual desktop.
        let mon = MonitorRect::new(0, 0, 1920, 1080);
        let vd = MonitorRect::new(0, 0, 1920, 1080);
        let (x, y) = map_cursor_to_virtual_desktop(0.0, 0.0, 1920.0, 1080.0, mon, vd);
        assert_eq!((x, y), (0, 0));
        let (x, y) = map_cursor_to_virtual_desktop(1920.0, 1080.0, 1920.0, 1080.0, mon, vd);
        assert_eq!((x, y), (65535, 65535));
        let (x, y) = map_cursor_to_virtual_desktop(960.0, 540.0, 1920.0, 1080.0, mon, vd);
        // Center of window → ~0x7FFF on both axes.
        assert!((x - 32767).abs() <= 2);
        assert!((y - 32767).abs() <= 2);
    }

    #[test]
    fn cursor_on_secondary_monitor_maps_into_virtual_desktop() {
        // Two 1920x1080 monitors side-by-side. Host captures the right one.
        let mon = MonitorRect::new(1920, 0, 3840, 1080);
        let vd = MonitorRect::new(0, 0, 3840, 1080);
        // Top-left of viewer window → top-left of captured (right) monitor
        // → x == 1920 in VD → ~half of 65535.
        let (x, y) = map_cursor_to_virtual_desktop(0.0, 0.0, 1920.0, 1080.0, mon, vd);
        assert!((x - 32767).abs() <= 2);
        assert_eq!(y, 0);
        // Bottom-right of viewer window → bottom-right of VD.
        let (x, y) = map_cursor_to_virtual_desktop(1920.0, 1080.0, 1920.0, 1080.0, mon, vd);
        assert_eq!((x, y), (65535, 65535));
    }

    #[test]
    fn cursor_on_monitor_left_of_primary_handles_negative_coords() {
        // Windows places secondary monitors at negative virtual-desktop coords
        // when they're positioned left of/above the primary.
        let mon = MonitorRect::new(-1920, 0, 0, 1080);
        let vd = MonitorRect::new(-1920, 0, 1920, 1080);
        // Top-left of viewer window → (-1920, 0) in VD, which is VD origin
        // after normalization.
        let (x, y) = map_cursor_to_virtual_desktop(0.0, 0.0, 1920.0, 1080.0, mon, vd);
        assert_eq!((x, y), (0, 0));
        // Bottom-right of viewer window → (0, 1080) in VD, which is half-x,
        // full-y of the two-wide virtual desktop.
        let (x, y) = map_cursor_to_virtual_desktop(1920.0, 1080.0, 1920.0, 1080.0, mon, vd);
        assert!((x - 32767).abs() <= 2);
        assert_eq!(y, 65535);
    }

    #[test]
    fn degenerate_virtual_desktop_rect_falls_back_to_window_mapping() {
        let mon = MonitorRect::new(0, 0, 0, 0);
        let vd = MonitorRect::new(0, 0, 0, 0);
        let (x, y) = map_cursor_to_virtual_desktop(960.0, 540.0, 1920.0, 1080.0, mon, vd);
        assert!((x - 32767).abs() <= 2);
        assert!((y - 32767).abs() <= 2);
    }

    #[test]
    fn normalize_9digit_with_dashes() {
        assert_eq!(normalize_host_id_input("123-456-789"), "123-456-789");
    }

    #[test]
    fn normalize_9digit_without_dashes() {
        assert_eq!(normalize_host_id_input("123456789"), "123-456-789");
    }

    #[test]
    fn normalize_opaque_passthrough() {
        assert_eq!(normalize_host_id_input("alice-desktop"), "alice-desktop");
        assert_eq!(normalize_host_id_input("w1-test"), "w1-test");
    }

    #[test]
    fn normalize_short_numeric_passthrough() {
        assert_eq!(normalize_host_id_input("12345"), "12345");
    }

    #[test]
    fn codec_flag_parses() {
        assert_eq!(CodecArg::parse("auto").unwrap(), CodecArg::Auto);
        assert_eq!(CodecArg::parse("h265").unwrap(), CodecArg::H265);
        assert_eq!(CodecArg::parse("h264").unwrap(), CodecArg::H264);
        // Reject invalid values; the error message must mention the
        // accepted set so users can self-correct without docs.
        let err = CodecArg::parse("vp9").unwrap_err().to_string();
        assert!(
            err.contains("auto") && err.contains("h265") && err.contains("h264"),
            "error must list accepted values, got: {err}"
        );
        // Auto sends H265 in Hello (historical default).
        assert_eq!(CodecArg::Auto.hello_codec(), Codec::H265);
        assert_eq!(CodecArg::H265.hello_codec(), Codec::H265);
        assert_eq!(CodecArg::H264.hello_codec(), Codec::H264);
    }

    #[test]
    fn negotiation_guard_nvdec_h265_ok() {
        let r = choose_decoder("nvdec", Codec::H265, CodecArg::Auto);
        assert_eq!(r.unwrap(), DecoderChoice::Nvdec);
    }

    #[test]
    fn negotiation_guard_nvdec_h264_errors_with_plan_message() {
        let err = choose_decoder("nvdec", Codec::H264, CodecArg::Auto).unwrap_err();
        // Plan §Phase 3 verbatim message.
        assert!(
            err.contains("viewer requested nvdec (H.265)")
                && err.contains("host negotiated H.264")
                && err.contains("--decoder openh264 or --decoder auto"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn negotiation_guard_mf_h264_errors() {
        let err = choose_decoder("mf", Codec::H264, CodecArg::Auto).unwrap_err();
        assert!(
            err.contains("viewer requested mf (H.265)") && err.contains("host negotiated H.264"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn negotiation_guard_openh264_h264_ok() {
        let r = choose_decoder("openh264", Codec::H264, CodecArg::Auto);
        assert_eq!(r.unwrap(), DecoderChoice::Openh264);
    }

    #[test]
    fn negotiation_guard_openh264_h265_errors_with_plan_message() {
        // Mirror case from plan iteration 3.
        let err = choose_decoder("openh264", Codec::H265, CodecArg::Auto).unwrap_err();
        assert!(
            err.contains("viewer requested openh264 (H.264)")
                && err.contains("host negotiated H.265")
                && err.contains("--decoder {nvdec|mf} or --decoder auto"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn negotiation_guard_auto_auto_picks_negotiated() {
        // Both auto: silent downgrade is permitted on either codec.
        assert_eq!(
            choose_decoder("auto", Codec::H265, CodecArg::Auto).unwrap(),
            DecoderChoice::Nvdec
        );
        assert_eq!(
            choose_decoder("auto", Codec::H264, CodecArg::Auto).unwrap(),
            DecoderChoice::Openh264
        );
    }

    #[test]
    fn negotiation_guard_explicit_codec_overrides_auto_decoder() {
        // --decoder auto --codec h265 against H264 host → error
        // (explicit --codec wins over auto-pick).
        let err = choose_decoder("auto", Codec::H264, CodecArg::H265).unwrap_err();
        assert!(
            err.contains("--codec h265") && err.contains("h264"),
            "unexpected message: {err}"
        );
        let err = choose_decoder("auto", Codec::H265, CodecArg::H264).unwrap_err();
        assert!(
            err.contains("--codec h264") && err.contains("h265"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn negotiation_guard_explicit_codec_match_ok() {
        // --codec matches negotiated → permitted, decoder dispatch
        // still uses the explicit --decoder.
        assert_eq!(
            choose_decoder("nvdec", Codec::H265, CodecArg::H265).unwrap(),
            DecoderChoice::Nvdec
        );
        assert_eq!(
            choose_decoder("openh264", Codec::H264, CodecArg::H264).unwrap(),
            DecoderChoice::Openh264
        );
    }

    #[test]
    fn idr_requester_cooldown() {
        let mut r = IdrRequester::new();
        // Initially no pending request → try_take returns false.
        assert!(!r.try_take(
            std::time::Instant::now(),
            std::time::Duration::from_millis(250)
        ));

        // Mark pending, then try_take immediately → should return true (first request, no prior).
        r.mark();
        assert!(r.try_take(
            std::time::Instant::now(),
            std::time::Duration::from_millis(250)
        ));

        // try_take consumed the pending flag; second call immediately after → false (cooldown).
        r.mark();
        assert!(!r.try_take(
            std::time::Instant::now(),
            std::time::Duration::from_millis(250)
        ));

        // After sleeping past cooldown, try_take succeeds again.
        std::thread::sleep(std::time::Duration::from_millis(260));
        assert!(r.try_take(
            std::time::Instant::now(),
            std::time::Duration::from_millis(250)
        ));
    }
}
