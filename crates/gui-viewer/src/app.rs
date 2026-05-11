use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use prdt_gui_common::{t, Config, HostEntry};
use tracing::warn;

use crate::LaunchOutcome;

/// Stable identity for a saved host that survives sort reordering.
/// Uses (label, addr, host_id) — unique enough given the UI prevents duplicates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostKey {
    pub label: String,
    pub addr: String,
    pub host_id: String,
}

impl HostKey {
    pub(crate) fn from_entry(e: &HostEntry) -> Self {
        Self {
            label: e.label.clone(),
            addr: e.addr.clone(),
            host_id: e.host_id.clone(),
        }
    }
}

pub struct LauncherApp {
    pub(crate) config: Arc<Mutex<Config>>,
    pub(crate) config_path: PathBuf,
    pub(crate) outcome: Arc<Mutex<Option<LaunchOutcome>>>,
    /// Identity of the selected host — stable across sort reorderings.
    pub(crate) selected: Option<HostKey>,
    pub(crate) add_form_open: bool,
    pub(crate) settings_open: bool,
    pub(crate) error: Option<String>,
    pub(crate) draft_host: crate::connect_form::DraftHost,
    /// Shared map written by the OnlineProbe background task (host addr/id → online).
    pub(crate) online_sink: Arc<Mutex<HashMap<String, bool>>>,
    /// Keep the probe stop handle alive; dropped first to signal graceful stop.
    _probe_handle: Option<crate::online_probe::StopHandle>,
    /// Keep the tokio runtime alive; dropped after the probe handle.
    _runtime: Option<tokio::runtime::Runtime>,
}

impl LauncherApp {
    pub fn new(
        config: Arc<Mutex<Config>>,
        config_path: PathBuf,
        outcome: Arc<Mutex<Option<LaunchOutcome>>>,
    ) -> Self {
        let online_sink: Arc<Mutex<HashMap<String, bool>>> = Arc::new(Mutex::new(HashMap::new()));

        // Single-thread runtime: one 30s WS probe doesn't need a thread pool.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| {
                warn!(error = %e, "tokio runtime init failed; online probe disabled");
                e
            })
            .ok();

        // Spawn the probe only when a signaling URL is configured.
        let probe_handle = if let Some(rt) = runtime.as_ref() {
            let cfg = config.lock().unwrap_or_else(|p| p.into_inner());
            let signaling_url_str = cfg.viewer.signaling_url.clone();
            let host_ids: Vec<String> = cfg
                .viewer
                .hosts
                .iter()
                .filter(|h| h.mode == "signaling" && !h.host_id.is_empty())
                .map(|h| h.host_id.clone())
                .collect();
            drop(cfg);

            if !signaling_url_str.is_empty() {
                if let Ok(url) = url::Url::parse(&signaling_url_str) {
                    let ids = Arc::new(Mutex::new(host_ids));
                    let sink = online_sink.clone();
                    let _guard = rt.enter();
                    Some(crate::online_probe::spawn(url, ids, sink))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        Self {
            config,
            config_path,
            outcome,
            selected: None,
            add_form_open: false,
            settings_open: false,
            error: None,
            draft_host: crate::connect_form::DraftHost::default(),
            online_sink,
            _probe_handle: probe_handle,
            _runtime: runtime,
        }
    }
}

impl eframe::App for LauncherApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.add_form_open {
            crate::connect_form::render(ctx, self);
        }
        if self.settings_open {
            crate::settings::render(ctx, self);
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading(t!("viewer-launcher-heading"));
            ui.add_space(8.0);
            crate::hosts_list::render(ui, self);

            ui.add_space(12.0);
            let decoder = self.config.lock().unwrap().viewer.decoder.clone();
            ui.horizontal(|ui| {
                ui.label(t!("viewer-decoder-label"));
                ui.label(&decoder);
                if ui.button(t!("host-button-settings")).clicked() {
                    self.settings_open = true;
                }
            });

            ui.separator();
            let mut quit = false;
            let mut try_connect = false;
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        self.selected.is_some(),
                        egui::Button::new(t!("viewer-button-connect")),
                    )
                    .clicked()
                {
                    try_connect = true;
                }
                if ui.button(t!("viewer-button-quit")).clicked() {
                    quit = true;
                }
            });
            if try_connect {
                self.try_connect();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            if quit {
                *self.outcome.lock().unwrap() = Some(LaunchOutcome::Quit);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            if let Some(err) = &self.error {
                ui.colored_label(egui::Color32::RED, err);
            }
        });
    }
}

impl LauncherApp {
    pub(crate) fn try_connect(&mut self) {
        let Some(ref key) = self.selected.clone() else {
            return;
        };
        // Resolve the stable HostKey back to a live-config index so sort
        // reordering in hosts_list::render never causes mismatched connections.
        let idx = {
            let cfg = self.config.lock().unwrap_or_else(|p| p.into_inner());
            cfg.viewer
                .hosts
                .iter()
                .position(|h| HostKey::from_entry(h) == *key)
        };
        let Some(idx) = idx else { return };

        // Stamp last_connected at button-press time. The launcher closes
        // immediately after this call (window Close command), so there is no
        // async success callback available in this architecture. The actual
        // connection attempt happens in the caller of run_viewer_launcher.
        // TODO(P6 T8 follow-up): move stamp to actual connect-success site
        // once the viewer crate exposes a result channel.
        {
            let mut cfg = self.config.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(entry) = cfg.viewer.hosts.get_mut(idx) {
                entry.last_connected = std::time::SystemTime::now();
            }
            if let Err(e) = cfg.save(&self.config_path) {
                warn!(error = %e, path = ?self.config_path,
                    "failed to save config after last_connected update");
            }
        }
        let cfg = self.config.lock().unwrap_or_else(|p| p.into_inner());
        let Some(entry) = cfg.viewer.hosts.get(idx) else {
            return;
        };
        let viewer = &cfg.viewer;
        let mode = if entry.mode == "signaling" {
            crate::ConnectMode::Signaling
        } else {
            crate::ConnectMode::Direct
        };
        let direct_addr = if mode == crate::ConnectMode::Direct {
            entry.addr.parse().ok()
        } else {
            None
        };
        let signaling_url = if mode == crate::ConnectMode::Signaling {
            url::Url::parse(&viewer.signaling_url).ok()
        } else {
            None
        };
        let host_id = if mode == crate::ConnectMode::Signaling && !entry.host_id.is_empty() {
            Some(entry.host_id.clone())
        } else {
            None
        };
        let pubkey = if entry.pubkey.is_empty() {
            None
        } else {
            Some(entry.pubkey.clone())
        };
        let args = crate::ConnectArgs {
            mode,
            direct_addr,
            signaling_url,
            host_id,
            pubkey,
            decoder: viewer.decoder.clone(),
            recv_dir: viewer.recv_dir.clone(),
            known_hosts_path: viewer.known_hosts.clone(),
            known_host_ids_path: viewer.known_host_ids.clone(),
            default_resolution: viewer.default_resolution.clone(),
            default_fps: viewer.default_fps,
        };
        *self.outcome.lock().unwrap() = Some(crate::LaunchOutcome::Connect(Box::new(args)));
    }
}
