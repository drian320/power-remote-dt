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
