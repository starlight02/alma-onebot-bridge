# generate_response Protocol Reference

This document is the complete WebSocket protocol reference for Alma's `generate_response`
message and the server-to-bridge response events.

## WebSocket Endpoint

```
ws://127.0.0.1:23001/ws/threads
```

All bridges (built-in and custom) connect here. The server accepts JSON messages.

## Request: `generate_response`

```json
{
  "type": "generate_response",
  "data": {
    "threadId": "<uuid>",
    "model": "<model-id or undefined>",
    "userMessage": {
      "role": "user",
      "parts": [
        {"type": "text", "text": "<formatted message>"},
        {"type": "file", "url": "data:image/jpeg;base64,...", "mediaType": "image/jpeg", "filename": "photo.jpg"}
      ]
    },
    "ephemeralContext": "<full system prompt string>",
    "source": "<platform-identifier>"
  }
}
```

### Required Fields

| Field | Type | Description |
|-------|------|-------------|
| `data.threadId` | string | Alma thread UUID (from `channel_mappings` or `POST /api/threads`) |
| `data.userMessage` | object | The user message with `role` and `parts` |
| `data.userMessage.parts` | array | Array of content parts (text, file) |

### Optional Fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `data.model` | string \| null | Server default | Model ID (e.g., `"anthropic:claude-sonnet-4-20250514"`) |
| `data.source` | string | none | Platform identifier (triggers source-specific processing) |
| `data.ephemeralContext` | string | `""` | Per-turn system prompt (SENDER PROFILE, group history, etc.) |
| `data.retryOfMessageId` | string | none | Retry a failed generation from this message |
| `data.replaceMessageId` | string | none | Replace an existing message in-place |
| `data.tools` | string[] | all | Override available tool keys |
| `data.reasoningEffort` | string | default | Reasoning effort level |
| `data.enabledMCPServerIds` | string[] | none | MCP servers to enable for this turn |
| `data.noTools` | boolean | false | Disable all tools |
| `data.ephemeralModel` | string | none | Override model for this turn only |
| `data.userMessageMetadata` | object | none | Metadata attached to saved user message |
| `data.fromQuickChat` | boolean | false | Quick chat mode flag |
| `data.hummingbirdContext` | object | none | Hummingbird context data |

### `userMessage.parts` Content Types

| Type | Fields | Purpose |
|------|--------|---------|
| `text` | `text: string` | Plain text message content (includes `[From:]` prefix, `[msg:N]` tags) |
| `file` | `url: string`, `mediaType: string`, `filename: string` | Image/file attachment (supports `data:` URIs for base64) |
| `step-start` | none | Internal: marks start of a generation step |
| `reasoning` | `text: string` | Internal: model thinking/reasoning content |

Only `text` and `file` types should be sent by bridges. `step-start` and `reasoning` are
server-internal.

## Response Events (Server → Bridge)

### Event Sequence During Generation

The complete event sequence observed from a `generate_response` call:

```
 1. thread_created                          Thread created (new threads only)
 2. message_added      (user, text)         User message saved to DB
 3. message_updated    (user)               User message state updated
 4. skill_analysis_progress                 Skill analysis phase
 5. thread_generating  {isGenerating: true} Generation started
 6. message_added      (assistant, EMPTY!)  Assistant message shell created
 7. message_updated    (assistant, partial) Partial text during generation
 8. message_delta      (multiple)           Streaming text chunks
 9. generation_completed                    Generation finished
10. thread_generating {isGenerating: false} Generation ended
11. context_usage_update                     Token usage report
12. message_updated   (assistant, FULL)      Final complete text
```

### `message_delta` — Streaming Text Chunks

```json
{
  "type": "message_delta",
  "data": {
    "threadId": "...",
    "deltas": [
      {"type": "text_append", "partType": "text", "text": "chunk of text"},
      {"type": "text_append", "partType": "reasoning", "text": "...thinking..."}
    ]
  }
}
```

**Accumulation rule**: Only accumulate deltas where `partType == "text"`. Ignore
`partType == "reasoning"` (model thinking/internal).

### `message_updated` — Message State Change

```json
{
  "type": "message_updated",
  "data": {
    "threadId": "...",
    "messageId": "...",
    "role": "assistant",
    "text": "full or partial message text",
    "isGenerating": true
  }
}
```

Fires multiple times per message:
- During generation: `text` is partial, `isGenerating` is `true`
- After completion: `text` is the final complete text, `isGenerating` is `false`

### `message_added` — New Message Saved

```json
{
  "type": "message_added",
  "data": {
    "threadId": "...",
    "messageId": "...",
    "role": "assistant",
    "text": ""
  }
}
```

**Critical pitfall**: `text` is **always empty** for assistant messages. The message shell
is created before the AI starts generating. Do NOT use this event to capture assistant
replies — use `message_updated` or `message_delta` instead.

### `thread_generating` — Generation State Toggle

```json
{
  "type": "thread_generating",
  "data": {
    "threadId": "...",
    "isGenerating": true
  }
}
```

Use this to track whether a thread is currently generating. Essential for:
- Filtering `message_updated` events (only forward after `isGenerating: false`)
- Preventing duplicate forwarding during active generation
- UI indicators (typing animation, etc.)

### `generation_completed`

```json
{
  "type": "generation_completed",
  "data": {
    "threadId": "...",
    "itemId": "..."
  }
}
```

Signals that generation is finished. The final text is available in the last `message_updated`
event (with `isGenerating: false`).

### Other Events

| Event | Purpose |
|-------|---------|
| `tool_status` | Tool execution update: `{threadId, toolName, status}` |
| `error` | Error: `{threadId, error}` |
| `thread_updated` | Thread metadata changed (title, etc.) |
| `context_usage_update` | Token usage: `{threadId, usage}` |
| `skill_analysis_progress` | Skill analysis phase |

## Accumulating the Response

The recommended pattern for collecting the full AI response:

```
1. On generate_response sent: mark thread as "generating"
2. On message_delta (partType=="text"): append delta.text to buffer
3. On thread_generating {isGenerating: false}: mark thread as "done"
4. On message_updated (role=="assistant", after done): use as authoritative final text
5. Strip <think>...</think> blocks from accumulated text
6. Send to platform
```

Alternative: accumulate only from `message_delta` text_append events. This works but may
miss edge cases where the server modifies text after streaming (e.g., tool result insertion).

## Server-Side Source Processing

When the server receives `generate_response`, it checks `source`:

```javascript
if (source && ["telegram", "telegram-group", "discord", "feishu"].includes(source)) {
  // Strip stale ephemeral content from OLDER user messages in history:
  // - [SENDER PROFILE] blocks
  // - RECENT GROUP CHAT HISTORY blocks
  // - GROUP-SPECIFIC RULES blocks
  // - RECENT GROUP INTERACTIONS blocks
  // - PEOPLE PROFILES blocks
  // The LAST user message keeps its full ephemeral context.
}
```

**Important**: `"weixin"` is NOT included — WeChat messages do not get history stripping.

Additionally, the server appends channel-specific system prompt sections:

| Source | Added System Prompt |
|--------|-------------------|
| `"telegram"` | Telegram formatting rules, file/sticker/voice sending, message links |
| `"telegram-group"` | All of above + group chat rules (privacy, observation, frequency control) |
| `"discord"` | Discord markdown, channel directory, group behavior |
| `"feishu"` | Feish-specific rules (mostly handled via ephemeralContext from bridge) |
| `"weixin"` | WeChat file/voice sending, plain text formatting rules |
