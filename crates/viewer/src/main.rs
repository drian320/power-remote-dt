#![cfg(windows)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use prdt_crypto::{KnownHosts, PubKey};
use prdt_input_win::RawInputCapturer;
use prdt_media_win::{
    pick_default_adapter, D3d11Device, D3d11Texture, MfD3d11Consumer, Nv12Renderer, SwapChain,
};
use prdt_protocol::{frame::Codec, ControlMessage, InputEvent, VideoConsumer};
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
    /// Latest decoded texture, populated by the tokio consumer loop.
    latest_texture: Arc<Mutex<Option<D3d11Texture>>>,
    /// Stream dimensions negotiated from HelloAck (and later refined by the
    /// decoder's reported texture size).
    stream_width: Mutex<u32>,
    stream_height: Mutex<u32>,
    /// Input events captured by winit, drained by the tokio send loop.
    input_tx: mpsc::UnboundedSender<InputEvent>,
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
            WindowEvent::RedrawRequested => {
                self.render_frame();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
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
        let window_w = swap.width() as f64;
        let window_h = swap.height() as f64;
        let norm_x = position.x / window_w.max(1.0);
        let norm_y = position.y / window_h.max(1.0);
        let abs_x = (norm_x * 65535.0).clamp(0.0, 65535.0) as i32;
        let abs_y = (norm_y * 65535.0).clamp(0.0, 65535.0) as i32;
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

        if let Some(tex) = maybe_tex {
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
        }

        if let Err(e) = render.swap.present(true) {
            warn!(?e, "Present failed");
        }
    }
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

    let shared = Arc::new(ViewerShared {
        latest_texture: Arc::new(Mutex::new(None)),
        stream_width: Mutex::new(req_w),
        stream_height: Mutex::new(req_h),
        input_tx,
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
        args.host,
        pubkey,
        req_w,
        req_h,
        args.fps,
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
    host_addr: SocketAddr,
    pubkey: PubKey,
    req_w: u32,
    req_h: u32,
    req_fps: u32,
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
            "handshake complete"
        );
        *shared.stream_width.lock().unwrap() = ack.neg_width;
        *shared.stream_height.lock().unwrap() = ack.neg_height;

        // Build consumer.
        let consumer = match MfD3d11Consumer::new(&dev, ack.neg_width, ack.neg_height) {
            Ok(c) => Arc::new(tokio::sync::Mutex::new(c)),
            Err(e) => {
                warn!(?e, "MfD3d11Consumer::new failed");
                return;
            }
        };
        info!("MfD3d11Consumer created; spawning worker tasks");

        // Recv loop: video → consumer; also handle control messages.
        let recv_shared = Arc::clone(&shared);
        let recv_transport = Arc::clone(&transport);
        let recv_consumer = Arc::clone(&consumer);
        let recv_task = tokio::spawn(async move {
            info!("recv_task started");
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
                        let is_kf = frame.is_keyframe;
                        let nal_len = frame.nal_units.len();
                        let mut c = recv_consumer.lock().await;
                        if let Err(e) = c.submit(frame).await {
                            warn!(?e, seq, is_kf, nal_len, "consumer.submit error");
                            continue;
                        }
                        if let Some(tex) = c.take_latest_texture() {
                            tex_count += 1;
                            *recv_shared.latest_texture.lock().unwrap() = Some(tex);
                        }
                    }
                    Ok(ReceivedMessage::Control(ControlMessage::Bye)) => {
                        info!("host sent Bye");
                        break;
                    }
                    Ok(ReceivedMessage::Control(ControlMessage::Pong { .. })) => {
                        control_count += 1;
                    }
                    Ok(ReceivedMessage::Control(_)) => {
                        control_count += 1;
                    }
                    Ok(ReceivedMessage::Input(_)) => {
                        input_count += 1;
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

        // If any loop exits, tear down.
        tokio::select! {
            _ = recv_task => info!("recv task ended"),
            _ = send_task => info!("send task ended"),
            _ = ping_task => info!("ping task ended"),
        }
    });
}
