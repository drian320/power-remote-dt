# Phase 2 W4 — manual smoke (user action)

Automated tests cover Allocate + CreatePermission + Send/Data Indication
round-trip against an in-process mock TURN server, plus Relay candidate
propagation through signaling. Full probe-over-TURN and Noise-over-TURN
data-plane is future work. This doc describes the smoke against a real
TURN server (coturn).

## Spin up coturn locally

```bash
docker run -d -p 3478:3478 -p 3478:3478/udp \
  coturn/coturn \
  -n --lt-cred-mech --fingerprint \
  --user=prdt:prdt --realm=prdt-test
```

## Terminal 1 — signaling server

```
cargo run -p prdt-signaling-server --release -- --bind 127.0.0.1:8080 --log debug
```

## Terminal 2 — host (uses TURN relay for transport)

```
cargo run -p prdt-host --release -- \
    --bind 127.0.0.1:9000 \
    --signaling-url ws://127.0.0.1:8080/signal \
    --host-id w4-manual \
    --signaling-timeout 60 \
    --turn-url turn://prdt:prdt@127.0.0.1:3478
```

## Terminal 3 — viewer

```
cargo run -p prdt-viewer --release -- \
    --signaling-url ws://127.0.0.1:8080/signal \
    --host-id w4-manual \
    --turn-url turn://prdt:prdt@127.0.0.1:3478
```

## Expected

- Host + viewer logs show `relay candidate sent` with a real relayed addr
  allocated by coturn
- signaling log shows `candidate_forwarded` for Relay type (previously
  rejected; fixed in W4)
- Transport constructed via `bind_with_relay`, so all send_to/recv_from
  go through TURN Send/Data Indications
- Noise handshake + Hello/HelloAck complete
- Video flows via the TURN relay

## Known W4 caveats

- Refresh NOT implemented — allocations expire after 10 minutes (coturn
  default).
- ChannelBind NOT implemented — traffic uses Send Indications (per-packet
  overhead ~36 bytes STUN header + 4 bytes XOR-PEER-ADDRESS TLV + 4 bytes
  DATA TLV header).
- Only one TURN server (both peers use same URL) tested.
- Probe over TURN not exercised in automated tests; real cross-NAT
  scenario depends on CreatePermission being issued for the correct peer
  BEFORE probe sends traffic. host/viewer bins currently call
  ensure_permission via TurnRelaySocket::send_to indirectly — but this
  adds 1 RTT per unique peer.
