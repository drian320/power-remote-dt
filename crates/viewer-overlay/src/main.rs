//! Phase 4 G2 viewer overlay binary. Spawned by prdt-viewer when the user
//! presses ESC. Reads stats.json from the IPC dir and shows an eframe
//! window with latency / decoder info plus Resume / Disconnect buttons.

use std::path::PathBuf;

use clap::Parser;
use prdt_gui_common::install_jp_font;

mod app;
mod ipc;

#[derive(Parser, Debug)]
#[command(name = "prdt-viewer-overlay")]
struct Args {
    /// Per-PID IPC directory (under dirs::cache_dir()/prdt/overlay-ipc/<pid>/).
    /// Required — passed by the spawning viewer.
    #[arg(long)]
    ipc_dir: PathBuf,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    prdt_gui_common::install_panic_hook(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));

    let args = Args::parse();

    // Apply the user's locale preference (best-effort: read viewer's
    // config.toml if it exists, else OS detect).
    let locale = prdt_gui_common::default_config_path()
        .and_then(|p| prdt_gui_common::Config::load(&p).ok())
        .map(|c| c.gui.locale)
        .unwrap_or_default();
    prdt_gui_common::init_locale(&locale);

    let ipc_dir = args.ipc_dir.clone();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([360.0, 280.0])
            .with_min_inner_size([320.0, 240.0])
            .with_resizable(false),
        ..Default::default()
    };
    eframe::run_native(
        &prdt_gui_common::tr("overlay-window-title"),
        options,
        Box::new(move |cc| {
            install_jp_font(&cc.egui_ctx);
            Ok(Box::new(app::OverlayApp::new(ipc_dir)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))
}
