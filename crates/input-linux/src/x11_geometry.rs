//! Virtual desktop rect via XRandR. L1 enforces (0, 0) origin —
//! non-zero-origin multi-monitor topologies are warned about and
//! collapsed to a (0, 0)-anchored rect; full support is L2.

use crate::error::LinuxInputError;
use prdt_protocol::MonitorRect;

const DEFAULT_FALLBACK: MonitorRect = MonitorRect {
    left: 0,
    top: 0,
    right: 1920,
    bottom: 1080,
};

pub fn virtual_desktop_rect() -> MonitorRect {
    match query() {
        Ok(rect) => rect,
        Err(e) => {
            tracing::warn!(error = %e, "RandR query failed, using default 1920x1080@(0,0)");
            DEFAULT_FALLBACK
        }
    }
}

fn query() -> Result<MonitorRect, LinuxInputError> {
    use x11rb::connection::Connection;
    use x11rb::protocol::randr::ConnectionExt as _;
    let (conn, screen_num) =
        x11rb::connect(None).map_err(|e| LinuxInputError::X11Connect(e.to_string()))?;
    let setup = conn.setup();
    let screen = &setup.roots[screen_num];
    let resources = conn
        .randr_get_screen_resources_current(screen.root)
        .map_err(|e| LinuxInputError::X11Connect(format!("randr_get_resources req: {e}")))?
        .reply()
        .map_err(|e| LinuxInputError::X11Connect(format!("randr_get_resources reply: {e}")))?;
    if resources.crtcs.is_empty() {
        return Err(LinuxInputError::NoCrtcs);
    }
    let mut left = i32::MAX;
    let mut top = i32::MAX;
    let mut right = i32::MIN;
    let mut bottom = i32::MIN;
    for crtc in &resources.crtcs {
        let info = match conn
            .randr_get_crtc_info(*crtc, x11rb::CURRENT_TIME)
            .ok()
            .and_then(|c| c.reply().ok())
        {
            Some(i) => i,
            None => continue,
        };
        if info.width == 0 || info.height == 0 {
            continue; // disconnected output
        }
        let x = info.x as i32;
        let y = info.y as i32;
        let w = info.width as i32;
        let h = info.height as i32;
        if x < left {
            left = x;
        }
        if y < top {
            top = y;
        }
        if x + w > right {
            right = x + w;
        }
        if y + h > bottom {
            bottom = y + h;
        }
    }
    if left == i32::MAX {
        return Err(LinuxInputError::NoCrtcs);
    }
    if left != 0 || top != 0 {
        tracing::warn!(
            left,
            top,
            "non-zero-origin virtual desktop detected — L1 collapses to (0,0)-anchored rect; multi-monitor with non-primary topology not supported until L2"
        );
        // Collapse: make rect start at (0, 0) with the original span.
        return Ok(MonitorRect {
            left: 0,
            top: 0,
            right: right - left,
            bottom: bottom - top,
        });
    }
    Ok(MonitorRect {
        left,
        top,
        right,
        bottom,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_fallback_is_1920x1080_at_origin() {
        assert_eq!(DEFAULT_FALLBACK.left, 0);
        assert_eq!(DEFAULT_FALLBACK.top, 0);
        assert_eq!(DEFAULT_FALLBACK.right, 1920);
        assert_eq!(DEFAULT_FALLBACK.bottom, 1080);
    }

    #[test]
    #[ignore = "requires X11 with RandR. Run with: cargo test -p prdt-input-linux -- --ignored"]
    fn live_virtual_desktop_rect_returns_sensible_value() {
        let r = virtual_desktop_rect();
        assert!(r.right > r.left);
        assert!(r.bottom > r.top);
    }
}
