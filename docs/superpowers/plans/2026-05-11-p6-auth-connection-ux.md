# P6 Auth & Connection UX Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add three-mode host authentication (TOFU / PIN / Ephemeral) with per-peer immutable PermissionSet (input/clipboard/file-transfer/audio) negotiated at Hello time and enforced per-channel on the host. Surface PIN dialog + connection history + onboarding wizard in the GUI.

**Architecture:** `protocol_version` 2 → 3. Hello carries `auth_method` + `auth_payload`, HelloAck carries `granted_permissions`, HelloReject carries `code: HelloRejectCode`. New module `crates/host/src/auth.rs` runs an `AuthValidator` state machine driven from the existing handshake task; `config.mode` (from `HostAuthConfig`) is authoritative — viewer's claimed `auth_method` is just a hint, mismatches respond `HelloReject{PinRequired|EphemeralRequired}`. `KnownPeer` schema extends with `permissions: PermissionSet` (serde defaulted to `deny_all` for legacy entries so old files force one re-approval). Per-channel enforcement is a thin guard in the host control-loop: `Input` task spawn / `Audio` task spawn are gated; `ClipboardText` / `FileTransfer*` ControlMessage arms early-out. Viewer auth flow is a Hello / HelloReject retry loop. signaling-proto gains `ProbeHosts/ProbeResult` for the online-badge UX.

**Tech Stack:** Rust 1.85, edition 2021, `bcrypt = "0.15"`, `subtle = "2"` (constant-time compare), `tokio` async, `tracing`, `serde + toml`, existing `qrcode` crate for the wizard QR. egui (existing) for GUI bits.

**Spec:** `docs/superpowers/specs/2026-05-11-p6-auth-connection-ux-design.md` (commit `71823fc`)

**Branch:** `phase-p6-auth-connection-ux`

**Tag (on completion):** `phase-p6-auth-connection-ux-complete`

**Cross-platform regression bar:** Linux + Windows both green for `cargo build/clippy/test --workspace -- -D warnings` (matches L0-L4 + P5A bar). Manual GUI smoke walkthrough on both platforms is part of T9 acceptance.

---

## File Structure

### Created

| Path | Responsibility |
|---|---|
| `crates/host/src/auth.rs` | `AuthValidator`, `AuthVerdict`, `PinAttemptState`, `EphemeralState`, rate-limit + bcrypt + constant-time compare |
| `crates/host/src/auth_config.rs` | `HostAuthConfig`, `AuthMode`, `host-auth.toml` (de)serialization |
| `crates/host/tests/auth_integration.rs` | pin_auth_success / pin_auth_wrong_then_correct / ephemeral_auth_success / ephemeral_expired / tofu_consent_remember_persists / protocol_version_mismatch / permission_deny_* |
| `crates/gui-host/src/onboarding.rs` | First-run wizard modal (Welcome → ID/QR → Auth-mode → PIN setup → Defaults → Done) |
| `crates/gui-host/src/auth_settings.rs` | Settings-tab auth subsection + Saved-peers list + Show-ephemeral / Rotate button |
| `crates/gui-viewer/src/auth_dialog.rs` | PIN / Ephemeral entry dialog used when HelloReject demands it |
| `crates/gui-viewer/src/online_probe.rs` | 30s polling task that asks signaling-server `ProbeHosts` |

### Modified

| Path | Change |
|---|---|
| `crates/protocol/src/control.rs` | Add `AuthMethod`, `PermissionSet`, `HelloRejectCode` types. Extend `Hello` (+ auth_method, auth_payload), `HelloAck` (+ granted_permissions), `HelloReject` (+ code). Bump default `protocol_version` in tests 2 → 3. |
| `crates/protocol/Cargo.toml` | No new deps; `serde` derive already present. |
| `crates/host/Cargo.toml` | `bcrypt = "0.15"`, `subtle = "2"`, optional `dirs` (already used elsewhere). |
| `crates/host/src/lib.rs` | Wire `AuthValidator` into the Hello handler. Replace existing `ConsentRequest` dispatch with the new AuthVerdict flow. Add `permissions: PermissionSet` to `SessionState`. Gate `Input`/`Audio` task spawns + `ClipboardText`/`FileTransfer*` arms. CLI flags `--auth-mode`/`--pin`/`--ephemeral-print`/`--allow-*`/`--no-*`. |
| `crates/crypto/src/known_peers.rs` | Extend `KnownPeer` with `permissions: PermissionSet`, `first_seen_at: SystemTime`, `last_seen_at: SystemTime`, all with `#[serde(default)]`. |
| `crates/viewer/src/lib.rs` | Hello / HelloReject retry loop. CLI flags `--pin <PIN>` / `--ephemeral <EPH>` / `--no-auth-prompt`. Wire `granted_permissions` to stats payload for overlay rendering. |
| `crates/viewer/src/overlay_ipc.rs` + `crates/viewer-overlay/src/ipc.rs` | Add `granted_permissions: Option<PermissionSet>` to StatsPayload (serde defaulted for backward compat). |
| `crates/viewer-overlay/src/app.rs` | Render the permission line under the codec line; greyed-out icons for denied channels. |
| `crates/signaling-proto/src/lib.rs` | Add `ClientMessage::ProbeHosts { host_ids: Vec<String> }` and `ServerMessage::ProbeResult { online: Vec<String> }`. |
| `crates/signaling-server/src/lib.rs` (or wherever WS handlers live) | Implement `ProbeHosts` handler: intersect requested IDs with the in-memory online table, reply `ProbeResult`. |
| `crates/signaling-client/src/lib.rs` | `pub async fn probe_hosts(&self, ids: Vec<String>) -> Result<Vec<String>>`. |
| `crates/gui-common/src/config.rs` | `HostEntry.last_connected` String → SystemTime (with String-tolerant custom Deserialize), add `last_known_online: Option<bool>`. Add `gui.onboarded: bool` (serde default = false). |
| `crates/gui-viewer/src/hosts_list.rs` | Sort by `last_connected DESC` + online-up; relative-time + online badge rendering. |
| `crates/gui-viewer/src/connect_form.rs` | Update last_connected on successful connect. |
| `crates/gui-host/src/lib.rs` (or `app.rs`) | Run onboarding wizard if `config.gui.onboarded == false`; mount auth subsection in Settings; permission-prompt modal extension (4 toggle + Remember checkbox). |
| `docs/superpowers/STATUS.md` | Add P6 entry under B2 / dedicated subsection; update Latest tag. |

---

## Task list overview

| # | Task | Files | Tests |
|---|---|---|---|
| T1 | Wire types: `AuthMethod`, `PermissionSet`, `HelloRejectCode` + extend Hello/HelloAck/HelloReject + protocol_version 2 → 3 + branch creation. | protocol/control.rs | bincode round-trip, kind_u8 stability, protocol_version constants |
| T2 | `HostAuthConfig` + `host-auth.toml` (de)serialization + bcrypt hash setup CLI + `EphemeralState` generator + `KnownPeer` schema extension. | host/{auth_config.rs, lib.rs}, crypto/known_peers.rs, host/Cargo.toml | config_round_trip, ephemeral_random_no_ambiguous_chars, known_peer_legacy_load |
| T3 | `AuthValidator` module + rate limiter + handshake integration in host. 6 integration tests (pin_*, ephemeral_*, tofu_*, protocol_version_mismatch). | host/{auth.rs, lib.rs}, host/tests/auth_integration.rs | pin_auth_success, pin_auth_wrong_then_correct, ephemeral_auth_success, ephemeral_expired, tofu_consent_remember_persists, protocol_version_mismatch |
| T4 | Per-channel enforcement: gate Input/Audio task spawn + ControlMessage arms (Clipboard, FileTransfer*). | host/lib.rs, host/tests/auth_integration.rs | permission_deny_clipboard, permission_deny_file_transfer, permission_deny_input, permission_deny_audio |
| T5 | Viewer Hello/HelloReject retry loop + CLI flags + stats payload extension. | viewer/lib.rs, viewer/src/overlay_ipc.rs | viewer_retries_on_pin_required, viewer_fails_fast_with_no_auth_prompt |
| T6 | signaling-proto `ProbeHosts`/`ProbeResult` + server handler + client method + viewer 30s poll. | signaling-{proto,server,client}/src/lib.rs, gui-viewer/online_probe.rs | probe_hosts_returns_intersection, probe_hosts_empty_list_ok |
| T7 | gui-host onboarding wizard + Settings auth subsection + permission prompt modal extension. | gui-host/{onboarding.rs, auth_settings.rs, lib.rs}, gui-common/config.rs (`gui.onboarded`) | wizard_writes_host_auth_toml (headless unit test of the wizard's submit handler) |
| T8 | gui-viewer hosts_list rewrite (sort + relative-time + online badge) + last_connected migration + viewer-overlay permission line. | gui-viewer/{hosts_list.rs, connect_form.rs}, gui-common/config.rs, viewer-overlay/{ipc.rs, app.rs} | relative_time_buckets, last_connected_legacy_string_parses, permission_line_renders |
| T9 | STATUS.md P6 entry + manual smoke walkthrough (Win+Linux) + annotated tag. | docs/superpowers/STATUS.md | (manual) |

---

## Conventions for every task

- Use `superpowers:test-driven-development`: write failing test → run to verify failure → minimal impl → run to verify pass → commit.
- `cargo fmt --all` before every commit (avoid the L4/P5A T8 rustfmt-fail experience).
- `cargo clippy --workspace --all-targets -- -D warnings` before every commit; if a clippy lint requires an `#[allow(...)]`, leave a one-line comment why.
- Each commit message follows `<scope>(p6/<topic>): <short>` form, e.g. `feat(p6/protocol): add AuthMethod/PermissionSet/HelloRejectCode + extend Hello`.
- Use `tracing::info!` for state transitions and `tracing::debug!` for permission drops; no `println!` in non-CLI code.
- All bincode round-trip tests pin `kind_u8()` values and field positions.

---

## Task 1: Wire types + Hello/HelloAck/HelloReject extension + branch

**Files:**
- Modify: `crates/protocol/src/control.rs`
- (Branch) Create: `phase-p6-auth-connection-ux` from `master`

- [ ] **Step 1: Create branch**

```bash
git checkout -b phase-p6-auth-connection-ux master
git log -1 --oneline   # confirm starting point is the P5A merge (58ba2b5 or later)
```

- [ ] **Step 2: Write failing test for AuthMethod / PermissionSet / HelloRejectCode round-trip**

Append to `crates/protocol/src/control.rs` test module:

```rust
#[test]
fn auth_method_round_trip() {
    for m in [AuthMethod::Tofu, AuthMethod::Pin, AuthMethod::Ephemeral] {
        let bytes = bincode::serialize(&m).unwrap();
        let back: AuthMethod = bincode::deserialize(&bytes).unwrap();
        assert_eq!(m, back);
    }
}

#[test]
fn auth_method_discriminants_stable() {
    assert_eq!(bincode::serialize(&AuthMethod::Tofu).unwrap()[0], 0);
    assert_eq!(bincode::serialize(&AuthMethod::Pin).unwrap()[0], 1);
    assert_eq!(bincode::serialize(&AuthMethod::Ephemeral).unwrap()[0], 2);
}

#[test]
fn permission_set_round_trip_and_constructors() {
    let s = PermissionSet {
        input: true,
        clipboard: false,
        file_transfer: true,
        audio: false,
    };
    let bytes = bincode::serialize(&s).unwrap();
    let back: PermissionSet = bincode::deserialize(&bytes).unwrap();
    assert_eq!(s, back);

    let all = PermissionSet::all();
    assert!(all.input && all.clipboard && all.file_transfer && all.audio);

    let vo = PermissionSet::view_only();
    assert!(!vo.input && !vo.clipboard && !vo.file_transfer && vo.audio);

    let deny = PermissionSet::deny_all();
    assert!(!deny.input && !deny.clipboard && !deny.file_transfer && !deny.audio);
}

#[test]
fn hello_reject_code_round_trip_and_discriminants() {
    let codes = [
        (HelloRejectCode::Unspecified, 0u8),
        (HelloRejectCode::ProtocolVersionMismatch, 1),
        (HelloRejectCode::UnsupportedCodec, 2),
        (HelloRejectCode::PinRequired, 3),
        (HelloRejectCode::EphemeralRequired, 4),
        (HelloRejectCode::AuthFailed, 5),
        (HelloRejectCode::AuthLockout, 6),
        (HelloRejectCode::ConsentDenied, 7),
    ];
    for (c, disc) in codes {
        let bytes = bincode::serialize(&c).unwrap();
        assert_eq!(bytes[0], disc, "{c:?} discriminant changed");
        let back: HelloRejectCode = bincode::deserialize(&bytes).unwrap();
        assert_eq!(c, back);
    }
}

#[test]
fn hello_round_trip_with_auth_fields() {
    let h = ControlMessage::Hello {
        protocol_version: 3,
        req_width: 1920,
        req_height: 1080,
        req_fps: 60,
        codec: Codec::H265,
        auth_method: AuthMethod::Pin,
        auth_payload: b"correct horse battery staple".to_vec(),
    };
    let bytes = bincode::serialize(&h).unwrap();
    let back: ControlMessage = bincode::deserialize(&bytes).unwrap();
    assert_eq!(h, back);
}

#[test]
fn hello_ack_round_trip_with_permissions() {
    let h = ControlMessage::HelloAck {
        session_id: 0xDEADBEEF,
        host_monotonic_base_us: 42,
        neg_width: 1920,
        neg_height: 1080,
        neg_fps: 60,
        neg_bitrate_bps: 30_000_000,
        host_monitor_rect: MonitorRect::new(0, 0, 1920, 1080),
        host_virtual_desktop_rect: MonitorRect::new(0, 0, 1920, 1080),
        negotiated_codec: Codec::H265,
        host_supported_codecs: vec![Codec::H265, Codec::H264],
        granted_permissions: PermissionSet::view_only(),
    };
    let bytes = bincode::serialize(&h).unwrap();
    let back: ControlMessage = bincode::deserialize(&bytes).unwrap();
    assert_eq!(h, back);
}

#[test]
fn hello_reject_round_trip_with_code() {
    let r = ControlMessage::HelloReject {
        reason: "PIN required".into(),
        code: HelloRejectCode::PinRequired,
    };
    let bytes = bincode::serialize(&r).unwrap();
    let back: ControlMessage = bincode::deserialize(&bytes).unwrap();
    assert_eq!(r, back);
}

#[test]
fn kind_u8_values_unchanged_by_p6() {
    // P6 only extends existing variants; no new discriminants.
    assert_eq!(
        ControlMessage::Hello {
            protocol_version: 3,
            req_width: 0,
            req_height: 0,
            req_fps: 0,
            codec: Codec::H265,
            auth_method: AuthMethod::Tofu,
            auth_payload: vec![],
        }
        .kind_u8(),
        0
    );
    assert_eq!(
        ControlMessage::HelloReject {
            reason: "".into(),
            code: HelloRejectCode::Unspecified,
        }
        .kind_u8(),
        22
    );
}
```

- [ ] **Step 3: Run tests to verify failure**

Run: `cargo test -p prdt-protocol control::tests 2>&1 | head -40`
Expected: compile errors — `AuthMethod`, `PermissionSet`, `HelloRejectCode` don't exist, Hello fields missing.

- [ ] **Step 4: Add the types and extend the variants**

At the top of `crates/protocol/src/control.rs` (after existing `use` lines), insert:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum AuthMethod {
    Tofu = 0,
    Pin = 1,
    Ephemeral = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PermissionSet {
    pub input: bool,
    pub clipboard: bool,
    pub file_transfer: bool,
    pub audio: bool,
}

impl PermissionSet {
    pub const fn all() -> Self {
        Self { input: true, clipboard: true, file_transfer: true, audio: true }
    }
    pub const fn view_only() -> Self {
        Self { input: false, clipboard: false, file_transfer: false, audio: true }
    }
    pub const fn deny_all() -> Self {
        Self { input: false, clipboard: false, file_transfer: false, audio: false }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum HelloRejectCode {
    Unspecified = 0,
    ProtocolVersionMismatch = 1,
    UnsupportedCodec = 2,
    PinRequired = 3,
    EphemeralRequired = 4,
    AuthFailed = 5,
    AuthLockout = 6,
    ConsentDenied = 7,
}
```

Extend `ControlMessage::Hello` (add the two new fields at the end):

```rust
Hello {
    protocol_version: u8,
    req_width: u32,
    req_height: u32,
    req_fps: u32,
    codec: Codec,
    auth_method: AuthMethod,
    auth_payload: Vec<u8>,
},
```

Extend `ControlMessage::HelloAck` (add `granted_permissions` last):

```rust
HelloAck {
    session_id: u64,
    host_monotonic_base_us: u64,
    neg_width: u32,
    neg_height: u32,
    neg_fps: u32,
    neg_bitrate_bps: u32,
    host_monitor_rect: MonitorRect,
    host_virtual_desktop_rect: MonitorRect,
    negotiated_codec: Codec,
    host_supported_codecs: Vec<Codec>,
    granted_permissions: PermissionSet,
},
```

Extend `ControlMessage::HelloReject` (add `code` last):

```rust
HelloReject {
    reason: String,
    code: HelloRejectCode,
},
```

Update the existing `helloreject_round_trip` test to include the new field (still keep it, just add `code: HelloRejectCode::Unspecified`).

Update the existing `helloack_negotiated_codec_round_trip` test to include `granted_permissions: PermissionSet::all()`.

Update the existing `control_kinds_are_stable` test's Hello literal to include `auth_method: AuthMethod::Tofu, auth_payload: vec![]`.

- [ ] **Step 5: Find every other site that constructs Hello/HelloAck/HelloReject and update**

The implementer should grep:

```bash
grep -rn "Hello {" crates/ | grep -v "//" | grep -v "test"
grep -rn "HelloAck {" crates/
grep -rn "HelloReject {" crates/
```

Likely call sites (non-exhaustive — implementer verifies):

- `crates/viewer/src/lib.rs` — viewer sends Hello.
- `crates/host/src/lib.rs` — host sends HelloAck / HelloReject.
- `crates/transport/tests/*.rs` — possibly literals in transport integration tests.
- `crates/host/tests/*.rs`.

For each:
- Hello literals: add `auth_method: AuthMethod::Tofu, auth_payload: vec![]` (T1's default; later tasks change these per-call).
- HelloAck literals: add `granted_permissions: PermissionSet::all()` (preserves pre-P6 behaviour).
- HelloReject literals: add `code: HelloRejectCode::Unspecified` (same).
- Bump any hard-coded `protocol_version: 2` to `protocol_version: 3` *only in producer sites*; receiver-side tests stay if they're testing version mismatch.

- [ ] **Step 6: Run tests**

Run: `cargo test --workspace --lib 2>&1 | tail -40`
Expected: all green. If anything fails because of a Hello/HelloAck literal you missed, fix and re-run.

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20`
Expected: zero warnings.

Run: `cargo fmt --all`

- [ ] **Step 7: Commit**

```bash
git add crates/protocol/src/control.rs
# plus any non-test sites you had to update (transport/host/viewer)
git commit -m "feat(p6/protocol): AuthMethod/PermissionSet/HelloRejectCode + extend Hello/HelloAck/HelloReject"
```

---

## Task 2: HostAuthConfig + host-auth.toml + KnownPeer schema

**Files:**
- Create: `crates/host/src/auth_config.rs`
- Modify: `crates/host/Cargo.toml` (deps: bcrypt, subtle)
- Modify: `crates/host/src/lib.rs` (`pub mod auth_config;`)
- Modify: `crates/crypto/src/known_peers.rs` (schema extension)
- Test: `crates/host/src/auth_config.rs` (inline) + `crates/crypto/src/known_peers.rs` (inline)

- [ ] **Step 1: Add bcrypt + subtle to host Cargo.toml**

```toml
# crates/host/Cargo.toml
[dependencies]
# ... existing ...
bcrypt = "0.15"
subtle = "2"
```

Run: `cargo check -p prdt-host`
Expected: deps resolve, build passes.

- [ ] **Step 2: Write failing test for HostAuthConfig round-trip**

Create `crates/host/src/auth_config.rs`:

```rust
//! Host-side auth + permissions configuration (P6).
//!
//! Persisted to `~/.config/prdt/host-auth.toml` (or `%APPDATA%\prdt\host-auth.toml`).
//! The PIN is stored as a bcrypt hash, never plaintext. The ephemeral is in
//! memory only.

use prdt_protocol::{AuthMethod, PermissionSet};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthMode { Tofu, Pin, Ephemeral }

impl Default for AuthMode {
    fn default() -> Self { AuthMode::Tofu }
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

fn default_ephemeral_lifetime_seconds() -> u32 { 120 }
fn default_permissions() -> PermissionSet { PermissionSet::all() }
fn default_max_pin_attempts() -> u8 { 5 }
fn default_pin_lockout_seconds() -> u32 { 300 }
fn default_consent_timeout_seconds() -> u32 { 60 }

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
            Some(h) => bcrypt::verify(plain, h).unwrap_or(false),
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
            default_permissions: PermissionSet { input: true, clipboard: false, file_transfer: true, audio: false },
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
                assert!(!matches!(ch, '0' | 'O' | '1' | 'I' | 'L'),
                    "ephemeral contains ambiguous char: {e}");
                assert!(ch.is_ascii_alphanumeric() && (ch.is_ascii_uppercase() || ch.is_ascii_digit()));
            }
        }
    }
}
```

Add `pub mod auth_config;` to `crates/host/src/lib.rs`.

Add `rand = "0.8"` to `crates/host/Cargo.toml` if not already there. (Likely already a transitive dep — verify with `cargo tree -p prdt-host -e all | grep rand`.)

- [ ] **Step 3: Run tests to verify failure (only `auth_config.rs` is new, expect compile errors only if missing rand)**

Run: `cargo test -p prdt-host auth_config 2>&1 | tail -20`
Expected: tests pass (this is a fresh module so step 3 effectively merges with 4 here).

- [ ] **Step 4: Write failing test for KnownPeer schema extension**

Find `crates/crypto/src/known_peers.rs`. Append to its test module:

```rust
#[test]
fn legacy_known_peer_loads_with_default_permissions() {
    // Pre-P6 format: only pubkey_b64 + label, no permissions/timestamps.
    let toml = r#"
[[peers]]
pubkey_b64 = "abc"
label = "old-laptop"
"#;
    let store: KnownPeers = toml::from_str(toml).expect("legacy format should parse");
    assert_eq!(store.peers.len(), 1);
    let p = &store.peers[0];
    assert_eq!(p.label, "old-laptop");
    // Defaulted serde-default fields.
    assert_eq!(p.permissions, prdt_protocol::PermissionSet::default()); // = deny_all
}

#[test]
fn known_peer_round_trip_with_permissions() {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now();
    let p = KnownPeer {
        pubkey_b64: "xyz".into(),
        label: "work".into(),
        permissions: prdt_protocol::PermissionSet::all(),
        first_seen_at: UNIX_EPOCH,
        last_seen_at: now,
    };
    let s = toml::to_string(&KnownPeers { peers: vec![p.clone()] }).unwrap();
    let back: KnownPeers = toml::from_str(&s).unwrap();
    assert_eq!(back.peers[0].pubkey_b64, p.pubkey_b64);
    assert_eq!(back.peers[0].permissions, p.permissions);
}
```

Run: `cargo test -p prdt-crypto known_peers 2>&1 | tail -20`
Expected: compile fail — `permissions`, `first_seen_at`, `last_seen_at` don't exist on `KnownPeer`.

- [ ] **Step 5: Extend KnownPeer schema**

Modify the `KnownPeer` struct in `crates/crypto/src/known_peers.rs`:

```rust
use prdt_protocol::PermissionSet;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnownPeer {
    pub pubkey_b64: String,
    pub label: String,
    #[serde(default)]
    pub permissions: PermissionSet,
    #[serde(default = "epoch")]
    pub first_seen_at: SystemTime,
    #[serde(default = "epoch")]
    pub last_seen_at: SystemTime,
}

fn epoch() -> SystemTime { UNIX_EPOCH }
```

Add `prdt-protocol = { path = "../protocol" }` to `crates/crypto/Cargo.toml` if not already a dep.

Find all existing `KnownPeer { pubkey_b64, label }` literals (probably only test code in `crates/crypto/src/known_peers.rs` itself) and extend with the new fields. Use `..Default::default()` if you `#[derive(Default)]` the struct, or write them out explicitly.

Run: `cargo test -p prdt-crypto known_peers 2>&1 | tail -20`
Expected: green.

- [ ] **Step 6: Run workspace tests**

Run: `cargo test --workspace --lib 2>&1 | tail -20`
Expected: green. The previous step's KnownPeer extension might break host code that constructs KnownPeer literals; fix those with the new fields.

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -10`
Run: `cargo fmt --all`

- [ ] **Step 7: Commit**

```bash
git add crates/host/Cargo.toml crates/host/src/auth_config.rs crates/host/src/lib.rs
git add crates/crypto/src/known_peers.rs crates/crypto/Cargo.toml
git commit -m "feat(p6/host): HostAuthConfig + KnownPeer schema extension (permissions + timestamps)"
```

---

## Task 3: AuthValidator + integration tests

**Files:**
- Create: `crates/host/src/auth.rs`
- Modify: `crates/host/src/lib.rs` (`pub mod auth;` + wire into Hello handler)
- Create: `crates/host/tests/auth_integration.rs`

This task is the heart of P6. It implements the §6 state machine. The implementation pattern: a single `AuthValidator::validate(&self, hello, peer_pubkey) -> AuthVerdict` async function plus helper structs. Mutate `known_peers` only on Granted-with-remember.

- [ ] **Step 1: Write failing test scaffolding**

Create `crates/host/tests/auth_integration.rs`. Start with the test that constructs the validator and verifies the simplest happy path.

```rust
//! P6 auth integration tests: drive AuthValidator through realistic Hello
//! payloads and assert the AuthVerdict that comes back.

use prdt_host::auth::{AuthValidator, AuthVerdict};
use prdt_host::auth_config::{AuthMode, HostAuthConfig};
use prdt_protocol::{AuthMethod, ControlMessage, HelloRejectCode, MonitorRect, PermissionSet, Codec};
use prdt_crypto::known_peers::{KnownPeer, KnownPeers};
use std::sync::Arc;
use tokio::sync::RwLock;

fn make_hello(auth_method: AuthMethod, payload: &[u8], protocol_version: u8) -> ControlMessage {
    ControlMessage::Hello {
        protocol_version,
        req_width: 1920,
        req_height: 1080,
        req_fps: 60,
        codec: Codec::H265,
        auth_method,
        auth_payload: payload.to_vec(),
    }
}

#[tokio::test]
async fn pin_auth_success() {
    let mut cfg = HostAuthConfig::default();
    cfg.mode = AuthMode::Pin;
    cfg.pin_hash = Some(HostAuthConfig::hash_pin("hunter2").unwrap());
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);

    let hello = make_hello(AuthMethod::Pin, b"hunter2", 3);
    let verdict = v.validate(&hello, "peerA").await;

    match verdict {
        AuthVerdict::Granted { permissions, remember } => {
            assert_eq!(permissions, PermissionSet::all());
            assert!(!remember); // default, no remember bit on first auth attempt
        }
        other => panic!("expected Granted, got {other:?}"),
    }
}

#[tokio::test]
async fn pin_auth_wrong_then_correct() {
    let mut cfg = HostAuthConfig::default();
    cfg.mode = AuthMode::Pin;
    cfg.pin_hash = Some(HostAuthConfig::hash_pin("hunter2").unwrap());
    cfg.max_pin_attempts = 5;
    cfg.pin_lockout_seconds = 300;
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);

    for _ in 0..2 {
        let hello = make_hello(AuthMethod::Pin, b"wrong", 3);
        let verdict = v.validate(&hello, "peerA").await;
        assert!(matches!(verdict, AuthVerdict::Rejected { code: HelloRejectCode::AuthFailed, .. }));
    }
    // correct PIN succeeds AND resets the counter
    let hello = make_hello(AuthMethod::Pin, b"hunter2", 3);
    assert!(matches!(
        v.validate(&hello, "peerA").await,
        AuthVerdict::Granted { .. }
    ));
}

#[tokio::test]
async fn pin_auth_lockout_after_max_attempts() {
    let mut cfg = HostAuthConfig::default();
    cfg.mode = AuthMode::Pin;
    cfg.pin_hash = Some(HostAuthConfig::hash_pin("hunter2").unwrap());
    cfg.max_pin_attempts = 3;
    cfg.pin_lockout_seconds = 300;
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);

    for _ in 0..3 {
        let hello = make_hello(AuthMethod::Pin, b"wrong", 3);
        let _ = v.validate(&hello, "peerA").await;
    }
    let hello = make_hello(AuthMethod::Pin, b"hunter2", 3); // correct, but locked
    let verdict = v.validate(&hello, "peerA").await;
    assert!(matches!(verdict, AuthVerdict::Rejected { code: HelloRejectCode::AuthLockout, .. }));
}

#[tokio::test]
async fn ephemeral_auth_success() {
    let mut cfg = HostAuthConfig::default();
    cfg.mode = AuthMode::Ephemeral;
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);
    let eph = v.rotate_ephemeral_for_test();

    let hello = make_hello(AuthMethod::Ephemeral, eph.as_bytes(), 3);
    let verdict = v.validate(&hello, "peerA").await;
    assert!(matches!(verdict, AuthVerdict::Granted { .. }));
}

#[tokio::test]
async fn ephemeral_auth_wrong_rejected() {
    let mut cfg = HostAuthConfig::default();
    cfg.mode = AuthMode::Ephemeral;
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);
    let _real = v.rotate_ephemeral_for_test();

    let hello = make_hello(AuthMethod::Ephemeral, b"WRONG123", 3);
    let verdict = v.validate(&hello, "peerA").await;
    assert!(matches!(verdict, AuthVerdict::Rejected { code: HelloRejectCode::AuthFailed, .. }));
}

#[tokio::test]
async fn ephemeral_expired_rejected() {
    let mut cfg = HostAuthConfig::default();
    cfg.mode = AuthMode::Ephemeral;
    cfg.ephemeral_lifetime_seconds = 1; // short, so the test waits briefly
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);
    let eph = v.rotate_ephemeral_for_test();
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    let hello = make_hello(AuthMethod::Ephemeral, eph.as_bytes(), 3);
    let verdict = v.validate(&hello, "peerA").await;
    assert!(matches!(verdict, AuthVerdict::Rejected { code: HelloRejectCode::AuthFailed, .. }));
}

#[tokio::test]
async fn pin_required_when_viewer_sends_tofu_to_pin_host() {
    let mut cfg = HostAuthConfig::default();
    cfg.mode = AuthMode::Pin;
    cfg.pin_hash = Some(HostAuthConfig::hash_pin("hunter2").unwrap());
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);

    let hello = make_hello(AuthMethod::Tofu, b"", 3);
    let verdict = v.validate(&hello, "peerA").await;
    assert!(matches!(verdict, AuthVerdict::Rejected { code: HelloRejectCode::PinRequired, .. }));
}

#[tokio::test]
async fn ephemeral_required_when_viewer_sends_tofu_to_ephemeral_host() {
    let mut cfg = HostAuthConfig::default();
    cfg.mode = AuthMode::Ephemeral;
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);
    let _ = v.rotate_ephemeral_for_test();

    let hello = make_hello(AuthMethod::Tofu, b"", 3);
    let verdict = v.validate(&hello, "peerA").await;
    assert!(matches!(verdict, AuthVerdict::Rejected { code: HelloRejectCode::EphemeralRequired, .. }));
}

#[tokio::test]
async fn protocol_version_mismatch_rejected() {
    let cfg = HostAuthConfig::default();
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);

    let hello = make_hello(AuthMethod::Tofu, b"", 2); // pre-P6
    let verdict = v.validate(&hello, "peerA").await;
    assert!(matches!(verdict, AuthVerdict::Rejected { code: HelloRejectCode::ProtocolVersionMismatch, .. }));
}

#[tokio::test]
async fn tofu_known_peer_grants_without_prompt() {
    let cfg = HostAuthConfig::default(); // mode = Tofu
    let custom_perms = PermissionSet { input: true, clipboard: false, file_transfer: false, audio: true };
    let peer = KnownPeer {
        pubkey_b64: "peerA".into(),
        label: "work".into(),
        permissions: custom_perms,
        first_seen_at: std::time::UNIX_EPOCH,
        last_seen_at: std::time::SystemTime::now(),
    };
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![peer] }));
    let v = AuthValidator::new(cfg, known);

    let hello = make_hello(AuthMethod::Tofu, b"", 3);
    let verdict = v.validate(&hello, "peerA").await;
    match verdict {
        AuthVerdict::Granted { permissions, .. } => assert_eq!(permissions, custom_perms),
        other => panic!("expected Granted, got {other:?}"),
    }
}

#[tokio::test]
async fn tofu_unknown_peer_needs_consent() {
    let cfg = HostAuthConfig::default();
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);
    let hello = make_hello(AuthMethod::Tofu, b"", 3);
    let verdict = v.validate(&hello, "peerA").await;
    assert!(matches!(verdict, AuthVerdict::NeedsConsent { .. }));
}

#[tokio::test]
async fn pin_known_peer_still_requires_pin() {
    let mut cfg = HostAuthConfig::default();
    cfg.mode = AuthMode::Pin;
    cfg.pin_hash = Some(HostAuthConfig::hash_pin("hunter2").unwrap());
    let peer = KnownPeer {
        pubkey_b64: "peerA".into(),
        label: "work".into(),
        permissions: PermissionSet { input: true, clipboard: false, file_transfer: false, audio: true },
        first_seen_at: std::time::UNIX_EPOCH,
        last_seen_at: std::time::SystemTime::now(),
    };
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![peer] }));
    let v = AuthValidator::new(cfg, known);

    // Empty PIN doesn't auto-pass even for known peer.
    let hello = make_hello(AuthMethod::Pin, b"wrong", 3);
    let verdict = v.validate(&hello, "peerA").await;
    assert!(matches!(verdict, AuthVerdict::Rejected { code: HelloRejectCode::AuthFailed, .. }));

    // With correct PIN, granted with the *known* permissions (not defaults).
    let hello = make_hello(AuthMethod::Pin, b"hunter2", 3);
    let verdict = v.validate(&hello, "peerA").await;
    match verdict {
        AuthVerdict::Granted { permissions, .. } => {
            assert_eq!(
                permissions,
                PermissionSet { input: true, clipboard: false, file_transfer: false, audio: true }
            );
        }
        other => panic!("expected Granted, got {other:?}"),
    }
}

#[tokio::test]
async fn auth_payload_oversize_rejected() {
    let mut cfg = HostAuthConfig::default();
    cfg.mode = AuthMode::Pin;
    cfg.pin_hash = Some(HostAuthConfig::hash_pin("hunter2").unwrap());
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);

    let huge = vec![b'A'; 65]; // > 64-byte cap
    let hello = make_hello(AuthMethod::Pin, &huge, 3);
    let verdict = v.validate(&hello, "peerA").await;
    assert!(matches!(verdict, AuthVerdict::Rejected { code: HelloRejectCode::Unspecified, .. }));
}
```

Run: `cargo test -p prdt-host --test auth_integration 2>&1 | tail -20`
Expected: compile errors — `auth` module doesn't exist.

- [ ] **Step 2: Implement `AuthValidator`**

Create `crates/host/src/auth.rs`:

```rust
//! P6 AuthValidator — single source of truth for the Hello-time auth decision.
//!
//! Drives the §6 state machine: protocol_version → codec → dispatch on
//! `config.mode` → mode-specific verification (TOFU consent prompt, PIN bcrypt,
//! Ephemeral constant-time compare) → AuthVerdict.
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

#[derive(Debug)]
pub enum AuthVerdict {
    /// Authentication succeeded. `permissions` is the set the session
    /// runs under (immutable from this point on). `remember` is true when
    /// the host should persist this peer; false for first-time TOFU connects
    /// (the GUI prompt sets it) and for ephemeral connects.
    Granted { permissions: PermissionSet, remember: bool },
    /// Authentication failed. `code` tells the viewer what UI to show next
    /// (PIN dialog, ephemeral dialog, version-mismatch error, etc.).
    Rejected { code: HelloRejectCode, reason: String },
    /// TOFU mode + unknown peer; the host control loop must surface a GUI
    /// consent prompt and call `AuthValidator::finalize_consent(...)`
    /// when the user clicks Allow/Deny.
    NeedsConsent { peer_pubkey_b64: String, default_permissions: PermissionSet },
}

#[derive(Debug, Clone)]
struct PinAttemptState {
    failed_count: u8,
    locked_until: Option<Instant>,
}

#[derive(Debug, Clone)]
struct EphemeralState {
    value: String,
    created_at: Instant,
}

pub struct AuthValidator {
    config: HostAuthConfig,
    known_peers: Arc<RwLock<KnownPeers>>,
    ephemeral: Arc<Mutex<Option<EphemeralState>>>,
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

    /// Test-only sync wrapper (sometimes the test harness can't `.await` in scope).
    #[doc(hidden)]
    pub fn rotate_ephemeral_for_test(&self) -> String {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.rotate_ephemeral())
        })
    }

    pub async fn validate(&self, msg: &ControlMessage, peer_pubkey_b64: &str) -> AuthVerdict {
        let ControlMessage::Hello {
            protocol_version,
            codec,
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

        // §6 state machine, top to bottom.

        if *protocol_version != PROTOCOL_VERSION_REQUIRED {
            return AuthVerdict::Rejected {
                code: HelloRejectCode::ProtocolVersionMismatch,
                reason: format!(
                    "host requires protocol_version={PROTOCOL_VERSION_REQUIRED} (P6+); got {protocol_version}"
                ),
            };
        }

        // Codec gate — for now the host advertises both H264 and H265, mirroring
        // pre-P6 behaviour. If a future codec restriction lands, plumb it here.
        let _ = codec; // explicit no-op so reviewers see the gate exists.

        if auth_payload.len() > AUTH_PAYLOAD_MAX_BYTES {
            return AuthVerdict::Rejected {
                code: HelloRejectCode::Unspecified,
                reason: "auth_payload too long".into(),
            };
        }

        // Dispatch on config.mode (authoritative).
        match self.config.mode {
            AuthMode::Tofu => self.validate_tofu(*auth_method, peer_pubkey_b64).await,
            AuthMode::Pin => {
                self.validate_pin(*auth_method, auth_payload, peer_pubkey_b64).await
            }
            AuthMode::Ephemeral => {
                self.validate_ephemeral(*auth_method, auth_payload, peer_pubkey_b64).await
            }
        }
    }

    async fn validate_tofu(&self, viewer_method: AuthMethod, peer: &str) -> AuthVerdict {
        if viewer_method != AuthMethod::Tofu {
            return AuthVerdict::Rejected {
                code: HelloRejectCode::ConsentDenied,
                reason: "host is in TOFU mode; viewer should set auth_method=Tofu".into(),
            };
        }
        let known = self.known_peers.read().await;
        if let Some(p) = known.peers.iter().find(|p| p.pubkey_b64 == peer) {
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
                reason: "host is in PIN mode".into(),
            };
        }

        // lockout check
        {
            let mut attempts = self.pin_attempts.lock().await;
            if let Some(state) = attempts.get(peer) {
                if let Some(locked_until) = state.locked_until {
                    if Instant::now() < locked_until {
                        return AuthVerdict::Rejected {
                            code: HelloRejectCode::AuthLockout,
                            reason: "too many wrong PINs; locked out".into(),
                        };
                    }
                }
            }
            // Prune entries past lockout window.
            attempts.retain(|_, s| {
                s.failed_count > 0
                    || s.locked_until
                        .map(|t| Instant::now() < t)
                        .unwrap_or(false)
            });
        }

        let plain = match std::str::from_utf8(payload) {
            Ok(s) => s,
            Err(_) => {
                return AuthVerdict::Rejected {
                    code: HelloRejectCode::AuthFailed,
                    reason: "PIN must be valid UTF-8".into(),
                };
            }
        };

        let ok = self.config.verify_pin(plain);
        if !ok {
            let mut attempts = self.pin_attempts.lock().await;
            let entry = attempts
                .entry(peer.to_string())
                .or_insert(PinAttemptState { failed_count: 0, locked_until: None });
            entry.failed_count += 1;
            if entry.failed_count >= self.config.max_pin_attempts {
                entry.locked_until = Some(
                    Instant::now() + Duration::from_secs(self.config.pin_lockout_seconds as u64),
                );
                warn!(peer = %peer, count = entry.failed_count, "PIN lockout fired");
            }
            return AuthVerdict::Rejected {
                code: HelloRejectCode::AuthFailed,
                reason: format!(
                    "wrong PIN ({}/{})",
                    entry.failed_count, self.config.max_pin_attempts
                ),
            };
        }

        // success — reset counter, load permissions
        self.pin_attempts.lock().await.remove(peer);
        let permissions = {
            let known = self.known_peers.read().await;
            known
                .peers
                .iter()
                .find(|p| p.pubkey_b64 == peer)
                .map(|p| p.permissions)
                .unwrap_or(self.config.default_permissions)
        };
        AuthVerdict::Granted { permissions, remember: false }
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
                reason: "host is in Ephemeral mode".into(),
            };
        }

        let guard = self.ephemeral.lock().await;
        let Some(eph) = guard.as_ref() else {
            return AuthVerdict::Rejected {
                code: HelloRejectCode::AuthFailed,
                reason: "no active ephemeral; host operator must generate one".into(),
            };
        };

        // expiry check
        let age = Instant::now() - eph.created_at;
        if age > Duration::from_secs(self.config.ephemeral_lifetime_seconds as u64) {
            return AuthVerdict::Rejected {
                code: HelloRejectCode::AuthFailed,
                reason: "ephemeral expired".into(),
            };
        }

        // constant-time compare; pad shorter to longer length with zero bytes
        // so equal-length is the precondition for `ct_eq` (length difference is
        // itself a fail per spec §14).
        let expected = eph.value.as_bytes();
        if payload.len() != expected.len() {
            return AuthVerdict::Rejected {
                code: HelloRejectCode::AuthFailed,
                reason: "ephemeral length mismatch".into(),
            };
        }
        let ok: bool = expected.ct_eq(payload).into();
        if !ok {
            return AuthVerdict::Rejected {
                code: HelloRejectCode::AuthFailed,
                reason: "ephemeral mismatch".into(),
            };
        }

        // Consume the ephemeral — single-use semantics. The host operator
        // must rotate before the next connection.
        drop(guard);
        *self.ephemeral.lock().await = None;

        let permissions = {
            let known = self.known_peers.read().await;
            known
                .peers
                .iter()
                .find(|p| p.pubkey_b64 == peer)
                .map(|p| p.permissions)
                .unwrap_or(self.config.default_permissions)
        };
        debug!(peer = %peer, "ephemeral consumed; granted");
        AuthVerdict::Granted { permissions, remember: false }
    }
}
```

Add `pub mod auth;` to `crates/host/src/lib.rs`.

- [ ] **Step 3: Run integration tests**

Run: `cargo test -p prdt-host --test auth_integration 2>&1 | tail -30`
Expected: all green except possibly `ephemeral_expired_rejected` (uses real-time sleep — may be flaky on slow CI; if so, use `tokio::time::pause()` + advance manually).

If `ephemeral_expired_rejected` is flaky, refactor it to:

```rust
#[tokio::test(start_paused = true)]
async fn ephemeral_expired_rejected() {
    let mut cfg = HostAuthConfig::default();
    cfg.mode = AuthMode::Ephemeral;
    cfg.ephemeral_lifetime_seconds = 1;
    let known = Arc::new(RwLock::new(KnownPeers { peers: vec![] }));
    let v = AuthValidator::new(cfg, known);
    let eph = v.rotate_ephemeral().await;
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    // …
}
```

But `Instant::now()` is *not* under `tokio::time::pause` control. The pragmatic
fix: use a `Clock` trait abstraction or just `tokio::time::sleep` with a 1100ms
delay and `cfg.ephemeral_lifetime_seconds = 1`. The test takes 1.1s — acceptable.

- [ ] **Step 4: Wire validator into host Hello handler**

In `crates/host/src/lib.rs`, find the existing Hello handling. Replace the
ConsentRequest dispatch with the new flow:

```rust
// In the handshake/Hello arm:
let verdict = auth_validator.validate(&hello_msg, peer_pubkey_b64).await;
let granted_permissions = match verdict {
    AuthVerdict::Granted { permissions, remember } => {
        if remember {
            // (existing) update known_peers with last_seen_at
            update_last_seen(&known_peers, peer_pubkey_b64).await;
        }
        permissions
    }
    AuthVerdict::Rejected { code, reason } => {
        let reject = ControlMessage::HelloReject { reason, code };
        transport.send_control(reject).await.ok();
        return Err(/* clean disconnect */);
    }
    AuthVerdict::NeedsConsent { peer_pubkey_b64, default_permissions } => {
        let resp = consent_prompt(&peer_pubkey_b64, default_permissions, consent_timeout).await;
        match resp {
            ConsentDecision::Accepted { permissions, remember, label } => {
                if remember {
                    insert_or_update_known_peer(
                        &known_peers, &peer_pubkey_b64, label, permissions
                    ).await;
                }
                permissions
            }
            ConsentDecision::Rejected => {
                let reject = ControlMessage::HelloReject {
                    reason: "host operator denied consent".into(),
                    code: HelloRejectCode::ConsentDenied,
                };
                transport.send_control(reject).await.ok();
                return Err(/* clean disconnect */);
            }
        }
    }
};

let ack = ControlMessage::HelloAck {
    // ...existing fields...
    granted_permissions,
};
transport.send_control(ack).await?;

// Store permissions on the session state — see Task 4.
session_state.permissions = granted_permissions;
```

- [ ] **Step 5: Run workspace tests**

Run: `cargo test --workspace 2>&1 | tail -30`
Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -10`
Run: `cargo fmt --all`

- [ ] **Step 6: Commit**

```bash
git add crates/host/src/auth.rs crates/host/src/lib.rs crates/host/tests/auth_integration.rs
git commit -m "feat(p6/host): AuthValidator state machine + integration tests"
```

---

## Task 4: Per-channel permission enforcement

**Files:**
- Modify: `crates/host/src/lib.rs` — `SessionState.permissions`, control-loop arms, Input/Audio gate
- Modify: `crates/host/tests/auth_integration.rs` — `permission_deny_*` tests

- [ ] **Step 1: Write failing tests**

Append to `crates/host/tests/auth_integration.rs`:

```rust
//! These tests drive a real in-process host (with mock transport) through
//! Hello → HelloAck → control messages and assert that denied channels are
//! silently dropped (no error, no state change visible to the peer).
//!
//! Build on InProcTransport (already used elsewhere in the workspace).

#[tokio::test]
async fn permission_deny_clipboard() {
    let perms = PermissionSet { input: true, clipboard: false, file_transfer: true, audio: true };
    let dropped = run_host_with_perms_and_send(perms, ControlMessage::ClipboardText {
        text: "secret".into(),
    }).await;
    assert!(dropped, "ClipboardText should be silently dropped when clipboard=false");
}

#[tokio::test]
async fn permission_deny_file_transfer() {
    let perms = PermissionSet { input: true, clipboard: true, file_transfer: false, audio: true };
    let dropped = run_host_with_perms_and_send(perms, ControlMessage::FileTransferBegin {
        transfer_id: 1,
        filename: "secret.txt".into(),
        total_bytes: 10,
    }).await;
    assert!(dropped);
}

#[tokio::test]
async fn permission_deny_input_does_not_spawn_task() {
    // Drive a host with input=false and assert no input task is observable
    // (e.g. via a counter the input task increments on first poll).
    let perms = PermissionSet { input: false, clipboard: true, file_transfer: true, audio: true };
    let spawned = run_host_with_perms_and_check_input_spawn(perms).await;
    assert!(!spawned, "input task should not spawn when input=false");
}

#[tokio::test]
async fn permission_deny_audio_does_not_spawn_task() {
    let perms = PermissionSet { input: true, clipboard: true, file_transfer: true, audio: false };
    let spawned = run_host_with_perms_and_check_audio_spawn(perms).await;
    assert!(!spawned, "audio task should not spawn when audio=false");
}
```

These tests need helpers `run_host_with_perms_and_send` / `run_host_with_perms_and_check_input_spawn` / `..._audio_spawn`. The implementer writes them as thin wrappers over the existing in-process host harness (find any pre-existing harness in `crates/host/tests/` — there's likely a `request_idr_handler_smoke` integration test or similar that already spawns a real host).

If no harness exists yet, this task grows by ~150 LoC of test scaffolding. That's fine — those helpers become reusable.

Run: `cargo test -p prdt-host --test auth_integration permission_deny 2>&1 | tail -20`
Expected: compile errors / red until step 2.

- [ ] **Step 2: Add `permissions` to SessionState + gate control loop**

In `crates/host/src/lib.rs`, find the per-session state struct (search for
`session_id: u64,` if the struct doesn't have an obvious name). Add:

```rust
permissions: PermissionSet,
```

Find the control loop (search for `match msg` after `recv_control`). Wrap
each ControlMessage arm with the channel gate:

```rust
fn channel_allowed(perms: &PermissionSet, msg: &ControlMessage) -> bool {
    match msg {
        ControlMessage::ClipboardText { .. } => perms.clipboard,
        ControlMessage::FileTransferBegin { .. }
        | ControlMessage::FileChunk { .. }
        | ControlMessage::FileTransferEnd { .. } => perms.file_transfer,
        _ => true,
    }
}

// inside the recv loop:
Ok(ReceivedMessage::Control(msg)) => {
    if !channel_allowed(&session.permissions, &msg) {
        tracing::debug!(?msg, "channel denied; dropping");
        continue;
    }
    match msg { /* existing arms unchanged */ }
}
```

Gate the input task spawn (find `spawn(input_task` or similar):

```rust
if session.permissions.input {
    tokio::spawn(input_task(/* ... */));
} else {
    tracing::info!("input channel denied for this session");
}
```

Gate the audio task spawn the same way.

- [ ] **Step 3: Run tests**

Run: `cargo test -p prdt-host --test auth_integration 2>&1 | tail -20`
Expected: green for all `permission_deny_*` tests.

- [ ] **Step 4: Workspace checks**

Run: `cargo test --workspace 2>&1 | tail -20`
Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -10`
Run: `cargo fmt --all`

- [ ] **Step 5: Commit**

```bash
git add crates/host/src/lib.rs crates/host/tests/auth_integration.rs
git commit -m "feat(p6/host): per-channel permission enforcement (input/clipboard/file-transfer/audio)"
```

---

## Task 5: Viewer Hello/HelloReject retry loop + CLI flags + stats payload

**Files:**
- Modify: `crates/viewer/src/lib.rs` (Hello retry loop + clap args + StatsPayload populate)
- Modify: `crates/viewer/src/overlay_ipc.rs` (StatsPayload + granted_permissions)

- [ ] **Step 1: Write failing test for viewer retry behaviour**

Add a unit test inside `crates/viewer/src/lib.rs` (or a new
`crates/viewer/tests/auth_retry.rs`). Since the viewer's connect logic is
hard to unit-test cleanly, the test drives a `try_hello_attempt` helper
function that takes (auth_method, payload, prompt_provider) and walks the
state machine. Extract that helper as a pure async function.

```rust
#[tokio::test]
async fn viewer_retries_on_pin_required() {
    // Mock transport that:
    // - on Hello#1 (Tofu, empty): respond HelloReject{PinRequired}
    // - on Hello#2 (Pin, "hunter2"): respond HelloAck
    // Mock prompt_pin: returns Some("hunter2")
    let result = run_viewer_auth_loop(/* mock_transport */, /* mock_pin = Some("hunter2") */).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn viewer_fails_fast_with_no_auth_prompt_flag() {
    // mock_transport responds HelloReject{PinRequired}.
    // viewer was started with --no-auth-prompt; prompt_pin = None.
    let result = run_viewer_auth_loop(/* */, /* mock_pin = None, prompt_allowed=false */).await;
    assert!(matches!(result, Err(AuthLoopError::PromptDisabled)));
}

#[tokio::test]
async fn viewer_shows_error_on_protocol_version_mismatch() {
    // mock_transport responds HelloReject{ProtocolVersionMismatch}
    let result = run_viewer_auth_loop(/* */, /* */).await;
    assert!(matches!(result, Err(AuthLoopError::HostRequiresUpgrade)));
}
```

- [ ] **Step 2: Extract the auth loop**

In `crates/viewer/src/lib.rs`, create a function:

```rust
pub(crate) async fn run_viewer_auth_loop<T, P>(
    transport: &mut T,
    hello_template: HelloTemplate,
    prompt: &P,
) -> Result<HelloAck, AuthLoopError>
where
    T: HelloTransport,    // trait abstraction over send_control / recv_control
    P: AuthPromptProvider, // get_pin / get_ephemeral hooks
{
    let mut attempt = HelloAttempt::FirstTry;
    loop {
        let hello = hello_template.build(&attempt);
        transport.send_hello(hello).await?;
        match transport.recv_response().await? {
            ControlMessage::HelloAck { granted_permissions, .. } => {
                return Ok(/* the HelloAck */);
            }
            ControlMessage::HelloReject { code, reason } => match code {
                HelloRejectCode::PinRequired => {
                    let pin = prompt.get_pin().await?;
                    attempt = HelloAttempt::WithPin(pin);
                }
                HelloRejectCode::EphemeralRequired => {
                    let eph = prompt.get_ephemeral().await?;
                    attempt = HelloAttempt::WithEphemeral(eph);
                }
                HelloRejectCode::AuthFailed => {
                    prompt.notify_wrong().await;
                    // re-prompt; keep prev attempt kind
                    match attempt {
                        HelloAttempt::FirstTry | HelloAttempt::WithPin(_) => {
                            let pin = prompt.get_pin().await?;
                            attempt = HelloAttempt::WithPin(pin);
                        }
                        HelloAttempt::WithEphemeral(_) => {
                            let eph = prompt.get_ephemeral().await?;
                            attempt = HelloAttempt::WithEphemeral(eph);
                        }
                    }
                }
                HelloRejectCode::AuthLockout => return Err(AuthLoopError::Locked(reason)),
                HelloRejectCode::ProtocolVersionMismatch => return Err(AuthLoopError::HostRequiresUpgrade),
                _ => return Err(AuthLoopError::Other(reason)),
            },
            _ => return Err(AuthLoopError::UnexpectedPreAck),
        }
    }
}
```

Plus the support types (`HelloAttempt`, `AuthLoopError`, `HelloTemplate`,
`AuthPromptProvider` trait + a `CliPromptProvider` impl that reads
`--pin <PIN>`/`--ephemeral <EPH>` once, returns Error if `--no-auth-prompt`
and the cached values are exhausted).

Wire the existing viewer connect logic to call this function.

- [ ] **Step 3: Add CLI flags**

In the viewer's clap setup:

```rust
#[arg(long)] pub pin: Option<String>,
#[arg(long)] pub ephemeral: Option<String>,
#[arg(long)] pub no_auth_prompt: bool,
```

- [ ] **Step 4: Extend StatsPayload**

In `crates/viewer/src/overlay_ipc.rs`:

```rust
pub struct StatsPayload {
    // ... existing P5A fields ...
    #[serde(default)]
    pub granted_permissions: Option<PermissionSet>,
}
```

In `build_stats_payload`, populate it from the current session's
`granted_permissions` (held in viewer state after a successful HelloAck).

- [ ] **Step 5: Run tests**

Run: `cargo test -p prdt-viewer 2>&1 | tail -20`
Run: `cargo clippy --workspace --all-targets -- -D warnings`
Run: `cargo fmt --all`

- [ ] **Step 6: Commit**

```bash
git add crates/viewer/src/lib.rs crates/viewer/src/overlay_ipc.rs
git commit -m "feat(p6/viewer): Hello/HelloReject auth loop + CLI flags + stats payload extension"
```

---

## Task 6: signaling-proto ProbeHosts + server handler + viewer poll

**Files:**
- Modify: `crates/signaling-proto/src/lib.rs`
- Modify: `crates/signaling-server/src/lib.rs` (or handler module)
- Modify: `crates/signaling-client/src/lib.rs`
- Create: `crates/gui-viewer/src/online_probe.rs`

- [ ] **Step 1: Write failing test for ProbeHosts wire**

In `crates/signaling-proto/src/lib.rs` test module:

```rust
#[test]
fn probe_hosts_round_trip() {
    let msg = ClientMessage::ProbeHosts {
        host_ids: vec!["111-111-111".into(), "222-222-222".into()],
    };
    let bytes = bincode::serialize(&msg).unwrap();
    let back: ClientMessage = bincode::deserialize(&bytes).unwrap();
    assert_eq!(msg, back);
}

#[test]
fn probe_result_round_trip() {
    let msg = ServerMessage::ProbeResult {
        online: vec!["111-111-111".into()],
    };
    let bytes = bincode::serialize(&msg).unwrap();
    let back: ServerMessage = bincode::deserialize(&bytes).unwrap();
    assert_eq!(msg, back);
}
```

Run: `cargo test -p prdt-signaling-proto 2>&1 | tail -10`
Expected: compile fail (variants don't exist).

- [ ] **Step 2: Add the variants**

```rust
pub enum ClientMessage {
    // ... existing ...
    ProbeHosts { host_ids: Vec<String> },
}

pub enum ServerMessage {
    // ... existing ...
    ProbeResult { online: Vec<String> },
}
```

Run the tests. Green.

- [ ] **Step 3: Write failing server-handler test**

Add to `crates/signaling-server/tests/probe_test.rs`:

```rust
#[tokio::test]
async fn probe_hosts_returns_intersection() {
    let server = TestServer::spawn().await;
    let _host = TestHost::register(&server, "111-111-111").await;
    let _host2 = TestHost::register(&server, "222-222-222").await;
    // 333 is not registered

    let viewer = TestClient::connect(&server).await;
    let online = viewer
        .probe_hosts(vec![
            "111-111-111".into(),
            "222-222-222".into(),
            "333-333-333".into(),
        ])
        .await
        .unwrap();
    assert_eq!(online.len(), 2);
    assert!(online.contains(&"111-111-111".into()));
    assert!(online.contains(&"222-222-222".into()));
}

#[tokio::test]
async fn probe_hosts_empty_list_ok() {
    let server = TestServer::spawn().await;
    let viewer = TestClient::connect(&server).await;
    let online = viewer.probe_hosts(vec![]).await.unwrap();
    assert!(online.is_empty());
}
```

(If `TestServer`/`TestHost`/`TestClient` don't exist, look at existing
signaling-server integration tests for an equivalent harness — `phase2-w5`
tests already register hosts so the harness exists.)

- [ ] **Step 4: Implement server handler**

In the signaling-server's WS dispatch (search for the existing
`ClientMessage::Connect` arm):

```rust
ClientMessage::ProbeHosts { host_ids } => {
    let online: Vec<String> = host_ids
        .into_iter()
        .filter(|id| online_hosts.contains_key(id))
        .collect();
    socket.send(ServerMessage::ProbeResult { online }).await?;
}
```

`online_hosts` is the in-memory map the server already maintains (search
`hosts.insert(` or similar).

- [ ] **Step 5: Implement client method**

In `crates/signaling-client/src/lib.rs`:

```rust
impl SignalingClient {
    pub async fn probe_hosts(&self, host_ids: Vec<String>) -> Result<Vec<String>> {
        self.send(ClientMessage::ProbeHosts { host_ids }).await?;
        let reply = self.recv().await?;
        match reply {
            ServerMessage::ProbeResult { online } => Ok(online),
            other => Err(eyre!("expected ProbeResult, got {other:?}")),
        }
    }
}
```

- [ ] **Step 6: Implement viewer polling**

Create `crates/gui-viewer/src/online_probe.rs`:

```rust
//! 30s background task that polls signaling for which saved hosts are online.
//! Runs only while the hosts_list view is open; cancelled on session start.

use prdt_signaling_client::SignalingClient;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{interval, Duration};
use tokio_util::sync::CancellationToken;

pub struct OnlineProbe {
    client: Arc<Mutex<SignalingClient>>,
    cancel: CancellationToken,
}

impl OnlineProbe {
    pub fn spawn(
        client: Arc<Mutex<SignalingClient>>,
        host_ids: Arc<Mutex<Vec<String>>>,
        result_sink: Arc<Mutex<std::collections::HashMap<String, bool>>>,
    ) -> CancellationToken {
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(30));
            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    _ = ticker.tick() => {
                        let ids = host_ids.lock().await.clone();
                        if ids.is_empty() { continue; }
                        let probe = client.lock().await.probe_hosts(ids.clone()).await;
                        if let Ok(online) = probe {
                            let mut sink = result_sink.lock().await;
                            for id in &ids {
                                sink.insert(id.clone(), online.contains(id));
                            }
                        }
                    }
                }
            }
        });
        cancel
    }
}
```

(The actual gui-viewer plumbing — wiring this to the egui repaint loop —
is small and done in T8.)

- [ ] **Step 7: Workspace checks + commit**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
git add crates/signaling-proto crates/signaling-server crates/signaling-client crates/gui-viewer/src/online_probe.rs
git commit -m "feat(p6/signaling): ProbeHosts/ProbeResult + viewer 30s online poll"
```

---

## Task 7: gui-host onboarding wizard + Settings auth + permission prompt

**Files:**
- Create: `crates/gui-host/src/onboarding.rs`
- Create: `crates/gui-host/src/auth_settings.rs`
- Modify: `crates/gui-host/src/lib.rs` (or `app.rs`) — wizard launch + Settings mount + prompt modal
- Modify: `crates/gui-common/src/config.rs` — add `gui.onboarded: bool`

This task is GUI-heavy. The implementer writes the egui code by following
the spec §9 / §10 step-by-step. Headless tests cover the *submit handlers*
(write `host-auth.toml`, update `gui.onboarded`); the egui rendering is
tested by manual smoke (T9) only.

- [ ] **Step 1: Add `gui.onboarded` to Config**

```rust
// crates/gui-common/src/config.rs
pub struct GuiConfig {
    // ... existing ...
    #[serde(default)]
    pub onboarded: bool,
}
```

Inline test:

```rust
#[test]
fn legacy_config_loads_onboarded_false() {
    let toml = "[gui]\n";  // no `onboarded` field
    let c: Config = toml::from_str(toml).unwrap();
    assert!(!c.gui.onboarded);
}
```

- [ ] **Step 2: Wizard submit handler (testable)**

In `crates/gui-host/src/onboarding.rs`, model the wizard's *result*:

```rust
#[derive(Debug, Clone)]
pub struct WizardSubmission {
    pub mode: AuthMode,
    pub pin_plain: Option<String>,    // None if mode != Pin
    pub default_permissions: PermissionSet,
}

pub fn apply_wizard(
    submission: WizardSubmission,
    config: &mut Config,
    host_auth: &mut HostAuthConfig,
) -> Result<(), WizardError> {
    host_auth.mode = submission.mode;
    if submission.mode == AuthMode::Pin {
        let pin = submission.pin_plain.ok_or(WizardError::PinRequired)?;
        if pin.len() < 6 { return Err(WizardError::PinTooShort); }
        host_auth.pin_hash = Some(HostAuthConfig::hash_pin(&pin)?);
    }
    host_auth.default_permissions = submission.default_permissions;
    config.gui.onboarded = true;
    Ok(())
}
```

Test:

```rust
#[test]
fn apply_wizard_writes_pin_hash() {
    let submission = WizardSubmission {
        mode: AuthMode::Pin,
        pin_plain: Some("hunter2".into()),
        default_permissions: PermissionSet::all(),
    };
    let mut cfg = Config::default();
    let mut auth = HostAuthConfig::default();
    apply_wizard(submission, &mut cfg, &mut auth).unwrap();
    assert!(cfg.gui.onboarded);
    assert_eq!(auth.mode, AuthMode::Pin);
    assert!(auth.pin_hash.is_some());
    assert!(auth.verify_pin("hunter2"));
}

#[test]
fn apply_wizard_pin_too_short_rejects() {
    let submission = WizardSubmission {
        mode: AuthMode::Pin,
        pin_plain: Some("123".into()),
        default_permissions: PermissionSet::all(),
    };
    let mut cfg = Config::default();
    let mut auth = HostAuthConfig::default();
    let err = apply_wizard(submission, &mut cfg, &mut auth).unwrap_err();
    assert!(matches!(err, WizardError::PinTooShort));
}

#[test]
fn apply_wizard_tofu_skips_pin() {
    let submission = WizardSubmission {
        mode: AuthMode::Tofu,
        pin_plain: None,
        default_permissions: PermissionSet::view_only(),
    };
    let mut cfg = Config::default();
    let mut auth = HostAuthConfig::default();
    apply_wizard(submission, &mut cfg, &mut auth).unwrap();
    assert!(cfg.gui.onboarded);
    assert_eq!(auth.mode, AuthMode::Tofu);
    assert!(auth.pin_hash.is_none());
    assert_eq!(auth.default_permissions, PermissionSet::view_only());
}
```

- [ ] **Step 3: Wizard egui rendering**

In `onboarding.rs`, write the 5-step modal. The pattern matches the
existing gui-host modals (look at `phase4-g5` crash-report banner for
modal style). 5 panels driven by an enum `WizardStep`:

```rust
pub enum WizardStep {
    Welcome,
    AuthMode,
    PinSetup,         // only entered if mode == Pin
    Defaults,
    Done,
}

pub struct WizardState {
    step: WizardStep,
    selected_mode: AuthMode,
    pin_input: String,
    pin_confirm: String,
    permissions: PermissionSet,
    error: Option<String>,
}

impl WizardState {
    pub fn show(&mut self, ctx: &egui::Context, on_finish: impl FnOnce(WizardSubmission)) {
        // ...egui modal window with step-by-step UI...
    }
}
```

- [ ] **Step 4: Settings auth subsection**

In `crates/gui-host/src/auth_settings.rs`, add the Settings panel:

- AuthMode radio (Tofu/Pin/Ephemeral) — switching prompts for save
- PIN field (`••••••••` masked); "Change PIN" button opens a modal that requires the current PIN
- Ephemeral mode: "Current ephemeral: XXXXXXXX" + "Rotate" + "Show/Hide"
- Default permissions: 4 toggles
- Saved peers list:
  ```
  ┌──────────────────────────────────────────────┐
  │ Label        Permissions         Last seen   │
  │ work         🖱️ 📋 📁 🔊         2h ago      │ [Delete]
  │ home         🖱️ 📋               yesterday   │ [Delete]
  └──────────────────────────────────────────────┘
  ```

- [ ] **Step 5: Permission prompt modal**

Extend the existing ConsentRequest gate to show a modal with:
- Header: "Viewer connecting: pubkey {first 8 hex chars} from {remote_addr}"
- Label input
- 4 toggles (default from `host_auth.default_permissions`)
- ☑️ Remember
- Allow / Deny + auto-Deny on `consent_timeout_seconds`

The existing `ConsentResponder` is extended to carry `(PermissionSet, bool remember, String label)` instead of just `bool accept`.

- [ ] **Step 6: Wire wizard launch in main**

In `gui-host`'s app `update()` or wherever the GUI starts:

```rust
if !self.config.gui.onboarded {
    self.wizard.show(ctx, |submission| {
        apply_wizard(submission, &mut self.config, &mut self.host_auth).unwrap();
        save_config(&self.config).unwrap();
        save_host_auth(&self.host_auth).unwrap();
    });
    return; // block other UI
}
// normal UI flow
```

- [ ] **Step 7: Tests + commit**

Run: `cargo test -p prdt-gui-host 2>&1 | tail -10`
Run: `cargo clippy --workspace --all-targets -- -D warnings`
Run: `cargo fmt --all`

```bash
git add crates/gui-host crates/gui-common/src/config.rs
git commit -m "feat(p6/gui-host): onboarding wizard + Settings auth + permission prompt"
```

---

## Task 8: gui-viewer hosts_list + last_connected migration + viewer-overlay permission line

**Files:**
- Modify: `crates/gui-common/src/config.rs` — HostEntry `last_connected: SystemTime` + `last_known_online`
- Modify: `crates/gui-viewer/src/hosts_list.rs` — sort + relative time + online badge
- Modify: `crates/gui-viewer/src/connect_form.rs` — update last_connected on success
- Modify: `crates/viewer-overlay/src/ipc.rs` — StatsPayload `granted_permissions`
- Modify: `crates/viewer-overlay/src/app.rs` — render permission line

- [ ] **Step 1: Failing test for last_connected migration**

```rust
// crates/gui-common/src/config.rs tests
#[test]
fn host_entry_legacy_string_last_connected_parses() {
    let toml = r#"
[[viewer.hosts]]
label = "old"
mode = "direct"
addr_or_host_id = "127.0.0.1:9000"
pubkey = ""
last_connected = "2025-12-01T00:00:00Z"
"#;
    let c: Config = toml::from_str(toml).unwrap();
    assert_eq!(c.viewer.hosts.len(), 1);
    let e = &c.viewer.hosts[0];
    assert!(e.last_connected > std::time::UNIX_EPOCH);
}

#[test]
fn host_entry_missing_last_connected_defaults_to_epoch() {
    let toml = r#"
[[viewer.hosts]]
label = "fresh"
mode = "direct"
addr_or_host_id = "127.0.0.1:9000"
pubkey = ""
"#;
    let c: Config = toml::from_str(toml).unwrap();
    assert_eq!(c.viewer.hosts[0].last_connected, std::time::UNIX_EPOCH);
}
```

- [ ] **Step 2: Implement migration**

```rust
pub struct HostEntry {
    pub label: String,
    pub mode: HostMode,
    pub addr_or_host_id: String,
    pub pubkey: String,
    #[serde(deserialize_with = "deser_last_connected", default = "epoch")]
    pub last_connected: SystemTime,
    #[serde(default)]
    pub last_known_online: Option<bool>,
}

fn epoch() -> SystemTime { std::time::UNIX_EPOCH }

fn deser_last_connected<'de, D>(d: D) -> Result<SystemTime, D::Error>
where D: serde::Deserializer<'de> {
    use serde::Deserialize;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum LegacyOrModern {
        Modern(SystemTime),
        Legacy(String),
    }
    match LegacyOrModern::deserialize(d)? {
        LegacyOrModern::Modern(t) => Ok(t),
        LegacyOrModern::Legacy(s) => {
            // RFC3339 parse, fallback to epoch
            humantime::parse_rfc3339(&s)
                .map(|t| t.into())
                .or_else(|_| Ok::<SystemTime, D::Error>(std::time::UNIX_EPOCH))
        }
    }
}
```

If `humantime` is not in the workspace, switch to `chrono` (also widely used) or just `time::OffsetDateTime::parse(&s, &Rfc3339)`. Check `Cargo.lock` first.

- [ ] **Step 3: Failing test for relative-time formatter**

```rust
#[test]
fn relative_time_buckets() {
    let now = SystemTime::now();
    assert_eq!(format_relative(now), "just now");
    assert_eq!(format_relative(now - Duration::from_secs(120)), "2 min ago");
    assert_eq!(format_relative(now - Duration::from_secs(7200)), "2 hours ago");
    assert_eq!(format_relative(now - Duration::from_secs(86400 * 3)), "3 days ago");
}

#[test]
fn relative_time_epoch_shows_never() {
    assert_eq!(format_relative(std::time::UNIX_EPOCH), "never");
}
```

- [ ] **Step 4: Implement formatter + hosts_list rewrite**

```rust
// crates/gui-viewer/src/hosts_list.rs
pub fn format_relative(t: SystemTime) -> String {
    if t == std::time::UNIX_EPOCH { return "never".into(); }
    let elapsed = SystemTime::now().duration_since(t).unwrap_or(Duration::ZERO);
    let s = elapsed.as_secs();
    if s < 60 { "just now".into() }
    else if s < 3600 { format!("{} min ago", s / 60) }
    else if s < 86400 { format!("{} hours ago", s / 3600) }
    else if s < 86400 * 30 { format!("{} days ago", s / 86400) }
    else { format!("on {}", humantime::format_rfc3339(t).to_string().split('T').next().unwrap_or("?")) }
}

pub fn render(ui: &mut egui::Ui, hosts: &mut Vec<HostEntry>, online: &HashMap<String, bool>) {
    // Sort: online first, then by last_connected DESC.
    hosts.sort_by_key(|e| {
        let is_online = online.get(&e.addr_or_host_id).copied().unwrap_or(false);
        (
            std::cmp::Reverse(is_online),
            std::cmp::Reverse(e.last_connected),
        )
    });

    for entry in hosts.iter() {
        let is_online = online.get(&entry.addr_or_host_id).copied().unwrap_or(false);
        let badge = if is_online { "🟢" } else { "⚪" };
        ui.horizontal(|ui| {
            ui.label(badge);
            ui.label(&entry.label);
            ui.label("·");
            ui.label(format_relative(entry.last_connected));
            ui.label("·");
            ui.label(&entry.addr_or_host_id);
        });
    }
}
```

- [ ] **Step 5: connect_form updates last_connected**

On successful connect (existing site already exists):

```rust
if let Some(entry) = config.viewer.hosts.iter_mut().find(|h| h.addr_or_host_id == addr) {
    entry.last_connected = SystemTime::now();
}
```

- [ ] **Step 6: viewer-overlay permission line**

In `crates/viewer-overlay/src/ipc.rs`:

```rust
pub struct StatsPayload {
    // ...
    #[serde(default)]
    pub granted_permissions: Option<PermissionSet>,
}
```

In `crates/viewer-overlay/src/app.rs`, render under the codec line:

```rust
fn render_permission_line(ui: &mut egui::Ui, perms: &PermissionSet) {
    ui.horizontal(|ui| {
        ui.label("Perms:");
        ui.colored_label(color(perms.input), "🖱️⌨️");
        ui.colored_label(color(perms.clipboard), "📋");
        ui.colored_label(color(perms.file_transfer), "📁");
        ui.colored_label(color(perms.audio), "🔊");
    });
}
fn color(on: bool) -> egui::Color32 {
    if on { egui::Color32::WHITE } else { egui::Color32::DARK_GRAY }
}
```

Add a backward-compat test:

```rust
#[test]
fn stats_payload_legacy_no_perms_parses() {
    let json = r#"{"present_p50_us": 1000}"#;
    let p: StatsPayload = serde_json::from_str(json).unwrap();
    assert!(p.granted_permissions.is_none());
}
```

- [ ] **Step 7: Tests + commit**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
git add crates/gui-common crates/gui-viewer crates/viewer-overlay
git commit -m "feat(p6/viewer-overlay): hosts_list sort + online badge + permission line"
```

---

## Task 9: STATUS update + Win+Linux smoke + tag

**Files:**
- Modify: `docs/superpowers/STATUS.md`

This is the user-blocking task. Before tagging, the user runs the manual
smoke walkthrough on both platforms; the implementer just prepares
artifacts and writes the walkthrough notes.

- [ ] **Step 1: Open PR**

```bash
git push -u origin phase-p6-auth-connection-ux
gh pr create --title "P6: auth + connection UX (TOFU/PIN/Ephemeral + per-peer permissions)" --body "$(cat <<'EOF'
## Summary

- Three-mode host auth (TOFU/PIN/Ephemeral) with mode-authoritative dispatch
- Per-peer immutable PermissionSet (input/clipboard/file-transfer/audio)
- protocol_version 2 → 3, Hello/HelloAck/HelloReject extended in place
- KnownPeer schema extension (permissions + timestamps, deny-all default for legacy)
- Onboarding wizard + Settings auth subsection
- Connection history UX (sort + relative time + online badge via signaling probe)
- Viewer auth retry loop + overlay permission line

## Test plan

- [ ] `cargo test --workspace -- -D warnings` green on Linux
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` green on Linux
- [ ] Same two on Windows (release workflow)
- [ ] Manual smoke walkthrough on Linux WSLg host ↔ Wayland viewer (notes in STATUS.md)
- [ ] Manual smoke walkthrough on Windows host ↔ Linux Wayland viewer (notes in STATUS.md)

Spec: `docs/superpowers/specs/2026-05-11-p6-auth-connection-ux-design.md` (commit `71823fc`)
Plan: `docs/superpowers/plans/2026-05-11-p6-auth-connection-ux.md`

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 2: CI green check**

Watch CI; when both Linux and Windows go green, request user smoke.

- [ ] **Step 3: User-driven manual smoke walkthrough**

The controller (Claude) provides the following walkthrough script. User
executes it and reports back; controller transcribes results into STATUS.md.

```text
Linux smoke (WSLg host + real Wayland viewer):

1. On WSLg, fresh install path:
   $ rm -f ~/.config/prdt/host-auth.toml ~/.config/prdt/config.toml
   $ ./target/release/prdt-gui-host
   - Expect: onboarding wizard appears.
   - Step 1: Welcome shows 9-digit host ID + QR.
   - Step 2: Choose "PIN", enter "hunter2" twice, click Next.
   - Step 3: Default permissions: leave all-on, click Finish.
   - Verify: ~/.config/prdt/host-auth.toml exists with mode = "Pin",
     pin_hash starts with $2b$12$..., default_permissions all = true.

2. On real Wayland host (release artifact prdt-viewer):
   $ ./prdt-viewer --signaling <SIGNALING_URL> --host-id <9-digit> --codec h264 --decoder openh264
   - Expect: PIN dialog (or terminal prompt) appears.
   - Enter wrong PIN "wrong1" → see "Wrong PIN" toast.
   - Enter correct PIN "hunter2" → connect succeeds, video starts.
   - Verify in overlay: Perms line shows all 4 icons in white.
   - Verify in host logs: "AuthVerdict::Granted" + "permissions=all".

3. Disconnect, reconnect with same viewer:
   - Expect: PIN dialog (PIN mode requires PIN every connect).
   - Verify: previous Remember choice does not skip PIN.

4. On host GUI, Settings → Default permissions → toggle clipboard off, Save.
5. Disconnect viewer, reconnect (re-enter PIN):
   - In overlay, 📋 icon is dark grey.
   - In viewer terminal: copy text on host, paste in viewer fails (clipboard
     channel denied; no error toast — silent drop is intentional).

Windows smoke (Win host + Linux Wayland viewer):

1. Download prdt-gui-host.msi from GitHub release artifact.
2. Install, run prdt-gui-host.
3. Choose TOFU mode in wizard, default permissions = view_only.
4. From Linux viewer:
   $ ./prdt-viewer --signaling <URL> --host-id <ID> --codec h264 --decoder openh264
   - Expect: connects without PIN dialog; on host GUI, permission prompt
     appears asking "Viewer pubkey ABC12345... connecting".
   - In the prompt, toggle audio off, leave Remember on, click Allow.
   - Verify: viewer overlay shows 🖱️⌨️ greyed (view_only), 📋📁 greyed, 🔊 greyed
     (toggled off in prompt).
5. Disconnect viewer, reconnect:
   - Expect: NO permission prompt (TOFU + Remember = auto-accept).
   - Verify: overlay shows the same greyed icons.
```

- [ ] **Step 4: Write STATUS.md entry**

Append to `docs/superpowers/STATUS.md`, B2 section (or new dedicated subsection),
following the P5A entry's pattern. Include:
- Summary of changes (crate map, test count delta)
- Wire change summary (Hello/HelloAck/HelloReject + protocol_version bump)
- Smoke walkthrough findings (any bugs found + fixes shipped)
- Latest tag update

Update header:

```markdown
**Last updated:** YYYY-MM-DD
**Latest tag:** `phase-p6-auth-connection-ux-complete`
```

Commit STATUS.md update on the branch.

- [ ] **Step 5: User merges via GitHub UI (squash)**

(Controller waits for user confirmation.)

- [ ] **Step 6: Tag**

After merge:

```bash
git fetch origin
git checkout master
git reset --hard origin/master   # align local with squash commit
SQUASH_SHA=$(git log -1 --format=%H)
git tag -a phase-p6-auth-connection-ux-complete -m "P6: auth + connection UX (TOFU/PIN/Ephemeral + per-peer permissions)" "$SQUASH_SHA"
git push origin phase-p6-auth-connection-ux-complete
```

- [ ] **Step 7: Final summary**

Report to user: phase tag created, P6 closed, list of next roadmap candidates
(P5B Wayland portal, P5C Linux HW codec, P7A macOS Viewer).

---

## Summary

P6 ships in 9 tasks, ~10-15 days of focused work for a single agent. The
wire is a hard break from pre-P6 binaries (intentional, lockstep release
posture), guarded by `HelloReject{ProtocolVersionMismatch}` for clear
error messaging.

## Test plan

Approximate test delta vs master:
- 11 new bincode round-trip / discriminant pin tests (T1)
- 5 new HostAuthConfig / KnownPeer tests (T2)
- 12 new AuthValidator integration tests (T3)
- 4 new permission_deny_* tests (T4)
- 3 new viewer auth-loop tests (T5)
- 2 new signaling-proto + 2 new signaling-server probe tests (T6)
- 3 new wizard apply_wizard tests (T7)
- 4 new gui-viewer + viewer-overlay tests (T8)
- **≈ 46 new tests cross-platform**

## Cross-task notes

- **`bcrypt` cost=12** is intentionally CPU-bound; budgets the host at ~50ms
  per PIN attempt. The rate-limiter at 5 attempts/peer/5min prevents this
  from being a DoS surface.
- **Constant-time compare** for ephemeral via `subtle::ConstantTimeEq`. Pad
  shorter payload to longer length is NOT done in the spec — length
  mismatch is itself a reject, which keeps the cryptographic primitive
  unambiguous.
- **`config.mode` is authoritative**: a viewer claiming `auth_method=Tofu`
  to a PIN host always gets `HelloReject{PinRequired}`. There is no
  downgrade path the viewer can force.
- **KnownPeer's serde-default `permissions = deny_all`** is intentionally
  conservative: existing users will see each saved peer pop the consent
  prompt once after the P6 upgrade. The wizard's "We've upgraded
  permissions" banner explains this.
- **GUI tests are submit-handler unit tests, not egui rendering tests**.
  Rendering correctness is verified by the T9 manual smoke walkthrough.
- **No new `kind_u8` discriminants** are introduced. Future P-phases
  (TOTP, account-based auth) can append new discriminants safely.
