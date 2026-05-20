//! Unified `prdt` client GUI.
//!
//! Two tabs in one window: "This Device" (host listener controls) and
//! "Connect" (peer ID + Connect button that spawns `prdt connect ...` as
//! a child process). Outbound viewer sessions run as separate processes
//! because the viewer owns a `winit` event loop and a D3D11 swapchain that
//! cannot coexist with the egui window in the same process today.

mod app;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use prdt_gui_common::{install_jp_font, Config};

/// Run the unified client GUI as a blocking call. Returns when the user
/// closes the window. Cross-platform as of GUI modernization P2 (Linux +
/// Windows); the egui/eframe stack is identical on both, so there is no
/// platform split here.
pub fn run_client_gui(config_path: Option<PathBuf>) -> anyhow::Result<()> {
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
            .with_inner_size([720.0, 520.0])
            .with_min_inner_size([520.0, 360.0]),
        ..Default::default()
    };

    let cfg = shared_cfg.clone();
    let path = config_path.clone();
    eframe::run_native(
        "Power Remote Desktop",
        options,
        Box::new(move |cc| {
            install_jp_font(&cc.egui_ctx);
            Ok(Box::new(app::ClientApp::new(cfg, path, rt_handle)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;

    drop(_enter);
    drop(runtime);
    Ok(())
}
