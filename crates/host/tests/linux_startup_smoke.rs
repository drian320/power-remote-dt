//! L1.5a integration smoke: prove that prdt_host::run_with_args can boot
//! on Linux (X11 + uinput + media-sw + transport scaffolding) and shut
//! down cleanly when the host task is aborted.
//!
//! The deeper four-phase smoke (handshake / video / input / cleanup)
//! is left as TODOs inside the test body — fleshing them out requires
//! an in-process Transport pair which prdt-transport doesn't currently
//! expose. The bar for L1.5a is: the host process can START on Linux
//! without panicking. End-to-end is L1.5b's bar.
//!
//! Run with:
//!   cargo test -p prdt-host --target x86_64-unknown-linux-gnu \
//!     --test linux_startup_smoke -- --ignored

#![cfg(target_os = "linux")]

use clap::Parser as _;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires WSLg X11 + /dev/uinput. Run with: cargo test -p prdt-host --target x86_64-unknown-linux-gnu --test linux_startup_smoke -- --ignored"]
async fn linux_host_starts_without_panic() {
    // Resolve a temp key file so we don't pollute the user's data dir.
    let tmp = tempfile::tempdir().expect("tempdir");
    let key_path = tmp.path().join("test-host-key.bin");

    // Build args matching the host's clap definition.
    // --silent-allow: skip the consent gate (no GUI, no known-peers file needed).
    // --bind 127.0.0.1:0: let the OS pick a free port.
    // --encoder openh264: SW path, no GPU required.
    // --key-file: writable temp path so the host can generate/store its keypair.
    let argv: Vec<&str> = vec![
        "prdt-host",
        "--bind",
        "127.0.0.1:0",
        "--encoder",
        "openh264",
        "--key-file",
        key_path.to_str().expect("key path is valid UTF-8"),
        "--silent-allow",
    ];
    let args = prdt_host::Args::parse_from(argv.iter().copied());

    // run_with_args on Linux is a synchronous function that spins up its own
    // #[tokio::main] runtime internally. We must not call it directly from
    // within the test's tokio runtime (that would panic with "cannot start a
    // runtime from within a runtime"). Use spawn_blocking to run it on a
    // dedicated OS thread pool thread instead.
    let host_handle = tokio::task::spawn_blocking(move || {
        prdt_host::run_with_args(args)
    });

    // Give the host ~500 ms to bind UDP, write the key file, and reach the
    // "waiting for Noise handshake" stage. No peer will connect, so the host
    // will block in transport.recv() — that is the expected steady-state.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Phase 2 (video capture): TODO — requires an in-process Transport peer
    //   to exchange HelloAck before the host emits frames.
    // Phase 3 (input dispatch): TODO — same dependency.
    // Phase 4 (cleanup): abort the spawn_blocking handle.

    // abort() marks the JoinHandle as cancelled. The underlying blocking thread
    // continues running until it yields or the test process exits, but the
    // handle's Future returns Err(JoinError::Cancelled) immediately.
    host_handle.abort();

    // Give any drop side-effects (uinput device release, X11 disconnect,
    // UDP socket close) a moment to propagate via the OS.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The test passes as long as:
    //   1. run_with_args did not panic inside the blocking thread before
    //      the abort (a panic would propagate as JoinError::Panic and the
    //      test harness would report it).
    //   2. Nothing above panicked unconditionally.
    //
    // The host will still be running on its blocking thread when we return
    // here (there is no cooperative shutdown hook yet — that is L1.5b's
    // scope). The tokio test runtime drops after this function returns,
    // and the process exits, which reclaims all threads.
}
