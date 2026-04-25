//! Phase 4 G1 host GUI (skeleton — Task 5 adds Listening state).

mod app;
mod keygen;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use prdt_gui_common::{install_jp_font, Config};

/// Run the host GUI as the main blocking call. Returns when the user
/// closes the window.
pub fn run_host_gui(config_path: Option<PathBuf>) -> anyhow::Result<()> {
    let config_path = config_path
        .or_else(prdt_gui_common::default_config_path)
        .ok_or_else(|| anyhow::anyhow!("could not resolve config path"))?;

    let config = Config::load(&config_path)?;
    let shared_cfg = Arc::new(Mutex::new(config));

    let cfg = shared_cfg.clone();
    let path = config_path.clone();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([720.0, 480.0])
            .with_min_inner_size([520.0, 360.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Power Remote Desktop — Host",
        options,
        Box::new(move |cc| {
            install_jp_font(&cc.egui_ctx);
            Ok(Box::new(app::HostApp::new(cfg, path)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))
}
