//! Unified client app: "This Device" + "Connect" tabs in one egui window.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clap::Parser as _;
use prdt_gui_common::Config;
use tokio::runtime;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    ThisDevice,
    Connect,
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

    // Connect
    peer_host: String,
    peer_pubkey: String,
    last_connect_status: Option<String>,
}

struct ListenerState {
    cancel: CancellationToken,
    join: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl ClientApp {
    pub fn new(
        cfg: Arc<Mutex<Config>>,
        config_path: PathBuf,
        rt_handle: runtime::Handle,
    ) -> Self {
        let mut app = Self {
            cfg,
            config_path,
            rt_handle,
            tab: Tab::ThisDevice,
            pubkey_b64: None,
            pubkey_load_error: None,
            listener: None,
            peer_host: "127.0.0.1:9000".to_string(),
            peer_pubkey: String::new(),
            last_connect_status: None,
        };
        app.refresh_pubkey();
        app
    }

    /// Read host-key.bin (default path) and derive the pubkey for display.
    /// On miss the pubkey is None until the host listener generates one.
    fn refresh_pubkey(&mut self) {
        let path = std::path::Path::new("host-key.bin");
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
                self.pubkey_load_error =
                    Some(format!("host-key.bin is {} bytes, expected 32", other.len()));
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

    fn start_listener(&mut self) {
        if self.is_listening() {
            return;
        }
        // Build prdt-host Args from defaults + GUI config. Construct via
        // parse_from so all clap defaults apply, then override known fields.
        let mut argv: Vec<std::ffi::OsString> = vec!["prdt-host".into()];
        // Bind / bitrate / monitor / key_file come from gui-common Config.
        let cfg = self.cfg.lock().unwrap().clone();
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
                self.last_connect_status = Some(format!("invalid host args: {e}"));
                return;
            }
        };

        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let join = self
            .rt_handle
            .spawn(async move { prdt_host::run_host(args, None, cancel_for_task).await });
        self.listener = Some(ListenerState { cancel, join });
        // After a successful start, host will (re)generate host-key.bin if needed.
        // Refresh pubkey on next ui tick — egui repaints frequently so we just
        // mark for refresh by clearing the error; the next refresh_pubkey call
        // will pick up the new file.
    }

    fn stop_listener(&mut self) {
        if let Some(state) = self.listener.take() {
            state.cancel.cancel();
            // Don't await here; the task will wind down on the runtime.
            // Drop the join handle and let it run to completion in background.
            let _ = self.rt_handle.spawn(async move {
                let _ = state.join.await;
            });
        }
    }

    fn spawn_connect(&mut self) {
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                self.last_connect_status = Some(format!("current_exe: {e}"));
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
        match cmd.spawn() {
            Ok(child) => {
                self.last_connect_status =
                    Some(format!("launched viewer (pid {})", child.id()));
            }
            Err(e) => {
                self.last_connect_status = Some(format!("spawn failed: {e}"));
            }
        }
    }
}

impl eframe::App for ClientApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Tab bar
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

        // Repaint at 1Hz so listener-state transitions are visible without input.
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
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
                ui.label("Pubkey: (not generated yet — start the listener to create host-key.bin)");
            }
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // Listener controls
        let listening = self.is_listening();
        ui.horizontal(|ui| {
            if listening {
                ui.colored_label(egui::Color32::LIGHT_GREEN, "● Listening");
                if ui.button("Stop Listener").clicked() {
                    self.stop_listener();
                }
            } else {
                ui.colored_label(egui::Color32::GRAY, "○ Idle");
                if ui.button("Start Listener").clicked() {
                    self.start_listener();
                    self.refresh_pubkey();
                }
            }
            if ui.button("Refresh Pubkey").clicked() {
                self.refresh_pubkey();
            }
        });

        if let Some(status) = &self.last_connect_status {
            ui.add_space(8.0);
            ui.label(status);
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

        ui.add_space(8.0);
        if ui.button("Connect").clicked() {
            self.spawn_connect();
        }

        if let Some(status) = &self.last_connect_status {
            ui.add_space(8.0);
            ui.label(status);
        }
    }
}
