//! Capture InputEvents on the viewer. Integrates with winit WindowEvent stream.
//! Callers push events from their winit event loop into the capturer, and
//! consume them via an mpsc channel.

use prdt_protocol::{InputEvent, MouseButton};
use tokio::sync::mpsc;

pub struct RawInputCapturer {
    tx: mpsc::UnboundedSender<InputEvent>,
}

impl RawInputCapturer {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<InputEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }

    pub fn emit(&self, ev: InputEvent) {
        let _ = self.tx.send(ev);
    }

    /// Convert a winit MouseButton to protocol MouseButton.
    pub fn map_winit_mouse_button(b: winit::event::MouseButton) -> Option<MouseButton> {
        use winit::event::MouseButton as W;
        Some(match b {
            W::Left => MouseButton::Left,
            W::Right => MouseButton::Right,
            W::Middle => MouseButton::Middle,
            W::Back => MouseButton::X1,
            W::Forward => MouseButton::X2,
            W::Other(_) => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prdt_protocol::InputEvent;

    #[tokio::test]
    async fn capturer_channel_roundtrip() {
        let (cap, mut rx) = RawInputCapturer::new();
        cap.emit(InputEvent::Key {
            scancode: 0x1E,
            pressed: true,
        });
        let ev = rx.recv().await.unwrap();
        assert!(matches!(
            ev,
            InputEvent::Key {
                scancode: 0x1E,
                pressed: true
            }
        ));
    }
}
