mod platform;
mod status;
mod watchdog;

use std::fs;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use platform::{
    build_video_producer, clipboard_sequence_number, dispatch_input, output_display_name,
    pick_default_output, probe as platform_probe, factory as platform_factory,
    read_clipboard_text, virtual_desktop_rect, write_clipboard_text,
    MAX_CLIPBOARD_BYTES,
};
use prdt_audio::{LoopbackCapture, OpusEncoder};
use prdt_crypto::KeyPair;
use prdt_filetransfer::{send_file, TransferReceiver, DEFAULT_MAX_TRANSFER_BYTES};
use prdt_protocol::{wire::AudioPacket, Codec, ControlMessage, MonitorRect};

use prdt_transport::{
    host_handshake, now_monotonic_us, CustomUdpTransport, ReceivedMessage, Transport,
    UdpTransportConfig,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use status::SharedStatus;

/// Returns the default path for the host's long-term private key file.
/// Prefers the OS-conventional data directory (`%APPDATA%\prdt\` on
/// Windows, `~/.local/share/prdt/` on Linux, `~/Library/Application
/// Support/prdt/` on macOS) and creates the directory on demand. Falls
/// back to `host-key.bin` in the current working directory if no data
/// dir is available (rare — typically only on stripped-down systems).
pub fn default_host_key_path() -> std::path::PathBuf {
    if let Some(base) = dirs::data_local_dir() {
        let dir = base.join("prdt");
        let _ = std::fs::create_dir_all(&dir);
        return dir.join("host-key.bin");
    }
    std::path::PathBuf::from("host-key.bin")
}

const FILE_RECV_DIR: &str = "prdt-received";
const FILE_SEND_DIR: &str = "prdt-outgoing";
const FILE_SEND_SENT_SUBDIR: &str = "sent";
const OUTGOING_POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Parser, Debug, Clone)]
#[command(
    name = "prdt-host",
    about = "power-remote-dt host (capture + encode + input inject)"
)]
pub struct Args {
    /// Local bind address, e.g. 0.0.0.0:9000.
    #[arg(long, default_value = "0.0.0.0:9000")]
    bind: SocketAddr,

    /// Monitor output index (from enumerate_outputs).
    #[arg(long, default_value_t = 0u32)]
    monitor: u32,

    /// Target bitrate in Mbps (e.g., 30 for 30 Mbps).
    #[arg(long, default_value_t = 30u32)]
    bitrate_mbps: u32,

    /// Path to host's long-term private key file (32 bytes). Generated on
    /// first run if the file doesn't exist; print the public key to stdout
    /// so the viewer can pin it via `--host-pubkey`.
    #[arg(long, default_value_os_t = default_host_key_path())]
    pub key_file: std::path::PathBuf,

    /// Directory the host watches for outgoing files. Any file dropped into
    /// this dir is streamed to the connected viewer and then moved to
    /// `<outgoing_dir>/sent/` so it isn't sent twice. Created on demand.
    #[arg(long, default_value = FILE_SEND_DIR)]
    outgoing_dir: std::path::PathBuf,

    /// Rendezvous via a signaling server instead of listening for a direct viewer.
    #[arg(long)]
    signaling_url: Option<url::Url>,

    /// Opaque host identifier to register with the signaling server.
    /// Required when --signaling-url is specified.
    #[arg(long, required = false)]
    host_id: Option<String>,

    /// Path to persist the signaling-server-allocated host ID. Created on
    /// first successful register; read on subsequent starts.
    #[arg(long, default_value = "host-id.txt")]
    host_id_file: std::path::PathBuf,

    /// Rendezvous overall timeout in seconds.
    #[arg(long, default_value_t = 10)]
    signaling_timeout: u64,

    /// STUN server URL (e.g. stun://stun.l.google.com:19302). Optional.
    /// When set together with --signaling-url, the host learns its public
    /// addr and sends it alongside the LAN Host candidate.
    #[arg(long)]
    stun_url: Option<url::Url>,

    /// TURN server URL (turn://user:pass@host:port). Optional. When set,
    /// transport is built via bind_with_relay (TURN relay mode) and the
    /// signaling-client emits a Relay candidate.
    #[arg(long)]
    turn_url: Option<url::Url>,

    /// Encoder backend: auto (default) | nvenc | mf | openh264.
    /// "auto" picks the best available: nvenc > mf > openh264. On NVIDIA
    /// boxes nvenc wins; on Intel/AMD it falls back to the MF H.265 MFT;
    /// if neither is available the cross-platform OpenH264 software path
    /// kicks in (advertises H.264 in HelloAck instead of H.265).
    /// Specifying a non-"auto" value enables Strict mode (no failover).
    #[arg(long, default_value = "auto")]
    encoder: String,

    /// Soft hint: prefer this backend if available, but failover is still
    /// allowed. Mutually informative with --encoder; ignored when --encoder
    /// is not "auto". Valid values: nvenc | mf | openh264.
    #[arg(long)]
    encoder_hint: Option<String>,

    /// Shorthand for --encoder openh264. Convenient for support cases where
    /// a software-only path is needed without remembering the exact backend name.
    #[arg(long, default_value_t = false)]
    force_sw: bool,

    /// Run in CLI-only mode without launching the GUI. Required for headless servers / CI.
    #[arg(long)]
    headless: bool,

    /// Override the GUI config file location (default: %APPDATA%/prdt/config.toml).
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Path to known-peer-ids file. Each line: `<label> <pubkey-b64>`. Peers
    /// listed here connect silently; unknown peers are prompted via GUI or
    /// rejected in headless mode. Created on first GUI-accepted unknown peer.
    #[arg(long, default_value = "known-peer-ids")]
    pub known_peers_file: std::path::PathBuf,

    /// Disable the consent gate entirely: every successful Noise handshake is
    /// accepted regardless of known-peer-ids contents, with no GUI prompt and
    /// no persistence. Intended for CI / scripted setups where the operator
    /// has out-of-band confidence in who can reach the bind address.
    /// SECURITY: anyone who can complete the Noise handshake (i.e. anyone
    /// with a viewer key — by default, anyone) gets in. Use only on isolated
    /// networks or behind another auth layer.
    #[arg(long)]
    pub silent_allow: bool,
}

#[derive(Debug)]
pub struct ConsentRequest {
    pub peer_pubkey: prdt_crypto::PubKey,
    pub responder: tokio::sync::oneshot::Sender<ConsentDecision>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentDecision {
    Accept,
    Reject,
}

pub type ConsentSender = tokio::sync::mpsc::UnboundedSender<ConsentRequest>;

pub async fn run_host(
    args: Args,
    _status: Option<SharedStatus>,
    consent_tx: Option<ConsentSender>,
    _cancel: CancellationToken,
) -> Result<()> {
    // Load or generate the host keypair.
    let keypair = if args.key_file.exists() {
        let priv_bytes = fs::read(&args.key_file)
            .context(format!("read key file {}", args.key_file.display()))?;
        if priv_bytes.len() != 32 {
            anyhow::bail!(
                "key file must be exactly 32 bytes, got {}",
                priv_bytes.len()
            );
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&priv_bytes);
        KeyPair::from_private(arr)
    } else {
        tracing::info!(path = %args.key_file.display(), "generating new host key");
        let kp = KeyPair::generate();
        fs::write(&args.key_file, kp.private.0)
            .context(format!("write key file {}", args.key_file.display()))?;
        kp
    };
    println!("Host public key: {}", keypair.public.to_base64());
    println!(
        "(Pass --host-pubkey {} to the viewer)",
        keypair.public.to_base64()
    );

    let output = pick_default_output(&args).context("pick_default_output")?;

    info!(
        monitor = args.monitor,
        device_name = output_display_name(&output),
        bitrate_mbps = args.bitrate_mbps,
        encoder = %args.encoder,
        "host starting"
    );

    // Bind UDP first; wait for viewer to say Hello.
    let cfg = UdpTransportConfig {
        session_id: 0, // client picks
        ..Default::default()
    };

    // If --bind's IP is wildcard (0.0.0.0 or ::) and we're in signaling mode,
    // auto-detect the outbound interface the kernel would use to reach the
    // signaling server. This avoids the operator having to hand the host its
    // LAN IP explicitly. Direct mode has no URL to probe, so we keep the
    // user-supplied wildcard (the transport binds to all interfaces, which
    // is fine for server-side listen, but the Host candidate we emit won't
    // be used in direct mode anyway).
    let effective_bind = if args.bind.ip().is_unspecified() {
        if let Some(url) = args.signaling_url.as_ref() {
            match prdt_signaling_client::discover_outbound_ip(url).await {
                Ok(ip) => {
                    let new_bind = SocketAddr::new(ip, args.bind.port());
                    info!(orig = %args.bind, new = %new_bind, "host auto-detected LAN bind IP via signaling URL");
                    new_bind
                }
                Err(e) => {
                    tracing::warn!(error = %e, "outbound IP discovery failed; keeping wildcard bind (Host candidate may be unroutable)");
                    args.bind
                }
            }
        } else {
            args.bind
        }
    } else {
        args.bind
    };

    let transport = Arc::new(if let Some(url) = args.turn_url.clone() {
        let turn_cfg = prdt_nat_traversal::TurnConfig::from_url(&url)
            .await
            .context("parse turn URL")?;
        CustomUdpTransport::bind_with_relay(effective_bind, cfg, turn_cfg)
            .await
            .context("UDP bind with TURN relay")?
    } else {
        CustomUdpTransport::bind(effective_bind, cfg)
            .await
            .context("UDP bind")?
    });
    let local_udp = transport.local_addr()?;
    info!(local = ?local_udp, "UDP bound");

    if let Some(signaling_url) = args.signaling_url.clone() {
        // Priority: explicit --host-id > persisted host-id.txt > empty (triggers allocation)
        let effective_host_id = match &args.host_id {
            Some(id) => id.clone(),
            None => std::fs::read_to_string(&args.host_id_file)
                .ok()
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
        };
        let outcome = prdt_signaling_client::rendezvous_as_host(
            prdt_signaling_client::RendezvousConfig {
                url: signaling_url,
                host_id: effective_host_id.clone(),
                timeout: Duration::from_secs(args.signaling_timeout),
                stun_url: args.stun_url.clone(),
                turn_url: args.turn_url.clone(),
                aggregation_window:
                    prdt_signaling_client::RendezvousConfig::DEFAULT_AGGREGATION_WINDOW,
            },
            prdt_signaling_client::HostIdentity {
                pubkey_b64: keypair.public.to_base64(),
            },
            local_udp,
        )
        .await
        .context("signaling rendezvous (host)")?;
        if outcome.allocated_host_id != effective_host_id {
            if let Err(e) = std::fs::write(&args.host_id_file, &outcome.allocated_host_id) {
                tracing::warn!(error = %e, path = %args.host_id_file.display(), "failed to persist host_id");
            } else {
                tracing::info!(host_id = %outcome.allocated_host_id, path = %args.host_id_file.display(), "persisted host_id");
            }
        }
        let cand_addrs: Vec<SocketAddr> = outcome
            .peer_candidates
            .iter()
            .filter_map(|c| format!("{}:{}", c.ip, c.port).parse().ok())
            .collect();
        info!(
            session_id = %outcome.session_id,
            host_id = %outcome.allocated_host_id,
            candidate_count = cand_addrs.len(),
            "signaling_rendezvous_completed"
        );
        let peer_addr = transport
            .probe_and_commit_peer(&cand_addrs, Duration::from_secs(10))
            .await
            .context("probe_and_commit_peer")?;
        info!(%peer_addr, "probe selected winner");
    } else {
        info!("no --signaling-url; using LAN fixed-address mode");
    }

    loop {
        transport.reset_session().await;

        info!("waiting for Noise handshake");
        let peer_pubkey = match transport.handshake_as_server(&keypair).await {
            Ok(pk) => pk,
            Err(e) => {
                warn!(?e, "Noise server handshake failed; resetting session");
                continue;
            }
        };
        info!(
            peer = %peer_pubkey.to_base64(),
            "Noise handshake complete — encrypted channel established"
        );

        // Consent gate: known-peer-ids check + optional GUI prompt. Bypassed
        // when --silent-allow is set (CI / scripted use only — see Args docs).
        if args.silent_allow {
            info!(
                peer=%peer_pubkey.to_base64(),
                "silent-allow enabled; skipping consent gate"
            );
        } else {
            let known = match prdt_crypto::KnownPeers::load_or_default(&args.known_peers_file) {
                Ok(k) => k,
                Err(e) => {
                    warn!(?e, path=?args.known_peers_file, "failed to load known-peer-ids; rejecting session");
                    continue;
                }
            };
            if !known.contains(&peer_pubkey) {
                let decision = match &consent_tx {
                    Some(tx) => {
                        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                        let req = ConsentRequest {
                            peer_pubkey,
                            responder: resp_tx,
                        };
                        if tx.send(req).is_err() {
                            warn!("consent channel closed; rejecting unknown peer");
                            continue;
                        }
                        match resp_rx.await {
                            Ok(d) => d,
                            Err(_) => {
                                warn!("consent responder dropped; rejecting unknown peer");
                                continue;
                            }
                        }
                    }
                    None => {
                        warn!(
                            peer=%peer_pubkey.to_base64(),
                            "unknown peer connected and no consent channel (headless without --silent-allow); rejecting"
                        );
                        continue;
                    }
                };
                match decision {
                    ConsentDecision::Reject => {
                        info!(peer=%peer_pubkey.to_base64(), "consent rejected; resetting session");
                        continue;
                    }
                    ConsentDecision::Accept => {
                        // Persist so future connections from this peer are silent.
                        let mut updated = known;
                        updated.insert(peer_pubkey, peer_pubkey.to_base64());
                        if let Err(e) = updated.save(&args.known_peers_file) {
                            warn!(
                                ?e,
                                path=?args.known_peers_file,
                                "failed to persist known-peer-ids; session continues but won't be remembered"
                            );
                        } else {
                            info!(
                                peer=%peer_pubkey.to_base64(),
                                path=?args.known_peers_file,
                                "added peer to known-peer-ids"
                            );
                        }
                    }
                }
            } else {
                info!(peer=%peer_pubkey.to_base64(), "peer is in known-peer-ids; auto-accepted");
            }
        } // end of !silent_allow branch

        // Wait for Hello, send HelloAck. Session ID is random per host start so
        // a reconnect from a viewer that had the old ID cached gets treated as a
        // fresh session (no stale seq expectations from an earlier run).
        let session_id: u64 = {
            use rand_core::{OsRng, RngCore};
            let mut buf = [0u8; 8];
            OsRng.fill_bytes(&mut buf);
            u64::from_le_bytes(buf)
        };
        let bitrate_bps = args.bitrate_mbps.saturating_mul(1_000_000);
        let vd_rect = virtual_desktop_rect();
        // On Windows the per-monitor rect is the selected DXGI output;
        // on Linux (single-monitor only per spec §3) it's the virtual
        // desktop rect.
        #[cfg(windows)]
        let monitor_rect = MonitorRect::new(
            output.desktop_rect.left,
            output.desktop_rect.top,
            output.desktop_rect.right,
            output.desktop_rect.bottom,
        );
        #[cfg(target_os = "linux")]
        let monitor_rect = {
            // Match the X11 capturer's clamp (encoder max 3840x2160 on
            // multi-monitor WSLg). Without this the viewer scales mouse
            // input to a rect bigger than what the host actually captures.
            use prdt_media_linux::x11_capture::{MAX_CAPTURE_H, MAX_CAPTURE_W};
            let clipped_right = vd_rect
                .left
                .saturating_add(((vd_rect.right - vd_rect.left) as u32).min(MAX_CAPTURE_W) as i32);
            let clipped_bottom = vd_rect
                .top
                .saturating_add(((vd_rect.bottom - vd_rect.top) as u32).min(MAX_CAPTURE_H) as i32);
            MonitorRect::new(vd_rect.left, vd_rect.top, clipped_right, clipped_bottom)
        };
        info!(
            monitor = ?monitor_rect,
            virtual_desktop = ?vd_rect,
            "advertising desktop geometry to viewer",
        );
        // Codec advertisement: Windows queries the GPU adapter to decide
        // between HW (H.265) and SW (H.264); Linux is SW-only (H.264).
        #[cfg(windows)]
        let host_supported = {
            let adapter = prdt_media_win::pick_default_adapter()
                .context("pick_default_adapter for codec advertisement")?;
            crate::platform::win::supported_codecs_for_encoder_arg(&args.encoder, &adapter)
        };
        #[cfg(target_os = "linux")]
        let host_supported: Vec<Codec> = vec![Codec::H264];
        let req = match host_handshake(
            &*transport,
            session_id,
            now_monotonic_us(),
            bitrate_bps,
            monitor_rect,
            vd_rect,
            &host_supported,
            Duration::from_secs(60),
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(?e, "host_handshake failed; resetting session");
                continue;
            }
        };
        info!(?req, "handshake complete");

        // P5A: Parse --encoder-hint and --force-sw into policy types.
        let (user_override, _encoder_strict) = match args.encoder.as_str() {
            "auto" => (None, false),
            "nvenc" => (Some(prdt_media_policy::BackendKind::Nvenc), true),
            "mf" => (Some(prdt_media_policy::BackendKind::MfHevc), true),
            "openh264" => (Some(prdt_media_policy::BackendKind::Openh264), true),
            other => {
                warn!(encoder = %other, "unknown --encoder value; treating as auto");
                (None, false)
            }
        };
        let user_hint = args.encoder_hint.as_deref().and_then(|s| match s {
            "nvenc" => Some(prdt_media_policy::BackendKind::Nvenc),
            "mf" | "mf-hevc" => Some(prdt_media_policy::BackendKind::MfHevc),
            "openh264" => Some(prdt_media_policy::BackendKind::Openh264),
            other => {
                warn!(encoder_hint = %other, "unknown --encoder-hint value; ignoring");
                None
            }
        });
        // --force-sw overrides user_override to Openh264 if set.
        let (user_override, force_sw) = if args.force_sw {
            (Some(prdt_media_policy::BackendKind::Openh264), true)
        } else {
            (user_override, false)
        };

        // P5A policy: probe available backends and log the ranked order for
        // observability. Actual producer construction still uses the legacy
        // build_video_producer path (Windows factory wiring deferred to P5C;
        // Linux factory is wired but PolicyDriven::bootstrap is attempted first).
        let policy_codec = to_policy_codec(req.codec);
        let policy_ctx = prdt_media_policy::PolicyContext {
            target_resolution: (1920, 1080), // advisory; host derives real dims from output
            target_fps: 60,
            target_bitrate_bps: bitrate_bps,
            codec: policy_codec,
            user_override,
            user_hint,
            force_sw,
        };
        let probe_arc = platform_probe();
        let factory_arc = platform_factory();
        let scoring_policy: std::sync::Arc<dyn prdt_media_policy::SelectionPolicy> =
            std::sync::Arc::new(prdt_media_policy::ScoringPolicy::load_default_or_fallback());
        {
            let caps = probe_arc.list_encoders();
            let ranked = scoring_policy.rank(&caps, &policy_ctx, &prdt_media_policy::HistoryTable::new());
            info!(
                ranked = ?ranked,
                user_override = ?user_override,
                user_hint = ?user_hint,
                force_sw,
                "P5A policy ranked backends"
            );
        }

        // Attempt PolicyDriven::bootstrap (works on Linux where LinuxSwFactory
        // can actually create a producer; on Windows the factory stubs out and
        // bootstrap will return Err, falling through to the legacy path below).
        let policy_cfg = prdt_media_policy::ProducerConfig {
            width: 1920,   // advisory — Linux factory reads real dims from X11
            height: 1080,  // advisory
            fps: 60,
            initial_bitrate_bps: bitrate_bps,
            codec: policy_codec,
        };
        let policy_producer = prdt_media_policy::PolicyDriven::bootstrap(
            std::sync::Arc::clone(&probe_arc),
            std::sync::Arc::clone(&factory_arc),
            std::sync::Arc::clone(&scoring_policy),
            policy_cfg,
            policy_ctx,
        );

        let mut producer: Box<dyn prdt_protocol::VideoProducer> = match policy_producer {
            Ok(pd) => {
                let boxed: Box<dyn prdt_protocol::VideoProducer> = Box::new(pd);
                info!(
                    backend = boxed.backend_name(),
                    codec = req.codec.name(),
                    "PolicyDriven bootstrap succeeded; using policy-driven producer"
                );
                boxed
            }
            Err(bootstrap_err) => {
                // Windows factory stubs out → expected Err; fall back to legacy path.
                debug!(error = %bootstrap_err, "PolicyDriven bootstrap deferred; using legacy build_video_producer");
                build_video_producer(
                    &args.encoder,
                    &output,
                    bitrate_bps,
                    60, // TODO(L2): thread fps from Args::fps when --fps flag is added
                    req.codec,
                )
                .context("build_video_producer")?
            }
        };
        info!(
            backend = producer.backend_name(),
            codec = req.codec.name(),
            "encoder ready"
        );

        let cancel = CancellationToken::new();
        let last_keepalive = Arc::new(AtomicU64::new(now_monotonic_us()));
        // Shared flag: control loop sets this when viewer requests an IDR;
        // video loop reads+clears it before each encode call.
        // Mirrors last_keepalive: Arc<AtomicU64> (same task-safety pattern).
        let force_idr_flag = Arc::new(AtomicBool::new(false));
        // L3 adaptive bitrate channel: control loop forwards viewer's
        // SetBitrate target_bps; video loop drains to latest before each
        // next_frame() and calls producer.set_target_bitrate(). Unbounded
        // because messages are tiny u32s at ~1 Hz, far below memory pressure.
        let (bitrate_tx, mut bitrate_rx) = tokio::sync::mpsc::unbounded_channel::<u32>();

        // Spawn video loop. `handshake_complete_at` anchors the first-frame-latency
        // measurement (Phase 4 acceptance: ≤ 500ms max-of-20 cold-start).
        let tx_video = Arc::clone(&transport);
        let cancel_video = cancel.clone();
        let cancel_video_propagate = cancel.clone();
        let video_force_idr = Arc::clone(&force_idr_flag);
        let handshake_complete_at = std::time::Instant::now();
        let video = tokio::spawn(async move {
            let mut frames_sent = 0u64;
            let mut send_errors = 0u64;
            // L4: 1-second window byte counter so smoke can verify
            // "encoder actually shrunk frames" alongside L3's target_bps log.
            let mut bytes_sent_window: u64 = 0;
            let mut last_log = std::time::Instant::now();
            let mut first_frame_logged = false;
            loop {
                tokio::select! {
                    _ = cancel_video.cancelled() => break,
                    _ = async {
                        // L3: drain bitrate channel to newest, apply to encoder.
                        let mut latest_bps: Option<u32> = None;
                        while let Ok(bps) = bitrate_rx.try_recv() {
                            latest_bps = Some(bps);
                        }
                        if let Some(bps) = latest_bps {
                            producer.set_target_bitrate(bps);
                            debug!(target_bps = bps, "applied viewer-requested bitrate");
                        }
                        if video_force_idr.swap(false, Ordering::AcqRel) {
                            producer.request_idr();
                            info!("viewer requested IDR; producer.request_idr() called");
                        }
                        match producer.next_frame().await {
                            Ok(frame) => {
                                if !first_frame_logged {
                                    let elapsed_ms = handshake_complete_at.elapsed().as_millis();
                                    info!(elapsed_ms = elapsed_ms as u64, "first frame ready");
                                    first_frame_logged = true;
                                }
                                let nal_len = frame.nal_units.len();
                                let is_kf = frame.is_keyframe;
                                let bytes_in_frame = frame.nal_units.len() as u64;
                                if let Err(e) = tx_video.send_video(frame).await {
                                    send_errors += 1;
                                    warn!(?e, nal_len, is_kf, "send_video error; continuing");
                                } else {
                                    frames_sent += 1;
                                    bytes_sent_window += bytes_in_frame;
                                }
                                if last_log.elapsed() >= std::time::Duration::from_secs(1) {
                                    info!(frames_sent, send_errors, bytes_sent_window, "host tx stats");
                                    bytes_sent_window = 0;
                                    last_log = std::time::Instant::now();
                                }
                            }
                            Err(e) => {
                                match &e {
                                    prdt_protocol::ProducerError::DeviceLost { backend, reason } => {
                                        warn!(
                                            backend = %backend,
                                            reason = %reason,
                                            "backend reported device lost; PolicyDriven handles failover internally"
                                        );
                                    }
                                    other => warn!(?other, "producer error; continuing"),
                                }
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                        }
                    } => {}
                }
            }
            cancel_video_propagate.cancel();
        });

        // Spawn audio capture + encode + send loop. If the default output device
        // isn't 48kHz stereo (or loopback fails for any other reason) we log and
        // skip audio — video/input continue normally.
        //
        // `LoopbackCapture` wraps a `cpal::Stream` which is `!Send` on Windows
        // (WASAPI streams are bound to the creating thread via COM), so it lives
        // on a dedicated OS thread. The thread hands PCM frames over to the
        // async encode/send task via a tokio mpsc.
        let (pcm_async_tx, mut pcm_async_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<f32>>();
        std::thread::Builder::new()
            .name("prdt-host-audio-capture".into())
            .spawn(move || match LoopbackCapture::start() {
                Ok((cap, mut pcm_rx)) => {
                    // Keep the capture stream alive for the thread's lifetime.
                    let _cap = cap;
                    // Bridge the std-thread-owned blocking receiver to the async
                    // side. The cpal callback sends into a tokio UnboundedReceiver
                    // via `unbounded_send`, which doesn't require a runtime, so we
                    // can block_recv and forward.
                    while let Some(frame) = pcm_rx.blocking_recv() {
                        if pcm_async_tx.send(frame).is_err() {
                            break; // async side gone
                        }
                    }
                }
                Err(e) => {
                    warn!(?e, "audio capture failed; skipping audio");
                }
            })
            .expect("spawn audio capture thread");

        let audio_transport = Arc::clone(&transport);
        let cancel_audio = cancel.clone();
        // Audio is optional. Its failure must NOT cancel the session — video +
        // input must continue. (Pre-L1.5b this code propagated cancel; that
        // killed every WSLg session because ALSA has no default device there.)
        let audio_task = tokio::spawn(async move {
            let mut encoder = match OpusEncoder::new() {
                Ok(e) => e,
                Err(e) => {
                    warn!(?e, "opus encoder init failed; continuing without audio");
                    return;
                }
            };
            let epoch = std::time::Instant::now();
            let mut seq = 0u64;
            loop {
                tokio::select! {
                    _ = cancel_audio.cancelled() => break,
                    msg = pcm_async_rx.recv() => {
                        match msg {
                            Some(frame) => {
                                let opus_bytes = match encoder.encode(&frame) {
                                    Ok(b) => b,
                                    Err(e) => {
                                        warn!(?e, "opus encode");
                                        continue;
                                    }
                                };
                                seq += 1;
                                let pkt = AudioPacket {
                                    seq,
                                    timestamp_us: epoch.elapsed().as_micros() as u64,
                                    opus_bytes,
                                };
                                if let Err(e) = audio_transport.send_audio(pkt).await {
                                    warn!(?e, "send_audio");
                                }
                            }
                            None => break, // channel closed (e.g. capture init failed); exit silently
                        }
                    }
                }
            }
        });

        // Shared "last clipboard text we received from peer" — used by the
        // clipboard watcher to avoid echoing remote updates back to the peer.
        let last_remote_clipboard: Arc<tokio::sync::Mutex<Option<String>>> =
            Arc::new(tokio::sync::Mutex::new(None));

        // Spawn input injection loop.
        let rx_input = Arc::clone(&transport);
        let input_last_remote = Arc::clone(&last_remote_clipboard);
        let cancel_input = cancel.clone();
        let cancel_input_propagate = cancel.clone();
        let last_ka_input = Arc::clone(&last_keepalive);
        let input_force_idr = Arc::clone(&force_idr_flag);
        let host_max_bps = args.bitrate_mbps.saturating_mul(1_000_000);
        let input = tokio::spawn(async move {
            let mut ft_rx = TransferReceiver::new(FILE_RECV_DIR, DEFAULT_MAX_TRANSFER_BYTES);
            loop {
                tokio::select! {
                    _ = cancel_input.cancelled() => break,
                    msg = rx_input.recv() => {
                        match msg {
                            Ok(ReceivedMessage::Input(ev)) => {
                                if let Err(e) = dispatch_input(ev) {
                                    warn!(error = %e, "inject error");
                                }
                            }
                            Ok(ReceivedMessage::Control(ControlMessage::KeepAlive)) => {
                                last_ka_input.store(now_monotonic_us(), Ordering::Relaxed);
                            }
                            Ok(ReceivedMessage::Control(ControlMessage::ClipboardText { text })) => {
                                // Remember this text so the watcher loop doesn't echo it back.
                                *input_last_remote.lock().await = Some(text.clone());
                                if let Err(e) = write_clipboard_text(&text) {
                                    warn!(?e, "write_clipboard_text failed");
                                }
                            }
                            Ok(ReceivedMessage::Control(ControlMessage::Bye)) => {
                                info!("peer sent Bye");
                                break;
                            }
                            Ok(ReceivedMessage::Control(ControlMessage::LatencyReport {
                                samples,
                                arrival_p50_us,
                                arrival_p95_us,
                                decode_p50_us,
                                decode_p95_us,
                                present_p50_us,
                                present_p95_us,
                                present_p99_us,
                            })) => {
                                info!(
                                    samples,
                                    arrival_p50_us,
                                    arrival_p95_us,
                                    decode_p50_us,
                                    decode_p95_us,
                                    present_p50_us,
                                    present_p95_us,
                                    present_p99_us,
                                    "viewer latency report",
                                );
                            }
                            Ok(ReceivedMessage::Control(ControlMessage::RequestIdr)) => {
                                info!("viewer requested IDR; setting force_idr for next encode");
                                input_force_idr.store(true, Ordering::Release);
                            }
                            Ok(ReceivedMessage::Control(ControlMessage::SetBitrate {
                                target_bps,
                            })) => {
                                const HOST_MIN_BPS: u32 = 1_000_000;
                                let clamped = target_bps.clamp(HOST_MIN_BPS, host_max_bps);
                                if clamped != target_bps {
                                    warn!(
                                        target_bps,
                                        clamped,
                                        host_max_bps,
                                        "viewer SetBitrate out of host range; clamping"
                                    );
                                }
                                info!(
                                    target_bps = clamped,
                                    "viewer requested bitrate change"
                                );
                                let _ = bitrate_tx.send(clamped);
                            }
                            Ok(ReceivedMessage::Control(msg)) => {
                                let _ = ft_rx.handle(msg);
                            }
                            Ok(_) => {}
                            Err(e) => {
                                warn!(?e, "recv error");
                                break;
                            }
                        }
                    }
                }
            }
            cancel_input_propagate.cancel();
        });

        // Spawn clipboard watcher. We poll `GetClipboardSequenceNumber` at 50ms
        // which is cheap (no OpenClipboard handshake, no text copy), and only
        // actually read the clipboard when the sequence counter moves. This
        // drops copy-paste lag from the old 500ms polling interval while
        // keeping CPU use minimal when the clipboard is idle.
        let clip_transport = Arc::clone(&transport);
        let clip_last_remote = Arc::clone(&last_remote_clipboard);
        let cancel_clip = cancel.clone();
        let cancel_clip_propagate = cancel.clone();
        let clip_task = tokio::spawn(async move {
            let mut last_sent: Option<String> = None;
            let mut last_seq = clipboard_sequence_number();
            loop {
                tokio::select! {
                    _ = cancel_clip.cancelled() => break,
                    _ = async {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        let seq = clipboard_sequence_number();
                        if seq == last_seq {
                            return;
                        }
                        last_seq = seq;
                        let current = match read_clipboard_text() {
                            Ok(t) => t,
                            Err(_) => return, // no text / inaccessible / transient failure
                        };
                        if current.len() > MAX_CLIPBOARD_BYTES {
                            return;
                        }
                        if last_sent.as_ref() == Some(&current) {
                            return;
                        }
                        // Skip if this matches what we just received from the peer —
                        // don't echo remote updates back.
                        if clip_last_remote.lock().await.as_ref() == Some(&current) {
                            return;
                        }
                        if let Err(e) = clip_transport
                            .send_control(ControlMessage::ClipboardText {
                                text: current.clone(),
                            })
                            .await
                        {
                            warn!(?e, "send clipboard failed");
                        } else {
                            last_sent = Some(current);
                        }
                    } => {}
                }
            }
            cancel_clip_propagate.cancel();
        });

        // Outgoing-dir watcher: poll `args.outgoing_dir` every few seconds.
        // Any regular file (not in the `sent/` subdir, not a dotfile) gets
        // streamed to the viewer and then moved into `sent/` so we don't
        // resend on the next poll. The `sent/` subdir is created on demand.
        let ft_transport = Arc::clone(&transport);
        let outgoing_dir = args.outgoing_dir.clone();
        let cancel_outgoing = cancel.clone();
        let cancel_outgoing_propagate = cancel.clone();
        let outgoing_task = tokio::spawn(async move {
            let sent_dir = outgoing_dir.join(FILE_SEND_SENT_SUBDIR);
            loop {
                tokio::select! {
                    _ = cancel_outgoing.cancelled() => break,
                    _ = async {
                        tokio::time::sleep(OUTGOING_POLL_INTERVAL).await;
                        if !outgoing_dir.is_dir() {
                            return;
                        }
                        let mut read_dir = match tokio::fs::read_dir(&outgoing_dir).await {
                            Ok(r) => r,
                            Err(e) => {
                                warn!(?e, path = %outgoing_dir.display(), "read_dir failed");
                                return;
                            }
                        };
                        while let Ok(Some(entry)) = read_dir.next_entry().await {
                            let path = entry.path();
                            if !path.is_file() {
                                continue;
                            }
                            let name = path.file_name().and_then(|s| s.to_str());
                            if name.is_none_or(|n| n.starts_with('.')) {
                                continue;
                            }
                            info!(path = %path.display(), "sending outgoing file to viewer");
                            match send_file(&*ft_transport, &path, DEFAULT_MAX_TRANSFER_BYTES).await {
                                Ok(()) => {
                                    if let Err(e) = tokio::fs::create_dir_all(&sent_dir).await {
                                        warn!(?e, "create sent/ subdir failed");
                                        continue;
                                    }
                                    let dest = sent_dir.join(path.file_name().unwrap());
                                    let dest = prdt_filetransfer::unique_path(&dest);
                                    if let Err(e) = tokio::fs::rename(&path, &dest).await {
                                        warn!(
                                            ?e,
                                            from = %path.display(),
                                            to = %dest.display(),
                                            "move to sent/ failed; file will be resent on next poll",
                                        );
                                    }
                                }
                                Err(e) => warn!(?e, path = %path.display(), "send_file failed"),
                            }
                        }
                    } => {}
                }
            }
            cancel_outgoing_propagate.cancel();
        });

        let watchdog = watchdog::spawn_watchdog(cancel.clone(), Arc::clone(&last_keepalive));

        tokio::select! {
            _ = cancel.cancelled() => {
                info!("session cancelled — joining workers");
            }
            _ = tokio::signal::ctrl_c() => {
                info!("ctrl-c received; shutting down");
                cancel.cancel();
                let _ = tokio::join!(video, input, audio_task, clip_task, outgoing_task, watchdog);
                return Ok(());
            }
        }

        // Cancel any survivors and drain JoinHandles so encoder Drops run before
        // the next handshake (NVENC/MF release GPU resources here).
        cancel.cancel();
        let _ = tokio::join!(video, input, audio_task, clip_task, outgoing_task, watchdog);
        info!("session ended; returning to handshake wait");
    }
}

pub fn run_main() -> Result<()> {
    run_with_args(Args::parse())
}

#[cfg(windows)]
pub fn run_with_args(args: Args) -> Result<()> {
    if args.headless {
        return run_cli(args);
    }

    // GUI mode: gui-host installs its own tracing subscriber + tokio runtime.
    let args_arc = std::sync::Arc::new(args.clone());
    let run_host_fn: prdt_gui_host::RunHostFn = std::sync::Arc::new(move |cancel| {
        let args = args_arc.clone();
        tokio::spawn(async move { run_host((*args).clone(), None, None, cancel).await })
    });
    prdt_gui_host::run_host_gui(env!("CARGO_PKG_NAME"), args.config.clone(), run_host_fn)
}

/// On Linux the host is CLI-only for L1.5a — the GUI shell (`prdt-gui-host`)
/// is a Windows-only dependency and the Linux GUI is deferred to L2 per
/// the plan §3 scope. `run_with_args` therefore always invokes the
/// headless CLI path on Linux regardless of `args.headless`.
#[cfg(target_os = "linux")]
pub fn run_with_args(args: Args) -> Result<()> {
    run_cli(args)
}

#[tokio::main(flavor = "multi_thread")]
async fn run_cli(args: Args) -> Result<()> {
    init_tracing();
    #[cfg(windows)]
    prdt_gui_common::install_panic_hook(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    run_host(args, None, None, tokio_util::sync::CancellationToken::new()).await
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
}

/// Map `prdt_protocol::Codec` → `prdt_media_policy::Codec`.
///
/// `prdt_protocol::Codec` includes `Av1` which `prdt_media_policy::Codec`
/// does not support in P5A. `Av1` falls back to `H264` (SW path) with a
/// warn-log; a proper codec hot-swap is Phase 5C scope.
fn to_policy_codec(c: prdt_protocol::Codec) -> prdt_media_policy::Codec {
    match c {
        Codec::H264 => prdt_media_policy::Codec::H264,
        Codec::H265 => prdt_media_policy::Codec::H265,
        Codec::Av1 => {
            tracing::warn!(
                "prdt_protocol::Codec::Av1 not supported by prdt_media_policy in P5A; \
                 falling back to H264 for policy ranking (codec hot-swap deferred to P5C)"
            );
            prdt_media_policy::Codec::H264
        }
    }
}

// Tests below exercise Windows-specific encoder/adapter surfaces
// (`pick_default_adapter`, `supported_codecs_for_encoder_arg`) and so are
// gated to Windows. The cross-platform unit tests live in
// `platform/mod.rs` and `platform/linux.rs`.
#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use clap::Parser;
    use platform::win::supported_codecs_for_encoder_arg;

    /// `--encoder openh264 --headless` must parse cleanly even on a
    /// machine with no NVENC SDK / no NVIDIA GPU. Pre-mortem #1 from
    /// the plan: we don't want the SW build path gated on hardware
    /// availability.
    #[test]
    fn cli_parses_encoder_openh264() {
        let args = Args::try_parse_from([
            "prdt-host",
            "--encoder",
            "openh264",
            "--headless",
            "--bitrate-mbps",
            "30",
            "--key-file",
            "C:/tmp/test-host-key.bin",
        ])
        .expect("CLI should parse with --encoder openh264");
        assert_eq!(args.encoder, "openh264");
        assert!(args.headless);
        assert_eq!(args.bitrate_mbps, 30);
    }

    #[test]
    fn cli_rejects_unknown_encoder_value_at_pick_time() {
        // clap accepts any string for --encoder (it's String, not enum);
        // the unknown-value bail happens at pick_encoder. This test
        // documents that contract so the GUI / future enum migration
        // doesn't accidentally regress it.
        let args = Args::try_parse_from([
            "prdt-host",
            "--encoder",
            "bogus-backend",
            "--key-file",
            "C:/tmp/test-host-key.bin",
        ])
        .expect("clap accepts any string for --encoder");
        assert_eq!(args.encoder, "bogus-backend");
    }

    #[test]
    fn supported_codecs_for_encoder_openh264_advertises_h264_only() {
        // adapter is unused by the openh264 branch; build a bogus one
        // for the test by going through `pick_default_adapter`. If the
        // test machine has no GPU at all this would skip — but every
        // dev/CI box has at least the basic display adapter.
        let adapter = prdt_media_win::pick_default_adapter().expect("adapter for test");
        let codecs = supported_codecs_for_encoder_arg("openh264", &adapter);
        assert_eq!(codecs, vec![Codec::H264]);

        let codecs = supported_codecs_for_encoder_arg("nvenc", &adapter);
        assert_eq!(codecs, vec![Codec::H265]);

        let codecs = supported_codecs_for_encoder_arg("auto", &adapter);
        assert!(codecs.contains(&Codec::H265));
        assert!(codecs.contains(&Codec::H264));
    }
}
