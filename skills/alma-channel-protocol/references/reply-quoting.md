# Reply/Quoting Protocol Reference

This document covers the complete reply/quoting protocol for Alma channel bridges —
both incoming (platform user quotes a message) and outgoing (bridge bot replies to a
specific message).

## Overview

Alma's built-in bridges implement a `buildReplyContext()` function that converts a quoted
message into a text annotation. This annotation is prepended to the user's message text,
allowing the AI to understand the quoting context.

```typescript
// Telegram bridge — MessageBridge.buildReplyContext()
buildReplyContext(msg: TelegramMessage): string {
  if (!msg.reply_to_message) return "";
  const sender = msg.reply_to_message.from;
  const name = sender.id === botId ? "Alma (YOU)" : sender.first_name;
  const quoted = msg.reply_to_message.text?.slice(0, 200) || "";
  return `[Replying to ${name}'s message: "${quoted}"]`;
}
```

## Incoming: Platform User Quotes a Message

### Telegram Bridge

When a Telegram user replies to a message:

```typescript
// telegram.message_id is the actual platform message_id (integer)
// msg.reply_to_message contains the quoted message
const replyContext = buildReplyContext(msg.reply_to_message);
const text = `${replyContext}\n${msg.text}`;
// Sent as: [msg:message_id] [Replying to X: "..."]\nactual text
```

### Discord Bridge

```typescript
// discord.message.id is a Snowflake ID (string)
// msg.reference?.messageId points to the quoted message
const quoted = await channel.messages.fetch(msg.reference.messageId);
const replyContext = `[Replying to ${quoted.author.username}'s message: "${quoted.content.slice(0, 200)}"]`;
```

### QQ/OneBot Bridge

OneBot v11 sends a `reply` segment as the **first element** of the message array:

```json
{
  "message": [
    {"type": "reply", "data": {"id": "12345"}},
    {"type": "at", "data": {"qq": "12345678"}},
    {"type": "text", "data": {"text": "my reply"}}
  ]
}
```

**Extraction**: Check if `segments[0].type === "reply"`, then read `segments[0].data.id`.

**Fetching quoted content**: Call the OneBot `get_msg` API:

```json
// Request
{"action": "get_msg", "params": {"message_id": "12345"}, "echo": "..."}

// Response
{
  "status": "ok",
  "retcode": 0,
  "data": {
    "message_id": 12345,
    "sender": {"user_id": 87654321, "nickname": "Alice"},
    "message": [{"type": "text", "data": {"text": "original message"}}],
    "raw_message": "original message"
  }
}
```

**Text extraction**: Prefer extracting from `data.message[]` segments (filter `type: "text"`,
concatenate). Fall back to `data.raw_message` if segments are unavailable.

**Formatted result**:

```
[From: 萌依 [id:123456789] [msg:12346]] [Replying to Alice's message: "original message text"]
my reply
```

### Handling Edge Cases

| Case | Behavior |
|------|----------|
| Quoted message deleted | `get_msg` fails → skip reply context, process normally |
| Quoted text > 200 chars | Truncate to 200 chars + append `...` |
| Quoted message is from bot | Use `"Alma (YOU)"` as sender name |
| No reply segment | No reply context → message processed without quoting info |
| `get_msg` timeout | Same as failure → skip silently, log at debug level |

## Outgoing: Bot Replies to a Specific Message

### Telegram Bridge

```typescript
await telegram.sendMessage(chatId, responseText, {
  reply_parameters: { message_id: userMessageId },
  // Only set for the first chunk of multi-part responses
});
```

### Discord Bridge

```typescript
await channel.send({
  content: responseText,
  messageReference: { messageId: originalMessageId },
  // Only for first chunk
});
```

### QQ/OneBot Bridge

Prepend a `reply` segment to the message array:

```json
{
  "action": "send_msg",
  "params": {
    "message_type": "group",
    "group_id": 100200300,
    "message": [
      {"type": "reply", "data": {"id": "<user_message_id>"}},
      {"type": "text", "data": {"text": "bot's response"}}
    ]
  }
}
```

**Critical**: The `reply` segment MUST be the **first element** in the message array.
Some OneBot implementations (NapCat, Lagrange) ignore the reply reference if it's not first.

### Multi-Part Response Rules

When the AI response is split into multiple messages (paragraphs, character limit):

| Chunk | Reply Reference |
|-------|----------------|
| First chunk of first paragraph | YES — include reply segment |
| Subsequent chunks | NO — send as plain messages |

This prevents redundant reply markers in the chat while still providing conversation context
for the primary response.

### Alma GUI → Platform Forwarding

When a message is typed in the Alma GUI and forwarded to the platform, do NOT include a
reply reference — there is no specific user message to reply to. The message appears as
a standalone bot message.

## Group History Format

In group chat history (written to `~/.config/alma/groups/`), messages use a compact format
with `[msg:N]` references:

```
[14:30] [msg:42] [萌依]: 大家好
[14:31] [msg:43] [Alma (YOU)]: 你好！有什么可以帮忙的吗？
[14:32] [msg:44] [Alice]: [Replying to Alma (YOU)'s message: "你好！有什么可以帮忙的吗？"] 帮我查一下天气
```

The `[msg:N]` values are the real platform message IDs, enabling cross-referencing between
history logs and live messages.

## Platform Message ID Conventions

| Platform | Message ID Type | Example | Used In |
|----------|----------------|---------|---------|
| Telegram | Integer (sequential per chat) | `42` | `[msg:42]` |
| Discord | Snowflake string | `"1234567890123456789"` | `[msg:1234567890123456789]` |
| Feishu | Message ID string | `"om_xxx"` | `[msg:om_xxx]` |
| QQ/OneBot | Integer | `12345` | `[msg:12345]` |

The message ID in `[msg:N]` must be the **actual platform message_id** from the incoming
event, NOT a sequential counter or hardcoded 0. This is what makes message referencing
work correctly in Alma's group history and reply threading.
