//! Host-side auth + permissions configuration (P6).
//!
//! Persisted to `~/.config/prdt/host-auth.toml` (or `%APPDATA%\prdt\host-auth.toml`).
//! The PIN is stored as a bcrypt hash, never plaintext. The ephemeral is in
//! memory only (handled by AuthValidator in T3).
//!
//! Moved to `prdt-gui-common` in T7 so that `prdt-gui-host` can consume
//! `HostAuthConfig` without creating a dependency cycle with `prdt-host`.

use prdt_protocol::PermissionSet;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AuthMode {
    #[default]
    Tofu,
    Pin,
    Ephemeral,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostAuthConfig {
    #[serde(default)]
    pub mode: AuthMode,
    #[serde(default)]
    pub pin_hash: Option<String>,
    /// Plaintext of the current fixed PIN, kept so the host can *display* it to
    /// its own local operator (AC-2). Verification always uses `pin_hash`; this
    /// is display-only. Storing it in plaintext is acceptable because it lives
    /// on the host's own machine, in the same trust boundary as the private
    /// key. `None` when no PIN has been generated yet.
    #[serde(default)]
    pub pin_plaintext: Option<String>,
    #[serde(default = "default_ephemeral_lifetime_seconds")]
    pub ephemeral_lifetime_seconds: u32,
    #[serde(default = "default_permissions")]
    pub default_permissions: PermissionSet,
    #[serde(default = "default_max_pin_attempts")]
    pub max_pin_attempts: u8,
    #[serde(default = "default_pin_lockout_seconds")]
    pub pin_lockout_seconds: u32,
    #[serde(default = "default_consent_timeout_seconds")]
    pub consent_timeout_seconds: u32,
}

fn default_ephemeral_lifetime_seconds() -> u32 {
    120
}
fn default_permissions() -> PermissionSet {
    PermissionSet::all()
}
fn default_max_pin_attempts() -> u8 {
    5
}
fn default_pin_lockout_seconds() -> u32 {
    300
}
fn default_consent_timeout_seconds() -> u32 {
    60
}

impl Default for HostAuthConfig {
    fn default() -> Self {
        Self {
            mode: AuthMode::default(),
            pin_hash: None,
            pin_plaintext: None,
            ephemeral_lifetime_seconds: default_ephemeral_lifetime_seconds(),
            default_permissions: default_permissions(),
            max_pin_attempts: default_max_pin_attempts(),
            pin_lockout_seconds: default_pin_lockout_seconds(),
            consent_timeout_seconds: default_consent_timeout_seconds(),
        }
    }
}

impl HostAuthConfig {
    pub fn hash_pin(plain: &str) -> Result<String, bcrypt::BcryptError> {
        bcrypt::hash(plain, 12)
    }

    /// Generate a fresh fixed PIN: 6 decimal digits. Uniform over
    /// `000000..=999999`. Short enough to read aloud, long enough that the
    /// existing 5-attempt lockout makes online guessing impractical.
    pub fn generate_pin() -> String {
        use rand::Rng;
        let n: u32 = rand::thread_rng().gen_range(0..1_000_000);
        format!("{n:06}")
    }

    /// Set the PIN from plaintext: stores both the bcrypt hash (for
    /// verification) and the plaintext (for local display). Does NOT change
    /// `mode`; callers decide whether to switch to [`AuthMode::Pin`].
    pub fn set_pin(&mut self, plain: &str) -> Result<(), bcrypt::BcryptError> {
        self.pin_hash = Some(Self::hash_pin(plain)?);
        self.pin_plaintext = Some(plain.to_string());
        Ok(())
    }

    pub fn verify_pin(&self, plain: &str) -> bool {
        match &self.pin_hash {
            Some(h) => bcrypt::verify(plain, h).unwrap_or_else(|e| {
                tracing::warn!(
                    error = %e,
                    "bcrypt::verify failed (corrupted pin_hash?); treating as wrong PIN"
                );
                false
            }),
            None => false,
        }
    }

    /// 8-char ASCII upper+digit ephemeral, ambiguous chars removed (0/O, 1/I/L).
    pub fn generate_ephemeral() -> String {
        use rand::Rng;
        const ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
        let mut rng = rand::thread_rng();
        (0..8)
            .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
            .collect()
    }

    /// Load from `path`. Missing file returns `HostAuthConfig::default()`.
    /// Malformed TOML returns an error.
    pub fn load_or_default(path: &Path) -> Result<Self, HostAuthConfigError> {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                let cfg: HostAuthConfig = toml::from_str(&s)?;
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(HostAuthConfigError::Io(e)),
        }
    }

    /// Atomically write to `path`.
    ///
    /// Held across the advisory [`prdt_crypto::FileLock`] so two host/GUI
    /// processes fighting over the same config dir cannot interleave, and
    /// routed through [`prdt_crypto::atomic_write`] (temp + `fsync` + `rename`)
    /// so a power-loss mid-write cannot corrupt the PIN hash. On Unix the file
    /// is chmod'd to `0600`: this config now stores the plaintext PIN next to
    /// the bcrypt hash, and that PIN gates *incoming* remote-control, so no
    /// other local user may read it. No-op on Windows (inherits profile ACL).
    pub fn save(&self, path: &Path) -> Result<(), HostAuthConfigError> {
        let s = toml::to_string_pretty(self)?;
        let _lock = prdt_crypto::FileLock::acquire(path)?;
        prdt_crypto::atomic_write(path, s.as_bytes())?;
        set_owner_only(path)?;
        Ok(())
    }
}

/// Restrict `path` to owner read/write only. Mirrors
/// `prdt_crypto::keyfile::set_owner_only_permissions` so the plaintext PIN in
/// host-auth.toml is never left world-readable at the default umask.
#[cfg(unix)]
fn set_owner_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum HostAuthConfigError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml decode: {0}")]
    TomlDecode(#[from] toml::de::Error),
    #[error("toml encode: {0}")]
    TomlEncode(#[from] toml::ser::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_spec() {
        let c = HostAuthConfig::default();
        assert_eq!(c.mode, AuthMode::Tofu);
        assert_eq!(c.pin_hash, None);
        assert_eq!(c.ephemeral_lifetime_seconds, 120);
        assert_eq!(c.default_permissions, PermissionSet::all());
        assert_eq!(c.max_pin_attempts, 5);
        assert_eq!(c.pin_lockout_seconds, 300);
        assert_eq!(c.consent_timeout_seconds, 60);
    }

    #[test]
    fn toml_round_trip() {
        let c = HostAuthConfig {
            mode: AuthMode::Pin,
            pin_hash: Some("$2b$12$abcde".into()),
            pin_plaintext: Some("123456".into()),
            ephemeral_lifetime_seconds: 60,
            default_permissions: PermissionSet {
                input: true,
                clipboard: false,
                file_transfer: true,
                audio: false,
            },
            max_pin_attempts: 3,
            pin_lockout_seconds: 120,
            consent_timeout_seconds: 30,
        };
        let serialized = toml::to_string(&c).unwrap();
        let back: HostAuthConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(back.mode, c.mode);
        assert_eq!(back.pin_hash, c.pin_hash);
        assert_eq!(back.pin_plaintext, c.pin_plaintext);
        assert_eq!(back.default_permissions, c.default_permissions);
    }

    #[test]
    fn generate_pin_is_six_digits() {
        for _ in 0..200 {
            let pin = HostAuthConfig::generate_pin();
            assert_eq!(pin.len(), 6, "pin {pin} not 6 chars");
            assert!(
                pin.chars().all(|c| c.is_ascii_digit()),
                "pin {pin} not numeric"
            );
        }
    }

    #[test]
    fn set_pin_stores_hash_and_plaintext() {
        let mut c = HostAuthConfig::default();
        assert!(c.pin_hash.is_none());
        assert!(c.pin_plaintext.is_none());
        c.set_pin("246810").unwrap();
        assert_eq!(c.pin_plaintext.as_deref(), Some("246810"));
        assert!(c.verify_pin("246810"));
        assert!(!c.verify_pin("000000"));
        // Regenerating changes the stored hash and plaintext.
        let old_hash = c.pin_hash.clone();
        c.set_pin("135790").unwrap();
        assert_ne!(c.pin_hash, old_hash, "hash must change on new pin");
        assert_eq!(c.pin_plaintext.as_deref(), Some("135790"));
        assert!(c.verify_pin("135790"));
        assert!(!c.verify_pin("246810"));
    }

    // Legacy host-auth.toml files predate `pin_plaintext`; the serde default
    // must populate it as None without erroring.
    #[test]
    fn legacy_toml_without_pin_plaintext_loads() {
        // AuthMode serializes with its variant names (PascalCase); legacy
        // files predate only `pin_plaintext`, which must default to None.
        let back: HostAuthConfig =
            toml::from_str("mode = \"Pin\"\npin_hash = \"$2b$12$abcde\"\n").unwrap();
        assert_eq!(back.mode, AuthMode::Pin);
        assert_eq!(back.pin_plaintext, None);
    }

    #[test]
    fn empty_toml_loads_with_defaults() {
        let back: HostAuthConfig = toml::from_str("").unwrap();
        assert_eq!(back.mode, AuthMode::Tofu);
        assert_eq!(back.default_permissions, PermissionSet::all());
    }

    #[test]
    fn pin_hash_and_verify_round_trip() {
        let h = HostAuthConfig::hash_pin("hunter2").unwrap();
        let c = HostAuthConfig {
            pin_hash: Some(h),
            ..Default::default()
        };
        assert!(c.verify_pin("hunter2"));
        assert!(!c.verify_pin("hunter3"));
        assert!(!c.verify_pin(""));
    }

    #[test]
    fn ephemeral_no_ambiguous_chars() {
        for _ in 0..100 {
            let e = HostAuthConfig::generate_ephemeral();
            assert_eq!(e.len(), 8);
            for ch in e.chars() {
                assert!(
                    !matches!(ch, '0' | 'O' | '1' | 'I' | 'L'),
                    "ephemeral contains ambiguous char: {e}"
                );
                assert!(
                    ch.is_ascii_alphanumeric() && (ch.is_ascii_uppercase() || ch.is_ascii_digit())
                );
            }
        }
    }

    #[test]
    fn load_or_default_missing_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host-auth.toml");
        let cfg = HostAuthConfig::load_or_default(&path).unwrap();
        assert_eq!(cfg.mode, AuthMode::Tofu);
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host-auth.toml");
        let cfg = HostAuthConfig {
            mode: AuthMode::Pin,
            pin_hash: Some("$2b$12$test".into()),
            ..Default::default()
        };
        cfg.save(&path).unwrap();
        let loaded = HostAuthConfig::load_or_default(&path).unwrap();
        assert_eq!(loaded.mode, AuthMode::Pin);
        assert_eq!(loaded.pin_hash, cfg.pin_hash);
    }

    #[test]
    fn save_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/dir/host-auth.toml");
        HostAuthConfig::default().save(&path).unwrap();
        assert!(path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn save_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        // The plaintext PIN lives next to the bcrypt hash; the on-disk file must
        // be chmod 0600 so no other local user can read the PIN that gates
        // incoming remote-control.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host-auth.toml");
        let mut cfg = HostAuthConfig::default();
        cfg.set_pin("246810").unwrap();
        cfg.save(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "host-auth.toml should be chmod 600");
    }

    #[test]
    fn save_atomic_uses_pid_suffix() {
        // The tmp file's extension must contain the current PID so that
        // concurrent processes use distinct temp file names.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host-auth.toml");
        HostAuthConfig::default().save(&path).unwrap();
        // Final file exists and tmp (with pid suffix) was cleaned up by rename.
        assert!(path.exists());
        let pid = std::process::id();
        let tmp = path.with_extension(format!("toml.tmp.{pid}"));
        assert!(!tmp.exists(), "tmp file must be gone after atomic rename");
        // Two sequential saves must both succeed (no leftover tmp collision).
        HostAuthConfig::default().save(&path).unwrap();
        assert!(path.exists());
    }
}
