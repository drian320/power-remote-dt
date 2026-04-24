# Phase 2 W3 — manual smoke (user action)

Automated tests green on this branch (12 signaling-client tests incl. W3
E2E, 29 transport tests, 42 protocol tests, etc.). Manual confirmation
with real network verifies hole punching + candidate selection works
end-to-end.

## Same-machine (expected: Host candidate wins via probe)

Same 3-terminal as W2 manual smoke, but observe the probe log lines:

Terminal 1 — signaling server:
```
cargo run -p prdt-signaling-server --release -- --bind 127.0.0.1:8080 --log debug
```

Terminal 2 — host:
```
cargo run -p prdt-host --release -- \
    --bind 127.0.0.1:9000 \
    --signaling-url ws://127.0.0.1:8080/signal \
    --host-id w3-manual \
    --signaling-timeout 60 \
    --stun-url stun://stun.l.google.com:19302
```

Terminal 3 — viewer:
```
cargo run -p prdt-viewer --release -- \
    --signaling-url ws://127.0.0.1:8080/signal \
    --host-id w3-manual \
    --stun-url stun://stun.l.google.com:19302
```

Expected host.log:
```
signaling_rendezvous_completed ... candidate_count=2
probe winner ...
probe selected winner peer_addr=127.0.0.1:<viewer_port>
```

Viewer.log similar. Video flows.

## Cross-network (real NAT traversal)

Two machines, one behind NAT-A, one behind NAT-B. Same commands but host
drops `--bind 127.0.0.1:9000` (defaults to 0.0.0.0:9000). Both machines
need outbound UDP to `stun.l.google.com:19302`.

Expected: probe selects the working path (Srflx for cross-NAT);
connection succeeds within 10 seconds. If both NATs are symmetric, probe
times out — W4 TURN will fix this.
