//! eframe app that renders the overlay window. Polls stats.json @ 5 Hz and
//! displays the parsed StatsPayload. Resume / Disconnect buttons.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use prdt_gui_common::t;

use crate::ipc::{self, StatsPayload};

pub struct OverlayApp {
    ipc_dir: PathBuf,
    last_poll: Instant,
    stats: Option<StatsPayload>,
    error: Option<String>,
}

impl OverlayApp {
    pub fn new(ipc_dir: PathBuf) -> Self {
        Self {
            ipc_dir,
            last_poll: Instant::now() - Duration::from_secs(60),
            stats: None,
            error: None,
        }
    }

    fn poll_if_due(&mut self) {
        if self.last_poll.elapsed() < Duration::from_millis(200) {
            return;
        }
        self.last_poll = Instant::now();
        match ipc::read_stats(&self.ipc_dir) {
            Ok(s) => {
                self.stats = Some(s);
                self.error = None;
            }
            Err(ipc::IpcError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                // Pre-first-write: keep showing whatever we had (likely
                // None → "Connecting…").
            }
            Err(e) => {
                self.error = Some(format!("read_stats: {e}"));
            }
        }
    }
}

impl eframe::App for OverlayApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(200));
        self.poll_if_due();

        let mut want_close = false;
        let mut want_disconnect = false;

        egui::CentralPanel::default().show(ctx, |ui| {
            match &self.stats {
                Some(s) if s.connection_state == "connected" && s.latency_us.is_some() => {
                    let l = s.latency_us.as_ref().unwrap();
                    let backend_label = s
                        .encoder_backend
                        .as_deref()
                        .unwrap_or(s.decoder.as_str());
                    ui.label(t!("overlay-host-label", host => s.host_label.as_str()));
                    ui.add_space(8.0);
                    ui.heading(t!("overlay-stats-latency"));
                    ui.label(format!("p50: {:.1} ms", l.p50 as f64 / 1000.0));
                    ui.label(format!("p95: {:.1} ms", l.p95 as f64 / 1000.0));
                    ui.label(format!("p99: {:.1} ms", l.p99 as f64 / 1000.0));
                    ui.label(t!("overlay-stats-samples", n => l.samples as i64));
                    ui.add_space(8.0);
                    ui.label(t!("overlay-stats-decoder", name => backend_label));
                    ui.label(format!("FPS: {:.1}", s.fps_observed));
                }
                Some(s) => {
                    let backend_label = s
                        .encoder_backend
                        .as_deref()
                        .unwrap_or(s.decoder.as_str());
                    ui.heading(t!("overlay-stats-connecting"));
                    ui.add_space(4.0);
                    ui.label(t!("overlay-host-label", host => s.host_label.as_str()));
                    ui.label(t!("overlay-stats-decoder", name => backend_label));
                }
                None => {
                    ui.heading(t!("overlay-stats-connecting"));
                }
            }

            if let Some(err) = &self.error {
                ui.colored_label(egui::Color32::RED, err);
            }

            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if ui.button(t!("overlay-button-resume")).clicked() {
                    want_close = true;
                }
                if ui.button(t!("overlay-button-disconnect")).clicked() {
                    want_disconnect = true;
                }
            });
        });

        if want_disconnect {
            if let Err(e) = ipc::write_disconnect(&self.ipc_dir) {
                tracing::warn!(?e, "write_disconnect failed");
                self.error = Some(format!("disconnect: {e}"));
            }
            want_close = true;
        }
        if want_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}
