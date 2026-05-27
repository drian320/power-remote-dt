//! Windows host-only privilege elevation.
//!
//! `prdt.exe` ships as `asInvoker` so viewer-only use never triggers UAC.
//! But the host injects remote input via `SendInput`, and Windows UIPI
//! silently drops synthetic input aimed at higher-integrity windows (Task
//! Manager, UAC dialogs, anything "run as administrator"). So when the user
//! starts the host listener we relaunch the GUI elevated (`runas`) with
//! `--host-autostart`; the elevated instance auto-starts the listener and the
//! original non-elevated window closes.

use std::os::windows::ffi::OsStrExt;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::{IsUserAnAdmin, ShellExecuteExW, SHELLEXECUTEINFOW};

/// True if the current process is running elevated (admin integrity).
pub fn is_elevated() -> bool {
    // SAFETY: IsUserAnAdmin takes no arguments and only reads the calling
    // process token; it has no preconditions.
    unsafe { IsUserAnAdmin().as_bool() }
}

/// Relaunch this exe elevated via the shell "runas" verb, asking the new
/// instance to auto-start the host listener. Returns Err if the user declines
/// the UAC prompt or the shell call fails. On success the caller should close
/// the current (non-elevated) window — the elevated copy takes over hosting.
pub fn relaunch_elevated_for_host() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    // Wide, NUL-terminated path; must outlive the synchronous ShellExecuteExW
    // call below (it borrows the pointer for the duration of the call only).
    let exe_w: Vec<u16> = exe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe_w.as_ptr()),
        lpParameters: w!("--host-autostart"),
        nShow: 1, // SW_SHOWNORMAL
        hwnd: HWND::default(),
        ..Default::default()
    };

    // SAFETY: `info` is fully initialized with a valid cbSize; `exe_w` and the
    // static `w!` strings remain alive across this synchronous call.
    unsafe { ShellExecuteExW(&mut info) }?;
    Ok(())
}
