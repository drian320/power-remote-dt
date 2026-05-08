//! Persistent configuration shared by host and viewer GUIs.
//!
//! Schema is documented in
//! `docs/superpowers/specs/2026-04-25-phase4-g1-egui-foundation-design.md`.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GuiConfig {
    /// Locale: "" = auto-detect from OS, "en" / "ja" = forced.
    #[serde(default)]
    pub locale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub gui: GuiConfig,
    #[serde(default)]
    pub host: HostConfig,
    #[serde(default)]
    pub viewer: ViewerConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostConfig {
    pub bind: String,
    pub monitor: u32,
    pub bitrate_mbps: u32,
    pub outgoing_dir: PathBuf,
    #[serde(default)]
    pub signaling_url: String,
    pub host_id_file: PathBuf,
    pub key_file: PathBuf,
    #[serde(default)]
    pub auto_start: bool,
    /// Encoder backend choice. "auto" picks NVENC on NVIDIA, MF
    /// elsewhere. Other values: "nvenc", "mf".
    #[serde(default = "default_encoder_choice")]
    pub encoder: String,
}

fn default_encoder_choice() -> String {
    "auto".into()
}

fn default_host_key_path() -> std::path::PathBuf {
    if let Some(base) = dirs::data_local_dir() {
        let dir = base.join("prdt");
        let _ = std::fs::create_dir_all(&dir);
        return dir.join("host-key.bin");
    }
    std::path::PathBuf::from("host-key.bin")
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:9000".into(),
            monitor: 0,
            bitrate_mbps: 30,
            outgoing_dir: PathBuf::from("prdt-outgoing"),
            signaling_url: String::new(),
            host_id_file: PathBuf::from("host-id.txt"),
            key_file: default_host_key_path(),
            auto_start: false,
            encoder: "auto".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ViewerConfig {
    pub recv_dir: PathBuf,
    pub decoder: String,
    pub default_resolution: String,
    pub default_fps: u32,
    #[serde(default)]
    pub signaling_url: String,
    pub known_hosts: PathBuf,
    pub known_host_ids: PathBuf,
    #[serde(default)]
    pub hosts: Vec<HostEntry>,
}

impl Default for ViewerConfig {
    fn default() -> Self {
        Self {
            recv_dir: PathBuf::from("prdt-received"),
            decoder: "nvdec".into(),
            default_resolution: "1920x1080".into(),
            default_fps: 60,
            signaling_url: String::new(),
            known_hosts: PathBuf::from("known-hosts"),
            known_host_ids: PathBuf::from("known-host-ids"),
            hosts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostEntry {
    pub label: String,
    pub mode: String, // "direct" | "signaling"
    #[serde(default)]
    pub addr: String,
    #[serde(default)]
    pub host_id: String,
    #[serde(default)]
    pub pubkey: String,
    #[serde(default)]
    pub last_connected: String,
}

#[allow(clippy::derivable_impls)]
impl Default for Config {
    fn default() -> Self {
        Self {
            gui: GuiConfig::default(),
            host: HostConfig::default(),
            viewer: ViewerConfig::default(),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ConfigError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml decode: {0}")]
    TomlDecode(#[from] toml::de::Error),
    #[error("toml encode: {0}")]
    TomlEncode(#[from] toml::ser::Error),
}

impl Config {
    /// Load config from `path`. If the file doesn't exist, return
    /// `Config::default()` and write it to disk so subsequent loads pick
    /// up the same defaults.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                let cfg: Config = toml::from_str(&s)?;
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Config::default();
                cfg.save(path)?;
                Ok(cfg)
            }
            Err(e) => Err(ConfigError::Io(e)),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let s = toml::to_string_pretty(self)?;
        std::fs::write(path, s)?;
        Ok(())
    }

    /// Parsed bind address; falls back to `0.0.0.0:9000` if invalid.
    pub fn bind_addr(&self) -> SocketAddr {
        self.host
            .bind
            .parse()
            .unwrap_or_else(|_| "0.0.0.0:9000".parse().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_round_trips_through_toml() {
        let c = Config::default();
        let s = toml::to_string_pretty(&c).unwrap();
        let parsed: Config = toml::from_str(&s).unwrap();
        assert_eq!(c, parsed);
    }

    #[test]
    fn load_missing_writes_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let c = Config::load(&path).unwrap();
        assert_eq!(c, Config::default());
        assert!(path.exists(), "missing file should have been created");
    }

    #[test]
    fn save_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/dir/config.toml");
        Config::default().save(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn legacy_config_without_gui_section_loads() {
        // Older config files (G1) had no [gui] section. serde defaults must
        // populate it without erroring.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let legacy = r#"
[host]
bind = "0.0.0.0:9000"
monitor = 0
bitrate_mbps = 30
outgoing_dir = "prdt-outgoing"
host_id_file = "host-id.txt"
key_file = "host-key.bin"

[viewer]
recv_dir = "prdt-received"
decoder = "mf"
default_resolution = "1920x1080"
default_fps = 60
known_hosts = "known-hosts"
known_host_ids = "known-host-ids"
"#;
        std::fs::write(&path, legacy).unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.gui.locale, "");
    }

    #[test]
    fn host_entry_supports_signaling_only() {
        let mut c = Config::default();
        c.viewer.hosts.push(HostEntry {
            label: "Home".into(),
            mode: "signaling".into(),
            addr: String::new(),
            host_id: "123-456-789".into(),
            pubkey: String::new(),
            last_connected: String::new(),
        });
        let s = toml::to_string_pretty(&c).unwrap();
        let parsed: Config = toml::from_str(&s).unwrap();
        assert_eq!(parsed.viewer.hosts.len(), 1);
        assert_eq!(parsed.viewer.hosts[0].host_id, "123-456-789");
    }
}
