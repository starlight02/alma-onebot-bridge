<p align="center">
  <img src="../../logo.svg" alt="AlmaOneBotBridge app icon" width="128" height="128">
</p>

# AlmaOneBotBridge for Windows

This directory contains the native Windows tray app for Alma OneBot Bridge.
It is a Rust + WinUI 3 app built with the GNU toolchain. It starts silently,
creates a notification-area icon, and runs the bridge in the app process.

## Runtime Layout

WinUI 3 is not a strict single-file runtime. The runnable Windows payload is:

```text
AlmaOneBotBridge.exe
Microsoft.WindowsAppRuntime.Bootstrap.dll
resources.pri
```

The app executable is a GUI subsystem PE, so double-clicking it does not open a
console window. `Microsoft.WindowsAppRuntime.Bootstrap.dll` and `resources.pri`
must stay next to the executable.

## User-Facing Packages

Release builds produce both formats:

- Velopack MSI installer: `dist/windows-velopack/`
- Portable ZIP: `dist/windows-zip/AlmaOneBotBridge-<version>-windows-x86_64.zip`

The MSI is built per-machine with:

```powershell
vpk pack --msi --instLocation PerMachine --noPortable --skipUpdates --noInst
```

`--noPortable` and `--skipUpdates` avoid duplicate portable ZIP and update-channel
files (`nupkg`, `RELEASES`, `*.json`). `--noInst` skips Velopack's setup.exe.
The portable ZIP for releases is built with `package-windows-zip.ps1` instead.

Velopack uses the app id as the default application folder name. This project
uses `AlmaOneBotBridge`, so the installer does not add an extra `Alma` directory
level. For an exact custom path, install with:

```powershell
msiexec /i <installer>.msi VELOPACK_INSTALLDIR="C:\Program Files\Alma OneBot Bridge"
```

## Build On Windows

Install prerequisites:

- Rust target: `rustup target add x86_64-pc-windows-gnu`
- MSYS2 MinGW64 packages: `mingw-w64-x86_64-gcc` and `mingw-w64-x86_64-binutils`
- .NET 8 SDK for the Velopack `vpk` tool

Build the raw payload:

```powershell
.\scripts\build-windows-gnu.ps1
```

Build the MSI installer:

```powershell
.\scripts\package-windows-velopack.ps1
```

Build the portable ZIP:

```powershell
.\scripts\package-windows-zip.ps1
```

## Build From macOS Or Linux

Cross-building requires Docker and `cross`:

```bash
cargo install cross
./scripts/build-windows-cross.sh
```

The ZIP package can also be created from macOS/Linux:

```bash
./scripts/package-windows-zip.sh
```

Local release check on macOS/Linux (cross build + portable ZIP; MSI still needs Windows):

```bash
./scripts/test-windows-packaging-macos.sh
```

Use `./scripts/build-windows-cross.sh` (not bare `cross build`) so `GetHostNameW` shim
`RUSTFLAGS` and arm64 Docker `linux/amd64` platform are applied.

Velopack MSI packaging must run on Windows. The `vpk [win] pack` cross-pack path
does not expose Velopack's MSI/per-machine installer options.

## Size Notes

The executable includes the bridge runtime and Rust networking/database stack.
It does not bundle the Windows App SDK runtime. In framework-dependent mode the
WinUI sidecar files are the bootstrap DLL and PRI file listed above. Release
builds strip symbols and enable thin LTO to keep the PE smaller without changing
the runtime layout.
