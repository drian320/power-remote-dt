# Phase 2 W2 — manual smoke (user action, real Internet)

Automated tests + mock STUN fully green on this branch. Manual confirmation
with a real public STUN server verifies the srflx learning against a live
RFC 5389 implementation.

## Prerequisites

- Same as W1 (NV_CODEC_SDK_PATH etc.)
- Outbound UDP to `stun.l.google.com:19302` permitted

## Terminal 1 — signaling server
```
cargo run -p prdt-signaling-server --release -- --bind 127.0.0.1:8080 --log debug
```

## Terminal 2 — host (LAN-bind for same-machine test)
```
cargo run -p prdt-host --release -- \
    --bind 127.0.0.1:9000 \
    --signaling-url ws://127.0.0.1:8080/signal \
    --host-id w2-manual \
    --signaling-timeout 60 \
    --stun-url stun://stun.l.google.com:19302
```

## Terminal 3 — viewer
```
cargo run -p prdt-viewer --release -- \
    --signaling-url ws://127.0.0.1:8080/signal \
    --host-id w2-manual \
    --stun-url stun://stun.l.google.com:19302
```

## Expected

- Both host and viewer log `srflx candidate sent` with a real public IP:port
- Signaling server log shows multiple `candidate_forwarded` events per side
  (1 Host, 1 Srflx each)
- Noise + Hello/HelloAck complete as in W1
- Video flows (subject to the same single-GPU loopback limits as W1)

## Known W2 caveat

The srflx port in the candidate is NOT the port the main UDP transport
uses (STUN probe is on a separate socket, bound to `0.0.0.0:0`). Therefore
a real remote peer would see the srflx IP correctly but the port would
not map to host's actual media socket. This is intentional for W2; W3
fixes it by sharing the transport socket with STUN.

Once confirmed, tag `phase2-w2-complete` is fully validated against a
live STUN server.
