# Alma CLI Reference

Run `alma help` for the complete list. Environment: `ALMA_API_URL` overrides base URL (default `http://localhost:23001`).

## Configuration & Providers

```bash
alma status                              # Check if running
alma config list                         # All settings (JSON)
alma config get <path>                   # Read setting (dot-path)
alma config set <path> <value>           # Update setting
alma providers                           # List providers
alma provider add <name> <type>          # Add (--api-key, --base-url, --models)
alma provider delete <id>                # Remove provider
alma providers <id> models               # Provider's models
alma models                              # All models
alma model set <provider:model>          # Set default
```

## Threads & Projects

```bash
alma threads [limit]                     # Recent threads
alma thread create <title>               # New thread (--project <id>)
alma thread delete <id>                  # Delete
alma thread search <query>               # Search
alma thread compact <id>                 # Compact context
alma thread messages <id> [limit]        # Show messages
alma thread switch <id>                  # Switch active chat
alma project list                        # List projects (workspaces)
alma project create <name>               # Create (--path <dir>)
alma project show <id>                   # Details
alma project delete <id>                 # Delete (must be empty)
alma project assign <threadId> <id>      # Move thread to project
```

## AI Completion (`alma run`)

```bash
alma run [prompt]                        # Full pipeline: SOUL + Memory + Tools + Plugins
  -m, --model <provider:model>           # Override model
  -s, --system <prompt>                  # Prepend system instruction
  -t, --temperature <n>                  # 0-2
  --raw                                  # Strip markdown
  --no-stream                            # Buffer, print at end
  -v, --verbose                          # Show thinking + tool calls
  -l, --list-models                      # List models and exit

# Target specific thread (critical for bridge services):
ALMA_THREAD_ID=<id> alma run "hello"
```

## Memory, Identity, Emotion

```bash
alma memory list                         # All memories
alma memory search <query>               # Search
alma memory add <content>                # Add memory
alma memory delete <id>                  # Delete
alma memory stats                        # Statistics
alma soul                                # Show SOUL.md
alma soul set "<content>"                # Replace SOUL.md
alma soul append-trait "<desc>"          # Add evolved trait
alma user                                # Show USER.md
alma emotion status                      # Current emotion state
alma emotion set-base <mood> <energy> <valence> "<desc>"
alma emotion set-context <chatId> <mood> <valence> "<trigger>"
```

## Skills, Tools, Cron

```bash
alma skill list                          # Installed skills
alma skill search <query>                # Search skills.sh
alma skill install <source>              # Install (skills.sh or GitHub)
alma skill update                        # Update installed
alma skill uninstall <name>              # Remove
alma tool list                           # List invocable tools
alma tool call <name> [jsonArgs]         # Invoke tool directly
alma cron list                           # List cron jobs
alma cron add <name> <type> <sched>      # Add (at|every|cron)
alma cron remove <id>                    # Remove
alma cron run <id>                       # Run now
```

## Media & Messaging

```bash
alma dm <userId> <message>               # Telegram DM
alma msg delete <chatId> <messageId>     # Delete message
alma send photo <path> [caption]         # Send photo
alma send file <path> [caption]          # Send file
alma group list                          # List Telegram groups
alma group send <chatId> "text"          # Send to group
alma voices                              # List TTS voices
alma image models                        # List image gen models
alma image generate <prompt>             # Generate image
alma sing generate "lyrics"              # Generate song (Suno)
```

## Browser & Desktop

```bash
alma browser status                      # Chrome Relay status
alma browser tabs                        # List open tabs
alma browser open [url]                  # Open new tab
alma browser goto <tabId> <url>          # Navigate
alma browser click <tabId> <selector>    # Click element
alma browser type <tabId> <sel> <text>   # Type text
alma browser screenshot [tabId]          # Take screenshot
alma browser read <tabId>                # Read as markdown
alma browser eval <tabId> <code>         # Run JS
alma cu <subcommand>                     # macOS AX automation
```

## Activity Recorder

```bash
alma activity status                     # Status + storage
alma activity start                      # Start recording
alma activity stop                       # Stop recording
alma activity sessions [limit]           # Recent sessions
alma activity show <id>                  # Full session detail
alma activity search <query>             # Keyword search
alma activity summary <daily|weekly>     # Generate summary
alma activity report [date]              # Thematic work journal
```

## Data & Workspace

```bash
alma usage                               # Usage statistics
alma export                              # Export all data
alma import <file>                       # Import data
alma workspace list                      # List workspaces
alma workspace set <id> <path>           # Update workspace path
alma update [check|download|install]     # Updates
alma version                             # Version info
```

## Environment Variables

| Variable | Default | Used By |
|----------|---------|---------|
| `ALMA_API_URL` | `http://localhost:23001` | All CLI commands (base URL) |
| `ALMA_THREAD_ID` | *(none)* | `alma run` — target a specific thread |

## Related References

- [rest-api.md](rest-api.md) — REST endpoints that CLI commands wrap
- [channel-bridge.md](channel-bridge.md) — How bridges use `alma run` with `ALMA_THREAD_ID`
- [skills-and-mcp.md](skills-and-mcp.md) — Skill and MCP management commands
- [plugin-system.md](plugin-system.md) — Plugin system (no CLI management — files only)
