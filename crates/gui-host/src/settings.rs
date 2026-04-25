//! Host GUI settings modal. Edits the shared `Config`; saves to disk on Save.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use prdt_gui_common::t;
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
    egui::Window::new(t!("settings-window-title"))
        .open(open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label(t!("host-settings-bind"));
            ui.text_edit_singleline(&mut local.host.bind);
            ui.label(t!("host-settings-monitor"));
            ui.add(egui::DragValue::new(&mut local.host.monitor));
            ui.label(t!("host-settings-bitrate"));
            ui.add(egui::DragValue::new(&mut local.host.bitrate_mbps).range(1..=200));
            ui.label(t!("host-settings-outgoing"));
            ui.horizontal(|ui| {
                let mut s = local.host.outgoing_dir.to_string_lossy().into_owned();
                if ui.text_edit_singleline(&mut s).changed() {
                    local.host.outgoing_dir = PathBuf::from(s);
                }
                if ui.button(t!("common-button-browse")).clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        local.host.outgoing_dir = p;
                    }
                }
            });
            ui.label(t!("host-settings-signaling-optional"));
            ui.text_edit_singleline(&mut local.host.signaling_url);

            // Phase 4 G3: Auto-start on login.
            ui.separator();
            ui.checkbox(&mut local.host.auto_start, t!("settings-autostart-label"));

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
                    let auto_start = local.host.auto_start;
                    *config.lock().unwrap() = local.clone();
                    if let Err(e) = local.save(config_path) {
                        *error = Some(t!("host-error-config-save", error => e.to_string()));
                    }
                    if let Err(e) = crate::autostart::set_enabled(auto_start) {
                        *error = Some(t!("host-error-autostart", error => e.to_string()));
                    }
                    apply_locale(&new_locale);
                    close = true;
                }
            });
        });
    if close {
        *open = false;
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
