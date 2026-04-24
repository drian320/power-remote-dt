# Phase 2 W1 — manual 3-terminal smoke (user action)

Automated tests fully green on this branch. The manual smoke is the final
exit criterion and requires `NV_CODEC_SDK_PATH` + related build env (see
memory `build_env.md`).

## Terminal 1 — signaling server
```
cargo run -p prdt-signaling-server --release -- --bind 127.0.0.1:8080 --log debug
```

## Terminal 2 — host (loopback bind required for same-machine test)
```
cargo run -p prdt-host --release -- --bind 127.0.0.1:9000 --signaling-url ws://127.0.0.1:8080/signal --host-id w1-manual
```

## Terminal 3 — viewer
```
cargo run -p prdt-viewer --release -- --signaling-url ws://127.0.0.1:8080/signal --host-id w1-manual
```

Expected: viewer window opens within 2-3 s of launch and shows the host
screen. Signaling server log shows `register` → `connect` →
`candidate_forwarded` (x2) → `session_completed`. Exit with Esc or window close.

Once confirmed, the `phase2-w1-complete` tag on this branch is fully
validated and may be merged into master.

---

## Results — 2026-04-24 (manual smoke executed)

All W1 signaling-layer objectives verified end-to-end on the user's
RTX 3070 Ti dev machine with `--bind 127.0.0.1:9000` for same-machine
loopback.

Observed timeline (release build, `--signaling-timeout 60`):

```
register → connect → session_start (0.01 s)
signaling_rendezvous_completed (peer_addr=127.0.0.1:<port>, session=UUID)
tofu_first_seen: recorded host pubkey (known-host-ids file created)
Noise handshake complete
Hello/HelloAck complete (session_id=0x6566143f99ce66d3)
host frames_sent ≈ 60 fps (NVENC stable)
viewer frames_received ≈ 60 fps
viewer latency report flowing back to host (bidirectional control ok)
```

W1 signaling-layer PASS. Tag `phase2-w1-complete` is now fully
validated against the real host+viewer video pipeline and may be
merged to master.

### Visual artefacts (NOT W1 — expected known-limitations)

- `textures_decoded ≈ 3 fps` on single-GPU loopback — `known_limitations.md #1`
  (NVENC + MF-decoder + D3D11 render all on the same RTX 3070 Ti).
  Dropping to `--bitrate-mbps 3` improved `present_p95` from 1.15 s to 72 ms
  but decode rate is still GPU-bound. Full rate will appear on a real
  2-machine LAN.
- Viewer window flickers when moved — consequence of the above (winit
  modal move-loop + 3-fps decode amplifies stale-frame presentation).
  Out of scope for W1; revisit in Phase 4 GUI polish.

None of the above touches the W1 signaling / handshake / TOFU /
transport-handoff code paths, which all worked as designed.
