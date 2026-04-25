use prdt_gui_common::{t, HostEntry};

use crate::app::LauncherApp;

pub struct DraftHost {
    pub label: String,
    pub mode: String, // "direct" | "signaling"
    pub addr: String,
    pub host_id: String,
    pub pubkey: String,
}

impl Default for DraftHost {
    fn default() -> Self {
        Self {
            label: String::new(),
            mode: "direct".into(),
            addr: String::new(),
            host_id: String::new(),
            pubkey: String::new(),
        }
    }
}

pub fn render(ctx: &egui::Context, app: &mut LauncherApp) {
    let mut close = false;
    let mut save = false;
    egui::Window::new(t!("viewer-form-title"))
        .open(&mut app.add_form_open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label(t!("viewer-form-label"));
            ui.text_edit_singleline(&mut app.draft_host.label);
            ui.add_space(4.0);
            ui.label(t!("viewer-form-mode"));
            ui.horizontal(|ui| {
                ui.radio_value(&mut app.draft_host.mode, "direct".into(), t!("viewer-form-mode-direct"));
                ui.radio_value(&mut app.draft_host.mode, "signaling".into(), t!("viewer-form-mode-signaling"));
            });
            if app.draft_host.mode == "direct" {
                ui.label(t!("viewer-form-addr"));
                ui.text_edit_singleline(&mut app.draft_host.addr);
            } else {
                ui.label(t!("viewer-form-host-id"));
                ui.text_edit_singleline(&mut app.draft_host.host_id);
            }
            ui.label(t!("viewer-form-pubkey"));
            ui.text_edit_singleline(&mut app.draft_host.pubkey);

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button(t!("common-button-cancel")).clicked() {
                    close = true;
                }
                let valid = !app.draft_host.label.is_empty()
                    && (app.draft_host.mode == "direct" && !app.draft_host.addr.is_empty()
                        || app.draft_host.mode == "signaling"
                            && !app.draft_host.host_id.is_empty());
                if ui
                    .add_enabled(valid, egui::Button::new(t!("common-button-save")))
                    .clicked()
                {
                    save = true;
                }
            });
        });

    if save {
        let entry = HostEntry {
            label: app.draft_host.label.clone(),
            mode: app.draft_host.mode.clone(),
            addr: app.draft_host.addr.clone(),
            host_id: app.draft_host.host_id.clone(),
            pubkey: app.draft_host.pubkey.clone(),
            last_connected: String::new(),
        };
        let mut cfg = app.config.lock().unwrap();
        cfg.viewer.hosts.push(entry);
        if let Err(e) = cfg.save(&app.config_path) {
            app.error = Some(t!("viewer-error-config-save", error => e.to_string()));
        }
        drop(cfg);
        app.draft_host = DraftHost::default();
        close = true;
    }
    if close {
        app.add_form_open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_default_starts_in_direct_mode() {
        let d = DraftHost::default();
        assert_eq!(d.mode, "direct");
        assert!(d.label.is_empty());
        assert!(d.addr.is_empty());
        assert!(d.host_id.is_empty());
    }
}
