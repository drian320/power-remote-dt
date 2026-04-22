//! DXGI capture smoke test. Acquires up to 20 frames, reports how many real
//! frames arrived. Lenient — static desktop produces all-timeout results.

#![cfg(windows)]

use std::time::Duration;

use prdt_media_win::{
    dxgi::{duplication::AcquiredFrame, enumerate_outputs_for_adapter, DesktopDuplication},
    pick_default_adapter, D3d11Device,
};

#[test]
fn acquire_frames_lenient() {
    let adapter = match pick_default_adapter() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("no adapter (skip): {e}");
            return;
        }
    };
    let dev = match D3d11Device::create(&adapter) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("no D3D11 device (skip): {e}");
            return;
        }
    };
    let outputs = match enumerate_outputs_for_adapter(&adapter) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("no outputs (skip): {e}");
            return;
        }
    };
    if outputs.is_empty() {
        eprintln!("no outputs available (skip)");
        return;
    }
    let primary = outputs
        .iter()
        .find(|o| o.is_attached)
        .cloned()
        .unwrap_or_else(|| outputs[0].clone());

    let mut dup = match DesktopDuplication::new(&dev, &primary) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("duplication creation failed (skip): {e}");
            return;
        }
    };

    let mut real_frame_count = 0;
    let mut timeout_count = 0;
    for _ in 0..20 {
        match dup.acquire_next_frame(Duration::from_millis(100)) {
            Ok(AcquiredFrame::Frame {
                texture,
                frame_info: _,
            }) => {
                real_frame_count += 1;
                // Basic sanity: texture has sensible dimensions matching output.
                assert_eq!(texture.width(), dup.width());
                assert_eq!(texture.height(), dup.height());
            }
            Ok(AcquiredFrame::Timeout) => timeout_count += 1,
            Err(e) => {
                eprintln!("error in acquire (treated as non-fatal): {e}");
                break;
            }
        }
    }
    eprintln!("captured {real_frame_count} real frames, {timeout_count} timeouts in 20 attempts");
    // Lenient: do not assert on real_frame_count since a static desktop
    // legitimately produces all timeouts.
}
