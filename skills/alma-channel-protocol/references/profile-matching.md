# SENDER PROFILE Matching Reference

This document details the bridge-side SENDER PROFILE matching algorithm — how each bridge
identifies the sender and injects their profile into the `ephemeralContext`.

## Architecture Principle

**Profile matching happens in the bridge, NOT the server.** The Alma server:
- Receives `ephemeralContext` with SENDER PROFILE blocks already injected
- Strips stale `[SENDER PROFILE]` blocks from **older** user messages in conversation history
- Does NOT perform any platform-specific ID matching

Each bridge is responsible for:
1. Reading people profile files from `~/.config/alma/people/`
2. Matching the sender's platform ID against YAML frontmatter
3. Injecting the matched profile into `ephemeralContext`

## People Profile File Structure

Location: `~/.config/alma/people/*.md`

Format: YAML frontmatter + Markdown body.

```markdown
---
telegram_id: "123456789"
qq_id: "123456789"
discord_id: "987654321"
discord_username: "someone"
feishu_id: "ou_xxxxx"
username: someone
---
# Someone

Person description and notes...
```

### Frontmatter Fields

| Field | Type | Used By | Notes |
|-------|------|---------|-------|
| `telegram_id` | quoted string | Telegram bridge | Telegram user ID |
| `discord_id` | quoted string | Discord bridge | Discord user ID (Snowflake) |
| `discord_username` | quoted string | Discord bridge | Fallback match |
| `feishu_id` | quoted string | Feishu bridge | Feishu open_id or union_id |
| `qq_id` | quoted string | QQ/OneBot bridge | QQ number |
| `username` | quoted string | All bridges | Display name / handle |

All ID fields must be **quoted strings** (e.g., `telegram_id: "123456789"`, not
`telegram_id: 123456789`). This prevents YAML from interpreting large IDs as numbers
and losing precision.

### File Naming Convention

Recommended: name files by a stable platform ID, not by nickname.

| Convention | Example | Rationale |
|-----------|---------|-----------|
| QQ ID | `123456789.md` | Stable, no collisions from shared nicknames |
| Telegram ID | `123456789.md` | Same rationale |
| Username | `alice.md` | Works if usernames are unique on the platform |
| Sanitized nickname | `Alice_W.md` | Risky — different people can have the same nickname |

For cross-platform profiles, use the primary platform's ID as the filename and include
all other IDs in frontmatter.

## Matching Algorithm

### Step-by-Step

```
for each .md file in ~/.config/alma/people/:
    read file content
    parse YAML frontmatter (between --- markers)

    # Primary match: platform-specific ID field
    if frontmatter[platform_id_field] == sender_id (quoted or unquoted):
        if file content length < 500 chars:
            return SENDER PROFILE block
        else:
            skip (profile too long)

    # Fallback match: filename == display name
    if file_stem.lower() == display_name.lower():
        if file content length < 500 chars:
            return SENDER PROFILE block
        else:
            skip (profile too long)

return None (no match)
```

### Platform-Specific Matching

**Telegram bridge** (`MessageBridge`):
```
Primary: telegram_id == sender.id (as string)
Fallback: filename (without .md) == sender display name (case-insensitive)
```

**Discord bridge** (`DiscordBridge`):
```
Primary: discord_id == author.id
Secondary: discord_username == author.username
Fallback: filename == author display name (case-insensitive)
```

**Feishu bridge** (`FeishuBridge`):
```
Primary: feishu_id == sender.open_id or sender.union_id
Fallback: filename == sender display name (case-insensitive)
```

**QQ/OneBot bridge** (custom):
```
Primary: qq_id == user_id (as string)
Fallback: filename == sender display name (case-insensitive)
```

### Frontmatter Parsing

Match both quoted and unquoted values for robustness:

```
# Both should match for sender ID "123456789":
telegram_id: "123456789"   ← quoted (recommended)
telegram_id: 123456789     ← unquoted (also match)
```

Simple string containment check works for most cases:
```
content.contains("telegram_id: \"123456789\"") ||
content.contains("telegram_id: 123456789")
```

For production bridges, a proper YAML parser is more reliable but the containment
approach works well for the simple frontmatter structures used in practice.

## Injected Format

The matched profile is wrapped in `[SENDER PROFILE]` tags:

```
[SENDER PROFILE — alice]:
---
telegram_id: "123456789"
qq_id: "123456789"
username: alice
---
# Alice

Alice is a designer based in Tokyo.
[/SENDER PROFILE]
```

**Size limit**: If the profile file content exceeds 500 characters, skip injection.
Large profiles would consume too much of the context window. The 500-char limit
includes frontmatter and body.

## PEOPLE PROFILES Summary Line

After the SENDER PROFILE block, always append a summary line:

```
PEOPLE PROFILES — You know 42 people. Use `alma people list` or `alma people show <name>` to look up profiles on demand.
```

The count is the total number of `.md` files in the people directory. This tells the AI
how many people it "knows" and that it can look up additional profiles via the People CLI.

## EphemeralContext Assembly Order

The full `ephemeralContext` string is assembled in this order:

```
1. [SENDER PROFILE — name]: ... [/SENDER PROFILE]    ← bridge injects this
2. PEOPLE PROFILES — You know N people. ...           ← bridge injects this
3. RECENT GROUP CHAT HISTORY: ...                     ← bridge injects (groups only)
4. RECENT GROUP INTERACTIONS: ...                     ← bridge injects (optional)
```

The server then adds its own layers when processing `generate_response`:
- SOUL.md (persona definition)
- Memory (long-term recall)
- Tool descriptions
- Channel-specific system prompt (based on `source` field)

## Server-Side History Stripping

When `source` matches a known channel, the server strips the following blocks from
**older** user messages in conversation history (the last user message keeps them):

- `[SENDER PROFILE ... ]` → `[/SENDER PROFILE]`
- `PEOPLE PROFILES — ...` (to the end of that line)
- `RECENT GROUP CHAT HISTORY:` (to the next section or end)
- `GROUP-SPECIFIC RULES:` (to the next section or end)
- `RECENT GROUP INTERACTIONS:` (to the next section or end)

This prevents stale profile data from accumulating across turns. The bridge does not
need to implement stripping — the server handles it automatically when `source` is set
to a recognized channel identifier.

## Auto-Creating Profile Files

Some bridges (notably Discord) auto-create profile files for users who don't have one yet:

```markdown
---
discord_id: "987654321"
discord_username: "someone"
---
Discord user. First seen in Discord chat.
```

For QQ/OneBot bridges, auto-creation is recommended:

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

Include both `telegram_id` and `qq_id` (set to the QQ ID) for cross-platform matching
with the Telegram bridge.

## Cross-Platform Identity

When the same person uses multiple platforms, include all known IDs in one profile file:

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
- All resolve to the same profile → Alma recognizes the same person across platforms
