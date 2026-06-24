<p align="center">
  <img src="AlmaOneBotBridge/Assets.xcassets/BridgeIcon.imageset/bridge-icon.svg" alt="AlmaOneBotBridge app icon" width="128" height="128">
</p>

# AlmaOneBotBridge for macOS

AlmaOneBotBridge is a menu bar app that connects Alma to QQ through a OneBot
v11 reverse WebSocket client. It starts and stops the bridge, saves settings,
and gives you quick access to logs.

## Requirements

- macOS 26.0 or newer
- Alma running on the same Mac
- A OneBot v11 client, such as snowluma, NapCat, or Lagrange

## Install

Download the latest macOS PKG from GitHub Releases:

- `arm64`: Apple Silicon Macs
- `amd64`: Intel Macs

Open the PKG and follow Installer. The installer asks you to accept the
AGPL-3.0 license, then installs the app into `/Applications`.
If the installed app is already running, Installer quits it before replacing the
app and reopens the newly installed app after installation.

Unsigned builds include `unsigned` in the file name. macOS may show a Gatekeeper
warning for unsigned packages. If that happens, open System Settings, go to
Privacy & Security, and allow the app after you confirm the package came from
this project.

## First Launch

Open `AlmaOneBotBridge` from Launchpad or `/Applications`.

The app has no Dock icon. It runs in the menu bar and starts the bridge when it
opens. The status row shows whether the bridge is running and whether the
health check succeeds.

## Configure

Open `Preferences...` from the menu bar item.

Common settings:

- Bridge port: default `8090`
- Alma API: default `http://localhost:23001`
- Alma model: leave empty to use Alma's default model
- Access token: optional token for OneBot WebSocket auth and non-local HTTP send commands
- Group history size: how many recent group messages are sent to Alma as context
- Thinking message: optional message sent before a slow AI reply

Preferences are saved to:

```text
~/.config/alma/bridge/config.toml
```

Most chat and model changes reload without a full restart. Port, Alma API,
access token, and database path changes restart the bridge.

## Connect OneBot

Configure your OneBot client to connect to:

```text
ws://<bridge-host>:8090/ws
```

If the OneBot client runs in Docker or OrbStack on the same Mac, use:

```text
ws://host.docker.internal:8090/ws
```

If you changed the bridge port in Preferences, update the OneBot URL to match.

## Menu Bar Actions

- `Start`: starts the bridge
- `Stop`: stops the bridge
- `Restart`: restarts the bridge
- `Preferences...`: opens settings
- `Open Config Directory`: opens the config and log folder
- `Open Bridge Log`: opens the current bridge log
- `About Alma Bridge`: shows the installed app version, source commit, project URL, author, license, and bridge status
- `Quit`: stops the bridge and exits the app

## Logs and Files

The app keeps its runtime files here:

```text
~/.config/alma/bridge/config.toml
~/.config/alma/bridge/bridge.log
~/.config/alma/bridge/bridge.pid
```

Use `Open Bridge Log` from the menu bar when the bridge does not behave as
expected.

## Troubleshooting

- If OneBot cannot connect, check the bridge port and OneBot WebSocket URL.
- If the app says the bridge is not healthy, confirm Alma is running and the
  Alma API URL is correct.
- If settings fail to save, check the error shown in Preferences and make sure
  `~/.config/alma/bridge` is writable.
- If you use an access token, make sure the OneBot client sends the same token.
- If you change the bridge port, update the OneBot reverse WebSocket URL.

## Uninstall

Quit the app, then remove it from `/Applications`.

To remove local bridge settings and logs as well:

```bash
rm -rf ~/.config/alma/bridge
```

Development and release notes live in
[`src/docs/DEVELOPMENT_KNOWLEDGE_BASE.md`](../../src/docs/DEVELOPMENT_KNOWLEDGE_BASE.md).
