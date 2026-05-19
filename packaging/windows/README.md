# Windows FFmpeg DLL Bootstrap

This directory contains the version-pinned manifest for the pre-built BtbN
FFmpeg Windows DLLs used by the `media-win-ffmpeg` Cargo feature.

## 5-step bootstrap workflow

1. **Verify prerequisites** — PowerShell 7+, Cargo, MSVC toolchain (via
   Visual Studio 2022 or Build Tools), Rust target
   `x86_64-pc-windows-msvc` installed.

2. **Fetch DLLs** — run from the repo root:

   ```powershell
   .\scripts\fetch-ffmpeg-windows.ps1
   ```

   On the first run with a sha256 placeholder, set
   `$env:ALLOW_UNVERIFIED_SHA256=1` (bootstrap mode only — never in CI),
   copy the printed hash into `packaging/windows/ffmpeg-manifest.json`, and
   re-run without the override to confirm the hash.

3. **Source the env file** — the script writes
   `target\windows-ffmpeg\env.ps1` which exports the three env vars that
   `crates/media-win/build.rs` needs:

   ```powershell
   . .\target\windows-ffmpeg\env.ps1
   ```

4. **Build** — with the env vars in scope:

   ```powershell
   cargo build -p prdt-media-win --features media-win-ffmpeg --target x86_64-pc-windows-msvc
   ```

5. **Run** — copy the DLLs from `target\windows-ffmpeg\bin\` next to the
   compiled binary before launching, or add `$env:FFMPEG_DLL_PATH` to
   `PATH`:

   ```powershell
   $env:PATH = "$env:FFMPEG_DLL_PATH;$env:PATH"
   cargo run -p prdt --features media-win-ffmpeg --target x86_64-pc-windows-msvc -- --encoder ffmpeg-nvenc-hevc
   ```

## Mirror — Cloudflare R2

The `mirror_url` field in `ffmpeg-manifest.json` points to:

```
https://pub-a0d71751e9de470a8ba614ad2abd87c8.r2.dev/ffmpeg-n7.1.4-win64-lgpl-shared.zip
```

Backend: **Cloudflare R2** (S3-compatible object storage), bucket
`prdt-windows-ffmpeg-mirror` on the Cloudflare account associated with
`Nakanosita@gmail.com`. The `pub-*.r2.dev` hostname is the
auto-provisioned public dev URL (created via
`wrangler r2 bucket dev-url enable prdt-windows-ffmpeg-mirror`).

Public read is anonymous — no AWS OIDC role or signed URL needed by CI.
The `Invoke-WebRequest` call in `scripts/fetch-ffmpeg-windows.ps1` falls
back to the mirror when the BtbN primary URL goes 404 (as happens when
upstream rotates an autobuild tag out of its short-retention window).

### Rotating the pin

When bumping `btbn_release_tag` to a fresher autobuild:

1. Download the new zip from BtbN, record its sha256.
2. Re-upload to the same bucket under the new filename:
   ```sh
   export CLOUDFLARE_ACCOUNT_ID=3599220f74f38dfa291894c3aef204b0
   export CLOUDFLARE_API_TOKEN=<R2:Edit token>
   wrangler r2 object put \
     prdt-windows-ffmpeg-mirror/ffmpeg-n<NEW_VERSION>-win64-lgpl-shared.zip \
     --file=<path> --content-type=application/zip --remote
   ```
3. Update both `url`, `mirror_url`, `sha256`, `ffmpeg_version`,
   `btbn_release_tag`, and the DLL SONAMEs in `expected_files`.
4. Bump `crates/media-win/Cargo.toml`'s `rusty_ffmpeg-win` ABI feature
   if the FFmpeg major changed (5/6/7/8).

The `ffmpeg-mirror-healthcheck.yml` workflow polls the mirror on a
schedule to catch silent regressions (e.g. bucket misconfigured, R2
service incident).

## License

The fetched bundle is the BtbN `lgpl-shared` variant of FFmpeg n6.1.1.  It
contains no GPL components (no libx265, no libfdk-aac).  Dynamic linking to
LGPL FFmpeg DLLs from an Apache-2.0 binary is permitted under the LGPL
provided the DLLs and their source-availability terms are documented at
distribution.  See `docs/superpowers/windows-ffmpeg-install.md` for the
user-facing license note.
