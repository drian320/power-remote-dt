#![cfg(windows)]

mod dxgi_sw_producer;
mod encoder_dispatch;
mod status;
mod watchdog;

use std::fs;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use prdt_audio::{LoopbackCapture, OpusEncoder};
use prdt_crypto::KeyPair;
use prdt_filetransfer::{send_file, TransferReceiver, DEFAULT_MAX_TRANSFER_BYTES};
use prdt_input_win::{
    clipboard_sequence_number, read_clipboard_text, virtual_desktop_rect, write_clipboard_text,
    SendInputInjector, MAX_CLIPBOARD_BYTES,
};
use prdt_media_sw::{Openh264Encoder, Openh264EncoderConfig};
use prdt_media_win::{
    dxgi::enumerate_outputs_for_adapter, pick_default_adapter, D3d11Device, DxgiNvencProducer,
    HwHevcEncoder, MfH265Encoder, NvencEncoder, NvencEncoderConfig,
};
use prdt_protocol::{wire::AudioPacket, Codec, ControlMessage, MonitorRect, VideoProducer};

use dxgi_sw_producer::DxgiSwProducer;
use encoder_dispatch::VideoEncoderBackend;
use prdt_transport::{
    host_handshake, now_monotonic_us, CustomUdpTransport, ReceivedMessage, Transport,
    UdpTransportConfig,
};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use status::SharedStatus;

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
    #[arg(long, default_value = "host-key.bin")]
    key_file: std::path::PathBuf,

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
    #[arg(long, default_value = "auto")]
    encoder: String,

    /// Run in CLI-only mode without launching the GUI. Required for headless servers / CI.
    #[arg(long)]
    headless: bool,

    /// Override the GUI config file location (default: %APPDATA%/prdt/config.toml).
    #[arg(long)]
    config: Option<std::path::PathBuf>,
}

pub async fn run_host(
    args: Args,
    _status: Option<SharedStatus>,
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

    let adapter = pick_default_adapter().context("no GPU adapter")?;
    let dev = D3d11Device::create(&adapter).context("D3D11 device")?;
    let outputs = enumerate_outputs_for_adapter(&adapter).context("outputs")?;
    if outputs.is_empty() {
        anyhow::bail!("no display outputs found on adapter");
    }
    let output = outputs
        .get(args.monitor as usize)
        .with_context(|| {
            format!(
                "no output at index {} (available: 0..{})",
                args.monitor,
                outputs.len()
            )
        })?
        .clone();

    info!(
        monitor = args.monitor,
        device_name = %output.device_name,
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
        if let Err(e) = transport.handshake_as_server(&keypair).await {
            warn!(?e, "Noise server handshake failed; resetting session");
            continue;
        }
        info!("Noise handshake complete — encrypted channel established");

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
        let monitor_rect = MonitorRect::new(
            output.desktop_rect.left,
            output.desktop_rect.top,
            output.desktop_rect.right,
            output.desktop_rect.bottom,
        );
        let vd_rect = virtual_desktop_rect();
        info!(
            monitor = ?monitor_rect,
            virtual_desktop = ?vd_rect,
            "advertising desktop geometry to viewer",
        );
        let host_supported = supported_codecs_for_encoder_arg(&args.encoder, &adapter);
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

        // Build producer after handshake so the viewer's negotiated codec
        // selects between the HW (H.265) and SW (H.264) paths.
        let width = (output.desktop_rect.right - output.desktop_rect.left) as u32;
        let height = (output.desktop_rect.bottom - output.desktop_rect.top) as u32;
        let backend =
            pick_encoder(&args.encoder, &adapter, &dev, width, height, bitrate_bps, req.codec)
                .context("encoder")?;
        info!(backend = backend.backend_name(), codec = req.codec.name(), "encoder ready");
        let mut producer: Box<dyn VideoProducer> = match backend {
            VideoEncoderBackend::Hw(enc) => Box::new(
                DxgiNvencProducer::with_encoder(&dev, &output, enc).context("hw producer")?,
            ),
            VideoEncoderBackend::SwH264(enc) => Box::new(
                DxgiSwProducer::with_encoder(&dev, &output, enc).context("sw producer")?,
            ),
        };

        let cancel = CancellationToken::new();
        let last_keepalive = Arc::new(AtomicU64::new(now_monotonic_us()));

        // Spawn video loop. `handshake_complete_at` anchors the first-frame-latency
        // measurement (Phase 4 acceptance: ≤ 500ms max-of-20 cold-start).
        let tx_video = Arc::clone(&transport);
        let cancel_video = cancel.clone();
        let cancel_video_propagate = cancel.clone();
        let handshake_complete_at = std::time::Instant::now();
        let mut video = tokio::spawn(async move {
            let mut frames_sent = 0u64;
            let mut send_errors = 0u64;
            let mut last_log = std::time::Instant::now();
            let mut first_frame_logged = false;
            loop {
                tokio::select! {
                    _ = cancel_video.cancelled() => break,
                    _ = async {
                        match producer.next_frame().await {
                            Ok(frame) => {
                                if !first_frame_logged {
                                    let elapsed_ms = handshake_complete_at.elapsed().as_millis();
                                    info!(elapsed_ms = elapsed_ms as u64, "first frame ready");
                                    first_frame_logged = true;
                                }
                                let nal_len = frame.nal_units.len();
                                let is_kf = frame.is_keyframe;
                                if let Err(e) = tx_video.send_video(frame).await {
                                    send_errors += 1;
                                    warn!(?e, nal_len, is_kf, "send_video error; continuing");
                                } else {
                                    frames_sent += 1;
                                }
                                if last_log.elapsed() >= std::time::Duration::from_secs(1) {
                                    info!(frames_sent, send_errors, "host tx stats");
                                    last_log = std::time::Instant::now();
                                }
                            }
                            Err(e) => {
                                warn!(?e, "producer error; continuing");
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
        let cancel_audio_propagate = cancel.clone();
        let mut audio_task = tokio::spawn(async move {
            let mut encoder = match OpusEncoder::new() {
                Ok(e) => e,
                Err(e) => {
                    warn!(?e, "opus encoder init");
                    cancel_audio_propagate.cancel();
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
                            None => break, // channel closed, exit task
                        }
                    }
                }
            }
            cancel_audio_propagate.cancel();
        });

        // Shared "last clipboard text we received from peer" — used by the
        // clipboard watcher to avoid echoing remote updates back to the peer.
        let last_remote_clipboard: Arc<tokio::sync::Mutex<Option<String>>> =
            Arc::new(tokio::sync::Mutex::new(None));

        // Spawn input injection loop.
        let rx_input = Arc::clone(&transport);
        let injector = SendInputInjector::new();
        let input_last_remote = Arc::clone(&last_remote_clipboard);
        let cancel_input = cancel.clone();
        let cancel_input_propagate = cancel.clone();
        let last_ka_input = Arc::clone(&last_keepalive);
        let mut input = tokio::spawn(async move {
            let mut ft_rx = TransferReceiver::new(FILE_RECV_DIR, DEFAULT_MAX_TRANSFER_BYTES);
            loop {
                tokio::select! {
                    _ = cancel_input.cancelled() => break,
                    msg = rx_input.recv() => {
                        match msg {
                            Ok(ReceivedMessage::Input(ev)) => {
                                if let Err(e) = injector.inject(ev) {
                                    warn!(?e, "inject error");
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
        let mut clip_task = tokio::spawn(async move {
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
        let mut outgoing_task = tokio::spawn(async move {
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
                            if name.map_or(true, |n| n.starts_with('.')) {
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

        let mut watchdog = watchdog::spawn_watchdog(cancel.clone(), Arc::clone(&last_keepalive));

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

pub fn run_with_args(args: Args) -> Result<()> {
    if args.headless {
        return run_cli(args);
    }

    // GUI mode: gui-host installs its own tracing subscriber + tokio runtime.
    let args_arc = std::sync::Arc::new(args.clone());
    let run_host_fn: prdt_gui_host::RunHostFn = std::sync::Arc::new(move |cancel| {
        let args = args_arc.clone();
        tokio::spawn(async move { run_host((*args).clone(), None, cancel).await })
    });
    prdt_gui_host::run_host_gui(env!("CARGO_PKG_NAME"), args.config.clone(), run_host_fn)
}

#[tokio::main(flavor = "multi_thread")]
async fn run_cli(args: Args) -> Result<()> {
    init_tracing();
    prdt_gui_common::install_panic_hook(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    run_host(args, None, tokio_util::sync::CancellationToken::new()).await
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
}

/// Resolve `--encoder` to a concrete backend. The `auto` selector picks
/// nvenc > mf > openh264. When the resolved backend is HW, the negotiated
/// codec must be H.265; when SW, must be H.264. The handshake layer rejects
/// mismatches before we get here, so a mismatch in this fn is a programmer
/// error and we bail.
fn pick_encoder(
    args_encoder: &str,
    adapter: &prdt_media_win::AdapterInfo,
    dev: &D3d11Device,
    width: u32,
    height: u32,
    bitrate_bps: u32,
    negotiated_codec: Codec,
) -> anyhow::Result<VideoEncoderBackend> {
    let choice = resolve_encoder_choice(args_encoder, adapter, negotiated_codec);
    match choice {
        "nvenc" => {
            if negotiated_codec != Codec::H265 {
                anyhow::bail!(
                    "encoder=nvenc but negotiated codec={:?}; handshake layer should have rejected this",
                    negotiated_codec
                );
            }
            let cfg = NvencEncoderConfig {
                width,
                height,
                fps_numerator: 60,
                fps_denominator: 1,
                bitrate_bps,
                gop_length: 60,
            };
            let enc = NvencEncoder::new(dev, &cfg).context("NvencEncoder::new")?;
            Ok(VideoEncoderBackend::Hw(HwHevcEncoder::from(enc)))
        }
        "mf" => {
            if negotiated_codec != Codec::H265 {
                anyhow::bail!(
                    "encoder=mf but negotiated codec={:?}; handshake layer should have rejected this",
                    negotiated_codec
                );
            }
            let cfg = NvencEncoderConfig {
                width,
                height,
                fps_numerator: 60,
                fps_denominator: 1,
                bitrate_bps,
                gop_length: 60,
            };
            let enc = MfH265Encoder::new(dev, &cfg).context("MfH265Encoder::new")?;
            Ok(VideoEncoderBackend::Hw(HwHevcEncoder::from(enc)))
        }
        "openh264" => {
            if negotiated_codec != Codec::H264 {
                anyhow::bail!(
                    "encoder=openh264 but negotiated codec={:?}; handshake layer should have rejected this",
                    negotiated_codec
                );
            }
            let cfg = Openh264EncoderConfig {
                width,
                height,
                target_bitrate_bps: bitrate_bps,
                max_fps: 60.0,
            };
            let enc = Openh264Encoder::new(cfg).context("Openh264Encoder::new")?;
            Ok(VideoEncoderBackend::SwH264(enc))
        }
        other => anyhow::bail!("unknown --encoder {other:?} (valid: auto, nvenc, mf, openh264)"),
    }
}

/// Apply the `auto` selection policy: nvenc > mf > openh264. Plan §Phase 2:
/// "auto selection order: nvenc > mf > openh264". The viewer-requested
/// codec narrows the choice — if the viewer wants H.264 we pick openh264
/// regardless of GPU vendor (HW H.264 encode is not implemented in this
/// repo and is out of scope for the software-codec tag).
fn resolve_encoder_choice<'a>(
    args_encoder: &'a str,
    adapter: &prdt_media_win::AdapterInfo,
    negotiated_codec: Codec,
) -> &'a str {
    if args_encoder == "auto" {
        match negotiated_codec {
            Codec::H264 => "openh264",
            Codec::H265 => {
                if adapter.is_nvidia() {
                    "nvenc"
                } else {
                    "mf"
                }
            }
            // Any future codec falls through to nvenc/mf which will then
            // bail with a clear "encoder=X but negotiated codec=Y" error.
            _ => {
                if adapter.is_nvidia() {
                    "nvenc"
                } else {
                    "mf"
                }
            }
        }
    } else {
        args_encoder
    }
}

/// What we advertise in HelloAck `host_supported_codecs` based on the
/// `--encoder` flag. An explicit choice locks us to a single codec; `auto`
/// advertises the full HW set so the viewer's preference wins.
fn supported_codecs_for_encoder_arg(
    args_encoder: &str,
    adapter: &prdt_media_win::AdapterInfo,
) -> Vec<Codec> {
    match args_encoder {
        "openh264" => vec![Codec::H264],
        "nvenc" | "mf" => vec![Codec::H265],
        // "auto" or anything else — caller resolves to a HW backend
        // (nvenc/mf), both of which emit H.265. media-sw is built into
        // this binary, so SW H.264 is also reachable; advertise both
        // so a viewer that explicitly asks for H.264 (with `--codec
        // h264`) can still negotiate without forcing the operator to
        // pass `--encoder openh264` on the host side.
        _ => {
            let _ = adapter;
            vec![Codec::H265, Codec::H264]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

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
