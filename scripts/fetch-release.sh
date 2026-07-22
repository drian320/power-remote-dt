#!/usr/bin/env bash
# Download a public prdt RELEASE binary for Linux into a directory you choose.
#
# Pulls assets straight from the GitHub Release page — public, so no `gh` login
# or token is needed (plain curl). Verifies the sha256 when the release ships
# one. Different from scripts/fetch-ci-artifacts.sh, which pulls per-run CI
# *workflow artifacts* via `gh` (auth required).
#
# Usage:
#   ./fetch-release.sh <dest-dir> [--tag <tag>] [--elf] [--repo owner/repo]
#
#   <dest-dir>   directory to download into (created if missing). REQUIRED.
#   --tag T      release tag (e.g. v0.1.2-rustdesk-ux). Default: newest release
#                INCLUDING pre-releases (which these tags are).
#   --elf        fetch the bare prdt-linux-x86_64 ELF (VAAPI H.264 only) instead
#                of the recommended AppImage (bundles FFmpeg 6 + VA-API).
#   --repo R     owner/repo. Default: drian320/power-remote-dt.
#
# Examples:
#   ./fetch-release.sh ~/prdt
#   ./fetch-release.sh ~/prdt --tag v0.1.2-rustdesk-ux
#   ./fetch-release.sh ~/prdt --elf
set -euo pipefail

REPO="drian320/power-remote-dt"
TAG=""
VARIANT="appimage"   # or "elf"
DEST=""

while [ $# -gt 0 ]; do
    case "$1" in
        --tag)      TAG="${2:?--tag needs a value}"; shift 2 ;;
        --repo)     REPO="${2:?--repo needs a value}"; shift 2 ;;
        --elf)      VARIANT="elf"; shift ;;
        --appimage) VARIANT="appimage"; shift ;;
        -h|--help)  sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        -*)         echo "unknown option: $1" >&2; exit 2 ;;
        *)          if [ -z "$DEST" ]; then DEST="$1"; else echo "unexpected arg: $1" >&2; exit 2; fi; shift ;;
    esac
done

[ -n "$DEST" ] || { echo "usage: $0 <dest-dir> [--tag T] [--elf] [--repo owner/repo]" >&2; exit 2; }

need() { command -v "$1" >/dev/null 2>&1 || { echo "missing required tool: $1" >&2; exit 1; }; }
need curl
need sha256sum

if [ -z "$TAG" ]; then
    # /releases/latest skips pre-releases; list all (newest first) and take the
    # first tag_name. Capture the full response FIRST, then parse via a
    # here-string — piping `curl | grep -m1` makes grep close the pipe early,
    # which SIGPIPEs curl into exit 23 under `set -o pipefail`. awk needs no jq.
    api="$(curl -fsSL "https://api.github.com/repos/$REPO/releases")" \
        || { echo "failed to query releases for $REPO" >&2; exit 1; }
    TAG="$(awk -F'"' '/"tag_name"/{print $4; exit}' <<<"$api")"
    [ -n "$TAG" ] || { echo "could not resolve the latest release tag for $REPO" >&2; exit 1; }
    echo "Latest release: $TAG"
fi

mkdir -p "$DEST"
BASE="https://github.com/$REPO/releases/download/$TAG"

if [ "$VARIANT" = "elf" ]; then
    ASSET="prdt-linux-x86_64"
else
    ASSET="prdt-${TAG}-x86_64.AppImage"
fi

echo "Downloading $ASSET ..."
curl -fL --progress-bar -o "$DEST/$ASSET" "$BASE/$ASSET"

# sha256: a mismatch is fatal; a missing .sha256 is only a warning.
if curl -fsSL -o "$DEST/$ASSET.sha256" "$BASE/$ASSET.sha256" 2>/dev/null; then
    ( cd "$DEST" && sha256sum -c "$ASSET.sha256" ) \
        || { echo "sha256 MISMATCH for $ASSET" >&2; exit 1; }
else
    echo "warning: no .sha256 published for $ASSET; skipping integrity check" >&2
fi

chmod +x "$DEST/$ASSET"
echo
echo "Done. Run:  $DEST/$ASSET"
if [ "$VARIANT" = "appimage" ]; then
    echo "(needs libfuse2 + a desktop graphics stack; if FUSE errors: $DEST/$ASSET --appimage-extract-and-run)"
fi
