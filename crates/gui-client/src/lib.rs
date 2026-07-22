//! Unified `prdt` client GUI.
//!
//! One RustDesk-style home screen: "This device" (server-allocated 9-digit ID
//! + fixed PIN + key fingerprint/QR + share start/stop) beside "Connect to a
//! device" (peer ID + PIN, plus a collapsible Advanced section for legacy
//! direct mode and a recent-connections list). Outbound viewer sessions run as
//! separate processes because the viewer owns a `winit` event loop and a D3D11
//! swapchain that cannot coexist with the egui window in the same process today
//! (ADR B1 / AC-5改: the process boundary is kept but hidden behind the UX).

mod app;
#[cfg(windows)]
mod elevate;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use prdt_gui_common::{install_theme, Config};

/// Run the unified client GUI as a blocking call. Returns when the user
/// closes the window. Cross-platform as of GUI modernization P2 (Linux +
/// Windows); the egui/eframe stack is identical on both, so there is no
/// platform split here.
pub fn run_client_gui(config_path: Option<PathBuf>, autostart_host: bool) -> anyhow::Result<()> {
    // Install tracing FIRST so config-load and everything after is captured.
    // The returned guard flushes the non-blocking file writer; it MUST stay
    // alive for the whole process, which this binding does — its scope spans
    // the blocking `eframe::run_native` call below (i.e. until the window
    // closes). Dropping it early would drop buffered log lines.
    let _log_guard = init_gui_tracing();

    let config_path = config_path
        .or_else(prdt_gui_common::default_config_path)
        .ok_or_else(|| anyhow::anyhow!("could not resolve config path"))?;

    let config = Config::load(&config_path)?;
    prdt_gui_common::init_locale(&config.gui.locale);
    let shared_cfg = Arc::new(Mutex::new(config));

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let rt_handle = runtime.handle().clone();
    let _enter = runtime.enter();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 620.0])
            .with_min_inner_size([560.0, 420.0]),
        // Force wgpu — glow's glutin path fails on Wayland (COSMIC).
        renderer: eframe::Renderer::Wgpu,
        // Drop wgpu's GL/GLES backend: its EGL init panics on Wayland
        // (wgpu-hal egl.rs `unwrap()` on None). egui-wgpu's default backends
        // are PRIMARY|GL; removing GL leaves Vulkan (Linux), DX12/Vulkan
        // (Windows), Metal (macOS). WGPU_BACKEND env still overrides.
        wgpu_options: {
            let mut o = eframe::egui_wgpu::WgpuConfiguration::default();
            if let eframe::egui_wgpu::WgpuSetup::CreateNew(c) = &mut o.wgpu_setup {
                c.instance_descriptor
                    .backends
                    .remove(eframe::wgpu::Backends::GL);
            }
            o
        },
        ..Default::default()
    };

    let cfg = shared_cfg.clone();
    let path = config_path.clone();
    eframe::run_native(
        "Power Remote Desktop",
        options,
        Box::new(move |cc| {
            install_theme(&cc.egui_ctx);
            Ok(Box::new(app::ClientApp::new(
                cfg,
                path,
                rt_handle,
                autostart_host,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;

    drop(_enter);
    drop(runtime);
    Ok(())
}

/// Initialize tracing for the unified client GUI process. Installs two layers:
///
/// 1. a stderr layer, unchanged in spirit from the host/viewer CLI's own
///    `tracing_subscriber::fmt` (honors `RUST_LOG`, default `info`); and
/// 2. a non-blocking, daily-rolling **file** layer at
///    `<config_root>/logs/prdt-gui.log` — no ANSI, timestamp + target — so the
///    logs survive a double-click / elevated-relaunch launch where stderr is
///    lost.
///
/// This is the ONLY place the unified client GUI installs a subscriber, and it
/// is reached exactly on the GUI entry paths (`prdt` no-subcommand and
/// `prdt connect` → launcher). The in-process host listener started from
/// `app::start_listener` (via `prdt_host::run_host`) emits through this same
/// global subscriber, so its logs land in the file too. The host/viewer CLI
/// subcommands keep their own `init_tracing` in `prdt-host` / `prdt-viewer` and
/// are unaffected — they already have stderr.
///
/// Returns the [`tracing_appender::non_blocking::WorkerGuard`] for the file
/// writer, or `None` when no file layer was installed (the log directory could
/// not be created → degrade to stderr-only, never panic). The caller must keep
/// the guard alive for the process lifetime.
#[must_use]
fn init_gui_tracing() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    use tracing_subscriber::Layer as _;

    // Per-layer filter so both stderr and file honor RUST_LOG (default `info`).
    fn env_filter() -> tracing_subscriber::EnvFilter {
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())
    }

    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(env_filter());

    // Best-effort file layer. If the logs dir can't be created (or there is no
    // config dir), fall back to stderr-only rather than failing GUI startup.
    let (file_layer, guard) = match prdt_gui_common::logs_dir() {
        Some(dir) => match std::fs::create_dir_all(&dir) {
            Ok(()) => {
                let appender = tracing_appender::rolling::daily(&dir, "prdt-gui.log");
                let (non_blocking, guard) = tracing_appender::non_blocking(appender);
                let layer = tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_target(true)
                    .with_writer(non_blocking)
                    .with_filter(env_filter());
                (Some(layer), Some(guard))
            }
            Err(e) => {
                eprintln!(
                    "prdt: could not create log directory {}: {e} — logging to stderr only",
                    dir.display()
                );
                (None, None)
            }
        },
        None => {
            eprintln!("prdt: no config directory for logs — logging to stderr only");
            (None, None)
        }
    };

    // `Option<Layer>` is itself a `Layer` (no-op when `None`), so this installs
    // the file layer only when it was built.
    tracing_subscriber::registry()
        .with(stderr_layer)
        .with(file_layer)
        .init();

    guard
}
