//! Contract tests over any `CaptureSource` impl.
//!
//! Driven by an in-memory `MockCheckerboardCapture` stub; the X11
//! variant is gated `#[ignore]` because it needs a real X server.

#![cfg(target_os = "linux")]

use prdt_media_linux::capture_source::{CaptureSource, CaptureSourceError};

/// Test stub that fills `out` with a deterministic checkerboard pattern.
struct MockCheckerboardCapture {
    width: u32,
    height: u32,
    tick: u32,
}

impl CaptureSource for MockCheckerboardCapture {
    fn geometry(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    fn capture_into(&mut self, out: &mut Vec<u8>) -> Result<(), CaptureSourceError> {
        let n = (self.width as usize) * (self.height as usize) * 4;
        out.resize(n, 0);
        for (i, b) in out.iter_mut().enumerate() {
            *b = ((i as u32 ^ self.tick) & 0xFF) as u8;
        }
        self.tick = self.tick.wrapping_add(1);
        Ok(())
    }
}

#[test]
fn mock_capture_obeys_geometry_invariant() {
    let mut cap = MockCheckerboardCapture {
        width: 320,
        height: 240,
        tick: 0,
    };
    let (w, h) = cap.geometry();
    assert!(w >= 1 && h >= 1, "geometry must be ≥ 1×1");
    let mut buf = Vec::new();
    cap.capture_into(&mut buf).expect("capture_into ok");
    assert!(
        buf.len() >= (w as usize) * (h as usize) * 4,
        "capture_into wrote enough bytes"
    );
}

#[test]
fn mock_capture_advances_between_calls() {
    let mut cap = MockCheckerboardCapture {
        width: 8,
        height: 8,
        tick: 0,
    };
    let mut a = Vec::new();
    let mut b = Vec::new();
    cap.capture_into(&mut a).unwrap();
    cap.capture_into(&mut b).unwrap();
    assert_ne!(a, b, "successive frames should differ (advancing tick)");
}

#[test]
fn mock_capture_geometry_is_stable_when_unchanged() {
    let cap = MockCheckerboardCapture {
        width: 1920,
        height: 1080,
        tick: 0,
    };
    assert_eq!(cap.geometry(), (1920, 1080));
    assert_eq!(cap.geometry(), (1920, 1080)); // idempotent
}

#[test]
#[ignore = "requires real X11 connection — run on WSL2 with: cargo test -p prdt-media-linux --test capture_source_contract -- --ignored"]
fn x11_capturer_implements_capture_source() {
    use prdt_media_linux::x11_capture::X11ShmCapturer;
    let mut cap = X11ShmCapturer::new().expect("X11 connect");
    let (w, h) = cap.geometry();
    let mut buf = Vec::new();
    cap.capture_into(&mut buf).expect("grab");
    assert_eq!(buf.len(), (w as usize) * (h as usize) * 4);
}
