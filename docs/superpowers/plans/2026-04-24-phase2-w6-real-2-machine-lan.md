# Phase 2 W6 — Real 2-machine LAN Smoke (findings)

**Date**: 2026-04-24
**Setup**: Same-LAN 2-machine (Machine A: RTX 3070 Ti + GTX 1080, 192.168.100.101 / Machine B: laptop with GTX 1080, 192.168.100.127)
**Status**: Connected end-to-end, frames flow.

---

## What worked

- W5 server-side 9-digit ID allocation + SQLite persistence
- Signaling rendezvous across LAN (2 machines, same subnet)
- Host's Host candidate (LAN IP)  + viewer's Host candidate (LAN IP) exchanged via signaling
- Probe RTT succeeded once both sides were reachable
- Noise handshake complete
- Hello/HelloAck negotiation complete (session_id 0x...)
- Host sustains 60 fps NVENC sends to viewer
- Viewer decodes + presents (subject to MF decoder rate)

## Blockers encountered + fixes

### 1. Viewer hard-coded bind 127.0.0.1:0 in signaling mode

**Symptom**: viewer's Host candidate carried `127.0.0.1:<ephemeral>`; host's probe failed with `AddrNotAvailable (code 10049)` trying to send to it from the LAN interface. Viewer's probe failed with `NetworkUnreachable (code 10051)` because loopback-bound socket can't send to LAN IPs.

**Fix**: commit `49bb3e7` on `phase2-w6-lan-fixes` branch
- Added `--bind` CLI flag (default `0.0.0.0:0`)
- Auto-detects outbound interface IP when bind is wildcard, by connecting a temp UDP socket to the signaling server's addr and reading `local_addr`. Used as the Host candidate.

### 2. Machine B Windows Firewall blocks unsolicited inbound UDP

**Symptom**: viewer's probe reached host → host echoed ProbeAck → viewer committed successfully. But host's probe to viewer's ephemeral port was dropped by Machine B's Windows Firewall (default block for unregistered apps). Host timed out; viewer's subsequent Noise packets also never arrived inbound.

**Fix (user action)**:
```powershell
# On Machine B, as administrator:
New-NetFirewallRule -DisplayName "prdt-viewer-w6" -Direction Inbound -Program "C:\Users\user\Downloads\prdt-viewer.exe" -Action Allow -Profile Private,Domain
```

Alternatively: approve the Windows Firewall dialog on first run.

## Observed performance (Machine A host → Machine B viewer, 30 Mbps, 1080p60 H.265)

```
host:
  frames_sent avg  ≈ 60 fps, send_errors 0
  occasional bursty pauses (1987 → 1992 over 2 seconds) — likely FEC queue fill

viewer:
  frames_received  ≈ 60 fps incoming
  textures_decoded ≈ 3 fps (MF decoder rate, similar to W1-W5 loopback)
  decode_p95       = 40 ms
  present_p50      = 27 ms
  present_p95      = 79 ms
  present_p99      = 91 ms
```

Decode bottleneck is likely HEVC MF decoder on Machine B's GTX 1080 under load; not a W6 signaling/transport regression. Possible mitigations:
- `--bitrate-mbps 5` on host side reduces bitstream complexity, letting decoder keep up
- Try `--decoder nvdec` on viewer side (currently slower per memory `known_limitations.md` §2d, but worth trying with real network path)
- Verify HEVC Video Extensions are the paid AppX variant on Machine B (ProductId 9NMZLZ57R3T7)

## Recommended follow-up code changes

1. **Probe retry** — current `probe_and_commit_peer` sends one Probe per candidate and waits 10s for Ack. Real-world firewalls often drop the first unsolicited packet before stateful tracking kicks in. Resending the Probe 5× at 200ms intervals would mask transient single-packet drops without increasing the commit latency for the happy path.

2. **Host-side auto-detect** — host currently requires user to pass `--bind <LAN_IP>:9000` explicitly. Apply the same `discover_outbound_ip(signaling_url)` trick that viewer now uses, so `--bind 0.0.0.0:9000` (the default) can resolve to the right interface automatically.

3. **Firewall rule auto-install** — host and viewer binaries could self-install inbound rules via `netsh advfirewall` on first run (opt-in flag `--self-register-firewall`). Privileged elevation required.

4. **UPnP IGD / NAT-PMP** for actual cross-NAT (outside W6 scope, more useful for public Internet E2E with no STUN/TURN).

## Command reference (for repeat tests)

### Machine A (host + server)

```powershell
# One-time admin firewall setup
New-NetFirewallRule -DisplayName "prdt-signaling" -Direction Inbound -Program "E:\project\rust-desktop\power-remote-dt\target\release\prdt-signaling-server.exe" -Action Allow -Profile Private,Domain
New-NetFirewallRule -DisplayName "prdt-host"      -Direction Inbound -Program "E:\project\rust-desktop\power-remote-dt\target\release\prdt-host.exe"      -Action Allow -Profile Private,Domain

# Terminal 1 — signaling server
cd E:\project\rust-desktop\power-remote-dt
Remove-Item -ErrorAction SilentlyContinue prdt-w6.sqlite, signaling.log
.\target\release\prdt-signaling-server.exe --bind 0.0.0.0:8080 --db prdt-w6.sqlite --log debug 2>&1 | Tee-Object signaling.log

# Terminal 2 — host (LAN IP required for Host candidate)
Remove-Item -ErrorAction SilentlyContinue host-id.txt, known-host-ids, host.log
.\target\release\prdt-host.exe --bind 192.168.100.101:9000 --signaling-url ws://192.168.100.101:8080/signal --signaling-timeout 60 2>&1 | Tee-Object host.log
```

Read allocated ID once `host.log` shows `signaling_rendezvous_completed ... host_id=XXX-XXX-XXX` (or earlier from `signaling.log` via `Select-String -Path signaling.log -Pattern "register"`).

### Machine B (viewer)

```powershell
# One-time admin firewall setup
New-NetFirewallRule -DisplayName "prdt-viewer" -Direction Inbound -Program "C:\Users\user\Downloads\prdt-viewer.exe" -Action Allow -Profile Private,Domain

# Copy latest prdt-viewer.exe from Machine A's target\release
# Then:
cd C:\Users\user\Downloads
$HOST_ID = "XXX-XXX-XXX"  # from Machine A's host-id.txt
.\prdt-viewer.exe --signaling-url ws://192.168.100.101:8080/signal --host-id $HOST_ID 2>&1 | Tee-Object viewer.log
```

If the auto-detect picks an unexpected interface (e.g. Hyper-V virtual NIC instead of physical Ethernet), override with `--bind <desired_LAN_IP>:0`.

## W6 verdict

**Phase 2 signaling/transport architecture works end-to-end on real 2-machine LAN.** The remaining friction (firewall rule + binary copy) is typical of any peer-to-peer LAN tool. Probe retry and host auto-detect are nice-to-haves that would reduce the operator burden.
