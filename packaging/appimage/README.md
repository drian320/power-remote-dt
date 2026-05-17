# AppImage packaging assets for `prdt`

| File | Purpose |
|---|---|
| `net.example.PowerRemoteDt.desktop` | freedesktop.org Desktop Entry — embedded in the AppImage |
| `net.example.PowerRemoteDt.svg` | 256×256 placeholder icon (replace with a designed icon before GA) |
| `net.example.PowerRemoteDt.appdata.xml` | AppStream metadata template — `${VERSION}`, `${BUILD_DATE}`, `${OWNER}`, `${REPO}` are substituted by `scripts/build-appimage.sh` |
| `excludelist.txt` | Libraries that must NOT be bundled in the AppImage payload (proprietary NVIDIA libs, VA-API backend drivers, glibc/libstdc++) |
| `AppRun` | Shell entry point executed when the AppImage starts — sets `LD_LIBRARY_PATH`, runs preflight driver probes, then execs the bundled `prdt` |

The build script is at `scripts/build-appimage.sh`.

---

## Supply-chain pin policy

`scripts/build-appimage.sh` downloads two external binaries at build time.
Both are pinned to specific releases and verified by sha256 before use.
Any bump requires a PR that updates both the pin AND the sha256 in the
script. CI rejects mismatched checksums.

| Binary | Pin type | Current pin |
|---|---|---|
| `linuxdeploy` | release tag | `1-alpha-20240109-1` |
| `linuxdeploy-plugin-gtk` | commit hash | `0a939a51` |

The `continuous` rolling tag for `linuxdeploy` is **forbidden** — it is a
mutable pointer that silently drifts with every upstream merge.

### 5-step bootstrap workflow (first download or pin bump)

1. **Run locally with the opt-in env var** (never in CI):
   ```sh
   ALLOW_UNVERIFIED_SHA256=1 \
   PRDT_BIN=/path/to/prdt VERSION=test ARCH=x86_64 \
     ./scripts/build-appimage.sh
   ```
   The script downloads the binary, prints `::warning::` lines showing
   the actual sha256 of each placeholder, then proceeds.

2. **Capture the `::warning::` lines** — they include the exact hex strings:
   ```
   ::warning::Bootstrap: run once with ALLOW_UNVERIFIED_SHA256=1, then paste this into the script: abc123...
   ```

3. **Edit `scripts/build-appimage.sh`** — replace the
   `VERIFY_AT_PR_TIME_RUN_sha256sum_AFTER_FIRST_DOWNLOAD` placeholder(s)
   with the captured hex strings:
   ```sh
   LD_SHA256="abc123..."
   LDG_SHA256="def456..."
   ```

4. **Commit the updated sha256 values** in the same PR as the tag/commit-hash
   change.

5. **Subsequent runs** (local and CI) work without `ALLOW_UNVERIFIED_SHA256=1`.

### Alternative: manual sha256 capture

```sh
cd /tmp && rm -rf verify && mkdir verify && cd verify
LD_TAG="1-alpha-20240109-1"
LDG_COMMIT="0a939a51"
curl -fL -o linuxdeploy.AppImage \
    "https://github.com/linuxdeploy/linuxdeploy/releases/download/${LD_TAG}/linuxdeploy-x86_64.AppImage"
curl -fL -o plugin-gtk.sh \
    "https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/${LDG_COMMIT}/linuxdeploy-plugin-gtk.sh"
echo "LD_SHA256=\"$(sha256sum linuxdeploy.AppImage | awk '{print $1}')\""
echo "LDG_SHA256=\"$(sha256sum plugin-gtk.sh | awk '{print $1}')\""
```

Paste the printed values into `scripts/build-appimage.sh`.

### Pin bump cadence

- **`linuxdeploy`**: re-pin only when upstream ships a new numbered release
  AND the new release has been validated end-to-end. Update both `LD_TAG`
  and `LD_SHA256` in the same PR.
- **`linuxdeploy-plugin-gtk`**: re-pin every 6 months or when a bug fix is
  needed. Upstream publishes no tags; commit-hash is the only mechanism.

---

## Feature set (B-3)

The AppImage is built with these Cargo features (FFmpeg 6 pinned, Choice C-2):

```
vaapi-h264
ffmpeg-encode-hevc-vaapi-ffmpeg6
ffmpeg-decode-hevc-vaapi-ffmpeg6
ffmpeg-decode-hevc-sw-ffmpeg6
ffmpeg-encode-hevc-nvenc-ffmpeg6
ffmpeg-decode-hevc-nvdec-ffmpeg6
ffmpeg-encode-hevc-vaapi-main10-ffmpeg6
ffmpeg-encode-hevc-nvenc-main10-ffmpeg6
ffmpeg-decode-hevc-vaapi-main10-ffmpeg6
ffmpeg-decode-hevc-nvdec-main10-ffmpeg6
```

CUDA NPP is excluded to keep the AppImage under the 150 MB size budget.
A separate `prdt-cuda-x86_64.AppImage` variant is tracked as F-AppImage-1.

## glibc floor

Committed to **glibc 2.35 (Ubuntu 22.04)**. CI gate V5 scans `prdt` and
all bundled `.so` files for `GLIBC_2.36+` symbol references and fails the
build if any are found. Do not silently raise the floor — amend the plan
(`docs/superpowers/specs/2026-05-17-appimage-linux-packaging.md §N4`) and
update this file if the floor must change.
