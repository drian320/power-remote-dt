//! Unified client app: a left nav rail (Home / Settings / Logs) in one egui
//! window. Home is the RustDesk-style single screen — "This device"
//! (server-allocated 9-digit ID + fixed PIN + key fingerprint/QR + share
//! start/stop) sits beside "Connect to a device" (enter a peer ID + PIN and
//! connect, with a collapsible Advanced section for legacy direct mode and a
//! recent-connections list). Settings is the full persisted-config surface;
//! Logs is a placeholder.
//!
//! The "This device" panel is wired to the offline-first provisioning API
//! (`prdt_gui_common::identity::DeviceIdentity`): the identity + fixed PIN are
//! available without any network, and the server-allocated ID is filled in
//! asynchronously via a background `reprovision` task (never on the UI thread).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use clap::Parser as _;
use prdt_gui_common::auth_config::{AuthMode, HostAuthConfig};
use prdt_gui_common::t;
use prdt_gui_common::theme::tokens;
use prdt_gui_common::{generate_qr, Config, DeviceIdentity, HostEntry, IdentityPaths};
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

/// Which recent-connection entry to record after a successful launch.
enum RecentUpsert {
    Signaling(String),
    Direct { addr: String, pubkey: String },
}

/// How long a freshly-spawned viewer child stays in the "connecting" phase
/// before the UI upgrades it to "session running". We keep the viewer as a
/// child process (ADR B1 / AC-5改) and have no IPC to it, so there is no real
/// "connected" signal — a child that outlives this window has almost certainly
/// established a session (connection failures — bad PIN, unreachable host,
/// signaling timeout — exit well before this), whereas a child still alive
/// later is a live session or a user-open window. This is a liveness heuristic,
/// deliberately not a claim of protocol-level connectedness.
const CONNECT_GRACE: Duration = Duration::from_secs(3);

/// The single active outbound viewer session (AC-10: at most one at a time).
///
/// Owns the spawned `prdt connect --headless` child so the GUI can (a) show a
/// live status instead of a fire-and-forget spawn, (b) reap it on app exit so
/// no orphan/zombie is left behind, and (c) enforce the one-session limit. The
/// viewer keeps its own winit event loop + D3D11 device in this child process
/// (that boundary is intentionally preserved per the ADR); this struct is only
/// the parent-side handle to it.
struct ViewerSession {
    child: std::process::Child,
    /// Cached child PID for the status line (valid even after `child` is reaped).
    pid: u32,
    /// Peer ID (signaling) or host:port (direct), shown in the status line.
    target: String,
    /// When the child was spawned; drives the connecting → running heuristic.
    started_at: Instant,
}

impl ViewerSession {
    /// Best-effort connection phase for display only. `true` during the initial
    /// [`CONNECT_GRACE`] window; see that constant for why this is a heuristic
    /// and not a protocol-level "connected" signal.
    fn is_connecting(&self) -> bool {
        self.started_at.elapsed() < CONNECT_GRACE
    }
}

/// #11a guard: resolve a viewer `--decoder` selection to a value that is valid
/// for *this* build, falling back to `auto`. Empty or unsupported selections
/// (e.g. the `ViewerConfig` default `decoder = "nvdec"` on a Linux build, which
/// bypasses clap's `PossibleValuesParser` when injected via `config.toml` and
/// crashes the child at decoder dispatch) collapse to `auto`, which the viewer
/// resolves post-handshake. `supported_decoder_args()` always lists `auto`
/// first, so the fallback is itself always valid.
fn resolve_decoder_arg(sel: &str) -> String {
    let sel = sel.trim();
    if !sel.is_empty()
        && prdt_viewer::supported_decoder_args()
            .iter()
            .any(|d| *d == sel)
    {
        sel.to_string()
    } else {
        "auto".to_string()
    }
}

/// #11a guard for `--codec`: only the values the viewer CLI accepts pass
/// through; anything else (empty, stale, or unknown) collapses to `auto`.
fn resolve_codec_arg(sel: &str) -> String {
    match sel.trim() {
        v @ ("auto" | "h264" | "h265") => v.to_string(),
        _ => "auto".to_string(),
    }
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

    // This Device — offline-first provisioning identity (AC-2/AC-3/AC-9/AC-14).
    /// Loaded (and bootstrapped) at startup with no network I/O. `None` only if
    /// the on-disk key/record could not be read.
    identity: Option<DeviceIdentity>,
    /// Set when `DeviceIdentity::load_or_create` failed at startup.
    identity_error: Option<String>,
    /// Whether the fixed PIN is currently shown in the clear.
    pin_revealed: bool,
    /// Whether the fingerprint QR (out-of-band verification) is expanded.
    show_fingerprint_qr: bool,
    /// Cached fingerprint QR texture; regenerated lazily on first show and
    /// dropped when the identity is reloaded.
    fingerprint_qr: Option<egui::TextureHandle>,
    /// In-flight background `reprovision` task: receives the mutated identity on
    /// success, or an error message. `Some` while a provisioning attempt runs.
    reprovision_rx: Option<oneshot::Receiver<Result<DeviceIdentity, String>>>,
    /// Last provisioning status line (result of a Retry / regenerate).
    provision_status: Option<String>,

    listener: Option<ListenerState>,
    /// Last status line shown under the sharing controls (start/stop result,
    /// or the error returned by the host task when it exits).
    host_status: Option<String>,

    // Connect — easy path (signaling): peer ID + PIN.
    peer_id: String,
    peer_pin: String,
    // Connect — advanced / legacy direct mode (kept so nothing is lost).
    peer_host: String,
    peer_pubkey: String,
    peer_codec: String,
    peer_decoder: String,
    /// Last status line shown under the Connect button (terminal / error text;
    /// the live phase of an active session is derived from `viewer_session`).
    connect_status: Option<String>,
    /// Whether `connect_status` should render as an error (destructive color).
    connect_error: bool,
    /// The single active outbound viewer session (AC-10). `Some` while a
    /// spawned viewer child is tracked; polled each frame and reaped on exit
    /// or app close (AC-5改). `None` means no outbound session is running.
    viewer_session: Option<ViewerSession>,

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
        // The config directory holds identity.toml + host-auth.toml (mirrors
        // gui-host). host-key.bin lives in the data dir and is passed explicitly.
        let config_root = config_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let host_auth_path = config_root.join("host-auth.toml");

        let (peer_codec, peer_decoder, signaling_url, key_file, config_snapshot) = {
            let cfg_guard = cfg.lock().unwrap();
            (
                cfg_guard.viewer.codec.clone(),
                cfg_guard.viewer.decoder.clone(),
                cfg_guard.host.signaling_url.clone(),
                cfg_guard.host.key_file.clone(),
                cfg_guard.clone(),
            )
        };

        // Offline-first identity bootstrap (AC-9): generates + fsyncs the key,
        // ensures a fixed PIN, reconciles the record — no network on this
        // (UI) thread. The server-allocated ID is filled in later via Retry.
        let id_paths = IdentityPaths::from_config_root(&config_root, key_file);
        let (identity, identity_error) =
            match DeviceIdentity::load_or_create(id_paths, &signaling_url) {
                Ok(id) => (Some(id), None),
                Err(e) => (
                    None,
                    Some(t!("home-identity-error", error => e.to_string())),
                ),
            };

        // host-auth.toml may have just been bootstrapped by the identity above;
        // load it now so the consent flow sees the same file.
        let host_auth = HostAuthConfig::load_or_default(&host_auth_path).unwrap_or_default();
        let settings_draft = SettingsDraft {
            config: config_snapshot,
            host_auth: host_auth.clone(),
        };

        Self {
            cfg,
            config_path,
            rt_handle,
            view: View::Home,
            identity,
            identity_error,
            pin_revealed: false,
            show_fingerprint_qr: false,
            fingerprint_qr: None,
            reprovision_rx: None,
            provision_status: None,
            listener: None,
            host_status: None,
            peer_id: String::new(),
            peer_pin: String::new(),
            peer_host: "127.0.0.1:9000".to_string(),
            peer_pubkey: String::new(),
            peer_codec,
            peer_decoder,
            connect_status: None,
            connect_error: false,
            viewer_session: None,
            consent_rx: None,
            pending_consent: None,
            host_auth,
            host_auth_path,
            settings_draft,
            settings_status: None,
            pending_autostart: autostart_host,
            request_exit: false,
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
            // AC-11: the elevated relaunch closes this GUI, whose Drop reaps the
            // tracked outbound viewer child. If a viewer session is live, that
            // relaunch would tear it down — exactly the teardown AC-11(b)
            // forbids. Preserve the session: host un-elevated instead (works for
            // normal target windows; input into elevated windows is unavailable
            // until the viewer is disconnected and sharing restarted). The full
            // fix is the narrow elevated input-injection helper (follow-up).
            if self.viewer_session.is_some() {
                self.host_status = Some(t!("home-host-elevation-skipped-viewer"));
                // fall through: start a non-elevated host, viewer left running.
            } else {
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

        // M1: point the host at the SAME config + auth/peers files the GUI
        // writes (siblings of config_path) so a non-default `--config` install
        // honours the configured PIN / permissions instead of silently reading
        // the OS-default %APPDATA%/prdt/... copies.
        argv.push("--config".into());
        argv.push(self.config_path.as_os_str().to_owned());
        argv.push("--host-auth-file".into());
        argv.push(self.host_auth_path.as_os_str().to_owned());
        let host_peers_path = self
            .config_path
            .parent()
            .map(|p| p.join("host-peers.toml"))
            .unwrap_or_else(|| PathBuf::from("host-peers.toml"));
        argv.push("--host-peers-file".into());
        argv.push(host_peers_path.into_os_string());

        // B1: when a signaling server is configured, run the host in rendezvous
        // mode so the 9-digit ID shown in "This device" is actually reachable
        // by a signaling viewer. Without --signaling-url the host runs LAN
        // fixed-address mode and never calls rendezvous_as_host, so the green
        // "共有中" state would be a lie. We register under the SAME id the UI
        // displays (DeviceIdentity::host_id()); the signaling store is
        // idempotent by pubkey (same key_file), so passing the id explicitly
        // makes the host live-registered under exactly that id.
        let signaling_url = cfg.host.signaling_url.trim();
        if !signaling_url.is_empty() {
            argv.push("--signaling-url".into());
            argv.push(signaling_url.into());
            if let Some(host_id) = self.identity.as_ref().and_then(|id| id.host_id()) {
                argv.push("--host-id".into());
                argv.push(host_id.into());
            }
        }

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

    /// Kick off a background provisioning attempt (AC-3). Clones the identity
    /// into a Tokio task and runs `reprovision` there — never on the UI thread,
    /// since it performs blocking network I/O across `.await`. The mutated
    /// identity (with the freshly allocated ID) comes back over a oneshot.
    fn start_reprovision(&mut self) {
        if self.reprovision_rx.is_some() {
            return; // already in flight
        }
        let Some(mut ident) = self.identity.clone() else {
            return;
        };
        let (tx, rx) = oneshot::channel();
        self.reprovision_rx = Some(rx);
        self.provision_status = None;
        self.rt_handle.spawn(async move {
            let res = match ident.reprovision(Duration::from_secs(10)).await {
                Ok(()) => Ok(ident),
                Err(e) => Err(e.to_string()),
            };
            let _ = tx.send(res);
        });
    }

    /// Poll the in-flight reprovision task without blocking. On success the
    /// live identity is replaced with the mutated one (which now carries the
    /// server-allocated ID and has persisted `identity.toml`).
    fn drain_reprovision(&mut self) {
        let Some(rx) = self.reprovision_rx.as_mut() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(ident)) => {
                self.identity = Some(ident);
                self.reprovision_rx = None;
                self.provision_status = Some(t!("home-provisioned"));
            }
            Ok(Err(e)) => {
                self.reprovision_rx = None;
                self.provision_status = Some(t!("home-provision-failed", error => e));
            }
            Err(oneshot::error::TryRecvError::Empty) => {}
            Err(oneshot::error::TryRecvError::Closed) => {
                self.reprovision_rx = None;
            }
        }
    }

    /// Regenerate the fixed PIN (AC-2). Writes host-auth.toml via the identity,
    /// then reloads the app's auth copy so the consent flow and the Settings
    /// draft stay consistent (they share the same file).
    fn regenerate_pin(&mut self) {
        let Some(ident) = self.identity.as_mut() else {
            return;
        };
        match ident.regenerate_pin() {
            Ok(_new) => {
                self.host_auth =
                    HostAuthConfig::load_or_default(&self.host_auth_path).unwrap_or_default();
                // Keep the Settings draft's PIN in sync without clobbering any
                // other unsaved edits it may hold.
                self.settings_draft.host_auth.pin_hash = self.host_auth.pin_hash.clone();
                self.settings_draft.host_auth.pin_plaintext = self.host_auth.pin_plaintext.clone();
                self.settings_draft.host_auth.mode = self.host_auth.mode;
                self.pin_revealed = true;
                self.provision_status = None;
            }
            Err(e) => {
                self.provision_status = Some(t!("home-provision-failed", error => e.to_string()));
            }
        }
    }

    /// Lazily build the fingerprint QR texture (AC-14) and cache it.
    fn ensure_fingerprint_qr(&mut self, ctx: &egui::Context) {
        if self.fingerprint_qr.is_some() {
            return;
        }
        let Some(ident) = self.identity.as_ref() else {
            return;
        };
        let fp = ident.fingerprint_full();
        if fp.is_empty() {
            return;
        }
        if let Ok(image) = generate_qr(fp, 4) {
            self.fingerprint_qr =
                Some(ctx.load_texture("device_fp_qr", image, egui::TextureOptions::default()));
        }
    }

    /// Launch a viewer session over the signaling path (the primary flow): the
    /// child process rendezvouses on the configured server by 9-digit ID and
    /// authenticates with the PIN. The process boundary is deliberately kept
    /// (ADR B1 / AC-5改) — this only wires ID+PIN into the existing spawn.
    fn connect_signaling(&mut self) {
        // AC-10: at most one outbound viewer session. Guard here so no code
        // path (button, recent entry, direct mode) can spawn a second child.
        if self.viewer_session.is_some() {
            self.set_connect_status(t!("home-connect-already-active"), false);
            return;
        }
        let id = self.peer_id.trim().to_string();
        if id.is_empty() {
            self.set_connect_status(t!("home-connect-need-id"), true);
            return;
        }
        let signaling_url = {
            let g = self.cfg.lock().unwrap();
            let v = g.viewer.signaling_url.trim().to_string();
            if v.is_empty() {
                g.host.signaling_url.trim().to_string()
            } else {
                v
            }
        };
        if signaling_url.is_empty() {
            self.set_connect_status(t!("home-connect-need-signaling"), true);
            return;
        }
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                self.set_connect_status(format!("current_exe: {e}"), true);
                return;
            }
        };
        let pin = self.peer_pin.trim().to_string();
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("connect").arg("--headless");
        cmd.arg("--config").arg(&self.config_path);
        cmd.arg("--signaling-url").arg(&signaling_url);
        cmd.arg("--host-id").arg(&id);
        if !pin.is_empty() {
            // S2: pass the PIN via the PRDT_PIN env var, never argv — the child's
            // /proc/<pid>/cmdline is world-readable (any local user could `ps`
            // the PIN out of it), whereas /proc/<pid>/environ is owner-only. The
            // viewer reads PRDT_PIN when --pin is absent; an explicit --pin still
            // wins for CLI back-compat.
            cmd.env("PRDT_PIN", &pin);
        }
        self.push_codec_decoder_args(&mut cmd);
        self.launch_viewer(cmd, id.clone(), RecentUpsert::Signaling(id));
    }

    /// Launch a viewer session in legacy direct mode (host:port + optional
    /// pubkey). Kept behind the Advanced section so benches/scripts' workflow
    /// stays reachable from the GUI (AC-6 CLI-compat spirit).
    fn connect_direct(&mut self) {
        // AC-10: one outbound session (see connect_signaling).
        if self.viewer_session.is_some() {
            self.set_connect_status(t!("home-connect-already-active"), false);
            return;
        }
        let host = self.peer_host.trim().to_string();
        if host.is_empty() {
            self.set_connect_status(t!("home-connect-need-host"), true);
            return;
        }
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                self.set_connect_status(format!("current_exe: {e}"), true);
                return;
            }
        };
        let pubkey = self.peer_pubkey.trim().to_string();
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("connect").arg("--headless");
        cmd.arg("--config").arg(&self.config_path);
        cmd.arg("--host").arg(&host);
        if !pubkey.is_empty() {
            cmd.arg("--host-pubkey").arg(&pubkey);
        }
        self.push_codec_decoder_args(&mut cmd);
        self.launch_viewer(
            cmd,
            host.clone(),
            RecentUpsert::Direct { addr: host, pubkey },
        );
    }

    /// #11a guard: always append explicit, build-valid `--decoder` and
    /// `--codec` to a viewer spawn. Passing them unconditionally (rather than
    /// only when the GUI field is non-empty) stops a stale `config.toml` from
    /// silently overriding an unset value in the child — the child's
    /// `parse_args_with_config` uses `CLI flag > config.toml > clap default`,
    /// so an explicit flag here wins. `resolve_*_arg` collapse empty/unknown
    /// values to `auto` so the child never receives a value it cannot dispatch.
    fn push_codec_decoder_args(&self, cmd: &mut std::process::Command) {
        cmd.arg("--decoder")
            .arg(resolve_decoder_arg(&self.peer_decoder));
        cmd.arg("--codec").arg(resolve_codec_arg(&self.peer_codec));
    }

    /// Spawn the built viewer command and, on success, adopt it as the single
    /// tracked outbound session (AC-5改 / AC-10). Callers must have already
    /// checked `viewer_session.is_none()`.
    fn launch_viewer(
        &mut self,
        mut cmd: std::process::Command,
        target: String,
        recent: RecentUpsert,
    ) {
        match cmd.spawn() {
            Ok(child) => {
                let pid = child.id();
                self.viewer_session = Some(ViewerSession {
                    child,
                    pid,
                    target,
                    started_at: Instant::now(),
                });
                // The live phase now renders from `viewer_session`; drop any
                // stale terminal line from a previous session.
                self.connect_status = None;
                self.connect_error = false;
                self.finalize_connect(recent);
            }
            Err(e) => {
                self.set_connect_status(format!("spawn failed: {e}"), true);
            }
        }
    }

    /// Poll the tracked viewer child without blocking (AC-5改). On exit, reap it
    /// (`try_wait` clears the zombie), record a terminal JA status, and clear
    /// the session so the Connect button re-enables. The host listener is never
    /// touched here: a viewer death must not stop hosting.
    fn drain_viewer_session(&mut self) {
        let Some(session) = self.viewer_session.as_mut() else {
            return;
        };
        // Resolve the child's state, then drop the `session` borrow before
        // touching other `self` fields below.
        let status = match session.child.try_wait() {
            Ok(Some(status)) => status,
            Ok(None) => return, // still running
            Err(e) => {
                // m2: try_wait itself failed. Best-effort reap (kill + wait)
                // before we drop the handle so we don't leak the child (a
                // zombie on Unix); then stop tracking so the UI can't get stuck
                // with a permanently-disabled Connect button.
                let _ = session.child.kill();
                let _ = session.child.wait();
                self.set_connect_status(format!("viewer wait failed: {e}"), true);
                self.viewer_session = None;
                return;
            }
        };
        if status.success() {
            self.set_connect_status(t!("home-connect-disconnected"), false);
        } else {
            let detail = match status.code() {
                Some(code) => t!("home-connect-exit-code", code => code.to_string()),
                None => t!("home-connect-exit-signal"),
            };
            self.set_connect_status(t!("home-connect-failed", detail => detail), true);
        }
        self.viewer_session = None;
    }

    /// AC-10 "切断": terminate and reap the tracked viewer child so a new
    /// session can start. Best-effort kill (the child may have just exited).
    fn disconnect_viewer(&mut self) {
        if let Some(mut session) = self.viewer_session.take() {
            let _ = session.child.kill();
            let _ = session.child.wait();
            self.set_connect_status(t!("home-connect-disconnected"), false);
        }
    }

    /// Set the Connect status line and its severity in one place.
    fn set_connect_status(&mut self, msg: String, is_error: bool) {
        self.connect_status = Some(msg);
        self.connect_error = is_error;
    }

    /// Persist codec/decoder selection and upsert the recent-connections entry
    /// in one config write.
    fn finalize_connect(&mut self, recent: RecentUpsert) {
        let now = SystemTime::now();
        let mut g = self.cfg.lock().unwrap();
        g.viewer.codec = self.peer_codec.clone();
        g.viewer.decoder = self.peer_decoder.clone();
        match recent {
            RecentUpsert::Signaling(id) => {
                if let Some(e) = g
                    .viewer
                    .hosts
                    .iter_mut()
                    .find(|e| e.mode == "signaling" && e.host_id == id)
                {
                    e.last_connected = now;
                } else {
                    g.viewer.hosts.push(HostEntry {
                        label: id.clone(),
                        mode: "signaling".into(),
                        addr: String::new(),
                        host_id: id,
                        pubkey: String::new(),
                        last_connected: now,
                        last_known_online: None,
                    });
                }
            }
            RecentUpsert::Direct { addr, pubkey } => {
                if let Some(e) = g
                    .viewer
                    .hosts
                    .iter_mut()
                    .find(|e| e.mode == "direct" && e.addr == addr)
                {
                    e.last_connected = now;
                    if !pubkey.is_empty() {
                        e.pubkey = pubkey;
                    }
                } else {
                    g.viewer.hosts.push(HostEntry {
                        label: addr.clone(),
                        mode: "direct".into(),
                        addr,
                        host_id: String::new(),
                        pubkey,
                        last_connected: now,
                        last_known_online: None,
                    });
                }
            }
        }
        let snapshot = g.clone();
        drop(g);
        if let Err(e) = snapshot.save(&self.config_path) {
            tracing::warn!(?e, "recents/config save failed");
        }
    }

    fn delete_recent(&mut self, target: &HostEntry) {
        let mut g = self.cfg.lock().unwrap();
        g.viewer.hosts.retain(|e| {
            !(e.mode == target.mode && e.host_id == target.host_id && e.addr == target.addr)
        });
        let snapshot = g.clone();
        drop(g);
        if let Err(e) = snapshot.save(&self.config_path) {
            tracing::warn!(?e, "recents delete save failed");
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

        // A changed signaling URL / key file must take effect for provisioning
        // without an app restart (AC-3). Reload the identity offline (no
        // network) so a subsequent Retry provisions against the new server.
        let config_root = self
            .host_auth_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let key_file = cfg_snapshot.host.key_file.clone();
        let signaling_url = cfg_snapshot.host.signaling_url.clone();
        let id_paths = IdentityPaths::from_config_root(&config_root, key_file);
        match DeviceIdentity::load_or_create(id_paths, &signaling_url) {
            Ok(id) => {
                self.identity = Some(id);
                self.identity_error = None;
                self.fingerprint_qr = None;
            }
            Err(e) => {
                self.identity_error = Some(t!("home-identity-error", error => e.to_string()));
            }
        }
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
        // Reap / update the tracked outbound viewer child (AC-5改) so its exit
        // surfaces as a status change instead of a silent orphan.
        self.drain_viewer_session();
        // Pick up a completed background provisioning attempt.
        self.drain_reprovision();

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

        // Repaint cadence: snappy while a provisioning attempt is in flight,
        // 1Hz while listening to keep state transitions visible, otherwise idle.
        let next = if self.reprovision_rx.is_some() {
            Duration::from_millis(200)
        } else if self.viewer_session.is_some() || self.is_listening() {
            // Keep polling the viewer child / listener so exit and the
            // connecting → running transition surface promptly.
            Duration::from_secs(1)
        } else {
            Duration::from_secs(2)
        };
        ctx.request_repaint_after(next);
    }
}

impl Drop for ClientApp {
    fn drop(&mut self) {
        // AC-5改: never leak an orphaned/zombie viewer child when the window
        // closes. `run_native` drops the app after the event loop ends, so this
        // is the reliable reap point. (A viewer session is deliberately *not*
        // preserved across a normal app close — that is the user quitting the
        // whole app, not the AC-11 host-elevation teardown we guard against.)
        if let Some(mut session) = self.viewer_session.take() {
            let _ = session.child.kill();
            let _ = session.child.wait();
        }
    }
}

impl ClientApp {
    fn draw_nav(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("\u{25cf}")
                    .color(tokens::ACCENT)
                    .size(9.0),
            );
            ui.heading("prdt");
        });
        ui.add_space(14.0);
        self.nav_entry(ui, View::Home, &t!("nav-home"));
        self.nav_entry(ui, View::Settings, &t!("nav-settings"));
        self.nav_entry(ui, View::Logs, &t!("nav-logs"));
    }

    /// A full-width selectable nav entry (Linear-style side rail): the active
    /// route gets a faint accent-tinted fill, an accent label, and a 3px accent
    /// bar down its left edge; inactive routes are quiet ghost rows.
    fn nav_entry(&mut self, ui: &mut egui::Ui, view: View, label: &str) {
        let selected = self.view == view;
        let fill = if selected {
            tokens::ACCENT_WEAK
        } else {
            egui::Color32::TRANSPARENT
        };
        let text_color = if selected {
            tokens::ACCENT
        } else {
            tokens::TEXT_DIM
        };
        let button = egui::Button::new(egui::RichText::new(label).color(text_color))
            .fill(fill)
            .stroke(egui::Stroke::NONE)
            .min_size(egui::vec2(ui.available_width(), 34.0));
        let resp = ui.add(button);
        if selected {
            let r = resp.rect;
            ui.painter().rect_filled(
                egui::Rect::from_min_size(r.left_top(), egui::vec2(3.0, r.height())),
                egui::CornerRadius::ZERO,
                tokens::ACCENT,
            );
        }
        if resp.clicked() {
            self.view = view;
        }
        ui.add_space(2.0);
    }

    fn draw_home(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(10.0);
                let card = |ui: &mut egui::Ui| {
                    egui::Frame::group(ui.style())
                        .fill(tokens::SURFACE)
                        .stroke(egui::Stroke::new(1.0, tokens::BORDER))
                        .corner_radius(egui::CornerRadius::same(tokens::RADIUS_CARD))
                        .inner_margin(egui::Margin::same(20))
                };
                ui.columns(2, |cols| {
                    card(&mut cols[0]).show(&mut cols[0], |ui| {
                        self.draw_this_device(ui);
                    });
                    card(&mut cols[1]).show(&mut cols[1], |ui| {
                        self.draw_connect(ui);
                    });
                });
            });
    }

    /// "This device" — server-allocated ID, fixed PIN, key fingerprint/QR, and
    /// the share start/stop control. Renders from an owned snapshot so the
    /// deferred `&mut self` actions (retry / regenerate / share) don't collide
    /// with the identity borrow.
    fn draw_this_device(&mut self, ui: &mut egui::Ui) {
        ui.heading(t!("home-this-device-title"));
        ui.add_space(12.0);

        if let Some(err) = self.identity_error.clone() {
            ui.colored_label(tokens::DESTRUCTIVE, err);
            ui.add_space(8.0);
            ui.colored_label(tokens::TEXT_DIM, t!("home-identity-error-hint"));
            return;
        }
        let (host_id, pin, fp_short) = match self.identity.as_ref() {
            Some(id) => (
                id.host_id().map(str::to_string),
                id.pin_plaintext().map(str::to_string),
                id.fingerprint_short().to_string(),
            ),
            None => return,
        };
        let signaling_configured = {
            let g = self.cfg.lock().unwrap();
            !g.host.signaling_url.trim().is_empty()
        };
        let busy = self.reprovision_rx.is_some();

        let mut do_copy_id: Option<String> = None;
        let mut do_retry = false;
        let mut do_toggle_pin = false;
        let mut do_regenerate = false;
        let mut do_start = false;
        let mut do_stop = false;

        // --- Device ID (AC-2 / AC-3) ---
        ui.label(dim_caption(t!("home-device-id-label")));
        match &host_id {
            Some(id) => {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(group_id(id))
                            .monospace()
                            .size(32.0)
                            .strong()
                            .color(tokens::TEXT),
                    );
                    ui.add_space(4.0);
                    if ui.button(t!("common-button-copy")).clicked() {
                        // Copy the raw (un-grouped) id so it pastes back cleanly.
                        do_copy_id = Some(id.clone());
                    }
                });
            }
            None => {
                let msg = if signaling_configured {
                    t!("home-unprovisioned")
                } else {
                    t!("home-unprovisioned-no-url")
                };
                ui.colored_label(tokens::WARN, msg);
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!busy && signaling_configured, |ui| {
                        if ui.button(t!("home-button-retry")).clicked() {
                            do_retry = true;
                        }
                    });
                    if busy {
                        ui.spinner();
                        ui.label(t!("home-provisioning"));
                    }
                });
            }
        }
        if let Some(s) = &self.provision_status {
            ui.add_space(4.0);
            ui.colored_label(tokens::TEXT_DIM, s);
        }

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(12.0);

        // --- Fixed PIN (AC-2) ---
        ui.label(dim_caption(t!("home-pin-label")));
        match &pin {
            Some(pin) => {
                ui.horizontal(|ui| {
                    let shown = if self.pin_revealed {
                        pin.clone()
                    } else {
                        "\u{2022}".repeat(pin.chars().count())
                    };
                    ui.label(
                        egui::RichText::new(shown)
                            .monospace()
                            .size(22.0)
                            .strong()
                            .color(tokens::TEXT),
                    );
                    let toggle = if self.pin_revealed {
                        t!("home-pin-hide")
                    } else {
                        t!("home-pin-show")
                    };
                    if ui.button(toggle).clicked() {
                        do_toggle_pin = true;
                    }
                    if ui.button(t!("home-button-regenerate")).clicked() {
                        do_regenerate = true;
                    }
                });
            }
            None => {
                ui.colored_label(tokens::TEXT_DIM, t!("home-pin-none"));
                if ui.button(t!("home-button-generate-pin")).clicked() {
                    do_regenerate = true;
                }
            }
        }

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(12.0);

        // --- Key fingerprint + QR (AC-14, out-of-band verification) ---
        ui.label(dim_caption(t!("home-fingerprint-label")));
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(fp_short.as_str())
                    .monospace()
                    .color(tokens::TEXT),
            );
            let qr_label = if self.show_fingerprint_qr {
                t!("home-button-hide-qr")
            } else {
                t!("home-button-show-qr")
            };
            if ui.button(qr_label).clicked() {
                self.show_fingerprint_qr = !self.show_fingerprint_qr;
            }
        });
        ui.colored_label(tokens::TEXT_DIM, t!("home-fingerprint-hint"));
        if self.show_fingerprint_qr {
            self.ensure_fingerprint_qr(ui.ctx());
            if let Some(qr) = &self.fingerprint_qr {
                ui.add_space(6.0);
                ui.image(egui::load::SizedTexture::new(qr.id(), qr.size_vec2()));
            }
        }

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(12.0);

        // --- Sharing (host listener) ---
        ui.label(dim_caption(t!("home-sharing-label")));
        let listening = self.is_listening();
        ui.horizontal(|ui| {
            if listening {
                ui.colored_label(tokens::OK, "\u{25cf}");
                ui.label(t!("home-sharing-on"));
            } else {
                ui.colored_label(tokens::TEXT_DIM, "\u{25cb}");
                ui.label(t!("home-sharing-off"));
            }
        });
        ui.add_space(6.0);
        if listening {
            let stop = egui::Button::new(
                egui::RichText::new(t!("home-button-stop-sharing")).color(tokens::TEXT),
            )
            .fill(tokens::DESTRUCTIVE);
            if ui.add(stop).clicked() {
                do_stop = true;
            }
        } else {
            let start = egui::Button::new(
                egui::RichText::new(t!("home-button-start-sharing")).color(tokens::BG_DEEP),
            )
            .fill(tokens::ACCENT);
            if ui.add(start).clicked() {
                do_start = true;
            }
        }
        if let Some(status) = &self.host_status {
            ui.add_space(8.0);
            if status.starts_with("listener error")
                || status.starts_with("cannot start")
                || status.starts_with("invalid host args")
            {
                ui.colored_label(tokens::DESTRUCTIVE, status);
            } else {
                ui.colored_label(tokens::TEXT_DIM, status);
            }
        }

        // Apply deferred actions (outside the render borrows above).
        if let Some(id) = do_copy_id {
            ui.ctx().copy_text(id);
        }
        if do_toggle_pin {
            self.pin_revealed = !self.pin_revealed;
        }
        if do_retry {
            self.start_reprovision();
        }
        if do_regenerate {
            self.regenerate_pin();
        }
        if do_start {
            self.start_listener();
        }
        if do_stop {
            self.stop_listener();
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

    /// "Connect to a device" — the easy path (peer ID + PIN), a collapsible
    /// Advanced section for legacy direct mode, and a recent-connections list.
    fn draw_connect(&mut self, ui: &mut egui::Ui) {
        ui.heading(t!("home-connect-title"));
        ui.add_space(12.0);

        // --- Easy path: peer ID + PIN (AC-1) ---
        ui.label(dim_caption(t!("home-peer-id-label")));
        ui.add(
            egui::TextEdit::singleline(&mut self.peer_id)
                .hint_text("123-456-789")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(8.0);
        ui.label(dim_caption(t!("home-peer-pin-label")));
        ui.add(
            egui::TextEdit::singleline(&mut self.peer_pin)
                .hint_text(t!("home-peer-pin-hint"))
                .password(true)
                .desired_width(f32::INFINITY),
        );

        ui.add_space(12.0);
        // AC-10: at most one outbound session. When one is live the primary
        // action becomes "切断" (disconnect) and a second connect is impossible.
        let session_active = self.viewer_session.is_some();
        let mut do_connect_signaling = false;
        let mut do_disconnect = false;
        if session_active {
            let disc = egui::Button::new(
                egui::RichText::new(t!("home-button-disconnect"))
                    .size(16.0)
                    .color(tokens::TEXT),
            )
            .fill(tokens::DESTRUCTIVE)
            .min_size(egui::vec2(ui.available_width(), 40.0));
            if ui.add(disc).clicked() {
                do_disconnect = true;
            }
        } else {
            let connect_btn = egui::Button::new(
                egui::RichText::new(t!("home-button-connect"))
                    .size(16.0)
                    .color(tokens::BG_DEEP),
            )
            .fill(tokens::ACCENT)
            .min_size(egui::vec2(ui.available_width(), 40.0));
            if ui.add(connect_btn).clicked() {
                do_connect_signaling = true;
            }
        }
        ui.add_space(4.0);
        ui.colored_label(tokens::TEXT_DIM, t!("home-connect-single-session-note"));

        // Status line: a live session shows its (heuristic) phase; otherwise the
        // last terminal / error message from the previous session or a guard.
        if let Some(session) = &self.viewer_session {
            ui.add_space(8.0);
            if session.is_connecting() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.colored_label(
                        tokens::TEXT_DIM,
                        t!("home-connect-connecting",
                            target => session.target.clone(),
                            pid => session.pid.to_string()),
                    );
                });
            } else {
                ui.colored_label(
                    tokens::OK,
                    t!("home-connect-active",
                        target => session.target.clone(),
                        pid => session.pid.to_string()),
                );
            }
        } else if let Some(status) = &self.connect_status {
            ui.add_space(8.0);
            let color = if self.connect_error {
                tokens::DESTRUCTIVE
            } else {
                tokens::TEXT_DIM
            };
            ui.colored_label(color, status);
        }

        ui.add_space(12.0);

        // --- Advanced: legacy direct mode (kept so nothing is lost) ---
        let mut do_connect_direct = false;
        egui::CollapsingHeader::new(t!("home-advanced"))
            .default_open(false)
            .show(ui, |ui| {
                ui.label(dim_caption(t!("home-advanced-host")));
                ui.add(
                    egui::TextEdit::singleline(&mut self.peer_host).desired_width(f32::INFINITY),
                );
                ui.add_space(4.0);
                ui.label(dim_caption(t!("home-advanced-pubkey")));
                ui.add(
                    egui::TextEdit::singleline(&mut self.peer_pubkey).desired_width(f32::INFINITY),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(t!("home-codec-label"));
                    egui::ComboBox::from_id_salt("connect-codec-combo")
                        .selected_text(&self.peer_codec)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.peer_codec, "auto".to_string(), "auto");
                            ui.selectable_value(&mut self.peer_codec, "h264".to_string(), "h264");
                            ui.selectable_value(&mut self.peer_codec, "h265".to_string(), "h265");
                        });
                });
                ui.horizontal(|ui| {
                    ui.label(t!("home-decoder-label"));
                    egui::ComboBox::from_id_salt("connect-decoder-combo")
                        .selected_text(&self.peer_decoder)
                        .show_ui(ui, |ui| {
                            for opt in prdt_viewer::supported_decoder_args() {
                                ui.selectable_value(&mut self.peer_decoder, opt.to_string(), opt);
                            }
                        });
                });
                ui.add_space(6.0);
                ui.add_enabled_ui(!session_active, |ui| {
                    if ui.button(t!("home-button-connect-direct")).clicked() {
                        do_connect_direct = true;
                    }
                });
            });

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);

        // --- Recent connections ---
        ui.label(dim_caption(t!("home-recent-title")));
        let recents: Vec<HostEntry> = {
            let g = self.cfg.lock().unwrap();
            let mut v = g.viewer.hosts.clone();
            v.sort_by_key(|e| std::cmp::Reverse(e.last_connected));
            v.truncate(6);
            v
        };
        let mut connect_recent: Option<HostEntry> = None;
        let mut delete_recent: Option<HostEntry> = None;
        if recents.is_empty() {
            ui.colored_label(tokens::TEXT_DIM, t!("home-recent-empty"));
        } else {
            for e in &recents {
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!session_active, |ui| {
                        if ui.button(t!("home-button-connect-short")).clicked() {
                            connect_recent = Some(e.clone());
                        }
                    });
                    let detail = if e.mode == "signaling" {
                        e.host_id.clone()
                    } else {
                        e.addr.clone()
                    };
                    ui.label(egui::RichText::new(detail).monospace().color(tokens::TEXT));
                    if ui.small_button(t!("home-recent-remove")).clicked() {
                        delete_recent = Some(e.clone());
                    }
                });
            }
        }

        // Apply deferred actions (outside the render borrows above).
        if do_disconnect {
            self.disconnect_viewer();
        }
        if do_connect_signaling {
            self.connect_signaling();
        }
        if do_connect_direct {
            self.connect_direct();
        }
        if let Some(e) = connect_recent {
            if e.mode == "signaling" {
                self.peer_id = e.host_id.clone();
                self.connect_signaling();
            } else {
                self.peer_host = e.addr.clone();
                self.peer_pubkey = e.pubkey.clone();
                self.connect_direct();
            }
        }
        if let Some(e) = delete_recent {
            self.delete_recent(&e);
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
                    ui.colored_label(
                        tokens::TEXT_DIM,
                        "A PIN is set (manage it from the Home screen).",
                    );
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

/// A faint, small eyebrow caption used to label each field group. Sits one
/// step quieter than body text so the value it introduces is what the eye
/// lands on.
fn dim_caption(text: String) -> egui::RichText {
    egui::RichText::new(text).color(tokens::TEXT_FAINT).small()
}

/// Group a 9-digit device ID into `123 456 789` for legibility. Non-9-digit
/// ids (or anything non-numeric) pass through unchanged.
fn group_id(id: &str) -> String {
    if id.len() == 9 && id.bytes().all(|b| b.is_ascii_digit()) {
        format!("{} {} {}", &id[0..3], &id[3..6], &id[6..9])
    } else {
        id.to_string()
    }
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
    fn resolve_decoder_arg_falls_back_to_auto() {
        // Empty / whitespace / unknown selections must never reach the child
        // verbatim — they collapse to `auto` (#11a).
        assert_eq!(resolve_decoder_arg(""), "auto");
        assert_eq!(resolve_decoder_arg("   "), "auto");
        assert_eq!(resolve_decoder_arg("definitely-not-a-decoder"), "auto");
        // `auto` is always a supported value on every build.
        assert_eq!(resolve_decoder_arg("auto"), "auto");
        // The stale-config default (`decoder = "nvdec"`) is only forwarded when
        // this build actually compiled that backend in; otherwise it collapses
        // to `auto` rather than crashing the child at decoder dispatch.
        let supported = prdt_viewer::supported_decoder_args();
        if supported.iter().any(|d| *d == "nvdec") {
            assert_eq!(resolve_decoder_arg("nvdec"), "nvdec");
        } else {
            assert_eq!(resolve_decoder_arg("nvdec"), "auto");
        }
        // Every advertised option round-trips unchanged.
        for opt in supported {
            assert_eq!(resolve_decoder_arg(opt), opt);
        }
    }

    #[test]
    fn resolve_codec_arg_only_accepts_known_values() {
        assert_eq!(resolve_codec_arg("auto"), "auto");
        assert_eq!(resolve_codec_arg("h264"), "h264");
        assert_eq!(resolve_codec_arg("h265"), "h265");
        assert_eq!(resolve_codec_arg(" h265 "), "h265");
        assert_eq!(resolve_codec_arg(""), "auto");
        assert_eq!(resolve_codec_arg("h266"), "auto");
    }
}
