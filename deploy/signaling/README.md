# Deploying `prdt-signaling-server`

This directory packages `crates/signaling-server` (binary `prdt-signaling-server`)
as an always-on service: a Docker image + compose file, a systemd unit for
bare-metal/VM hosts, and notes on fronting it with Cloudflare Tunnel later.
Nothing in this directory changes the server's Rust source — see
"Flagged follow-ups" at the bottom for the one behavior a future code change
would need to address.

## What the server actually exposes

Read from `crates/signaling-server/src/main.rs` and `src/lib.rs`:

| Flag                  | Default                    | Meaning                                             |
|------------------------|-----------------------------|------------------------------------------------------|
| `--bind`               | `127.0.0.1:8080`            | TCP listen address (`SocketAddr`, IP **and** port)   |
| `--log`                | `info`                      | `tracing_subscriber` env-filter level                |
| `--session-timeout-ms` | `60000`                     | Viewer/host handshake session inactivity timeout     |
| `--db`                 | `prdt-signaling.sqlite`     | SQLite file path for the host_id ↔ pubkey store      |

There is no config file and no environment-variable input — every setting is
a CLI flag with a default. **The default `--bind 127.0.0.1:8080` is loopback
only**; every artifact in this directory overrides it to `0.0.0.0:8080`
(container-internal, or the interface you choose on bare metal) since a
loopback bind would be unreachable from your VPN/LAN.

Routes (from `src/lib.rs`):
- `GET /health` — JSON `{"hosts": N, "sessions": N}`, used for the Docker
  healthcheck.
- `GET /signal` — the WebSocket rendezvous endpoint. This is the only path
  clients ever connect to; it is not configurable.

There is no TLS in the binary itself (`axum::serve` over a plain
`TcpListener`) — TLS termination is expected to happen in front of it, which
is exactly the Cloudflare Tunnel model described below.

## Docker

```sh
docker compose -f deploy/signaling/docker-compose.yml up -d --build
curl http://127.0.0.1:8080/health
```

- Builds only `prdt-signaling-server` (`cargo build -p prdt-signaling-server
  --release`), not the whole workspace — see `Dockerfile`'s comments for why
  the build context still has to be the repo root.
- The SQLite store lives in the named volume `signaling-data`, mounted at
  `/data`, so `docker compose down` / image rebuilds don't lose registered
  host IDs.
- `PRDT_SIGNALING_PORT` (default `8080`), `PRDT_SIGNALING_BIND_IP` (default
  `0.0.0.0`), `PRDT_LOG_LEVEL` (default `info`), and
  `PRDT_SESSION_TIMEOUT_MS` (default `60000`) are read from a `.env` file
  next to `docker-compose.yml` if present, or your shell environment.
- To bind only your Tailscale interface instead of all interfaces:
  `PRDT_SIGNALING_BIND_IP=100.x.y.z` (see Tailscale section below).

## systemd (no Docker)

See `prdt-signaling-server.service` — it documents the build/install steps
in its header comment. Summary: build the release binary in the repo's dev
container, install it to `/usr/local/bin`, create a dedicated `prdt-signaling`
system user, drop the unit into `/etc/systemd/system/`, then
`systemctl enable --now prdt-signaling-server`. The unit runs with
`ProtectSystem=strict` / `NoNewPrivileges` / a restricted syscall filter,
since the process only needs one listen socket and one SQLite file.

## Tailscale (recommended VPN for the "runs on user's VPN/LAN" constraint)

1. Install Tailscale on the host running `prdt-signaling-server` and join
   your tailnet: `tailscale up`.
2. Find the host's Tailscale IP: `tailscale ip -4`.
3. Bind the server to *that* IP instead of `0.0.0.0` so it's reachable from
   your tailnet but not the public internet:
   - Docker: `PRDT_SIGNALING_BIND_IP=100.x.y.z docker compose -f deploy/signaling/docker-compose.yml up -d`
   - systemd: edit `ExecStart`'s `--bind 100.x.y.z:8080` in the installed unit.
4. Point clients at `ws://100.x.y.z:8080/signal` via `--signaling-url` (see
   below) from any device on the same tailnet.

This is the simplest correct setup today. Cloudflare Tunnel (next section) is
for when you want a stable public hostname instead of a tailnet-only address.

## Cloudflare Tunnel compatibility

The spec constraint is: signaling runs on the VPN/LAN box now, and must
later sit behind a Cloudflare Tunnel reverse proxy **unchanged** (no
Cloudflare Workers rewrite). Findings below are from reading the actual
routing/handshake code, not assumptions.

### Confirmed OK

- **No Host/Origin validation.** `crates/signaling-server/src/lib.rs:32-37`
  builds the `Router` with no `tower_http::cors` layer and no Host-header
  middleware; `crates/signaling-server/src/ws.rs` never inspects the
  incoming request's Host or Origin before upgrading. `cloudflared` rewriting
  the Host header to your tunnel hostname will not be rejected.
- **Single WS path, no base-path assumptions.** Both routes
  (`crates/signaling-server/src/lib.rs:34-35`) are absolute (`/health`,
  `/signal`) with no prefix stripping anywhere in the handlers, so a
  Cloudflare Tunnel `ingress` rule that maps a whole hostname to this origin
  (the standard single-service-per-hostname pattern) needs no path rewriting.
- **Plain-HTTP origin is the expected model.** The server has no TLS of its
  own (`crates/signaling-server/src/main.rs:44-45`, `tokio::net::TcpListener`
  + `axum::serve` with no `rustls`/`native-tls` layer), which matches
  `cloudflared`'s default: it terminates TLS at Cloudflare's edge and
  forwards to the origin over plain `http://` — no origin-side cert needed.
- **WebSocket upgrade itself.** `axum::extract::ws::WebSocketUpgrade`
  (`crates/signaling-server/src/ws.rs:17-22`) is a standard HTTP/1.1
  Upgrade handshake, which `cloudflared` proxies transparently; no special
  ingress config flag is required for the upgrade to succeed.

### Example `cloudflared` ingress config

```yaml
# /etc/cloudflared/config.yml (or wherever cloudflared reads it from)
tunnel: <your-tunnel-id>
credentials-file: /etc/cloudflared/<your-tunnel-id>.json

ingress:
  - hostname: signal.example.com
    service: http://127.0.0.1:8080   # or http://signaling:8080 if cloudflared
                                      # runs in the same compose network —
                                      # see the commented `cloudflared`
                                      # service in docker-compose.yml
  - service: http_status:404
```

Client then connects with `--signaling-url wss://signal.example.com/signal`
(cloudflared upgrades the browser/client-facing `wss://` to plain
`ws://` towards the origin above; the server never sees TLS).

### Flagged follow-up (needs a server code change — not implemented here)

- **No application-level WebSocket keepalive.** Neither `host_loop` nor
  `viewer_loop` (`crates/signaling-server/src/ws.rs:204-248` and `:250-294`)
  ever sends a `Message::Ping`, and the `ClientMessage`/`ServerMessage`
  protocol (`crates/signaling-proto/src/lib.rs:57-90`) has no heartbeat
  variant. A registered host sits idle on its WebSocket between connect
  attempts, sending and receiving nothing. Reverse proxies — Cloudflare
  Tunnel included — commonly drop WebSocket connections that go idle past
  some timeout window. Today, on the bare VPN/LAN deployment, the OS TCP
  stack keeps the idle connection alive indefinitely, so this hasn't been a
  problem; once Cloudflare Tunnel is in front of it, a long-idle registered
  host could get silently disconnected and have to re-register. Fixing this
  properly means adding a periodic `Message::Ping` (or an app-level
  heartbeat `ClientMessage`/`ServerMessage` variant) inside the two
  `tokio::select!` loops in `ws.rs` — an actual behavior change to
  `crates/signaling-server/src/ws.rs`, which is out of scope for this
  packaging task. Flagging for a follow-up ticket before (or shortly after)
  the Cloudflare Tunnel cutover.

## Pointing a `prdt` client at this signaling server

Both the host and viewer binaries (`crates/host`, `crates/viewer`, and the
unified `prdt-client`/GUI built on top of them) take the same flag:

```sh
prdt-host   --signaling-url ws://100.x.y.z:8080/signal   # or wss://signal.example.com/signal
prdt-viewer --signaling-url ws://100.x.y.z:8080/signal --host-id 123-456-789
```

- `--host-id` is required on the viewer side (which host to rendezvous
  with); on the host side it's optional — omit it on first run and the
  server allocates a fresh 9-digit ID, returned in the `Registered` message
  and persisted to the host's `host-id.txt` (see `--host-id-file`) for reuse
  on subsequent runs.
- The GUI apps persist this as `signaling_url` under `[host]` / `[viewer]` in
  their TOML config (`crates/gui-common/src/config.rs`) instead of requiring
  the flag every launch — set it once in the config file and it behaves as
  if `--signaling-url` were passed each time (CLI flag still wins if both
  are set).
- The scheme (`ws://` vs `wss://`) is not validated by the client beyond
  what `tokio-tungstenite::connect_async` accepts
  (`crates/signaling-client/src/rendezvous.rs:33`) — use `wss://` once the
  server is behind Cloudflare Tunnel (or any TLS-terminating proxy), `ws://`
  for direct VPN/LAN access today.

## What was intentionally NOT done here

- No Rust source in `crates/signaling-server/**` (or anywhere else) was
  modified — see the flagged follow-up above for the one thing that would
  require it.
- `cargo build --workspace` / the dev container were not run from this task,
  to avoid racing another agent's build; verification of the Rust build
  itself is owned by a separate verification pass.
