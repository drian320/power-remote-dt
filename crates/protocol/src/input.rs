use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum MouseButton {
    Left = 0,
    Right = 1,
    Middle = 2,
    X1 = 3,
    X2 = 4,
}

impl MouseButton {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Left),
            1 => Some(Self::Right),
            2 => Some(Self::Middle),
            3 => Some(Self::X1),
            4 => Some(Self::X2),
            _ => None,
        }
    }
}

/// Input event sent from the viewer to the host.
///
/// - Mouse coordinates: `absolute=true` means host-screen-space pixels;
///   `absolute=false` means a delta from the previous position.
/// - Scancode: host-OS-native scancode (we do NOT translate virtual keys
///   between viewer and host - passthrough avoids layout mismatches).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputEvent {
    MouseMove { x: i32, y: i32, absolute: bool },
    MouseButton { button: MouseButton, pressed: bool },
    MouseWheel { dx: i32, dy: i32 },
    Key { scancode: u32, pressed: bool },
}

/// Discriminant byte used in the wire format (InputPacket.event_kind).
impl InputEvent {
    pub fn kind_u8(&self) -> u8 {
        match self {
            Self::MouseMove { .. } => 0,
            Self::MouseButton { .. } => 1,
            Self::MouseWheel { .. } => 2,
            Self::Key { .. } => 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_button_round_trip() {
        for v in 0u8..=4 {
            let b = MouseButton::from_u8(v).unwrap();
            assert_eq!(b as u8, v);
        }
        assert!(MouseButton::from_u8(99).is_none());
    }

    #[test]
    fn event_kinds_are_stable() {
        assert_eq!(
            InputEvent::MouseMove {
                x: 0,
                y: 0,
                absolute: true
            }
            .kind_u8(),
            0
        );
        assert_eq!(
            InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true
            }
            .kind_u8(),
            1,
        );
        assert_eq!(InputEvent::MouseWheel { dx: 0, dy: 1 }.kind_u8(), 2);
        assert_eq!(
            InputEvent::Key {
                scancode: 0x1E,
                pressed: true
            }
            .kind_u8(),
            3
        );
    }
}
