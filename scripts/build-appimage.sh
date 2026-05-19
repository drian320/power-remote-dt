#!/usr/bin/env bash
# scripts/build-appimage.sh
# Build a prdt AppImage from a pre-built ELF.
#
# Usage:
#   PRDT_BIN=target/x86_64-unknown-linux-gnu/release/prdt \
#   VERSION=0.0.1 \
#   ARCH=x86_64 \
#   ./scripts/build-appimage.sh
#
# Output: dist/prdt-${VERSION}-${ARCH}.AppImage + .sha256
#
# Inputs (env or defaults):
#   PRDT_BIN  — path to release-built prdt ELF (required)
#   VERSION   — version string baked into the AppImage filename (required)
#   ARCH      — x86_64 (only supported arch in this PR)
#   APPDIR    — staging dir (default: ./AppDir)
#   DIST      — output dir (default: ./dist)
#
# Dependencies on the build host:
#   - linuxdeploy (downloaded if missing) + linuxdeploy-plugin-gtk
#   - patchelf (apt)
#   - desktop-file-utils (apt)
#   - file (apt; for `file` command)
#   - objdump (binutils, apt)

set -euo pipefail

: "${PRDT_BIN:?PRDT_BIN env var required (path to prdt ELF)}"
: "${VERSION:?VERSION env var required}"
ARCH="${ARCH:-x86_64}"
APPDIR="${APPDIR:-AppDir}"
DIST="${DIST:-dist}"

# 1. Sanity check
test -x "$PRDT_BIN" || { echo "PRDT_BIN $PRDT_BIN is not executable"; exit 1; }
file "$PRDT_BIN" | grep -q "ELF 64-bit LSB" || { echo "$PRDT_BIN is not an x86_64 ELF"; exit 1; }

# ---------------------------------------------------------------------------
# Supply-chain tool download helpers
# ---------------------------------------------------------------------------
# Both linuxdeploy and linuxdeploy-plugin-gtk are pinned to specific releases
# and verified by sha256 before use. Any version bump REQUIRES updating BOTH
# the pin AND the sha256 in this script (CI rejects mismatched checksums).
#
# linuxdeploy: tag 1-alpha-20240109-1 (latest stable numbered release at
#   plan authoring time; the `continuous` rolling tag is FORBIDDEN per plan §9).
# linuxdeploy-plugin-gtk: commit 3b67a1d1c1b0c8268f57f2bce40fe2d33d409cea
#   (current HEAD of master, last commit 2023-10-04 — repo received no
#   further commits, maintainer publishes no tags). The previous 8-char
#   short-sha pin `0a939a51` no longer resolves via raw.githubusercontent
#   (likely force-pushed away), so the pin was refreshed to the full sha
#   of current HEAD; updating the pin requires recomputing the sha256
#   with: sha256sum tools/linuxdeploy-plugin-gtk

LD_TAG="1-alpha-20240109-1"
LD_URL="https://github.com/linuxdeploy/linuxdeploy/releases/download/${LD_TAG}/linuxdeploy-x86_64.AppImage"
LD_SHA256="${LD_SHA256:-c86d6540f1df31061f02f539a2d3445f8d7f85cc3994eee1e74cd1ac97b76df0}"

LDG_COMMIT="3b67a1d1c1b0c8268f57f2bce40fe2d33d409cea"
LDG_URL="https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/${LDG_COMMIT}/linuxdeploy-plugin-gtk.sh"
LDG_SHA256="${LDG_SHA256:-b0f4cbc684a0103a9651f0955b635eaea0096b3a66c0f5a2c2aa337960375171}"

verify_or_die() {
    local file="$1" expected="$2"
    # Placeholder branch MUST fail by default. CI runs without ALLOW_UNVERIFIED_SHA256=1
    # so a placeholder still in the script at CI time = hard build failure requiring
    # reviewer action. Bootstrap (first download or upstream bump) opts in via the env var.
    if [[ "$expected" == *VERIFY_AT_PR_TIME_RUN_sha256sum_AFTER_FIRST_DOWNLOAD* ]]; then
        local actual
        actual="$(sha256sum "$file" | awk '{print $1}')"
        echo "::warning::sha256 placeholder for $file" >&2
        echo "::warning::Bootstrap: run once with ALLOW_UNVERIFIED_SHA256=1, then paste this into the script: $actual" >&2
        if [[ "${ALLOW_UNVERIFIED_SHA256:-0}" == "1" ]]; then
            echo "::warning::ALLOW_UNVERIFIED_SHA256=1 set — proceeding without verification (bootstrap mode only)" >&2
            return 0
        fi
        echo "::error::Refusing to proceed without sha256 verification. Set ALLOW_UNVERIFIED_SHA256=1 ONLY for first-download bootstrap (never in CI)." >&2
        exit 1
    fi
    local actual
    actual=$(sha256sum "$file" | awk '{print $1}')
    if [ "$actual" != "$expected" ]; then
        echo "FATAL: sha256 mismatch for $file" >&2
        echo "  expected: $expected" >&2
        echo "  actual:   $actual" >&2
        echo "  This means the upstream binary CHANGED at the pinned URL — investigate before bumping." >&2
        exit 1
    fi
}

# ---------------------------------------------------------------------------
# FFmpeg version detection
# ---------------------------------------------------------------------------
# Sets globals: FFMPEG_MAJOR, LIBAVCODEC, LIBAVUTIL, LIBAVFORMAT,
# LIBAVFILTER, LIBAVDEVICE, LIBSWRESAMPLE, LIBSWSCALE (SONAME version numbers).
# Prefers FFmpeg 6 (CI canonical on ubuntu-24.04); falls back to FFmpeg 5
# (dev-container on Debian 12 bookworm). Refuses everything else.
detect_ffmpeg_libs() {
    if [ -f /usr/lib/x86_64-linux-gnu/libavcodec.so.60 ]; then
        FFMPEG_MAJOR=6
        LIBAVCODEC=60; LIBAVUTIL=58; LIBAVFORMAT=60; LIBAVFILTER=9
        LIBAVDEVICE=60; LIBSWRESAMPLE=4; LIBSWSCALE=7
    elif [ -f /usr/lib/x86_64-linux-gnu/libavcodec.so.59 ]; then
        FFMPEG_MAJOR=5
        LIBAVCODEC=59; LIBAVUTIL=57; LIBAVFORMAT=59; LIBAVFILTER=8
        LIBAVDEVICE=59; LIBSWRESAMPLE=4; LIBSWSCALE=6
        echo "WARNING: FFmpeg 5 detected (dev-container path). Resulting AppImage" >&2
        echo "  is for LOCAL SMOKE ONLY; canonical release artifact is FFmpeg 6 from CI." >&2
    else
        echo "FATAL: No supported FFmpeg (5 or 6) found on build host." >&2
        echo "  Install libavcodec-dev (FFmpeg 6 preferred); see CI's apt-get list." >&2
        exit 1
    fi
}

# Parse libavcodec SONAME from the prdt ELF's DT_NEEDED to detect which FFmpeg
# major the binary was actually linked against, regardless of apt packages.
detect_prdt_ffmpeg_major() {
    objdump -p "$PRDT_BIN" 2>/dev/null \
        | awk '/NEEDED .*libavcodec\.so\.[0-9]+/ {
                  n = $NF;
                  sub(/.*\.so\./, "", n);
                  if      (n == "60") print 6;
                  else if (n == "59") print 5;
                  else if (n == "61") print 7;
                  else if (n == "58") print 4;
                  else                print "unknown:" n;
                  exit
              }'
}

detect_ffmpeg_libs

# ABI cross-version guard: refuse to bundle .so files from a different FFmpeg
# major than the one the prdt ELF was linked against. Catches the case where
# a developer runs this script inside the bookworm dev-container (FFmpeg 5)
# against a prdt ELF that was compiled with -ffmpeg6 features (FFmpeg 6 symbols).
EXPECTED_FFMPEG_MAJOR=6
PRDT_FFMPEG_MAJOR="$(detect_prdt_ffmpeg_major)"
if [ -z "$PRDT_FFMPEG_MAJOR" ]; then
    echo "::error::Could not detect libavcodec SONAME in $PRDT_BIN — refusing to bundle." >&2
    exit 1
fi
if [ "$PRDT_FFMPEG_MAJOR" != "$EXPECTED_FFMPEG_MAJOR" ]; then
    echo "::error::prdt was linked against FFmpeg $PRDT_FFMPEG_MAJOR but B-3 cargo features require FFmpeg $EXPECTED_FFMPEG_MAJOR (C-2 pin)." >&2
    echo "::error::Host detected FFmpeg $FFMPEG_MAJOR via detect_ffmpeg_libs." >&2
    echo "::error::Bundling mismatched .so files would cause runtime symbol-lookup failures." >&2
    echo "::error::Fix: build on ubuntu-24.04 (FFmpeg 6) — the dev-container's FFmpeg 5 cannot produce a release-shaped AppImage." >&2
    exit 1
fi
if [ "$FFMPEG_MAJOR" != "$EXPECTED_FFMPEG_MAJOR" ]; then
    echo "::error::FFmpeg ABI inconsistency: host apt = $FFMPEG_MAJOR but prdt linked = $PRDT_FFMPEG_MAJOR." >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# 2. Download linuxdeploy + plugin-gtk (cached in tools/)
# ---------------------------------------------------------------------------
mkdir -p tools

if [ ! -f tools/linuxdeploy ]; then
    curl -fL -o tools/linuxdeploy.dl "$LD_URL"
    verify_or_die tools/linuxdeploy.dl "$LD_SHA256"
    mv tools/linuxdeploy.dl tools/linuxdeploy
    chmod +x tools/linuxdeploy
fi
if [ ! -f tools/linuxdeploy-plugin-gtk ]; then
    curl -fL -o tools/linuxdeploy-plugin-gtk.dl "$LDG_URL"
    verify_or_die tools/linuxdeploy-plugin-gtk.dl "$LDG_SHA256"
    mv tools/linuxdeploy-plugin-gtk.dl tools/linuxdeploy-plugin-gtk
    chmod +x tools/linuxdeploy-plugin-gtk
fi
export PATH="$PWD/tools:$PATH"

# ---------------------------------------------------------------------------
# 3-6. Stage AppDir
# ---------------------------------------------------------------------------
rm -rf "$APPDIR" "$DIST"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/lib" \
         "$APPDIR/usr/share/applications" \
         "$APPDIR/usr/share/icons/hicolor/256x256/apps" \
         "$APPDIR/usr/share/metainfo" \
         "$DIST"

cp "$PRDT_BIN" "$APPDIR/usr/bin/prdt"
chmod +x "$APPDIR/usr/bin/prdt"
cp packaging/appimage/AppRun "$APPDIR/AppRun"
chmod +x "$APPDIR/AppRun"
cp packaging/appimage/net.example.PowerRemoteDt.desktop \
   "$APPDIR/usr/share/applications/"
cp packaging/appimage/net.example.PowerRemoteDt.svg \
   "$APPDIR/usr/share/icons/hicolor/256x256/apps/"

# 8.5. Stage AppStream metadata with template substitution
sed -e "s/\${OWNER}/${GITHUB_REPOSITORY_OWNER:-example}/g" \
    -e "s/\${REPO}/${GITHUB_REPOSITORY_NAME:-power-remote-dt}/g" \
    -e "s/\${VERSION}/${VERSION}/g" \
    -e "s/\${BUILD_DATE}/$(date -u +%Y-%m-%d)/g" \
    packaging/appimage/net.example.PowerRemoteDt.appdata.xml \
    > "$APPDIR/usr/share/metainfo/net.example.PowerRemoteDt.appdata.xml"

# ---------------------------------------------------------------------------
# 7. Exclude flags (read from excludelist.txt, strip comments + blank lines)
# ---------------------------------------------------------------------------
EXCLUDE_FLAGS=()
while IFS= read -r line; do
    line="${line%%#*}"; line="${line## }"; line="${line%% }"
    [ -z "$line" ] && continue
    EXCLUDE_FLAGS+=(--exclude-library "$line")
done < packaging/appimage/excludelist.txt

# ---------------------------------------------------------------------------
# 8. Library flags (explicit adds for dlopen-loaded libs not in DT_NEEDED)
# ---------------------------------------------------------------------------
LIB_FLAGS=()
for lib in \
    /usr/lib/x86_64-linux-gnu/libavcodec.so.${LIBAVCODEC} \
    /usr/lib/x86_64-linux-gnu/libavutil.so.${LIBAVUTIL} \
    /usr/lib/x86_64-linux-gnu/libavformat.so.${LIBAVFORMAT} \
    /usr/lib/x86_64-linux-gnu/libavfilter.so.${LIBAVFILTER} \
    /usr/lib/x86_64-linux-gnu/libavdevice.so.${LIBAVDEVICE} \
    /usr/lib/x86_64-linux-gnu/libswresample.so.${LIBSWRESAMPLE} \
    /usr/lib/x86_64-linux-gnu/libswscale.so.${LIBSWSCALE} \
    /usr/lib/x86_64-linux-gnu/libva.so.2 \
    /usr/lib/x86_64-linux-gnu/libva-drm.so.2 \
    /usr/lib/x86_64-linux-gnu/libva-x11.so.2 \
    /usr/lib/x86_64-linux-gnu/libpipewire-0.3.so.0 \
    /usr/lib/x86_64-linux-gnu/libopenh264.so.7 \
    /usr/lib/x86_64-linux-gnu/libayatana-appindicator3.so.1 \
    /usr/lib/x86_64-linux-gnu/libasound.so.2
do
    test -f "$lib" || { echo "Missing required lib: $lib"; exit 1; }
    LIB_FLAGS+=(--library "$lib")
done
# libasound.so.2 must be force-added: it is a DT_NEEDED dep of prdt (cpal /
# audiopus link ALSA), but linuxdeploy's *built-in* default exclude list drops
# it — upstream's rationale is that ALSA dlopen-loads versioned plugins from
# /usr/lib/.../alsa-lib/ and a bundled libasound can mismatch the host's
# plugins. We only need the client library resolvable at load time (the
# plugin path is exercised only when an actual PCM device is opened, which the
# `--help` smoke test never does), and the release notes promise the AppImage
# needs nothing beyond libfuse2 — so an explicit `--library` add is correct.

# ---------------------------------------------------------------------------
# 9. Run linuxdeploy
# ---------------------------------------------------------------------------
# DEPLOY_GTK_VERSION=3 is mandatory: linuxdeploy-plugin-gtk's auto-detect
# probes for `pkg-config gtk{2,3,4}-x11` and bails ("failed to auto-detect
# GTK version") when none match. On Ubuntu 24.04 runner images, libgtk-3-dev
# installs gtk+-3.0.pc (not the `-x11` suffix variant the plugin grep'd
# for), so even with the dep present the plugin gives up. prdt links against
# GTK3 (libayatana-appindicator3 + libgtk-3 — see the apt list above), so
# pinning to 3 is correct.
mkdir -p "$DIST"
# `$OUT` must be absolute — section 12's `--appimage-extract` step runs from
# inside a mktemp dir, so a relative path would fail with command-not-found
# (exit 127) the moment we `cd` away from the repo root.
OUT="$(cd "$DIST" && pwd)/prdt-${VERSION}-${ARCH}.AppImage"
DEPLOY_GTK_VERSION=3 OUTPUT="$OUT" linuxdeploy --output appimage --appdir "$APPDIR" \
    --executable "$APPDIR/usr/bin/prdt" \
    --desktop-file "$APPDIR/usr/share/applications/net.example.PowerRemoteDt.desktop" \
    --icon-file "$APPDIR/usr/share/icons/hicolor/256x256/apps/net.example.PowerRemoteDt.svg" \
    --plugin gtk \
    "${LIB_FLAGS[@]}" \
    "${EXCLUDE_FLAGS[@]}"

# ---------------------------------------------------------------------------
# 10-11. Verify output + generate checksum
# ---------------------------------------------------------------------------
test -f "$OUT" || {
    echo "FATAL: linuxdeploy did not produce $OUT despite OUTPUT env"
    ls -la "$DIST/" .
    exit 1
}
chmod +x "$OUT"
(cd "$DIST" && sha256sum "$(basename "$OUT")" > "$(basename "$OUT").sha256")

# ---------------------------------------------------------------------------
# 12. Post-build assertions (A1, A3, A4, A5, V5)
# ---------------------------------------------------------------------------
SIZE=$(stat -c%s "$OUT")
MAX=$((150 * 1024 * 1024))          # 150 MB hard budget
ESTIMATE=$((145 * 1024 * 1024))     # iter-1 estimate (unmeasured; update after first CI run)
SOFT_MAX=$((ESTIMATE * 120 / 100))  # +20% drift guard
if [ "$SIZE" -gt "$MAX" ]; then
    echo "A1 FAIL (HARD): AppImage size $SIZE > 150 MB budget $MAX"
    exit 1
fi
if [ "$SIZE" -gt "$SOFT_MAX" ]; then
    echo "A1 FAIL (SOFT): AppImage size $SIZE > 1.2× estimate ($SOFT_MAX)" >&2
    echo "  Estimate is stale; update the estimate in this script + plan, or investigate bloat." >&2
    exit 1
fi
echo "A1 PASS: AppImage size $SIZE bytes (hard $MAX, soft $SOFT_MAX, estimate $ESTIMATE)"

# Extract AppImage for inspection
EXTRACT_DIR=$(mktemp -d)
trap 'rm -rf "$EXTRACT_DIR"' EXIT
( cd "$EXTRACT_DIR" && "$OUT" --appimage-extract > /dev/null )

LDD_OUT=$(ldd "$EXTRACT_DIR/squashfs-root/usr/bin/prdt" 2>/dev/null || true)

# A3: bundled libs must resolve inside the AppImage, not to host system paths.
# linuxdeploy sets the prdt binary's rpath to $ORIGIN/../lib, so ldd resolves
# bundled libs through `squashfs-root/usr/bin/../lib/`, NOT the canonicalized
# `squashfs-root/usr/lib/`. The match must accept either spelling — what we
# actually care about is the `squashfs-root/` prefix (i.e. NOT a host path
# like /lib/x86_64-linux-gnu/).
if echo "$LDD_OUT" \
    | grep -E "(libavcodec|libavutil|libavformat|libva\.|libpipewire-0\.3|libgtk-3)\.so" \
    | grep -qv "squashfs-root/"; then
    echo "A3 FAIL: external host lib referenced for a bundled library:"
    echo "$LDD_OUT" | grep -E "(libavcodec|libavutil|libavformat|libva\.|libpipewire-0\.3|libgtk-3)\.so"
    exit 1
fi
echo "A3 PASS: all bundled libs resolve under squashfs-root/"

# A4: no proprietary NVIDIA libs bundled
if find "$EXTRACT_DIR/squashfs-root/usr/lib" \
    \( -name 'libnvidia*' -o -name 'libnvcuvid*' -o -name 'libcuda*' \
       -o -name 'libnppc*' -o -name 'libnppi*' \) | grep -q .; then
    echo "A4 FAIL: proprietary NVIDIA libs found in AppImage payload"
    exit 1
fi
echo "A4 PASS: no proprietary NVIDIA libs bundled"

# A5: no VAAPI backend drivers bundled
if [ -d "$EXTRACT_DIR/squashfs-root/usr/lib/dri" ]; then
    echo "A5 FAIL: usr/lib/dri/ exists in AppImage; VAAPI backend drivers must not be bundled"
    exit 1
fi
echo "A5 PASS: no VAAPI backend drivers bundled"

# V5: glibc floor enforcement — floor is glibc 2.39 (Ubuntu 24.04 runner).
# The original plan committed to 2.35 (Ubuntu 22.04 target), but the workflow's
# `release-linux-appimage` job is pinned to `ubuntu-24.04` because Ubuntu 22.04
# ships libpipewire 0.3.48 — below the 0.3.55 ABI floor that
# pipewire-rs 0.9 / libspa-sys-0.9.2 require — and the build script panics
# regardless of libpipewire-0.3-dev being installed. Once the runner moved
# forward, every bundled .so (libtasn1, libplacebo, libnuma, libXcursor,
# librist, libzmq, …) starts referencing GLIBC_2.36-2.38 because that is
# what Ubuntu 24.04 builds against. So the AppImage target is officially
# Ubuntu 24.04+; Ubuntu 22.04 is unsupported on this packaging path.
# Scan prdt binary AND all bundled .so files recursively (covers GTK modules in
# subdirs, libssl.so.3, libpipewire, etc. that a non-recursive glob would miss).
GLIBC_VIOLATIONS=$(mktemp)
trap 'rm -f "$GLIBC_VIOLATIONS"' EXIT

scan_elf_for_glibc() {
    local elf="$1"
    file "$elf" 2>/dev/null | grep -q "ELF " || return 0
    local hits
    # `|| true` is mandatory: when an ELF has no glibc 2.40+ refs (which is
    # the success case for this scan), `grep -oE` exits 1 and `pipefail`
    # propagates it. Without the catcher, `set -e` then kills the script
    # silently RIGHT BEFORE V5 PASS prints, and the runner reports a bare
    # "exit code 1" with no diagnostic — exactly the regression that
    # turned the AppImage job into a 7-minute mystery failure.
    #
    # Regex matches GLIBC_2.40 — GLIBC_2.99 (the actual ceiling we enforce —
    # 2.39 is the floor, anything above is forward-incompatible with the
    # build runner). `[4-9][0-9]+` requires at least TWO digits so the
    # version suffix `2.4`, `2.7`, `2.8`, … (legitimate sub-2.35 versions
    # commonly referenced) does NOT false-match — the previous `[0-9]*`
    # quantifier allowed zero trailing digits and matched them all.
    hits=$(objdump -T "$elf" 2>/dev/null \
        | grep -oE 'GLIBC_2\.[4-9][0-9]+' \
        | sort -u || true)
    if [ -n "$hits" ]; then
        {
            echo "  $elf:"
            # shellcheck disable=SC2086  # intentional word-split: $hits is newline-separated GLIBC symbols, one per line
            printf '    %s\n' $hits
        } >> "$GLIBC_VIOLATIONS"
    fi
}

scan_elf_for_glibc "$EXTRACT_DIR/squashfs-root/usr/bin/prdt"

while IFS= read -r -d '' so_file; do
    scan_elf_for_glibc "$so_file"
done < <(find "$EXTRACT_DIR/squashfs-root/usr/lib" -type f -name '*.so*' -print0)

if [ -s "$GLIBC_VIOLATIONS" ]; then
    echo "V5 FAIL: glibc 2.40+ symbol references found (floor is 2.39, Ubuntu 24.04 runner):" >&2
    cat "$GLIBC_VIOLATIONS" >&2
    exit 1
fi
echo "V5 PASS: no glibc 2.40+ symbols referenced in prdt or any bundled .so (recursive scan)"

echo "Build complete: $OUT"
