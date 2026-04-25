use std::path::PathBuf;

use crate::app::LauncherApp;

pub fn render(ctx: &egui::Context, app: &mut LauncherApp) {
    let mut local = app.config.lock().unwrap().clone();
    let mut close = false;
    egui::Window::new("Viewer Settings")
        .open(&mut app.settings_open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label("Decoder:");
            ui.horizontal(|ui| {
                ui.radio_value(&mut local.viewer.decoder, "mf".into(), "MF (default)");
                ui.radio_value(&mut local.viewer.decoder, "nvdec".into(), "NVDEC (zero-copy)");
            });

            ui.label("Default resolution:");
            ui.text_edit_singleline(&mut local.viewer.default_resolution);

            ui.label("Default fps:");
            ui.add(egui::DragValue::new(&mut local.viewer.default_fps).range(15..=240));

            ui.label("Receive directory:");
            ui.horizontal(|ui| {
                let mut s = local.viewer.recv_dir.to_string_lossy().into_owned();
                if ui.text_edit_singleline(&mut s).changed() {
                    local.viewer.recv_dir = PathBuf::from(s);
                }
                if ui.button("Browse").clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        local.viewer.recv_dir = p;
                    }
                }
            });

            ui.label("Signaling URL:");
            ui.text_edit_singleline(&mut local.viewer.signaling_url);

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                if ui.button("Save").clicked() {
                    *app.config.lock().unwrap() = local.clone();
                    if let Err(e) = local.save(&app.config_path) {
                        app.error = Some(format!("config save failed: {e}"));
                    }
                    close = true;
                }
            });
        });
    if close {
        app.settings_open = false;
    }
}
