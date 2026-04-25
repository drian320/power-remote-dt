#![cfg(windows)]

mod status;

use std::fs;
use std::net::SocketAddr;
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
use prdt_media_win::{
    dxgi::enumerate_outputs_for_adapter, pick_default_adapter, D3d11Device, DxgiNvencProducer,
};
use prdt_protocol::{wire::AudioPacket, ControlMessage, MonitorRect, VideoProducer};
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

    info!("waiting for Noise handshake");
    transport
        .handshake_as_server(&keypair)
        .await
        .context("Noise server handshake")?;
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
    let req = host_handshake(
        &*transport,
        session_id,
        now_monotonic_us(),
        bitrate_bps,
        monitor_rect,
        vd_rect,
        Duration::from_secs(60),
    )
    .await
    .context("handshake")?;
    info!(?req, "handshake complete");

    // Build producer after handshake so the viewer's negotiated params can
    // eventually influence encoder setup (Phase 0 keeps this fixed).
    let mut producer = DxgiNvencProducer::new(&dev, &output, bitrate_bps).context("producer")?;

    // Spawn video loop.
    let tx_video = Arc::clone(&transport);
    let video = tokio::spawn(async move {
        let mut frames_sent = 0u64;
        let mut send_errors = 0u64;
        let mut last_log = std::time::Instant::now();
        loop {
            match producer.next_frame().await {
                Ok(frame) => {
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
        }
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
    let audio_task = tokio::spawn(async move {
        let mut encoder = match OpusEncoder::new() {
            Ok(e) => e,
            Err(e) => {
                warn!(?e, "opus encoder init");
                return;
            }
        };
        let epoch = std::time::Instant::now();
        let mut seq = 0u64;
        while let Some(frame) = pcm_async_rx.recv().await {
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
    });

    // Shared "last clipboard text we received from peer" — used by the
    // clipboard watcher to avoid echoing remote updates back to the peer.
    let last_remote_clipboard: Arc<tokio::sync::Mutex<Option<String>>> =
        Arc::new(tokio::sync::Mutex::new(None));

    // Spawn input injection loop.
    let rx_input = Arc::clone(&transport);
    let injector = SendInputInjector::new();
    let input_last_remote = Arc::clone(&last_remote_clipboard);
    let input = tokio::spawn(async move {
        let mut ft_rx = TransferReceiver::new(FILE_RECV_DIR, DEFAULT_MAX_TRANSFER_BYTES);
        loop {
            match rx_input.recv().await {
                Ok(ReceivedMessage::Input(ev)) => {
                    if let Err(e) = injector.inject(ev) {
                        warn!(?e, "inject error");
                    }
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
    });

    // Spawn clipboard watcher. We poll `GetClipboardSequenceNumber` at 50ms
    // which is cheap (no OpenClipboard handshake, no text copy), and only
    // actually read the clipboard when the sequence counter moves. This
    // drops copy-paste lag from the old 500ms polling interval while
    // keeping CPU use minimal when the clipboard is idle.
    let clip_transport = Arc::clone(&transport);
    let clip_last_remote = Arc::clone(&last_remote_clipboard);
    let clip_task = tokio::spawn(async move {
        let mut last_sent: Option<String> = None;
        let mut last_seq = clipboard_sequence_number();
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let seq = clipboard_sequence_number();
            if seq == last_seq {
                continue;
            }
            last_seq = seq;
            let current = match read_clipboard_text() {
                Ok(t) => t,
                Err(_) => continue, // no text / inaccessible / transient failure
            };
            if current.len() > MAX_CLIPBOARD_BYTES {
                continue;
            }
            if last_sent.as_ref() == Some(&current) {
                continue;
            }
            // Skip if this matches what we just received from the peer —
            // don't echo remote updates back.
            if clip_last_remote.lock().await.as_ref() == Some(&current) {
                continue;
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
        }
    });

    // Outgoing-dir watcher: poll `args.outgoing_dir` every few seconds.
    // Any regular file (not in the `sent/` subdir, not a dotfile) gets
    // streamed to the viewer and then moved into `sent/` so we don't
    // resend on the next poll. The `sent/` subdir is created on demand.
    let ft_transport = Arc::clone(&transport);
    let outgoing_dir = args.outgoing_dir.clone();
    let outgoing_task = tokio::spawn(async move {
        let sent_dir = outgoing_dir.join(FILE_SEND_SENT_SUBDIR);
        loop {
            tokio::time::sleep(OUTGOING_POLL_INTERVAL).await;
            if !outgoing_dir.is_dir() {
                continue;
            }
            let mut read_dir = match tokio::fs::read_dir(&outgoing_dir).await {
                Ok(r) => r,
                Err(e) => {
                    warn!(?e, path = %outgoing_dir.display(), "read_dir failed");
                    continue;
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
        }
    });

    tokio::select! {
        _ = video => info!("video task ended"),
        _ = input => info!("input task ended"),
        _ = audio_task => info!("audio task ended"),
        _ = clip_task => info!("clipboard task ended"),
        _ = outgoing_task => info!("outgoing file watcher ended"),
        _ = tokio::signal::ctrl_c() => info!("ctrl-c received"),
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let args = Args::parse();
    run_host(args, None, CancellationToken::new()).await
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
}
