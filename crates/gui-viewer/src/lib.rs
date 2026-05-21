//! Phase 4 G1 viewer launcher GUI.

mod app;
mod connect_form;
mod hosts_list;
pub mod online_probe;
mod settings;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use prdt_gui_common::{install_theme, Config};

/// Returns the `--decoder` argument values valid for the current build/OS so
/// the launcher can populate its decoder dropdown without offering options
/// that fail at dispatch (e.g. Windows-only `mf`/`nvdec` on Linux, or ffmpeg
/// HEVC backends that were not compiled in).
///
/// This mirrors `prdt_viewer::supported_decoder_args`, but lives here because
/// `prdt-viewer` depends on this crate for the Windows launcher — a direct
/// dependency back on `prdt-viewer` would form a Cargo cycle. The ffmpeg
/// variants are gated on this crate's `*-any` marker features (see
/// `Cargo.toml`); the always-available native backends are unconditional.
///
/// Ordered `auto` first, then native backends for the target OS, then any
/// compiled-in ffmpeg HEVC variants.
pub fn supported_decoder_args() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = vec!["auto"];

    #[cfg(windows)]
    {
        out.push("nvdec");
        out.push("mf");
        out.push("openh264");
        #[cfg(feature = "media-win-ffmpeg-nvdec-any")]
        out.push("ffmpeg-nvdec-hevc");
        #[cfg(feature = "media-win-ffmpeg-nvdec-main10-any")]
        out.push("ffmpeg-nvdec-hevc-main10");
    }

    #[cfg(target_os = "linux")]
    {
        out.push("openh264");
        #[cfg(feature = "ffmpeg-decode-hevc-sw-any")]
        out.push("ffmpeg-sw-hevc");
        #[cfg(feature = "ffmpeg-decode-hevc-vaapi-any")]
        out.push("ffmpeg-vaapi-hevc");
        #[cfg(feature = "ffmpeg-decode-hevc-nvdec-any")]
        out.push("ffmpeg-nvdec-hevc");
        #[cfg(feature = "ffmpeg-decode-hevc-sw-main10-any")]
        out.push("ffmpeg-sw-hevc-main10");
        #[cfg(feature = "ffmpeg-decode-hevc-vaapi-main10-any")]
        out.push("ffmpeg-vaapi-hevc-main10");
        #[cfg(feature = "ffmpeg-decode-hevc-nvdec-main10-any")]
        out.push("ffmpeg-nvdec-hevc-main10");
    }

    out
}

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
        // Force wgpu — glow's glutin path fails on Wayland (COSMIC).
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    let title = prdt_gui_common::tr("viewer-window-title");
    eframe::run_native(
        &title,
        options,
        Box::new(move |cc| {
            install_theme(&cc.egui_ctx);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_decoder_args_always_contains_auto() {
        assert!(supported_decoder_args().contains(&"auto"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn supported_decoder_args_linux_excludes_windows_only_and_includes_openh264() {
        let args = supported_decoder_args();
        assert!(args.contains(&"openh264"));
        assert!(!args.contains(&"nvdec"));
        assert!(!args.contains(&"mf"));
    }
}
