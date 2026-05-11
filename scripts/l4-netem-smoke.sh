#!/usr/bin/env bash
# L4 smoke: inject controlled packet loss to verify L3 controller +
# L4 encoder reconfigure path. Run host with --bitrate-mbps 30 first,
# then this script during the viewer connect.
#
# Usage:
#   sudo ./scripts/l4-netem-smoke.sh add <iface> <loss_pct>
#   sudo ./scripts/l4-netem-smoke.sh del <iface>
#   sudo ./scripts/l4-netem-smoke.sh status <iface>
#
# Note on egress-only (codex MEDIUM #4 / spec section 8 risk):
#   tc qdisc on a regular interface only affects EGRESS (host→viewer).
#   That is the correct direction for this smoke because L3's controller
#   measures viewer-perceived loss in the host→viewer path. Viewer→host
#   KeepAlive is unaffected and the host watchdog stays quiet.
#
# Note on loss percentage (codex MEDIUM #4):
#   The transport's default FEC (k=64, m=6) recovers up to ~8.5% loss.
#   Pure 5% loss therefore yields purge=0 and the controller never sees
#   loss. Default to 15% loss + 50ms±20ms delay/jitter to push past the
#   FEC threshold and burst-amplify; this reliably triggers
#   purge_assembler() in the viewer.
set -euo pipefail
ACTION="${1:?usage: $0 <add|del|status> <iface> [loss_pct]}"
IFACE="${2:?missing iface (e.g. eth0)}"
case "$ACTION" in
  add)
    LOSS="${3:?missing loss percent (e.g. 15)}"
    tc qdisc add dev "$IFACE" root netem \
      loss "${LOSS}%" delay 50ms 20ms distribution normal
    echo "netem added: $IFACE loss ${LOSS}% delay 50ms±20ms"
    ;;
  del)
    tc qdisc del dev "$IFACE" root || true
    echo "netem removed: $IFACE"
    ;;
  status)
    tc qdisc show dev "$IFACE"
    ;;
  *) echo "unknown action: $ACTION"; exit 1 ;;
esac
