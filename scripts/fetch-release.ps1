<#
.SYNOPSIS
  Download (and, for the FFmpeg build, extract) a public prdt RELEASE binary for
  Windows into a directory you choose.

.DESCRIPTION
  Pulls assets straight from the GitHub Release page — these are public, so no
  `gh` login or token is needed (plain HTTPS). Verifies the sha256 when the
  release ships one. This is different from scripts/fetch-ci-artifacts.ps1,
  which pulls per-run CI *workflow artifacts* via `gh` (auth required).

.PARAMETER Dest
  Directory to download into (created if missing). REQUIRED (positional #1).

.PARAMETER Tag
  Release tag to fetch (e.g. v0.1.2-rustdesk-ux). Default: the newest release,
  INCLUDING pre-releases (the plain /releases/latest API skips pre-releases,
  which these tags are, so we list all and take the newest).

.PARAMETER Ffmpeg
  Fetch prdt-windows-x86_64-ffmpeg.zip and extract it (prdt.exe + FFmpeg DLLs),
  instead of the bare Media-Foundation prdt-windows-x86_64.exe.

.PARAMETER Repo
  owner/repo. Default: drian320/power-remote-dt.

.EXAMPLE
  ./fetch-release.ps1 C:\prdt
  ./fetch-release.ps1 -Dest C:\prdt -Ffmpeg
  ./fetch-release.ps1 C:\prdt -Tag v0.1.2-rustdesk-ux
#>
param(
    [Parameter(Mandatory = $true, Position = 0)] [string]$Dest,
    [string]$Tag = '',
    [switch]$Ffmpeg,
    [string]$Repo = 'drian320/power-remote-dt'
)

$ErrorActionPreference = 'Stop'
try { [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 } catch {}

function Resolve-LatestTag([string]$repo) {
    # /releases/latest skips pre-releases; these tags ARE pre-releases, so list
    # all releases (newest first) and take the first tag_name.
    $rels = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases" `
        -Headers @{ 'User-Agent' = 'prdt-fetch-release' }
    if (-not $rels -or @($rels).Count -eq 0) { throw "no releases found for $repo" }
    return @($rels)[0].tag_name
}

if (-not $Tag) {
    $Tag = Resolve-LatestTag $Repo
    Write-Host "Latest release: $Tag"
}

New-Item -ItemType Directory -Force -Path $Dest | Out-Null
$base = "https://github.com/$Repo/releases/download/$Tag"

function Get-AssetVerified([string]$name) {
    $out = Join-Path $Dest $name
    Write-Host "Downloading $name ..."
    Invoke-WebRequest -Uri "$base/$name" -OutFile $out -UseBasicParsing

    $shaFile = "$out.sha256"
    $haveSha = $false
    try { Invoke-WebRequest -Uri "$base/$name.sha256" -OutFile $shaFile -UseBasicParsing; $haveSha = $true } catch {}
    if ($haveSha) {
        $expected = ((Get-Content -Raw -Path $shaFile).Trim() -split '\s+')[0].ToLower()
        $actual = (Get-FileHash -Algorithm SHA256 $out).Hash.ToLower()
        if ($expected -ne $actual) {
            throw "sha256 MISMATCH for ${name}: expected $expected, got $actual"
        }
        Write-Host "  sha256 OK"
    }
    else {
        Write-Warning "no .sha256 published for $name; skipping integrity check"
    }
    return $out
}

if ($Ffmpeg) {
    $zip = Get-AssetVerified 'prdt-windows-x86_64-ffmpeg.zip'
    $extractDir = Join-Path $Dest 'prdt-ffmpeg'
    Write-Host "Extracting to $extractDir ..."
    Expand-Archive -Path $zip -DestinationPath $extractDir -Force
    Write-Host ""
    Write-Host "Done. Run:  $extractDir\prdt.exe"
    Write-Host "(keep the FFmpeg DLLs next to prdt.exe — they load at runtime)"
}
else {
    $exe = Get-AssetVerified 'prdt-windows-x86_64.exe'
    Write-Host ""
    Write-Host "Done. Run:  $exe"
}
