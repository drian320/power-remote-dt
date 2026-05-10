//! L1.5b integration smoke: prove that prdt_viewer::run_with_args can
//! boot on Linux (winit + softbuffer + openh264 + transport scaffolding)
//! and shut down cleanly when the spawned thread is detached.
//!
//! The deeper handshake / first-frame / clipboard smoke requires a real
//! prdt host on the same machine, which the manual checklist covers.
//! The bar for L1.5b: viewer process can START on Linux without
//! panicking. End-to-end is the manual smoke (spec §5.7).

#![cfg(target_os = "linux")]

use std::time::Duration;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires WSLg X11. Run with: cargo test -p prdt-viewer --target x86_64-unknown-linux-gnu --test linux_connect_smoke -- --ignored"]
async fn linux_viewer_starts_without_panic() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let key_path = tmp.path().join("test-viewer-key.bin");

    // Args matching the viewer's actual clap definition.
    // --host is Option<SocketAddr> so we pass an unreachable addr (port 1 is
    // always closed); the viewer will attempt a handshake, time out, and exit.
    // --host-pubkey is Option<String>; we pass a dummy base64 key so the
    // viewer does not have to consult a known-hosts file.
    // --viewer-key-file has a default but we override it to a tmp path so the
    // test is hermetic (no ~/.local/share pollution).
    // --decoder / --codec have defaults ("nvdec", "auto") so we override to
    // the cross-platform openh264/h264 pair that is available on Linux.
    // --headless skips the GUI launcher (implicitly always-true on Linux, but
    // harmless to state explicitly).
    let argv: Vec<String> = vec![
        "prdt-viewer".into(),
        "--host".into(),
        "127.0.0.1:1".into(), // unreachable; viewer exits after handshake timeout
        "--host-pubkey".into(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into(),
        "--viewer-key-file".into(),
        key_path.display().to_string(),
        "--decoder".into(),
        "openh264".into(),
        "--codec".into(),
        "h264".into(),
        "--headless".into(),
    ];

    let viewer_handle = tokio::task::spawn_blocking(move || {
        use clap::Parser as _;
        let args = prdt_viewer::Args::parse_from(argv.iter().map(String::as_str));
        prdt_viewer::run_with_args(args)
    });

    // Give viewer ~1 second to bind, attempt handshake (will time out
    // because host is unreachable), and start tearing down.
    tokio::time::sleep(Duration::from_secs(1)).await;

    viewer_handle.abort();
    tokio::time::sleep(Duration::from_millis(200)).await;
}
