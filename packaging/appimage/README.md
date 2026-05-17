# AppImage packaging assets for `prdt`

| File | Purpose |
|---|---|
| `net.example.PowerRemoteDt.desktop` | freedesktop.org Desktop Entry — embedded in the AppImage |
| `net.example.PowerRemoteDt.svg` | 256×256 placeholder icon (replace with a designed icon before GA) |
| `net.example.PowerRemoteDt.appdata.xml` | AppStream metadata template — `${VERSION}`, `${BUILD_DATE}`, `${OWNER}`, `${REPO}` are substituted by `scripts/build-appimage.sh` |
| `excludelist.txt` | Libraries that must NOT be bundled in the AppImage payload (proprietary NVIDIA libs, VA-API backend drivers, glibc/libstdc++) |
| `AppRun` | Shell entry point executed when the AppImage starts — sets `LD_LIBRARY_PATH`, runs preflight driver probes, then execs the bundled `prdt` |

The build script is at `scripts/build-appimage.sh`.
