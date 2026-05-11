//! Viewer-side IPC: serialize StatsPayload and atomically write to
//! `ipc_dir/stats.json`; poll `ipc_dir/control.json` for action requests
//! from the overlay (consumed-on-read).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LatencyUs {
    pub p50: u64,
    pub p95: u64,
    pub p99: u64,
    pub samples: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatsPayload {
    pub version: u32,
    pub viewer_pid: u32,
    pub updated_at_unix_ms: u64,
    pub connection_state: String,
    pub host_label: String,
    pub decoder: String,
    pub latency_us: Option<LatencyUs>,
    pub fps_observed: f32,
    /// Badge + label for the encoder backend, e.g. `"🚀 HW nvenc-h265"`.
    /// `None` on old payloads that pre-date this field.
    #[serde(default)]
    pub encoder_backend: Option<String>,
}

#[derive(thiserror::Error, Debug)]
pub enum IpcError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn stats_path(ipc_dir: &Path) -> PathBuf {
    ipc_dir.join("stats.json")
}

pub fn control_path(ipc_dir: &Path) -> PathBuf {
    ipc_dir.join("control.json")
}

/// Atomically write `payload` to `ipc_dir/stats.json` via `tempfile + rename`.
pub fn write_stats(ipc_dir: &Path, payload: &StatsPayload) -> Result<(), IpcError> {
    let tmp = ipc_dir.join(".stats.tmp");
    std::fs::write(&tmp, serde_json::to_string(payload)?)?;
    std::fs::rename(&tmp, stats_path(ipc_dir))?;
    Ok(())
}

/// Poll `ipc_dir/control.json`. If present, parse the `action` field and
/// remove the file (consume-on-read semantics). Returns `Ok(None)` when
/// no control file exists.
pub fn read_control(ipc_dir: &Path) -> Result<Option<String>, IpcError> {
    let path = control_path(ipc_dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(IpcError::Io(e)),
    };
    let _ = std::fs::remove_file(&path);
    let v: serde_json::Value = serde_json::from_str(&raw)?;
    let action = v
        .get("action")
        .and_then(|a| a.as_str())
        .map(|s| s.to_string());
    Ok(action)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> StatsPayload {
        StatsPayload {
            version: 1,
            viewer_pid: 99,
            updated_at_unix_ms: 0,
            connection_state: "connected".into(),
            host_label: "h".into(),
            decoder: "mf".into(),
            latency_us: None,
            fps_observed: 0.0,
            encoder_backend: Some("💻 SW mf".into()),
        }
    }

    #[test]
    fn write_then_read_stats_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let payload = sample();
        write_stats(dir.path(), &payload).unwrap();
        let raw = std::fs::read_to_string(stats_path(dir.path())).unwrap();
        let parsed: StatsPayload = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed, payload);
    }

    #[test]
    fn read_control_no_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_control(dir.path()).unwrap().is_none());
    }

    #[test]
    fn read_control_consumes_file_on_success() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            control_path(dir.path()),
            r#"{"action":"disconnect","issued_at_unix_ms":0}"#,
        )
        .unwrap();
        let action = read_control(dir.path()).unwrap();
        assert_eq!(action, Some("disconnect".to_string()));
        assert!(!control_path(dir.path()).exists());
    }
}
