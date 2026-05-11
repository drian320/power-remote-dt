use std::cmp::Reverse;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use prdt_gui_common::t;

use crate::app::LauncherApp;

/// Format a `SystemTime` as a human-readable relative string.
/// Returns `"never"` for `UNIX_EPOCH`, `"just now"` for < 60 s ago,
/// otherwise minutes / hours / days.
pub fn format_relative(t: SystemTime) -> String {
    if t == UNIX_EPOCH {
        return "never".into();
    }
    let elapsed = SystemTime::now()
        .duration_since(t)
        .unwrap_or(Duration::ZERO);
    let s = elapsed.as_secs();
    if s < 60 {
        "just now".into()
    } else if s < 3600 {
        format!("{} min ago", s / 60)
    } else if s < 86400 {
        format!("{} hours ago", s / 3600)
    } else if s < 86400 * 30 {
        format!("{} days ago", s / 86400)
    } else {
        "long ago".into()
    }
}

pub fn render(ui: &mut egui::Ui, app: &mut LauncherApp) {
    // Snapshot config and online state under the lock; release before UI work.
    let mut cfg = app.config.lock().unwrap().clone();
    let online = app.online_sink.lock().unwrap().clone();

    // Sort: online-first, then last_connected DESC (most recent first).
    cfg.viewer.hosts.sort_by_key(|e| {
        let is_online = online
            .get(&e.addr)
            .or_else(|| online.get(&e.host_id))
            .copied()
            .unwrap_or(e.last_known_online.unwrap_or(false));
        (Reverse(is_online), Reverse(e.last_connected))
    });

    if cfg.viewer.hosts.is_empty() {
        ui.label(t!("viewer-no-connections"));
    } else {
        for (orig_i, h) in cfg.viewer.hosts.iter().enumerate() {
            // Find the original index in the unsorted config for selection tracking.
            let selected = app.selected == Some(orig_i);
            let is_online = online
                .get(&h.addr)
                .or_else(|| online.get(&h.host_id))
                .copied()
                .unwrap_or(h.last_known_online.unwrap_or(false));
            let badge = if is_online { "🟢" } else { "⚪" };
            let detail = if h.mode == "signaling" {
                h.host_id.clone()
            } else {
                h.addr.clone()
            };
            let row_text = format!(
                "{badge} {} · {} · {}",
                h.label,
                format_relative(h.last_connected),
                detail
            );
            if ui.selectable_label(selected, row_text).clicked() {
                app.selected = Some(orig_i);
            }
        }
    }
    if ui.button(t!("viewer-button-add")).clicked() {
        app.add_form_open = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prdt_gui_common::HostEntry;
    use std::collections::HashMap;

    #[test]
    fn relative_time_buckets() {
        let now = SystemTime::now();
        assert_eq!(format_relative(now), "just now");
        assert_eq!(format_relative(now - Duration::from_secs(120)), "2 min ago");
        assert_eq!(
            format_relative(now - Duration::from_secs(7200)),
            "2 hours ago"
        );
        assert_eq!(
            format_relative(now - Duration::from_secs(86400 * 3)),
            "3 days ago"
        );
    }

    #[test]
    fn relative_time_epoch_shows_never() {
        assert_eq!(format_relative(UNIX_EPOCH), "never");
    }

    #[test]
    fn relative_time_long_ago_bucket() {
        let now = SystemTime::now();
        assert_eq!(
            format_relative(now - Duration::from_secs(86400 * 31)),
            "long ago"
        );
    }

    #[test]
    fn sort_online_first_then_recency() {
        let now = SystemTime::now();
        let older = now - Duration::from_secs(3600);
        let mut hosts: Vec<HostEntry> = [
            HostEntry {
                label: "offline-recent".into(),
                mode: "direct".into(),
                addr: "1.1.1.1:9000".into(),
                host_id: String::new(),
                pubkey: String::new(),
                last_connected: now,
                last_known_online: Some(false),
            },
            HostEntry {
                label: "online-old".into(),
                mode: "direct".into(),
                addr: "2.2.2.2:9000".into(),
                host_id: String::new(),
                pubkey: String::new(),
                last_connected: older,
                last_known_online: Some(true),
            },
            HostEntry {
                label: "offline-old".into(),
                mode: "direct".into(),
                addr: "3.3.3.3:9000".into(),
                host_id: String::new(),
                pubkey: String::new(),
                last_connected: older,
                last_known_online: Some(false),
            },
        ]
        .into();

        let online: HashMap<String, bool> = HashMap::new();
        hosts.sort_by_key(|e| {
            let is_online = online
                .get(&e.addr)
                .or_else(|| online.get(&e.host_id))
                .copied()
                .unwrap_or(e.last_known_online.unwrap_or(false));
            (Reverse(is_online), Reverse(e.last_connected))
        });

        assert_eq!(hosts[0].label, "online-old");
        assert_eq!(hosts[1].label, "offline-recent");
        assert_eq!(hosts[2].label, "offline-old");
    }
}
