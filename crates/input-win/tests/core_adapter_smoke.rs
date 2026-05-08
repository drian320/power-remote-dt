//! Verifies that all three `prdt_input_core` traits are usable via
//! `dyn` references on the Windows adapter types. Does not exercise
//! actual SendInput / clipboard / monitor enumeration — those are
//! covered by `injector_constructs` and the desktop module's tests.

#![cfg(windows)]

use prdt_input_core::{ClipboardProvider, InputInjector, VirtualDesktopGeometry};
use prdt_input_win::{SendInputInjector, Win32Clipboard, Win32VirtualDesktop};

#[test]
fn injector_dyn_dispatch() {
    let injector = SendInputInjector::new();
    let dyn_inj: &dyn InputInjector = &injector;
    assert_eq!(dyn_inj.backend_name(), "send-input");
}

#[test]
fn clipboard_dyn_dispatch() {
    let mut cb = Win32Clipboard::new();
    let dyn_cb: &mut dyn ClipboardProvider = &mut cb;
    assert_eq!(dyn_cb.backend_name(), "win32-clipboard");
    // sequence_number() should not panic on a fresh session, even if
    // CI has no actual clipboard contents.
    let _ = dyn_cb.sequence_number();
}

#[test]
fn desktop_dyn_dispatch() {
    let d = Win32VirtualDesktop::new();
    let dyn_d: &dyn VirtualDesktopGeometry = &d;
    let r = dyn_d.virtual_desktop_rect();
    assert!(r.width() >= 0);
    assert!(r.height() >= 0);
}
