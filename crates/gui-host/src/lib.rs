//! Phase 4 G1 host GUI.

mod app;
pub mod auth_settings;
#[allow(dead_code)] // is_enabled() consumed in Phase 4 G4+ for query UI
mod autostart;
pub mod consent_channel;
pub mod consent_prompt;
mod keygen;
mod notif;
pub mod onboarding;
mod settings;
mod tray;
pub mod update;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use consent_channel::ConsentSender;
use prdt_gui_common::{install_theme, Config, TailLayer};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

/// Closure injected by the host binary so the GUI can spawn the real
/// `run_host` without depending on `prdt-host` (which depends on
/// `prdt-gui-host` already — depending the other way would cycle).
pub type RunHostFn = Arc<
    dyn Fn(CancellationToken, ConsentSender) -> tokio::task::JoinHandle<anyhow::Result<()>>
        + Send
        + Sync,
>;

/// Run the host GUI as the main blocking call. Returns when the user
/// closes the window.
///
/// `binary_name` should be `env!("CARGO_PKG_NAME")` from the calling binary
/// so that `CrashReport.binary` is consistent regardless of launch mode.
pub fn run_host_gui(
    binary_name: &'static str,
    config_path: Option<PathBuf>,
    run_host: RunHostFn,
) -> anyhow::Result<()> {
    let config_path = config_path
        .or_else(prdt_gui_common::default_config_path)
        .ok_or_else(|| anyhow::anyhow!("could not resolve config path"))?;

    let config = Config::load(&config_path)?;
    prdt_gui_common::init_locale(&config.gui.locale);
    let shared_cfg = Arc::new(Mutex::new(config));

    let (tail_layer, tail_handle) = TailLayer::new(200);
    let _ = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        ))
        .with(tail_layer)
        .try_init();

    // Phase 4 G5: feed the panic hook so crash dumps include recent log lines.
    prdt_gui_common::register_tail(tail_handle.clone());
    prdt_gui_common::install_panic_hook(binary_name, env!("CARGO_PKG_VERSION"));

    // Read any unacknowledged crash reports from previous runs.
    let pending_crashes = match prdt_gui_common::list_pending_crashes() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(?e, "failed to list pending crashes");
            Vec::new()
        }
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let rt_handle = runtime.handle().clone();
    let _enter = runtime.enter();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([720.0, 480.0])
            .with_min_inner_size([520.0, 360.0]),
        // Force wgpu — glow's glutin path fails on Wayland (COSMIC).
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    // Phase 4 G3: Build the system tray (best-effort — failures are logged
    // and the app continues without a tray icon).
    let asset_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");
    let tray = match tray::TrayController::new(&asset_dir) {
        Ok(t) => Some(t),
        Err(e) => {
            tracing::warn!(?e, "tray init failed; continuing without tray icon");
            None
        }
    };

    let cfg = shared_cfg.clone();
    let path = config_path.clone();
    let tail = tail_handle.clone();
    let title = prdt_gui_common::tr("host-window-title");
    eframe::run_native(
        &title,
        options,
        Box::new(move |cc| {
            install_theme(&cc.egui_ctx);
            Ok(Box::new(app::HostApp::new(
                cfg,
                path,
                tail,
                rt_handle,
                run_host,
                tray,
                pending_crashes,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;

    drop(_enter);
    drop(runtime);
    Ok(())
}
