# Development Knowledge Base

This document captures technical decisions, API findings, and operational knowledge
for the Alma × OneBot v11 Bridge project.

---

## 1. Architecture Overview

### Reverse WebSocket Bridge with Alma WebSocket Pipeline

The bridge acts as a **WebSocket Server** for the OneBot client (snowluma/NapCat) and
a **WebSocket Client** for Alma's internal chat pipeline. All AI generation goes through
the Alma WebSocket protocol, ensuring messages are persisted in threads and visible in
the GUI, with full access to SOUL, Memory, People Profiles, and Skills.

```
QQ User ──► snowluma (OneBot) ──WS──► Bridge ──WS──► Alma (ws://localhost:23001/ws/threads)
                                       │
                                       ├──REST──► Alma REST API (thread creation, settings)
                                       │
                                       └──bidirectional──► Alma GUI ↔ QQ forwarding
```

### Data Flow (QQ → Alma → QQ)

1. QQ user sends message
2. snowluma pushes OneBot event via reverse WS to bridge
3. Bridge extracts text + face emojis + media info, filters @bot for group messages
4. Bridge records message to group chat history ring buffer (all group messages, before @bot gate)
5. Bridge handles reply/quoting context and forwarded message extraction
6. Bridge ensures People Profile exists for the user
7. Bridge finds or creates an Alma Thread (REST API `POST /api/threads`)
8. Bridge sends `generate_response` via Alma WebSocket (`ws://localhost:23001/ws/threads`)
9. Alma processes with full pipeline (SOUL + Memory + People Profiles + Skills)
10. Bridge collects response via `message_delta` text accumulation
11. Bridge splits reply into paragraphs and sends each via OneBot `send_msg` API (with reply + @mention for first chunk in groups)
12. QQ user receives the reply

### Data Flow (Alma GUI → QQ, Bidirectional)

1. User types message in Alma GUI for a tracked thread
2. Alma processes and generates response
3. Bridge receives `message_updated` event (after generation completes)
4. Bridge checks dedup (was this reply already sent by the bridge pipeline?)
5. If not duplicate, bridge forwards to QQ via OneBot `send_msg`

### Module Structure

| Module | Responsibility |
|--------|---------------|
| `config.rs` | Config from `config.toml` + env var overrides (priority: env > TOML > default) |
| `state.rs` | Shared state with Turso (libsql) persistence + in-memory group chat history ring buffer |
| `onebot/event.rs` | OneBot v11 event/message serde types + helpers for text, media, face, forward extraction |
| `onebot/api.rs` | Echo-based WS API call mechanism + `send_text_message`, `send_reply_message`, `get_msg`, `get_forward_msg` helpers |
| `handlers/ws.rs` | Reverse WS connection lifecycle + access token validation + recall event logging + bidirectional forwarding |
| `handlers/http.rs` | Health check endpoint |
| `alma.rs` | Alma REST API: thread creation (`POST /api/threads`) + settings fetch |
| `alma_ws.rs` | Alma WebSocket client: `generate_response` + event dispatch + bidirectional events |
| `people.rs` | Auto-create People Profile files in `~/.config/alma/people/` |
| `face_map.rs` | QQ face expression ID → human-readable name lookup table (~100 common expressions) |
| `pipeline.rs` | End-to-end message processing + media/face/forward handling + group history + bidirectional forwarding |

---

## 2. Key Technical Findings

### 2.1 Alma REST API (`localhost:23001`)

**Thread management and settings are available; message sending is NOT exposed via REST.**

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/api/threads` | `GET` | List all threads |
| `/api/threads` | `POST` | Create thread: `{"title": "..."}` → returns `{"id": "..."}` |
| `/api/threads/:id` | `GET` | Get thread with messages |
| `/api/threads/:id` | `PUT` | Update thread title |
| `/api/threads/:id` | `DELETE` | Delete thread |
| `/api/settings` | `GET` | App settings (includes `chat.defaultModel`) |
| `/api/settings` | `PUT` | Update app settings |
| `/api/health` | `GET` | Health check |

**Critical**: There is NO `POST /api/threads/:id/messages` or similar endpoint.
The Electron app uses IPC/internal channels for message sending.
To get AI replies programmatically, use the **Alma WebSocket protocol** (preferred) or
the `alma run` CLI.

### 2.2 Alma WebSocket Protocol (`ws://localhost:23001/ws/threads`)

**This is the primary interface for AI generation.** It is the same protocol used by the
Alma GUI and `alma run` CLI, ensuring full pipeline access.

#### Generating a Response

Send a `generate_response` message:

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

#### Event Sequence During Generation

The full event sequence observed from a `generate_response` call:

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
10. thread_generating  {isGenerating: false}
11. context_usage_update
12. message_updated    (assistant, FULL final text)
```

#### Critical Discovery: `message_added` vs `message_updated`

| Event | Assistant Text | When |
|-------|---------------|------|
| `message_added` | **Always empty** for assistant | Fires immediately when message shell is created |
| `message_updated` | **Partial** during generation, **full** after completion | Fires multiple times |

**Implication for bidirectional forwarding**: You CANNOT use `message_added` to capture
assistant replies — the text is always empty. Use `message_updated` and filter by
generation state (only forward the final update after `thread_generating {isGenerating: false}`).

#### Message Parts Format

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

Only extract `type: "text"` parts. Skip `step-start` and `reasoning` (internal thinking).

#### `message_delta` Accumulation

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

Only accumulate `partType: "text"` deltas. Ignore `reasoning` (thinking) deltas.

#### `<think>...</think>` Blocks

Some models emit thinking blocks in accumulated text. The bridge strips these with
`strip_think_blocks()` before returning the final response.

### 2.3 Alma CLI: `alma run` (Fallback)

```bash
ALMA_THREAD_ID="<thread_id>" alma run --no-stream --raw "user message here"
```

The CLI is a fallback for environments where WebSocket is unavailable.
The bridge uses the WebSocket protocol directly for better performance and integration.

### 2.4 OneBot v11 Reverse WebSocket Protocol

**Connection**: OneBot client connects to `ws://bridge-host:port/`

**Headers** (sent by OneBot client):
- `X-Self-Id: <bot_qq_id>` — the bot's QQ number
- `X-Client-Role: Universal` — connection role

**Event format** (OneBot → Bridge):
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
  "sender": {"user_id": 87654321, "nickname": "Alice", "card": "群名片"}
}
```

**API call format** (Bridge → OneBot):
```json
{
  "action": "send_msg",
  "params": {"message_type": "private", "user_id": 87654321, "message": "hi"},
  "echo": "unique-id-string"
}
```

**API response format** (OneBot → Bridge):
```json
{
  "status": "ok",
  "retcode": 0,
  "data": {"message_id": 200},
  "echo": "unique-id-string"
}
```

### 2.5 Echo Correlation Mechanism

The bridge and OneBot share a single WS connection for both events and API calls.
The `echo` field disambiguates:

- Messages WITH `echo` + `retcode` → **API responses** (correlated to pending calls)
- Messages WITH `post_type` → **Events** (dispatched to event handlers)

Implementation: `HashMap<String, oneshot::Sender<ApiResponse>>` protected by `Arc<Mutex<>>`.
Each API call gets a unique echo ID (`bridge-{uuid}-{action}`), stores a oneshot sender,
and awaits the response with configurable timeout.

### 2.6 Warp 0.4.3 Specifics

| Item | Detail |
|------|--------|
| `server` feature | **Must be explicitly enabled** — not default in 0.4.x |
| `websocket` feature | Must be explicitly enabled for WS support |
| `warp::ws()` filter | Performs WS handshake upgrade on same HTTP port |
| `warp::ws::Message` | Has `is_text()`, `to_str()`, `is_close()`, `Message::text()` |
| Route composition | `.or()` chains filters; HTTP and WS coexist on same port |

### 2.7 People Profiles

Location: `~/.config/alma/people/{qq_id}.md`

Format: YAML frontmatter + Markdown body. The frontmatter uses Alma's standard fields
with both `telegram_id` and `qq_id` (set to the same QQ ID) for cross-platform matching:

```markdown
---
telegram_id: "12345678"
qq_id: "12345678"
username: "Alice"
---
# Alice

- QQ 用户，ID: 12345678
- 昵称: Alice
- 首次互动: 2026-06-19
```

**Important**: Use `username` (not `qq_nickname`) in frontmatter — this matches
Alma's standard field naming. Include both `telegram_id` and `qq_id` so the same
person can be matched by both the QQ bridge and the Telegram bridge.

Profile files are named by QQ ID (not nickname) to prevent collisions when different
users share the same display name across groups.

When Alma processes a message, it automatically loads relevant People Profiles
into the AI context based on the `qq_id` field, giving the AI "memory" about QQ users.

### 2.8 Message Format (Telegram-Style for Alma Protocol)

Messages sent to Alma use the same format as Alma's built-in bridges:

| Chat Type | Format |
|-----------|--------|
| Group | `[From: Alice \| id:12345678]\n\n[msg:12345] 消息内容` |
| Private | `[From: Bob \| id:12345678]\n\n[msg:67890] 消息内容` |
| With Reply | `[From: Alice \| id:12345678]\n\n[msg:12345] [Replying to Bob's message: "引用的消息内容"]\n实际消息` |
| With Forward | `[From: Alice \| id:12345678]\n\n[msg:12346] [Forwarded messages (3 total):Alice: "hello", Bob: "world", Charlie: "hi"]` |
| With Media | `[From: Alice \| id:12345678]\n\n[msg:12347] 看看这个\n[Image: https://gchat.qpic.cn/...]\n[Voice message]` |
| With Emoji | `[From: Alice \| id:12345678]\n\n[msg:12348] 哈哈哈 [emoji:斜眼笑] [emoji:doge]` |

`[msg:N]` uses the real OneBot `message_id` from the event (same pattern as Telegram bridge's
`message_id`). This lets Alma reference messages in group history logs.

When a user replies to (quotes) a message, the bridge fetches the quoted message content
via OneBot `get_msg` API and prepends a `[Replying to X's message: "..."]` line, matching
Telegram bridge's `buildReplyContext()` pattern. Quoted text is truncated at 200 chars.

When a user forwards merged messages, the bridge fetches the forwarded content via OneBot
`get_forward_msg` API and prepends a `[Forwarded messages (N total):...]` summary. Limited
to the first 20 nodes, each node's text truncated at 100 chars.

Face segments (`type: "face"`) are converted to `[emoji:name]` text (e.g., `[emoji:斜眼笑]`)
using the face_map lookup table. Unknown face IDs become `[emoji:face_X]`.

Media segments (image, record, video, share, location) are appended as additional lines
after the message text. Image URLs are included as `[Image: <url>]` so the AI can potentially
access them. Other media types get human-readable labels like `[Voice message]`, `[Video]`.

The QQ ID is included as a stable identifier (nicknames change frequently).
The `source` field is set to `"telegram-group"` for groups and `"telegram"` for private chats,
so Alma's server applies the appropriate group chat rules and history stripping.

Additionally, `ephemeralContext` carries:
- `[SENDER PROFILE — name]: ... [/SENDER PROFILE]` block (from people profile matching)
- `PEOPLE PROFILES — You know N people.` summary line
- `RECENT GROUP CHAT HISTORY (last N messages):` block with timestamps (for group chats)

### 2.9 Bidirectional Forwarding Architecture

```
Alma WS reader task
  ↓ (message_updated events)
AlmaWsClient internal channel
  ↓ (drain task polls every 500ms)
tokio::sync::broadcast channel (capacity: 64)
  ↓ (each OneBot connection subscribes)
handle_alma_event()
  ↓ (dedup check: first 100 chars)
OneBot send_msg → QQ
```

**Generating-thread filter**: A `HashSet<String>` in the reader task tracks threads
currently generating. `message_updated` for assistant messages is only forwarded when
the thread is NOT generating. This prevents:
- Forwarding partial text during active generation
- Duplicate messages (bridge pipeline sends directly, Alma event would re-send)

**Dedup mechanism**: `register_sent_reply()` + `was_sent_recently()` compare the first
100 characters of outgoing text. This prevents echo loops when the bridge sends a reply
to QQ and Alma also records it in the thread.

### 2.10 Reply/Quoting Protocol

#### Incoming (QQ → Alma)

When a QQ user replies to (quotes) a message, OneBot sends a `reply` segment as the
**first element** of the message array:

```json
{"message": [
  {"type": "reply", "data": {"id": "12345"}},
  {"type": "text", "data": {"text": "我的回复"}}
]}
```

The bridge:
1. Extracts the reply segment's `data.id` (quoted message ID) via `extract_reply_id()`
2. Calls OneBot `get_msg` API to fetch the quoted message's sender name + text
3. Formats as `[Replying to <name>'s message: "<text up to 200 chars>"]`
4. Prepends this line before the user's actual message in the `[msg:N]` block

If `get_msg` fails (unsupported client, deleted message, timeout), the reply context
is silently skipped — the message is still processed normally.

#### Outgoing (Alma → QQ)

The bot **always replies to the triggering user message** (matching Telegram bridge's
`reply_parameters: {message_id: userMessageId}` pattern). This is done by prepending a
`reply` segment to the OneBot message array:

```json
{"message": [
  {"type": "reply", "data": {"id": "<user_message_id>"}},
  {"type": "at", "data": {"qq": "<user_id>", "name": "username"}},
  {"type": "text", "data": {"text": "bot的回复"}}
]}
```

In **group chats**, an `at` segment is inserted between the `reply` and `text` segments
to @mention the user who triggered the response. This ensures they receive a notification.
In **private chats**, no `at` segment is added (unnecessary in 1-on-1 conversations).

Only the **first chunk** of the first paragraph uses the reply segment (and @mention).
Subsequent paragraphs/chunks use plain `send_text_message` to avoid redundant reply markers.

For Alma GUI → QQ forwarding (`handle_alma_event`), no reply segment is used since
there's no specific user message to reply to.

#### OneBot APIs Used

| API | Purpose | Response Fields Used |
|-----|---------|---------------------|
| `get_msg` | Fetch quoted message by ID | `sender.nickname`, `message[]` segments or `raw_message` |
| `get_forward_msg` | Fetch forwarded message content by forward ID | `data.message[]` nodes with `nickname` + `content` |
| `send_msg` | Send with reply + optional at segment | `data.message_id` (returned for logging) |

### 2.11 People Profile Cross-Platform Identity

Each QQ user's People Profile file (`{qq_id}.md`) includes both `telegram_id` and
`qq_id` in the YAML frontmatter, set to the same QQ ID value. This enables:

- **QQ bridge**: matches by `qq_id` frontmatter field
- **Telegram bridge**: matches the same person by `telegram_id` field
- **Cross-platform identity**: Alma recognizes the same person across QQ and Telegram

### 2.12 Face Segment Conversion

QQ uses `face` segments with numeric IDs to represent emoji expressions. There are ~348 known
face IDs, but only a subset is commonly used. The bridge converts these to human-readable
text so Alma's AI can understand the emotional content.

**Implementation**: `face_map.rs` contains a `face_name(id: &str) -> Option<&'static str>`
function mapping ~100 common face IDs to Chinese names:

| Face ID | Name | Usage |
|---------|------|-------|
| 178 | 斜眼笑 | Smirk/sarcastic laugh |
| 179 | doge | Doge meme face |
| 271 | 吃瓜 | Eating melon (watching drama) |
| 180 | 惊喜 | Surprised |
| ... | ... | ... |

**Conversion**: `convert_faces_to_text()` in `event.rs` finds all `face` segments and
produces text like `[emoji:斜眼笑] [emoji:doge]`. Unknown IDs become `[emoji:face_123]`.

**Pipeline integration**: Face text is combined with plain text before the empty-message check.
A message containing only face emojis (no text) is NOT skipped.

### 2.13 Incoming Media Handling

When users send non-text content (images, voice, video, shares, locations), the bridge
describes them in the message forwarded to Alma.

**Helper functions in `event.rs`**:

| Function | Returns | Purpose |
|----------|---------|---------|
| `extract_images()` | `Vec<String>` | URLs of all `image` segments |
| `extract_media_summary()` | `Vec<String>` | Human-readable labels: `[Image]`, `[Voice message]`, `[Video]`, `[Shared: title]`, `[Location: desc]` |
| `has_media_segments()` | `bool` | Whether message contains any media types |

**Pipeline integration**:

1. Image URLs are appended as `[Image: <url>]` lines — the AI can potentially access them
2. Other media types get descriptive labels (e.g., `[Voice message]`)
3. The empty-text check considers media: a message with images but no text is NOT skipped
4. When `get_msg` fetches a quoted message containing images, `[image]` is appended to the quoted text

### 2.14 Forwarded Message Extraction

QQ supports "merged forwarded messages" (合并转发), where multiple messages from different
users are bundled into a single forward card. OneBot represents this as a `forward` segment
containing only an opaque `id` — the actual content must be fetched separately.

**Flow**:

1. `extract_forward_id()` finds the `forward` segment and returns its `id`
2. `get_forward_msg()` calls the OneBot API with this ID
3. The response contains an array of `node` segments, each with `nickname` + `content`
   (nested message array)
4. Bridge extracts text from each node's content and formats as:
   `[Forwarded messages (N total):Alice: "hello", Bob: "world", ...]`

**Limits**: First 20 nodes only, each node's text truncated at 100 chars. This prevents
massive forwarded messages from overwhelming the AI context.

**Fallback**: If `get_forward_msg` fails (unsupported client, expired forward, timeout),
the bridge falls back to `[Forwarded message]` without content details.

### 2.15 Group Chat History Context

The bridge maintains an in-memory ring buffer of recent group messages, injected into
`ephemeralContext` so Alma's AI knows what the group has been discussing — even messages
that didn't @mention the bot.

**Architecture**:

- **Storage**: `HashMap<String, VecDeque<GroupMessage>>` in `AppState`, keyed by session key
- **GroupMessage**: `{ display_name: String, text: String, timestamp: u64 }`
- **Capacity**: Configurable via `group_history_size` (default: 30, set to 0 to disable)
- **Persistence**: In-memory only — resets on bridge restart (intentional: ephemeral context)

**Pipeline order (critical)**:

```
extract text + faces + media
  → compute display_name (BEFORE @bot check)
  → record to group history (ALL group messages, before @bot gate)
  → @bot check (early return if no @bot)
  → ... rest of pipeline
```

The `display_name` computation and history recording happen BEFORE the `@bot` filter.
This ensures the history captures the full group conversation, not just messages directed
at the bot. If these steps were after the `@bot` gate, the history would only contain
bot-directed messages, defeating the purpose.

**Injected format** (in `ephemeralContext`):

```
RECENT GROUP CHAT HISTORY (last 5 messages):
[14:30] Alice: 大家好
[14:31] Bob: 你好啊
[14:35] Charlie: 今天天气不错
[14:36] Alice: 是啊
[14:40] Bob: 出去玩吗
```

Timestamps are formatted as HH:MM in UTC+8. Each message is truncated at 200 chars.

### 2.16 Thinking Indicator (Optional)

OneBot v11 has **no typing indicator API** and no message edit API, so there's no way to
show "typing..." in the traditional sense. The bridge offers an optional "thinking message"
workaround: send a brief text message before generation starts, so users see activity
immediately while Alma processes.

**Configuration**: Disabled by default. Enable via config:

```toml
[chat]
thinking_message = "思考中..."
```

Or env var: `THINKING_MESSAGE="思考中..."`

**Behavior**: When enabled, the bridge sends a plain text message (NOT as a reply) to the
chat before calling `alma_ws.generate()`. The actual AI reply follows as a separate message
(with reply segment + @mention). This adds one extra message per interaction.

**Trade-off**: The thinking message provides immediate feedback but increases message noise.
For fast responses (< 3 seconds), the extra message may feel unnecessary. For slow responses
(> 10 seconds), it reassures users the bot is working. The default (disabled) is conservative.

### 2.17 Message Recall Logging

When users recall (withdraw) messages in QQ, OneBot sends a `notice` event with
`notice_type: "group_recall"` or `notice_type: "friend_recall"`. The bridge logs these
events for visibility.

**Event fields**:

| Field | Type | Present In | Description |
|-------|------|-----------|-------------|
| `user_id` | `i64` | Both | The user who recalled the message |
| `operator_id` | `Option<i64>` | `group_recall` | Admin who performed the recall (may differ from `user_id`) |
| `message_id` | `i64` | Both | The recalled message's ID |
| `group_id` | `i64` | `group_recall` | The group where the recall happened |

**Log output examples**:
- `[Recall] User 12345 recalled message 67890 in group 111222`
- `[Recall] Admin 99999 recalled user 12345's message 67890 in group 111222`
- `[Recall] User 12345 recalled private message 67890`

**Why logging only**: Alma's server has no "delete message from thread" API, and QQ's
`delete_msg` only deletes the bot's own messages. Logging provides visibility for
debugging and potential future use.

### 2.18 WebSocket Access Token Authentication

The bridge supports OneBot's `access_token` authentication mechanism. When configured,
incoming WebSocket connections must provide a valid token or they are rejected.

**Token delivery**: The OneBot client sends the token via the `Authorization: Bearer <token>`
HTTP header during the WebSocket handshake. Some clients also support `?access_token=<token>`
as a query parameter, but the bridge only validates the header.

**Validation flow**:

1. Warp extracts `Authorization` header as `Option<String>` before WS upgrade
   (using `warp::header::optional::<String>("authorization")`)
2. Handler compares the extracted token against the configured `access_token`
3. If `access_token` is not configured (`None`), all connections are accepted
4. If configured and the header is missing, malformed, or doesn't match, the connection
   is rejected with a warning log and immediately closed

**Configuration**: `ACCESS_TOKEN` env var or `onebot.access_token` in config.toml.

---

## 3. Configuration

Configuration supports **three layers** with priority: **env vars > config.toml > defaults**.

The repository ships with `config.toml.example` as a template. Copy it to `config.toml`
and edit as needed. `config.toml` is in `.gitignore` to prevent committing local settings
(especially `access_token` values).

### config.toml

```toml
[bridge]
port = 8090

[alma]
api = "http://localhost:23001"
model = "anthropic:claude-sonnet-4-20250514"  # Override Alma's default model
timeout = 120                                  # Generation timeout in seconds
max_retries = 2                                # Retry attempts for failed generations
retry_delay_ms = 3000                          # Base delay between retries (exponential backoff)

[database]
path = "bridge-state.db"

[people]
# dir = "~/.config/alma/people"

[onebot]
api_timeout = 30
# access_token = ""

[chat]
group_history_size = 30        # Number of recent group messages for context (0 = disabled)
# thinking_message = "思考中..."  # Optional message sent before AI generation starts
```

The bridge looks for `config.toml` or `bridge.toml` in the current directory.

### Environment Variables

| Variable | TOML Key | Default | Description |
|----------|----------|---------|-------------|
| `BRIDGE_PORT` | `bridge.port` | `8090` | WebSocket/HTTP listen port |
| `ALMA_API` | `alma.api` | `http://localhost:23001` | Alma REST API base URL |
| `ALMA_MODEL` | `alma.model` | *(Alma settings)* | Override AI model |
| `ALMA_TIMEOUT` | `alma.timeout` | `120` | Generation timeout (seconds) |
| `ALMA_MAX_RETRIES` | `alma.max_retries` | `2` | Number of retry attempts for failed generations |
| `ALMA_RETRY_DELAY` | `alma.retry_delay_ms` | `3000` | Base delay between retries (ms, exponential backoff) |
| `PEOPLE_DIR` | `people.dir` | `~/.config/alma/people` | People profiles directory |
| `DB_PATH` | `database.path` | `bridge-state.db` | Turso database file path |
| `ONEBOT_API_TIMEOUT` | `onebot.api_timeout` | `30` | OneBot API call timeout (seconds) |
| `ACCESS_TOKEN` | `onebot.access_token` | *(none)* | OneBot access token for WS auth |
| `GROUP_HISTORY_SIZE` | `chat.group_history_size` | `30` | Number of recent group messages for context (0 = disabled) |
| `THINKING_MESSAGE` | `chat.thinking_message` | *(none)* | Optional "thinking" message sent before generation |
| `RUST_LOG` | *(n/a)* | `info` | Tracing log level (env-filter syntax) |

### Model Priority

The AI model used for generation follows this priority:
1. `ALMA_MODEL` env var (highest)
2. `alma.model` in config.toml
3. Alma's default model from `GET /api/settings` → `chat.defaultModel`
4. Hardcoded fallback: `anthropic:claude-sonnet-4-20250514`

---

## 4. State Persistence

### Turso (libsql) Database

The bridge uses a local Turso database file (`bridge-state.db` by default) with two tables:

```sql
CREATE TABLE threads (
    session_key TEXT PRIMARY KEY,   -- "private:12345678" or "group:98765432"
    thread_id TEXT NOT NULL          -- Alma thread ID
);

CREATE TABLE profiles (
    user_id TEXT PRIMARY KEY,       -- QQ user ID as string
    profile_name TEXT NOT NULL       -- Sanitized filename for the people profile
);
```

**Session key format**: `{type}:{id}` where type is `private` or `group`.

**Reverse lookup**: An in-memory `HashMap<String, String>` maps `thread_id → session_key`
for bidirectional forwarding (Alma GUI → QQ). Populated on `get_thread_id()` and
`set_thread_id()` calls.

---

## 5. Common Pitfalls

### 5.1 Warp 0.4.x `server` Feature

The `warp::serve()` function requires the `server` feature flag. In warp 0.4.x this
is NOT enabled by default (unlike 0.3.x). Add it to Cargo.toml:

```toml
warp = { version = "0.4", features = ["server", "websocket"] }
```

### 5.2 `message_added` Has Empty Text for Assistant Messages

**This is the #1 pitfall for bidirectional forwarding.** When Alma creates an assistant
message, it fires `message_added` with an empty `parts` array (text = ""). The text is
populated later via `message_delta` events, and the complete text arrives in the final
`message_updated` event.

**Fix**: Use `message_updated` (not `message_added`) for bidirectional forwarding,
and filter by generation state to avoid forwarding partial updates.

### 5.3 `message_updated` Fires Multiple Times

A single assistant message generates multiple `message_updated` events:
- **During generation**: partial text (skip these)
- **After `thread_generating {isGenerating: false}`**: full final text (forward this)

**Fix**: Track generating threads in a `HashSet<String>`. Only forward `message_updated`
for assistant messages when the thread is NOT in the generating set.

### 5.4 libsql `Statement` Methods Take `&self`

The `libsql` v0.9 crate's `Statement::query()` and `Statement::execute()` take `&self`,
not `&mut self`. Declaring `let mut stmt = ...` triggers unused-mut warnings.

```rust
// Correct — no mut needed:
let stmt = conn.prepare("SELECT ...").await?;
let rows = stmt.query(params).await?;
```

### 5.5 `alma run` Has No REST Alternative

Do NOT try to send messages via `POST /api/threads/:id/messages` — this endpoint
does not exist. Use the Alma WebSocket protocol (`generate_response`) or the
`alma run` CLI.

### 5.6 WebSocket Split Pattern

In Warp, `ws.split()` returns `(SplitSink, SplitStream)`. You cannot use the
`SplitSink` from multiple tasks directly. The correct pattern is:

1. Split the WS into sink + stream
2. Create an `mpsc::unbounded_channel`
3. Spawn a dedicated writer task that reads from the channel and writes to the sink
4. Any task can send to the channel

### 5.7 OneBot Group Messages: @bot Required

In group chats, the bridge only responds when the bot is @mentioned. The @mention
segment is stripped from the text before passing to Alma, so the AI sees clean input.

### 5.8 Long Messages Need Splitting

QQ has a ~4500 character limit per message. The bridge splits long Alma replies
by paragraphs first (double newline), then by character limit within each paragraph.

### 5.9 Edition 2024 Compatibility

This project uses Rust edition 2024 (default for `cargo init` on Rust 1.85+).
All dependencies (warp 0.4.3, tokio 1.52, reqwest 0.13) are compatible.

### 5.10 OneBot Message Segment Format

Always use the **array format** for messages, not the CQ string format:

```json
// Correct (array format):
{"type": "text", "data": {"text": "hello"}}

// Wrong (CQ string format — not used by this bridge):
"[CQ:text,text=hello]"
```

### 5.11 WS Path Must Match Client Config

The bridge accepts connections at three paths:

- `/` — generic root path
- `/ws` — NapCat/snowluma default (`ws://host:port/ws`)
- `/onebot/v11/ws` — Lagrange default

If the OneBot client is configured for a path the bridge doesn't listen on,
the connection will be rejected.

### 5.12 Port 8080 Often Occupied

On systems running Docker containers with nginx-ui or similar reverse proxies,
port 8080 is commonly taken. The bridge defaults to **8090** to avoid this conflict.

### 5.13 Container-to-Host Networking (OrbStack)

When snowluma runs in an OrbStack container, use `host.docker.internal` as the
hostname to reach services on the Mac host. The OrbStack bridge network
(192.168.148.0/24) cannot directly reach other LAN devices.

### 5.14 Dedup Comparison Precision

The dedup mechanism compares the first 100 characters of text. This is sufficient
for most messages but could theoretically miss duplicates that diverge after 100 chars.
The sent reply buffer keeps the last 20 entries per thread.

### 5.15 Per-Thread Generation Guards

Concurrent `generate()` calls for the same thread would corrupt the pending map.
A `HashMap<String, Arc<Mutex<()>>>` serializes generations per thread. The guard
is acquired before sending `generate_response` and held until the response arrives.

### 5.16 OneBot Reply Segment Must Be First Element

When sending a reply via OneBot `send_msg` with array format, the `reply` segment
**must be the first element** in the message array. Placing it elsewhere causes
some OneBot implementations (NapCat, Lagrange) to ignore the reply reference.

```json
// Correct:
[{"type":"reply","data":{"id":"123"}}, {"type":"text","data":{"text":"reply text"}}]

// Wrong (reply not first):
[{"type":"text","data":{"text":"reply text"}}, {"type":"reply","data":{"id":"123"}}]
```

### 5.17 `get_msg` API Availability

The `get_msg` OneBot API is available in most v11 implementations (NapCat, Lagrange,
LLOneBot) but may return different response formats. The bridge extracts text from
the `message[]` segments array first, falling back to `raw_message` string.

### 5.18 Face Segments Are Not Text

OneBot `face` segments have `type: "face"` (not `type: "text"`), so `extract_text()`
ignores them entirely. A message containing only emoji faces would be treated as empty
without the separate `convert_faces_to_text()` call. The pipeline combines both before
the empty check.

### 5.19 Forward Segment Only Carries an Opaque ID

The `forward` segment in OneBot v11 contains only `data.id` — no actual message content.
You must call `get_forward_msg` with this ID to fetch the content. The response structure
is nested: an array of `node` segments, each containing a `nickname` and a `content` field
(which is itself a message segment array). Some OneBot implementations may not support
this API, so always handle the fallback gracefully.

### 5.20 Group History Must Be Recorded Before @bot Check

The group chat history ring buffer records ALL group messages, not just those that @mention
the bot. This means `display_name` computation and `record_group_message()` must happen
BEFORE the `contains_at_bot()` check. If you move the @bot check earlier, the history will
only contain bot-directed messages, making it useless as conversational context.

### 5.21 `get_forward_msg` Node Truncation

Forwarded messages can contain hundreds of nodes. The bridge limits extraction to the first
20 nodes, with each node's text truncated at 100 chars. This prevents the AI context from
being overwhelmed by a single massive forward. If the user needs full content, they should
paste it directly.

### 5.22 Warp Header Extraction Before WS Upgrade

To validate an `Authorization` header on WebSocket connections, you must extract it
BEFORE the WS upgrade. Use `warp::header::optional::<String>("authorization")` as a
filter composed with `warp::ws()`. The header is not accessible after the connection
has been upgraded to WebSocket.

---

## 6. Deployment

### Prerequisites

- Alma running locally (`alma status` to verify)
- OneBot client (snowluma/NapCat) configured for reverse WS
- Rust toolchain for building

### snowluma/NapCat Configuration (Reverse WS)

snowluma uses a JSON config file at `/app/snowluma-data/config/onebot_<qq_id>.json`.
Add a new entry to the `wsClients` array:

```json
{
  "networks": {
    "wsClients": [
      {
        "name": "Alma",
        "url": "ws://host.docker.internal:8090/ws",
        "messageFormat": "array",
        "reportSelfMessage": false,
        "role": "Universal",
        "reconnectIntervalMs": 5000
      }
    ]
  }
}
```

After editing the config, restart the container: `docker restart snowluma`.
Verify connection in logs: `docker logs snowluma --tail 20 | grep Alma`.

Note: If snowluma runs in a Docker/OrbStack container, use `host.docker.internal`
or the host's LAN IP to reach the bridge on the Mac host.

### Build & Run

```bash
cargo build --release
./target/release/alma-onebot-bridge

# Or with custom config:
RUST_LOG=debug BRIDGE_PORT=8080 ./target/release/alma-onebot-bridge

# Or using config.toml (recommended):
cp config.toml.example config.toml
# Edit config.toml, then:
cargo run --release
```

### Startup Order

1. Start Alma
2. Start the bridge service
3. Start the OneBot client (it will auto-reconnect to the bridge)

---

## 7. Future Enhancements

- **Outgoing image segments**: Alma AI may generate image URLs or markdown images in replies;
  convert these to OneBot `image` segments for native QQ rendering
- **Alma WS reconnection**: Currently the bridge does not auto-reconnect if the Alma WS drops
- **Streaming to QQ**: Forward partial replies as they arrive (QQ rate-limits messages, so this
  would need throttling)
- **Persistent group history**: Current in-memory ring buffer resets on restart; could persist
  to SQLite or write to `~/.config/alma/groups/` for cross-session context
- **Message edit/delete from Alma**: When Alma deletes or edits a message in the GUI, call
  OneBot `delete_msg` or edit equivalent (if supported by the OneBot implementation)

---

## 8. Alma Channel Bridge Protocol (Reverse-Engineered)

This section documents how Alma's built-in channel bridges (Telegram, Discord, Feishu, Weixin)
communicate with the Alma server, and how our QQ/OneBot bridge can integrate with this protocol.

### 8.1 Key Architecture Insight: Profile Matching is Bridge-Side

**The most important discovery**: SENDER PROFILE injection happens in each bridge, NOT on the
Alma server. Each built-in bridge:

1. Receives a platform message with sender ID
2. Scans `~/.config/alma/people/*.md` for a matching frontmatter field
3. If found and profile < 500 chars, injects into `ephemeralContext`:
   ```
   [SENDER PROFILE — alice]:
   ---
   telegram_id: "123456789"
   username: alice
   ---
   Alice is a designer based in Tokyo.
   [/SENDER PROFILE]
   ```
4. Sends the message to Alma WebSocket with `ephemeralContext` containing the profile

The Alma server only handles stripping stale `[SENDER PROFILE]` blocks from older messages
in the conversation history — it does NOT perform the profile matching itself.

### 8.2 Platform-Specific Profile Matching

| Bridge | Frontmatter Field | Fallback |
|--------|------------------|----------|
| Telegram (MessageBridge) | `telegram_id: "id"` | Filename matches display name |
| Discord (DiscordBridge) | `discord_id: "id"` or `discord_username: "name"` | Filename matches display name |
| Feishu (FeishuBridge) | `feishu_id: "id"` | Filename matches display name |
| QQ (this bridge) | `qq_id: "id"` | Filename matches display name |

Built-in bridges don't support `qq_id` — we implement our own matching in the bridge.

### 8.3 The `source` Field

The `generate_response` message includes a `source` field that tells Alma's server which
platform the message comes from. The server uses this to:

- Inject platform-specific system prompt rules (group chat rules, formatting, etc.)
- Strip stale ephemeral content from history (SENDER PROFILE, group chat history blocks)

| Source | Channel | Context |
|--------|---------|---------|
| `"telegram"` | Telegram | Private chat |
| `"telegram-group"` | Telegram | Group chat |
| `"discord"` | Discord | All messages |
| `"feishu"` | Feishu | All messages |
| `"lark"` | Lark | International Feishu |
| `"weixin"` | WeChat | All messages |

**QQ bridge uses `"telegram-group"` for groups and `"telegram"` for private chats.**
This gets us Telegram's group chat system prompt (privacy firewall, group observation rules,
frequency control) and proper history stripping.

### 8.4 The `ephemeralContext` Field

A per-turn system prompt string sent with each `generate_response`. Contains:
- SENDER PROFILE block (injected by bridge)
- PEOPLE PROFILES summary line
- Recent group chat history (for group chats — our bridge injects the last N messages
  from the in-memory ring buffer with timestamps)
- Channel-specific rules (for some platforms)

The Alma server merges this with its own system prompt (SOUL.md, Memory, tools).
For `"telegram-group"` source, the server strips stale `RECENT GROUP CHAT HISTORY` blocks
from older messages in the conversation history, keeping only the last message's context.

### 8.5 The `channel_mappings` Database Table

Maps platform conversations to Alma threads:

```sql
CREATE TABLE channel_mappings (
    id TEXT PRIMARY KEY,
    platform TEXT NOT NULL,          -- "telegram"|"discord"|"feishu"|"lark"|"weixin"
    external_chat_id TEXT NOT NULL,
    external_user_id TEXT NOT NULL,
    thread_id TEXT NOT NULL REFERENCES chat_threads(id) ON DELETE CASCADE,
    is_active INTEGER DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
```

Lookup key: `(platform, external_chat_id, external_user_id)`.

For QQ groups: `platform="telegram"`, `external_chat_id=qq_group_id`, `external_user_id="group"`.
For QQ private: `platform="telegram"`, `external_chat_id=qq_user_id`, `external_user_id=qq_user_id`.

### 8.6 Telegram-Style Message Format

Built-in bridges prefix messages with sender identification:

```
[From: DisplayName (@username) | id:123456789]

[msg:42] actual message text
```

`[msg:N]` uses the actual platform `message_id` (not a counter). When replying:

```
[From: DisplayName (@username) | id:123456789]

[msg:43] [Replying to Someone's message: "quoted text up to 200 chars"]
actual reply text
```

Our QQ bridge adopts this format, with additional support for media, forwards, and face emojis:

```
[From: Alice | id:12345678]

[msg:12345] 你好世界 [emoji:斜眼笑]
```

With reply context:

```
[From: Alice | id:12345678]

[msg:12346] [Replying to Bob's message: "之前说的话"]
这是回复
```

With forwarded content:

```
[From: Alice | id:12345678]

[msg:12347] [Forwarded messages (3 total):Alice: "hello", Bob: "world", Charlie: "hi"]
```

With media:

```
[From: Alice | id:12345678]

[msg:12348] 看看这个
[Image: https://gchat.qpic.cn/...]
[Voice message]
```

### 8.7 Outgoing Reply Behavior (Bridge → Platform)

Built-in bridges always reply to the triggering user message:

| Bridge | How | Field |
|--------|-----|-------|
| Telegram | `reply_parameters: {message_id: userMessageId}` | In `sendMessage` API call |
| Discord | `messageReference: {messageId: originalId}` | In `createMessage` API call |
| QQ/OneBot | `reply` segment + `at` segment prepended to message array | First segments in `send_msg` params |

The QQ bridge additionally includes an `at` segment in group replies to @mention the
triggering user, ensuring they receive a notification. Private chats omit the `at` segment.

Only the first chunk of the first paragraph includes the reply reference (and @mention).
Subsequent chunks are sent as plain messages.

### 8.8 `detectPlatformForChatId` Routing

When Alma sends a reply back to a platform, it looks up the `channel_mappings` table:
- If `platform = "discord"` → Discord routing
- If `platform = "weixin"` → WeChat routing
- **Everything else → Telegram routing (default fallback)**

This is why `platform = "telegram"` works for our QQ bridge — Alma treats unrecognized
platforms as Telegram by default.

### 8.9 What We Get by Spoofing "telegram-group"

**Free from Alma server**:
- Telegram group chat system prompt (group rules, privacy, people observation)
- History stripping of stale `[SENDER PROFILE]` and `RECENT GROUP CHAT HISTORY` blocks
- People Profiles CLI integration (`alma people show/list/append`)
- Thread management via `channel_mappings`

**Must implement ourselves**:
- SENDER PROFILE scanning and injection (server doesn't do matching)
- `ephemeralContext` construction with profile + people summary + group chat history
- Telegram-style `[From: ... | id:...]` message format with media/forward/reply context
- Response delivery back to QQ (with reply segment + @mention for groups)

- **Image/voice message forwarding**: Convert Alma image outputs to OneBot image segments
