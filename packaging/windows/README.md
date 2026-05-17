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

## Mirror and AWS account

The `mirror_url` field in `ffmpeg-manifest.json` points to:

```
https://prdt-windows-ffmpeg-mirror.s3.amazonaws.com/ffmpeg-n6.1.1-win64-lgpl-shared.zip
```

AWS account owning the `prdt-windows-ffmpeg-mirror` bucket:
`<TO_BE_FILLED_BY_INFRA_OWNER>`

The bucket must be pre-staged before PR1 merges (it is a merge
precondition). CI's GitHub Actions OIDC role requires `s3:GetObject` on the
bucket. See `docs/superpowers/windows-ffmpeg-mirror.md` for the full
rotation runbook.

## License

The fetched bundle is the BtbN `lgpl-shared` variant of FFmpeg n6.1.1.  It
contains no GPL components (no libx265, no libfdk-aac).  Dynamic linking to
LGPL FFmpeg DLLs from an Apache-2.0 binary is permitted under the LGPL
provided the DLLs and their source-availability terms are documented at
distribution.  See `docs/superpowers/windows-ffmpeg-install.md` for the
user-facing license note.
