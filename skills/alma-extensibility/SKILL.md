---
name: alma-extensibility
version: 2.0.0
description: >-
  Complete guide to Alma's extensibility: REST API (localhost:23001), CLI commands
  (alma run, alma config, alma thread, etc.), Plugin system (TypeScript hooks),
  Skill system (SKILL.md), MCP servers, People Profiles, and custom channel integration
  patterns. Use this skill whenever working with Alma integrations, building plugins,
  creating skills, connecting external messaging platforms (QQ/OneBot, Slack, etc.),
  automating Alma via CLI, calling the Alma API, or answering questions about what Alma
  can/cannot do programmatically. Also use when asked about alma run, alma hooks,
  plugin development, or extending Alma.
description_zh: >-
  Alma 扩展能力完整指南：REST API（localhost:23001）、CLI 命令（alma run、alma config、
  alma thread 等）、Plugin 系统（TypeScript hooks）、Skill 系统（SKILL.md）、MCP 服务器、
  People Profiles，以及自定义渠道集成模式。适用于 Alma 集成开发、插件构建、技能创建、
  接入外部消息平台（QQ/OneBot、Slack 等）、CLI 自动化、API 调用，以及了解 Alma 的
  编程扩展边界。
---

# Alma Extensibility Guide

Reference for building integrations, plugins, and extensions for Alma.

## Core Architecture

Alma's **channel system is closed** — Telegram, Discord, WeChat are built-in with no
registration API. But there are **5 extension points** covering most needs:

| Point | Mechanism | Best For |
|-------|-----------|----------|
| **REST API** | `http://localhost:23001/api/*` | External processes managing Alma state |
| **`alma run`** | CLI with full AI pipeline | Getting AI replies from scripts/bridges |
| **Plugins** | TypeScript in `~/.config/alma/plugins/` | Hooking into message/tool events |
| **Skills** | SKILL.md prompt templates | Teaching the AI new capabilities |
| **MCP Servers** | External tool servers | Providing structured tools |

---

## 1. REST API

**Base URL:** `http://localhost:23001` | **Spec:** `~/.config/alma/api-spec.md`

Covers: settings, AI providers, models, threads. Key rules:

- **Settings PUT**: Must send the complete object (no partial updates). Always GET first.
- **Model IDs**: Always `providerId:modelId` format (e.g., `abc123:gpt-4o`).
- **API keys**: Encrypted in storage, never exposed in responses.
- **WebSocket sync**: API changes broadcast to connected GUI clients.

For the full endpoint table, request/response schemas, and usage examples,
see [rest-api.md](references/rest-api.md).

---

## 2. CLI

Full help: `alma help`. Environment: `ALMA_API_URL` overrides base URL (default
`http://localhost:23001`).

The CLI covers: configuration & providers, threads & projects, AI completion (`alma run`),
memory & identity (`alma memory`, `alma soul`, `alma emotion`), skills & tools & cron,
media & messaging, browser & desktop automation, activity recording, and data management.

**Most important for integrations — `alma run`:**

```bash
alma run [prompt]                        # Full pipeline: SOUL + Memory + Tools + Plugins
  -m, --model <provider:model>           # Override model
  -s, --system <prompt>                  # Prepend system instruction
  --raw                                  # Strip markdown
  --no-stream                            # Buffer, print at end

# Target specific thread (critical for bridge services):
ALMA_THREAD_ID=<id> alma run "hello"
```

For the complete command reference with all subcommands and flags,
see [cli-reference.md](references/cli-reference.md).

---

## 3. Plugin System

Plugins are **TypeScript extensions** running inside Alma's process. They hook into
message and tool lifecycle events.

**Good for:** forwarding messages, modifying prompts before AI sees them, logging tool
usage, reacting to AI replies, custom UI commands.

**Directory:** `~/.config/alma/plugins/<plugin-name>/`

**Key hook events:**

| Event | Trigger | Key Capability |
|-------|---------|---------------|
| `chat.message.willSend` | Before user message reaches AI | **Mutable** — modify prompt text or model |
| `chat.message.didReceive` | After AI generates response | React to replies (e.g., forward to external channel) |
| `tool.willExecute` | Before a tool runs | Log, block, or modify tool arguments |
| `tool.didExecute` | After tool completes | Analytics, telemetry |

**Hard limitations:**
- Cannot create inbound messages (no `chat.message.create`)
- Cannot register channel adapters (channels are built into Alma's binary)
- Cannot modify the AI pipeline internals (only `willSend`/`didReceive` on user side)
- Runs in Alma's process — uncaught exceptions can crash the app

For manifest.json schema, PluginContext API, permissions, and a complete example plugin,
see [plugin-system.md](references/plugin-system.md).

---

## 4. Skill System

Skills are **prompt templates** loaded into the AI's context. They teach the agent new
capabilities using natural language instructions + access to Bash/file tools.

**Structure:**

```
~/.config/alma/skills/<name>/
├── SKILL.md              # Required: YAML frontmatter + markdown instructions
├── references/           # Optional: docs loaded on-demand by the AI
└── assets/               # Optional: templates, images, data files
```

**Design principles:**
1. **Description drives triggering** — be specific about use cases in the description field.
2. **Keep SKILL.md under ~500 lines** — use `references/` for detailed docs.
3. **Explain WHY, not just WHAT** — the AI is smart enough to generalize from principles.
4. **Include examples** — concrete input/output examples beat abstract instructions.

For frontmatter schema, progressive disclosure patterns, and management commands,
see [skills-and-mcp.md](references/skills-and-mcp.md).

---

## 5. MCP Servers

External tool servers providing structured capabilities to the AI. MCP tools appear
alongside built-in tools and can be listed via `alma tool list`.

**Config:** `~/.config/alma/mcp.json`

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

For transport types, tool schema, and authentication patterns,
see [skills-and-mcp.md](references/skills-and-mcp.md).

---

## 6. People Profiles

User profiles as markdown with YAML frontmatter, auto-loaded by `alma run` as context.

**Location:** `~/.config/alma/people/<name>.md`

```markdown
---
telegram_id: "123456789"
discord_id: "987654321"
qq_id: "10001000"
---
# Display Name

- Key facts, preferences, interaction history
```

All platform IDs must be **quoted strings** in YAML. Include IDs from every platform
the person uses for cross-platform identity matching.

For frontmatter field conventions, auto-creation patterns, and cross-platform identity,
see [people-profiles.md](references/people-profiles.md).

---

## 7. Custom Channel Bridge Pattern

For integrating unsupported platforms (e.g., QQ/OneBot, Slack, Matrix), there are
**two approaches** with different tradeoffs:

### Approach A: WebSocket Bridge (Native — Recommended)

Alma's built-in channels all use a WebSocket protocol to communicate with the Alma server.
You can build a custom bridge using the same protocol for **much better integration**.

```
External Platform → Custom Bridge → ws://127.0.0.1:23001/ws/threads → Alma Server
                    (generate_response, ephemeralContext, SENDER PROFILE injection)
```

**Advantages:** streaming responses, native SENDER PROFILE matching, reply/quoting support,
same processing pipeline as built-in channels, messages appear in GUI.

For the full WebSocket protocol (`generate_response` format, event sequence, profile
matching, reply/quoting), use the **`alma-channel-protocol`** skill — it is the
authoritative reference for this protocol.

### Approach B: REST API + `alma run` (External — Simpler)

For quick prototypes or when WebSocket integration is too complex:

```
External Platform → Adapter → Bridge Service → REST API + alma run → Alma
```

**Steps:**
1. **Inbound**: Bridge receives events from external platform
2. **Session mapping**: Maintain `{platform}:{chatId}` → Alma Thread ID (persist to file/DB)
3. **Thread creation**: `POST /api/threads {"title": "..."}` — appears in GUI
4. **AI completion**: `ALMA_THREAD_ID=<id> alma run --raw '<message>'`
5. **Outbound**: Call external platform API to send reply

See [rest-api.md](references/rest-api.md) for thread endpoints and
[cli-reference.md](references/cli-reference.md) for `alma run` flags.

### Comparison

| Feature | Approach A (WebSocket) | Approach B (REST + CLI) |
|---------|----------------------|------------------------|
| Streaming output | Real-time deltas | Must wait for full response |
| GUI integration | Native-level | Basic thread visibility |
| Profile matching | Automatic (SENDER PROFILE) | Manual (write profile files) |
| Reply/quoting | Supported | Not supported |
| Implementation effort | Higher (WebSocket protocol) | Lower (HTTP calls) |
| Dependencies | None (direct WS connection) | Requires `alma` CLI available |

### What Works vs What Doesn't

| Works | Doesn't Work |
|-------|-------------|
| Threads visible in GUI sidebar | No native channel settings panel |
| Full AI pipeline (SOUL + Memory + Tools + Plugins) | No channel registration API |
| Bidirectional messaging | No GUI ↔ external sync without Plugin |
| Per-user/group conversation history | No channel identity badges |
| People Profiles auto-loaded | |

---

## 8. Decision Guide

| Goal | Use | Reference |
|------|-----|-----------|
| Get AI reply from external script | `alma run` | [cli-reference.md](references/cli-reference.md) |
| Manage settings programmatically | REST API | [rest-api.md](references/rest-api.md) |
| React to AI replies (forward, log) | Plugin (`didReceive`) | [plugin-system.md](references/plugin-system.md) |
| Modify prompts before AI sees them | Plugin (`willSend`) | [plugin-system.md](references/plugin-system.md) |
| Monitor tool usage / errors | Plugin (`didExecute`, `onError`) | [plugin-system.md](references/plugin-system.md) |
| Teach AI new capabilities | Skill (SKILL.md) | [skills-and-mcp.md](references/skills-and-mcp.md) |
| Add tools from external service | MCP Server | [skills-and-mcp.md](references/skills-and-mcp.md) |
| Remember info about users | People Profiles | [people-profiles.md](references/people-profiles.md) |
| Connect unsupported chat platform | Bridge (WebSocket) | `alma-channel-protocol` skill |
| Quick prototype bridge | Bridge (REST + CLI) | [channel-bridge.md](references/channel-bridge.md) |
| Register a native channel adapter | Not possible | — |
