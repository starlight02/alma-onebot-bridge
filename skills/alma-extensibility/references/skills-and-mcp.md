# Alma Skill System & MCP Servers Reference

## Skill System

Skills are **prompt templates** loaded into the AI's context. They teach the agent new
capabilities using natural language instructions + access to Bash/file tools.

### Directory Structure

```
~/.config/alma/skills/<name>/
├── SKILL.md              # Required: YAML frontmatter + markdown instructions
├── scripts/              # Optional: executable helper scripts
├── references/           # Optional: docs loaded on-demand by the AI
└── assets/               # Optional: templates, images, data files
```

### SKILL.md Frontmatter

```yaml
---
name: my-skill
description: >
  What the skill does AND when to trigger it.
  Be specific — vague descriptions cause under-triggering.
allowed-tools:
  - Bash
  - Read
  - Write
  - Grep
  - Glob
  - WebSearch
  - WebFetch
---
```

The `allowed-tools` field lists which tools the skill can use. Available tools include
`Bash`, `Read`, `Write`, `Grep`, `Glob`, `WebSearch`, `WebFetch`, and any registered
MCP tool names.

### Progressive Disclosure

1. **Metadata** (name + description) — always in context (~100 words)
2. **SKILL.md body** — loaded when skill triggers (< 500 lines ideal)
3. **Bundled resources** — read on demand via markdown links (unlimited size)

### Design Principles

1. **Description drives triggering** — the AI reads name + description to decide whether
   to load the skill. Make it specific about use cases and trigger terms.
2. **Keep SKILL.md under ~500 lines** — use `references/` for detailed docs, point to
   them from SKILL.md via markdown links.
3. **Explain WHY, not just WHAT** — the AI is smart enough to generalize from principles.
4. **Include examples** — concrete input/output examples are worth more than abstract
   instructions.

### Management

```bash
alma skill list                  # List installed
alma skill search <query>        # Search skills.sh
alma skill install <source>      # Install (skills.sh or GitHub repo)
alma skill update                # Update installed skills
alma skill uninstall <name>      # Remove
```

---

## MCP Servers

External tool servers providing structured capabilities to the AI. MCP tools appear
alongside built-in tools and can be listed via `alma tool list`. They can also be
referenced in skill `allowed-tools` frontmatter by tool name.

### Configuration

**Config file:** `~/.config/alma/mcp.json`

```json
{
  "mcpServers": {
    "my-service": {
      "url": "http://127.0.0.1:8001/mcp/",
      "transport": "streamable-http"
    }
  }
}
```

### Transport Types

| Transport | Config | Use Case |
|-----------|--------|----------|
| `streamable-http` | `url` field | Remote HTTP-based MCP servers |
| `sse` | `url` field | Server-Sent Events based servers |
| `stdio` | `command` + `args` fields | Local process-based servers |

Example with stdio transport:

```json
{
  "mcpServers": {
    "local-tool": {
      "command": "node",
      "args": ["mcp-server.js"],
      "transport": "stdio"
    }
  }
}
```

### Using MCP Tools

MCP tools are automatically discovered and available to the AI. List them:

```bash
alma tool list                    # Shows all tools including MCP
alma tool call <toolName> '<jsonArgs>'   # Invoke directly
```

In skills, reference MCP tools in `allowed-tools` by their registered name.

### Error Handling

- If an MCP server is unreachable, its tools are silently unavailable (no error)
- Tool call failures return error objects to the AI, which can retry or report
- Server configuration errors in `mcp.json` prevent all MCP servers from loading

---

## Related References

- [people-profiles.md](people-profiles.md) — People Profiles (user data files)
- [channel-bridge.md](channel-bridge.md) — How channel bridges use skills and profiles
- The **`alma-channel-protocol`** skill — for building channel-specific integration skills
