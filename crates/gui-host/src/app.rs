//! Host GUI state machine. Stage transitions: NeedsKey → Idle → Listening.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use prdt_gui_common::auth_config::HostAuthConfig;
use prdt_gui_common::t;
use prdt_gui_common::{generate_qr, Config, TailHandle};
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

use crate::consent_channel::{ConsentDecision, ConsentReceiver};
use crate::keygen;
use crate::onboarding::{apply_wizard, WizardState};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stage {
    NeedsKey,
    Idle,
    Listening,
}

pub(crate) struct PendingPrompt {
    state: crate::consent_prompt::ConsentPromptState,
    responder: tokio::sync::oneshot::Sender<ConsentDecision>,
}

pub struct HostApp {
    config: Arc<Mutex<Config>>,
    config_path: PathBuf,
    stage: Stage,
    pubkey_b64: String,
    qr_handle: Option<egui::TextureHandle>,
    error: Option<String>,
    tail: TailHandle,
    rt_handle: Handle,
    cancel: Option<CancellationToken>,
    join: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
    run_host: crate::RunHostFn,
    settings_open: bool,
    tray: Option<crate::tray::TrayController>,
    notifier: crate::notif::Notifier,
    /// Cached state for tray icon updates (avoid re-setting the same icon
    /// every frame).
    last_tray_state: Option<crate::tray::HostState>,
    /// Sticky exit flag set by tray Quit menu. Checked in update().
    quit_requested: bool,
    update_ui: Arc<Mutex<crate::settings::UpdateUi>>,
    /// Phase 4 G5 — Crash reports from previous runs that the user has not
    /// yet acknowledged. Populated once at startup; mutated when the user
    /// clicks "Acknowledge all".
    pending_crashes: Vec<prdt_gui_common::CrashReport>,
    /// P6 T7: first-run onboarding wizard (shown when config.gui.onboarded == false).
    wizard: WizardState,
    /// P6 T7: host auth config (PIN hash, mode, default permissions).
    host_auth: HostAuthConfig,
    /// Path to host-auth.toml for persistence.
    host_auth_path: PathBuf,
    /// Receiver half of the consent channel; populated when listening starts.
    consent_rx: Option<ConsentReceiver>,
    /// In-flight consent prompt, if any.
    pending_consent: Option<PendingPrompt>,
}

impl HostApp {
    pub fn new(
        config: Arc<Mutex<Config>>,
        config_path: PathBuf,
        tail: TailHandle,
        rt_handle: Handle,
        run_host: crate::RunHostFn,
        tray: Option<crate::tray::TrayController>,
        pending_crashes: Vec<prdt_gui_common::CrashReport>,
    ) -> Self {
        let key_path = config.lock().unwrap().host.key_file.clone();

        // Derive host-auth.toml path from the config directory.
        let host_auth_path = config_path
            .parent()
            .map(|p| p.join("host-auth.toml"))
            .unwrap_or_else(|| PathBuf::from("host-auth.toml"));

        let host_auth = HostAuthConfig::load_or_default(&host_auth_path).unwrap_or_default();

        let mut app = Self {
            config,
            config_path,
            stage: if key_path.exists() {
                Stage::Idle
            } else {
                Stage::NeedsKey
            },
            pubkey_b64: String::new(),
            qr_handle: None,
            error: None,
            tail,
            rt_handle,
            cancel: None,
            join: None,
            run_host,
            settings_open: false,
            tray,
            notifier: crate::notif::Notifier::new(),
            last_tray_state: None,
            quit_requested: false,
            update_ui: Arc::new(Mutex::new(crate::settings::UpdateUi::default())),
            pending_crashes,
            wizard: WizardState::new(),
            host_auth,
            host_auth_path,
            consent_rx: None,
            pending_consent: None,
        };
        if app.stage == Stage::Idle {
            app.try_load_key(&key_path);
        }
        app
    }

    fn try_load_key(&mut self, path: &std::path::Path) {
        match keygen::try_load_or_generate(path) {
            Ok(out) => {
                self.pubkey_b64 = out.pubkey_b64;
                self.stage = Stage::Idle;
            }
            Err(e) => self.error = Some(t!("host-error-key-load", error => e.to_string())),
        }
    }

    fn ensure_qr_texture(&mut self, ctx: &egui::Context) {
        if self.qr_handle.is_some() || self.pubkey_b64.is_empty() {
            return;
        }
        match generate_qr(&self.pubkey_b64, 4) {
            Ok(image) => {
                let handle = ctx.load_texture("host_qr", image, egui::TextureOptions::default());
                self.qr_handle = Some(handle);
            }
            Err(e) => self.error = Some(t!("host-error-qr", error => e.to_string())),
        }
    }

    fn start_listening(&mut self) {
        let cancel = CancellationToken::new();
        let (consent_tx, consent_rx) = tokio::sync::mpsc::unbounded_channel();
        let _enter = self.rt_handle.enter();
        let join = (self.run_host)(cancel.clone(), consent_tx);
        self.cancel = Some(cancel);
        self.join = Some(join);
        self.consent_rx = Some(consent_rx);
        self.stage = Stage::Listening;
    }

    fn stop_listening(&mut self) {
        if let Some(c) = self.cancel.take() {
            c.cancel();
        }
        // Don't block on join here — let it drop / clean up async.
        self.join = None;
        self.consent_rx = None;
        if let Some(p) = self.pending_consent.take() {
            // sender side will treat the dropped responder as rejection
            let _ = p.responder.send(ConsentDecision::Rejected);
        }
        self.stage = Stage::Idle;
    }

    /// Poll for new consent requests from the host network task.
    ///
    /// Drains exactly one pending request into `pending_consent` (only if
    /// `pending_consent` is currently `None`). On channel disconnect, clears
    /// `consent_rx`.
    pub(crate) fn poll_consent_requests(&mut self) {
        poll_consent_requests_impl(
            &mut self.consent_rx,
            &mut self.pending_consent,
            &self.host_auth,
        );
    }

    fn current_tray_state(&self) -> crate::tray::HostState {
        if self.error.is_some() {
            crate::tray::HostState::Error
        } else {
            match self.stage {
                Stage::Listening => crate::tray::HostState::Listening,
                _ => crate::tray::HostState::Idle,
            }
        }
    }

    fn dispatch_tray_action(&mut self, action: crate::tray::TrayAction) {
        match action {
            crate::tray::TrayAction::OpenSettings => {
                self.settings_open = true;
            }
            crate::tray::TrayAction::StopListening => {
                if self.stage == Stage::Listening {
                    self.stop_listening();
                }
            }
            crate::tray::TrayAction::ShowLogs => {
                if let Some(root) = prdt_gui_common::config_root() {
                    let _ = open_in_explorer(&root);
                }
            }
            crate::tray::TrayAction::Quit => {
                self.quit_requested = true;
            }
        }
    }

    fn check_log_for_notifications(&mut self) {
        let lines = self.tail.snapshot();
        if let Some(last) = lines.last() {
            if last.contains("viewer connected from") {
                self.notifier.fire(crate::notif::NotifKind::Connected, last);
            } else if last.contains("viewer disconnected") {
                self.notifier
                    .fire(crate::notif::NotifKind::Disconnected, last);
            } else if last.contains("ERROR")
                || last.contains("encoder failed")
                || last.contains("DXGI_ERROR")
            {
                self.notifier.fire(crate::notif::NotifKind::Error, last);
            }
        }
    }
}

/// Free function implementing the consent-poll logic so it can be tested
/// without constructing a full `HostApp`.
pub(crate) fn poll_consent_requests_impl(
    rx: &mut Option<ConsentReceiver>,
    pending: &mut Option<PendingPrompt>,
    host_auth: &HostAuthConfig,
) {
    if pending.is_some() {
        return;
    }
    let Some(rx_ref) = rx.as_mut() else { return };
    use tokio::sync::mpsc::error::TryRecvError;
    match rx_ref.try_recv() {
        Ok(req) => {
            use std::time::Duration;
            let state = crate::consent_prompt::ConsentPromptState::new(
                req.peer_pubkey.to_base64(),
                host_auth.default_permissions,
                Duration::from_secs(u64::from(host_auth.consent_timeout_seconds)),
            );
            *pending = Some(PendingPrompt {
                state,
                responder: req.responder,
            });
        }
        Err(TryRecvError::Empty) => {}
        Err(TryRecvError::Disconnected) => {
            *rx = None;
        }
    }
}

pub(crate) fn open_in_explorer(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::process::Command::new("explorer")
            .arg(path)
            .spawn()
            .map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map(|_| ())
    }
}

impl eframe::App for HostApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(std::time::Duration::from_millis(250));

        // Tray: update icon when state changes; drain menu events.
        if let Some(tray) = &self.tray {
            let s = self.current_tray_state();
            if Some(s) != self.last_tray_state {
                tray.set_state(s);
                self.last_tray_state = Some(s);
            }
            if let Some(action) = tray.poll_menu() {
                self.dispatch_tray_action(action);
            }
        }

        // Hide-to-tray: intercept the user pressing the window 'x' button.
        // Only hide if a tray icon is alive (otherwise the window is the
        // only way back to the app).
        if self.tray.is_some() && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        // True quit (from tray): close the viewport for real.
        if self.quit_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // P6 T7: first-run onboarding wizard blocks all other UI.
        let onboarded = self.config.lock().unwrap().gui.onboarded;
        if !onboarded {
            // Read host_id from the host_id_file for display in the wizard.
            let host_id = {
                let cfg = self.config.lock().unwrap();
                std::fs::read_to_string(&cfg.host.host_id_file)
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            };
            let host_id = if host_id.is_empty() {
                self.pubkey_b64.chars().take(16).collect::<String>()
            } else {
                host_id
            };
            if let Some(submission) = self.wizard.show(ctx, &host_id) {
                let mut config = self.config.lock().unwrap();
                match apply_wizard(submission, &mut config, &mut self.host_auth) {
                    Ok(()) => {
                        if let Err(e) = config.save(&self.config_path) {
                            self.wizard.error = Some(format!("Failed to save config: {e}"));
                        } else if let Err(e) = self.host_auth.save(&self.host_auth_path) {
                            self.wizard.error = Some(format!("Failed to save auth config: {e}"));
                        }
                    }
                    Err(e) => {
                        self.wizard.error = Some(e.to_string());
                    }
                }
            }
            return; // block normal UI while wizard is active
        }

        // Consent prompt: poll for new requests and render any in-flight prompt.
        // Placed after the wizard return (wizard takes priority) and before
        // the Settings panel so the prompt overlays Settings if both are active.
        self.poll_consent_requests();
        if let Some(p) = self.pending_consent.as_mut() {
            if let Some(decision) = p.state.show(ctx) {
                if let Some(p) = self.pending_consent.take() {
                    let _ = p.responder.send(decision);
                }
            }
        }

        if self.settings_open {
            crate::settings::render(
                ctx,
                &self.config,
                &self.config_path,
                &mut self.settings_open,
                &mut self.error,
                &self.update_ui,
                &self.rt_handle,
                &mut self.pending_crashes,
            );
        }

        egui::CentralPanel::default().show(ctx, |ui| match self.stage {
            Stage::NeedsKey => self.show_needs_key(ui),
            Stage::Idle => {
                self.ensure_qr_texture(ctx);
                self.show_idle(ui);
            }
            Stage::Listening => {
                self.ensure_qr_texture(ctx);
                self.show_listening(ui);
            }
        });
    }
}

impl HostApp {
    fn show_needs_key(&mut self, ui: &mut egui::Ui) {
        ui.heading(t!("host-welcome-heading"));
        ui.add_space(12.0);
        ui.label(t!("host-welcome-body"));
        ui.add_space(8.0);
        let key_path = self.config.lock().unwrap().host.key_file.clone();
        ui.label(t!("host-key-file-label", path => key_path.display().to_string()));
        ui.add_space(20.0);
        if ui.button(t!("host-button-generate-key")).clicked() {
            self.try_load_key(&key_path);
        }
        if let Some(err) = &self.error {
            ui.colored_label(egui::Color32::RED, err);
        }
    }

    fn show_idle(&mut self, ui: &mut egui::Ui) {
        ui.heading(t!("host-status-idle"));
        ui.add_space(8.0);
        self.draw_pubkey_with_qr(ui);
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if ui.button(t!("host-button-start-listening")).clicked() {
                self.start_listening();
            }
            if ui.button(t!("host-button-settings")).clicked() {
                self.settings_open = true;
            }
        });
        if let Some(err) = &self.error {
            ui.colored_label(egui::Color32::RED, err);
        }
    }

    fn show_listening(&mut self, ui: &mut egui::Ui) {
        self.check_log_for_notifications();
        let bind = self.config.lock().unwrap().host.bind.clone();
        ui.heading(t!("host-status-listening", bind => bind.as_str()));
        ui.add_space(8.0);
        self.draw_pubkey_with_qr(ui);
        ui.add_space(12.0);
        ui.label(t!("host-recent-activity"));
        let lines = self.tail.snapshot();
        egui::ScrollArea::vertical()
            .max_height(160.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for l in &lines {
                    ui.label(l);
                }
            });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(t!("host-button-stop")).clicked() {
                self.stop_listening();
            }
            if ui.button(t!("host-button-settings")).clicked() {
                self.settings_open = true;
            }
        });
    }

    fn draw_pubkey_with_qr(&mut self, ui: &mut egui::Ui) {
        ui.label(t!("host-pubkey-label"));
        ui.horizontal(|ui| {
            ui.code(&self.pubkey_b64);
            if ui.button(t!("common-button-copy")).clicked() {
                ui.ctx().copy_text(self.pubkey_b64.clone());
            }
        });
        if let Some(qr) = &self.qr_handle {
            ui.add_space(8.0);
            ui.image(egui::load::SizedTexture::new(qr.id(), qr.size_vec2()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent_channel::{ConsentDecision, ConsentRequest};
    use prdt_gui_common::auth_config::HostAuthConfig;

    #[test]
    fn poll_consent_requests_picks_up_request() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let _guard = rt.enter();

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<ConsentRequest>();
        let mut consent_rx: Option<ConsentReceiver> = Some(rx);
        let mut pending: Option<PendingPrompt> = None;
        let host_auth = HostAuthConfig::default();

        // Send a consent request with a dummy pubkey.
        let peer_key = prdt_crypto::PubKey([0u8; 32]);
        let (resp_tx, _resp_rx) = tokio::sync::oneshot::channel::<ConsentDecision>();
        tx.send(ConsentRequest {
            peer_pubkey: peer_key,
            responder: resp_tx,
        })
        .unwrap();

        poll_consent_requests_impl(&mut consent_rx, &mut pending, &host_auth);

        assert!(pending.is_some(), "pending_consent should be populated");
        assert!(consent_rx.is_some(), "rx should still be open");
    }

    #[test]
    fn poll_consent_requests_clears_rx_on_disconnect() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let _guard = rt.enter();

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<ConsentRequest>();
        let mut consent_rx: Option<ConsentReceiver> = Some(rx);
        let mut pending: Option<PendingPrompt> = None;
        let host_auth = HostAuthConfig::default();

        // Drop the sender so the channel is disconnected.
        drop(tx);

        poll_consent_requests_impl(&mut consent_rx, &mut pending, &host_auth);

        assert!(consent_rx.is_none(), "rx should be cleared on disconnect");
        assert!(pending.is_none(), "no pending prompt expected");
    }
}
