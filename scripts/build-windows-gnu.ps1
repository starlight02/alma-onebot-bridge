param(
    [string]$Target = "x86_64-pc-windows-gnu",
    [string]$Profile = "release"
)

$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
$WindowsRoot = Join-Path $RepoRoot "platforms/windows"
$AppName = "AlmaOneBotBridge"

function Add-PathEntry {
    param([string]$PathEntry)
    if ($env:Path -notlike "*$PathEntry*") {
        $env:Path = "$PathEntry;$env:Path"
    }
}

function Write-GetHostNameShim {
    param([string]$OutputDir)

    New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
    $source = Join-Path $OutputDir "gethostnamew_shim.c"
    $object = Join-Path $OutputDir "gethostnamew_shim.o"

    @'
#include <winsock2.h>
#include <windows.h>

int WSAAPI GetHostNameW(PWSTR name, int namelen) {
    DWORD size;

    if (name == NULL || namelen <= 0) {
        WSASetLastError(WSAEFAULT);
        return SOCKET_ERROR;
    }

    size = (DWORD)namelen;
    if (GetComputerNameW(name, &size)) {
        return 0;
    }

    WSASetLastError(WSAEFAULT);
    return SOCKET_ERROR;
}
'@ | Set-Content -Path $source -Encoding ASCII

    & gcc -Wall -Wextra -c $source -o $object
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to compile GetHostNameW MinGW shim"
    }

    return $object
}

if (Test-Path "C:\msys64\mingw64\bin") {
    Add-PathEntry "C:\msys64\mingw64\bin"
}
if (Test-Path "C:\msys64\usr\bin") {
    Add-PathEntry "C:\msys64\usr\bin"
}

if (-not (Get-Command gcc -ErrorAction SilentlyContinue)) {
    throw "MinGW gcc was not found. Install MSYS2 mingw-w64-x86_64-gcc and add it to PATH."
}
if (-not (Get-Command windres -ErrorAction SilentlyContinue)) {
    throw "MinGW windres was not found. Install MSYS2 mingw-w64-x86_64-binutils and add it to PATH."
}

$ShimDir = Join-Path $RepoRoot "target/windows-gnu-shims/$Target"
$ShimObj = Write-GetHostNameShim $ShimDir

$oldRustFlags = $env:RUSTFLAGS
$shimArg = "-C link-arg=$ShimObj -C link-arg=-lws2_32 -C link-arg=-lkernel32"
if ([string]::IsNullOrWhiteSpace($oldRustFlags)) {
    $env:RUSTFLAGS = $shimArg
} elseif ($oldRustFlags -notlike "*$shimArg*") {
    $env:RUSTFLAGS = "$oldRustFlags $shimArg"
}

Push-Location $WindowsRoot
try {
    $cargoArgs = @("build", "--target", $Target)
    if ($Profile -eq "release") {
        $cargoArgs += "--release"
    }
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed"
    }
} finally {
    Pop-Location
    $env:RUSTFLAGS = $oldRustFlags
}

$BuildDir = Join-Path $WindowsRoot "target/$Target/$Profile"
$PayloadDir = Join-Path $RepoRoot "dist/windows-$Target-$Profile"
Remove-Item -Recurse -Force $PayloadDir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $PayloadDir | Out-Null

foreach ($file in @("$AppName.exe", "Microsoft.WindowsAppRuntime.Bootstrap.dll", "resources.pri")) {
    $source = Join-Path $BuildDir $file
    if (-not (Test-Path $source)) {
        throw "Expected Windows payload file not found: $source"
    }
    Copy-Item $source $PayloadDir
}

Write-Host "Built Windows app:"
Write-Host "  $(Join-Path $BuildDir "$AppName.exe")"
Write-Host "Prepared Windows payload:"
Write-Host "  $PayloadDir"
