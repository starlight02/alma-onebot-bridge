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
