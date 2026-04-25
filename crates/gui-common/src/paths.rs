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
