# P6 Manual Smoke Walkthrough

**Branch**: `phase-p6-auth-connection-ux`
**Date**: 2026-05-11
**Purpose**: End-to-end manual verification of auth + connection UX features introduced in P6 (T1-T8).

---

## Prerequisites

- Linux WSLg environment (for host) with `cargo` installed and workspace built.
- Real Wayland machine (for viewer) with `prdt-viewer` binary from GitHub Actions release artifact (PR #6 CI run).
- Signaling server accessible from both machines. Use the existing staging URL from `~/.config/prdt/config.toml`, or start one locally with `cargo run -p prdt-signaling-server`.
- Note the 9-digit host-id after first launch (displayed in GUI title bar or terminal).

Build fresh binaries on the WSLg host:

```bash
cd /home/ubuntu/project/power-remote-dt
cargo build --release -p prdt-gui-host -p prdt-gui-viewer -p prdt-viewer
```

---

## Section A: Linux Smoke (WSLg host + real Wayland viewer)

### A1. Fresh install — onboarding wizard

On WSLg, clean any prior P6 state:

```bash
rm -f ~/.config/prdt/host-auth.toml \
       ~/.config/prdt/host-peers.toml \
       ~/.config/prdt/config.toml
./target/release/prdt-gui-host
```

**Expected**: 5-step wizard appears on first launch.

- **Step 1 (Welcome)**: Host ID (9-digit) is shown. QR code is displayed.
- **Step 2 (AuthMode)**: Select "PIN", click Next.
- **Step 3 (PinSetup)**: Enter `hunter2` in both PIN fields. Click Next.
- **Step 4 (Defaults)**: All 4 permission toggles (Input / Clipboard / File Transfer / Audio) set to ON. Click Finish.

**Verify config written**:

```bash
cat ~/.config/prdt/host-auth.toml
```

Expected content:

```toml
mode = "Pin"
pin_hash = "$2b$12$..."   # bcrypt hash, starts with $2b$12$
default_permissions = { input = true, clipboard = true, file_transfer = true, audio = true }
auto_deny_seconds = 30
```

---

### A2. PIN connection smoke

On the real Wayland machine, run:

```bash
./prdt-viewer --signaling <SIGNALING_URL> --host-id <9-digit> --codec h264 --decoder openh264
```

**Expected sequence**:

1. Viewer terminal shows `HelloRejected { code: PinRequired }`.
2. PIN prompt appears in terminal (or GUI dialog if `--no-auth-prompt` is not set).
3. Enter wrong PIN `wrong1` → error toast/log: `Wrong PIN`.
4. Enter correct PIN `hunter2` → handshake succeeds, video stream starts.

**Verify overlay**:

- Open overlay (ESC key in the viewer window).
- Below the codec line, a Permissions line shows 4 icons: input / clipboard / file-transfer / audio — all white (granted).

**Verify host log**:

```
AuthVerdict::Granted permissions=PermissionSet { input: true, clipboard: true, file_transfer: true, audio: true }
```

---

### A3. Disconnect + reconnect (PIN required every connect)

- Disconnect viewer (Ctrl+C or Disconnect button in overlay).
- Reconnect with the same command.

**Expected**: PIN prompt appears again. PIN mode requires authentication on every connection; there is no "Remember" for PIN.

---

### A4. Permission change smoke

1. In gui-host Settings → Auth section → default_permissions → toggle **Clipboard** to OFF. Click Save.
2. Disconnect viewer if connected.
3. Reconnect viewer and enter PIN `hunter2`.

**Expected**:

- Viewer overlay: clipboard icon (📋) is dark grey (denied).
- On the real Wayland machine: copy text to clipboard on host, attempt to paste in viewer → nothing is pasted (silent drop at channel gate).

---

### A5. Saved peers list smoke

After at least one successful connection:

1. In gui-host → Settings → Saved Peers section.
2. Verify the peer entry shows: pubkey first 12 chars, label (if set), permissions, last seen timestamp.
3. Click **Delete** on the entry.

**Expected**:

- Entry disappears from the UI.
- `~/.config/prdt/host-peers.toml` no longer contains that peer's entry.

```bash
cat ~/.config/prdt/host-peers.toml   # should be empty or missing the deleted entry
```

---

## Section B: Windows Smoke (Windows GUI host + Linux Wayland viewer)

### B1. Setup

1. Download `prdt-gui-host` Windows binary from the GitHub Actions release artifact for PR #6.
2. Run it on the Windows machine.
3. Complete the onboarding wizard — select **TOFU** mode at Step 2, set all permissions ON, click Finish.

### B2. TOFU connection + permission prompt

On the Linux Wayland machine:

```bash
./prdt-viewer --signaling <SIGNALING_URL> --host-id <9-digit> --codec h264 --decoder openh264
```

**Expected on Windows host GUI**:

- Permission prompt modal appears:
  - Title: "New Viewer Connecting"
  - Pubkey: first 12 chars shown (e.g., `ABC12345EFGH`)
  - 4 toggles (Input / Clipboard / File Transfer / Audio) — default values from `default_permissions`.
  - **Remember** checkbox (default: ON).
  - **Allow** and **Deny** buttons. A countdown timer auto-Deny triggers after `auto_deny_seconds` (default 30s).

Actions:

- Toggle **Audio** to OFF.
- Click **Allow**.

**Expected on viewer**:

- Connection succeeds, video stream starts.
- Overlay permissions line: audio icon (🔊) dark grey, other 3 icons white.

### B3. Disconnect + reconnect (TOFU + Remember = auto-accept)

- Disconnect viewer (Ctrl+C).
- Reconnect with the same command.

**Expected**:

- No permission prompt appears on the Windows host (Remember was ON → peer stored).
- Permissions match prior choice: audio still OFF.
- Overlay shows same permission state.

---

## Section C: Hosts List Online Badge Smoke (gui-viewer)

This section tests the 30s OnlineProbe polling and HostKey-based identity selection.

### C1. Setup

- In gui-viewer (Linux or Windows), have 2-3 saved hosts registered in signaling mode with their host IDs.
- Ensure at least one host binary is currently running and connected to the signaling server.

### C2. Online badge appears

1. Open gui-viewer hosts list.
2. Within ~30 seconds, the running host's entry shows a green badge (🟢).
3. Other hosts that are not running show a grey/white badge (⚪).

### C3. Badge clears on shutdown

1. Stop the running host binary.
2. Within ~30 seconds, its badge changes from 🟢 to ⚪.

### C4. Identity-based selection (sort-safe)

1. The hosts list is sorted alphabetically or by last-connected.
2. Click on a host row (e.g., "Home") → the connection form opens with "Home" pre-filled.
3. Verify that clicking "Work" opens the form with "Work" host details — no cross-selection between rows.
4. Re-sort the list (e.g., by adding a new host that would sort before "Home").
5. Click on "Home" again — verify the same host is selected (HostKey-based selection, not position-based).

---

## Section D: Viewer Overlay Permission Line

This section verifies the viewer-overlay permission line that was added under the codec line.

### D1. All permissions granted

- Connect with all permissions ON.
- Open overlay (ESC).
- Below the codec line, verify: 4 icons are displayed in **white** (all granted).
- Icon order: 🖱️ (input) | ⌨️ (keyboard) | 📋 (clipboard) | 🔊 (audio)

### D2. Partial permissions

- Connect with clipboard and audio OFF (configure host default_permissions accordingly).
- Verify: input and keyboard icons white, clipboard and audio icons dark grey.

---

## Known Limitations / Out of Scope for This Smoke

- **Ephemeral mode**: Ephemeral token expiry renewal flow is T7 follow-up; not fully wired end-to-end.
- **Consent prompt channel wire-up**: TOFU consent prompt triggers the permission modal UI; the per-channel enforcement uses the permissions from `KnownPeer` storage. The wire between modal result and real-time channel gating is T7 follow-up.
- **Windows-only features**: Audio gate on Windows requires a Windows test machine; Linux viewer will see silent drop which is correct behaviour.
- **PIN change UI**: Available in Settings but full flow (re-hash + save) is T7 follow-up candidate.

---

## Smoke Result Log (fill in during execution)

| Section | Step | Expected | Actual | Pass/Fail |
|---------|------|----------|--------|-----------|
| A1 | Wizard 5 steps | Shown on fresh launch | | |
| A1 | host-auth.toml written | pin_hash starts with $2b$12$ | | |
| A2 | PinRequired reject | Shown before PIN entry | | |
| A2 | Wrong PIN error | Error shown | | |
| A2 | Correct PIN connect | Video stream starts | | |
| A2 | Overlay all-white | 4 icons white | | |
| A3 | PIN required on reconnect | Prompt appears again | | |
| A4 | Clipboard perm change | Paste silently dropped | | |
| A4 | Overlay clipboard grey | 📋 dark grey | | |
| A5 | Saved peers list | Peer shown after connect | | |
| A5 | Delete peer | Entry removed from UI + TOML | | |
| B2 | TOFU permission prompt | Modal appears | | |
| B2 | Audio OFF granted | 🔊 grey in overlay | | |
| B3 | Auto-accept on reconnect | No prompt, same perms | | |
| C2 | Online badge appears | 🟢 within 30s | | |
| C3 | Badge clears | ⚪ within 30s of host stop | | |
| C4 | Identity-based selection | Correct host on row click | | |
| D1 | All-white overlay | 4 white icons | | |
| D2 | Partial permissions | Grey icons for denied | | |
