param(
    [string]$Target = "x86_64-pc-windows-gnu",
    [string]$Profile = "release",
    [string]$DistDir = "",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot

function Read-CargoVersion {
    param([string]$Manifest)
    foreach ($line in Get-Content $Manifest) {
        if ($line -match '^version = "([^"]+)"') {
            return $Matches[1]
        }
    }
    throw "Could not read version from $Manifest"
}

if ([string]::IsNullOrWhiteSpace($DistDir)) {
    $DistDir = Join-Path $RepoRoot "dist/windows-zip"
}

if (-not $SkipBuild) {
    & (Join-Path $RepoRoot "scripts/build-windows-gnu.ps1") -Target $Target -Profile $Profile
    if ($LASTEXITCODE -ne 0) {
        throw "Windows GNU build failed"
    }
}

$Version = Read-CargoVersion (Join-Path $RepoRoot "Cargo.toml")
$PayloadDir = Join-Path $RepoRoot "dist/windows-$Target-$Profile"
if (-not (Test-Path $PayloadDir)) {
    throw "Windows payload directory not found: $PayloadDir"
}

Remove-Item -Recurse -Force $DistDir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $DistDir | Out-Null

$ZipPath = Join-Path $DistDir "AlmaOneBotBridge-$Version-windows-x86_64.zip"
Compress-Archive -Path (Join-Path $PayloadDir "*") -DestinationPath $ZipPath -Force

$hash = Get-FileHash -Algorithm SHA256 $ZipPath
"$($hash.Hash.ToLowerInvariant())  $(Split-Path -Leaf $ZipPath)" | Set-Content -Path "$ZipPath.sha256" -Encoding ASCII

Write-Host "Built Windows ZIP package:"
Write-Host "  $ZipPath"
