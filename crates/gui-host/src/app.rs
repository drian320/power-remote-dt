//! Host GUI state machine. Task 4 ships the Idle (key-loaded) screen.
//! Task 5 adds the Listening state + Settings modal.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use prdt_gui_common::{generate_qr, Config};

use crate::keygen;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stage {
    NeedsKey,
    Idle,
}

pub struct HostApp {
    config: Arc<Mutex<Config>>,
    config_path: PathBuf,
    stage: Stage,
    pubkey_b64: String,
    qr_handle: Option<egui::TextureHandle>,
    error: Option<String>,
}

impl HostApp {
    pub fn new(config: Arc<Mutex<Config>>, config_path: PathBuf) -> Self {
        let key_path = config.lock().unwrap().host.key_file.clone();
        let mut app = Self {
            config,
            config_path,
            stage: if key_path.exists() {
                Stage::Idle
            } else {
                Stage::NeedsKey
            },
            pubkey_b64: String::new(),
            qr_handle: None,
            error: None,
        };
        if app.stage == Stage::Idle {
            app.try_load_key(&key_path);
        }
        app
    }

    fn try_load_key(&mut self, path: &std::path::Path) {
        match keygen::try_load_or_generate(path) {
            Ok(out) => {
                self.pubkey_b64 = out.pubkey_b64;
                self.stage = Stage::Idle;
            }
            Err(e) => self.error = Some(format!("key load failed: {e}")),
        }
    }

    fn ensure_qr_texture(&mut self, ctx: &egui::Context) {
        if self.qr_handle.is_some() || self.pubkey_b64.is_empty() {
            return;
        }
        match generate_qr(&self.pubkey_b64, 4) {
            Ok(image) => {
                let handle =
                    ctx.load_texture("host_qr", image, egui::TextureOptions::default());
                self.qr_handle = Some(handle);
            }
            Err(e) => self.error = Some(format!("qr generation failed: {e}")),
        }
    }
}

impl eframe::App for HostApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| match self.stage {
            Stage::NeedsKey => self.show_needs_key(ui),
            Stage::Idle => {
                self.ensure_qr_texture(ctx);
                self.show_idle(ui);
            }
        });
    }
}

impl HostApp {
    fn show_needs_key(&mut self, ui: &mut egui::Ui) {
        ui.heading("Welcome");
        ui.add_space(12.0);
        ui.label("Generate a host key to start. The key uniquely identifies this machine to viewers.");
        ui.add_space(8.0);
        let key_path = self.config.lock().unwrap().host.key_file.clone();
        ui.label(format!("Key file: {}", key_path.display()));
        ui.add_space(20.0);
        if ui.button("Generate host key").clicked() {
            self.try_load_key(&key_path);
        }
        if let Some(err) = &self.error {
            ui.colored_label(egui::Color32::RED, err);
        }
    }

    fn show_idle(&mut self, ui: &mut egui::Ui) {
        ui.heading("Status: Idle");
        ui.add_space(8.0);
        ui.label("Public key:");
        ui.horizontal(|ui| {
            ui.code(&self.pubkey_b64);
            if ui.button("Copy").clicked() {
                ui.output_mut(|o| o.copied_text = self.pubkey_b64.clone());
            }
        });
        ui.add_space(12.0);
        if let Some(qr) = &self.qr_handle {
            ui.image(egui::load::SizedTexture::new(qr.id(), qr.size_vec2()));
        }
        ui.add_space(16.0);
        ui.label("[ Start listening ] (added in Task 5)");
        if let Some(err) = &self.error {
            ui.colored_label(egui::Color32::RED, err);
        }
        let _ = &self.config_path; // used by Task 5 settings modal
    }
}
