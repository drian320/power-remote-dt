# Deployment image for prdt-signaling-server (crates/signaling-server).
#
# Build context MUST be the repository root (not this directory), because
# this is a Cargo workspace and cargo needs every member crate's Cargo.toml
# present to resolve the workspace even though we only compile one binary:
#
#   docker build -f deploy/signaling/Dockerfile -t prdt-signaling-server .
#
# (docker-compose.yml in this directory sets `context: ../..` for you.)

# ---- builder ----------------------------------------------------------
FROM rust:1-bookworm AS builder

# rusqlite is used with the "bundled" feature (see workspace Cargo.toml),
# so SQLite is compiled from vendored C source into the binary rather than
# linked against a system libsqlite3 — build-essential is required, but the
# runtime image below needs no SQLite package at all.
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends \
      build-essential \
      pkg-config \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the whole workspace manifest set. Only prdt-signaling-server and its
# path-dependency prdt-signaling-proto are actually compiled (via `-p`), but
# `cargo build -p` still needs every workspace member's Cargo.toml on disk
# to resolve the workspace, so the full crates/ tree must be present.
COPY Cargo.toml ./Cargo.toml
COPY crates ./crates

RUN cargo build --release --target x86_64-unknown-linux-gnu \
      -p prdt-signaling-server

# ---- runtime ------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# curl is only for the HEALTHCHECK below (GET /health); the server itself
# makes no outbound network calls and needs no other runtime packages —
# rusqlite's "bundled" SQLite is statically compiled into the binary.
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends \
      curl \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --create-home --home-dir /data --shell /usr/sbin/nologin prdt

COPY --from=builder /app/target/x86_64-unknown-linux-gnu/release/prdt-signaling-server /usr/local/bin/prdt-signaling-server

USER prdt
WORKDIR /data
VOLUME ["/data"]

EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD curl -fsS http://127.0.0.1:8080/health || exit 1

ENTRYPOINT ["/usr/local/bin/prdt-signaling-server"]
# Default bind is 0.0.0.0 (container-internal) — the crate's own CLI default
# of 127.0.0.1:8080 would be unreachable from outside the container.
CMD ["--bind", "0.0.0.0:8080", "--db", "/data/prdt-signaling.sqlite"]
