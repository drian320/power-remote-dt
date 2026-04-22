//! Injects InputEvents on the host via `SendInput`.

use std::mem;

use prdt_protocol::{InputEvent, MouseButton};
use windows::Win32::UI::Input::KeyboardAndMouse::*;

#[derive(Debug, thiserror::Error)]
pub enum InjectError {
    #[error("SendInput: {0}")]
    SendInput(String),
}

#[derive(Debug, Default)]
pub struct SendInputInjector;

impl SendInputInjector {
    pub fn new() -> Self {
        Self
    }

    pub fn inject(&self, ev: InputEvent) -> Result<(), InjectError> {
        unsafe {
            match ev {
                InputEvent::MouseMove { x, y, absolute } => {
                    let mut flags = MOUSEEVENTF_MOVE;
                    if absolute {
                        // Map 0..65535 to the PRIMARY monitor only. Dropping
                        // MOUSEEVENTF_VIRTUALDESK because viewer currently
                        // captures one monitor at a time and normalizes in
                        // that monitor's local coord space. Plan 4+ with
                        // multi-monitor capture should re-add VIRTUALDESK
                        // and update viewer-side normalization to match.
                        flags |= MOUSEEVENTF_ABSOLUTE;
                    }
                    let input = INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: x,
                                dy: y,
                                mouseData: 0,
                                dwFlags: flags,
                                time: 0,
                                dwExtraInfo: 0,
                            },
                        },
                    };
                    let sent = SendInput(&[input], mem::size_of::<INPUT>() as i32);
                    if sent == 0 {
                        return Err(InjectError::SendInput("MouseMove sent 0".into()));
                    }
                }
                InputEvent::MouseButton { button, pressed } => {
                    let flags = match (button, pressed) {
                        (MouseButton::Left, true) => MOUSEEVENTF_LEFTDOWN,
                        (MouseButton::Left, false) => MOUSEEVENTF_LEFTUP,
                        (MouseButton::Right, true) => MOUSEEVENTF_RIGHTDOWN,
                        (MouseButton::Right, false) => MOUSEEVENTF_RIGHTUP,
                        (MouseButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
                        (MouseButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
                        (MouseButton::X1, true) => MOUSEEVENTF_XDOWN,
                        (MouseButton::X1, false) => MOUSEEVENTF_XUP,
                        (MouseButton::X2, true) => MOUSEEVENTF_XDOWN,
                        (MouseButton::X2, false) => MOUSEEVENTF_XUP,
                    };
                    let x_data = match button {
                        MouseButton::X1 => 1u32,
                        MouseButton::X2 => 2u32,
                        _ => 0,
                    };
                    let input = INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: 0,
                                dy: 0,
                                mouseData: x_data,
                                dwFlags: flags,
                                time: 0,
                                dwExtraInfo: 0,
                            },
                        },
                    };
                    let sent = SendInput(&[input], mem::size_of::<INPUT>() as i32);
                    if sent == 0 {
                        return Err(InjectError::SendInput("MouseButton".into()));
                    }
                }
                InputEvent::MouseWheel { dx, dy } => {
                    // Vertical wheel
                    if dy != 0 {
                        let input = INPUT {
                            r#type: INPUT_MOUSE,
                            Anonymous: INPUT_0 {
                                mi: MOUSEINPUT {
                                    dx: 0,
                                    dy: 0,
                                    mouseData: dy as u32,
                                    dwFlags: MOUSEEVENTF_WHEEL,
                                    time: 0,
                                    dwExtraInfo: 0,
                                },
                            },
                        };
                        SendInput(&[input], mem::size_of::<INPUT>() as i32);
                    }
                    if dx != 0 {
                        let input = INPUT {
                            r#type: INPUT_MOUSE,
                            Anonymous: INPUT_0 {
                                mi: MOUSEINPUT {
                                    dx: 0,
                                    dy: 0,
                                    mouseData: dx as u32,
                                    dwFlags: MOUSEEVENTF_HWHEEL,
                                    time: 0,
                                    dwExtraInfo: 0,
                                },
                            },
                        };
                        SendInput(&[input], mem::size_of::<INPUT>() as i32);
                    }
                }
                InputEvent::Key { scancode, pressed } => {
                    let mut flags = KEYEVENTF_SCANCODE;
                    if !pressed {
                        flags |= KEYEVENTF_KEYUP;
                    }
                    // Extended keys (arrow keys, etc.) use 0xE0 prefix in scancode.
                    if scancode & 0xFF00 == 0xE000 {
                        flags |= KEYEVENTF_EXTENDEDKEY;
                    }
                    let input = INPUT {
                        r#type: INPUT_KEYBOARD,
                        Anonymous: INPUT_0 {
                            ki: KEYBDINPUT {
                                wVk: VIRTUAL_KEY(0),
                                wScan: (scancode & 0xFF) as u16,
                                dwFlags: flags,
                                time: 0,
                                dwExtraInfo: 0,
                            },
                        },
                    };
                    let sent = SendInput(&[input], mem::size_of::<INPUT>() as i32);
                    if sent == 0 {
                        return Err(InjectError::SendInput("Key".into()));
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injector_constructs() {
        let _inj = SendInputInjector::new();
    }
}
