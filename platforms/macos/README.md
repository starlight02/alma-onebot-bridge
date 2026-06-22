<p align="center">
  <img src="AlmaOneBotBridge/Assets.xcassets/BridgeIcon.imageset/bridge-icon.svg" alt="AlmaOneBotBridge app icon" width="128" height="128">
</p>

# AlmaOneBotBridge for macOS

The macOS app runs `alma-onebot-bridge` from the menu bar. It ships with the
Rust bridge binary, manages the bridge process, edits config, and writes logs
under Alma's config directory.

## Requirements

- macOS 26.0 or newer
- Xcode 27 or newer
- Rust toolchain matching the bridge project
- Alma running on the same Mac
- A OneBot v11 client configured for reverse WebSocket

## Build

From the repository root:

```bash
./scripts/build-macos.sh
```

The script writes the app bundle here:

```text
platforms/macos/build/Build/Products/Release/AlmaOneBotBridge.app
```

Install it into `/Applications`:

```bash
INSTALL_TO_APPLICATIONS=1 ./scripts/build-macos.sh
```

The script builds the Xcode project, places the Rust bridge in app resources,
and signs the app with an ad-hoc signature for local launches.

## Package

Build a macOS Installer package:

```bash
./scripts/package-macos-pkg.sh
```

The script writes:

```text
dist/macos/AlmaOneBotBridge-<version>-universal-unsigned.pkg
dist/macos/AlmaOneBotBridge-<version>-universal-unsigned.pkg.sha256
```

The Installer shows the repository `LICENSE` before installation and installs
`AlmaOneBotBridge.app` into `/Applications`.

Build single-architecture packages when needed:

```bash
# Apple Silicon only
BUILD_UNIVERSAL=0 TARGET=aarch64-apple-darwin PACKAGE_ARCH=arm64 ./scripts/package-macos-pkg.sh

# Intel Mac only. The artifact uses the common amd64 label; Xcode/Rust call it x86_64.
BUILD_UNIVERSAL=0 TARGET=x86_64-apple-darwin PACKAGE_ARCH=amd64 ./scripts/package-macos-pkg.sh
```

Unsigned packages work for local testing and internal distribution. macOS
Gatekeeper will warn users because Apple has not notarized the package. For a
normal public release, sign the app with Developer ID Application, sign the
package with Developer ID Installer, then notarize and staple the package.

Use these environment variables for a signed local package:

```bash
APP_SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" \
INSTALLER_SIGN_IDENTITY="Developer ID Installer: Your Name (TEAMID)" \
APPLE_ID="you@example.com" \
APPLE_TEAM_ID="TEAMID" \
APPLE_APP_SPECIFIC_PASSWORD="app-specific-password" \
NOTARIZE=1 \
./scripts/package-macos-pkg.sh
```

## Run

Open the built app from Finder, Launchpad, or Terminal:

```bash
open platforms/macos/build/Build/Products/Release/AlmaOneBotBridge.app
```

The app has no Dock icon. It lives in the menu bar and starts the bridge on
launch.

## Menu Bar Controls

- `Start` starts the bridge if it is not running.
- `Stop` stops the bridge.
- `Restart` stops and starts the bridge.
- `Preferences...` opens settings.
- `Open Config Directory` opens the bridge config/log directory in Finder.
- `Open Bridge Log` opens the current bridge log.
- `Quit` stops the bridge and exits the app.

The status indicator checks the bridge process and `GET /health` on the
configured port.

## Configuration

Use Preferences for normal config changes. The app reads and writes:

```text
~/.config/alma/bridge/config.toml
```

You can edit the same file by hand. The Rust bridge reads config in this order:

1. `config.toml` in the current working directory
2. `bridge.toml` in the current working directory
3. `~/.config/alma/bridge/config.toml`
4. defaults

The macOS app starts the bridge in `~/.config/alma/bridge`, so Preferences
controls the active `config.toml`. Configure bridge behavior in TOML, not
environment variables. The app only passes process metadata such as log path and
management marker.

Saving Preferences applies changes:

- Chat, model, and timeout settings reload through `SIGHUP`.
- Port, Alma API URL, access token, and database path restart the bridge.

## Runtime Files

```text
~/.config/alma/bridge/config.toml
~/.config/alma/bridge/bridge.log
~/.config/alma/bridge/bridge.pid
```

The app uses `bridge.pid` for process discovery and cleanup. Before it sends a
stop or reload signal, it checks that the PID still belongs to
`alma-onebot-bridge`.

## OneBot URL

Configure the OneBot client to connect to the bridge:

```text
ws://<bridge-host>:8090/ws
```

For Docker or OrbStack on the same Mac, use:

```text
ws://host.docker.internal:8090/ws
```

After you change `bridge.port` in Preferences, update the OneBot reverse
WebSocket URL.

## Troubleshooting

- If the app reports that the bridge executable is missing, rebuild with
  `./scripts/build-macos.sh`.
- If OneBot cannot connect, check the bridge status and the port in the OneBot
  URL.
- If settings fail to save, check validation errors in Preferences and confirm
  `~/.config/alma/bridge` is writable.
- If the bridge starts but health stays failed, open `bridge.log` from the menu
  bar and check Alma API URL, port conflicts, and OneBot token settings.

## GitHub Release Builds

The GitHub workflow `.github/workflows/macos-pkg.yml` builds three PKG installers
on `macos-latest` and uploads them as artifacts:

- `arm64`: Apple Silicon Macs
- `amd64`: Intel Macs (`x86_64` in Xcode/Rust naming)
- `universal`: one package containing both architectures

Tags that match `v*` also attach all three packages to a GitHub Release.

Without Apple signing secrets, the workflow publishes an unsigned package named
`AlmaOneBotBridge-<version>-<arch>-unsigned.pkg`. This keeps release automation
usable before the project has a paid Apple Developer certificate.

To publish signed and notarized packages, add these repository secrets:

- `MACOS_CERTIFICATE_P12_BASE64`: base64-encoded `.p12` containing both Developer ID Application and Developer ID Installer certificates
- `MACOS_CERTIFICATE_PASSWORD`: password for that `.p12`
- `KEYCHAIN_PASSWORD`: temporary CI keychain password
- `APP_SIGN_IDENTITY`: Developer ID Application identity name
- `INSTALLER_SIGN_IDENTITY`: Developer ID Installer identity name
- `APPLE_ID`: Apple ID used for notarization
- `APPLE_TEAM_ID`: Apple Developer Team ID
- `APPLE_APP_SPECIFIC_PASSWORD`: app-specific password for notarization

If any signing secret is set, the workflow requires the full set. With the full
set present, it signs the app, signs the installer, submits the PKG to Apple
notary service, staples the ticket, and uploads the signed PKG.
