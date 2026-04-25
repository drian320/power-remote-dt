use crate::app::LauncherApp;

pub fn render(ui: &mut egui::Ui, app: &mut LauncherApp) {
    let cfg = app.config.lock().unwrap().clone();
    if cfg.viewer.hosts.is_empty() {
        ui.label("(no saved connections)");
    } else {
        for (i, h) in cfg.viewer.hosts.iter().enumerate() {
            let selected = app.selected == Some(i);
            let detail = if h.mode == "signaling" {
                h.host_id.as_str()
            } else {
                h.addr.as_str()
            };
            let label = format!("{} — {} ({})", h.label, detail, h.mode);
            if ui.selectable_label(selected, label).clicked() {
                app.selected = Some(i);
            }
        }
    }
    if ui.button("+ Add new connection").clicked() {
        app.add_form_open = true;
    }
}
