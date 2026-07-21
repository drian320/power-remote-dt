<#
.SYNOPSIS
  Automated E2E smoke test for `prdt` -- loopback (single machine) or
  two-box (Win<->Linux) host/viewer.

.DESCRIPTION
  Starts `prdt host` (and, in loopback mode, `prdt connect` too), waits for
  the viewer to report a decoded frame, and exits 0/non-zero accordingly.
  Distinguishes several failure states instead of a single pass/fail bit:

    exit 0  PASS  -- viewer decoded at least one frame (textures_decoded > 0
                     in a "viewer rx stats" log line).
    exit 1  FAIL  -- Noise handshake completed and packets arrived at the
                     viewer, but no frame was ever decoded (codec/decoder
                     mismatch, or a decode-path bug).
    exit 2  FAIL  -- Noise handshake completed but the viewer received zero
                     video packets (loopback mode: the host itself never
                     logged "first frame ready" -- almost always a
                     screen-capture problem on the host, e.g. no interactive
                     desktop session for DXGI Desktop Duplication / PipeWire
                     portal to attach to).
    exit 3  FAIL  -- the viewer's decoder backend failed to initialize, e.g.
                     "nvdec requested but built without prdt_nvdec_bindings
                     cfg" (crates/viewer/src/platform/win.rs:421) when the
                     resolved --decoder names a backend this build doesn't
                     have compiled in. See the -Decoder parameter notes
                     below for why this script always passes --decoder
                     explicitly.
    exit 4  FAIL  -- the viewer process never reported a completed Noise
                     handshake (network/transport/auth failure, or it
                     crashed on startup).

  The decode marker is viewer-side (crates/viewer/src/lib.rs ~2308-2329):
  the recv loop logs `"viewer rx stats"` roughly once a second with a
  `textures_decoded=N` field that increments on every frame successfully
  decoded and published to the renderer, on both the Windows (MF/NVDEC/
  OpenH264 consumer) and Linux (OpenH264 / FFmpeg HEVC) paths. That is a
  clearer, decode-verified signal than the host-side "first frame ready"
  line (crates/host/src/lib.rs:1177), which only proves the CAPTURE+ENCODE
  side produced a frame -- this script uses the host-side line only as a
  secondary signal to distinguish exit 1 vs exit 2 in loopback mode.

.PARAMETER Mode
  loopback (default, single machine, fully automated) | host (start a host
  here, print the paste-on-viewer-box command) | viewer (connect to a peer
  host started elsewhere).

.PARAMETER BinPath
  Path to a `prdt`/`prdt-windows-x86_64.exe` binary. Auto-detected from
  .smoke-artifacts/windows/ or smoke/ if omitted.

.PARAMETER Port
  Port to bind (loopback/host modes). Default 19000.

.PARAMETER BindHost
  Override the listen host. Default: 127.0.0.1 for loopback, 0.0.0.0 for host mode.

.PARAMETER PeerAddr
  viewer mode: the remote host's `ip:port` (as printed by `-Mode host`).

.PARAMETER PeerPubkey
  viewer mode: the remote host's pubkey (as printed by `-Mode host`).

.PARAMETER HostId
  TODO(signaling): opaque host id for the --signaling-url rendezvous path
  (Task #2, ID/PIN provisioning). Wired through to --host-id but not yet
  exercised by this harness.

.PARAMETER Pin
  PIN to supply for host-side PIN auth (--pin). Also used by the
  TODO(signaling) ID/PIN path once available.

.PARAMETER SignalingUrl
  TODO(signaling): --signaling-url passthrough for host/viewer modes, for
  rendezvous via a signaling server instead of a manually-shared IP:port.

.PARAMETER Encoder
  Host `--encoder`. Default: auto.

.PARAMETER Decoder
  Viewer `--decoder`. Default: auto. This script always passes --decoder
  explicitly (never relies on the Args default) because `prdt` overlays a
  per-user `%APPDATA%/prdt/config.toml` onto unset CLI flags -- on a machine
  where the GUI launcher has been used interactively, that file can carry a
  stale `[viewer] decoder = "..."` from a previous session, and if that
  value names a backend this build doesn't have compiled in (e.g. an
  ffmpeg-* or literal "nvdec" variant without prdt_nvdec_bindings), viewer
  startup hard-errors ("decoder backend init failed: ...", exit code 3
  below) purely because the flag was left unset. Passing --decoder
  explicitly (as this script does) sidesteps that trap entirely.

.PARAMETER Codec
  Viewer `--codec`. Default: auto.

.PARAMETER BitrateMbps
  Host `--bitrate-mbps` / viewer `--bitrate-mbps` upper clamp. Default: 30.

.PARAMETER WarmupSec
  loopback mode: seconds to wait for the host to print its pubkey before
  giving up. Default: 5.

.PARAMETER ConnectSec
  Seconds to wait for the decode marker before declaring FAIL. Default: 20.

.PARAMETER SilentAllow
  Pass --silent-allow to the host (bypasses the consent gate so an unknown
  viewer key is auto-accepted). Default: $true. Set -SilentAllow:$false to
  require the viewer's key to already be in --known-peers-file.

.PARAMETER OutDir
  Work dir for logs/keys. Default: .smoke-artifacts/e2e-<timestamp>.

.EXAMPLE
  ./scripts/e2e-smoke.ps1 -BinPath smoke/prdt-windows-x86_64.exe -Decoder mf
  # single-machine loopback smoke, explicit binary + decoder workaround

.EXAMPLE
  ./scripts/e2e-smoke.ps1 -Mode host -Port 9000
  # start a host here; paste the printed command on the other box

.EXAMPLE
  ./scripts/e2e-smoke.ps1 -Mode viewer -PeerAddr 192.168.1.20:9000 -PeerPubkey <b64> -Decoder mf
  # connect to a host started elsewhere
#>
[CmdletBinding()]
param(
    [ValidateSet("loopback", "host", "viewer")]
    [string]$Mode = "loopback",
    [string]$BinPath,
    [int]$Port = 19000,
    [string]$BindHost,
    [string]$PeerAddr,
    [string]$PeerPubkey,
    [string]$HostId,
    [string]$Pin,
    [string]$SignalingUrl,
    [string]$Encoder = "auto",
    [string]$Decoder = "auto",
    [string]$Codec = "auto",
    [int]$BitrateMbps = 30,
    [int]$WarmupSec = 5,
    [int]$ConnectSec = 20,
    [bool]$SilentAllow = $true,
    [string]$OutDir
)

$ErrorActionPreference = "Stop"
$root = Resolve-Path "$PSScriptRoot/.."
Set-Location $root

function Resolve-BinPath([string]$p) {
    if ($p) {
        if (-not (Test-Path $p)) { throw "BinPath not found: $p" }
        return (Resolve-Path $p).Path
    }
    $candidates = @(
        ".smoke-artifacts/windows/prdt-windows-x86_64.exe",
        "smoke/prdt-windows-x86_64.exe"
    )
    foreach ($c in $candidates) {
        if (Test-Path $c) { return (Resolve-Path $c).Path }
    }
    throw "No prdt binary found. Pass -BinPath, or run scripts/fetch-ci-artifacts.ps1 first."
}

$prdt = Resolve-BinPath $BinPath
Write-Host "Binary: $prdt"

if (-not $OutDir) {
    $OutDir = ".smoke-artifacts/e2e-$(Get-Date -Format 'yyyyMMdd-HHmmss')"
}
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path
Write-Host "Work dir: $OutDir"

$env:RUST_LOG = "info"

# tracing emits ANSI color codes around structured fields; strip them before
# regex matching (mirrors scripts/first-frame-latency.ps1's approach).
function Strip-Ansi([string]$s) { $s -replace "`e\[[0-9;]*m", "" }

function Get-DecodeVerdict([string]$viewerLogPath) {
    $result = [ordered]@{
        HandshakeOk     = $false
        FramesReceived  = 0
        TexturesDecoded = 0
        DecoderInitFail = $null
    }
    if (-not (Test-Path $viewerLogPath)) { return $result }
    $lines = Get-Content $viewerLogPath -ErrorAction SilentlyContinue | ForEach-Object { Strip-Ansi $_ }
    foreach ($l in $lines) {
        if ($l -match 'Noise handshake complete') { $result.HandshakeOk = $true }
        if ($l -match 'viewer rx stats' -and $l -match 'frames_received=(\d+)') {
            $n = [int]$matches[1]
            if ($n -gt $result.FramesReceived) { $result.FramesReceived = $n }
        }
        if ($l -match 'viewer rx stats' -and $l -match 'textures_decoded=(\d+)') {
            $n = [int]$matches[1]
            if ($n -gt $result.TexturesDecoded) { $result.TexturesDecoded = $n }
        }
        if (-not $result.DecoderInitFail -and $l -match 'decoder backend init failed.*') {
            $result.DecoderInitFail = $l.Trim()
        }
    }
    return $result
}

function Get-HostFirstFrameMs([string]$hostLogPath) {
    if (-not (Test-Path $hostLogPath)) { return $null }
    $line = Get-Content $hostLogPath -ErrorAction SilentlyContinue |
        ForEach-Object { Strip-Ansi $_ } |
        Where-Object { $_ -match 'first frame ready' } | Select-Object -First 1
    if ($line -and $line -match 'elapsed_ms=(\d+)') { return [int]$matches[1] }
    return $null
}

function Write-Verdict([hashtable]$v, [Nullable[int]]$firstFrameMs, [string[]]$logPaths) {
    Write-Host ""
    if ($v.TexturesDecoded -gt 0) {
        Write-Host "PASS: viewer decoded $($v.TexturesDecoded) frame(s) (frames_received=$($v.FramesReceived))." -ForegroundColor Green
        if ($null -ne $firstFrameMs) { Write-Host "  host first-frame latency: ${firstFrameMs}ms" }
        Write-Host "  logs: $($logPaths -join ' , ')"
        return 0
    }
    if ($v.DecoderInitFail) {
        Write-Host "FAIL (decoder init): $($v.DecoderInitFail)" -ForegroundColor Red
        Write-Host "  The resolved --decoder names a backend this build lacks. Try -Decoder mf or -Decoder auto."
        Write-Host "  logs: $($logPaths -join ' , ')"
        return 3
    }
    if (-not $v.HandshakeOk) {
        Write-Host "FAIL (connect): viewer never logged 'Noise handshake complete'." -ForegroundColor Red
        Write-Host "  logs: $($logPaths -join ' , ')"
        return 4
    }
    if ($v.FramesReceived -gt 0) {
        Write-Host "FAIL (no decode): viewer received $($v.FramesReceived) video packet(s) but decoded none." -ForegroundColor Red
        Write-Host "  Likely a codec/decoder mismatch or decode-path bug, not a network issue."
        Write-Host "  logs: $($logPaths -join ' , ')"
        return 1
    }
    if ($null -ne $firstFrameMs) {
        Write-Host "FAIL (no packets): host captured+encoded a frame (first-frame ${firstFrameMs}ms) but zero video packets reached the viewer." -ForegroundColor Red
        Write-Host "  Likely a network/transport issue between host and viewer."
        Write-Host "  logs: $($logPaths -join ' , ')"
        return 2
    }
    Write-Host "FAIL (no capture): connected, but the host never captured a first frame ('first frame ready' absent from host log)." -ForegroundColor Red
    Write-Host "  This usually means desktop/screen capture is producing nothing in this environment"
    Write-Host "  (e.g. no interactive/attached desktop session for DXGI Desktop Duplication / an xdg portal)."
    Write-Host "  logs: $($logPaths -join ' , ')"
    return 2
}

function Get-LanIPv4 {
    try {
        $ip = Get-NetIPAddress -AddressFamily IPv4 -ErrorAction Stop |
            Where-Object {
                $_.IPAddress -notlike '127.*' -and $_.IPAddress -notlike '169.254.*' -and
                $_.InterfaceAlias -notmatch 'Loopback'
            } | Select-Object -First 1 -ExpandProperty IPAddress
        if ($ip) { return $ip }
    } catch {}
    return "<THIS-HOST-IP>"
}

switch ($Mode) {
    "loopback" {
        if (-not $BindHost) { $BindHost = "127.0.0.1" }
        $bindAddr = "${BindHost}:${Port}"
        $hostLog = Join-Path $OutDir "host.log"
        $viewerLog = Join-Path $OutDir "viewer.log"

        $hostArgs = @(
            "host", "--bind", $bindAddr, "--headless", "--encoder", $Encoder,
            "--bitrate-mbps", $BitrateMbps,
            "--key-file", (Join-Path $OutDir "host-key.bin"),
            "--known-peers-file", (Join-Path $OutDir "known-peer-ids"),
            "--host-auth-file", (Join-Path $OutDir "host-auth.toml"),
            "--host-peers-file", (Join-Path $OutDir "host-peers.toml")
        )
        if ($SilentAllow) { $hostArgs += "--silent-allow" }
        if ($Pin) { $hostArgs += @("--pin", $Pin) }

        Write-Host "Starting host: $bindAddr"
        $hostProc = Start-Process -FilePath $prdt -ArgumentList $hostArgs `
            -RedirectStandardOutput $hostLog -RedirectStandardError "$hostLog.err" `
            -NoNewWindow -PassThru

        $viewerProc = $null
        try {
            $pubkey = $null
            $deadline = (Get-Date).AddSeconds($WarmupSec)
            while ((Get-Date) -lt $deadline -and -not $pubkey) {
                Start-Sleep -Milliseconds 300
                if ($hostProc.HasExited) { break }
                if (Test-Path $hostLog) {
                    $m = Select-String -Path $hostLog -Pattern 'Host public key: (\S+)' -ErrorAction SilentlyContinue | Select-Object -First 1
                    if ($m) { $pubkey = $m.Matches[0].Groups[1].Value }
                }
            }
            if ($hostProc.HasExited) {
                Write-Host "FAIL (host crashed on startup). Log:" -ForegroundColor Red
                Get-Content $hostLog -ErrorAction SilentlyContinue | Write-Host
                exit 5
            }
            if (-not $pubkey) { throw "host did not print its public key within ${WarmupSec}s; see $hostLog" }
            Write-Host "Host pubkey: $pubkey"

            $viewerArgs = @(
                "connect", "--host", $bindAddr, "--host-pubkey", $pubkey, "--headless",
                "--decoder", $Decoder, "--codec", $Codec, "--bitrate-mbps", $BitrateMbps,
                "--viewer-key-file", (Join-Path $OutDir "viewer-key.bin"),
                "--recv-dir", (Join-Path $OutDir "received")
            )
            if ($Pin) { $viewerArgs += @("--pin", $Pin) }

            Write-Host "Starting viewer -> ${bindAddr}"
            $viewerProc = Start-Process -FilePath $prdt -ArgumentList $viewerArgs `
                -RedirectStandardOutput $viewerLog -RedirectStandardError "$viewerLog.err" `
                -NoNewWindow -PassThru

            $deadline = (Get-Date).AddSeconds($ConnectSec)
            $v = Get-DecodeVerdict $viewerLog
            while ((Get-Date) -lt $deadline -and $v.TexturesDecoded -eq 0 -and -not $v.DecoderInitFail) {
                Start-Sleep -Seconds 1
                $v = Get-DecodeVerdict $viewerLog
            }
        } finally {
            if ($viewerProc -and -not $viewerProc.HasExited) { Stop-Process -Id $viewerProc.Id -Force -ErrorAction SilentlyContinue }
            if (-not $hostProc.HasExited) { Stop-Process -Id $hostProc.Id -Force -ErrorAction SilentlyContinue }
            Start-Sleep -Milliseconds 300
        }

        $firstFrameMs = Get-HostFirstFrameMs $hostLog
        $code = Write-Verdict $v $firstFrameMs @($hostLog, $viewerLog)
        exit $code
    }

    "host" {
        if (-not $BindHost) { $BindHost = "0.0.0.0" }
        $bindAddr = "${BindHost}:${Port}"
        $hostLog = Join-Path $OutDir "host.log"

        $hostArgs = @(
            "host", "--bind", $bindAddr, "--headless", "--encoder", $Encoder,
            "--bitrate-mbps", $BitrateMbps,
            "--key-file", (Join-Path $OutDir "host-key.bin"),
            "--known-peers-file", (Join-Path $OutDir "known-peer-ids"),
            "--host-auth-file", (Join-Path $OutDir "host-auth.toml"),
            "--host-peers-file", (Join-Path $OutDir "host-peers.toml")
        )
        if ($SilentAllow) { $hostArgs += "--silent-allow" }
        if ($Pin) { $hostArgs += @("--pin", $Pin) }
        if ($SignalingUrl) {
            # TODO(signaling): once Task #2's ID/PIN provisioning ships, this
            # lets the host register with a signaling server under --host-id
            # instead of requiring a manually-shared IP:port. Wired but
            # undocumented/untested by this harness until a signaling server
            # is reachable in this environment.
            $hostArgs += @("--signaling-url", $SignalingUrl)
            if ($HostId) { $hostArgs += @("--host-id", $HostId) }
        }

        Write-Host "Starting host on $bindAddr (PID will print below). Ctrl+C to stop."
        $hostProc = Start-Process -FilePath $prdt -ArgumentList $hostArgs `
            -RedirectStandardOutput $hostLog -RedirectStandardError "$hostLog.err" `
            -NoNewWindow -PassThru
        Write-Host "PID: $($hostProc.Id)  log: $hostLog"

        $printed = $false
        $lastCount = 0
        try {
            while (-not $hostProc.HasExited) {
                Start-Sleep -Milliseconds 500
                if (-not (Test-Path $hostLog)) { continue }
                $content = @(Get-Content $hostLog -ErrorAction SilentlyContinue)
                if ($content.Count -gt $lastCount) {
                    $content[$lastCount..($content.Count - 1)] | ForEach-Object { Write-Host $_ }
                    $lastCount = $content.Count
                }
                if (-not $printed) {
                    $m = $content | Where-Object { $_ -match 'Host public key: (\S+)' } | Select-Object -First 1
                    if ($m -and $m -match 'Host public key: (\S+)') {
                        $pubkey = $matches[1]
                        $lanIp = Get-LanIPv4
                        Write-Host ""
                        Write-Host "===================================================================="
                        Write-Host "Paste on the VIEWER box (verify ${lanIp} is the right reachable IP):"
                        Write-Host ""
                        Write-Host "  Windows viewer:"
                        Write-Host "    ./scripts/e2e-smoke.ps1 -Mode viewer -PeerAddr ${lanIp}:${Port} -PeerPubkey $pubkey -Decoder mf"
                        Write-Host ""
                        Write-Host "  Linux viewer:"
                        Write-Host "    ./scripts/e2e-smoke.sh viewer --peer-addr ${lanIp}:${Port} --peer-pubkey $pubkey"
                        Write-Host "===================================================================="
                        Write-Host ""
                        $printed = $true
                    }
                }
            }
            Write-Host "Host process exited on its own (code $($hostProc.ExitCode))."
        } finally {
            if (-not $hostProc.HasExited) { Stop-Process -Id $hostProc.Id -Force -ErrorAction SilentlyContinue }
        }
    }

    "viewer" {
        $haveDirect = [bool]$PeerAddr
        $haveSignaling = [bool]$SignalingUrl -and [bool]$HostId
        if (-not $haveDirect -and -not $haveSignaling) {
            throw "viewer mode needs -PeerAddr (+ -PeerPubkey), or -SignalingUrl + -HostId [+ -Pin] (TODO(signaling) path)"
        }
        $viewerLog = Join-Path $OutDir "viewer.log"
        $viewerArgs = @(
            "connect", "--headless", "--decoder", $Decoder, "--codec", $Codec,
            "--bitrate-mbps", $BitrateMbps,
            "--viewer-key-file", (Join-Path $OutDir "viewer-key.bin"),
            "--recv-dir", (Join-Path $OutDir "received")
        )
        if ($haveDirect) {
            $viewerArgs += @("--host", $PeerAddr)
            if ($PeerPubkey) { $viewerArgs += @("--host-pubkey", $PeerPubkey) }
        }
        if ($SignalingUrl) {
            # TODO(signaling): Task #2 ID/PIN provisioning path -- wired
            # through but not yet exercised by this harness (no signaling
            # server was reachable in this environment when this was written).
            $viewerArgs += @("--signaling-url", $SignalingUrl)
            if ($HostId) { $viewerArgs += @("--host-id", $HostId) }
        }
        if ($Pin) { $viewerArgs += @("--pin", $Pin) }

        Write-Host "Starting viewer -> $(if ($haveDirect) { $PeerAddr } else { "signaling:$HostId" })"
        $viewerProc = Start-Process -FilePath $prdt -ArgumentList $viewerArgs `
            -RedirectStandardOutput $viewerLog -RedirectStandardError "$viewerLog.err" `
            -NoNewWindow -PassThru
        try {
            $deadline = (Get-Date).AddSeconds($ConnectSec)
            $v = Get-DecodeVerdict $viewerLog
            while ((Get-Date) -lt $deadline -and $v.TexturesDecoded -eq 0 -and -not $v.DecoderInitFail) {
                Start-Sleep -Seconds 1
                $v = Get-DecodeVerdict $viewerLog
            }
        } finally {
            if (-not $viewerProc.HasExited) { Stop-Process -Id $viewerProc.Id -Force -ErrorAction SilentlyContinue }
        }
        $code = Write-Verdict $v $null @($viewerLog)
        exit $code
    }
}
