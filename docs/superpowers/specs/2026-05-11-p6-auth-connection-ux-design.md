# P6: Auth & Connection UX — Design

> **Status:** Brainstorm-validated 2026-05-11 (Claude + user).
> **Roadmap context:** `docs/superpowers/specs/2026-05-11-final-goal-roadmap.md` §3 P6.
> **Branch:** `phase-p6-auth-connection-ux` (created at T1 step N).
> **Tag on completion:** `phase-p6-auth-connection-ux-complete`.

P5A finished the *machine-level* runtime decision layer (which encoder backend
to pick when the OS exposes several). P6 closes the matching *human-level*
layer: how does an unattended user trust an incoming viewer, restrict what it
can do, and find a host they've connected to before. The 9-digit ID
infrastructure from Phase 2 W5 already exists; this phase wires PIN / ephemeral
passwords / per-peer permission memory / first-run onboarding around it.

---

## 1. Goal & DoD

**Goal.** Bring connection UX to parity with RustDesk's default flow:

1. Host can be in one of three auth modes (TOFU, PIN, Ephemeral) and switches
   between them from the GUI without restarting the binary.
2. Every accepted session carries an explicit `PermissionSet` (input /
   clipboard / file-transfer / audio) negotiated at handshake time, enforced
   per-channel on the host side, and surfaced on the viewer overlay.
3. The host **remembers** trust + permissions per-peer (keyed by pubkey).
4. Viewer's saved-hosts list shows last-connected-timestamp, sort order, and
   an online badge driven by the signaling server.
5. First-run host shows an onboarding wizard with ID / QR / auth-mode choice.

**DoD (must pass before tag).**

1. `protocol_version` 2 → 3 (with HelloReject path for version-mismatch),
   Hello/HelloAck/HelloReject extended in place, full bincode round-trip.
2. Three new headless integration tests on **Linux** (`cargo test --workspace`):
   - `pin_auth_success` — viewer sends correct PIN → HelloAck with permission set.
   - `pin_auth_wrong_then_correct` — wrong PIN → HelloReject(AuthFailed) →
     reconnect with correct PIN → HelloAck. `max_pin_attempts` lockout fires
     after N wrong attempts within a window.
   - `permission_deny_clipboard` — viewer attempts `ClipboardText` while
     `granted.clipboard == false`; host silently drops, viewer-visible state
     unchanged (no crash, no error). Same pattern for the other three channels
     via parametrised sub-tests.
3. Win + Linux `cargo build/clippy/test --workspace -- -D warnings` green
   on both targets (GitHub Actions release workflow + PR CI).
4. Manual GUI smoke walkthrough on both platforms, captured in
   `docs/superpowers/STATUS.md`:
   - **Linux**: host on WSLg (`prdt-gui-host` headless mode with PIN set),
     viewer on real Wayland (`prdt-gui-viewer`) connects via 9-digit ID +
     PIN dialog, allow/deny prompt is shown when peer is unknown, 4 toggles
     can be flipped and `Remember` checkbox is honored on second connect.
   - **Windows**: same flow with `prdt-gui-host` GUI run on a Windows machine
     (release artifact), connecting Linux viewer.
5. `phase-p6-auth-connection-ux-complete` tag on the squash-merge commit.

**Explicitly NOT in DoD** (deferred to follow-up tags):

- TURN refresh / channel-bind (still Phase-2-debt).
- Mobile (P8) auth flow.
- "Trusted device" cross-signing — only single-pubkey identity per peer for now.
- TOTP / 2FA — out of scope, RustDesk doesn't have it either by default.

---

## 2. Background — current state (2026-05-11)

| Concern | Status | File reference |
|---|---|---|
| 9-digit host ID (signaling-server SQLite) | ✅ done (Phase 2 W5) | `crates/signaling-server/src/host_store.rs:49-108` |
| `--host-id` / `host-id.txt` persistence | ✅ done | `crates/host/src/lib.rs:88-94` |
| TOFU verifier (viewer side) | ✅ done | `crates/crypto/src/known_hosts.rs` |
| Known-peer list (host side) | ✅ done (label only) | `crates/crypto/src/known_peers.rs` |
| ConsentRequest gate (host GUI) | ✅ done (binary accept / reject) | `crates/host/src/lib.rs:158-169` |
| QR generation utility | ✅ done | `crates/gui-common/src/qr.rs:14-41` |
| HostEntry.last_connected field | ✅ exists, ❌ unused in UI | `crates/gui-common/src/config.rs:104-115` |
| gui-viewer hosts_list rendering | ⚠️ label/mode/addr only | `crates/gui-viewer/src/hosts_list.rs:5-31` |
| gui-host Settings tab | ⚠️ update banner + crash list only | `crates/gui-host/src/settings.rs:1-100` |
| ControlMessage::Hello (kind=0) | ✅ extends-in-place pattern | `crates/protocol/src/control.rs:36-49` |
| ControlMessage::HelloReject (kind=22) | ✅ exists, `reason: String` only | `crates/protocol/src/control.rs:140-143` |
| `PROTOCOL_VERSION` constant | u8, currently `2` | `crates/protocol/src/control.rs:181` (Hello default), `crates/protocol/src/wire.rs:9` (wire header MAGIC + version) |

Two `PROTOCOL_VERSION` paths exist:

- `wire::PROTOCOL_VERSION = 0x01` — UDP packet header magic+version, **stays
  at 1**. P6 does not touch the framing layer.
- `ControlMessage::Hello.protocol_version` — application-level negotiation,
  bumps **2 → 3**. The host validates this and replies with
  `HelloReject { code: ProtocolVersionMismatch }` if the viewer is older.

---

## 3. Architecture

```text
viewer                                  signaling-server         host
  │ ── Connect{host_id} ──────────────────► dispatch ──────────► (host receives Connect)
  │ ◄────────────────────────────────────── SessionStart ◄──────│
  │ ── UDP hole punch / direct connect ──────────────────────────►
  │ ── Noise handshake (NK pattern) ─────────────────────────────►
  │ ── Hello{protocol_version=3,
  │         auth_method,
  │         auth_payload, ...} ─────────────────────────────────► AuthValidator
  │                                                                  │
  │                                          (host known_peers, HostAuthConfig)
  │                                                                  │
  │   ┌─────────────────────────────────────────────────┐            │
  │   │ TOFU: unknown peer → ConsentRequest → GUI prompt│            │
  │   │       known peer → load saved permissions       │            │
  │   │ PIN:  bcrypt::verify(pin, host_auth.pin_hash)   │            │
  │   │       → load saved permissions or defaults      │            │
  │   │       → on N failures, lockout for cooldown_s   │            │
  │   │ Ephemeral: const-time compare against active    │            │
  │   │            ephemeral, expires after lifetime_s  │            │
  │   └─────────────────────────────────────────────────┘            │
  │                                                                  │
  │ ◄── HelloAck{granted_permissions} ── or ─── HelloReject{code} ──┘
  │
  │ ── normal session, every channel gated by granted_permissions ──►
```

The flow has two **invariants** the implementation must preserve:

- **No control message (other than NoiseE1/E2/Hello/HelloAck/HelloReject/Bye)
  is processed on the host until HelloAck is sent.** Anything else is dropped.
- **Permissions are immutable for the lifetime of a session.** If the host
  user wants to change them mid-session, they must disconnect the viewer (the
  GUI surfaces a "Revoke" button). This keeps the host control-loop logic
  free of per-event re-checks against a mutable shared state.

### 3.1 Why Hello-time (not Connect-time, not mid-session)

- **Not Connect-time** (signaling layer): the signaling server is operated by
  someone other than the host user. Putting PINs there leaks them to the
  operator. Adding `Connect{auth_method}` as a *hint* (so the viewer GUI can
  show the right dialog up front) is tempting but adds wire surface for
  little gain — the host can equally well send `HelloReject{PinRequired}` on
  the first try and the viewer can re-Hello with `auth_payload`.
- **Not mid-session**: revoking permissions mid-session is rare and
  recoverable by disconnect-reconnect; constant rechecks on every input
  event is hot-path work for cold-path value.
- **Hello-time is post-Noise**, so PINs traverse an E2E-encrypted channel
  and an attacker on the path sees only ciphertext.

### 3.2 Hello-reject + retry vs. Hello-includes-everything

The viewer cannot know in advance which auth mode a given host is in. Two
options:

1. **Lazy**: viewer sends `Hello{auth_method=Tofu, auth_payload=[]}` first.
   If host requires PIN, it replies `HelloReject{code=PinRequired}`. Viewer
   shows PIN dialog, re-Hello with `auth_method=Pin, auth_payload=pin`. One
   round-trip extra in the PIN case.
2. **Eager**: viewer always shows the auth dialog up-front before connecting.
   No spare round-trip, but the viewer cannot reason about which dialog to
   show without prior knowledge.

**P6 chooses Lazy.** The savings of one round-trip are not worth the UX hit
of a wrong-mode dialog before any contact is made.

---

## 4. Wire protocol changes

All changes are in `crates/protocol/src/control.rs`. The wire pattern follows
the same extension model that already added `negotiated_codec` and
`host_supported_codecs` to HelloAck: append fields at the end of an existing
variant + bump `Hello.protocol_version` to gate. No new `kind_u8`
discriminants are introduced; existing 0/1/22 retain their meaning.

### 4.1 New types (added to `control.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum AuthMethod {
    /// Default; relies on TOFU + host-side ConsentRequest gate. Viewer
    /// sends empty `auth_payload`.
    Tofu = 0,
    /// PIN-mode; `auth_payload` is the UTF-8 PIN, 4..=64 bytes, host
    /// compares bcrypt(payload) vs stored hash.
    Pin = 1,
    /// Ephemeral; `auth_payload` is a 6..=12 byte ASCII random string
    /// shown on the host GUI; host compares constant-time vs the
    /// currently active ephemeral.
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
    /// Pre-P6 default; `reason` carries the human-readable message.
    Unspecified = 0,
    /// Viewer's protocol_version is lower than host's minimum supported
    /// (and the difference is not bridgeable). Viewer should prompt
    /// the user to upgrade.
    ProtocolVersionMismatch = 1,
    /// Viewer's requested codec is not in the host's supported set.
    /// Pre-existing case, now codified.
    UnsupportedCodec = 2,
    /// Host is in PIN mode; viewer should ask the user for the PIN
    /// and re-Hello with `auth_method = Pin`.
    PinRequired = 3,
    /// Host is in Ephemeral mode.
    EphemeralRequired = 4,
    /// Auth payload was wrong (wrong PIN, expired ephemeral, etc.).
    /// `reason` may carry "attempts remaining: N" hint.
    AuthFailed = 5,
    /// PIN entry rate limit fired. Viewer should not retry until
    /// the user dismisses the cooldown dialog.
    AuthLockout = 6,
    /// TOFU host-side ConsentRequest came back with reject.
    ConsentDenied = 7,
}
```

### 4.2 Hello / HelloAck / HelloReject (extended)

```rust
ControlMessage::Hello {
    protocol_version: u8,        // bump 2 → 3 default
    req_width: u32,
    req_height: u32,
    req_fps: u32,
    codec: Codec,
    auth_method: AuthMethod,     // NEW (P6)
    auth_payload: Vec<u8>,       // NEW (P6), max 64 bytes; empty for Tofu
}

ControlMessage::HelloAck {
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
    granted_permissions: PermissionSet,  // NEW (P6)
}

ControlMessage::HelloReject {
    reason: String,              // existing — human-readable display
    code: HelloRejectCode,       // NEW (P6) — machine-readable for viewer branching
}
```

### 4.3 Wire-compat policy

- **Host accepts** `protocol_version == 3` only. Anything else → respond with
  `HelloReject { code: ProtocolVersionMismatch, reason: "host requires
  protocol_version=3 (P6+)" }`.
- **Viewer** sends `protocol_version: 3` unconditionally; receiving a
  `HelloReject { code: ProtocolVersionMismatch }` shows an upgrade-required
  dialog.
- The bincode positional change for Hello / HelloAck / HelloReject is a
  hard wire break vs. pre-P6 releases. **All in-house binaries ship in
  lockstep** (same release tag); there is no "old field deployment" to
  protect — this is the same lockstep posture the codebase has held since
  Phase 0.
- `auth_payload` size is hard-capped at 64 bytes; host bincode-deserializer
  rejects larger payloads as `HelloReject { code: Unspecified, reason:
  "auth_payload too long" }` before any cryptographic work.

### 4.4 No new `kind_u8` discriminants

P6 deliberately does not introduce `AuthChallenge` / `AuthResponse` /
`AuthGranted` separate variants. The Hello/HelloReject/Hello round-trip
already expresses the challenge-response: HelloReject's `code` is the
challenge, the second Hello carries the response.

The `// DO NOT INSERT VARIANTS ABOVE THIS LINE` invariant in `control.rs:144`
stays intact; future P-phases can append new discriminants safely.

---

## 5. Host-side: configuration model

### 5.1 `HostAuthConfig` (new)

Lives next to existing `HostConfig` in `crates/host/src/lib.rs`. Serialized
to `~/.config/prdt/host-auth.toml` (or `%APPDATA%\prdt\host-auth.toml`) so
the PIN hash is in a separate file from general config, easier to revoke.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostAuthConfig {
    pub mode: AuthMode,                       // Tofu (default) | Pin | Ephemeral
    pub pin_hash: Option<String>,             // bcrypt hash, `Some` iff mode == Pin
    pub ephemeral_lifetime_seconds: u32,      // default 120
    pub default_permissions: PermissionSet,   // default = PermissionSet::all()
    pub max_pin_attempts: u8,                 // default 5
    pub pin_lockout_seconds: u32,             // default 300 (5 min)
    pub consent_timeout_seconds: u32,         // default 60; TOFU prompt auto-Deny timeout
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthMode { Tofu, Pin, Ephemeral }
```

**Defaults / migration.** If `host-auth.toml` is missing, treat as
`mode = Tofu, default_permissions = all(), pin_hash = None`. This keeps
pre-P6 behaviour identical for users who don't change anything.

**PIN storage.** bcrypt with cost=12. The bcrypt crate is already in the
workspace tree (via transitive use in some auth-related dep, audit at T1);
if not, add `bcrypt = "0.15"` as a direct dep of `crates/host`.

**Ephemeral generation.** 8-character ASCII (uppercase letters + digits, with
similar-looking pairs O/0 and I/1/L removed for usability). Regenerated when
the host user clicks "Show new ephemeral" in the GUI; previous ephemeral is
invalidated immediately. Stored in-memory only (`Arc<Mutex<Option<(String,
Instant)>>>`), never written to disk.

### 5.2 `KnownPeer` schema extension

Existing `crates/crypto/src/known_peers.rs` stores `(pubkey, label)`. Extend:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownPeer {
    pub pubkey_b64: String,
    pub label: String,
    pub permissions: PermissionSet,           // NEW; serde(default) for old files
    pub first_seen_at: SystemTime,            // NEW; serde(default = epoch)
    pub last_seen_at: SystemTime,             // NEW; serde(default = epoch)
}
```

Backwards compat: `#[serde(default)]` on the three new fields means existing
`known_peers.toml` files load unchanged with `permissions = PermissionSet::
default() = all-false`. This is intentionally **conservative**: existing
saved peers get **denied** until the host user re-approves them once. The
onboarding wizard surfaces this as "We've upgraded the security model; you
have N saved peers that need to be re-approved on next connection." Cost of
the extra prompt vs. silently granting unprompted permissions: easy choice.

Alternative considered: default to `PermissionSet::all()` for legacy peers.
Rejected because the user has not consented to the new per-channel concept,
so opting them in silently would violate informed consent.

### 5.3 Host CLI flags (`crates/host/src/lib.rs`)

| Flag | Effect |
|---|---|
| `--auth-mode {tofu, pin, ephemeral}` | Override `HostAuthConfig.mode` for this run. Headless smoke tests use this. |
| `--pin <PIN>` | Set PIN once (computes bcrypt, writes to `host-auth.toml`, exits). Not for interactive use. |
| `--ephemeral-print` | Print current ephemeral to stdout and exit (for scripting). |
| `--allow-input` / `--no-input` | Override `default_permissions.input` for this run. Same for `--allow-clipboard` / `--allow-file-transfer` / `--allow-audio` and their `--no-*` counterparts. |
| `--no-remember` | All sessions during this run treated as `Remember=false`; useful for ephemeral demo runs. |

Mutual exclusions:
- `--auth-mode pin` without `--pin <PIN>` (and no stored hash) → fatal exit
  with "PIN not set; run with `--pin <PIN>` first or use `--auth-mode tofu`".

---

## 6. Host-side: auth validator state machine

Lives in a new module `crates/host/src/auth.rs`. Single struct
`AuthValidator` driven from the existing handshake task in `host/src/lib.rs`.

```rust
pub struct AuthValidator {
    config: HostAuthConfig,
    known_peers: Arc<RwLock<KnownPeers>>,
    ephemeral: Arc<Mutex<Option<EphemeralState>>>,
    pin_attempts: Arc<Mutex<HashMap<String, PinAttemptState>>>, // keyed by pubkey_b64
}

pub enum AuthVerdict {
    Granted { permissions: PermissionSet, remember: bool },
    Rejected { code: HelloRejectCode, reason: String },
    NeedsConsent { responder: ConsentResponder }, // pre-existing TOFU path
}

impl AuthValidator {
    pub async fn validate(
        &self,
        hello: &Hello,
        peer_pubkey_b64: &str,
    ) -> AuthVerdict { ... }
}
```

State machine. The decision tree always starts with version + codec gates,
then dispatches on `config.mode` (not on viewer's `auth_method` — that is
just a candidate the viewer offers, and a mismatch is itself a reject):

```text
        Hello
          │
          ▼
  ┌────────────────────┐
  │ protocol_version=3?│ no → Reject(ProtocolVersionMismatch)
  └────────┬───────────┘
          yes
          ▼
  ┌────────────────────┐
  │ codec supported?   │ no → Reject(UnsupportedCodec)
  └────────┬───────────┘
          yes
          ▼
  ┌─────────────────────────────────────────┐
  │ dispatch on config.mode                  │
  └─┬───────────────┬───────────────────────┬┘
    │               │                       │
 Tofu mode      Pin mode                Ephemeral mode
    │               │                       │
    ▼               ▼                       ▼
 hello.auth_      hello.auth_             hello.auth_
 method ==        method == Pin?          method == Ephemeral?
 Tofu?            no → Reject(PinRequired) no → Reject(EphemeralRequired)
 no → Reject(    yes                       yes
   ConsentDenied  ▼                          ▼
 ; viewer can    pin_lockout fired?         expired?
 retry with      yes → Reject(AuthLockout)  yes → Reject(AuthFailed)
 method=Tofu)    no                          no
    yes          ▼                           ▼
    ▼            bcrypt::verify              subtle::ConstantTimeEq
 in known_       no → Reject(AuthFailed)     no → Reject(AuthFailed)
 peers?          yes (reset counter)         yes (clear ephemeral)
    │            │                           │
    │ no         ▼                           ▼
    ▼          peer in known_peers?       peer in known_peers?
 NeedsConsent  yes → Granted(             yes → Granted(
 (GUI prompt        saved_perms,                 saved_perms,
  with timeout      remember=true)               remember=true)
  consent_      no  → Granted(             no  → Granted(
  timeout_           default_perms,              default_perms,
  seconds)           remember=requested)         remember=requested)
    │
   yes
    ▼
 Granted(saved_perms, remember=true)
```

The key insight: **`config.mode` is authoritative**, viewer's
`auth_method` is just a hint. A PIN-mode host that receives
`auth_method=Tofu` returns `HelloReject{PinRequired}` so the viewer
prompts for PIN. This prevents downgrade: there's no path where a
viewer can talk a PIN host into TOFU.

**Known-peers fast-path semantics differ per mode** (this is the
big-picture reason for the cross-mode dispatch first):
- **TOFU**: known_peers means "auto-accept, skip prompt".
- **PIN**: known_peers stores per-peer permissions; PIN is still
  required every connection.
- **Ephemeral**: same as PIN — ephemeral every connection, known_peers
  only stores permissions.

Ephemeral structure parallels PIN: a `subtle::ConstantTimeEq` compare
against the active ephemeral string, with the host's
`EphemeralState { value: String, created_at: Instant }` checked for
expiry before the compare.

### 6.1 Rate limiting (PIN)

`pin_attempts: HashMap<peer_pubkey_b64, PinAttemptState>` with
`PinAttemptState { failed_count: u8, locked_until: Option<Instant> }`. Each
failure increments `failed_count`. On `failed_count >= max_pin_attempts`,
set `locked_until = now + pin_lockout_seconds`. The next attempt within the
lockout returns `AuthLockout`. After the lockout passes, `failed_count`
resets to 0. A successful PIN entry resets the counter immediately.

Per-pubkey scoping prevents one attacker from locking out a legitimate user
by spamming wrong PINs. Memory is bounded: entries with `failed_count == 0`
and `locked_until == None` are pruned on every check.

### 6.2 TOFU → ConsentRequest integration

The existing `ConsentRequest` gate (host GUI prompt for unknown peer) is
preserved unchanged for `AuthMode::Tofu`. The new bit is that the
ConsentResponder now carries a `PermissionSet` and a `bool remember` from
the GUI dialog, both of which flow into:

- If `remember == true`: insert/update `KnownPeer { pubkey, label, permissions,
  first_seen_at = now (if new), last_seen_at = now }` and write
  `known_peers.toml`.
- If `remember == false`: skip the persist step, treat as session-only grant.

### 6.3 Known-peer fast path (mode-dependent)

`known_peers` plays two different roles depending on `config.mode`:

- **In TOFU mode**: a known peer skips the ConsentRequest prompt and is
  granted `peer.permissions` directly. This is the "auto-accept previously
  approved" behaviour.
- **In PIN / Ephemeral mode**: a known peer still has to provide a valid
  PIN/ephemeral. `known_peers` is only consulted *after* the auth payload
  passes, to load `peer.permissions` instead of falling back to
  `default_permissions`. There is no "skip auth because we know you" path
  — that would defeat the point of PIN.

The host user can revoke from the Settings tab (a "Saved peers" list with
delete button), which removes the entry from `known_peers.toml`.

### 6.4 Headless host behaviour

If no GUI process is registered (no `ConsentResponder`) and the validator
hits a `NeedsConsent` state, default to `Reject(ConsentDenied)`. This makes
unattended servers safe by default: only PIN / Ephemeral modes work
headless. The CLI flag `--auto-accept-tofu` flips this default for testing
**only** (loud warning in stderr on startup).

---

## 7. Host-side: per-channel permission enforcement

In `crates/host/src/lib.rs`, the existing per-session state struct gains:

```rust
struct SessionState {
    // ... existing ...
    permissions: PermissionSet,  // NEW, set on HelloAck
}
```

`permissions` is **immutable** for the session's lifetime (see §3
invariant). The control loop arms are wrapped with a single helper:

```rust
fn channel_allowed(perms: &PermissionSet, msg: &ControlMessage) -> bool {
    match msg {
        ControlMessage::ClipboardText { .. } => perms.clipboard,
        ControlMessage::FileTransferBegin { .. } |
        ControlMessage::FileChunk { .. } |
        ControlMessage::FileTransferEnd { .. } => perms.file_transfer,
        // `Input` flows over a separate transport channel (not ControlMessage)
        // — see below.
        _ => true, // Ping, Pong, KeepAlive, etc. always allowed
    }
}
```

For `ControlMessage` arms in the host control loop, the existing arms each
get an early-out:

```rust
Ok(ReceivedMessage::Control(msg)) => {
    if !channel_allowed(&session.permissions, &msg) {
        tracing::debug!(?msg, peer = %session.peer_pubkey, "channel denied; dropping");
        continue; // silently drop
    }
    match msg { /* existing handling */ }
}
```

**Input** is delivered on a separate transport channel (`InputPacket`, not
`ControlMessage`). The input task spawn is wrapped:

```rust
if session.permissions.input {
    tokio::spawn(input_task(...));
} else {
    tracing::info!("input channel denied for this session");
}
```

When `permissions.input == false`, no input task is spawned; viewer-sent
InputPackets land in the assembler and are dropped (no consumer).

**Audio** follows the same pattern as input: the audio capture task is only
spawned when `permissions.audio == true`. Viewer audio downstream end is a
no-op if no audio frames arrive (existing path handles silent stream).

### 7.1 Why silent drop, not reject

The wire is post-Noise (peer is authenticated), so denied channels can't
exfiltrate state. Silent drop avoids leaking permission topology to a
hostile viewer and matches RustDesk's behaviour. The viewer's overlay shows
the granted set so the human user knows.

---

## 8. Viewer-side: auth flow + UI

### 8.1 Hello / HelloReject retry loop

In `crates/viewer/src/lib.rs`, the existing handshake task gains a
`HelloAttempt` state machine:

```rust
enum HelloAttempt {
    FirstTry,                                  // sent auth_method=Tofu, empty payload
    AfterPinChallenge { pin: Option<String> }, // user-prompted, may be None if user cancelled
    AfterEphemeralChallenge { ephemeral: Option<String> },
}
```

Pseudocode:

```rust
let mut attempt = HelloAttempt::FirstTry;
loop {
    let hello = build_hello(&attempt);
    transport.send_control(hello).await?;
    let msg = transport.recv_control().await?;
    match msg {
        HelloAck { granted_permissions, .. } => {
            // success path; set viewer state, render overlay badge
            break;
        }
        HelloReject { code, reason } => {
            match code {
                HelloRejectCode::PinRequired => {
                    let pin = prompt_pin_dialog().await; // GUI; CLI returns None
                    attempt = HelloAttempt::AfterPinChallenge { pin };
                }
                HelloRejectCode::EphemeralRequired => {
                    let eph = prompt_ephemeral_dialog().await;
                    attempt = HelloAttempt::AfterEphemeralChallenge { ephemeral: eph };
                }
                HelloRejectCode::AuthFailed => {
                    show_error("Wrong PIN — try again");
                    // recurse with prev attempt type
                }
                HelloRejectCode::AuthLockout => {
                    show_error("Too many wrong attempts; please wait 5 minutes.");
                    return Err(...);
                }
                HelloRejectCode::ProtocolVersionMismatch => {
                    show_error("Host requires a newer protocol; please upgrade.");
                    return Err(...);
                }
                _ => return Err(...),
            }
        }
        _ => return Err("unexpected pre-Ack message"),
    }
}
```

The CLI viewer (`prdt-viewer --headless ...`) handles dialogs by reading
`--pin <PIN>` and `--ephemeral <EPH>` flags up front; if Hello-reject
demands one that wasn't provided, the CLI exits with a clear error code.

### 8.2 Connection history UI

`HostEntry` schema change:

```rust
pub struct HostEntry {
    pub label: String,
    pub mode: HostMode,                  // existing (Direct | Signaling)
    pub addr_or_host_id: String,
    pub pubkey: String,
    pub last_connected: SystemTime,      // type change: String → SystemTime
                                          // serde(default) = epoch for forward-compat
    pub last_known_online: Option<bool>, // NEW; signaling-server probe result
}
```

Migration: existing config files have `last_connected: String`. T2 task adds
a custom `Deserialize` that accepts either a String (parses RFC3339, falls
back to epoch on parse error) or a SystemTime. After one round-trip through
`gui-viewer`, the field is written back as ISO timestamp.

`crates/gui-viewer/src/hosts_list.rs` rewrite:

- Sort by `last_connected DESC` with online entries hoisted to the top.
- Row format: `🟢 <label> · <relative-time> · <addr_or_host_id>`
- Relative time: "just now" / "N min ago" / "N hours ago" / "N days ago" /
  ISO date.
- Online badge driver: see §8.3.

### 8.3 Online badge via signaling-server

Existing signaling-proto already has `ClientMessage::Connect { host_id }` and
`ServerMessage::Error { code: HostNotFound | ... }`. Reusing `Connect` as a
probe is heavy (it triggers a real session attempt). Instead, add a new
lightweight RPC:

```rust
// signaling-proto
ClientMessage::ProbeHosts { host_ids: Vec<String> },
ServerMessage::ProbeResult { online: Vec<String> }, // subset of requested IDs
```

The signaling-server already knows which host IDs are currently connected
(they hold a WebSocket). The probe returns the intersection. No DB query,
constant work per request.

Viewer polls every 30s while the hosts_list view is open, never when in a
session.

**Out of scope**: cross-instance probes (when the user has hosts on
different signaling servers — Phase-9 federation territory).

### 8.4 Viewer overlay (extension of P5A backend badge)

P5A added `🚀 HW / 💻 SW` next to decoder name. P6 extends the StatsPayload:

```rust
struct StatsPayload {
    // ... existing P5A fields ...
    granted_permissions: Option<PermissionSet>, // NEW; serde(default)
}
```

Overlay app renders a permissions line:

```text
Video: NVENC (🚀 HW) → NVDEC
Perms: 🖱️ ⌨️ 📋 📁 🔊       ← grey out denied channels
```

Icons: 🖱️ mouse + ⌨️ keyboard (combined = `input`), 📋 clipboard, 📁
file-transfer, 🔊 audio. (Plain text fallback for terminals without emoji:
`I/C/F/A` with denied shown as lowercase.)

### 8.5 CLI flags (`crates/viewer/src/lib.rs`)

| Flag | Effect |
|---|---|
| `--pin <PIN>` | Preset PIN; if host demands one and this is set, send it without prompting. Headless smoke uses this. |
| `--ephemeral <EPH>` | Preset ephemeral. |
| `--no-auth-prompt` | If host demands PIN/Ephemeral and no flag is preset, exit immediately rather than blocking on a TTY prompt. CI smoke uses this. |

---

## 9. Host GUI: onboarding wizard

Triggered on `gui-host` start when `config.gui.onboarded == false`. Modal
that blocks the rest of the UI until completed (Skip is allowed but logs a
warning).

### 9.1 Steps

1. **Welcome + ID**: large display of the 9-digit host ID + QR code
   (`gui-common::qr::generate`). "Share this ID with the people you want
   to connect from."
2. **Auth mode**: 3 radio buttons:
   - **TOFU** (recommended) — "First connection asks for your approval; later
     connections from the same device are remembered."
   - **PIN** — "Anyone who knows the PIN can connect, no per-connection
     approval. Good for unattended access."
   - **Ephemeral** — "A fresh code is required for each connection. The
     code is shown on this screen; you tell it to the connecting person."
3. **PIN setup** (only shown if step 2 = PIN): two-field PIN entry with
   confirmation. Constraints: ≥ 6 chars, must not be all digits unless
   explicitly confirmed ("really use a 6-digit PIN?" warning).
4. **Default permissions**: 4 toggles (all-on by default) + "What's this?"
   tooltip per toggle.
5. **Done**: "Saving these settings to `host-auth.toml`. You can change them
   anytime from Settings." Sets `config.gui.onboarded = true`.

### 9.2 Settings tab integration

Same content + a "Reset to defaults" button. PIN field shows
`••••••••` placeholder; clicking unlocks an edit modal that requires
**re-entering the current PIN** if one is set (prevents the GUI-walking
scenario). For TOFU mode, a "Saved peers" subsection lists known peers
with their permissions and a delete button.

A "Show current ephemeral" button in Ephemeral mode reveals the current
8-character code; a "Rotate" button generates a new one and invalidates
the old.

---

## 10. Host GUI: permission prompt modal

Triggered when `AuthValidator` returns `NeedsConsent` (TOFU + unknown peer
case). Modal contains:

- Header: "Viewer connecting from: `<viewer_pubkey_short>` (`<remote_addr>`)"
  - `viewer_pubkey_short` is the first 8 hex chars of the pubkey.
- 4 toggles (default = `HostAuthConfig.default_permissions`):
  - 🖱️⌨️ Input — Mouse and keyboard control
  - 📋 Clipboard — Text clipboard sync
  - 📁 File transfer — Drag-drop both directions
  - 🔊 Audio — System audio
- Label field: "Save this peer as:" (default = `viewer_pubkey_short`)
- ☑️ **Remember this peer** (default = on)
- **Allow** (primary) / **Deny** (secondary) buttons
- Auto-deny timeout: 60 seconds, configurable in
  `HostAuthConfig.consent_timeout_seconds`. On timeout, treated as Deny.

If `Remember == on`, the saved KnownPeer carries the toggled permissions;
next connection from the same pubkey skips this modal.

---

## 11. Implementation order (informs the plan)

The plan that follows this spec breaks into roughly 9 tasks:

1. **T1** — `prdt-protocol` types (AuthMethod, PermissionSet,
   HelloRejectCode, extended Hello/HelloAck/HelloReject), protocol_version
   bump 2→3, full bincode round-trip tests, branch creation
   `phase-p6-auth-connection-ux`.
2. **T2** — `HostAuthConfig` + `host-auth.toml` persistence, CLI flags,
   bcrypt PIN setup, ephemeral generator, KnownPeer schema extension.
3. **T3** — `AuthValidator` module (auth.rs) + rate-limiter + handshake
   integration in `host/src/lib.rs`. Three new integration tests
   (pin_auth_success, pin_auth_wrong_then_correct, ephemeral_auth_success).
4. **T4** — Per-channel permission enforcement in host control loop +
   input/audio task gating. `permission_deny_*` parametrised tests.
5. **T5** — Viewer Hello/HelloReject retry loop + CLI flags
   (`--pin`/`--ephemeral`/`--no-auth-prompt`).
6. **T6** — signaling-proto ProbeHosts / ProbeResult + signaling-server
   handler + viewer poll task (online badge backend).
7. **T7** — gui-host onboarding wizard + Settings tab auth section +
   permission prompt modal extension.
8. **T8** — gui-viewer connection-history UI rewrite (sort + relative time
   + online badge) + viewer overlay permission line.
9. **T9** — STATUS.md update, manual smoke walkthrough on Win + Linux,
   `phase-p6-auth-connection-ux-complete` tag.

Each task follows the L4/P5A subagent-driven-development pattern: fresh
implementer → spec compliance reviewer → opus code quality reviewer →
commit → next task. T7/T8 are GUI-heavy and may need split if they grow
past ~6 hours of estimated work.

---

## 12. Test strategy

### 12.1 Unit tests (per crate)

- `prdt-protocol` (T1): bincode round-trip for new types, kind_u8 stability
  unchanged, `PermissionSet::all/view_only/deny_all` constants pinned.
- `prdt-host::auth` (T3): rate-limit reset on success, lockout fire/release,
  bcrypt verify ok/fail, ephemeral expiry, constant-time compare uses
  `subtle::ConstantTimeEq`.
- `prdt-crypto::known_peers` (T2): old-format file (without
  `permissions`/`first_seen_at`/`last_seen_at`) loads with serde defaults,
  round-trips back with new fields filled.
- `prdt-gui-common::config` (T2): HostEntry `last_connected` String→SystemTime
  migration roundtrip.
- `prdt-signaling-proto` (T6): ProbeHosts/ProbeResult round-trip.

### 12.2 Integration tests (workspace-level)

- `pin_auth_success` (T3): in-process host + viewer over `InProcTransport`,
  PIN configured, viewer connects with correct PIN, HelloAck arrives with
  `granted_permissions == default_permissions`.
- `pin_auth_wrong_then_correct` (T3): viewer sends wrong PIN twice, expects
  two `HelloReject { code: AuthFailed }`, then sends correct PIN, expects
  `HelloAck`. After 5 wrong attempts, expects `HelloReject { code: AuthLockout }`.
- `ephemeral_auth_success` (T3): host generates ephemeral, viewer connects
  with it, success.
- `ephemeral_expired` (T3): viewer connects with ephemeral generated > N
  seconds ago, expects `AuthFailed`.
- `tofu_consent_remember_persists` (T3): unknown peer first connect →
  ConsentRequest fires → fake GUI responds Accept + Remember + permissions
  {input:true, clipboard:false, ...}. Second connect from same pubkey →
  no ConsentRequest fires, HelloAck with same permissions.
- `permission_deny_clipboard` (T4): granted_permissions has
  `clipboard=false`. Viewer sends `ClipboardText { text: "hello" }`. Host
  control loop drops silently. Host's clipboard state is unchanged.
  Parametrised for the other three channels.
- `protocol_version_mismatch` (T1+T3): viewer sends Hello with
  `protocol_version = 2`, expects `HelloReject { code:
  ProtocolVersionMismatch }`.

### 12.3 Manual smoke (T9)

Captured in STATUS.md as text walkthrough with exact CLI invocations and
expected outputs:

1. Linux WSLg host (GUI) with PIN configured + Linux real Wayland viewer:
   - Connect: PIN dialog appears, enter wrong PIN once → error toast, enter
     correct PIN → connect succeeds. Verify HelloAck arrives in trace logs
     with `granted_permissions = all`.
   - Disconnect, reconnect: no PIN dialog (host remembers this peer); allow
     prompt does not appear; permissions = all.
   - In Settings, change `default_permissions.clipboard = false`. Disconnect
     all peers. Reconnect: HelloAck shows `clipboard = false`, viewer
     overlay shows `📋` greyed; copy/paste does nothing.
2. Windows GUI host + Linux Wayland viewer (release artifact):
   - First connect, TOFU mode: host shows permission prompt with 4 toggles,
     "Remember" checked, click Allow → viewer connects.
   - Disconnect, reconnect: no prompt; permissions match prior choice.
3. Onboarding wizard:
   - Fresh `prdt-gui-host` install (delete config first): wizard runs,
     choose PIN mode, set PIN "012345" (warning shown), set default
     permissions to `clipboard:false`, finish. Verify `host-auth.toml`
     contents.

### 12.4 Property tests (proptest)

- `permission_serde_roundtrip` — random PermissionSet bytes round-trip.
- `auth_payload_size_cap` — random `Vec<u8>` of size 0..=128, payload > 64
  bytes always rejected by host before bcrypt; ≤ 64 always reaches bcrypt
  (or constant-time compare).

---

## 13. Out of scope (deferred)

- **TURN refresh / channel-bind** — Phase-2 debt, unrelated.
- **TOTP / 2FA** — not in RustDesk's default; would require time sync UI.
- **Cross-device account / sync** — Phase-9 (federation).
- **Multi-pubkey identity per peer** — single pubkey only; if user re-runs
  `prdt-gui-host` and regenerates a key, host sees a new peer and prompts.
- **Permission templates / role definitions** — only the 4 toggles, no
  named roles like "Helpdesk view-only" / "Family member full".
- **Permission revoke mid-session** — disconnect-reconnect required.
- **Mobile (P8) auth UI** — P6 enables the wire, P8 builds the iOS/Android
  PIN dialog.
- **Per-channel **audit log** — drop counts only via `tracing::debug!`, no
  user-visible "the peer tried to use clipboard 17 times" stat.
- **Ephemeral via push notification / email** — only viewable on host GUI.
- **Account-based password recovery** — local hash only, lose the PIN →
  user resets via the GUI "Reset to TOFU" button.

---

## 14. Risks & mitigations

| Risk | Mitigation |
|---|---|
| bcrypt cost=12 takes ~50ms per attempt; attacker DoSes the host with PIN attempts. | Rate-limit at 5 attempts/peer/5min (§6.1). Even sustained attack from 1000 peers caps at ~17 bcrypt calls/sec, well below host CPU. |
| Ephemeral leaked through shoulder-surfing of host GUI. | 120s default lifetime + manual rotate button. User chose to display it; same trust model as RustDesk. |
| KnownPeer file growth (one entry per connecting device, forever). | Settings tab "Saved peers" subsection shows them with last_seen_at; user can prune. Hard cap not needed (~100 bytes per entry, 10k peers = 1MB). |
| PIN set via `--pin <CLI flag>` ends up in shell history. | Warn in CLI help text. Recommend GUI flow. The Settings tab does not show the PIN even if entered via CLI. |
| serde `#[serde(default)]` on KnownPeer.permissions = `deny_all()` is *too* conservative — existing users have to re-approve every saved peer. | One-time migration prompt: on first P6 launch, show a dialog "We've upgraded permissions: re-approve N saved peers? [List...]". User can bulk-approve. |
| Hello/HelloAck wire break with pre-P6 binaries. | Lockstep release (already the codebase posture). HelloReject with `code: ProtocolVersionMismatch` gives a clear error message instead of a confusing parse failure. |
| Viewer hangs waiting for PIN dialog in CLI smoke. | `--no-auth-prompt` flag forces immediate exit if a prompt would block. |
| Onboarding wizard skipped via X-button leaves PIN unset but mode=Pin → host won't accept any viewer. | Wizard's "Skip" button reverts mode to Tofu before exit, not Pin. |
| Online badge polling load on signaling-server with many viewers. | 30s poll interval + batched `ProbeHosts { host_ids: Vec<String> }` keeps QPS low. Server-side: O(1) HashMap lookup per ID. |
| Permission downgrade attack: viewer claims `auth_method=Tofu` to bypass PIN. | If `config.mode == Pin`, host *rejects* any `auth_method != Pin` with `HelloReject(PinRequired)`. Likewise for Ephemeral. The viewer cannot choose its own auth path. |
| Constant-time-equal bypass via short payload. | `subtle::ConstantTimeEq` requires equal-length inputs; host pads shorter payload to PIN/ephemeral length with zero-bytes before compare. Length-difference is itself a `Reject(AuthFailed)`. |

---

## 15. References

- Phase 2 W5 spec (9-digit ID + signaling): `docs/superpowers/specs/2026-04-2*-phase2-w5-*.md`.
- P5A spec (capability/policy layer, for the reviewer/badge UX patterns):
  `docs/superpowers/specs/2026-05-11-p5a-capability-policy-design.md`.
- Roadmap §3 P6 entry: `docs/superpowers/specs/2026-05-11-final-goal-roadmap.md`.
- bcrypt Rust crate: <https://docs.rs/bcrypt/latest/bcrypt/>
- `subtle::ConstantTimeEq`: <https://docs.rs/subtle/latest/subtle/trait.ConstantTimeEq.html>
- RustDesk auth model reference: <https://github.com/rustdesk/rustdesk/blob/master/docs/CLI.md>
  (PIN + temp password flow).
