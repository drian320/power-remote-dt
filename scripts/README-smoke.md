# CI-artifact E2E smoke harness

Automated download + smoke-test of `prdt` binaries built by
`.github/workflows/release.yml`, for the AC-1 acceptance (Win<->Linux
two-machine E2E). Two script pairs, each with a Windows (`.ps1`) and Linux/
bash (`.sh`) implementation:

| Script | Purpose |
| --- | --- |
| `fetch-ci-artifacts.ps1` / `.sh` | Download a Release-workflow build via `gh`, verify sha256 |
| `e2e-smoke.ps1` / `.sh` | Run host+viewer and verify a decoded frame, loopback or two-box |

Both scripts always pass `--decoder`/`--encoder`/`--codec` explicitly to
`prdt` (never relying on its own defaults) -- see "Known gotcha" below for
why.

## 1. Fetch a CI build

`.github/workflows/release.yml` uploads workflow artifacts on **every**
run, tag push or `workflow_dispatch`, so this works against a branch that
has never been tagged.

```powershell
# Windows: latest successful Release run on the current branch
./scripts/fetch-ci-artifacts.ps1

# Kick a fresh CI build of a specific branch, wait for it, then download
./scripts/fetch-ci-artifacts.ps1 -Ref feat/rustdesk-ux -Dispatch

# Also grab the Linux artifact, e.g. to stage both sides of a two-box test
# from one machine
./scripts/fetch-ci-artifacts.ps1 -Os both
```

```bash
# Linux: latest successful Release run on the current branch
scripts/fetch-ci-artifacts.sh

# Fresh CI build of a branch, wait, download
scripts/fetch-ci-artifacts.sh --ref feat/rustdesk-ux --dispatch

# AppImage instead of the bare ELF (VAAPI+NVENC+NVDEC+Main10 feature set)
scripts/fetch-ci-artifacts.sh --os linux-appimage
```

Artifacts land under `.smoke-artifacts/{windows,linux,linux-appimage}/` and
are sha256-verified against the `.sha256` file GitHub Actions uploaded
alongside the binary -- the scripts fail loudly on any mismatch. On Linux
the downloaded binary is `chmod +x`'d automatically.

Requires the `gh` CLI, authenticated (`gh auth login`). Both scripts check
for this up front and fail with a clear message if `gh` is missing or
unauthenticated.

## 2. Loopback smoke (single machine, fully automated)

Starts a host and a viewer on `127.0.0.1`, waits for the viewer to report a
decoded frame, and exits 0/non-zero:

```powershell
./scripts/e2e-smoke.ps1 -BinPath .smoke-artifacts/windows/prdt-windows-x86_64.exe
```

```bash
scripts/e2e-smoke.sh loopback --bin-path .smoke-artifacts/linux/prdt-linux-x86_64
```

If `-BinPath`/`--bin-path` is omitted, the script looks in
`.smoke-artifacts/{windows,linux}/` and then `smoke/` for a binary.

### Pass/fail signal

The primary marker is **viewer-side**, from the recv loop's periodic
status line (`crates/viewer/src/lib.rs:2308-2329`):

```
INFO prdt_viewer: viewer rx stats frames_received=12 textures_decoded=9 control_received=0 input_received=0 recv_errors=0 timeouts=0
```

`textures_decoded` increments every time a frame is successfully decoded
and published to the renderer (Windows: MF/NVDEC/OpenH264 consumer path;
Linux: OpenH264 / FFmpeg-HEVC path) -- both scripts poll this field and
declare PASS once it exceeds zero. This is a stronger signal than the
host-side `"first frame ready"` line
(`crates/host/src/lib.rs:1177`, `elapsed_ms=N`), which only proves the
**capture+encode** side produced a frame, not that the viewer ever decoded
it. The scripts use the host-side line only as a secondary signal to tell
apart two different failure modes (see below).

### Exit codes

| Code | Meaning |
| --- | --- |
| 0 | PASS -- viewer decoded >= 1 frame |
| 1 | FAIL -- packets reached the viewer but none decoded (codec/decoder mismatch or decode bug) |
| 2 | FAIL -- handshake OK but zero video packets reached the viewer (loopback: host never logged "first frame ready" -- almost always a screen-capture problem) |
| 3 | FAIL -- viewer's decoder backend failed to initialize (resolved `--decoder` names a backend this build lacks) |
| 4 | FAIL -- viewer never completed the Noise handshake |
| 5 | FAIL -- host process crashed on startup |

This distinguishes "connected+decoded" from "host started but produced no
frame" as required for diagnosing capture-vs-network-vs-decode issues
without reading raw logs every time.

### Result on this machine

Running the loopback smoke against the prebuilt `smoke/prdt-windows-x86_64.exe`
on this Windows dev box produced a **consistent, reproducible exit code 2**
across multiple runs: the Noise handshake completes, the host's encoder
initializes (`encoder ready backend="mf" codec="h265"`), but the host never
logs `"first frame ready"` and the viewer's `frames_received`/
`textures_decoded` stay at 0 indefinitely. In other words: **connection and
negotiation work end-to-end; screen capture itself produces nothing.** This
is consistent with this automation environment not having a real
interactive desktop session for DXGI Desktop Duplication to attach to (no
error is raised -- `producer.next_frame().await` simply never resolves).
This is exactly the ambiguity this harness is designed to surface instead
of hanging silently or reporting a false PASS. On a machine with a normal
interactive desktop session (a real Windows box with someone logged in, or
a Linux box with a reachable X11/Wayland display), the same command is
expected to reach exit 0.

## 3. Two-box E2E (Win<->Linux, both directions)

One command per box; the `host` role prints the exact command to paste on
the other box.

### Direction A: Windows hosts, Linux views

On the **Windows** box:

```powershell
./scripts/e2e-smoke.ps1 -Mode host -Port 9000
```

This prints something like:

```
====================================================================
Paste on the VIEWER box (verify 192.168.1.50 is the right reachable IP):

  Windows viewer:
    ./scripts/e2e-smoke.ps1 -Mode viewer -PeerAddr 192.168.1.50:9000 -PeerPubkey <b64> -Decoder mf

  Linux viewer:
    ./scripts/e2e-smoke.sh viewer --peer-addr 192.168.1.50:9000 --peer-pubkey <b64>
====================================================================
```

On the **Linux** box, paste the printed command:

```bash
scripts/e2e-smoke.sh viewer --peer-addr 192.168.1.50:9000 --peer-pubkey <b64-from-host>
```

### Direction B: Linux hosts, Windows views

On the **Linux** box:

```bash
scripts/e2e-smoke.sh host --port 9000
```

On the **Windows** box, paste the printed command:

```powershell
./scripts/e2e-smoke.ps1 -Mode viewer -PeerAddr <linux-ip>:9000 -PeerPubkey <b64-from-host>
```

Both directions use the same exit-code contract as loopback mode (0 =
decoded, 1/2/3/4 = the specific failure state -- see table above). `host`
mode itself has no pass/fail exit code; it just streams the host's log and
prints the paste-on-viewer-box command until you Ctrl+C it.

Firewalls: `host` mode binds `0.0.0.0:<port>` by default (not
`127.0.0.1`), so make sure the chosen UDP port is reachable between the two
boxes (LAN or an open inbound rule).

## 4. Known gotcha: always pass `--decoder`/`--encoder`/`--codec` explicitly

`prdt` overlays a per-user `config.toml` (`%APPDATA%/prdt/config.toml` on
Windows, `~/.config/prdt/config.toml` on Linux) onto any CLI flag left
unset, with precedence "explicit CLI flag > config.toml value > clap
default" (`crates/host/src/lib.rs:268-270`,
`crates/viewer/src/lib.rs` equivalent). On a machine where the integrated
GUI launcher has been used interactively even once, that file can carry a
stale decoder/encoder/codec choice from a previous session. If that stale
value names a backend the currently-running build doesn't have compiled
in, startup hard-errors with `"decoder backend init failed: ..."` (exit
code 3) purely because a flag was left unset -- this was observed directly
on this dev machine (a leftover `[viewer] decoder = "ffmpeg-nvdec-hevc"`
from a prior GUI session). **Both scripts always pass `--decoder`,
`--encoder`, and `--codec` explicitly** for exactly this reason; do the
same in any ad-hoc `prdt host`/`prdt connect` invocation on a machine that
has ever run the GUI.

Separately, note that on Windows the `"decoder ready; spawning worker
tasks backend=..."` log line
(`crates/viewer/src/lib.rs:2209-2212`) reports the *negotiation-guard's*
resolved `DecoderChoice` (from `choose_decoder()`,
`crates/viewer/src/lib.rs:719` maps `("auto", Codec::H265) =>
DecoderChoice::Nvdec` unconditionally), which is **not necessarily** the
consumer actually constructed -- that happens separately in
`platform::win::build_consumer()`
(`crates/viewer/src/platform/win.rs:394-422`), whose `("auto", Codec::H265)`
arm always builds the **MF** consumer regardless of what
`choose_decoder()` logged. In other words: `backend=Nvdec` in that log
line does not reliably mean NVDEC was used. This looks like a real (if
cosmetic) logging/observability inconsistency worth a closer look
separately from this smoke-harness task -- flagging it here rather than
fixing it, since Rust source changes are out of scope for this task.

## 5. `TODO(signaling)` hooks for Task #2 (ID/PIN provisioning)

`e2e-smoke.ps1`/`.sh` `host` and `viewer` modes already accept
`-SignalingUrl`/`--signaling-url`, `-HostId`/`--host-id`, and
`-Pin`/`--pin`, and pass them straight through to `prdt host --signaling-url
... --host-id ...` / `prdt connect --signaling-url ... --host-id ... --pin
...`. These are wired but **not exercised** by this harness -- no signaling
server was reachable in this environment when it was written. Once Task
#2's ID/PIN provisioning ships a running signaling server, the two-box
flow above becomes, in principle:

```powershell
# host box
./scripts/e2e-smoke.ps1 -Mode host -SignalingUrl wss://signaling.example/ws -HostId my-host

# viewer box
./scripts/e2e-smoke.ps1 -Mode viewer -SignalingUrl wss://signaling.example/ws -HostId my-host -Pin 123456
```

This removes the need to manually share an IP:port + pubkey between boxes.
Verify against a real signaling deployment before relying on it.

## 6. Verification performed for this harness

- `gh --version` / `gh auth status`: `gh` 2.88.0, authenticated as
  `drian320` (keyring, scopes include `repo` + `workflow`).
- Loopback smoke actually run against the prebuilt
  `smoke/prdt-windows-x86_64.exe` on this Windows machine: reproducibly
  exits 2 ("no capture") -- see section 2 above for the full analysis.
- `shellcheck` / PSScriptAnalyzer were **not available** in this
  environment (`shellcheck: command not found`; no PSScriptAnalyzer
  module installed) -- both `.sh` scripts were checked with `bash -n`
  (syntax OK) and both `.ps1` scripts were checked with
  `[System.Management.Automation.Language.Parser]::ParseFile` (syntax OK),
  but neither received a full lint pass.
- `gh run list --workflow=release.yml` / `gh run download` were **not**
  exercised against a real run in this session (would require either an
  existing successful Release run for the current branch or spending CI
  minutes on a fresh `-Dispatch`); the download + sha256-verify code path
  is implemented per the documented `gh run download`/`Get-FileHash`/
  `sha256sum -c` semantics but is unverified end-to-end. Recommend a real
  dry run (`./scripts/fetch-ci-artifacts.ps1 -Os windows`) before relying
  on it unattended.
