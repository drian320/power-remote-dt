use std::path::PathBuf;

use prdt_gui_common::t;

use crate::app::LauncherApp;

pub fn render(ctx: &egui::Context, app: &mut LauncherApp) {
    let mut local = app.config.lock().unwrap().clone();
    let mut close = false;
    egui::Window::new(t!("viewer-settings-title"))
        .open(&mut app.settings_open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label(t!("viewer-decoder-label"));
            ui.horizontal(|ui| {
                ui.radio_value(&mut local.viewer.decoder, "mf".into(), t!("viewer-settings-decoder-mf"));
                ui.radio_value(&mut local.viewer.decoder, "nvdec".into(), t!("viewer-settings-decoder-nvdec"));
            });

            ui.label(t!("viewer-settings-resolution"));
            ui.text_edit_singleline(&mut local.viewer.default_resolution);

            ui.label(t!("viewer-settings-fps"));
            ui.add(egui::DragValue::new(&mut local.viewer.default_fps).range(15..=240));

            ui.label(t!("viewer-settings-recv-dir"));
            ui.horizontal(|ui| {
                let mut s = local.viewer.recv_dir.to_string_lossy().into_owned();
                if ui.text_edit_singleline(&mut s).changed() {
                    local.viewer.recv_dir = PathBuf::from(s);
                }
                if ui.button(t!("common-button-browse")).clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        local.viewer.recv_dir = p;
                    }
                }
            });

            ui.label(t!("viewer-settings-signaling-url"));
            ui.text_edit_singleline(&mut local.viewer.signaling_url);

            // Language row (G6).
            ui.separator();
            ui.label(t!("settings-language"));
            language_dropdown(ui, &mut local.gui.locale);

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button(t!("common-button-cancel")).clicked() {
                    close = true;
                }
                if ui.button(t!("common-button-save")).clicked() {
                    let new_locale = local.gui.locale.clone();
                    *app.config.lock().unwrap() = local.clone();
                    if let Err(e) = local.save(&app.config_path) {
                        app.error = Some(t!("viewer-error-config-save", error => e.to_string()));
                    }
                    apply_locale(&new_locale);
                    close = true;
                }
            });
        });
    if close {
        app.settings_open = false;
    }
}

fn language_dropdown(ui: &mut egui::Ui, current: &mut String) {
    let label = if current.is_empty() {
        t!("settings-language-auto")
    } else if current == "en" {
        t!("settings-language-english")
    } else if current == "ja" {
        t!("settings-language-japanese")
    } else {
        current.clone()
    };

    egui::ComboBox::from_id_source("settings-language-combo")
        .selected_text(label)
        .show_ui(ui, |ui| {
            ui.selectable_value(current, String::new(), t!("settings-language-auto"));
            ui.selectable_value(current, "en".to_string(), t!("settings-language-english"));
            ui.selectable_value(current, "ja".to_string(), t!("settings-language-japanese"));
        });
}

fn apply_locale(config_str: &str) {
    use prdt_gui_common::{set_locale, Locale};
    if let Some(l) = Locale::from_config_str(config_str) {
        set_locale(l);
    } else {
        set_locale(prdt_gui_common::detect_locale());
    }
}
