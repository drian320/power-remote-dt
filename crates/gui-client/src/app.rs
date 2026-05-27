//! Unified client app: a left nav rail (Home / Settings / Logs) in one egui
//! window. Home is a split dashboard (share-this-device + connect-to-a-device);
//! Settings is the full persisted-config surface; Logs is a placeholder.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clap::Parser as _;
use prdt_gui_common::auth_config::{AuthMode, HostAuthConfig};
use prdt_gui_common::theme::tokens;
use prdt_gui_common::Config;
use tokio::runtime;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// Left-nav route. Replaces the old top tab bar.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Home,
    Settings,
    Logs,
}

struct PendingPrompt {
    state: prdt_gui_host::consent_prompt::ConsentPromptState,
    responder: tokio::sync::oneshot::Sender<prdt_gui_host::consent_channel::ConsentDecision>,
}

pub struct ClientApp {
    cfg: Arc<Mutex<Config>>,
    config_path: PathBuf,
    rt_handle: runtime::Handle,

    view: View,

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
    /// Path to host-auth.toml (sibling of `config_path`). Persisted on Save.
    host_auth_path: PathBuf,

    /// Working copies edited by the Settings view; persisted to disk (and into
    /// the live `cfg`/`host_auth`) only when Save is clicked.
    settings_draft: SettingsDraft,
    /// One-shot confirmation line shown under the Settings Save button.
    settings_status: Option<String>,

    /// Set when launched with `--host-autostart` (after an elevated relaunch on
    /// Windows): the first frame starts the host listener automatically.
    pending_autostart: bool,
    /// Set by the host-only self-elevation path: the next frame closes this
    /// (non-elevated) window because an elevated copy is taking over hosting.
    /// Always false on non-Windows.
    request_exit: bool,
}

/// Editable mirror of the persisted config, used by the Settings view so that
/// in-progress edits do not touch the live `cfg` mutex (which the listener
/// path reads) until the user explicitly saves.
struct SettingsDraft {
    config: Config,
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
    pub fn new(
        cfg: Arc<Mutex<Config>>,
        config_path: PathBuf,
        rt_handle: runtime::Handle,
        autostart_host: bool,
    ) -> Self {
        // Derive host-auth.toml path from the config directory (mirrors gui-host).
        let host_auth_path = config_path
            .parent()
            .map(|p| p.join("host-auth.toml"))
            .unwrap_or_else(|| PathBuf::from("host-auth.toml"));
        let host_auth = HostAuthConfig::load_or_default(&host_auth_path).unwrap_or_default();

        let (peer_codec, peer_decoder, config_snapshot) = {
            let cfg_guard = cfg.lock().unwrap();
            (
                cfg_guard.viewer.codec.clone(),
                cfg_guard.viewer.decoder.clone(),
                cfg_guard.clone(),
            )
        };
        let settings_draft = SettingsDraft {
            config: config_snapshot,
            host_auth: host_auth.clone(),
        };
        let mut app = Self {
            cfg,
            config_path,
            rt_handle,
            view: View::Home,
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
            host_auth_path,
            settings_draft,
            settings_status: None,
            pending_autostart: autostart_host,
            request_exit: false,
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

        // Host-only UAC elevation (Windows). The host injects input via
        // SendInput, which Windows UIPI blocks against higher-integrity windows
        // (Task Manager, UAC dialogs). If we're not already elevated, relaunch
        // the GUI as admin and let that copy auto-start the listener, then close
        // this one. If the user declines the prompt, fall through and host
        // un-elevated (works for normal windows, not elevated ones).
        #[cfg(windows)]
        if !crate::elevate::is_elevated() {
            match crate::elevate::relaunch_elevated_for_host() {
                Ok(()) => {
                    self.host_status = Some("管理者として再起動しています…".into());
                    self.request_exit = true;
                    return;
                }
                Err(e) => {
                    self.host_status = Some(format!(
                        "管理者昇格に失敗しました（{e}）。通常権限で起動します（タスクマネージャー等の管理者ウィンドウは操作できません）。"
                    ));
                    // fall through: start a non-elevated host anyway.
                }
            }
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
            if let Err(e) = cfg_snapshot.save(&path) {
                tracing::warn!(
                    ?e,
                    "config save failed (codec/decoder selection not persisted)"
                );
            }
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

    /// Persist the Settings working copy to disk and into the live state.
    /// Updates `self.cfg` in place (under the mutex) without holding it across
    /// the disk write, so the listener path is never blocked.
    fn save_settings(&mut self) {
        // Mirror codec/decoder edits into the Connect view fields so they stay
        // in sync with what was just persisted.
        self.peer_codec = self.settings_draft.config.viewer.codec.clone();
        self.peer_decoder = self.settings_draft.config.viewer.decoder.clone();

        let cfg_snapshot = self.settings_draft.config.clone();
        {
            let mut guard = self.cfg.lock().unwrap();
            *guard = cfg_snapshot.clone();
        }
        self.host_auth = self.settings_draft.host_auth.clone();

        let cfg_err = cfg_snapshot.save(&self.config_path).err();
        let auth_err = self.host_auth.save(&self.host_auth_path).err();

        self.settings_status = Some(match (cfg_err, auth_err) {
            (None, None) => "saved".to_string(),
            (Some(e), _) => format!("save failed (config): {e}"),
            (_, Some(e)) => format!("save failed (host-auth): {e}"),
        });
    }
}

impl eframe::App for ClientApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // After an elevated relaunch (`--host-autostart`), kick off the host
        // listener once on the first frame. We're already elevated here, so
        // start_listener won't re-trigger the elevation relaunch.
        if self.pending_autostart {
            self.pending_autostart = false;
            self.view = View::Home;
            self.start_listener();
            self.refresh_pubkey();
        }

        // Host-only self-elevation requested this (non-elevated) window to close
        // because an elevated copy is taking over hosting.
        if self.request_exit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

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

        // Draw the consent dialog (if any) before the panels so it sits over
        // every view. Pulls one request off the channel when idle; subsequent
        // requests stay buffered in the unbounded channel and surface in
        // arrival order as each dialog is dismissed.
        self.poll_consent_channel();
        self.draw_consent_dialog(ctx);

        egui::SidePanel::left("client_nav")
            .resizable(false)
            .exact_width(140.0)
            .show(ctx, |ui| {
                self.draw_nav(ui);
            });

        egui::CentralPanel::default().show(ctx, |ui| match self.view {
            View::Home => self.draw_home(ui),
            View::Settings => self.draw_settings(ui),
            View::Logs => self.draw_logs(ui),
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
    fn draw_nav(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.heading("prdt");
        ui.add_space(12.0);
        self.nav_entry(ui, View::Home, "Home");
        self.nav_entry(ui, View::Settings, "Settings");
        self.nav_entry(ui, View::Logs, "Logs");
    }

    /// A full-width selectable nav entry. The active route is highlighted with
    /// the accent fill so it reads as the current location.
    fn nav_entry(&mut self, ui: &mut egui::Ui, view: View, label: &str) {
        let selected = self.view == view;
        let mut button = egui::Button::new(label).min_size(egui::vec2(ui.available_width(), 0.0));
        if selected {
            button = button.fill(tokens::ACCENT).stroke(egui::Stroke::NONE);
        }
        let resp = ui.add(button);
        if selected {
            // Accent fill is bright cyan; paint the label in dark text on top
            // for legibility.
            ui.painter().text(
                resp.rect.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(14.0),
                tokens::BG_DEEP,
            );
        }
        if resp.clicked() {
            self.view = view;
        }
    }

    fn draw_home(&mut self, ui: &mut egui::Ui) {
        ui.heading("Home");
        ui.add_space(8.0);
        ui.columns(2, |cols| {
            egui::Frame::group(cols[0].style()).show(&mut cols[0], |ui| {
                self.draw_share_device(ui);
            });
            egui::Frame::group(cols[1].style()).show(&mut cols[1], |ui| {
                self.draw_connect(ui);
            });
        });
    }

    fn draw_share_device(&mut self, ui: &mut egui::Ui) {
        ui.heading("Share this device");
        ui.add_space(6.0);

        // Pubkey block: monospace, grouped in 4-char chunks for legibility.
        match (&self.pubkey_b64, &self.pubkey_load_error) {
            (Some(pk), _) => {
                ui.label("Pubkey");
                ui.label(
                    egui::RichText::new(group_in_chunks(pk, 4))
                        .monospace()
                        .color(tokens::TEXT),
                );
                if ui.button("Copy").clicked() {
                    ui.ctx().copy_text(pk.clone());
                }
            }
            (None, Some(err)) => {
                ui.colored_label(tokens::DESTRUCTIVE, err);
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

        // Listener controls with a colored status dot.
        let listening = self.is_listening();
        ui.horizontal(|ui| {
            if listening {
                ui.colored_label(tokens::OK, "\u{25cf}");
                ui.label("Listening");
            } else {
                ui.colored_label(tokens::TEXT_DIM, "\u{25cb}");
                ui.label("Idle");
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if listening {
                if ui.button("Stop Listener").clicked() {
                    self.stop_listener();
                }
            } else if ui.button("Start Listener").clicked() {
                self.start_listener();
                self.refresh_pubkey();
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
                ui.colored_label(tokens::DESTRUCTIVE, status);
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
        ui.heading("Connect to a device");
        ui.add_space(6.0);

        ui.label("Peer host:port (direct mode)");
        ui.add(egui::TextEdit::singleline(&mut self.peer_host).desired_width(280.0));

        ui.add_space(4.0);
        ui.label("Peer pubkey (base64)");
        ui.add(egui::TextEdit::singleline(&mut self.peer_pubkey).desired_width(420.0));

        ui.add_space(4.0);
        ui.label("Codec");
        egui::ComboBox::from_id_salt("connect-codec-combo")
            .selected_text(&self.peer_codec)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.peer_codec, "auto".to_string(), "auto");
                ui.selectable_value(&mut self.peer_codec, "h264".to_string(), "h264");
                ui.selectable_value(&mut self.peer_codec, "h265".to_string(), "h265");
            });

        ui.add_space(4.0);
        ui.label("Decoder");
        egui::ComboBox::from_id_salt("connect-decoder-combo")
            .selected_text(&self.peer_decoder)
            .show_ui(ui, |ui| {
                for opt in prdt_viewer::supported_decoder_args() {
                    ui.selectable_value(&mut self.peer_decoder, opt.to_string(), opt);
                }
            });

        ui.add_space(8.0);
        // Prominent accent-filled primary action.
        let connect = egui::Button::new(egui::RichText::new("Connect").color(tokens::BG_DEEP))
            .fill(tokens::ACCENT);
        if ui.add(connect).clicked() {
            self.spawn_connect();
        }

        if let Some(status) = &self.connect_status {
            ui.add_space(8.0);
            if status.starts_with("spawn failed") || status.starts_with("current_exe") {
                ui.colored_label(tokens::DESTRUCTIVE, status);
            } else {
                ui.label(status);
            }
        }
    }

    fn draw_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Settings");
        ui.add_space(8.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            let d = &mut self.settings_draft;

            // General
            ui.group(|ui| {
                ui.heading("General");
                ui.add_space(4.0);
                ui.label("Locale");
                egui::ComboBox::from_id_salt("set-locale")
                    .selected_text(locale_label(&d.config.gui.locale))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut d.config.gui.locale, String::new(), "auto");
                        ui.selectable_value(&mut d.config.gui.locale, "en".to_string(), "en");
                        ui.selectable_value(&mut d.config.gui.locale, "ja".to_string(), "ja");
                    });
                ui.checkbox(&mut d.config.host.auto_start, "Auto-start host on launch");
            });

            // Network
            ui.add_space(6.0);
            ui.group(|ui| {
                ui.heading("Network");
                ui.add_space(4.0);
                labeled_text(ui, "Host bind", &mut d.config.host.bind);
                labeled_text(ui, "Host signaling URL", &mut d.config.host.signaling_url);
                labeled_path(ui, "Host ID file", &mut d.config.host.host_id_file);
                labeled_text(
                    ui,
                    "Viewer signaling URL",
                    &mut d.config.viewer.signaling_url,
                );
                labeled_path(ui, "Known hosts", &mut d.config.viewer.known_hosts);
                labeled_path(ui, "Known host IDs", &mut d.config.viewer.known_host_ids);
            });

            // Video — Host
            ui.add_space(6.0);
            ui.group(|ui| {
                ui.heading("Video \u{2014} Host");
                ui.add_space(4.0);
                ui.label("Encoder");
                egui::ComboBox::from_id_salt("set-encoder")
                    .selected_text(&d.config.host.encoder)
                    .show_ui(ui, |ui| {
                        for opt in prdt_host::supported_encoder_args() {
                            ui.selectable_value(&mut d.config.host.encoder, opt.to_string(), opt);
                        }
                    });
                labeled_drag_u32(ui, "Bitrate (Mbps)", &mut d.config.host.bitrate_mbps);
                labeled_drag_u32(ui, "Monitor index", &mut d.config.host.monitor);
            });

            // Video — Viewer
            ui.add_space(6.0);
            ui.group(|ui| {
                ui.heading("Video \u{2014} Viewer");
                ui.add_space(4.0);
                ui.label("Decoder");
                egui::ComboBox::from_id_salt("set-decoder")
                    .selected_text(&d.config.viewer.decoder)
                    .show_ui(ui, |ui| {
                        for opt in prdt_viewer::supported_decoder_args() {
                            ui.selectable_value(&mut d.config.viewer.decoder, opt.to_string(), opt);
                        }
                    });
                ui.label("Codec");
                egui::ComboBox::from_id_salt("set-codec")
                    .selected_text(&d.config.viewer.codec)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut d.config.viewer.codec, "auto".to_string(), "auto");
                        ui.selectable_value(&mut d.config.viewer.codec, "h264".to_string(), "h264");
                        ui.selectable_value(&mut d.config.viewer.codec, "h265".to_string(), "h265");
                    });
                labeled_text(
                    ui,
                    "Default resolution",
                    &mut d.config.viewer.default_resolution,
                );
                labeled_drag_u32(ui, "Default FPS", &mut d.config.viewer.default_fps);
            });

            // Paths
            ui.add_space(6.0);
            ui.group(|ui| {
                ui.heading("Paths");
                ui.add_space(4.0);
                labeled_path(ui, "Host key file", &mut d.config.host.key_file);
                labeled_path(ui, "Host outgoing dir", &mut d.config.host.outgoing_dir);
                labeled_path(ui, "Viewer receive dir", &mut d.config.viewer.recv_dir);
            });

            // Security
            ui.add_space(6.0);
            ui.group(|ui| {
                ui.heading("Security");
                ui.add_space(4.0);
                ui.label("Auth mode");
                egui::ComboBox::from_id_salt("set-auth-mode")
                    .selected_text(auth_mode_label(d.host_auth.mode))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut d.host_auth.mode, AuthMode::Tofu, "Tofu");
                        ui.selectable_value(&mut d.host_auth.mode, AuthMode::Pin, "Pin");
                        ui.selectable_value(
                            &mut d.host_auth.mode,
                            AuthMode::Ephemeral,
                            "Ephemeral",
                        );
                    });
                if d.host_auth.pin_hash.is_some() {
                    ui.colored_label(tokens::TEXT_DIM, "A PIN is set (edit PIN out of scope).");
                }
                labeled_drag_u32(
                    ui,
                    "Consent timeout (s)",
                    &mut d.host_auth.consent_timeout_seconds,
                );
                labeled_drag_u8(ui, "Max PIN attempts", &mut d.host_auth.max_pin_attempts);
                labeled_drag_u32(ui, "PIN lockout (s)", &mut d.host_auth.pin_lockout_seconds);
                labeled_drag_u32(
                    ui,
                    "Ephemeral lifetime (s)",
                    &mut d.host_auth.ephemeral_lifetime_seconds,
                );
                ui.add_space(4.0);
                ui.label("Default permissions");
                ui.checkbox(&mut d.host_auth.default_permissions.input, "Input");
                ui.checkbox(&mut d.host_auth.default_permissions.clipboard, "Clipboard");
                ui.checkbox(
                    &mut d.host_auth.default_permissions.file_transfer,
                    "File transfer",
                );
                ui.checkbox(&mut d.host_auth.default_permissions.audio, "Audio");
            });

            ui.add_space(10.0);
            if ui.button("Save").clicked() {
                self.save_settings();
            }
            if let Some(status) = &self.settings_status {
                ui.add_space(4.0);
                if status.starts_with("save failed") {
                    ui.colored_label(tokens::DESTRUCTIVE, status);
                } else {
                    ui.colored_label(tokens::OK, status);
                }
            }
        });
    }

    fn draw_logs(&mut self, ui: &mut egui::Ui) {
        ui.heading("Logs");
        ui.add_space(8.0);
        ui.colored_label(
            tokens::TEXT_DIM,
            "Recent activity is written to stderr; in-app log tailing is not yet wired here.",
        );
    }
}

/// Group a string into space-separated chunks of `n` chars (e.g. base64 pubkey
/// into 4-char blocks for readability).
fn group_in_chunks(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    chars
        .chunks(n)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

fn locale_label(locale: &str) -> &str {
    match locale {
        "en" => "en",
        "ja" => "ja",
        _ => "auto",
    }
}

fn auth_mode_label(mode: AuthMode) -> &'static str {
    match mode {
        AuthMode::Tofu => "Tofu",
        AuthMode::Pin => "Pin",
        AuthMode::Ephemeral => "Ephemeral",
    }
}

/// A label followed by a single-line text editor on its own row.
fn labeled_text(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::TextEdit::singleline(value).desired_width(280.0));
    });
}

/// A label followed by a text editor bound to a `PathBuf` (edited as its
/// lossy string form; written back verbatim).
fn labeled_path(ui: &mut egui::Ui, label: &str, value: &mut PathBuf) {
    let mut text = value.to_string_lossy().into_owned();
    ui.horizontal(|ui| {
        ui.label(label);
        if ui
            .add(egui::TextEdit::singleline(&mut text).desired_width(280.0))
            .changed()
        {
            *value = PathBuf::from(text);
        }
    });
}

fn labeled_drag_u32(ui: &mut egui::Ui, label: &str, value: &mut u32) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::DragValue::new(value));
    });
}

fn labeled_drag_u8(ui: &mut egui::Ui, label: &str, value: &mut u8) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::DragValue::new(value));
    });
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

    #[test]
    fn group_in_chunks_splits_into_blocks() {
        assert_eq!(group_in_chunks("A1B2C3D4", 4), "A1B2 C3D4");
        assert_eq!(group_in_chunks("ABC", 4), "ABC");
        assert_eq!(group_in_chunks("ABCDE", 4), "ABCD E");
    }
}
