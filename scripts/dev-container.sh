#!/usr/bin/env bash
# Run cargo (or any command) inside a Debian bookworm container with
# libpipewire-0.3-dev + libspa-0.2-dev installed.
#
# Usage:
#   scripts/dev-container.sh cargo build -p prdt-media-linux --target x86_64-unknown-linux-gnu
#   scripts/dev-container.sh cargo test --workspace --lib --target x86_64-unknown-linux-gnu
#   scripts/dev-container.sh bash   # interactive shell
#
# Rationale: P5B-1 T5/T6 wires pipewire 0.9, which requires libpipewire-0.3
# >= 0.3.55. Ubuntu 22.04 (jammy) ships 0.3.48 and cannot build the crate;
# Debian bookworm ships 0.3.65 and works. The container is mounted at /work
# and inherits the host user so files written inside are owned correctly.
# Build artifacts go to target-docker/ so they don't conflict with any
# host-side target/ tree.
#
# First run pulls the rust:1-bookworm image (~600 MB) and runs apt-get
# install; subsequent runs reuse the image. A named volume preserves
# cargo's registry cache across invocations so cold rebuilds stay fast.

set -euo pipefail

WORKDIR="/work"

# Build a small derived image once per host so we don't apt-install on
# every invocation. The Dockerfile lives in scripts/.
TAG="prdt-dev:bookworm"
if ! docker image inspect "$TAG" >/dev/null 2>&1; then
    docker build -t "$TAG" -f scripts/Dockerfile.dev scripts/
fi

# Cargo's registry cache + target/ both go under target-docker/ so they're
# owned by the host user (the container runs as that user via --user) and
# we don't need a docker named-volume that gets root-owned on creation.
mkdir -p target-docker/cargo-home

TTY_FLAGS=()
if [ -t 0 ] && [ -t 1 ]; then
    TTY_FLAGS=(-it)
fi

# rusty_ffmpeg 0.13.0's build script (used by crates/media-ffmpeg when the
# `ffmpeg-encode-hevc-vaapi` feature is active) reads these to invoke
# bindgen against the apt-installed FFmpeg headers and to set the rustc
# link-search path for libavcodec.so. The paths are the standard Debian
# bookworm multiarch locations (libavcodec-dev installs them inside the
# image — see scripts/Dockerfile.dev). They are harmless when the feature
# is off (cargo ignores env vars not read by any build script).
FFMPEG_INCLUDE_DIR="${FFMPEG_INCLUDE_DIR:-/usr/include/x86_64-linux-gnu}"
FFMPEG_DLL_PATH="${FFMPEG_DLL_PATH:-/usr/lib/x86_64-linux-gnu/libavcodec.so}"

exec docker run --rm "${TTY_FLAGS[@]}" \
    --user "$(id -u):$(id -g)" \
    -v "$(pwd):$WORKDIR" \
    -w "$WORKDIR" \
    -e CARGO_HOME="$WORKDIR/target-docker/cargo-home" \
    -e CARGO_TARGET_DIR="$WORKDIR/target-docker" \
    -e FFMPEG_INCLUDE_DIR="$FFMPEG_INCLUDE_DIR" \
    -e FFMPEG_DLL_PATH="$FFMPEG_DLL_PATH" \
    "$TAG" \
    "$@"
