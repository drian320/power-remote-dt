param(
    [int]$Cycles = 20,
    [int]$WarmupSec = 4,
    [int]$ConnectSec = 12,
    [string]$OutCsv = "bench-out/first-frame-latency.csv",
    [string]$Pubkey = "pBfwMy6qXBDbEyY0nwzoDyFOtJHbWtTNqZxdUjQD9C0",
    [string]$BindAddr = "127.0.0.1:9000",
    [int]$BitrateMbps = 20
)

$ErrorActionPreference = "Stop"
$root = Resolve-Path "$PSScriptRoot/.."
Set-Location $root

$env:NV_CODEC_SDK_PATH = "C:/SDK/Video_Codec_SDK_13.0.37"
$env:LIBCLANG_PATH = "C:/Program Files/LLVM/bin"
$env:CUDA_PATH = "C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
$env:RUST_LOG = "info"

$hostExe = Join-Path $root "target/release/prdt-host.exe"
$viewerExe = Join-Path $root "target/release/prdt-viewer.exe"
if (-not (Test-Path $hostExe)) { throw "host binary not found at $hostExe; run cargo build --release first" }
if (-not (Test-Path $viewerExe)) { throw "viewer binary not found at $viewerExe; run cargo build --release first" }

$outDir = Split-Path -Parent $OutCsv
if ($outDir -and -not (Test-Path $outDir)) { New-Item -ItemType Directory -Path $outDir | Out-Null }
"cycle,elapsed_ms,outcome" | Set-Content -Path $OutCsv -Encoding utf8

$results = @()
for ($i = 1; $i -le $Cycles; $i++) {
    Write-Host "[${i}/${Cycles}] starting host..."
    $hostLog = "host-ffl-$i.log"
    $viewerLog = "viewer-ffl-$i.log"
    Remove-Item $hostLog,$viewerLog -ErrorAction SilentlyContinue

    $hostProc = Start-Process -FilePath $hostExe `
        -ArgumentList "--bind",$BindAddr,"--monitor","0","--bitrate-mbps","$BitrateMbps","--key-file","host-key.bin","--encoder","auto","--headless" `
        -RedirectStandardOutput $hostLog -RedirectStandardError "$hostLog.err" `
        -NoNewWindow -PassThru

    Start-Sleep -Seconds $WarmupSec

    $viewerProc = Start-Process -FilePath $viewerExe `
        -ArgumentList "--host",$BindAddr,"--host-pubkey",$Pubkey,"--headless" `
        -RedirectStandardOutput $viewerLog -RedirectStandardError "$viewerLog.err" `
        -NoNewWindow -PassThru

    Start-Sleep -Seconds $ConnectSec

    Stop-Process -Id $viewerProc.Id -Force -ErrorAction SilentlyContinue
    Stop-Process -Id $hostProc.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500

    $line = Select-String -Path $hostLog -Pattern 'first frame ready' -SimpleMatch | Select-Object -First 1
    if ($null -eq $line) {
        Write-Host "  cycle ${i}: NO first-frame-ready log line"
        Add-Content -Path $OutCsv -Value "$i,,no-first-frame"
        $results += [PSCustomObject]@{cycle=$i; elapsed_ms=$null; outcome="no-first-frame"}
        continue
    }
    # Strip ANSI escape sequences (tracing emits them around structured fields).
    $clean = $line.Line -replace "`e\[[0-9;]*m", ""
    if ($clean -match 'elapsed_ms=(\d+)') {
        $ms = [int]$matches[1]
        Write-Host "  cycle ${i}: first frame in ${ms}ms"
        Add-Content -Path $OutCsv -Value "$i,$ms,ok"
        $results += [PSCustomObject]@{cycle=$i; elapsed_ms=$ms; outcome="ok"}
    } else {
        Write-Host "  cycle ${i}: matched line but no elapsed_ms parse: $clean"
        Add-Content -Path $OutCsv -Value "$i,,parse-error"
        $results += [PSCustomObject]@{cycle=$i; elapsed_ms=$null; outcome="parse-error"}
    }
}

$ok = $results | Where-Object { $_.outcome -eq 'ok' }
if ($ok.Count -eq 0) {
    Write-Host "FAIL: no successful cycles"
    exit 2
}
$mx = ($ok | Measure-Object -Property elapsed_ms -Maximum).Maximum
$mn = ($ok | Measure-Object -Property elapsed_ms -Minimum).Minimum
$avg = [math]::Round(($ok | Measure-Object -Property elapsed_ms -Average).Average, 1)
Write-Host ""
Write-Host "Summary across $($ok.Count) successful cycles (of $Cycles):"
Write-Host "  min     = ${mn} ms"
Write-Host "  max     = ${mx} ms  (Phase 4 acceptance: <= 500 ms)"
Write-Host "  mean    = ${avg} ms"
Write-Host "Wrote: $OutCsv"
if ($mx -gt 500) {
    Write-Host "FAIL: max (${mx} ms) exceeds 500 ms threshold"
    exit 1
}
Write-Host "PASS: max (${mx} ms) within 500 ms threshold"
exit 0
