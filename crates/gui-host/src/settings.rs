//! Host GUI settings modal. Edits the shared `Config`; saves to disk on Save.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use prdt_gui_common::Config;

pub fn render(
    ctx: &egui::Context,
    config: &Arc<Mutex<Config>>,
    config_path: &std::path::Path,
    open: &mut bool,
    error: &mut Option<String>,
) {
    let mut local: Config = config.lock().unwrap().clone();
    let mut close = false;
    egui::Window::new("Settings")
        .open(open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label("Bind:");
            ui.text_edit_singleline(&mut local.host.bind);
            ui.label("Monitor:");
            ui.add(egui::DragValue::new(&mut local.host.monitor));
            ui.label("Bitrate (Mbps):");
            ui.add(egui::DragValue::new(&mut local.host.bitrate_mbps).range(1..=200));
            ui.label("Outgoing dir:");
            ui.horizontal(|ui| {
                let mut s = local.host.outgoing_dir.to_string_lossy().into_owned();
                if ui.text_edit_singleline(&mut s).changed() {
                    local.host.outgoing_dir = PathBuf::from(s);
                }
                if ui.button("Browse").clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        local.host.outgoing_dir = p;
                    }
                }
            });
            ui.label("Signaling URL (optional):");
            ui.text_edit_singleline(&mut local.host.signaling_url);

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                if ui.button("Save").clicked() {
                    *config.lock().unwrap() = local.clone();
                    if let Err(e) = local.save(config_path) {
                        *error = Some(format!("config save failed: {e}"));
                    }
                    close = true;
                }
            });
        });
    if close {
        *open = false;
    }
}
