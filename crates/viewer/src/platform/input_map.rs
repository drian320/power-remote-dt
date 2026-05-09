//! Cross-platform winit-event → wire mapping for mouse + keyboard.
//!
//! Extracted from `prdt_input_win::capturer::RawInputCapturer` (mouse) and
//! `crates/viewer/src/lib.rs::physical_key_to_scancode` (keyboard) so that
//! both Windows and Linux viewer paths use a single source of truth. The
//! mappings depend only on `winit` types and `prdt_protocol` wire types,
//! not on any OS-specific input library.

use prdt_protocol::MouseButton;
use winit::event::MouseButton as WinitMouseButton;
use winit::keyboard::PhysicalKey;
use winit::platform::scancode::PhysicalKeyExtScancode;

/// Map a winit `MouseButton` to the wire `MouseButton`. Returns `None`
/// for buttons that don't have a wire representation (e.g. extra mouse
/// buttons beyond the known set).
///
/// Note: `Back` maps to `MouseButton::X1` and `Forward` maps to
/// `MouseButton::X2` — matching the original in
/// `prdt_input_win::capturer::RawInputCapturer::map_winit_mouse_button`.
pub fn map_winit_mouse_button(b: WinitMouseButton) -> Option<MouseButton> {
    use WinitMouseButton as W;
    Some(match b {
        W::Left => MouseButton::Left,
        W::Right => MouseButton::Right,
        W::Middle => MouseButton::Middle,
        W::Back => MouseButton::X1,
        W::Forward => MouseButton::X2,
        W::Other(_) => return None,
    })
}

/// Map a winit `PhysicalKey` to its raw OS-level scancode (Windows
/// scancode on Windows, Linux evdev code on Linux). Returns `None` for
/// `PhysicalKey::Unidentified`. Cross-OS scancode normalization is L2.
pub fn physical_key_to_scancode(key: PhysicalKey) -> Option<u32> {
    key.to_scancode()
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::KeyCode;

    #[test]
    fn mouse_button_mapping_left_right_middle() {
        assert_eq!(map_winit_mouse_button(WinitMouseButton::Left), Some(MouseButton::Left));
        assert_eq!(map_winit_mouse_button(WinitMouseButton::Right), Some(MouseButton::Right));
        assert_eq!(map_winit_mouse_button(WinitMouseButton::Middle), Some(MouseButton::Middle));
    }

    #[test]
    fn mouse_button_extras_return_none() {
        // Back/Forward map to X1/X2 (not None) — matching the original capturer.
        assert_eq!(map_winit_mouse_button(WinitMouseButton::Back), Some(MouseButton::X1));
        assert_eq!(map_winit_mouse_button(WinitMouseButton::Forward), Some(MouseButton::X2));
        assert_eq!(map_winit_mouse_button(WinitMouseButton::Other(7)), None);
    }

    #[test]
    fn physical_key_to_scancode_known_keys_some() {
        // Don't assert exact value (varies by OS), only that mapping exists.
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
}
