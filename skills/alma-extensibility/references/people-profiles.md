# Alma People Profiles Reference

People Profiles are markdown files with YAML frontmatter that serve as the AI's
"memory" about specific users. They are auto-loaded by `alma run` and by channel
bridges when processing messages.

## Location

```
~/.config/alma/people/<filename>.md
```

## File Format

```markdown
---
telegram_id: "123456789"
discord_id: "987654321"
qq_id: "10001000"
feishu_id: "ou_xxxxx"
username: someone
---
# Display Name

- Key facts, preferences, interaction history
- Notes about communication style
- Relationship context
```

### Frontmatter Fields

| Field | Type | Used By | Notes |
|-------|------|---------|-------|
| `telegram_id` | quoted string | Telegram bridge | Telegram user ID |
| `discord_id` | quoted string | Discord bridge | Discord Snowflake ID |
| `discord_username` | quoted string | Discord bridge | Fallback match |
| `feishu_id` | quoted string | Feishu bridge | Feishu open_id or union_id |
| `qq_id` | quoted string | QQ/OneBot bridge | QQ number |
| `username` | quoted string | All bridges | Display name / handle |

All ID fields **must be quoted strings** (e.g., `telegram_id: "123456789"`). Unquoted
large integers can lose precision in YAML parsers.

### File Naming Convention

| Convention | Example | When to Use |
|-----------|---------|-------------|
| Platform ID | `123456789.md` | Recommended — stable, no collisions |
| Username | `alice.md` | When usernames are unique per platform |
| Sanitized nickname | `Alice_W.md` | Risky — different people can share nicknames |

For cross-platform profiles, use the primary platform's ID as the filename and
include all other platform IDs in frontmatter.

## How Profiles Are Loaded

### By `alma run` (CLI)

When you run `alma run "message"`, Alma scans all `.md` files in the people directory
and searches for matches against the message content. Matching profiles are injected
into the AI's context.

### By Channel Bridges (WebSocket)

Each built-in bridge implements its own matching logic:

1. Bridge receives a message with sender's platform ID
2. Bridge scans `~/.config/alma/people/*.md` for matching frontmatter field
3. If found and profile < 500 chars, injects into `ephemeralContext` as a
   `[SENDER PROFILE]` block

For the detailed bridge-side matching algorithm, see the **`alma-channel-protocol`** skill.

## Cross-Platform Identity

When the same person uses multiple platforms, include all known IDs in one file:

```yaml
---
telegram_id: "123456789"
qq_id: "123456789"
discord_id: "987654321"
username: starlight
---
```

- QQ bridge matches by `qq_id`
- Telegram bridge matches by `telegram_id`
- Discord bridge matches by `discord_id`

All resolve to the same profile, so Alma recognizes the same person across platforms.

## Auto-Creating Profiles

Bridges can auto-create profile files for users who don't have one yet:

```markdown
---
telegram_id: "123456789"
qq_id: "123456789"
username: "Alice"
---
# Alice

- QQ 用户，ID: 123456789
- 昵称: Alice
- 首次互动: 2026-06-19
```

Include both `telegram_id` and `qq_id` (set to the QQ ID) so the same person can be
matched by both the QQ bridge and the Telegram bridge.

## CLI Management

```bash
alma people list                   # List all profiles
alma people show <name>            # Show profile content
alma people append <name> <text>   # Append to profile
```

## Size Limit

Profile files larger than **500 characters** are skipped during bridge-side SENDER PROFILE
injection. Keep profiles concise — focus on facts the AI needs for context, not exhaustive
biographies.

## Related References

- [channel-bridge.md](channel-bridge.md) — Bridge patterns that use People Profiles
- [skills-and-mcp.md](skills-and-mcp.md) — How skills reference people data
- The **`alma-channel-protocol`** skill has the full SENDER PROFILE matching algorithm
