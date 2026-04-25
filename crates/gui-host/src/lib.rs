//! Phase 4 G1 host GUI.

mod app;
#[allow(dead_code)] // wired into Settings/HostApp in G3 Task 5
mod autostart;
mod keygen;
#[allow(dead_code)] // wired into HostApp in G3 Task 5
mod notif;
mod settings;
#[allow(dead_code)] // wired into HostApp in G3 Task 5
mod tray;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use prdt_gui_common::{install_jp_font, Config, TailLayer};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

/// Closure injected by the host binary so the GUI can spawn the real
/// `run_host` without depending on `prdt-host` (which depends on
/// `prdt-gui-host` already — depending the other way would cycle).
pub type RunHostFn =
    Arc<dyn Fn(CancellationToken) -> tokio::task::JoinHandle<anyhow::Result<()>> + Send + Sync>;

/// Run the host GUI as the main blocking call. Returns when the user
/// closes the window.
pub fn run_host_gui(config_path: Option<PathBuf>, run_host: RunHostFn) -> anyhow::Result<()> {
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

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let rt_handle = runtime.handle().clone();
    let _enter = runtime.enter();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([720.0, 480.0])
            .with_min_inner_size([520.0, 360.0]),
        ..Default::default()
    };

    let cfg = shared_cfg.clone();
    let path = config_path.clone();
    let tail = tail_handle.clone();
    let title = prdt_gui_common::tr("host-window-title");
    eframe::run_native(
        &title,
        options,
        Box::new(move |cc| {
            install_jp_font(&cc.egui_ctx);
            Ok(Box::new(app::HostApp::new(
                cfg, path, tail, rt_handle, run_host,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;

    drop(_enter);
    drop(runtime);
    Ok(())
}
