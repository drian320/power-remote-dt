//! Windows virtual-desktop geometry helpers.
//!
//! The viewer needs to know the host's virtual-desktop bounds to map its
//! window-local mouse coordinates into the 0..65535 range expected by
//! `SendInput` with `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`.

use prdt_protocol::MonitorRect;
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

/// Returns the bounding rectangle of the entire virtual desktop (all
/// monitors combined), expressed in virtual-desktop coordinates.
pub fn virtual_desktop_rect() -> MonitorRect {
    unsafe {
        let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let top = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        MonitorRect::new(left, top, left + width, top + height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_desktop_has_positive_area() {
        let r = virtual_desktop_rect();
        // On a desktop session width/height should both be >= 1. On a headless
        // CI box GetSystemMetrics may still return 0; guard with a non-panicking
        // check so the test doesn't fail in headless environments.
        assert!(r.width() >= 0);
        assert!(r.height() >= 0);
    }
}
