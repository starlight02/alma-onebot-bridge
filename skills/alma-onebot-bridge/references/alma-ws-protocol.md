# Alma WebSocket Protocol Reference

Detailed protocol documentation for the Alma internal WebSocket endpoint at `ws://localhost:23001/ws/threads`. This is the same protocol used by the Alma GUI and `alma run` CLI.

## Table of Contents

1. [Alma REST API Endpoints](#alma-rest-api-endpoints)
2. [Generating a Response](#generating-a-response)
3. [Event Sequence During Generation](#event-sequence-during-generation)
4. [message_added vs message_updated](#message_added-vs-message_updated)
5. [Message Parts Format](#message-parts-format)
6. [message_delta Accumulation](#message_delta-accumulation)
7. [Think Block Stripping](#think-block-stripping)
8. [OneBot v11 Protocol Formats](#onebot-v11-protocol-formats)
9. [Echo Correlation Details](#echo-correlation-details)
10. [Model Priority Chain](#model-priority-chain)

## Alma REST API Endpoints

Base URL: `http://localhost:23001`

| Endpoint | Method | Purpose |
|---|---|---|
| `/api/threads` | `GET` | List all threads |
| `/api/threads` | `POST` | Create thread: `{"title": "..."}` -> `{"id": "..."}` |
| `/api/threads/:id` | `GET` | Get thread with messages |
| `/api/threads/:id` | `PUT` | Update thread title |
| `/api/threads/:id` | `DELETE` | Delete thread |
| `/api/settings` | `GET` | App settings (includes `chat.defaultModel`) |
| `/api/settings` | `PUT` | Update app settings |
| `/api/health` | `GET` | Health check |

There is no `POST /api/threads/:id/messages` endpoint. Message sending must go through the WebSocket protocol. The Electron app uses IPC/internal channels for this.

## Generating a Response

Send a `generate_response` message over the WebSocket:

```json
{
  "type": "generate_response",
  "data": {
    "threadId": "<thread_id>",
    "model": "anthropic:claude-sonnet-4-20250514",
    "userMessage": {
      "role": "user",
      "parts": [{"type": "text", "text": "user message here"}]
    }
  }
}
```

## Event Sequence During Generation

The full observed event sequence from a `generate_response` call:

```
 1. thread_created
 2. message_added       (user, text = user message)
 3. message_updated     (user)
 4. skill_analysis_progress
 5. thread_generating   {isGenerating: true}
 6. message_added       (assistant, text = EMPTY!)
 7. message_updated     (assistant, partial text)
 8. message_delta       (multiple, text_append accumulation)
 9. generation_completed
10. thread_generating   {isGenerating: false}
11. context_usage_update
12. message_updated     (assistant, FULL final text)
```

## message_added vs message_updated

| Event | Assistant Text | When |
|---|---|---|
| `message_added` | Always empty for assistant | Fires immediately when message shell is created |
| `message_updated` | Partial during generation, full after completion | Fires multiple times |

For bidirectional forwarding, always use `message_updated`. The `message_added` event is useless for capturing assistant replies because the text is always empty at that point.

## Message Parts Format

Messages contain a `parts` array with typed content:

```json
{
  "parts": [
    {"type": "step-start"},
    {"type": "reasoning", "text": "...thinking..."},
    {"type": "text", "text": "actual response text"}
  ]
}
```

Only extract `type: "text"` parts. Skip `step-start` and `reasoning` (internal model thinking).

## message_delta Accumulation

During generation, `message_delta` events carry incremental text:

```json
{
  "type": "message_delta",
  "data": {
    "threadId": "...",
    "deltas": [
      {"type": "text_append", "partType": "text", "text": "chunk of text"},
      {"type": "text_append", "partType": "reasoning", "text": "..."}
    ]
  }
}
```

Only accumulate deltas where `partType` is `"text"`. Ignore `reasoning` deltas (model thinking).

## Think Block Stripping

Some models emit `<think>...</think>` blocks in accumulated text. Strip these before returning the final response. The stripping function processes the text sequentially, removing everything between `<think>` and `</think>` tags (inclusive).

## OneBot v11 Protocol Formats

### Event Format (OneBot -> Bridge)

```json
{
  "time": 1718000000,
  "self_id": 12345678,
  "post_type": "message",
  "message_type": "private",
  "sub_type": "friend",
  "message_id": 100,
  "user_id": 87654321,
  "message": [{"type": "text", "data": {"text": "hello"}}],
  "sender": {"user_id": 87654321, "nickname": "Alice", "card": "group card name"}
}
```

### API Call Format (Bridge -> OneBot)

```json
{
  "action": "send_msg",
  "params": {
    "message_type": "private",
    "user_id": 87654321,
    "message": "hi"
  },
  "echo": "unique-id-string"
}
```

### API Response Format (OneBot -> Bridge)

```json
{
  "status": "ok",
  "retcode": 0,
  "data": {"message_id": 200},
  "echo": "unique-id-string"
}
```

### Message Segment Format

Always use the array format, not CQ string format:

```json
[{"type": "text", "data": {"text": "hello"}}]
```

The CQ string format (`[CQ:text,text=hello]`) is not used by the bridge.

## Echo Correlation Details

The bridge and OneBot client share a single WS connection for both events and API calls. The `echo` field disambiguates:

- Messages with `echo` + `retcode` = API responses (correlated to pending calls)
- Messages with `post_type` = Events (dispatched to event handlers)

The echo value format is `bridge-{uuid}-{action}` using uuid v4 for uniqueness.

## Model Priority Chain

The AI model used for generation follows this priority:

1. `ALMA_MODEL` env var (highest priority)
2. `alma.model` in `config.toml`
3. Alma's default model from `GET /api/settings` -> `chat.defaultModel`
4. Hardcoded fallback: `anthropic:claude-sonnet-4-20250514`
