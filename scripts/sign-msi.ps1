# Phase 4 G5 — Sign a Power Remote Desktop MSI with Authenticode.
# Requires Windows SDK signtool.exe in PATH.
param(
    [Parameter(Mandatory=$true)] [string]$CertPath,
    [Parameter(Mandatory=$true)] [string]$CertPassword,
    [Parameter(Mandatory=$true)] [string]$MsiPath,
    [string]$TimestampUrl = "http://timestamp.digicert.com",
    [string]$Description = "Power Remote Desktop"
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $CertPath)) {
    throw "Certificate file not found: $CertPath"
}
if (-not (Test-Path $MsiPath)) {
    throw "MSI not found: $MsiPath"
}

$signtool = (Get-Command signtool.exe -ErrorAction SilentlyContinue).Source
if (-not $signtool) {
    throw "signtool.exe not in PATH. Install Windows SDK or add the SDK bin dir to PATH."
}

Write-Host "Signing $MsiPath..."
& $signtool sign `
    /f $CertPath `
    /p $CertPassword `
    /tr $TimestampUrl `
    /td sha256 `
    /fd sha256 `
    /d $Description `
    /v `
    $MsiPath
if ($LASTEXITCODE -ne 0) {
    throw "signtool sign failed (exit $LASTEXITCODE)"
}

Write-Host "Verifying signature..."
& $signtool verify /pa /v $MsiPath
if ($LASTEXITCODE -ne 0) {
    throw "signtool verify failed (exit $LASTEXITCODE)"
}

Write-Host "Successfully signed and verified $MsiPath"
