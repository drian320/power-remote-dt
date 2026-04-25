//! Phase 4 G3 auto-start (Windows HKCU\...\Run). On non-Windows this is a
//! no-op so `gui-host` still compiles cross-platform — Linux / macOS get
//! native auto-start in Phase 1+ via `.desktop` / LaunchAgent.

#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const VALUE_NAME: &str = "PrdtHost";

/// Toggle the per-user Run-on-login registration for the host binary.
/// On Windows, writes / deletes the value at
/// `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\PrdtHost`.
/// The value is `"<current_exe>" --headless` so the host comes up
/// minimized to tray rather than full-screen GUI.
#[cfg(windows)]
pub fn set_enabled(on: bool) -> std::io::Result<()> {
    use winreg::enums::*;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu.create_subkey(RUN_KEY)?;
    if on {
        let exe = std::env::current_exe()?;
        let cmd = format!("\"{}\" --headless", exe.display());
        key.set_value(VALUE_NAME, &cmd)?;
    } else {
        match key.delete_value(VALUE_NAME) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn set_enabled(_on: bool) -> std::io::Result<()> {
    Ok(())
}

/// Returns true if the registry value exists. Always false on non-Windows.
#[cfg(windows)]
pub fn is_enabled() -> bool {
    use winreg::enums::*;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    hkcu.open_subkey(RUN_KEY)
        .and_then(|k| k.get_value::<String, _>(VALUE_NAME))
        .is_ok()
}

#[cfg(not(windows))]
pub fn is_enabled() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip writing and clearing the registry value. Skipped unless
    /// PRDT_TEST_AUTOSTART=1 is set, because this writes to the user's real
    /// HKCU and will leave residue on accidental test runs.
    #[cfg(windows)]
    #[test]
    fn enable_then_disable_round_trip() {
        if std::env::var("PRDT_TEST_AUTOSTART").is_err() {
            eprintln!("skipping: set PRDT_TEST_AUTOSTART=1 to opt in");
            return;
        }
        let _ = set_enabled(false);
        assert!(!is_enabled(), "precondition: registry value should be absent");
        set_enabled(true).expect("set_enabled(true)");
        assert!(is_enabled(), "after enable, value should exist");
        set_enabled(false).expect("set_enabled(false)");
        assert!(!is_enabled(), "after disable, value should be gone");
    }

    #[cfg(windows)]
    #[test]
    fn disable_when_absent_is_ok() {
        if std::env::var("PRDT_TEST_AUTOSTART").is_err() {
            eprintln!("skipping: set PRDT_TEST_AUTOSTART=1 to opt in");
            return;
        }
        let _ = set_enabled(false);
        set_enabled(false).expect("idempotent disable");
    }

    #[cfg(not(windows))]
    #[test]
    fn no_op_on_non_windows() {
        set_enabled(true).expect("no-op succeeds");
        assert!(!is_enabled());
        set_enabled(false).expect("no-op succeeds");
    }
}
