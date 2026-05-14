//! Unified client app: "This Device" + "Connect" tabs in one egui window.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clap::Parser as _;
use prdt_gui_common::auth_config::HostAuthConfig;
use prdt_gui_common::Config;
use tokio::runtime;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    ThisDevice,
    Connect,
}

struct PendingPrompt {
    state: prdt_gui_host::consent_prompt::ConsentPromptState,
    responder: tokio::sync::oneshot::Sender<prdt_gui_host::consent_channel::ConsentDecision>,
}

pub struct ClientApp {
    cfg: Arc<Mutex<Config>>,
    #[allow(dead_code)] // wired in PR3.5+ when the GUI persists user edits
    config_path: PathBuf,
    rt_handle: runtime::Handle,

    tab: Tab,

    // This Device
    pubkey_b64: Option<String>,
    pubkey_load_error: Option<String>,
    listener: Option<ListenerState>,
    /// Last status line shown under the listener controls (start/stop result,
    /// or the error returned by the host task when it exits).
    host_status: Option<String>,

    // Connect
    peer_host: String,
    peer_pubkey: String,
    peer_codec: String,
    peer_decoder: String,
    /// Last status line shown under the Connect button.
    connect_status: Option<String>,

    /// Receiver for incoming consent requests from the host listener task.
    /// Polled in `update()` each frame; first request becomes `pending_consent`.
    consent_rx: Option<tokio::sync::mpsc::UnboundedReceiver<prdt_host::ConsentRequest>>,
    /// In-flight consent prompt state machine + responder oneshot. Dropping
    /// the `PendingPrompt` without sending is treated as Reject by the host.
    pending_consent: Option<PendingPrompt>,
    /// Host-auth config (default permissions, consent timeout). Loaded from
    /// host-auth.toml at startup; used to initialise each consent prompt.
    host_auth: HostAuthConfig,
}

struct ListenerState {
    cancel: CancellationToken,
    /// Wrapper task; `()` because the actual `Result<()>` is forwarded via the
    /// oneshot channel. Kept so we can poll `is_finished()` for the running
    /// indicator and to ensure we observe the wrapper's completion.
    join: tokio::task::JoinHandle<()>,
    result_rx: Option<oneshot::Receiver<anyhow::Result<()>>>,
}

impl ClientApp {
    pub fn new(cfg: Arc<Mutex<Config>>, config_path: PathBuf, rt_handle: runtime::Handle) -> Self {
        // Derive host-auth.toml path from the config directory (mirrors gui-host).
        let host_auth_path = config_path
            .parent()
            .map(|p| p.join("host-auth.toml"))
            .unwrap_or_else(|| PathBuf::from("host-auth.toml"));
        let host_auth = HostAuthConfig::load_or_default(&host_auth_path).unwrap_or_default();

        let (peer_codec, peer_decoder) = {
            let cfg_guard = cfg.lock().unwrap();
            (cfg_guard.viewer.codec.clone(), cfg_guard.viewer.decoder.clone())
        };
        let mut app = Self {
            cfg,
            config_path,
            rt_handle,
            tab: Tab::ThisDevice,
            pubkey_b64: None,
            pubkey_load_error: None,
            listener: None,
            host_status: None,
            peer_host: "127.0.0.1:9000".to_string(),
            peer_pubkey: String::new(),
            peer_codec,
            peer_decoder,
            connect_status: None,
            consent_rx: None,
            pending_consent: None,
            host_auth,
        };
        app.refresh_pubkey();
        app
    }

    /// Read the host key file (OS-conventional default path) and derive the pubkey for display.
    /// On miss the pubkey is None until the host listener generates one.
    fn refresh_pubkey(&mut self) {
        let resolved = prdt_host::default_host_key_path();
        let path = resolved.as_path();
        if !path.exists() {
            self.pubkey_b64 = None;
            self.pubkey_load_error = None;
            return;
        }
        match std::fs::read(path) {
            Ok(bytes) if bytes.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                let kp = prdt_crypto::KeyPair::from_private(arr);
                self.pubkey_b64 = Some(kp.public.to_base64());
                self.pubkey_load_error = None;
            }
            Ok(other) => {
                self.pubkey_b64 = None;
                self.pubkey_load_error = Some(format!(
                    "host-key.bin is {} bytes, expected 32",
                    other.len()
                ));
            }
            Err(e) => {
                self.pubkey_b64 = None;
                self.pubkey_load_error = Some(format!("read host-key.bin: {e}"));
            }
        }
    }

    fn is_listening(&self) -> bool {
        self.listener
            .as_ref()
            .is_some_and(|s| !s.join.is_finished())
    }

    /// Pre-flight UDP bind check. Returns Err with a friendly message when the
    /// port is already in use. Best-effort: there's a race between this drop
    /// and `run_host`'s real bind, but in practice it catches the common case
    /// of "I forgot to stop the previous host" cleanly.
    fn pre_flight_bind(addr_str: &str) -> Result<(), String> {
        let addr: std::net::SocketAddr = addr_str
            .parse()
            .map_err(|e| format!("invalid bind address {addr_str:?}: {e}"))?;
        match std::net::UdpSocket::bind(addr) {
            Ok(_socket) => Ok(()),
            Err(e) => Err(format!("port {} unavailable: {e}", addr.port())),
        }
    }

    fn start_listener(&mut self) {
        if self.is_listening() {
            return;
        }
        let cfg = self.cfg.lock().unwrap().clone();

        // Pre-flight: surface "port in use" before we even spawn the task.
        if let Err(msg) = Self::pre_flight_bind(&cfg.host.bind) {
            self.host_status = Some(format!("cannot start: {msg}"));
            return;
        }

        // Build prdt-host Args from defaults + GUI config. Construct via
        // parse_from so all clap defaults apply, then override known fields.
        let mut argv: Vec<std::ffi::OsString> = vec!["prdt-host".into()];
        argv.push("--bind".into());
        argv.push(cfg.host.bind.clone().into());
        argv.push("--monitor".into());
        argv.push(cfg.host.monitor.to_string().into());
        argv.push("--bitrate-mbps".into());
        argv.push(cfg.host.bitrate_mbps.to_string().into());
        argv.push("--key-file".into());
        argv.push(cfg.host.key_file.as_os_str().to_owned());
        argv.push("--encoder".into());
        argv.push(cfg.host.encoder.clone().into());
        argv.push("--headless".into()); // GUI manages the lifecycle; don't relaunch gui-host

        let args = match prdt_host::Args::try_parse_from(&argv) {
            Ok(a) => a,
            Err(e) => {
                self.host_status = Some(format!("invalid host args: {e}"));
                return;
            }
        };

        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let (result_tx, result_rx) = oneshot::channel();
        let (consent_tx, consent_rx) = tokio::sync::mpsc::unbounded_channel();
        let join = self.rt_handle.spawn(async move {
            let res = prdt_host::run_host(args, None, Some(consent_tx), cancel_for_task).await;
            // Receiver may be gone if the GUI has already moved on; ignore.
            let _ = result_tx.send(res);
        });
        self.consent_rx = Some(consent_rx);
        self.listener = Some(ListenerState {
            cancel,
            join,
            result_rx: Some(result_rx),
        });
        self.host_status = Some("starting listener\u{2026}".into());
    }

    fn stop_listener(&mut self) {
        if let Some(state) = self.listener.take() {
            state.cancel.cancel();
            // Detach: let the wrapper task wind down on the runtime. We don't
            // need the result here -- Stop is user-initiated.
            drop(self.rt_handle.spawn(async move {
                let _ = state.join.await;
            }));
            self.host_status = Some("listener stopped".into());
        }
        // Drop the consent channel first so no new requests arrive.
        self.consent_rx = None;
        // Explicitly reject any in-flight prompt so the host task unblocks
        // immediately rather than waiting for the oneshot to drop.
        if let Some(p) = self.pending_consent.take() {
            let _ = p
                .responder
                .send(prdt_gui_host::consent_channel::ConsentDecision::Rejected);
        }
    }

    /// Drain the listener result without awaiting. Called every frame; once
    /// the task finishes (gracefully or with error), we capture the message
    /// and clear `self.listener` so the UI flips back to "Idle".
    fn drain_listener_result(&mut self) {
        let Some(state) = self.listener.as_mut() else {
            return;
        };
        if !state.join.is_finished() {
            return;
        }
        // The wrapper task is finished; it should have already sent on the
        // oneshot. try_recv reflects that without blocking.
        let msg = match state.result_rx.take() {
            Some(mut rx) => match rx.try_recv() {
                Ok(Ok(())) => "listener exited cleanly".to_string(),
                Ok(Err(e)) => format!("listener error: {e:#}"),
                Err(oneshot::error::TryRecvError::Empty) => {
                    // Shouldn't happen since join is finished. Be defensive.
                    "listener task ended (no result)".to_string()
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    "listener task ended (sender dropped)".to_string()
                }
            },
            None => "listener task ended".to_string(),
        };
        self.host_status = Some(msg);
        self.listener = None;
    }

    fn spawn_connect(&mut self) {
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                self.connect_status = Some(format!("current_exe: {e}"));
                return;
            }
        };
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("connect").arg("--headless");
        if !self.peer_host.trim().is_empty() {
            cmd.arg("--host").arg(self.peer_host.trim());
        }
        if !self.peer_pubkey.trim().is_empty() {
            cmd.arg("--host-pubkey").arg(self.peer_pubkey.trim());
        }
        if !self.peer_codec.trim().is_empty() {
            cmd.arg("--codec").arg(self.peer_codec.trim());
        }
        if !self.peer_decoder.trim().is_empty() {
            cmd.arg("--decoder").arg(self.peer_decoder.trim());
        }
        // Persist codec/decoder selections back to ViewerConfig.
        {
            let mut cfg_guard = self.cfg.lock().unwrap();
            cfg_guard.viewer.codec = self.peer_codec.clone();
            cfg_guard.viewer.decoder = self.peer_decoder.clone();
            let path = self.config_path.clone();
            let cfg_snapshot = cfg_guard.clone();
            drop(cfg_guard);
            let _ = cfg_snapshot.save(&path);
        }
        match cmd.spawn() {
            Ok(child) => {
                self.connect_status = Some(format!("launched viewer (pid {})", child.id()));
            }
            Err(e) => {
                self.connect_status = Some(format!("spawn failed: {e}"));
            }
        }
    }
}

impl eframe::App for ClientApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drain any finished listener result before drawing so the UI shows
        // the cause (port-in-use, bind failure, etc.) instead of silently
        // flipping back to Idle.
        self.drain_listener_result();

        // Listener spawns asynchronously; the host task creates host-key.bin
        // a few hundred ms after Start Listener is clicked. Poll for it each
        // frame while waiting so the user doesn't have to click "Refresh
        // Pubkey" manually. Tighter repaint cadence kicks in below.
        let waiting_for_pubkey =
            self.is_listening() && self.pubkey_b64.is_none() && self.pubkey_load_error.is_none();
        if waiting_for_pubkey {
            self.refresh_pubkey();
        }

        // Draw the consent dialog (if any) before tabs so it sits over
        // both. Pulls one request off the channel when idle; subsequent
        // requests stay buffered in the unbounded channel and surface in
        // arrival order as each dialog is dismissed.
        self.poll_consent_channel();
        self.draw_consent_dialog(ctx);

        egui::TopBottomPanel::top("client_tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::ThisDevice, "This Device");
                ui.selectable_value(&mut self.tab, Tab::Connect, "Connect");
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::ThisDevice => self.draw_this_device(ui),
            Tab::Connect => self.draw_connect(ui),
        });

        // Repaint cadence: 100ms while polling for first pubkey (snappy
        // feedback during the ~hundreds-of-ms key-generation window),
        // 1Hz otherwise to keep listener-state transitions visible.
        let next = if waiting_for_pubkey {
            std::time::Duration::from_millis(100)
        } else {
            std::time::Duration::from_secs(1)
        };
        ctx.request_repaint_after(next);
    }
}

impl ClientApp {
    fn draw_this_device(&mut self, ui: &mut egui::Ui) {
        ui.heading("This Device");
        ui.add_space(6.0);

        // Pubkey block
        match (&self.pubkey_b64, &self.pubkey_load_error) {
            (Some(pk), _) => {
                ui.horizontal(|ui| {
                    ui.label("Pubkey:");
                    ui.add(
                        egui::TextEdit::singleline(&mut pk.clone())
                            .desired_width(420.0)
                            .interactive(false),
                    );
                    if ui.button("Copy").clicked() {
                        ui.output_mut(|o| o.copied_text = pk.clone());
                    }
                });
            }
            (None, Some(err)) => {
                ui.colored_label(egui::Color32::LIGHT_RED, err);
            }
            (None, None) => {
                ui.label(
                    "Pubkey: (not generated yet \u{2014} start the listener to create host-key.bin)",
                );
            }
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // Listener controls
        let listening = self.is_listening();
        ui.horizontal(|ui| {
            if listening {
                ui.colored_label(egui::Color32::LIGHT_GREEN, "\u{25cf} Listening");
                if ui.button("Stop Listener").clicked() {
                    self.stop_listener();
                }
            } else {
                ui.colored_label(egui::Color32::GRAY, "\u{25cb} Idle");
                if ui.button("Start Listener").clicked() {
                    self.start_listener();
                    self.refresh_pubkey();
                }
            }
            if ui.button("Refresh Pubkey").clicked() {
                self.refresh_pubkey();
            }
        });

        if let Some(status) = &self.host_status {
            ui.add_space(8.0);
            // Errors get red highlighting for quick triage.
            if status.starts_with("listener error")
                || status.starts_with("cannot start")
                || status.starts_with("invalid host args")
            {
                ui.colored_label(egui::Color32::LIGHT_RED, status);
            } else {
                ui.label(status);
            }
        }
    }

    /// Pull at most one consent request off the channel when no dialog is
    /// already up. Multiple in-flight unknown peers are rare and stay
    /// buffered in the unbounded channel until the user dismisses the
    /// current dialog.
    fn poll_consent_channel(&mut self) {
        poll_consent_channel_impl(
            &mut self.consent_rx,
            &mut self.pending_consent,
            &self.host_auth,
        );
    }

    fn draw_consent_dialog(&mut self, ctx: &egui::Context) {
        let Some(p) = self.pending_consent.as_mut() else {
            return;
        };
        if let Some(decision) = p.state.show(ctx) {
            if let Some(p) = self.pending_consent.take() {
                let _ = p.responder.send(decision);
            }
        }
    }

    fn draw_connect(&mut self, ui: &mut egui::Ui) {
        ui.heading("Connect");
        ui.add_space(6.0);

        ui.label("Peer host:port (direct mode)");
        ui.add(egui::TextEdit::singleline(&mut self.peer_host).desired_width(280.0));

        ui.add_space(4.0);
        ui.label("Peer pubkey (base64)");
        ui.add(egui::TextEdit::singleline(&mut self.peer_pubkey).desired_width(420.0));

        ui.add_space(4.0);
        ui.label("Codec");
        egui::ComboBox::from_id_source("connect-codec-combo")
            .selected_text(&self.peer_codec)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.peer_codec, "auto".to_string(), "auto");
                ui.selectable_value(&mut self.peer_codec, "h264".to_string(), "h264");
                ui.selectable_value(&mut self.peer_codec, "h265".to_string(), "h265");
            });

        ui.add_space(4.0);
        ui.label("Decoder");
        egui::ComboBox::from_id_source("connect-decoder-combo")
            .selected_text(&self.peer_decoder)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.peer_decoder, "auto".to_string(), "auto");
                ui.selectable_value(&mut self.peer_decoder, "nvdec".to_string(), "nvdec");
                ui.selectable_value(&mut self.peer_decoder, "mf".to_string(), "mf");
                ui.selectable_value(&mut self.peer_decoder, "openh264".to_string(), "openh264");
            });

        ui.add_space(8.0);
        if ui.button("Connect").clicked() {
            self.spawn_connect();
        }

        if let Some(status) = &self.connect_status {
            ui.add_space(8.0);
            if status.starts_with("spawn failed") || status.starts_with("current_exe") {
                ui.colored_label(egui::Color32::LIGHT_RED, status);
            } else {
                ui.label(status);
            }
        }
    }
}

/// Free function implementing the consent-poll logic so it can be tested
/// without constructing a full `ClientApp`.
fn poll_consent_channel_impl(
    rx: &mut Option<tokio::sync::mpsc::UnboundedReceiver<prdt_host::ConsentRequest>>,
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
            let state = prdt_gui_host::consent_prompt::ConsentPromptState::new(
                req.peer_pubkey.to_base64(),
                host_auth.default_permissions,
                std::time::Duration::from_secs(u64::from(host_auth.consent_timeout_seconds)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use prdt_gui_host::consent_channel::{ConsentDecision, ConsentRequest};

    #[test]
    fn poll_consent_channel_picks_up_request() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let _guard = rt.enter();

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<prdt_host::ConsentRequest>();
        let mut consent_rx: Option<
            tokio::sync::mpsc::UnboundedReceiver<prdt_host::ConsentRequest>,
        > = Some(rx);
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

        poll_consent_channel_impl(&mut consent_rx, &mut pending, &host_auth);

        assert!(pending.is_some(), "pending_consent should be populated");
        assert!(consent_rx.is_some(), "rx should still be open");
    }

    #[test]
    fn poll_consent_channel_clears_rx_on_disconnect() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let _guard = rt.enter();

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<prdt_host::ConsentRequest>();
        let mut consent_rx: Option<
            tokio::sync::mpsc::UnboundedReceiver<prdt_host::ConsentRequest>,
        > = Some(rx);
        let mut pending: Option<PendingPrompt> = None;
        let host_auth = HostAuthConfig::default();

        // Drop the sender so the channel is disconnected.
        drop(tx);

        poll_consent_channel_impl(&mut consent_rx, &mut pending, &host_auth);

        assert!(consent_rx.is_none(), "rx should be cleared on disconnect");
        assert!(pending.is_none(), "no pending prompt expected");
    }
}
