//! P6 AuthValidator — single source of truth for the Hello-time auth decision.
//!
//! Drives the §6 state machine: protocol_version → auth_payload size cap →
//! dispatch on `config.mode` → mode-specific verification (TOFU consent
//! prompt, PIN bcrypt, Ephemeral constant-time compare) → AuthVerdict.
//!
//! Codec validity is enforced downstream by the encoder negotiation layer
//! (the transport `host_handshake` function), not here.
//!
//! Per-mode known_peers semantics (see spec §6.3):
//! - Tofu: known_peers means "auto-accept, skip prompt".
//! - Pin/Ephemeral: known_peers only sources `peer.permissions` *after* the
//!   auth payload passes. The auth payload is required every connection.

use crate::auth_config::{AuthMode, HostAuthConfig};
use prdt_crypto::known_peers::KnownPeers;
use prdt_protocol::{AuthMethod, ControlMessage, HelloRejectCode, PermissionSet};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};

const PROTOCOL_VERSION_REQUIRED: u8 = 3;
const AUTH_PAYLOAD_MAX_BYTES: usize = 64;

/// The result of a single Hello-time auth decision.
#[derive(Debug)]
pub enum AuthVerdict {
    /// Authentication succeeded. `permissions` is the set the session
    /// runs under (immutable from this point on). `remember` is true when
    /// the host should persist this peer (TOFU known-peer fast path);
    /// false for PIN/Ephemeral (no auto-add) and for first-time TOFU
    /// unknowns (the GUI prompt sets it).
    Granted {
        permissions: PermissionSet,
        remember: bool,
    },
    /// Authentication failed. `code` tells the viewer what UI to show next
    /// (PIN dialog, ephemeral dialog, version-mismatch error, etc.).
    Rejected {
        code: HelloRejectCode,
        reason: String,
    },
    /// TOFU mode + unknown peer; the host control loop must surface a GUI
    /// consent prompt and call back into the session with the decision.
    NeedsConsent {
        peer_pubkey_b64: String,
        default_permissions: PermissionSet,
    },
}

#[derive(Debug, Clone)]
struct PinAttemptState {
    failed_count: u8,
    locked_until: Option<Instant>,
}

#[derive(Debug, Clone)]
struct EphemeralState {
    value: String,
    /// Process-local monotonic timestamp; ephemerals are RAM-only and do not
    /// survive host restart (spec §5.1).
    created_at: Instant,
}

/// The main P6 authentication state machine.
///
/// One instance lives for the lifetime of the host process (shared across
/// concurrent session accept loops via `Arc`). Mutable state is behind
/// interior-mutability primitives (Mutex / RwLock).
///
/// **Config lifecycle**: `config` is captured by value at construction. If
/// `HostAuthConfig` is edited after the validator was built (e.g. T7's wizard
/// changes the PIN or mode), the validator must be rebuilt — live edits do NOT
/// propagate. In practice this is fine because the host drops and recreates the
/// validator between sessions; a mid-session Settings change takes effect on
/// the next incoming connection.
pub struct AuthValidator {
    config: HostAuthConfig,
    known_peers: Arc<RwLock<KnownPeers>>,
    ephemeral: Arc<Mutex<Option<EphemeralState>>>,
    /// Keyed by peer_pubkey_b64. Stale tombstone entries (failed_count == 0,
    /// no active lockout) are pruned unconditionally on every PIN validate call
    /// to bound map growth regardless of whether the lockout fast-path fires.
    pin_attempts: Arc<Mutex<HashMap<String, PinAttemptState>>>,
}

impl AuthValidator {
    pub fn new(config: HostAuthConfig, known_peers: Arc<RwLock<KnownPeers>>) -> Self {
        Self {
            config,
            known_peers,
            ephemeral: Arc::new(Mutex::new(None)),
            pin_attempts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Generate a new ephemeral and replace the previous one (which is
    /// invalidated immediately). Returns the new value so the GUI can show
    /// it to the user.
    pub async fn rotate_ephemeral(&self) -> String {
        let new = HostAuthConfig::generate_ephemeral();
        *self.ephemeral.lock().await = Some(EphemeralState {
            value: new.clone(),
            created_at: Instant::now(),
        });
        info!(ephemeral_len = new.len(), "rotated ephemeral");
        new
    }

    /// Synchronous wrapper around `rotate_ephemeral` for use in tests that
    /// run inside a `#[tokio::test]` context but cannot easily `.await` in
    /// the test setup section.
    ///
    /// Requires the multi-thread tokio runtime; will panic under current_thread.
    #[doc(hidden)]
    pub fn rotate_ephemeral_for_test(&self) -> String {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.rotate_ephemeral())
        })
    }

    /// Evaluate a Hello message and return an auth verdict.
    ///
    /// The caller is responsible for wiring the verdict into HelloAck /
    /// HelloReject / consent prompt as appropriate.
    pub async fn validate(&self, msg: &ControlMessage, peer_pubkey_b64: &str) -> AuthVerdict {
        let ControlMessage::Hello {
            protocol_version,
            auth_method,
            auth_payload,
            ..
        } = msg
        else {
            return AuthVerdict::Rejected {
                code: HelloRejectCode::Unspecified,
                reason: "expected Hello".into(),
            };
        };

        // §6 state machine — top to bottom.

        // 1. Protocol version gate.
        if *protocol_version != PROTOCOL_VERSION_REQUIRED {
            return AuthVerdict::Rejected {
                code: HelloRejectCode::ProtocolVersionMismatch,
                reason: format!(
                    "host requires protocol_version={PROTOCOL_VERSION_REQUIRED} (P6+); got {protocol_version}"
                ),
            };
        }

        // 2. Auth payload size cap (before any crypto work).
        if auth_payload.len() > AUTH_PAYLOAD_MAX_BYTES {
            return AuthVerdict::Rejected {
                code: HelloRejectCode::Unspecified,
                reason: "auth_payload too long".into(),
            };
        }

        // 3. Dispatch on config.mode (authoritative). Viewer's auth_method is
        //    just a hint; mismatches produce a mode-specific reject code so the
        //    viewer can prompt the correct dialog.
        match self.config.mode {
            AuthMode::Tofu => self.validate_tofu(*auth_method, peer_pubkey_b64).await,
            AuthMode::Pin => {
                self.validate_pin(*auth_method, auth_payload, peer_pubkey_b64)
                    .await
            }
            AuthMode::Ephemeral => {
                self.validate_ephemeral(*auth_method, auth_payload, peer_pubkey_b64)
                    .await
            }
        }
    }

    // -----------------------------------------------------------------------
    // Mode-specific validators
    // -----------------------------------------------------------------------

    async fn validate_tofu(&self, viewer_method: AuthMethod, peer: &str) -> AuthVerdict {
        if viewer_method != AuthMethod::Tofu {
            return AuthVerdict::Rejected {
                code: HelloRejectCode::ConsentDenied,
                reason: "host is in TOFU mode; viewer must set auth_method=Tofu".into(),
            };
        }
        let known = self.known_peers.read().await;
        if let Some(p) = known.peers.iter().find(|p| p.pubkey_b64 == peer) {
            debug!(peer = %peer, "TOFU known-peer fast path");
            return AuthVerdict::Granted {
                permissions: p.permissions,
                remember: true,
            };
        }
        AuthVerdict::NeedsConsent {
            peer_pubkey_b64: peer.to_string(),
            default_permissions: self.config.default_permissions,
        }
    }

    async fn validate_pin(
        &self,
        viewer_method: AuthMethod,
        payload: &[u8],
        peer: &str,
    ) -> AuthVerdict {
        if viewer_method != AuthMethod::Pin {
            return AuthVerdict::Rejected {
                code: HelloRejectCode::PinRequired,
                reason: "host is in PIN mode; viewer must set auth_method=Pin".into(),
            };
        }

        // Lockout check — hold the lock briefly, then release before bcrypt.
        {
            let mut attempts = self.pin_attempts.lock().await;
            // Prune stale tombstones unconditionally (before any early return)
            // so map growth is bounded regardless of whether lockout fires.
            attempts.retain(|_, s| {
                s.failed_count > 0 || s.locked_until.map(|t| Instant::now() < t).unwrap_or(false)
            });
            if let Some(state) = attempts.get(peer) {
                if let Some(locked_until) = state.locked_until {
                    if Instant::now() < locked_until {
                        warn!(peer = %peer, "PIN attempt during lockout window");
                        return AuthVerdict::Rejected {
                            code: HelloRejectCode::AuthLockout,
                            reason: "too many wrong PINs; please wait for lockout to expire".into(),
                        };
                    }
                }
            }
        }

        // UTF-8 decode (bcrypt operates on &str).
        let plain = match std::str::from_utf8(payload) {
            Ok(s) => s,
            Err(_) => {
                return AuthVerdict::Rejected {
                    code: HelloRejectCode::AuthFailed,
                    reason: "PIN must be valid UTF-8".into(),
                };
            }
        };

        // bcrypt verify (potentially slow; not holding any lock).
        let ok = self.config.verify_pin(plain);

        if !ok {
            let mut attempts = self.pin_attempts.lock().await;
            let entry = attempts.entry(peer.to_string()).or_insert(PinAttemptState {
                failed_count: 0,
                locked_until: None,
            });
            entry.failed_count += 1;
            if entry.failed_count >= self.config.max_pin_attempts {
                let until = Instant::now()
                    + Duration::from_secs(u64::from(self.config.pin_lockout_seconds));
                entry.locked_until = Some(until);
                warn!(
                    peer = %peer,
                    count = entry.failed_count,
                    lockout_secs = self.config.pin_lockout_seconds,
                    "PIN lockout fired"
                );
            }
            return AuthVerdict::Rejected {
                code: HelloRejectCode::AuthFailed,
                reason: format!(
                    "wrong PIN ({}/{})",
                    entry.failed_count, self.config.max_pin_attempts
                ),
            };
        }

        // Success — reset per-peer failure counter.
        self.pin_attempts.lock().await.remove(peer);
        info!(peer = %peer, "PIN auth success");

        // Look up per-peer permissions (saved from a prior consent flow), falling
        // back to the host-wide default.
        let permissions = {
            let known = self.known_peers.read().await;
            known
                .peers
                .iter()
                .find(|p| p.pubkey_b64 == peer)
                .map(|p| p.permissions)
                .unwrap_or(self.config.default_permissions)
        };
        // PIN mode: remember=false. Peers are added to known_peers only via the
        // onboarding wizard / Settings tab (T7), not automatically on PIN success.
        AuthVerdict::Granted {
            permissions,
            remember: false,
        }
    }

    async fn validate_ephemeral(
        &self,
        viewer_method: AuthMethod,
        payload: &[u8],
        peer: &str,
    ) -> AuthVerdict {
        if viewer_method != AuthMethod::Ephemeral {
            return AuthVerdict::Rejected {
                code: HelloRejectCode::EphemeralRequired,
                reason: "host is in Ephemeral mode; viewer must set auth_method=Ephemeral".into(),
            };
        }

        // Acquire the ephemeral lock. We hold it through the comparison so
        // that two concurrent viewers racing can't both pass against the same
        // ephemeral before one of them clears it.
        let mut guard = self.ephemeral.lock().await;

        let eph = match guard.as_ref() {
            Some(e) => e,
            None => {
                return AuthVerdict::Rejected {
                    code: HelloRejectCode::AuthFailed,
                    reason: "no active ephemeral; host operator must generate one first".into(),
                };
            }
        };

        // Expiry check.
        let age = Instant::now().duration_since(eph.created_at);
        if age > Duration::from_secs(u64::from(self.config.ephemeral_lifetime_seconds)) {
            warn!(peer = %peer, age_ms = age.as_millis(), "ephemeral expired");
            return AuthVerdict::Rejected {
                code: HelloRejectCode::AuthFailed,
                reason: "ephemeral expired".into(),
            };
        }

        // Length-mismatch is itself a reject (spec §14). Checked BEFORE the
        // constant-time compare so we never call ct_eq on slices of different
        // length (subtle requires them to match for the timing guarantee).
        let expected = eph.value.as_bytes();
        if payload.len() != expected.len() {
            return AuthVerdict::Rejected {
                code: HelloRejectCode::AuthFailed,
                reason: "ephemeral length mismatch".into(),
            };
        }

        // Constant-time compare (subtle::ConstantTimeEq).
        let matches: bool = expected.ct_eq(payload).into();
        if !matches {
            return AuthVerdict::Rejected {
                code: HelloRejectCode::AuthFailed,
                reason: "ephemeral mismatch".into(),
            };
        }

        // Single-use: clear the ephemeral so a second viewer cannot reuse it.
        *guard = None;
        drop(guard);
        debug!(peer = %peer, "ephemeral consumed (single-use)");

        // Look up per-peer permissions.
        let permissions = {
            let known = self.known_peers.read().await;
            known
                .peers
                .iter()
                .find(|p| p.pubkey_b64 == peer)
                .map(|p| p.permissions)
                .unwrap_or(self.config.default_permissions)
        };
        // Ephemeral mode: remember=false (same rationale as PIN).
        AuthVerdict::Granted {
            permissions,
            remember: false,
        }
    }
}
