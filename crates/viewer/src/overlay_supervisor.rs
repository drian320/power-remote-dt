//! OverlaySupervisor — owns the per-PID IPC directory and the optional
//! `prdt-viewer-overlay` child process. Used by the viewer's main app to
//! spawn the overlay on ESC, write stats periodically, and poll for control
//! requests.
//!
//! `Drop` cleans up the child process and the IPC directory.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use crate::overlay_ipc::{self, StatsPayload};

pub struct OverlaySupervisor {
    ipc_dir: PathBuf,
    child: Option<Child>,
}

impl OverlaySupervisor {
    /// Build a supervisor with `dirs::cache_dir()/prdt/overlay-ipc/<pid>/`
    /// as the IPC directory. Creates the directory on disk.
    pub fn new() -> std::io::Result<Self> {
        let root = dirs::cache_dir()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "no cache dir")
            })?
            .join("prdt")
            .join("overlay-ipc");
        let ipc_dir = root.join(std::process::id().to_string());
        std::fs::create_dir_all(&ipc_dir)?;
        Ok(Self {
            ipc_dir,
            child: None,
        })
    }

    /// Test-only constructor that uses an explicit IPC directory.
    #[cfg(test)]
    pub fn with_ipc_dir(ipc_dir: PathBuf) -> Self {
        Self {
            ipc_dir,
            child: None,
        }
    }

    pub fn ipc_dir(&self) -> &Path {
        &self.ipc_dir
    }

    /// Resolve the path of the `prdt-viewer-overlay` binary that should sit
    /// next to `prdt-viewer` (cargo `install` / `target/debug` / app bundle
    /// MacOS layouts all put binaries in the same directory).
    pub fn overlay_binary_path() -> std::io::Result<PathBuf> {
        let exe = std::env::current_exe()?;
        let dir = exe.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "current_exe has no parent")
        })?;
        Ok(dir.join(format!("prdt-viewer-overlay{}", std::env::consts::EXE_SUFFIX)))
    }

    /// Spawn the overlay child if not already alive. Idempotent: if a child
    /// is already running, this is a no-op.
    pub fn spawn_if_idle(&mut self) -> std::io::Result<()> {
        if let Some(c) = self.child.as_mut() {
            match c.try_wait()? {
                Some(_) => self.child = None,
                None => return Ok(()),
            }
        }
        let bin = Self::overlay_binary_path()?;
        let child = Command::new(&bin)
            .arg("--ipc-dir")
            .arg(&self.ipc_dir)
            .spawn()?;
        self.child = Some(child);
        Ok(())
    }

    pub fn write_stats(&self, payload: &StatsPayload) -> Result<(), overlay_ipc::IpcError> {
        overlay_ipc::write_stats(&self.ipc_dir, payload)
    }

    /// Poll for a control action from the overlay. Returns `Ok(None)` if
    /// none pending.
    pub fn read_control(&self) -> Result<Option<String>, overlay_ipc::IpcError> {
        overlay_ipc::read_control(&self.ipc_dir)
    }
}

impl Drop for OverlaySupervisor {
    fn drop(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = std::fs::remove_dir_all(&self.ipc_dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_removes_ipc_dir() {
        let parent = tempfile::tempdir().unwrap();
        let ipc_dir = parent.path().join("drop-test");
        std::fs::create_dir_all(&ipc_dir).unwrap();
        std::fs::write(ipc_dir.join("stats.json"), "{}").unwrap();
        {
            let _s = OverlaySupervisor::with_ipc_dir(ipc_dir.clone());
        }
        assert!(!ipc_dir.exists(), "Drop should have removed {ipc_dir:?}");
    }

    #[test]
    fn write_then_read_stats_through_supervisor() {
        let parent = tempfile::tempdir().unwrap();
        let ipc_dir = parent.path().join("rw");
        std::fs::create_dir_all(&ipc_dir).unwrap();
        let s = OverlaySupervisor::with_ipc_dir(ipc_dir.clone());
        let payload = StatsPayload {
            version: 1,
            viewer_pid: 7,
            updated_at_unix_ms: 0,
            connection_state: "connected".into(),
            host_label: "x".into(),
            decoder: "mf".into(),
            latency_us: None,
            fps_observed: 0.0,
        };
        s.write_stats(&payload).unwrap();
        let raw = std::fs::read_to_string(ipc_dir.join("stats.json")).unwrap();
        let parsed: StatsPayload = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed, payload);
        assert!(s.read_control().unwrap().is_none());
        // Manually remove the dir so Drop doesn't fail (it's idempotent;
        // remove_dir_all on a missing dir is fine).
    }

    #[test]
    fn overlay_binary_path_uses_exe_suffix() {
        let p = OverlaySupervisor::overlay_binary_path().unwrap();
        let name = p.file_name().unwrap().to_string_lossy();
        if cfg!(windows) {
            assert!(name.ends_with(".exe"), "got {name}");
        }
        assert!(name.starts_with("prdt-viewer-overlay"), "got {name}");
    }

    #[test]
    fn supervisor_new_creates_dir_under_cache_root() {
        // We can't easily redirect dirs::cache_dir() in tests, so just verify
        // OverlaySupervisor::new() returns Ok and the dir exists — best effort
        // smoke. Skip if the env disallows cache_dir (CI sandboxes etc).
        match OverlaySupervisor::new() {
            Ok(s) => {
                assert!(s.ipc_dir().exists(), "ipc_dir missing: {:?}", s.ipc_dir());
                // Drop will clean up.
            }
            Err(e) => eprintln!("skipping: cache dir unavailable: {e}"),
        }
    }
}
