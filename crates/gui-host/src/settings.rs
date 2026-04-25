//! Host GUI settings modal. Edits the shared `Config`; saves to disk on Save.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::app::open_in_explorer;
use prdt_gui_common::t;
use prdt_gui_common::Config;

/// Phase 4 G4 — shared, mutable state for the Settings modal's update widgets.
/// Lives in `HostApp` and is passed by reference into `render()` so the
/// async check task can publish results.
#[derive(Default)]
pub struct UpdateUi {
    pub status: crate::update::CheckStatus,
    pub last_checked: Option<SystemTime>,
}

#[allow(clippy::too_many_arguments)]
pub fn render(
    ctx: &egui::Context,
    config: &Arc<Mutex<Config>>,
    config_path: &std::path::Path,
    open: &mut bool,
    error: &mut Option<String>,
    update_ui: &Arc<Mutex<UpdateUi>>,
    rt_handle: &tokio::runtime::Handle,
    pending_crashes: &mut Vec<prdt_gui_common::CrashReport>,
) {
    let mut local: Config = config.lock().unwrap().clone();
    let mut close = false;
    egui::Window::new(t!("settings-window-title"))
        .open(open)
        .resizable(false)
        .show(ctx, |ui| {
            // Phase 4 G4: Update banner (shown only when there's something
            // to say).
            {
                let ui_state = update_ui.lock().unwrap();
                match &ui_state.status {
                    crate::update::CheckStatus::Available { version, .. } => {
                        ui.colored_label(
                            egui::Color32::from_rgb(255, 220, 100),
                            t!("update-available", version => version.as_str()),
                        );
                        ui.separator();
                    }
                    crate::update::CheckStatus::UpToDate => {
                        ui.label(t!("update-up-to-date"));
                        ui.separator();
                    }
                    crate::update::CheckStatus::Checking => {
                        ui.label(t!("update-checking"));
                        ui.separator();
                    }
                    crate::update::CheckStatus::Error(msg) => {
                        ui.colored_label(
                            egui::Color32::RED,
                            t!("update-error", error => msg.as_str()),
                        );
                        ui.separator();
                    }
                    crate::update::CheckStatus::Idle => {}
                }
            }

            // Phase 4 G5: Pending crash reports from previous sessions.
            if !pending_crashes.is_empty() {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 220, 100),
                    t!(
                        "crashlog-pending-heading",
                        n => pending_crashes.len() as i64,
                    ),
                );
                for r in pending_crashes.iter().take(5) {
                    let summary = prdt_gui_common::truncate_for_display(&r.panic_message, 80);
                    ui.label(format!(
                        "{}  {}  \"{}\"",
                        r.timestamp_iso, r.binary, summary
                    ));
                }
                ui.horizontal(|ui| {
                    if ui.button(t!("crashlog-button-open-folder")).clicked() {
                        if let Some(dir) = prdt_gui_common::crashlog::crashes_dir() {
                            let _ = open_in_explorer(&dir);
                        }
                    }
                    if ui.button(t!("crashlog-button-acknowledge")).clicked() {
                        let snapshot = pending_crashes.clone();
                        for r in &snapshot {
                            if let Err(e) =
                                prdt_gui_common::mark_acknowledged(&r.timestamp_iso, &r.binary)
                            {
                                tracing::warn!(?e, "mark_acknowledged failed");
                            }
                        }
                        pending_crashes.clear();
                    }
                });
                ui.separator();
            }

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

            // Phase 4 G4: manual update check + install.
            ui.separator();
            ui.label(t!("update-section-heading"));
            {
                let ui_state = update_ui.lock().unwrap();
                if let Some(last) = ui_state.last_checked {
                    if let Ok(d) = SystemTime::now().duration_since(last) {
                        ui.label(format!("Last checked: {} hours ago", d.as_secs() / 3600));
                    }
                }
            }
            if ui.button(t!("update-button-check")).clicked() {
                {
                    let mut ui_state = update_ui.lock().unwrap();
                    ui_state.status = crate::update::CheckStatus::Checking;
                }
                let update_ui_clone = update_ui.clone();
                rt_handle.spawn(async move {
                    let status = crate::update::check_async().await;
                    let mut ui_state = update_ui_clone.lock().unwrap();
                    ui_state.status = status;
                    ui_state.last_checked = Some(SystemTime::now());
                });
            }
            // Install button only appears in Available state.
            {
                let ui_state = update_ui.lock().unwrap();
                if let crate::update::CheckStatus::Available { download_url, .. } = &ui_state.status
                {
                    let download_url = download_url.clone();
                    drop(ui_state);
                    if ui.button(t!("update-button-install")).clicked() {
                        let rt = rt_handle.clone();
                        rt_handle.spawn(async move {
                            if let Err(e) = crate::update::install_async(download_url).await {
                                tracing::error!(?e, "install_async failed");
                            } else {
                                tracing::info!("MSI install spawned; exiting");
                                let _ = rt;
                                std::process::exit(0);
                            }
                        });
                    }
                }
            }

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
