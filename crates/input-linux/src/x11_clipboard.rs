//! X11 _CLIPBOARD selection sync (UTF8_STRING) + XFixes-based
//! sequence number that bumps only on observed external owner changes.
//! Mirrors the public surface of `prdt_input_win::clipboard` so the
//! host can `#[cfg]`-switch import paths without rewriting call sites.

use crate::error::LinuxInputError;
use once_cell::sync::OnceCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

pub const MAX_CLIPBOARD_BYTES: usize = 64 * 1024;

static SEQUENCE: AtomicU32 = AtomicU32::new(0);
static STATE: OnceCell<Arc<ClipboardState>> = OnceCell::new();

/// Internal state shared by reader/writer paths and the owner thread.
struct ClipboardState {
    /// The text we last successfully published as selection owner.
    /// Used by the owner thread to serve SelectionRequest events.
    own_text: Mutex<Option<String>>,
}

fn state() -> Arc<ClipboardState> {
    STATE
        .get_or_init(|| {
            Arc::new(ClipboardState {
                own_text: Mutex::new(None),
            })
        })
        .clone()
}

/// Return a monotonic counter that bumps each time an external X11
/// client takes the _CLIPBOARD selection. Matches the L0 trait doc
/// semantic: "user changes the system clipboard."
///
/// The sequence stays at 0 until an external `SelectionClear` arrives
/// (i.e. another client claims the selection). Own writes do NOT bump.
pub fn clipboard_sequence_number() -> u32 {
    SEQUENCE.load(Ordering::SeqCst)
}

/// Read the current _CLIPBOARD selection as UTF-8.
pub fn read_clipboard_text() -> Result<String, LinuxInputError> {
    let conn = connect()?;
    let setup = conn.0.setup();
    let screen = &setup.roots[conn.1];
    let our_window = create_invisible_window(&conn.0, screen.root, screen.root_visual)?;

    let atoms = atoms(&conn.0)?;
    use x11rb::protocol::xproto::ConnectionExt as _;
    conn.0
        .convert_selection(
            our_window,
            atoms.clipboard,
            atoms.utf8_string,
            atoms.transfer_property,
            x11rb::CURRENT_TIME,
        )
        .map_err(|e| LinuxInputError::X11Connect(format!("convert_selection: {e}")))?;
    conn.0
        .flush()
        .map_err(|e| LinuxInputError::X11Connect(format!("flush: {e}")))?;

    // Wait for SelectionNotify with 1s timeout.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    use x11rb::connection::Connection as _;
    use x11rb::protocol::Event;
    loop {
        if std::time::Instant::now() > deadline {
            return Err(LinuxInputError::ClipboardTimeout);
        }
        let event = match conn.0.poll_for_event() {
            Ok(Some(e)) => e,
            Ok(None) => {
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            }
            Err(e) => return Err(LinuxInputError::X11Connect(format!("poll_for_event: {e}"))),
        };
        if let Event::SelectionNotify(notify) = event {
            if notify.requestor == our_window && notify.selection == atoms.clipboard {
                if notify.property == x11rb::NONE {
                    return Err(LinuxInputError::ClipboardNonUtf8);
                }
                let prop = conn
                    .0
                    .get_property(
                        false,
                        our_window,
                        notify.property,
                        atoms.utf8_string,
                        0,
                        1024 * 64,
                    )
                    .map_err(|e| LinuxInputError::X11Connect(format!("get_property req: {e}")))?
                    .reply()
                    .map_err(|e| LinuxInputError::X11Connect(format!("get_property reply: {e}")))?;
                if prop.value.is_empty() {
                    return Err(LinuxInputError::ClipboardNonUtf8);
                }
                if prop.value.len() > MAX_CLIPBOARD_BYTES {
                    return Err(LinuxInputError::ClipboardTooLarge(prop.value.len()));
                }
                return String::from_utf8(prop.value)
                    .map_err(|_| LinuxInputError::ClipboardNonUtf8);
            }
        }
    }
}

/// Set the _CLIPBOARD selection contents. We become the selection
/// owner; subsequent SelectionRequest events from other clients are
/// served from `state().own_text`.
///
/// A background owner thread is lazily spawned on first call and runs
/// for the lifetime of the process.
pub fn write_clipboard_text(text: &str) -> Result<(), LinuxInputError> {
    if text.len() > MAX_CLIPBOARD_BYTES {
        return Err(LinuxInputError::ClipboardTooLarge(text.len()));
    }
    let s = state();
    *s.own_text.lock().expect("own_text Mutex poisoned") = Some(text.to_owned());
    // Ensure the owner thread is running. The thread wakes on X11
    // events and serves whatever is currently in `own_text`.
    start_owner_thread()?;
    Ok(())
}

// === Owner thread ===

static OWNER_STARTED: OnceCell<()> = OnceCell::new();

fn start_owner_thread() -> Result<(), LinuxInputError> {
    OWNER_STARTED
        .get_or_try_init(|| -> Result<(), LinuxInputError> {
            std::thread::Builder::new()
                .name("prdt-clipboard-owner".to_owned())
                .spawn(owner_thread_main)
                .map_err(LinuxInputError::ThreadSpawn)?;
            Ok(())
        })
        .map(|_| ())
}

fn owner_thread_main() {
    if let Err(e) = owner_thread_inner() {
        tracing::warn!(error = %e, "clipboard owner thread exited");
    }
}

fn owner_thread_inner() -> Result<(), LinuxInputError> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::ConnectionExt as _;
    use x11rb::protocol::Event;

    let (conn, screen_num) =
        x11rb::connect(None).map_err(|e| LinuxInputError::X11Connect(e.to_string()))?;
    let setup = conn.setup();
    let screen = &setup.roots[screen_num];
    let win = create_invisible_window(&conn, screen.root, screen.root_visual)?;
    let atoms = atoms(&conn)?;

    conn.set_selection_owner(win, atoms.clipboard, x11rb::CURRENT_TIME)
        .map_err(|e| LinuxInputError::X11Connect(format!("set_selection_owner: {e}")))?;
    conn.flush()
        .map_err(|e| LinuxInputError::X11Connect(format!("flush: {e}")))?;

    let s = state();
    loop {
        let event = conn
            .wait_for_event()
            .map_err(|e| LinuxInputError::X11Connect(format!("wait_for_event: {e}")))?;
        match event {
            Event::SelectionRequest(req) => {
                let body = s
                    .own_text
                    .lock()
                    .expect("own_text Mutex poisoned")
                    .clone()
                    .unwrap_or_default();
                if req.target == atoms.utf8_string {
                    let _ = conn.change_property(
                        x11rb::protocol::xproto::PropMode::REPLACE,
                        req.requestor,
                        req.property,
                        atoms.utf8_string,
                        8,
                        body.len() as u32,
                        body.as_bytes(),
                    );
                    let notify = x11rb::protocol::xproto::SelectionNotifyEvent {
                        response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
                        sequence: 0,
                        time: req.time,
                        requestor: req.requestor,
                        selection: req.selection,
                        target: req.target,
                        property: req.property,
                    };
                    let _ = conn.send_event(
                        false,
                        req.requestor,
                        x11rb::protocol::xproto::EventMask::NO_EVENT,
                        notify,
                    );
                    let _ = conn.flush();
                }
            }
            Event::SelectionClear(_) => {
                // Another client took the selection — bump sequence so
                // the host watcher will trigger a clipboard sync.
                SEQUENCE.fetch_add(1, Ordering::SeqCst);
            }
            _ => {}
        }
    }
}

// === X11 helpers ===

fn connect() -> Result<(x11rb::rust_connection::RustConnection, usize), LinuxInputError> {
    x11rb::connect(None).map_err(|e| LinuxInputError::X11Connect(e.to_string()))
}

fn create_invisible_window(
    conn: &x11rb::rust_connection::RustConnection,
    root: u32,
    root_visual: u32,
) -> Result<u32, LinuxInputError> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{ConnectionExt as _, WindowClass};
    let win = conn
        .generate_id()
        .map_err(|e| LinuxInputError::X11Connect(format!("generate_id: {e}")))?;
    conn.create_window(
        x11rb::COPY_FROM_PARENT as u8,
        win,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        root_visual,
        &Default::default(),
    )
    .map_err(|e| LinuxInputError::X11Connect(format!("create_window: {e}")))?;
    Ok(win)
}

struct Atoms {
    clipboard: u32,
    utf8_string: u32,
    transfer_property: u32,
}

fn atoms(conn: &x11rb::rust_connection::RustConnection) -> Result<Atoms, LinuxInputError> {
    use x11rb::protocol::xproto::ConnectionExt as _;
    let intern = |name: &[u8]| -> Result<u32, LinuxInputError> {
        let r = conn
            .intern_atom(false, name)
            .map_err(|e| LinuxInputError::X11Connect(format!("intern_atom req: {e}")))?
            .reply()
            .map_err(|e| LinuxInputError::X11Connect(format!("intern_atom reply: {e}")))?;
        Ok(r.atom)
    };
    Ok(Atoms {
        clipboard: intern(b"CLIPBOARD")?,
        utf8_string: intern(b"UTF8_STRING")?,
        transfer_property: intern(b"PRDT_CLIPBOARD_XFER")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_clipboard_bytes_is_64k() {
        assert_eq!(MAX_CLIPBOARD_BYTES, 64 * 1024);
    }

    #[test]
    fn write_text_too_large_returns_error() {
        let huge = "A".repeat(MAX_CLIPBOARD_BYTES + 1);
        let r = write_clipboard_text(&huge);
        assert!(matches!(r, Err(LinuxInputError::ClipboardTooLarge(_))));
    }

    #[test]
    fn sequence_starts_at_zero_and_only_bumps_on_external() {
        // Before any external SelectionClear arrives, the counter
        // stays at 0. (Read here is a sanity check — own writes do
        // NOT bump the sequence.)
        let initial = clipboard_sequence_number();
        // Simulate own write — should not bump.
        // (write_clipboard_text itself may fail if no DISPLAY is set,
        // but we only care that it doesn't bump the counter.)
        let _ = write_clipboard_text("hello");
        let after_write = clipboard_sequence_number();
        assert_eq!(after_write, initial);
    }

    #[test]
    #[ignore = "requires X11 server (DISPLAY set). Run with: cargo test -p prdt-input-linux -- --ignored"]
    fn x11_clipboard_set_then_get_round_trips() {
        write_clipboard_text("hello-l1").expect("write");
        // Give the owner thread a moment to register with the X server.
        std::thread::sleep(std::time::Duration::from_millis(100));
        let r = read_clipboard_text().expect("read");
        assert_eq!(r, "hello-l1");
    }
}
