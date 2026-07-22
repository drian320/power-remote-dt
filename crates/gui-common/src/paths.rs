//! OS-aware paths under `dirs::config_dir()/prdt/`.

use std::path::PathBuf;

/// Root config directory: `%APPDATA%\prdt\` on Windows, `$XDG_CONFIG_HOME/prdt/` on Linux.
pub fn config_root() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("prdt"))
}

/// Default path for `config.toml`.
pub fn default_config_path() -> Option<PathBuf> {
    config_root().map(|d| d.join("config.toml"))
}

/// Directory for rolling log files: `config_root()/logs`. The unified client
/// GUI writes its daily-rolling `prdt-gui.log` here so that the in-process host
/// listener's logs survive a double-click / elevated-relaunch launch, where
/// stderr is discarded. Callers create the directory on first use.
pub fn logs_dir() -> Option<PathBuf> {
    config_root().map(|d| d.join("logs"))
}

/// Default path for the identity record (`identity.toml`) — the persisted
/// `{host_id, key_fingerprint, server_authority, provisioning_state}` produced
/// by offline-first provisioning (AC-9).
pub fn default_identity_path() -> Option<PathBuf> {
    config_root().map(|d| d.join("identity.toml"))
}

/// Default path for the host auth config (`host-auth.toml`), which holds the
/// PIN hash + plaintext and auth mode. Mirrors `prdt-host`'s own default so the
/// GUI and the host listener read/write the same file.
pub fn default_host_auth_path() -> Option<PathBuf> {
    config_root().map(|d| d.join("host-auth.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_root_ends_with_prdt() {
        let p = config_root().expect("OS has a config dir");
        assert!(
            p.ends_with("prdt"),
            "config_root() should end with 'prdt', got {p:?}"
        );
    }
}
