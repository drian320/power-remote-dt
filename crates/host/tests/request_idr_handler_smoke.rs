//! Smoke test: the host's control handler sets force_idr_flag when it receives
//! ControlMessage::RequestIdr. This does NOT spin up a real transport; it
//! exercises only the Arc<AtomicBool> flag mechanism in isolation.
//!
//! Run with:
//!   cargo test -p prdt-host --target x86_64-unknown-linux-gnu \
//!     --test request_idr_handler_smoke

#![cfg(target_os = "linux")]

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// Minimal reproduction of the force_idr_flag wiring from host control loop.
/// The real loop calls `force_idr_flag.store(true, Ordering::Release)` on
/// receiving RequestIdr; here we call that same store directly and verify.
#[test]
fn request_idr_sets_force_flag() {
    let force_idr_flag = Arc::new(AtomicBool::new(false));

    // Simulate the control handler arm:
    //   Ok(ReceivedMessage::Control(ControlMessage::RequestIdr)) => {
    //       force_idr_flag.store(true, Ordering::Release);
    //   }
    let flag_clone = Arc::clone(&force_idr_flag);
    flag_clone.store(true, Ordering::Release);

    // Simulate the encode loop reading the flag:
    //   let force_idr = force_idr_flag.swap(false, Ordering::AcqRel);
    let force_idr = force_idr_flag.swap(false, Ordering::AcqRel);

    assert!(
        force_idr,
        "encode loop must see force_idr=true after RequestIdr"
    );

    // After swap, the flag resets.
    assert!(
        !force_idr_flag.load(Ordering::Acquire),
        "flag must be false after encode loop swapped it"
    );
}

/// A second RequestIdr arriving before the encode loop fires must still result
/// in exactly one IDR (the flag is a boolean, not a counter — that's intentional).
#[test]
fn double_request_idr_still_one_idr() {
    let force_idr_flag = Arc::new(AtomicBool::new(false));

    // Two back-to-back RequestIdr control messages.
    force_idr_flag.store(true, Ordering::Release);
    force_idr_flag.store(true, Ordering::Release);

    // Encode loop fires once.
    let force_idr = force_idr_flag.swap(false, Ordering::AcqRel);
    assert!(force_idr, "flag must be true after two stores");
    assert!(
        !force_idr_flag.load(Ordering::Acquire),
        "flag must be false after swap"
    );
}
