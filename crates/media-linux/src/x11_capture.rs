//! Root-window screen capture via X11. Tries MIT-SHM (`xcb_shm_get_image`);
//! on extension-missing or shmget failure, falls back to plain
//! `xcb_get_image`. WSLg's X server may not always expose MIT-SHM.

use crate::error::LinuxMediaError;
use std::sync::Arc;
use x11rb::connection::Connection;
use x11rb::protocol::shm::ConnectionExt as _;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, Window};
use x11rb::rust_connection::RustConnection;

/// Maximum capture dimensions. Matches OpenH264's SW encoder limit
/// (3840x2160 horizontal, 2160x3840 vertical). When the X11 root is
/// larger than this (multi-monitor WSLg, large virtual desktops) the
/// capturer clips to the top-left subrect.
pub const MAX_CAPTURE_W: u32 = 3840;
/// See [`MAX_CAPTURE_W`].
pub const MAX_CAPTURE_H: u32 = 2160;

pub struct X11ShmCapturer {
    conn: Arc<RustConnection>,
    root: Window,
    width: u32,
    height: u32,
    use_shm: bool,
    // SHM state — `None` when use_shm == false.
    shm: Option<ShmSegment>,
}

struct ShmSegment {
    seg_id: u32, // xcb shm segment xid
    shmid: i32,  // SysV shmget id (for shmctl IPC_RMID + shmdt)
    addr: *mut libc::c_void,
    size: usize,
}

// Safety: ShmSegment owns a SysV SHM region accessed only from `&mut self`
// in `grab_into`. The pointer never escapes this struct.
unsafe impl Send for X11ShmCapturer {}

impl X11ShmCapturer {
    pub fn new() -> Result<Self, LinuxMediaError> {
        let (conn, screen_num) =
            x11rb::connect(None).map_err(|e| LinuxMediaError::X11Connect(e.to_string()))?;
        let conn = Arc::new(conn);
        let setup = conn.setup();
        let screen = &setup.roots[screen_num];
        let root = screen.root;
        let geometry = conn
            .get_geometry(root)
            .map_err(|e| LinuxMediaError::X11Connect(format!("get_geometry req: {e}")))?
            .reply()
            .map_err(|e| LinuxMediaError::X11Connect(format!("get_geometry reply: {e}")))?;
        // Clip to OpenH264 SW-encoder max (3840x2160). On WSLg the X11 root
        // is the entire virtual desktop, which can exceed this on multi-
        // monitor setups (e.g. 7680x2160). Capturing the top-left subrect
        // gives a working session at the cost of losing the right-side
        // monitor — proper per-monitor selection is L2.
        let width = (geometry.width as u32).min(MAX_CAPTURE_W);
        let height = (geometry.height as u32).min(MAX_CAPTURE_H);

        // Probe MIT-SHM extension. Scope the Cookie so its borrow of
        // `conn` ends before we move `conn` into `Self`.
        let use_shm = {
            match conn.shm_query_version() {
                Ok(c) => c.reply().is_ok(),
                Err(_) => false,
            }
        };

        let shm = if use_shm {
            match alloc_shm(&conn, width, height) {
                Ok(seg) => Some(seg),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "MIT-SHM allocation failed; falling back to plain XGetImage"
                    );
                    None
                }
            }
        } else {
            tracing::warn!("MIT-SHM extension unavailable; using plain XGetImage");
            None
        };
        let use_shm = shm.is_some();

        Ok(Self {
            conn,
            root,
            width,
            height,
            use_shm,
            shm,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn grab_into(&mut self, out: &mut [u8]) -> Result<(), LinuxMediaError> {
        let expected = (self.width as usize) * (self.height as usize) * 4;
        if out.len() != expected {
            return Err(LinuxMediaError::InvalidDimensions(self.width, self.height));
        }
        if self.use_shm {
            self.grab_shm(out)
        } else {
            self.grab_get_image(out)
        }
    }

    fn grab_shm(&self, out: &mut [u8]) -> Result<(), LinuxMediaError> {
        let shm = self.shm.as_ref().expect("use_shm true implies Some(shm)");
        let cookie = self
            .conn
            .shm_get_image(
                self.root,
                0,
                0,
                self.width as u16,
                self.height as u16,
                u32::MAX,
                ImageFormat::Z_PIXMAP.into(),
                shm.seg_id,
                0,
            )
            .map_err(|e| LinuxMediaError::X11Connect(format!("shm_get_image req: {e}")))?;
        cookie
            .reply()
            .map_err(|e| LinuxMediaError::X11Connect(format!("shm_get_image reply: {e}")))?;
        // Safety: shm.addr points to `shm.size` bytes of valid mapped memory
        // that we own; we copy out exactly `expected` bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(shm.addr as *const u8, out.as_mut_ptr(), out.len());
        }
        Ok(())
    }

    fn grab_get_image(&self, out: &mut [u8]) -> Result<(), LinuxMediaError> {
        let cookie = self
            .conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                self.root,
                0,
                0,
                self.width as u16,
                self.height as u16,
                u32::MAX,
            )
            .map_err(|e| LinuxMediaError::X11Connect(format!("get_image req: {e}")))?;
        let reply = cookie
            .reply()
            .map_err(|_| LinuxMediaError::XGetImageFailed)?;
        if reply.data.len() != out.len() {
            return Err(LinuxMediaError::InvalidDimensions(self.width, self.height));
        }
        out.copy_from_slice(&reply.data);
        Ok(())
    }
}

impl crate::capture_source::CaptureSource for X11ShmCapturer {
    fn geometry(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn capture_into(
        &mut self,
        out: &mut Vec<u8>,
    ) -> Result<(), crate::capture_source::CaptureSourceError> {
        let n = (self.width as usize) * (self.height as usize) * 4;
        out.resize(n, 0);
        self.grab_into(out.as_mut_slice()).map_err(|e| {
            crate::capture_source::CaptureSourceError::Terminal {
                backend: "linux-x11shm",
                reason: e.to_string(),
            }
        })
    }
}

impl Drop for X11ShmCapturer {
    fn drop(&mut self) {
        if let Some(seg) = self.shm.take() {
            // Detach from server.
            let _ = self.conn.shm_detach(seg.seg_id);
            // shmdt + shmctl IPC_RMID.
            unsafe {
                libc::shmdt(seg.addr);
                libc::shmctl(seg.shmid, libc::IPC_RMID, std::ptr::null_mut());
            }
        }
    }
}

fn alloc_shm(
    conn: &RustConnection,
    width: u32,
    height: u32,
) -> Result<ShmSegment, LinuxMediaError> {
    let size = (width as usize) * (height as usize) * 4;
    // Allocate SysV SHM.
    let shmid = unsafe { libc::shmget(libc::IPC_PRIVATE, size, 0o600 | libc::IPC_CREAT) };
    if shmid < 0 {
        return Err(LinuxMediaError::ShmUnavailable);
    }
    let addr = unsafe { libc::shmat(shmid, std::ptr::null(), 0) };
    if addr == (usize::MAX as *mut libc::c_void) {
        unsafe {
            libc::shmctl(shmid, libc::IPC_RMID, std::ptr::null_mut());
        }
        return Err(LinuxMediaError::ShmUnavailable);
    }
    // Attach to X server.
    let seg_id = conn
        .generate_id()
        .map_err(|e| LinuxMediaError::X11Connect(format!("shm generate_id: {e}")))?;
    conn.shm_attach(seg_id, shmid as u32, false)
        .map_err(|e| LinuxMediaError::X11Connect(format!("shm_attach req: {e}")))?
        .check()
        .map_err(|e| LinuxMediaError::X11Connect(format!("shm_attach check: {e}")))?;
    Ok(ShmSegment {
        seg_id,
        shmid,
        addr,
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_size_constant() {
        assert_eq!(1920u32 * 1080 * 4, 8_294_400);
    }

    #[test]
    #[ignore = "requires real X11 connection — run on WSL2 with: cargo test -p prdt-media-linux -- --ignored"]
    fn xshm_capture_one_frame() {
        let mut cap = X11ShmCapturer::new().expect("X11 connect");
        let mut buf = vec![0u8; (cap.width() * cap.height() * 4) as usize];
        cap.grab_into(&mut buf).expect("grab");
        // Sanity: capturing root should give us non-uniform pixels in
        // typical conditions, but we only assert the call returns Ok and
        // the buffer length matches.
        assert_eq!(buf.len(), (cap.width() * cap.height() * 4) as usize);
    }
}
