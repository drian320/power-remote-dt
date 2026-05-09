/// Integration-test entry point for platform::input_map that works on all
/// targets. Because crates/viewer/src/lib.rs carries `#![cfg(windows)]`,
/// the lib tests are invisible on Linux. This separate test binary uses
/// `#[path]` to compile platform/input_map.rs directly, bypassing that gate.
use prdt_protocol::MouseButton;
use winit::event::MouseButton as WinitMouseButton;
use winit::keyboard::PhysicalKey;
use winit::platform::scancode::PhysicalKeyExtScancode;

// Pull the module under test directly by path.
#[path = "../src/platform/input_map.rs"]
mod input_map;

use input_map::{map_winit_mouse_button, physical_key_to_scancode};

#[test]
fn mouse_button_mapping_left_right_middle() {
    assert_eq!(map_winit_mouse_button(WinitMouseButton::Left), Some(MouseButton::Left));
    assert_eq!(map_winit_mouse_button(WinitMouseButton::Right), Some(MouseButton::Right));
    assert_eq!(map_winit_mouse_button(WinitMouseButton::Middle), Some(MouseButton::Middle));
}

#[test]
fn mouse_button_extras_return_none() {
    // Back/Forward map to X1/X2 — matching the original capturer.
    assert_eq!(map_winit_mouse_button(WinitMouseButton::Back), Some(MouseButton::X1));
    assert_eq!(map_winit_mouse_button(WinitMouseButton::Forward), Some(MouseButton::X2));
    assert_eq!(map_winit_mouse_button(WinitMouseButton::Other(7)), None);
}

#[test]
fn physical_key_to_scancode_known_keys_some() {
    use winit::keyboard::KeyCode;
    assert!(physical_key_to_scancode(PhysicalKey::Code(KeyCode::Escape)).is_some());
    assert!(physical_key_to_scancode(PhysicalKey::Code(KeyCode::Enter)).is_some());
    assert!(physical_key_to_scancode(PhysicalKey::Code(KeyCode::Space)).is_some());
}

#[test]
fn physical_key_unidentified_returns_none() {
    use winit::keyboard::NativeKeyCode;
    // NativeKeyCode::Android is not a known scancode on Windows or Linux,
    // so to_scancode returns None on every OS.
    assert_eq!(
        physical_key_to_scancode(PhysicalKey::Unidentified(NativeKeyCode::Android(0))),
        None,
    );
}
