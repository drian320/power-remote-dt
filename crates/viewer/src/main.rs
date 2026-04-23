#![cfg(windows)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use prdt_audio::{AudioPlayback, OpusDecoder};
use prdt_crypto::{KnownHosts, PubKey};
use prdt_filetransfer::{send_file, TransferReceiver, DEFAULT_MAX_TRANSFER_BYTES};
use prdt_input_win::{
    clipboard_sequence_number, read_clipboard_text, write_clipboard_text, RawInputCapturer,
    MAX_CLIPBOARD_BYTES,
};
use prdt_media_win::{
    pick_default_adapter, D3d11Device, D3d11Texture, MfD3d11Consumer, Nv12Renderer,
    NvdecD3d11Consumer, SwapChain,
};
use prdt_protocol::{frame::Codec, ControlMessage, InputEvent, MonitorRect, VideoConsumer};

mod latency;
use latency::LatencyProbe;
use prdt_transport::{
    viewer_handshake, CustomUdpTransport, HelloRequest, ReceivedMessage, Transport,
    UdpTransportConfig, DEFAULT_HANDSHAKE_TIMEOUT, DEFAULT_HELLO_RETRIES, DEFAULT_HELLO_TIMEOUT,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tokio::sync::mpsc;
use tracing::{info, warn};
use windows::Win32::Foundation::HWND;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::PhysicalKey;
use winit::platform::scancode::PhysicalKeyExtScancode;
use winit::window::{Window, WindowId};

#[derive(Parser, Debug)]
#[command(name = "prdt-viewer", about = "power-remote-dt viewer")]
struct Args {
    /// Host address, e.g. 192.168.1.5:9000 or 127.0.0.1:9000.
    #[arg(long)]
    host: SocketAddr,

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

    /// Decoder backend. `mf` (default) uses Media Foundation's H.265 MFT
    /// via the HEVC Video Extensions store app. `nvdec` attempts the
    /// direct nvcuvid.dll path added in Plan 2d; when its FFI isn't yet
    /// wired up (CUDA Toolkit not installed / NvdecD3d11Consumer not
    /// implemented), the viewer logs a warning and falls back to mf.
    #[arg(long, default_value = "mf", value_parser = ["mf", "nvdec"])]
    decoder: String,
}

fn parse_resolution(s: &str) -> Result<(u32, u32)> {
    let (w, h) = s
        .split_once('x')
        .with_context(|| format!("bad --resolution {s:?}, expected WIDTHxHEIGHT"))?;
    let w: u32 = w.parse().with_context(|| format!("bad width in {s:?}"))?;
    let h: u32 = h.parse().with_context(|| format!("bad height in {s:?}"))?;
    Ok((w, h))
}

/// Shared state between the winit main thread and the tokio worker thread.
struct ViewerShared {
    /// Latest decoded texture alongside the host capture timestamp (in the
    /// shared monotonic clock). The render thread pops this, presents, and
    /// feeds `host_ts_us` into the LatencyProbe to close the glass-to-glass
    /// measurement loop.
    latest_texture: Arc<Mutex<Option<(D3d11Texture, u64)>>>,
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
}

/// Render state (main thread only).
struct ViewerRender {
    window: Arc<Window>,
    #[allow(dead_code)]
    dev: D3d11Device,
    swap: SwapChain,
    renderer: Option<Nv12Renderer>,
}

struct ViewerApp {
    req_w: u32,
    req_h: u32,
    shared: Arc<ViewerShared>,
    render: Option<ViewerRender>,
    dev: D3d11Device,
    // The tokio runtime running the UDP / decode worker thread; kept alive
    // for the duration of the event loop.
    _runtime: tokio::runtime::Runtime,
    /// Set by `render_frame` when it hits an unrecoverable error (e.g.
    /// D3D11 device removed). `about_to_wait` sees it and calls
    /// `event_loop.exit()` so the next iteration tears down cleanly.
    should_exit: bool,
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

        info!("extracting HWND");
        let hwnd = match extract_hwnd(&window) {
            Ok(h) => h,
            Err(e) => {
                warn!(?e, "failed to extract HWND");
                event_loop.exit();
                return;
            }
        };
        info!("HWND extracted");

        let size = window.inner_size();
        info!(
            width = size.width,
            height = size.height,
            "creating swapchain"
        );
        let swap =
            match SwapChain::new_for_hwnd(&self.dev, hwnd, size.width.max(1), size.height.max(1)) {
                Ok(s) => s,
                Err(e) => {
                    warn!(?e, "failed to create swapchain");
                    event_loop.exit();
                    return;
                }
            };
        info!("swapchain created");

        self.render = Some(ViewerRender {
            window: Arc::clone(&window),
            dev: self.dev.clone(),
            swap,
            renderer: None,
        });
        window.request_redraw();
        info!("resumed done, first redraw requested");
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
                    if let Err(e) = r.swap.resize(width.max(1), height.max(1)) {
                        warn!(?e, "swapchain resize failed");
                    }
                    if let Some(rn) = r.renderer.as_mut() {
                        rn.resize_output(width.max(1), height.max(1));
                    }
                    r.window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(r) = self.render.as_ref() {
                    self.emit_mouse_move(position, &r.swap);
                }
            }
            WindowEvent::MouseInput { button, state, .. } => {
                if let Some(btn) = RawInputCapturer::map_winit_mouse_button(button) {
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
        // Keep the render loop ticking. The present call gates on vsync so this
        // cannot spin at infinite rate once a texture has arrived; before the
        // first texture, we clear + present, still vsync-bound.
        if let Some(r) = self.render.as_ref() {
            r.window.request_redraw();
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        info!("viewer exiting");
    }
}

impl ViewerApp {
    fn emit_mouse_move(&self, position: PhysicalPosition<f64>, swap: &SwapChain) {
        let window_w = (swap.width() as f64).max(1.0);
        let window_h = (swap.height() as f64).max(1.0);
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

        // Pull the latest decoded texture, if any.
        let maybe_tex = self.shared.latest_texture.lock().unwrap().take();
        let mut presented_host_ts: Option<u64> = None;

        if let Some((tex, host_ts_us)) = maybe_tex {
            // Lazily build the renderer once we know the decoded frame size.
            let (iw, ih) = (tex.width(), tex.height());
            let needs_new = match render.renderer.as_ref() {
                Some(r) => r.input_size() != (iw, ih),
                None => true,
            };
            if needs_new {
                match Nv12Renderer::new(
                    &self.dev,
                    iw,
                    ih,
                    render.swap.width(),
                    render.swap.height(),
                ) {
                    Ok(r) => render.renderer = Some(r),
                    Err(e) => {
                        warn!(?e, "Nv12Renderer::new failed");
                        return;
                    }
                }
                // Cache the observed stream size so input scaling remains sane.
                *self.shared.stream_width.lock().unwrap() = iw;
                *self.shared.stream_height.lock().unwrap() = ih;
            }

            if let Some(r) = render.renderer.as_ref() {
                if let Err(e) = r.render(&tex, &render.swap) {
                    warn!(?e, "Nv12Renderer::render failed");
                }
            }
            presented_host_ts = Some(host_ts_us);
        }

        match render.swap.present(true) {
            Ok(()) => {
                if let Some(ts) = presented_host_ts {
                    self.shared.latency.record_present_for_host_ts(ts);
                }
            }
            Err(e) if e.is_device_removed() => {
                // TDR / driver crash / hybrid-GPU swap. Every D3D11 resource
                // is now dead. In-place recovery would require rebuilding
                // the device, swapchain, renderer, decoder, and all their
                // upstream references. For Phase 0 we log with the reason
                // HRESULT and ask winit to exit so the user can just restart
                // (systemd-style). Plan 4 F7 will add in-place recreation.
                tracing::error!(
                    ?e,
                    "D3D11 device removed — viewer cannot continue; \
                     restart the process. Common causes: NVIDIA driver \
                     TDR, driver crash, hybrid-GPU switch, hot-unplug.",
                );
                self.should_exit = true;
            }
            Err(e) => warn!(?e, "Present failed"),
        }
    }
}

/// Saturating cast u64 → u32 for telemetry fields. A latency beyond ~71
/// minutes would overflow — treat that as "big", not as wraparound.
fn clamp_u32(v: u64) -> u32 {
    v.try_into().unwrap_or(u32::MAX)
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

fn extract_hwnd(window: &Window) -> Result<HWND> {
    let handle = window.window_handle().context("window_handle()")?.as_raw();
    match handle {
        RawWindowHandle::Win32(h) => Ok(HWND(h.hwnd.get() as *mut _)),
        other => anyhow::bail!("unexpected window handle type: {:?}", other),
    }
}

fn physical_key_to_scancode(key: PhysicalKey) -> Option<u32> {
    PhysicalKeyExtScancode::to_scancode(key)
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Log panics via tracing so they appear in viewer.log even with panic=abort.
    std::panic::set_hook(Box::new(|info| {
        tracing::error!(panic = %info, "PANIC");
    }));

    let args = Args::parse();
    let (req_w, req_h) = parse_resolution(&args.resolution)?;
    let pubkey = match (&args.host_pubkey, &args.known_hosts) {
        (Some(b64), _) => {
            PubKey::from_base64(b64).map_err(|e| anyhow::anyhow!("invalid --host-pubkey: {e}"))?
        }
        (None, Some(path)) => {
            let kh = KnownHosts::load(path)
                .with_context(|| format!("load --known-hosts {}", path.display()))?;
            let host_str = args.host.to_string();
            kh.get(&host_str).copied().with_context(|| {
                format!(
                    "no entry for {host_str} in known-hosts file ({} entries)",
                    kh.len()
                )
            })?
        }
        (None, None) => {
            anyhow::bail!("one of --host-pubkey or --known-hosts is required");
        }
    };

    // D3D11 device. Created on the main thread, cloned into the worker so the
    // decoder uses the same device (required for zero-copy texture handoff).
    let adapter = pick_default_adapter().context("no GPU adapter")?;
    let dev = D3d11Device::create(&adapter).context("D3D11 device")?;

    info!(
        host = %args.host,
        resolution = %args.resolution,
        fps = args.fps,
        adapter = %dev.adapter().name,
        "viewer starting"
    );

    // Input channel: winit main thread → tokio send loop.
    let (input_tx, input_rx) = mpsc::unbounded_channel::<InputEvent>();
    // File-drop channel: winit main thread → tokio file-transfer task.
    let (file_drop_tx, file_drop_rx) = mpsc::unbounded_channel::<std::path::PathBuf>();

    let shared = Arc::new(ViewerShared {
        latest_texture: Arc::new(Mutex::new(None)),
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
        dev.clone(),
        Arc::clone(&shared),
        input_rx,
        file_drop_rx,
        args.host,
        pubkey,
        req_w,
        req_h,
        args.fps,
        args.recv_dir.clone(),
        args.decoder.clone(),
    );

    // Build the event loop + app.
    let event_loop = EventLoop::new().context("EventLoop::new")?;
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = ViewerApp {
        req_w,
        req_h,
        shared,
        render: None,
        dev,
        _runtime: runtime,
        should_exit: false,
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
    dev: D3d11Device,
    shared: Arc<ViewerShared>,
    mut input_rx: mpsc::UnboundedReceiver<InputEvent>,
    mut file_drop_rx: mpsc::UnboundedReceiver<std::path::PathBuf>,
    host_addr: SocketAddr,
    pubkey: PubKey,
    req_w: u32,
    req_h: u32,
    req_fps: u32,
    recv_dir: std::path::PathBuf,
    decoder: String,
) {
    handle.clone().spawn(async move {
        // Bind an ephemeral UDP socket and point it at the host. The host binds
        // the well-known port; we are the initiator.
        let bind_addr: SocketAddr = if host_addr.is_ipv4() {
            "0.0.0.0:0".parse().unwrap()
        } else {
            "[::]:0".parse().unwrap()
        };
        let cfg = UdpTransportConfig::default();
        let transport = match CustomUdpTransport::bind(bind_addr, cfg).await {
            Ok(t) => Arc::new(t),
            Err(e) => {
                warn!(?e, "UDP bind failed");
                return;
            }
        };
        transport.configure_peer(host_addr).await;
        info!(%host_addr, local = ?transport.local_addr().ok(), "viewer transport ready");

        // Noise client handshake first (establishes encrypted channel).
        // Uses DEFAULT_HANDSHAKE_TIMEOUT so a wrong pubkey or unreachable host
        // fails fast instead of hanging the viewer forever.
        if let Err(e) = transport
            .handshake_as_client(&pubkey, DEFAULT_HANDSHAKE_TIMEOUT)
            .await
        {
            warn!(?e, "Noise client handshake failed");
            return;
        }
        tracing::info!("Noise handshake complete");

        // Handshake.
        let req = HelloRequest {
            req_width: req_w,
            req_height: req_h,
            req_fps,
            codec: Codec::H265,
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

        // Build consumer. --decoder nvdec tries the Plan 2d path first;
        // until the NVDEC FFI is wired up it always returns NotAvailable
        // and we transparently fall back to MF with a warning.
        let consumer = if decoder == "nvdec" {
            match NvdecD3d11Consumer::new(&dev, ack.neg_width, ack.neg_height) {
                Ok(_nv) => {
                    // Once the FFI lands, we'll replace this with
                    // Arc::new(Mutex::new(_nv)). For now bail to MF.
                    warn!(
                        "unreachable: NvdecD3d11Consumer::new returned Ok but \
                         the VideoConsumer trait impl is a stub — falling back to MF",
                    );
                    None
                }
                Err(e) => {
                    warn!(%e, "NVDEC unavailable; falling back to MF decoder");
                    None
                }
            }
        } else {
            None
        }
        .unwrap_or_else(|| {
            // Fallback / default path.
            Arc::new(tokio::sync::Mutex::new(
                match MfD3d11Consumer::new(&dev, ack.neg_width, ack.neg_height) {
                    Ok(c) => c,
                    Err(e) => {
                        warn!(?e, "MfD3d11Consumer::new failed");
                        panic!("no decoder could be initialized");
                    }
                },
            ))
        });
        info!(backend = %decoder, "decoder ready; spawning worker tasks");

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
                        if let Err(e) = c.submit(frame).await {
                            warn!(?e, seq, is_kf, nal_len, "consumer.submit error");
                            continue;
                        }
                        if let Some(tex) = c.take_latest_texture() {
                            tex_count += 1;
                            recv_shared.latency.record_decoded(seq);
                            *recv_shared.latest_texture.lock().unwrap() = Some((tex, host_ts_us));
                        }
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

        // M1 latency reporter: every 1s log p50/p95/p99 locally; every 5s
        // also send a `LatencyReport` to the host so the host's logs show
        // what the viewer is actually experiencing (useful for distributed
        // debugging on real LAN). snapshot() is cheap (clones two small
        // VecDeques of u64s).
        let latency_probe = Arc::clone(&shared.latency);
        let latency_transport = Arc::clone(&transport);
        let latency_task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(1));
            ticker.tick().await; // fire first tick immediately; skip it
            let mut ticks_since_report: u32 = 0;
            loop {
                ticker.tick().await;
                let snap = latency_probe.snapshot();
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
}
