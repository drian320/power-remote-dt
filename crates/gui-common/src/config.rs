//! Persistent configuration shared by host and viewer GUIs.
//!
//! Schema is documented in
//! `docs/superpowers/specs/2026-04-25-phase4-g1-egui-foundation-design.md`.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GuiConfig {
    /// Locale: "" = auto-detect from OS, "en" / "ja" = forced.
    #[serde(default)]
    pub locale: String,
    /// Set to `true` after the first-run onboarding wizard completes.
    /// `false` (the serde default) causes the wizard to appear on next launch.
    #[serde(default)]
    pub onboarded: bool,
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

fn default_codec_choice() -> String {
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
    /// Codec preference forwarded to the viewer CLI as `--codec`.
    /// "auto" lets the host negotiate. Other values: "h264", "h265".
    #[serde(default = "default_codec_choice")]
    pub codec: String,
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
            codec: "auto".into(),
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
    #[serde(
        deserialize_with = "deser_last_connected",
        serialize_with = "ser_last_connected",
        default = "epoch"
    )]
    pub last_connected: SystemTime,
    /// Cached online state from the last OnlineProbe tick. Not persisted to
    /// disk (serialized as `None` always); used only at runtime.
    #[serde(default, skip_serializing)]
    pub last_known_online: Option<bool>,
}

fn epoch() -> SystemTime {
    UNIX_EPOCH
}

/// Serialize `SystemTime` as an RFC3339 string for human-readable TOML.
fn ser_last_connected<S>(t: &SystemTime, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::Error as _;
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    // Use chrono for RFC3339 formatting.
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 0)
        .ok_or_else(|| S::Error::custom("timestamp out of range"))?;
    s.serialize_str(&dt.to_rfc3339())
}

/// Deserialize `last_connected` from either an RFC3339 string (legacy) or a
/// missing field (defaults to UNIX_EPOCH). The `#[serde(default = "epoch")]`
/// attribute handles the missing-field case before this function is called.
fn deser_last_connected<'de, D>(d: D) -> Result<SystemTime, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let s = String::deserialize(d)?;
    if s.is_empty() {
        return Ok(UNIX_EPOCH);
    }
    // Try RFC3339 parse via chrono.
    match chrono::DateTime::parse_from_rfc3339(&s) {
        Ok(dt) => {
            let secs = dt.timestamp();
            let nanos = dt.timestamp_subsec_nanos();
            if secs < 0 {
                return Ok(UNIX_EPOCH);
            }
            Ok(UNIX_EPOCH + std::time::Duration::new(secs as u64, nanos))
        }
        Err(_) => Ok(UNIX_EPOCH),
    }
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
    fn legacy_config_loads_onboarded_false() {
        let toml = "[gui]\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert!(!c.gui.onboarded);
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
            last_connected: UNIX_EPOCH,
            last_known_online: None,
        });
        let s = toml::to_string_pretty(&c).unwrap();
        let parsed: Config = toml::from_str(&s).unwrap();
        assert_eq!(parsed.viewer.hosts.len(), 1);
        assert_eq!(parsed.viewer.hosts[0].host_id, "123-456-789");
    }

    // Helper: minimal viewer section prefix for TOML tests.
    fn viewer_prefix() -> &'static str {
        r#"
[viewer]
recv_dir = "prdt-received"
decoder = "nvdec"
default_resolution = "1920x1080"
default_fps = 60
known_hosts = "known-hosts"
known_host_ids = "known-host-ids"
"#
    }

    #[test]
    fn host_entry_legacy_string_last_connected_parses() {
        let toml_str = format!(
            "{}\n[[viewer.hosts]]\nlabel = \"old\"\nmode = \"direct\"\naddr = \"127.0.0.1:9000\"\npubkey = \"\"\nlast_connected = \"2025-12-01T00:00:00Z\"\n",
            viewer_prefix()
        );
        let c: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(c.viewer.hosts.len(), 1);
        let e = &c.viewer.hosts[0];
        assert!(e.last_connected > UNIX_EPOCH);
    }

    #[test]
    fn host_entry_missing_last_connected_defaults_to_epoch() {
        let toml_str = format!(
            "{}\n[[viewer.hosts]]\nlabel = \"fresh\"\nmode = \"direct\"\naddr = \"127.0.0.1:9000\"\npubkey = \"\"\n",
            viewer_prefix()
        );
        let c: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(c.viewer.hosts[0].last_connected, UNIX_EPOCH);
    }

    #[test]
    fn host_entry_modern_systemtime_round_trips() {
        // Write a HostEntry with a known time, serialize to TOML, parse back.
        let mut cfg = Config::default();
        let t = UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        cfg.viewer.hosts.push(HostEntry {
            label: "rt".into(),
            mode: "direct".into(),
            addr: "127.0.0.1:9000".into(),
            host_id: String::new(),
            pubkey: String::new(),
            last_connected: t,
            last_known_online: None,
        });
        let s = toml::to_string_pretty(&cfg).unwrap();
        // The serialized form must be an RFC3339 string.
        assert!(s.contains("last_connected"), "field should be present");
        let parsed: Config = toml::from_str(&s).unwrap();
        // Allow up to 1 second difference due to subsecond truncation.
        let diff = parsed.viewer.hosts[0]
            .last_connected
            .duration_since(t)
            .unwrap_or_else(|e| e.duration());
        assert!(diff.as_secs() < 2, "round-trip drift: {diff:?}");
    }
}
