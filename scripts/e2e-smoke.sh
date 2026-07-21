#!/usr/bin/env bash
# Automated E2E smoke test for `prdt` -- loopback (single machine) or
# two-box (Win<->Linux) host/viewer.
#
# Starts `prdt host` (and, in loopback mode, `prdt connect` too), waits for
# the viewer to report a decoded frame, and exits 0/non-zero accordingly.
# Distinguishes several failure states instead of a single pass/fail bit:
#
#   exit 0  PASS  -- viewer decoded at least one frame (textures_decoded > 0
#                    in a "viewer rx stats" log line).
#   exit 1  FAIL  -- Noise handshake completed and packets arrived at the
#                    viewer, but no frame was ever decoded.
#   exit 2  FAIL  -- Noise handshake completed but the viewer received zero
#                    video packets (loopback mode: the host itself never
#                    logged "first frame ready" -- almost always a
#                    screen-capture problem on the host, e.g. no reachable
#                    xdg-desktop-portal / X11 display).
#   exit 3  FAIL  -- the viewer's decoder backend failed to initialize (the
#                    resolved --decoder names a backend this build lacks).
#   exit 4  FAIL  -- the viewer never reported a completed Noise handshake.
#   exit 5  FAIL  -- the host process crashed on startup (loopback/host modes).
#
# The decode marker is viewer-side (crates/viewer/src/lib.rs ~2308-2329): the
# recv loop logs "viewer rx stats" roughly once a second with a
# textures_decoded=N field that increments on every frame successfully
# decoded and published to the renderer. The host-side "first frame ready"
# line (crates/host/src/lib.rs:1177) only proves capture+encode worked; this
# script uses it as a secondary signal to distinguish exit 1 vs exit 2 in
# loopback mode.
#
# NOTE on --decoder/--encoder/--codec: this script always passes these
# explicitly. `prdt` overlays a per-user config.toml (default
# ~/.config/prdt/config.toml on Linux) onto any CLI flag left unset -- on a
# machine where the GUI launcher has run interactively, that file can carry
# a stale decoder/encoder choice from a previous session, and if it names a
# backend this build doesn't have compiled in, startup hard-errors purely
# because the flag was left unset. Passing the flags explicitly sidesteps
# that trap.
#
# Usage:
#   scripts/e2e-smoke.sh loopback [options]
#   scripts/e2e-smoke.sh host [options]
#   scripts/e2e-smoke.sh viewer [options]
#
# Options (all optional; see defaults below):
#   --bin-path <path>       prdt binary (auto-detected from .smoke-artifacts/linux
#                            or smoke/ if omitted)
#   --port <n>               default 19000
#   --bind-host <ip>        default 127.0.0.1 (loopback) / 0.0.0.0 (host mode)
#   --peer-addr <ip:port>   viewer mode: remote host's address
#   --peer-pubkey <b64>     viewer mode: remote host's pubkey
#   --host-id <id>          TODO(signaling): Task #2 ID/PIN provisioning path
#   --pin <pin>             PIN auth (host and/or viewer)
#   --signaling-url <url>   TODO(signaling): Task #2 ID/PIN provisioning path
#   --encoder <name>        host --encoder, default auto
#   --decoder <name>        viewer --decoder, default auto
#   --codec <name>          viewer --codec, default auto
#   --bitrate-mbps <n>      default 30
#   --warmup-sec <n>        loopback: wait for host pubkey, default 5
#   --connect-sec <n>       wait for decode marker, default 20
#   --no-silent-allow       do not pass --silent-allow to the host
#   --out-dir <dir>         default .smoke-artifacts/e2e-<timestamp>
#
# Examples:
#   scripts/e2e-smoke.sh loopback --bin-path smoke/prdt-linux-x86_64
#   scripts/e2e-smoke.sh host --port 9000
#   scripts/e2e-smoke.sh viewer --peer-addr 192.168.1.20:9000 --peer-pubkey <b64>
set -uo pipefail

MODE="${1:-}"
shift || true
case "$MODE" in
    loopback|host|viewer) ;;
    -h|--help|"") sed -n '2,58p' "$0"; exit 0 ;;
    *) echo "FAIL: unknown mode '$MODE' (expected loopback|host|viewer)" >&2; exit 1 ;;
esac

BIN_PATH=""
PORT=19000
BIND_HOST=""
PEER_ADDR=""
PEER_PUBKEY=""
HOST_ID=""
PIN=""
SIGNALING_URL=""
ENCODER="auto"
DECODER="auto"
CODEC="auto"
BITRATE_MBPS=30
WARMUP_SEC=5
CONNECT_SEC=20
SILENT_ALLOW=1
OUT_DIR=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --bin-path) BIN_PATH="$2"; shift 2 ;;
        --port) PORT="$2"; shift 2 ;;
        --bind-host) BIND_HOST="$2"; shift 2 ;;
        --peer-addr) PEER_ADDR="$2"; shift 2 ;;
        --peer-pubkey) PEER_PUBKEY="$2"; shift 2 ;;
        --host-id) HOST_ID="$2"; shift 2 ;;
        --pin) PIN="$2"; shift 2 ;;
        --signaling-url) SIGNALING_URL="$2"; shift 2 ;;
        --encoder) ENCODER="$2"; shift 2 ;;
        --decoder) DECODER="$2"; shift 2 ;;
        --codec) CODEC="$2"; shift 2 ;;
        --bitrate-mbps) BITRATE_MBPS="$2"; shift 2 ;;
        --warmup-sec) WARMUP_SEC="$2"; shift 2 ;;
        --connect-sec) CONNECT_SEC="$2"; shift 2 ;;
        --no-silent-allow) SILENT_ALLOW=0; shift ;;
        --out-dir) OUT_DIR="$2"; shift 2 ;;
        *) echo "FAIL: unknown option: $1" >&2; exit 1 ;;
    esac
done

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

resolve_bin() {
    if [[ -n "$BIN_PATH" ]]; then
        [[ -f "$BIN_PATH" ]] || { echo "FAIL: BinPath not found: $BIN_PATH" >&2; exit 1; }
        echo "$(cd "$(dirname "$BIN_PATH")" && pwd)/$(basename "$BIN_PATH")"
        return
    fi
    for c in ".smoke-artifacts/linux/prdt-linux-x86_64" "smoke/prdt-linux-x86_64"; do
        if [[ -f "$c" ]]; then echo "$(cd "$(dirname "$c")" && pwd)/$(basename "$c")"; return; fi
    done
    echo "FAIL: no prdt binary found. Pass --bin-path, or run scripts/fetch-ci-artifacts.sh first." >&2
    exit 1
}
PRDT="$(resolve_bin)"
chmod +x "$PRDT" 2>/dev/null || true
echo "Binary: $PRDT"

if [[ -z "$OUT_DIR" ]]; then
    OUT_DIR=".smoke-artifacts/e2e-$(date +%Y%m%d-%H%M%S)"
fi
mkdir -p "$OUT_DIR"
OUT_DIR="$(cd "$OUT_DIR" && pwd)"
echo "Work dir: $OUT_DIR"

export RUST_LOG=info

strip_ansi() { sed -E 's/\x1b\[[0-9;]*m//g'; }

# Prints "HANDSHAKE_OK FRAMES_RECEIVED TEXTURES_DECODED DECODER_INIT_FAIL"
# (space-joined; DECODER_INIT_FAIL is the offending line, or "-").
decode_verdict() {
    local viewer_log="$1"
    [[ -f "$viewer_log" ]] || { echo "0 0 0 -"; return; }
    local clean handshake_ok=0 frames=0 textures=0 fail="-"
    clean="$(strip_ansi < "$viewer_log")"
    grep -q 'Noise handshake complete' <<<"$clean" && handshake_ok=1
    frames="$(grep 'viewer rx stats' <<<"$clean" | grep -oE 'frames_received=[0-9]+' | grep -oE '[0-9]+' | sort -n | tail -1)"
    textures="$(grep 'viewer rx stats' <<<"$clean" | grep -oE 'textures_decoded=[0-9]+' | grep -oE '[0-9]+' | sort -n | tail -1)"
    local fail_line
    fail_line="$(grep -m1 'decoder backend init failed' <<<"$clean" || true)"
    [[ -n "$fail_line" ]] && fail="$fail_line"
    echo "${handshake_ok} ${frames:-0} ${textures:-0} ${fail}"
}

host_first_frame_ms() {
    local host_log="$1"
    [[ -f "$host_log" ]] || return
    strip_ansi < "$host_log" | grep -m1 'first frame ready' | grep -oE 'elapsed_ms=[0-9]+' | grep -oE '[0-9]+'
}

# Prints the verdict text + returns the exit code via $?
write_verdict() {
    local handshake_ok="$1" frames="$2" textures="$3" fail="$4" first_frame_ms="$5"; shift 5
    local logs="$*"
    echo ""
    if [[ "$textures" -gt 0 ]]; then
        echo "PASS: viewer decoded ${textures} frame(s) (frames_received=${frames})."
        [[ -n "$first_frame_ms" ]] && echo "  host first-frame latency: ${first_frame_ms}ms"
        echo "  logs: $logs"
        return 0
    fi
    if [[ "$fail" != "-" ]]; then
        echo "FAIL (decoder init): $fail"
        echo "  The resolved --decoder names a backend this build lacks. Try --decoder mf or --decoder auto."
        echo "  logs: $logs"
        return 3
    fi
    if [[ "$handshake_ok" -eq 0 ]]; then
        echo "FAIL (connect): viewer never logged 'Noise handshake complete'."
        echo "  logs: $logs"
        return 4
    fi
    if [[ "$frames" -gt 0 ]]; then
        echo "FAIL (no decode): viewer received ${frames} video packet(s) but decoded none."
        echo "  Likely a codec/decoder mismatch or decode-path bug, not a network issue."
        echo "  logs: $logs"
        return 1
    fi
    if [[ -n "$first_frame_ms" ]]; then
        echo "FAIL (no packets): host captured+encoded a frame (first-frame ${first_frame_ms}ms) but zero video packets reached the viewer."
        echo "  Likely a network/transport issue between host and viewer."
        echo "  logs: $logs"
        return 2
    fi
    echo "FAIL (no capture): connected, but the host never captured a first frame ('first frame ready' absent from host log)."
    echo "  This usually means desktop/screen capture is producing nothing in this environment"
    echo "  (e.g. no reachable xdg-desktop-portal on Wayland, or no X11 display)."
    echo "  logs: $logs"
    return 2
}

lan_ipv4() {
    ip route get 1.1.1.1 2>/dev/null | grep -oE 'src [0-9.]+' | awk '{print $2}' || echo "<THIS-HOST-IP>"
}

case "$MODE" in
loopback)
    [[ -n "$BIND_HOST" ]] || BIND_HOST="127.0.0.1"
    BIND_ADDR="${BIND_HOST}:${PORT}"
    HOST_LOG="$OUT_DIR/host.log"
    VIEWER_LOG="$OUT_DIR/viewer.log"

    HOST_ARGS=(host --bind "$BIND_ADDR" --headless --encoder "$ENCODER"
        --bitrate-mbps "$BITRATE_MBPS"
        --key-file "$OUT_DIR/host-key.bin"
        --known-peers-file "$OUT_DIR/known-peer-ids"
        --host-auth-file "$OUT_DIR/host-auth.toml"
        --host-peers-file "$OUT_DIR/host-peers.toml")
    [[ "$SILENT_ALLOW" -eq 1 ]] && HOST_ARGS+=(--silent-allow)
    [[ -n "$PIN" ]] && HOST_ARGS+=(--pin "$PIN")

    echo "Starting host: $BIND_ADDR"
    "$PRDT" "${HOST_ARGS[@]}" >"$HOST_LOG" 2>"$HOST_LOG.err" &
    HOST_PID=$!

    cleanup() {
        [[ -n "${VIEWER_PID:-}" ]] && kill "$VIEWER_PID" 2>/dev/null
        kill "$HOST_PID" 2>/dev/null
        wait 2>/dev/null
    }
    trap cleanup EXIT

    PUBKEY=""
    deadline=$((SECONDS + WARMUP_SEC))
    while [[ $SECONDS -lt $deadline && -z "$PUBKEY" ]]; do
        sleep 0.3
        if ! kill -0 "$HOST_PID" 2>/dev/null; then break; fi
        PUBKEY="$(grep -oE 'Host public key: [^ ]+' "$HOST_LOG" 2>/dev/null | head -1 | awk '{print $4}')"
    done
    if ! kill -0 "$HOST_PID" 2>/dev/null; then
        echo "FAIL (host crashed on startup). Log:"
        cat "$HOST_LOG" 2>/dev/null
        exit 5
    fi
    [[ -n "$PUBKEY" ]] || { echo "FAIL: host did not print its public key within ${WARMUP_SEC}s; see $HOST_LOG" >&2; exit 5; }
    echo "Host pubkey: $PUBKEY"

    VIEWER_ARGS=(connect --host "$BIND_ADDR" --host-pubkey "$PUBKEY" --headless
        --decoder "$DECODER" --codec "$CODEC" --bitrate-mbps "$BITRATE_MBPS"
        --viewer-key-file "$OUT_DIR/viewer-key.bin"
        --recv-dir "$OUT_DIR/received")
    [[ -n "$PIN" ]] && VIEWER_ARGS+=(--pin "$PIN")

    echo "Starting viewer -> $BIND_ADDR"
    "$PRDT" "${VIEWER_ARGS[@]}" >"$VIEWER_LOG" 2>"$VIEWER_LOG.err" &
    VIEWER_PID=$!

    deadline=$((SECONDS + CONNECT_SEC))
    read -r H F T FAIL_LINE <<<"$(decode_verdict "$VIEWER_LOG")"
    while [[ $SECONDS -lt $deadline && "$T" -eq 0 && "$FAIL_LINE" == "-" ]]; do
        sleep 1
        read -r H F T FAIL_LINE <<<"$(decode_verdict "$VIEWER_LOG")"
    done

    FIRST_FRAME_MS="$(host_first_frame_ms "$HOST_LOG")"
    write_verdict "$H" "$F" "$T" "$FAIL_LINE" "$FIRST_FRAME_MS" "$HOST_LOG" "$VIEWER_LOG"
    exit $?
    ;;

host)
    [[ -n "$BIND_HOST" ]] || BIND_HOST="0.0.0.0"
    BIND_ADDR="${BIND_HOST}:${PORT}"
    HOST_LOG="$OUT_DIR/host.log"

    HOST_ARGS=(host --bind "$BIND_ADDR" --headless --encoder "$ENCODER"
        --bitrate-mbps "$BITRATE_MBPS"
        --key-file "$OUT_DIR/host-key.bin"
        --known-peers-file "$OUT_DIR/known-peer-ids"
        --host-auth-file "$OUT_DIR/host-auth.toml"
        --host-peers-file "$OUT_DIR/host-peers.toml")
    [[ "$SILENT_ALLOW" -eq 1 ]] && HOST_ARGS+=(--silent-allow)
    [[ -n "$PIN" ]] && HOST_ARGS+=(--pin "$PIN")
    if [[ -n "$SIGNALING_URL" ]]; then
        # TODO(signaling): Task #2 ID/PIN provisioning path -- lets the host
        # register under --host-id instead of requiring a manually-shared
        # IP:port. Wired but untested here (no signaling server reachable in
        # this environment when this was written).
        HOST_ARGS+=(--signaling-url "$SIGNALING_URL")
        [[ -n "$HOST_ID" ]] && HOST_ARGS+=(--host-id "$HOST_ID")
    fi

    echo "Starting host on $BIND_ADDR. Ctrl+C to stop."
    "$PRDT" "${HOST_ARGS[@]}" >"$HOST_LOG" 2>"$HOST_LOG.err" &
    HOST_PID=$!
    echo "PID: $HOST_PID  log: $HOST_LOG"
    trap 'kill $HOST_PID 2>/dev/null; wait 2>/dev/null' EXIT

    PRINTED=0
    LAST_LINES=0
    while kill -0 "$HOST_PID" 2>/dev/null; do
        sleep 0.5
        [[ -f "$HOST_LOG" ]] || continue
        TOTAL_LINES=$(wc -l < "$HOST_LOG")
        if [[ "$TOTAL_LINES" -gt "$LAST_LINES" ]]; then
            tail -n "+$((LAST_LINES + 1))" "$HOST_LOG"
            LAST_LINES="$TOTAL_LINES"
        fi
        if [[ "$PRINTED" -eq 0 ]]; then
            PUBKEY="$(grep -oE 'Host public key: [^ ]+' "$HOST_LOG" 2>/dev/null | head -1 | awk '{print $4}')"
            if [[ -n "$PUBKEY" ]]; then
                LAN_IP="$(lan_ipv4)"
                echo ""
                echo "===================================================================="
                echo "Paste on the VIEWER box (verify ${LAN_IP} is the right reachable IP):"
                echo ""
                echo "  Linux viewer:"
                echo "    ./scripts/e2e-smoke.sh viewer --peer-addr ${LAN_IP}:${PORT} --peer-pubkey $PUBKEY"
                echo ""
                echo "  Windows viewer:"
                echo "    ./scripts/e2e-smoke.ps1 -Mode viewer -PeerAddr ${LAN_IP}:${PORT} -PeerPubkey $PUBKEY"
                echo "===================================================================="
                echo ""
                PRINTED=1
            fi
        fi
    done
    echo "Host process exited."
    ;;

viewer)
    have_direct=0; have_signaling=0
    [[ -n "$PEER_ADDR" ]] && have_direct=1
    [[ -n "$SIGNALING_URL" && -n "$HOST_ID" ]] && have_signaling=1
    if [[ "$have_direct" -eq 0 && "$have_signaling" -eq 0 ]]; then
        echo "FAIL: viewer mode needs --peer-addr (+ --peer-pubkey), or --signaling-url + --host-id [+ --pin] (TODO(signaling) path)" >&2
        exit 1
    fi
    VIEWER_LOG="$OUT_DIR/viewer.log"
    VIEWER_ARGS=(connect --headless --decoder "$DECODER" --codec "$CODEC"
        --bitrate-mbps "$BITRATE_MBPS"
        --viewer-key-file "$OUT_DIR/viewer-key.bin"
        --recv-dir "$OUT_DIR/received")
    if [[ "$have_direct" -eq 1 ]]; then
        VIEWER_ARGS+=(--host "$PEER_ADDR")
        [[ -n "$PEER_PUBKEY" ]] && VIEWER_ARGS+=(--host-pubkey "$PEER_PUBKEY")
    fi
    if [[ -n "$SIGNALING_URL" ]]; then
        # TODO(signaling): Task #2 ID/PIN provisioning path -- wired but not
        # yet exercised by this harness.
        VIEWER_ARGS+=(--signaling-url "$SIGNALING_URL")
        [[ -n "$HOST_ID" ]] && VIEWER_ARGS+=(--host-id "$HOST_ID")
    fi
    [[ -n "$PIN" ]] && VIEWER_ARGS+=(--pin "$PIN")

    echo "Starting viewer -> ${PEER_ADDR:-signaling:$HOST_ID}"
    "$PRDT" "${VIEWER_ARGS[@]}" >"$VIEWER_LOG" 2>"$VIEWER_LOG.err" &
    VIEWER_PID=$!
    trap 'kill $VIEWER_PID 2>/dev/null; wait 2>/dev/null' EXIT

    deadline=$((SECONDS + CONNECT_SEC))
    read -r H F T FAIL_LINE <<<"$(decode_verdict "$VIEWER_LOG")"
    while [[ $SECONDS -lt $deadline && "$T" -eq 0 && "$FAIL_LINE" == "-" ]]; do
        sleep 1
        read -r H F T FAIL_LINE <<<"$(decode_verdict "$VIEWER_LOG")"
    done

    write_verdict "$H" "$F" "$T" "$FAIL_LINE" "" "$VIEWER_LOG"
    exit $?
    ;;
esac
