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
