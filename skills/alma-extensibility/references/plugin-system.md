# Alma Plugin System Reference

Plugins are TypeScript extensions running inside Alma's process. They hook into message and tool lifecycle events.

## Directory Structure

```
~/.config/alma/plugins/<plugin-name>/
├── manifest.json    # Plugin metadata, permissions, events
└── main.js          # Compiled JavaScript (from main.ts)
```

## manifest.json Schema

```json
{
  "id": "my-plugin",
  "name": "My Plugin",
  "version": "1.0.0",
  "description": "What it does",
  "author": { "name": "you" },
  "main": "main.js",
  "engines": { "alma": "^0.1.0" },
  "type": "transform",
  "permissions": ["chat:read", "chat:write"],
  "activationEvents": ["onStartup"],
  "contributes": {
    "commands": [{"id": "myPlugin.cmd", "title": "My Command"}],
    "configuration": {
      "title": "My Plugin",
      "properties": {
        "myPlugin.enabled": {"type": "boolean", "default": true, "description": "Toggle"}
      }
    }
  }
}
```

### Plugin Types
- `"transform"` — modifies data flow (messages, tools)
- `"ui"` — adds UI elements

### Available Permissions
`chat:read`, `chat:write`, `settings:read`, `settings:write`, `tools:read`, `tools:write`, `commands:read`, `commands:write`, `ui:read`, `ui:write`, `storage:read`, `storage:write`

## Entry Point

```typescript
import { PluginContext } from 'alma-plugin-api';

export function activate(context: PluginContext) {
  context.logger.info('Plugin activated');
  // Hook events, register commands, etc.
}
```

## PluginContext API

| Category | API | Description |
|----------|-----|-------------|
| **Logging** | `context.logger.info/debug/error(msg)` | Plugin-scoped logging |
| **Events** | `context.events.on(event, handler)` | Subscribe to hook → returns `Disposable` |
| **Settings** | `context.settings.get<T>(key, default?)` | Read plugin or global setting |
| **Settings** | `context.settings.update(key, value)` | Write setting |
| **Settings** | `context.settings.onDidChange(handler)` | React to setting changes |
| **Commands** | `context.commands.register(id, handler)` | Register callable command |
| **Commands** | `context.commands.execute(id)` | Execute command |
| **UI** | `context.ui.createStatusBarItem()` | Status bar element |
| **UI** | `context.ui.showNotification(msg)` | Desktop notification |
| **UI** | `context.ui.showWarning(msg)` | Warning dialog |
| **UI** | `context.ui.showQuickPick(items)` | Selection picker |
| **UI** | `context.ui.showConfirmDialog(msg)` | Yes/No dialog |
| **Storage** | `context.storage.local.get/set(key, value)` | Persistent key-value storage |
| **Chat** | `context.chat.listThreads()` | List all threads |
| **Chat** | `context.chat.getMessages(threadId)` | Read thread messages |

## Hook Events

| Event | Trigger | Payload | Capabilities |
|-------|---------|---------|-------------|
| `chat.message.willSend` | Before user message reaches AI | `{content, model}` — **mutable** | Modify prompt text or model before AI sees it |
| `chat.message.didReceive` | After AI generates response | `{threadId, response, pricing}` | React to replies — e.g., forward to external channel |
| `thread.activated` | User switches to a thread | `{threadId, usage?, pricing?}` | Track engagement, show analytics |
| `tool.willExecute` | Before a tool runs | `{tool, args, context: {threadId}}` | Log, block, or modify tool arguments |
| `tool.didExecute` | After tool completes | `{tool, duration, context: {threadId}}` | Analytics, telemetry |
| `tool.onError` | Tool execution fails | `{tool, duration, error, context: {threadId}}` | Error monitoring, retry logic |

## Example: Message Forwarder Plugin

```typescript
import { PluginContext } from 'alma-plugin-api';

export function activate(context: PluginContext) {
  context.events.on('chat.message.didReceive', async (event) => {
    const { threadId, response } = event;
    const threads = await context.chat.listThreads();
    const thread = threads.find(t => t.id === threadId);

    if (thread?.title?.match(/^QQ(群|私聊) \d+/)) {
      await fetch('http://localhost:3000/send_msg', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ /* OneBot params */ })
      });
    }
  });
  context.logger.info('Forwarder activated');
}
```

## Plugin Limitations (Hard Boundaries)

- **Cannot create inbound messages** — no `chat.message.create` or equivalent. Plugins react to existing events.
- **Cannot register channel adapters** — channels are built into Alma's binary.
- **Cannot modify the AI pipeline** — no hook for `beforeModelCall` or `afterToolCall` (only `willSend`/`didReceive` on the user side).
- **Runs in Alma's process** — uncaught exceptions can crash the app. Always wrap handlers in try/catch.
- **TypeScript must be compiled** — `main.ts` must be compiled to `main.js` before placing in plugin directory.

## Related References

- [channel-bridge.md](channel-bridge.md) — Plugin `didReceive` hook can forward messages to external platforms
- [skills-and-mcp.md](skills-and-mcp.md) — Skills and MCP servers (complementary extension points)
- [rest-api.md](rest-api.md) — REST API that plugins can call via `fetch()` internally
