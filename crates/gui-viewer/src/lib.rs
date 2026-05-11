//! Phase 4 G1 viewer launcher GUI.

mod app;
mod connect_form;
mod hosts_list;
pub mod online_probe;
mod settings;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use prdt_gui_common::{install_jp_font, Config};

#[derive(Debug, Clone, PartialEq)]
pub enum ConnectMode {
    Direct,
    Signaling,
}

#[derive(Debug, Clone)]
pub struct ConnectArgs {
    pub mode: ConnectMode,
    pub direct_addr: Option<std::net::SocketAddr>,
    pub signaling_url: Option<url::Url>,
    pub host_id: Option<String>,
    pub pubkey: Option<String>,
    pub decoder: String,
    pub recv_dir: PathBuf,
    pub known_hosts_path: PathBuf,
    pub known_host_ids_path: PathBuf,
    pub default_resolution: String,
    pub default_fps: u32,
}

#[derive(Debug)]
pub enum LaunchOutcome {
    Connect(Box<ConnectArgs>),
    Quit,
}

/// Run the viewer launcher as a blocking call. Returns when the user
/// presses Connect (with `LaunchOutcome::Connect`) or closes the
/// window (with `LaunchOutcome::Quit`).
pub fn run_viewer_launcher(config_path: Option<PathBuf>) -> anyhow::Result<LaunchOutcome> {
    let config_path = config_path
        .or_else(prdt_gui_common::default_config_path)
        .ok_or_else(|| anyhow::anyhow!("could not resolve config path"))?;

    let config = Config::load(&config_path)?;
    prdt_gui_common::init_locale(&config.gui.locale);
    let shared_cfg = Arc::new(Mutex::new(config));
    let outcome: Arc<Mutex<Option<LaunchOutcome>>> = Arc::new(Mutex::new(None));

    let cfg = shared_cfg.clone();
    let path = config_path.clone();
    let out = outcome.clone();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([640.0, 520.0])
            .with_min_inner_size([520.0, 360.0]),
        ..Default::default()
    };

    let title = prdt_gui_common::tr("viewer-window-title");
    eframe::run_native(
        &title,
        options,
        Box::new(move |cc| {
            install_jp_font(&cc.egui_ctx);
            Ok(Box::new(app::LauncherApp::new(cfg, path, out)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;

    let outcome = outcome
        .lock()
        .unwrap()
        .take()
        .unwrap_or(LaunchOutcome::Quit);
    Ok(outcome)
}
