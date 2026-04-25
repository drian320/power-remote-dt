//! Phase 4 G3 system tray. Owns the TrayIcon, three pre-loaded icon
//! variants, and a menu-event receiver. Callers (HostApp::update) call
//! `poll_menu()` once per frame to drain user clicks.

use std::path::Path;

use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostState {
    Idle,
    Listening,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    OpenSettings,
    StopListening,
    ShowLogs,
    Quit,
}

const ID_OPEN_SETTINGS: &str = "prdt-tray-open-settings";
const ID_STOP_LISTENING: &str = "prdt-tray-stop-listening";
const ID_SHOW_LOGS: &str = "prdt-tray-show-logs";
const ID_QUIT: &str = "prdt-tray-quit";

/// Resolve which PNG file to load for a given host state. Used by the
/// caller and exposed for unit testing the mapping.
pub fn icon_path_for_state(state: HostState) -> &'static str {
    match state {
        HostState::Idle => "tray-idle.png",
        HostState::Listening => "tray-listening.png",
        HostState::Error => "tray-error.png",
    }
}

pub struct TrayController {
    icon: TrayIcon,
    icon_idle: Icon,
    icon_listening: Icon,
    icon_error: Icon,
}

impl TrayController {
    pub fn new(asset_dir: &Path) -> Result<Self, TrayError> {
        let icon_idle = load_icon(&asset_dir.join(icon_path_for_state(HostState::Idle)))?;
        let icon_listening =
            load_icon(&asset_dir.join(icon_path_for_state(HostState::Listening)))?;
        let icon_error = load_icon(&asset_dir.join(icon_path_for_state(HostState::Error)))?;

        let menu = Menu::new();
        menu.append_items(&[
            &MenuItem::with_id(MenuId::new(ID_OPEN_SETTINGS), "Open settings", true, None),
            &MenuItem::with_id(MenuId::new(ID_STOP_LISTENING), "Stop listening", true, None),
            &MenuItem::with_id(MenuId::new(ID_SHOW_LOGS), "Show logs", true, None),
            &MenuItem::with_id(MenuId::new(ID_QUIT), "Quit", true, None),
        ])
        .map_err(|e| TrayError::Build(format!("menu append: {e}")))?;

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("PrdtHost")
            .with_icon(icon_idle.clone())
            .build()
            .map_err(|e| TrayError::Build(format!("tray build: {e}")))?;

        Ok(Self {
            icon,
            icon_idle,
            icon_listening,
            icon_error,
        })
    }

    /// Update the visible icon to match the host's current state. Cheap —
    /// this just sets a pre-loaded icon handle on the existing TrayIcon.
    pub fn set_state(&self, state: HostState) {
        let icon = match state {
            HostState::Idle => &self.icon_idle,
            HostState::Listening => &self.icon_listening,
            HostState::Error => &self.icon_error,
        };
        if let Err(e) = self.icon.set_icon(Some(icon.clone())) {
            tracing::warn!(?e, "tray set_icon failed");
        }
    }

    /// Drain pending tray menu events. Returns the most recent action
    /// (older events are silently discarded — only the latest click wins
    /// per frame).
    pub fn poll_menu(&self) -> Option<TrayAction> {
        let rx = MenuEvent::receiver();
        let mut latest: Option<TrayAction> = None;
        while let Ok(ev) = rx.try_recv() {
            let action = match ev.id().0.as_str() {
                ID_OPEN_SETTINGS => Some(TrayAction::OpenSettings),
                ID_STOP_LISTENING => Some(TrayAction::StopListening),
                ID_SHOW_LOGS => Some(TrayAction::ShowLogs),
                ID_QUIT => Some(TrayAction::Quit),
                other => {
                    tracing::warn!(other, "unknown tray menu id");
                    None
                }
            };
            if action.is_some() {
                latest = action;
            }
        }
        latest
    }
}

#[derive(thiserror::Error, Debug)]
pub enum TrayError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("image: {0}")]
    Image(#[from] image::ImageError),
    #[error("tray-icon: {0}")]
    TrayIcon(String),
    #[error("build: {0}")]
    Build(String),
}

fn load_icon(path: &Path) -> Result<Icon, TrayError> {
    let img = image::open(path)?.to_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h)
        .map_err(|e| TrayError::TrayIcon(format!("from_rgba: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_state_maps_to_distinct_filenames() {
        assert_eq!(icon_path_for_state(HostState::Idle), "tray-idle.png");
        assert_eq!(icon_path_for_state(HostState::Listening), "tray-listening.png");
        assert_eq!(icon_path_for_state(HostState::Error), "tray-error.png");
    }

    #[test]
    fn load_icon_reads_generated_placeholders() {
        // The build.rs generates assets/tray-idle.png at compile time, so
        // it should be readable from CARGO_MANIFEST_DIR/assets/.
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");
        for state in [HostState::Idle, HostState::Listening, HostState::Error] {
            let p = dir.join(icon_path_for_state(state));
            let _ = load_icon(&p).expect("placeholder PNG loadable");
        }
    }
}
