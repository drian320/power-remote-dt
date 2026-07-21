<#
.SYNOPSIS
  Download prdt's Release-workflow build artifacts via the GitHub CLI and
  verify their sha256, so a CI build can be smoke-tested without a manual
  browser trip to the Actions tab.

.DESCRIPTION
  `.github/workflows/release.yml` uploads workflow artifacts on EVERY run
  (tag push OR workflow_dispatch), so this script works against a branch
  that has never been tagged -- e.g. feat/rustdesk-ux.

  Modes:
    - Default: find the latest SUCCESSFUL Release run for -Ref and download
      from it.
    - -RunId <id>: use that specific run instead of searching.
    - -Dispatch: trigger a brand-new `gh workflow run release.yml --ref
      <Ref>`, wait for it to finish, then download from it. This is the
      "one command kicks CI, waits, downloads" path -- opt-in because it
      costs CI minutes and takes several minutes (Windows + Linux + AppImage
      jobs all build+smoke-test).

  Artifacts (see release.yml):
    prdt-windows-x86_64        -> prdt-windows-x86_64.exe (+ .sha256)
    prdt-linux-x86_64          -> prdt-linux-x86_64 (+ .sha256)
    prdt-linux-x86_64-appimage -> prdt-<version>-x86_64.AppImage (+ .sha256)

.PARAMETER Ref
  Branch or tag to fetch/dispatch for. Default: current git branch.

.PARAMETER RunId
  Use this specific workflow run id instead of searching for the latest
  success. Mutually exclusive with -Dispatch.

.PARAMETER Dispatch
  Trigger a new workflow_dispatch run of Release for -Ref, wait for it, then
  download its artifacts.

.PARAMETER Os
  Which artifact family to download: windows | linux | linux-appimage | both.
  "both" downloads windows + linux (not the AppImage) for cross-box staging.
  Default: windows.

.PARAMETER OutDir
  Download + verify into this directory. Default: .smoke-artifacts

.PARAMETER Repo
  owner/repo. Default: inferred via `gh repo view`.

.PARAMETER TimeoutSec
  Passed to `gh run watch` polling when -Dispatch is set. Default: 1800 (30 min).

.EXAMPLE
  ./scripts/fetch-ci-artifacts.ps1
  # latest successful Release run on the current branch, Windows artifact

.EXAMPLE
  ./scripts/fetch-ci-artifacts.ps1 -Ref feat/rustdesk-ux -Dispatch
  # kick a fresh CI build of this branch, wait for it, then download

.EXAMPLE
  ./scripts/fetch-ci-artifacts.ps1 -Os both
  # also grab the Linux artifact, e.g. to stage a two-box E2E from a Windows box
#>
[CmdletBinding()]
param(
    [string]$Ref,
    [string]$RunId,
    [switch]$Dispatch,
    [ValidateSet("windows", "linux", "linux-appimage", "both")]
    [string]$Os = "windows",
    [string]$OutDir = ".smoke-artifacts",
    [string]$Repo,
    [int]$TimeoutSec = 1800
)

$ErrorActionPreference = "Stop"
$root = Resolve-Path "$PSScriptRoot/.."
Set-Location $root

function Fail([string]$msg) {
    Write-Host "FAIL: $msg" -ForegroundColor Red
    exit 1
}

# --- gh presence + auth --------------------------------------------------
& gh --version *> $null
if ($LASTEXITCODE -ne 0) {
    Fail "GitHub CLI ('gh') not found on PATH. Install: https://cli.github.com/"
}
& gh auth status *> $null
if ($LASTEXITCODE -ne 0) {
    Fail "gh is installed but not authenticated. Run: gh auth login"
}

if (-not $Repo) {
    $Repo = (& gh repo view --json nameWithOwner -q .nameWithOwner)
    if ($LASTEXITCODE -ne 0 -or -not $Repo) {
        Fail "could not infer the repo from the current directory; pass -Repo owner/name"
    }
}
Write-Host "Repo: $Repo"

if (-not $Ref) {
    $Ref = (& git rev-parse --abbrev-ref HEAD).Trim()
}
Write-Host "Ref:  $Ref"

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path

# --- resolve a run id -----------------------------------------------------
if ($Dispatch) {
    if ($RunId) { Fail "-RunId and -Dispatch are mutually exclusive" }

    Write-Host "Dispatching Release workflow for ref '$Ref'..."
    $dispatchAt = (Get-Date).ToUniversalTime()
    & gh workflow run release.yml --repo $Repo --ref $Ref
    if ($LASTEXITCODE -ne 0) { Fail "gh workflow run failed" }

    Write-Host "Waiting for the dispatched run to register with GitHub..."
    $run = $null
    $findDeadline = (Get-Date).AddSeconds(60)
    while ((Get-Date) -lt $findDeadline -and -not $run) {
        Start-Sleep -Seconds 3
        $raw = & gh run list --repo $Repo --workflow=release.yml --branch $Ref `
            --event workflow_dispatch --json databaseId,createdAt --limit 5
        if ($LASTEXITCODE -ne 0) { continue }
        $candidates = $raw | ConvertFrom-Json
        $fresh = $candidates |
            Where-Object { [datetime]$_.createdAt -ge $dispatchAt.AddSeconds(-5) } |
            Sort-Object createdAt -Descending | Select-Object -First 1
        if ($fresh) { $run = $fresh }
    }
    if (-not $run) { Fail "could not find the dispatched run within 60s (check: gh run list --repo $Repo --workflow=release.yml)" }
    $RunId = $run.databaseId
    Write-Host "Run id: $RunId -- waiting (up to ${TimeoutSec}s) for it to finish..."
    & gh run watch $RunId --repo $Repo --exit-status --interval 15
    if ($LASTEXITCODE -ne 0) {
        Fail "Release run $RunId did not succeed. Inspect: gh run view $RunId --repo $Repo --log-failed"
    }
    Write-Host "Run $RunId succeeded."
}
elseif (-not $RunId) {
    Write-Host "Looking up the latest successful Release run for ref '$Ref'..."
    $raw = & gh run list --repo $Repo --workflow=release.yml --branch $Ref `
        --status success --json databaseId,createdAt,url --limit 1
    if ($LASTEXITCODE -ne 0) { Fail "gh run list failed" }
    $json = $raw | ConvertFrom-Json
    if (-not $json -or $json.Count -eq 0) {
        Fail "no successful Release run found for ref '$Ref'. Use -Dispatch to trigger one, or pass -RunId <id>."
    }
    $RunId = $json[0].databaseId
    Write-Host "Using run $RunId ($($json[0].url))"
}
else {
    Write-Host "Using explicit run id $RunId"
}

# --- download + verify -----------------------------------------------------
function Get-Artifact([string]$name, [string]$destSubdir) {
    $dest = Join-Path $OutDir $destSubdir
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    Write-Host "Downloading artifact '$name' -> $dest"
    & gh run download $RunId --repo $Repo --name $name --dir $dest
    if ($LASTEXITCODE -ne 0) { Fail "gh run download failed for artifact '$name' (run $RunId). Was the workflow run built from a commit old enough to not have this artifact?" }
    return $dest
}

function Test-Sha256([string]$dir, [string]$binFile, [string]$shaFile) {
    $shaPath = Join-Path $dir $shaFile
    $binPath = Join-Path $dir $binFile
    if (-not (Test-Path $shaPath)) { Fail "missing $shaFile next to $binFile in $dir" }
    if (-not (Test-Path $binPath)) { Fail "missing $binFile in $dir" }
    $raw = (Get-Content $shaPath -Raw)
    if ($raw -notmatch '([0-9a-fA-F]{64})') {
        Fail "could not parse a sha256 hash out of $shaPath"
    }
    $expected = $matches[1].ToLower()
    $actual = (Get-FileHash -Algorithm SHA256 $binPath).Hash.ToLower()
    if ($expected -ne $actual) {
        Fail "sha256 MISMATCH for $binFile`n  expected: $expected`n  actual:   $actual"
    }
    Write-Host "sha256 OK: $binFile ($actual)"
}

$downloaded = [ordered]@{}

$wantWindows = ($Os -eq "windows" -or $Os -eq "both")
$wantLinux = ($Os -eq "linux" -or $Os -eq "both")
$wantAppImage = ($Os -eq "linux-appimage")

if ($wantWindows) {
    $dir = Get-Artifact "prdt-windows-x86_64" "windows"
    Test-Sha256 $dir "prdt-windows-x86_64.exe" "prdt-windows-x86_64.exe.sha256"
    $downloaded["windows"] = Join-Path $dir "prdt-windows-x86_64.exe"
}
if ($wantLinux) {
    $dir = Get-Artifact "prdt-linux-x86_64" "linux"
    Test-Sha256 $dir "prdt-linux-x86_64" "prdt-linux-x86_64.sha256"
    $downloaded["linux"] = Join-Path $dir "prdt-linux-x86_64"
}
if ($wantAppImage) {
    $dir = Get-Artifact "prdt-linux-x86_64-appimage" "linux-appimage"
    $appImg = Get-ChildItem $dir -Filter "*.AppImage" | Select-Object -First 1
    if (-not $appImg) { Fail "no .AppImage file found inside the downloaded artifact directory: $dir" }
    Test-Sha256 $dir $appImg.Name "$($appImg.Name).sha256"
    $downloaded["linux-appimage"] = $appImg.FullName
}

Write-Host ""
Write-Host "Downloaded + sha256-verified:"
foreach ($k in $downloaded.Keys) { Write-Host "  ${k}: $($downloaded[$k])" }
Write-Host ""
Write-Host "Next: ./scripts/e2e-smoke.ps1 -BinPath `"$($downloaded['windows'])`""
