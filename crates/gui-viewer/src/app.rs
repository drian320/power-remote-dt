use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use prdt_gui_common::{t, Config};

use crate::LaunchOutcome;

pub struct LauncherApp {
    pub(crate) config: Arc<Mutex<Config>>,
    pub(crate) config_path: PathBuf,
    pub(crate) outcome: Arc<Mutex<Option<LaunchOutcome>>>,
    pub(crate) selected: Option<usize>,
    pub(crate) add_form_open: bool,
    pub(crate) settings_open: bool,
    pub(crate) error: Option<String>,
    pub(crate) draft_host: crate::connect_form::DraftHost,
}

impl LauncherApp {
    pub fn new(
        config: Arc<Mutex<Config>>,
        config_path: PathBuf,
        outcome: Arc<Mutex<Option<LaunchOutcome>>>,
    ) -> Self {
        Self {
            config,
            config_path,
            outcome,
            selected: None,
            add_form_open: false,
            settings_open: false,
            error: None,
            draft_host: crate::connect_form::DraftHost::default(),
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
                    .add_enabled(self.selected.is_some(), egui::Button::new(t!("viewer-button-connect")))
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
        let Some(idx) = self.selected else { return };
        let cfg = self.config.lock().unwrap();
        let Some(entry) = cfg.viewer.hosts.get(idx) else { return };
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
