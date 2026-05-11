//! Host-side auth + permissions configuration (P6).
//!
//! Persisted to `~/.config/prdt/host-auth.toml` (or `%APPDATA%\prdt\host-auth.toml`).
//! The PIN is stored as a bcrypt hash, never plaintext. The ephemeral is in
//! memory only (handled by AuthValidator in T3).

use prdt_protocol::PermissionSet;
use serde::{Deserialize, Serialize};

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
        assert_eq!(back.default_permissions, c.default_permissions);
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
}
