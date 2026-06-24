param(
    [string]$Target = "x86_64-pc-windows-gnu",
    [string]$Profile = "release",
    [string]$DistDir = ""
)

$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
$AppName = "AlmaOneBotBridge"
$PackId = "AlmaOneBotBridge"
$PackTitle = "Alma OneBot Bridge"
$PackAuthors = "星光の殲滅者"
$DotnetToolsDir = Join-Path $RepoRoot "target/dotnet-tools"

function Read-CargoVersion {
    param([string]$Manifest)
    foreach ($line in Get-Content $Manifest) {
        if ($line -match '^version = "([^"]+)"') {
            return $Matches[1]
        }
    }
    throw "Could not read version from $Manifest"
}

function Add-PathEntry {
    param([string]$PathEntry)
    if ($env:Path -notlike "*$PathEntry*") {
        $env:Path = "$PathEntry;$env:Path"
    }
}

if ([string]::IsNullOrWhiteSpace($DistDir)) {
    $DistDir = Join-Path $RepoRoot "dist/windows-velopack"
}

& (Join-Path $RepoRoot "scripts/build-windows-gnu.ps1") -Target $Target -Profile $Profile
if ($LASTEXITCODE -ne 0) {
    throw "Windows GNU build failed"
}

Add-PathEntry $DotnetToolsDir
if (-not (Get-Command dotnet -ErrorAction SilentlyContinue)) {
    throw "dotnet SDK 8 is required to install and run Velopack vpk"
}

New-Item -ItemType Directory -Force -Path $DotnetToolsDir | Out-Null
dotnet tool update --tool-path $DotnetToolsDir vpk | Write-Host
if (-not (Get-Command vpk -ErrorAction SilentlyContinue)) {
    throw "Velopack vpk was not found after dotnet tool install"
}

$vpkPackHelp = (& vpk pack -H 2>&1) -join "`n"
if ($LASTEXITCODE -ne 0) {
    throw "Velopack vpk pack verbose help failed"
}
foreach ($requiredOption in @("--msi", "--instLocation")) {
    if ($vpkPackHelp -notlike "*$requiredOption*") {
        throw "Installed Velopack vpk does not support $requiredOption. Per-machine MSI packaging requires a Windows host vpk pack with MSI support."
    }
}

$Version = Read-CargoVersion (Join-Path $RepoRoot "Cargo.toml")
$PayloadDir = Join-Path $RepoRoot "dist/windows-$Target-$Profile"
$IconPath = Join-Path $RepoRoot "platforms/windows/assets/app.ico"

Remove-Item -Recurse -Force $DistDir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $DistDir | Out-Null

& vpk pack `
    --outputDir $DistDir `
    --packId $PackId `
    --packVersion $Version `
    --packDir $PayloadDir `
    --mainExe "$AppName.exe" `
    --packTitle $PackTitle `
    --packAuthors $PackAuthors `
    --runtime win-x64 `
    --icon $IconPath `
    --msi `
    --instLocation PerMachine
if ($LASTEXITCODE -ne 0) {
    throw "Velopack packaging failed"
}

$msiFiles = Get-ChildItem $DistDir -File -Filter "*.msi"
if ($msiFiles.Count -eq 0) {
    throw "Velopack did not produce a per-machine MSI in $DistDir"
}

Get-ChildItem $DistDir -File | Where-Object { $_.Name -notlike "*.sha256" } | ForEach-Object {
    $hash = Get-FileHash -Algorithm SHA256 $_.FullName
    "$($hash.Hash.ToLowerInvariant())  $($_.Name)" | Set-Content -Path "$($_.FullName).sha256" -Encoding ASCII
}

Write-Host "Built Velopack Windows installer:"
Write-Host "  $DistDir"
Write-Host "Default MSI scope:"
Write-Host "  Per-machine under Program Files, using app id $PackId as the default folder name."
Write-Host "Custom install directory:"
Write-Host "  msiexec /i <installer>.msi VELOPACK_INSTALLDIR=`"C:\Program Files\Alma OneBot Bridge`""
