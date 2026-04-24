# Phase 2 W5 — manual smoke (user action)

Verifies 9-digit ID allocation + pubkey pinning + host-id.txt persistence.

## Terminal 1 — signaling server with SQLite DB
```
cargo run -p prdt-signaling-server --release -- \
    --bind 127.0.0.1:8080 \
    --db /tmp/prdt-w5.sqlite \
    --log debug
```

## Terminal 2 — host (first run, no host-id.txt)
```
rm -f host-id.txt host-key.bin known-host-ids
cargo run -p prdt-host --release -- \
    --bind 127.0.0.1:9000 \
    --signaling-url ws://127.0.0.1:8080/signal \
    --signaling-timeout 60
```
Expected: new `host-key.bin` + `host-id.txt` created; the latter contains a 9-digit dashed ID.

## Terminal 3 — viewer
```
HOST_ID=$(cat host-id.txt)
cargo run -p prdt-viewer --release -- \
    --signaling-url ws://127.0.0.1:8080/signal \
    --host-id "$HOST_ID"
```

Both `--host-id 123-456-789` and `--host-id 123456789` should work (viewer normalizes).

## Reconnection scenario

Stop host (Ctrl-C), restart with the same command. Host reads `host-id.txt`, sends to server, server verifies pubkey matches — re-register succeeds.

## Pubkey mismatch

`mv host-key.bin host-key.bin.bak; rm host-id.txt` and restart host without providing an ID. Server allocates a FRESH ID for the new key. Then `cp host-key.bin.bak host-key.bin` and edit `host-id.txt` to the OLD ID — server returns `HostIdPubkeyMismatch`, host exits.
