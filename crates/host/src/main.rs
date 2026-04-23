#![cfg(windows)]

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
    read_clipboard_text, virtual_desktop_rect, write_clipboard_text, SendInputInjector,
    MAX_CLIPBOARD_BYTES,
};
use prdt_media_win::{
    dxgi::enumerate_outputs_for_adapter, pick_default_adapter, D3d11Device, DxgiNvencProducer,
};
use prdt_protocol::{wire::AudioPacket, ControlMessage, MonitorRect, VideoProducer};
use prdt_transport::{
    host_handshake, now_monotonic_us, CustomUdpTransport, ReceivedMessage, Transport,
    UdpTransportConfig,
};
use tracing::{info, warn};

const FILE_RECV_DIR: &str = "prdt-received";
const FILE_SEND_DIR: &str = "prdt-outgoing";
const FILE_SEND_SENT_SUBDIR: &str = "sent";
const OUTGOING_POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Parser, Debug)]
#[command(
    name = "prdt-host",
    about = "power-remote-dt host (capture + encode + input inject)"
)]
struct Args {
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
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();

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
    let transport = Arc::new(
        CustomUdpTransport::bind(args.bind, cfg)
            .await
            .context("UDP bind")?,
    );
    info!(local = ?transport.local_addr()?, "listening; waiting for Noise handshake");
    transport
        .handshake_as_server(&keypair)
        .await
        .context("Noise server handshake")?;
    info!("Noise handshake complete — encrypted channel established");

    // Wait for Hello, send HelloAck.
    let session_id: u64 = 0xDEADBEEF; // stable ID for Phase 0; randomize in Plan 4
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

    // Spawn clipboard watcher: poll the OS clipboard and forward changes.
    let clip_transport = Arc::clone(&transport);
    let clip_last_remote = Arc::clone(&last_remote_clipboard);
    let clip_task = tokio::spawn(async move {
        let mut last_sent: Option<String> = None;
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let current = match read_clipboard_text() {
                Ok(t) => t,
                Err(_) => continue, // no text / inaccessible / transient failure
            };
            if current.len() > MAX_CLIPBOARD_BYTES {
                continue;
            }
            // Skip if same as last we sent.
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
                match send_file(&ft_transport, &path, DEFAULT_MAX_TRANSFER_BYTES).await {
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
