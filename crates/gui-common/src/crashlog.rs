//! Phase 4 G5 crash reporter. Installs a `std::panic::set_hook` that writes
//! a JSON dump under `dirs::cache_dir()/prdt/crashes/<timestamp>-<binary>-<pid>.json`,
//! including the most recent log lines (when a `TailHandle` is registered
//! via `register_tail`). The host GUI's Settings reads pending dumps with
//! `list_pending_crashes` and moves them to `crashes/acknowledged/` via
//! `mark_acknowledged` once the user has seen them.
//!
//! Native exceptions (e.g. DXGI_DEVICE_REMOVED bypassing Rust panics) are
//! NOT covered — see `known_limitations.md` §7. Phase 5 may add minidump
//! support.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::TailHandle;

/// Optional log-line provider. If set before a panic, recent lines are
/// included in the JSON dump.
static TAIL: OnceLock<TailHandle> = OnceLock::new();

/// Provide a TailHandle so future panic dumps include recent log lines.
/// Idempotent — only the first call wins.
pub fn register_tail(tail: TailHandle) {
    let _ = TAIL.set(tail);
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CrashReport {
    pub binary: String,
    pub version: String,
    pub timestamp_iso: String,
    pub panic_message: String,
    pub panic_location: String,
    #[serde(default)]
    pub recent_log_lines: Vec<String>,
}

/// Install a panic hook that writes a JSON dump on every Rust panic.
/// `binary_name` and `version` are typically `env!("CARGO_PKG_NAME")` and
/// `env!("CARGO_PKG_VERSION")` from the binary calling this.
#[allow(deprecated)] // PanicInfo renamed to PanicHookInfo in 1.81; keep PanicInfo for MSRV 1.78
pub fn install_panic_hook(binary_name: &'static str, version: &'static str) {
    std::panic::set_hook(Box::new(move |info| {
        let report = build_report(binary_name, version, info);
        if let Err(e) = write_report(&report) {
            // We're already in a panic; tracing might also be torn down.
            eprintln!("crashlog: failed to write report: {e}");
        }
        // Mirror the existing tracing-subscriber behaviour from the
        // pre-G5 viewer hook so logs still capture panics inline.
        tracing::error!(
            binary = report.binary,
            location = report.panic_location,
            message = report.panic_message,
            "PANIC"
        );
    }));
}

#[allow(deprecated)] // PanicInfo renamed to PanicHookInfo in 1.81; keep PanicInfo for MSRV 1.78
fn build_report(binary: &str, version: &str, info: &std::panic::PanicInfo<'_>) -> CrashReport {
    let panic_message = match info.payload().downcast_ref::<&'static str>() {
        Some(s) => (*s).to_string(),
        None => info
            .payload()
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_else(|| "panic with non-string payload".to_string()),
    };
    let panic_location = info
        .location()
        .map(|l| format!("{}:{}", l.file(), l.line()))
        .unwrap_or_else(|| "unknown".to_string());
    let recent_log_lines = TAIL.get().map(|h| h.snapshot()).unwrap_or_default();
    CrashReport {
        binary: binary.to_string(),
        version: version.to_string(),
        timestamp_iso: chrono::Utc::now().to_rfc3339(),
        panic_message,
        panic_location,
        recent_log_lines,
    }
}

/// Resolve the crashes directory, honoring the `PRDT_CRASHLOG_DIR` env
/// override (used by tests). Falls back to `dirs::cache_dir()/prdt/crashes/`.
pub fn crashes_dir() -> Option<PathBuf> {
    if let Ok(s) = std::env::var("PRDT_CRASHLOG_DIR") {
        return Some(PathBuf::from(s));
    }
    dirs::cache_dir().map(|d| d.join("prdt").join("crashes"))
}

fn write_report(report: &CrashReport) -> std::io::Result<PathBuf> {
    let dir = crashes_dir().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "no cache dir")
    })?;
    write_report_to(report, &dir)
}

fn write_report_to(report: &CrashReport, dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let path = dir.join(format!(
        "{}-{}-{}.json",
        stamp,
        report.binary,
        std::process::id()
    ));
    let json = serde_json::to_string_pretty(report)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, json)?;
    Ok(path)
}

/// List unacknowledged reports, newest first.
pub fn list_pending_crashes() -> std::io::Result<Vec<CrashReport>> {
    let Some(dir) = crashes_dir() else {
        return Ok(Vec::new());
    };
    list_pending_in(&dir)
}

fn list_pending_in(dir: &Path) -> std::io::Result<Vec<CrashReport>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    paths.reverse();
    let mut out = Vec::new();
    for p in paths {
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(r) = serde_json::from_str::<CrashReport>(&s) {
                out.push(r);
            }
        }
    }
    Ok(out)
}

/// Move a matching report into `crashes/acknowledged/`. Returns NotFound
/// if no report has the given (timestamp, binary) pair.
pub fn mark_acknowledged(timestamp_iso: &str, binary: &str) -> std::io::Result<()> {
    let Some(dir) = crashes_dir() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no cache dir",
        ));
    };
    mark_acknowledged_in(&dir, timestamp_iso, binary)
}

fn mark_acknowledged_in(dir: &Path, timestamp_iso: &str, binary: &str) -> std::io::Result<()> {
    let acked = dir.join("acknowledged");
    std::fs::create_dir_all(&acked)?;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let p = entry.path();
        if !p.is_file() || p.extension().map(|e| e != "json").unwrap_or(true) {
            continue;
        }
        let s = match std::fs::read_to_string(&p) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Ok(r) = serde_json::from_str::<CrashReport>(&s) {
            if r.timestamp_iso == timestamp_iso && r.binary == binary {
                let dest = acked.join(p.file_name().expect("file name"));
                std::fs::rename(&p, dest)?;
                return Ok(());
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no matching crash report",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report(binary: &str, ts: &str) -> CrashReport {
        CrashReport {
            binary: binary.to_string(),
            version: "0.0.1-test".to_string(),
            timestamp_iso: ts.to_string(),
            panic_message: "boom".to_string(),
            panic_location: "src/main.rs:42".to_string(),
            recent_log_lines: vec!["INFO test: started".to_string()],
        }
    }

    #[test]
    fn json_round_trip_preserves_fields() {
        let r = sample_report("prdt-host", "2026-04-25T12:00:00Z");
        let json = serde_json::to_string(&r).unwrap();
        let back: CrashReport = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn write_then_list_returns_one_report() {
        let dir = tempfile::tempdir().unwrap();
        let r = sample_report("prdt-host", "2026-04-25T12:00:00Z");
        write_report_to(&r, dir.path()).unwrap();
        let pending = list_pending_in(dir.path()).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].binary, "prdt-host");
        assert_eq!(pending[0].panic_message, "boom");
    }

    #[test]
    fn list_pending_in_missing_dir_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("never-created");
        let pending = list_pending_in(&missing).unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn list_pending_in_returns_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let older = sample_report("prdt-host", "2026-04-25T10:00:00Z");
        std::fs::write(
            dir.path().join("20260425-100000-prdt-host-1.json"),
            serde_json::to_string(&older).unwrap(),
        )
        .unwrap();
        let newer = sample_report("prdt-viewer", "2026-04-25T15:00:00Z");
        std::fs::write(
            dir.path().join("20260425-150000-prdt-viewer-2.json"),
            serde_json::to_string(&newer).unwrap(),
        )
        .unwrap();
        let pending = list_pending_in(dir.path()).unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].binary, "prdt-viewer", "newest first");
        assert_eq!(pending[1].binary, "prdt-host");
    }

    #[test]
    fn mark_acknowledged_moves_file_into_subdir() {
        let dir = tempfile::tempdir().unwrap();
        let r = sample_report("prdt-host", "2026-04-25T12:00:00Z");
        let path = write_report_to(&r, dir.path()).unwrap();
        assert!(path.exists());

        mark_acknowledged_in(dir.path(), &r.timestamp_iso, &r.binary).unwrap();

        assert!(!path.exists(), "original file moved");
        let acked: Vec<_> = std::fs::read_dir(dir.path().join("acknowledged"))
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(acked.len(), 1, "exactly one entry under acknowledged/");
    }

    #[test]
    fn mark_acknowledged_missing_match_returns_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let r = sample_report("prdt-host", "2026-04-25T12:00:00Z");
        write_report_to(&r, dir.path()).unwrap();
        let err = mark_acknowledged_in(dir.path(), "wrong-ts", "prdt-host").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
