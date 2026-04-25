//! Read-side IPC: pull StatsPayload out of stats.json and write control flags
//! back into control.json.

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
    pub connection_state: String, // "connecting" | "connected" | "disconnecting"
    pub host_label: String,
    pub decoder: String,
    /// None when no samples yet (still connecting / handshaking).
    pub latency_us: Option<LatencyUs>,
    pub fps_observed: f32,
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

/// Read the latest stats from `ipc_dir/stats.json`. Returns Err(NotFound) if
/// the writer hasn't written one yet (overlay shows "Connecting…").
pub fn read_stats(ipc_dir: &Path) -> Result<StatsPayload, IpcError> {
    let raw = std::fs::read_to_string(stats_path(ipc_dir))?;
    let parsed = serde_json::from_str(&raw)?;
    Ok(parsed)
}

/// Write `{ "action": "disconnect", "issued_at_unix_ms": <now> }` to
/// `ipc_dir/control.json`. Atomic via tempfile + rename.
pub fn write_disconnect(ipc_dir: &Path) -> Result<(), IpcError> {
    write_control(ipc_dir, "disconnect")
}

fn write_control(ipc_dir: &Path, action: &str) -> Result<(), IpcError> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let payload = serde_json::json!({
        "action": action,
        "issued_at_unix_ms": now_ms,
    });
    let tmp = ipc_dir.join(".control.tmp");
    std::fs::write(&tmp, serde_json::to_string(&payload)?)?;
    std::fs::rename(&tmp, control_path(ipc_dir))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_stats() -> StatsPayload {
        StatsPayload {
            version: 1,
            viewer_pid: 42,
            updated_at_unix_ms: 1_714_024_822_123,
            connection_state: "connected".into(),
            host_label: "192.168.1.5:9000".into(),
            decoder: "nvdec".into(),
            latency_us: Some(LatencyUs {
                p50: 18_000,
                p95: 41_000,
                p99: 67_000,
                samples: 512,
            }),
            fps_observed: 59.8,
        }
    }

    #[test]
    fn stats_round_trip_through_json() {
        let dir = tempfile::tempdir().unwrap();
        let payload = sample_stats();
        std::fs::write(
            stats_path(dir.path()),
            serde_json::to_string(&payload).unwrap(),
        )
        .unwrap();
        let parsed = read_stats(dir.path()).unwrap();
        assert_eq!(payload, parsed);
    }

    #[test]
    fn read_stats_missing_returns_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = read_stats(dir.path()).unwrap_err();
        assert!(matches!(err, IpcError::Io(_)));
    }

    #[test]
    fn write_disconnect_creates_control_json() {
        let dir = tempfile::tempdir().unwrap();
        write_disconnect(dir.path()).unwrap();
        let raw = std::fs::read_to_string(control_path(dir.path())).unwrap();
        assert!(raw.contains("\"disconnect\""));
        assert!(raw.contains("issued_at_unix_ms"));
    }

    #[test]
    fn null_latency_parses_as_connecting() {
        let dir = tempfile::tempdir().unwrap();
        let raw = r#"{
            "version": 1,
            "viewer_pid": 42,
            "updated_at_unix_ms": 0,
            "connection_state": "connecting",
            "host_label": "test",
            "decoder": "mf",
            "latency_us": null,
            "fps_observed": 0.0
        }"#;
        std::fs::write(stats_path(dir.path()), raw).unwrap();
        let parsed = read_stats(dir.path()).unwrap();
        assert!(parsed.latency_us.is_none());
        assert_eq!(parsed.connection_state, "connecting");
    }
}
