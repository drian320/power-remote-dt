#!/usr/bin/env bash
# Download prdt's Release-workflow build artifacts via the GitHub CLI and
# verify their sha256, so a CI build can be smoke-tested without a manual
# browser trip to the Actions tab.
#
# .github/workflows/release.yml uploads workflow artifacts on EVERY run (tag
# push OR workflow_dispatch), so this works against a branch that has never
# been tagged -- e.g. feat/rustdesk-ux.
#
# Usage:
#   scripts/fetch-ci-artifacts.sh [options]
#
# Options:
#   --ref <branch|tag>       Branch/tag to fetch or dispatch for (default: current git branch)
#   --run-id <id>            Use this specific workflow run id instead of searching
#   --dispatch               Trigger a fresh `gh workflow run release.yml --ref <ref>`,
#                             wait for it, then download from it
#   --os <windows|linux|linux-appimage|both>   Which artifact(s) to fetch (default: linux)
#   --out-dir <dir>          Download + verify into this dir (default: .smoke-artifacts)
#   --repo <owner/name>      Default: inferred via `gh repo view`
#   --timeout-sec <n>        Max wait for a dispatched run (default: 1800)
#   -h, --help               Show this help
#
# Examples:
#   scripts/fetch-ci-artifacts.sh
#     # latest successful Release run on the current branch, Linux artifact
#   scripts/fetch-ci-artifacts.sh --ref feat/rustdesk-ux --dispatch
#     # kick a fresh CI build of this branch, wait for it, then download
#   scripts/fetch-ci-artifacts.sh --os both
#     # also grab the Windows artifact, e.g. to stage a two-box E2E from a Linux box
set -euo pipefail

REF=""
RUN_ID=""
DISPATCH=0
OS="linux"
OUT_DIR=".smoke-artifacts"
REPO=""
TIMEOUT_SEC=1800

fail() { echo "FAIL: $*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
    case "$1" in
        --ref) REF="$2"; shift 2 ;;
        --run-id) RUN_ID="$2"; shift 2 ;;
        --dispatch) DISPATCH=1; shift ;;
        --os) OS="$2"; shift 2 ;;
        --out-dir) OUT_DIR="$2"; shift 2 ;;
        --repo) REPO="$2"; shift 2 ;;
        --timeout-sec) TIMEOUT_SEC="$2"; shift 2 ;;
        -h|--help) sed -n '2,33p' "$0"; exit 0 ;;
        *) fail "unknown option: $1" ;;
    esac
done

case "$OS" in
    windows|linux|linux-appimage|both) ;;
    *) fail "--os must be one of: windows, linux, linux-appimage, both" ;;
esac

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

command -v gh >/dev/null 2>&1 || fail "GitHub CLI ('gh') not found on PATH. Install: https://cli.github.com/"
gh auth status >/dev/null 2>&1 || fail "gh is installed but not authenticated. Run: gh auth login"

if [[ -z "$REPO" ]]; then
    REPO="$(gh repo view --json nameWithOwner -q .nameWithOwner)" || fail "could not infer repo; pass --repo owner/name"
fi
echo "Repo: $REPO"

if [[ -z "$REF" ]]; then
    REF="$(git rev-parse --abbrev-ref HEAD)"
fi
echo "Ref:  $REF"

mkdir -p "$OUT_DIR"
OUT_DIR="$(cd "$OUT_DIR" && pwd)"

if [[ "$DISPATCH" -eq 1 ]]; then
    [[ -n "$RUN_ID" ]] && fail "--run-id and --dispatch are mutually exclusive"

    echo "Dispatching Release workflow for ref '$REF'..."
    dispatch_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    gh workflow run release.yml --repo "$REPO" --ref "$REF" || fail "gh workflow run failed"

    echo "Waiting for the dispatched run to register with GitHub..."
    run_id=""
    deadline=$((SECONDS + 60))
    while [[ $SECONDS -lt $deadline && -z "$run_id" ]]; do
        sleep 3
        run_id="$(gh run list --repo "$REPO" --workflow=release.yml --branch "$REF" \
            --event workflow_dispatch --json databaseId,createdAt --limit 5 \
            --jq "[.[] | select(.createdAt >= \"$dispatch_at\")] | sort_by(.createdAt) | last | .databaseId // empty" 2>/dev/null || true)"
    done
    [[ -n "$run_id" ]] || fail "could not find the dispatched run within 60s (check: gh run list --repo $REPO --workflow=release.yml)"
    RUN_ID="$run_id"
    echo "Run id: $RUN_ID -- waiting (up to ${TIMEOUT_SEC}s) for it to finish..."
    if ! gh run watch "$RUN_ID" --repo "$REPO" --exit-status --interval 15; then
        fail "Release run $RUN_ID did not succeed. Inspect: gh run view $RUN_ID --repo $REPO --log-failed"
    fi
    echo "Run $RUN_ID succeeded."
elif [[ -z "$RUN_ID" ]]; then
    echo "Looking up the latest successful Release run for ref '$REF'..."
    RUN_ID="$(gh run list --repo "$REPO" --workflow=release.yml --branch "$REF" \
        --status success --json databaseId,url --limit 1 --jq '.[0].databaseId // empty')"
    [[ -n "$RUN_ID" ]] || fail "no successful Release run found for ref '$REF'. Use --dispatch to trigger one, or pass --run-id <id>."
    run_url="$(gh run list --repo "$REPO" --workflow=release.yml --branch "$REF" --status success --json url --limit 1 --jq '.[0].url')"
    echo "Using run $RUN_ID ($run_url)"
else
    echo "Using explicit run id $RUN_ID"
fi

get_artifact() {
    local name="$1" dest_subdir="$2"
    local dest="$OUT_DIR/$dest_subdir"
    mkdir -p "$dest"
    echo "Downloading artifact '$name' -> $dest"
    gh run download "$RUN_ID" --repo "$REPO" --name "$name" --dir "$dest" \
        || fail "gh run download failed for artifact '$name' (run $RUN_ID)"
    echo "$dest"
}

verify_sha256() {
    local dir="$1" bin_file="$2" sha_file="$3"
    [[ -f "$dir/$sha_file" ]] || fail "missing $sha_file next to $bin_file in $dir"
    [[ -f "$dir/$bin_file" ]] || fail "missing $bin_file in $dir"
    ( cd "$dir" && sha256sum -c "$sha_file" --strict ) || fail "sha256 MISMATCH for $bin_file (see $dir/$sha_file)"
}

declare -A DOWNLOADED=()

want_windows=0; want_linux=0; want_appimage=0
case "$OS" in
    windows) want_windows=1 ;;
    linux) want_linux=1 ;;
    linux-appimage) want_appimage=1 ;;
    both) want_windows=1; want_linux=1 ;;
esac

if [[ "$want_windows" -eq 1 ]]; then
    dir="$(get_artifact prdt-windows-x86_64 windows)"
    verify_sha256 "$dir" "prdt-windows-x86_64.exe" "prdt-windows-x86_64.exe.sha256"
    DOWNLOADED[windows]="$dir/prdt-windows-x86_64.exe"
fi
if [[ "$want_linux" -eq 1 ]]; then
    dir="$(get_artifact prdt-linux-x86_64 linux)"
    verify_sha256 "$dir" "prdt-linux-x86_64" "prdt-linux-x86_64.sha256"
    chmod +x "$dir/prdt-linux-x86_64"
    DOWNLOADED[linux]="$dir/prdt-linux-x86_64"
fi
if [[ "$want_appimage" -eq 1 ]]; then
    dir="$(get_artifact prdt-linux-x86_64-appimage linux-appimage)"
    appimg="$(find "$dir" -maxdepth 1 -name '*.AppImage' | head -n1)"
    [[ -n "$appimg" ]] || fail "no .AppImage file found inside the downloaded artifact directory: $dir"
    verify_sha256 "$dir" "$(basename "$appimg")" "$(basename "$appimg").sha256"
    chmod +x "$appimg"
    DOWNLOADED[linux-appimage]="$appimg"
fi

echo ""
echo "Downloaded + sha256-verified:"
for k in "${!DOWNLOADED[@]}"; do echo "  $k: ${DOWNLOADED[$k]}"; done
echo ""
if [[ -n "${DOWNLOADED[linux]:-}" ]]; then
    echo "Next: ./scripts/e2e-smoke.sh loopback --bin-path \"${DOWNLOADED[linux]}\""
fi
