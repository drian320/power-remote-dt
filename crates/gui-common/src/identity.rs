//! Offline-first device identity + provisioning (AC-9, AC-2, AC-12).
//!
//! A prdt device is identified by its long-term Curve25519 key. On first
//! launch we must be able to show the user a stable identity (key fingerprint +
//! a fixed PIN) *without* contacting the network, then fill in the
//! server-allocated 9-digit ID asynchronously once the signaling server is
//! reachable. This module owns that lifecycle:
//!
//! 1. **Key first.** [`DeviceIdentity::load_or_create`] generates and `fsync`s
//!    the private key before anything else (via
//!    [`prdt_crypto::load_or_create_keypair`]). A crash right after first
//!    launch never loses the identity.
//! 2. **Atomic record.** The `{host_id, key_fingerprint, server_authority,
//!    provisioning_state}` record is persisted to `identity.toml` under an
//!    advisory lock with a temp-file + rename write (AC-12).
//! 3. **Never blocks.** If the signaling server is unreachable the device stays
//!    in [`ProvisioningState::Unprovisioned`]: no ID yet, but the key
//!    fingerprint and PIN are available for display.
//! 4. **Idempotent provisioning.** [`DeviceIdentity::reprovision`] resends the
//!    persisted `host_id` + pubkey; the server returns the same ID for the same
//!    key rather than allocating a new one.
//! 5. **Fixed PIN.** On first launch a 6-digit PIN is generated and stored as
//!    both a bcrypt hash (for verification by the host) and plaintext (for
//!    local display), with the auth mode set to [`AuthMode`]`::Pin` (AC-2).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::auth_config::{AuthMode, HostAuthConfig, HostAuthConfigError};

/// Where the three identity-related files live. Construct with
/// [`IdentityPaths::from_config_root`] for the OS-conventional layout, or set
/// the fields directly (e.g. from an existing `Config`).
#[derive(Debug, Clone)]
pub struct IdentityPaths {
    /// 32-byte raw Curve25519 private key.
    pub key_file: PathBuf,
    /// `identity.toml` — the provisioning record.
    pub identity_file: PathBuf,
    /// `host-auth.toml` — PIN hash + plaintext + auth mode.
    pub host_auth_file: PathBuf,
}

impl IdentityPaths {
    /// Standard layout under a config root (`identity.toml`, `host-auth.toml`)
    /// plus a caller-supplied key-file path (the key lives in the data dir, not
    /// the config dir, so it is passed explicitly to match `prdt-host`).
    pub fn from_config_root(config_root: &Path, key_file: PathBuf) -> Self {
        Self {
            key_file,
            identity_file: config_root.join("identity.toml"),
            host_auth_file: config_root.join("host-auth.toml"),
        }
    }
}

/// Whether the device has a server-allocated 9-digit ID yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProvisioningState {
    /// No valid server-allocated ID (never provisioned, offline, or the
    /// signaling server changed). Key + PIN are still available.
    #[default]
    Unprovisioned,
    /// A server-allocated 9-digit ID is held in `host_id`.
    Provisioned,
}

/// The persisted identity record (`identity.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityRecord {
    /// Server-allocated 9-digit ID in dashed form (`123-456-789`). `None` until
    /// provisioned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    /// Full fingerprint of the device public key (SSH-style hex).
    pub key_fingerprint: String,
    /// The signaling server URL that allocated `host_id`. If the configured
    /// server changes, `host_id` is no longer valid and is reset.
    #[serde(default)]
    pub server_authority: String,
    #[serde(default)]
    pub provisioning_state: ProvisioningState,
}

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml decode: {0}")]
    TomlDecode(#[from] toml::de::Error),
    #[error("toml encode: {0}")]
    TomlEncode(#[from] toml::ser::Error),
    #[error("bcrypt: {0}")]
    Bcrypt(#[from] bcrypt::BcryptError),
    #[error("host auth: {0}")]
    HostAuth(#[from] HostAuthConfigError),
    #[error("no signaling server URL configured")]
    NoSignalingUrl,
    #[error("invalid signaling URL: {0}")]
    BadUrl(String),
    #[error("signaling: {0}")]
    Signaling(#[from] prdt_signaling_client::SignalingError),
}

/// The typed identity + provisioning API consumed by the GUI.
///
/// Cheap to clone (holds paths + cached record/config, not the private key —
/// only the public key is retained). Clone it into a background task to run
/// [`DeviceIdentity::reprovision`], then read back the updated state.
#[derive(Debug, Clone)]
pub struct DeviceIdentity {
    paths: IdentityPaths,
    pubkey_b64: String,
    fingerprint_full: String,
    fingerprint_short: String,
    record: IdentityRecord,
    host_auth: HostAuthConfig,
}

impl DeviceIdentity {
    /// Load or bootstrap the device identity. Offline-first: generates and
    /// `fsync`s the key if missing, ensures a PIN, and reconciles the record —
    /// all without any network I/O. `signaling_url` is the currently configured
    /// server (may be empty; provisioning is deferred until one is set).
    pub fn load_or_create(
        paths: IdentityPaths,
        signaling_url: &str,
    ) -> Result<Self, IdentityError> {
        // 1. Key first — generated + fsync'd before anything else.
        let keypair = prdt_crypto::load_or_create_keypair(&paths.key_file)?;
        let pubkey_b64 = keypair.public.to_base64();
        let fingerprint_full = keypair.public.fingerprint_full();
        let fingerprint_short = keypair.public.fingerprint_short();

        // 2. Reconcile the persisted record against the current key + server.
        let url = signaling_url.trim();
        let mut record = load_record(&paths.identity_file)?.unwrap_or_else(|| IdentityRecord {
            host_id: None,
            key_fingerprint: fingerprint_full.clone(),
            server_authority: url.to_string(),
            provisioning_state: ProvisioningState::Unprovisioned,
        });

        let mut dirty = !paths.identity_file.exists();

        // Key changed underneath us → the old ID belonged to a different
        // device. Start fresh.
        if record.key_fingerprint != fingerprint_full {
            record.key_fingerprint = fingerprint_full.clone();
            record.host_id = None;
            record.provisioning_state = ProvisioningState::Unprovisioned;
            record.server_authority = url.to_string();
            dirty = true;
        }

        // Server changed → the server-allocated ID is no longer valid. Only act
        // when a non-empty URL is configured, so an unconfigured launch doesn't
        // wipe a previously provisioned ID.
        if !url.is_empty() && record.server_authority != url {
            record.server_authority = url.to_string();
            record.host_id = None;
            record.provisioning_state = ProvisioningState::Unprovisioned;
            dirty = true;
        }

        // Keep state honest: a record claiming Provisioned with no ID is
        // Unprovisioned.
        if record.host_id.is_none() && record.provisioning_state == ProvisioningState::Provisioned {
            record.provisioning_state = ProvisioningState::Unprovisioned;
            dirty = true;
        }

        if dirty {
            save_record(&paths.identity_file, &record)?;
        }

        // 3. Ensure a fixed PIN exists (AC-2). Only bootstrap when host-auth.toml
        //    is absent (true first launch) so we never clobber a deliberate
        //    later choice made via the settings/onboarding UI.
        let host_auth = ensure_pin(&paths.host_auth_file)?;

        Ok(Self {
            paths,
            pubkey_b64,
            fingerprint_full,
            fingerprint_short,
            record,
            host_auth,
        })
    }

    /// The server-allocated 9-digit ID (dashed), or `None` when unprovisioned.
    pub fn host_id(&self) -> Option<&str> {
        self.record.host_id.as_deref()
    }

    pub fn provisioning_state(&self) -> ProvisioningState {
        self.record.provisioning_state
    }

    pub fn is_provisioned(&self) -> bool {
        self.record.provisioning_state == ProvisioningState::Provisioned
            && self.record.host_id.is_some()
    }

    /// Base64 (no-pad) device public key — the value sent to the signaling
    /// server and pinned by peers.
    pub fn pubkey_b64(&self) -> &str {
        &self.pubkey_b64
    }

    /// Full SSH-style key fingerprint for out-of-band verification (AC-14).
    pub fn fingerprint_full(&self) -> &str {
        &self.fingerprint_full
    }

    /// Compact fingerprint (`AABB:CCDD`) for at-a-glance display.
    pub fn fingerprint_short(&self) -> &str {
        &self.fingerprint_short
    }

    /// The configured signaling server URL this identity provisions against.
    pub fn server_authority(&self) -> &str {
        &self.record.server_authority
    }

    /// The plaintext fixed PIN for local display, if one has been generated.
    pub fn pin_plaintext(&self) -> Option<&str> {
        self.host_auth.pin_plaintext.as_deref()
    }

    pub fn pin_present(&self) -> bool {
        self.host_auth.pin_hash.is_some()
    }

    /// Regenerate the fixed PIN: new plaintext + bcrypt hash, persisted
    /// atomically to `host-auth.toml`. Ensures the mode is `Pin` so the new PIN
    /// actually gates connections. Returns the new plaintext for display.
    pub fn regenerate_pin(&mut self) -> Result<String, IdentityError> {
        let pin = HostAuthConfig::generate_pin();
        self.host_auth.set_pin(&pin)?;
        self.host_auth.mode = AuthMode::Pin;
        self.host_auth.save(&self.paths.host_auth_file)?;
        Ok(pin)
    }

    /// Provision (or re-confirm) the server-allocated ID. Idempotent by key:
    /// resends the persisted `host_id` (or empty) + pubkey, and the server
    /// returns the same ID for the same key. On success the record is updated
    /// and persisted; on failure state is left unchanged (still Unprovisioned).
    ///
    /// Never call this on the UI thread — it performs blocking network I/O
    /// across `.await`. Clone the identity into a task and read the result back.
    pub async fn reprovision(&mut self, timeout: Duration) -> Result<(), IdentityError> {
        let authority = self.record.server_authority.trim().to_string();
        if authority.is_empty() {
            return Err(IdentityError::NoSignalingUrl);
        }
        let url = url::Url::parse(&authority).map_err(|e| IdentityError::BadUrl(e.to_string()))?;
        let current = self.record.host_id.clone().unwrap_or_default();

        let allocated =
            prdt_signaling_client::register_host(&url, &current, &self.pubkey_b64, timeout).await?;

        self.record.host_id = Some(allocated);
        self.record.provisioning_state = ProvisioningState::Provisioned;
        save_record(&self.paths.identity_file, &self.record)?;
        Ok(())
    }
}

/// Load the identity record, returning `None` when the file is absent.
fn load_record(path: &Path) -> Result<Option<IdentityRecord>, IdentityError> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(toml::from_str(&s)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Persist the identity record atomically under an advisory lock (AC-12).
fn save_record(path: &Path, record: &IdentityRecord) -> Result<(), IdentityError> {
    // Hold the lock across serialize + write so a concurrent provisioner can't
    // interleave and lose our update.
    let _lock = prdt_crypto::FileLock::acquire(path)?;
    let s = toml::to_string_pretty(record)?;
    prdt_crypto::atomic_write(path, s.as_bytes())?;
    Ok(())
}

/// Ensure `host-auth.toml` exists with a usable fixed PIN.
///
/// * Absent → bootstrap in `Pin` mode with a fresh generated PIN (AC-2 first
///   launch, default = Pin).
/// * Present in `Pin` mode but missing a hash → generate one so the mode is
///   functional.
/// * Otherwise → respect the existing config unchanged.
fn ensure_pin(path: &Path) -> Result<HostAuthConfig, IdentityError> {
    if !path.exists() {
        let mut cfg = HostAuthConfig {
            mode: AuthMode::Pin,
            ..Default::default()
        };
        let pin = HostAuthConfig::generate_pin();
        cfg.set_pin(&pin)?;
        cfg.save(path)?;
        return Ok(cfg);
    }

    let mut cfg = HostAuthConfig::load_or_default(path)?;
    if cfg.mode == AuthMode::Pin && cfg.pin_hash.is_none() {
        let pin = HostAuthConfig::generate_pin();
        cfg.set_pin(&pin)?;
        cfg.save(path)?;
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths_in(dir: &Path) -> IdentityPaths {
        IdentityPaths {
            key_file: dir.join("host-key.bin"),
            identity_file: dir.join("identity.toml"),
            host_auth_file: dir.join("host-auth.toml"),
        }
    }

    #[test]
    fn first_launch_offline_is_unprovisioned_without_panic() {
        let dir = tempfile::tempdir().unwrap();
        // Empty signaling URL => no network attempt, offline-first.
        let id = DeviceIdentity::load_or_create(paths_in(dir.path()), "").unwrap();
        assert!(!id.is_provisioned());
        assert_eq!(id.provisioning_state(), ProvisioningState::Unprovisioned);
        assert_eq!(id.host_id(), None);
        // Key + fingerprint + PIN are all available despite being offline.
        assert!(!id.pubkey_b64().is_empty());
        assert!(!id.fingerprint_full().is_empty());
        assert!(!id.fingerprint_short().is_empty());
        assert!(id.pin_present());
        assert!(id.pin_plaintext().is_some());
        // The key and record were persisted.
        assert!(dir.path().join("host-key.bin").exists());
        assert!(dir.path().join("identity.toml").exists());
        assert!(dir.path().join("host-auth.toml").exists());
    }

    #[test]
    fn key_and_fingerprint_are_stable_across_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let a = DeviceIdentity::load_or_create(paths_in(dir.path()), "").unwrap();
        let b = DeviceIdentity::load_or_create(paths_in(dir.path()), "").unwrap();
        assert_eq!(a.pubkey_b64(), b.pubkey_b64());
        assert_eq!(a.fingerprint_full(), b.fingerprint_full());
        // PIN is fixed: a reload does not regenerate it.
        assert_eq!(a.pin_plaintext(), b.pin_plaintext());
    }

    #[test]
    fn regenerate_pin_changes_hash_and_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let mut id = DeviceIdentity::load_or_create(paths_in(dir.path()), "").unwrap();
        let first = id.pin_plaintext().unwrap().to_string();
        let regenerated = id.regenerate_pin().unwrap();
        assert_ne!(first, regenerated, "regenerate must change the PIN");
        assert_eq!(id.pin_plaintext(), Some(regenerated.as_str()));
        // Persisted: a fresh load sees the regenerated PIN and can verify it.
        let reloaded = HostAuthConfig::load_or_default(&dir.path().join("host-auth.toml")).unwrap();
        assert_eq!(
            reloaded.pin_plaintext.as_deref(),
            Some(regenerated.as_str())
        );
        assert!(reloaded.verify_pin(&regenerated));
        assert!(!reloaded.verify_pin(&first));
    }

    #[test]
    fn first_launch_sets_pin_mode_and_fixed_pin() {
        let dir = tempfile::tempdir().unwrap();
        let id = DeviceIdentity::load_or_create(paths_in(dir.path()), "").unwrap();
        let auth = HostAuthConfig::load_or_default(&dir.path().join("host-auth.toml")).unwrap();
        assert_eq!(auth.mode, AuthMode::Pin);
        assert!(auth.verify_pin(id.pin_plaintext().unwrap()));
    }

    #[test]
    fn existing_host_auth_is_respected() {
        // A prior TOFU choice must not be overwritten by identity bootstrap.
        let dir = tempfile::tempdir().unwrap();
        let auth_path = dir.path().join("host-auth.toml");
        let tofu = HostAuthConfig {
            mode: AuthMode::Tofu,
            ..Default::default()
        };
        tofu.save(&auth_path).unwrap();
        let id = DeviceIdentity::load_or_create(paths_in(dir.path()), "").unwrap();
        let auth = HostAuthConfig::load_or_default(&auth_path).unwrap();
        assert_eq!(auth.mode, AuthMode::Tofu, "must not force Pin over Tofu");
        assert!(!id.pin_present());
    }

    #[test]
    fn changing_server_resets_host_id() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths_in(dir.path());
        // Seed a record as if we had provisioned against server A.
        let seeded = IdentityRecord {
            host_id: Some("123-456-789".into()),
            key_fingerprint: {
                let kp = prdt_crypto::load_or_create_keypair(&p.key_file).unwrap();
                kp.public.fingerprint_full()
            },
            server_authority: "ws://server-a:9000".into(),
            provisioning_state: ProvisioningState::Provisioned,
        };
        save_record(&p.identity_file, &seeded).unwrap();

        // Same server → ID retained.
        let same = DeviceIdentity::load_or_create(p.clone(), "ws://server-a:9000").unwrap();
        assert_eq!(same.host_id(), Some("123-456-789"));
        assert!(same.is_provisioned());

        // Different server → ID reset to Unprovisioned.
        let moved = DeviceIdentity::load_or_create(p.clone(), "ws://server-b:9000").unwrap();
        assert_eq!(moved.host_id(), None);
        assert!(!moved.is_provisioned());
    }

    #[tokio::test]
    async fn reprovision_offline_returns_err_and_stays_unprovisioned() {
        let dir = tempfile::tempdir().unwrap();
        // Point at a closed port so the connect fails fast (refused).
        let mut id =
            DeviceIdentity::load_or_create(paths_in(dir.path()), "ws://127.0.0.1:1").unwrap();
        assert!(!id.is_provisioned());
        let res = id.reprovision(Duration::from_secs(2)).await;
        assert!(res.is_err(), "unreachable server must error");
        // State must be untouched — no phantom ID, still Unprovisioned.
        assert!(!id.is_provisioned());
        assert_eq!(id.host_id(), None);
    }

    #[tokio::test]
    async fn reprovision_without_url_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut id = DeviceIdentity::load_or_create(paths_in(dir.path()), "").unwrap();
        let err = id.reprovision(Duration::from_secs(1)).await.unwrap_err();
        assert!(matches!(err, IdentityError::NoSignalingUrl));
    }
}
