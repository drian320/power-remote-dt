//! Windows clipboard text helpers. Read/write the system clipboard's
//! CF_UNICODETEXT format via Win32 APIs.

use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber, OpenClipboard,
    SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;

/// Maximum clipboard text length we'll transmit, in bytes (UTF-8).
pub const MAX_CLIPBOARD_BYTES: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    #[error("OpenClipboard failed")]
    OpenFailed,
    #[error("windows: {0}")]
    Windows(String),
    #[error("clipboard text too large: {0} bytes")]
    TooLarge(usize),
    #[error("no text in clipboard")]
    NoText,
}

/// Try to open the clipboard, retrying a handful of times to handle
/// transient contention with other clipboard listeners (common on Windows:
/// e.g. Explorer / cloud paste tools hold the clipboard briefly).
unsafe fn open_clipboard_with_retry() -> Result<(), ClipboardError> {
    const ATTEMPTS: u32 = 10;
    const SLEEP: std::time::Duration = std::time::Duration::from_millis(10);
    for _ in 0..ATTEMPTS {
        if OpenClipboard(HWND::default()).is_ok() {
            return Ok(());
        }
        std::thread::sleep(SLEEP);
    }
    Err(ClipboardError::OpenFailed)
}

/// Read current clipboard text as UTF-8. Returns `NoText` if no text is available.
pub fn read_clipboard_text() -> Result<String, ClipboardError> {
    unsafe {
        open_clipboard_with_retry()?;
        let result = read_inner();
        let _ = CloseClipboard();
        result
    }
}

unsafe fn read_inner() -> Result<String, ClipboardError> {
    let handle = GetClipboardData(CF_UNICODETEXT.0 as u32).map_err(|_| ClipboardError::NoText)?;
    if handle.0.is_null() {
        return Err(ClipboardError::NoText);
    }
    let hglobal = HGLOBAL(handle.0);
    let ptr = GlobalLock(hglobal) as *const u16;
    if ptr.is_null() {
        return Err(ClipboardError::Windows("GlobalLock returned null".into()));
    }
    // Find null terminator. Cap search at MAX_CLIPBOARD_BYTES u16 code units
    // (far more generous than the byte cap — the UTF-8 result is what we
    // ultimately size-check on the sender side).
    let max_u16_units = MAX_CLIPBOARD_BYTES;
    let mut len = 0usize;
    while *ptr.add(len) != 0 {
        len += 1;
        if len > max_u16_units {
            let _ = GlobalUnlock(hglobal);
            return Err(ClipboardError::TooLarge(len * 2));
        }
    }
    let slice = std::slice::from_raw_parts(ptr, len);
    let text = String::from_utf16_lossy(slice);
    let _ = GlobalUnlock(hglobal);
    Ok(text)
}

/// Returns a monotonic counter that increments each time the clipboard
/// changes, system-wide. Cheap to call (no clipboard handle needed) so the
/// watcher loop can poll at high frequency and only `read_clipboard_text`
/// when the counter moved.
pub fn clipboard_sequence_number() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

/// Set the clipboard text. Caller must ensure the text fits within
/// MAX_CLIPBOARD_BYTES; otherwise returns TooLarge.
pub fn write_clipboard_text(text: &str) -> Result<(), ClipboardError> {
    if text.len() > MAX_CLIPBOARD_BYTES {
        return Err(ClipboardError::TooLarge(text.len()));
    }
    // UTF-16 encode (null-terminated).
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        open_clipboard_with_retry()?;
        let result = (|| -> Result<(), ClipboardError> {
            EmptyClipboard()
                .map_err(|e| ClipboardError::Windows(format!("EmptyClipboard: {e}")))?;
            let bytes = wide.len() * std::mem::size_of::<u16>();
            let hmem = GlobalAlloc(GMEM_MOVEABLE, bytes)
                .map_err(|e| ClipboardError::Windows(format!("GlobalAlloc: {e}")))?;
            let ptr = GlobalLock(hmem) as *mut u16;
            if ptr.is_null() {
                return Err(ClipboardError::Windows("GlobalLock returned null".into()));
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            let _ = GlobalUnlock(hmem);
            SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(hmem.0))
                .map_err(|e| ClipboardError::Windows(format!("SetClipboardData: {e}")))?;
            // Ownership of hmem is transferred to the clipboard.
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}

#[cfg(test)]
mod tests {
    //! NOTE: these tests MUTATE THE ACTUAL SYSTEM CLIPBOARD on the test
    //! machine. They are gated behind `#[ignore]` so `cargo test` doesn't
    //! clobber the user's clipboard in regular runs / CI. Run them with:
    //!
    //!     cargo test -p prdt-input-win -- --ignored
    use super::*;

    #[test]
    #[ignore = "mutates system clipboard; run with --ignored"]
    fn roundtrip_simple_ascii() {
        let text = "hello clipboard from phase 3b";
        write_clipboard_text(text).expect("write");
        let back = read_clipboard_text().expect("read");
        assert_eq!(back, text);
    }

    #[test]
    #[ignore = "mutates system clipboard; run with --ignored"]
    fn roundtrip_unicode() {
        let text = "こんにちは 🌸 clipboard";
        write_clipboard_text(text).expect("write");
        let back = read_clipboard_text().expect("read");
        assert_eq!(back, text);
    }

    #[test]
    fn too_large_rejected() {
        let big = "A".repeat(MAX_CLIPBOARD_BYTES + 1);
        let r = write_clipboard_text(&big);
        assert!(matches!(r, Err(ClipboardError::TooLarge(_))));
    }

    #[test]
    fn sequence_number_returns_some_value() {
        // GetClipboardSequenceNumber always succeeds and returns a u32. We
        // can't assert a specific value, but we can verify it's callable and
        // two back-to-back calls agree (no change expected).
        let a = clipboard_sequence_number();
        let b = clipboard_sequence_number();
        assert_eq!(a, b);
    }
}
