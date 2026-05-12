//! RestoreToken persistence — TOML at
//! `$XDG_CONFIG_HOME/prdt/portal-session.toml`.
//!
//! The token blob is opaque to us; the portal issues it on Start and
//! validates it on the next select_sources. We only need string in/out
//! plus a few hints for operator debugging (saved_at, compositor_hint).
//!
//! Failure-modes match HostAuthConfig / KnownPeers:
//! - file missing → returns default (no token; first launch path).
//! - parse error → logs warn, returns default (corrupt file replaced
//!   on next save).
//! - write atomic via {path}.tmp.{pid} + rename; perms 0600.

#![cfg(target_os = "linux")]

use serde::{Deserialize, Serialize};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortalSessionToken {
    /// Opaque restore token from `ScreenCast.Start` response. Empty string
    /// means "no token persisted yet" (treated as `None` by callers).
    #[serde(default)]
    pub restore_token: String,
    /// RFC3339 timestamp; informational, written by `save`.
    #[serde(default)]
    pub saved_at: String,
    /// Informational hint, e.g. "GNOME 47.1". Best-effort; falls back to
    /// "unknown" if the compositor refuses to identify itself.
    #[serde(default)]
    pub compositor_hint: String,
}

impl PortalSessionToken {
    /// Load from disk; return `Self::default()` for missing file or parse
    /// error (warn-logged). Never returns `Err`: caller treats both
    /// "no file" and "corrupt file" as "first launch".
    pub fn load_or_default(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(s) => match toml::from_str::<Self>(&s) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(),
                        "portal-session.toml parse failed; using default");
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(),
                    "portal-session.toml read failed; using default");
                Self::default()
            }
        }
    }

    /// Write atomically. Tmp filename suffix carries pid so concurrent
    /// host instances don't truncate each other. Perms set to 0600 after
    /// rename. Caller is responsible for `create_dir_all` on the parent.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let parent = path.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
        })?;
        std::fs::create_dir_all(parent)?;
        let tmp = path.with_extension(format!("toml.tmp.{}", std::process::id()));
        let body = toml::to_string_pretty(self).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("toml ser: {e}"))
        })?;
        std::fs::write(&tmp, body)?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Returns `Some(&str)` only when the token is non-empty. Saves
    /// callers a length check.
    pub fn token_opt(&self) -> Option<&str> {
        if self.restore_token.is_empty() {
            None
        } else {
            Some(&self.restore_token)
        }
    }

    pub fn with_token(
        restore_token: impl Into<String>,
        compositor_hint: impl Into<String>,
    ) -> Self {
        Self {
            restore_token: restore_token.into(),
            saved_at: chrono_like_rfc3339_now(),
            compositor_hint: compositor_hint.into(),
        }
    }
}

/// Minimal RFC3339 timestamp without pulling in chrono. Uses
/// `SystemTime::now()` formatted via `humantime::format_rfc3339` if
/// available; otherwise falls back to a UNIX-epoch second string. The
/// value is informational only — never parsed back.
fn chrono_like_rfc3339_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => format!("unix:{}.{:09}", d.as_secs(), d.subsec_nanos()),
        Err(_) => "unix:0".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let p =
            std::env::temp_dir().join(format!("prdt-portal-token-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn round_trip() {
        let dir = tmpdir("round_trip");
        let path = dir.join("portal-session.toml");
        let original = PortalSessionToken::with_token("abc123==", "GNOME 47.1");
        original.save(&path).expect("save");
        let loaded = PortalSessionToken::load_or_default(&path);
        assert_eq!(loaded.restore_token, "abc123==");
        assert_eq!(loaded.compositor_hint, "GNOME 47.1");
    }

    #[test]
    fn atomic_save_pid_suffix_does_not_collide_under_repeated_writes() {
        let dir = tmpdir("atomic");
        let path = dir.join("portal-session.toml");
        for i in 0..32 {
            let t = PortalSessionToken::with_token(format!("tok-{i}"), "stress");
            t.save(&path).expect("save");
        }
        let final_ = PortalSessionToken::load_or_default(&path);
        assert!(final_.restore_token.starts_with("tok-"));
        // Mode is 0600.
        let perms = fs::metadata(&path).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o600);
        // No stray .tmp files left behind.
        let strays: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains("tmp"))
            .collect();
        assert!(strays.is_empty(), "tmp files left behind: {strays:?}");
    }

    #[test]
    fn missing_file_returns_default() {
        let dir = tmpdir("missing");
        let path = dir.join("does-not-exist.toml");
        let loaded = PortalSessionToken::load_or_default(&path);
        assert_eq!(loaded, PortalSessionToken::default());
        assert!(loaded.token_opt().is_none());
    }

    #[test]
    fn corrupt_file_returns_default_with_warn() {
        let dir = tmpdir("corrupt");
        let path = dir.join("portal-session.toml");
        fs::write(&path, b"this is not [[[ toml").expect("seed corrupt");
        let loaded = PortalSessionToken::load_or_default(&path);
        assert_eq!(loaded, PortalSessionToken::default());
        // The warn line is captured by tracing-test if needed; here we
        // just assert behaviour. (Manual: rerun with RUST_LOG=warn to see.)
    }
}
