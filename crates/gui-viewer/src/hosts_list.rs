use prdt_gui_common::t;

use crate::app::LauncherApp;

pub fn render(ui: &mut egui::Ui, app: &mut LauncherApp) {
    let cfg = app.config.lock().unwrap().clone();
    if cfg.viewer.hosts.is_empty() {
        ui.label(t!("viewer-no-connections"));
    } else {
        for (i, h) in cfg.viewer.hosts.iter().enumerate() {
            let selected = app.selected == Some(i);
            let detail = if h.mode == "signaling" {
                h.host_id.clone()
            } else {
                h.addr.clone()
            };
            let label = t!(
                "viewer-host-entry",
                label => h.label.as_str(),
                detail => detail.as_str(),
                mode => h.mode.as_str(),
            );
            if ui.selectable_label(selected, label).clicked() {
                app.selected = Some(i);
            }
        }
    }
    if ui.button(t!("viewer-button-add")).clicked() {
        app.add_form_open = true;
    }
}
