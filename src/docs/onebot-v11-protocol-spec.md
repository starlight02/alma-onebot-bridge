# OneBot v11 Protocol Specification: Reverse WebSocket Bridge Reference

Compiled from the official OneBot 12 standard (backwards-compatible v11 spec),
go-cqhttp documentation, and NapCat implementation details.

---

## 1. Reverse WebSocket Connection Flow

### Concept
In **reverse** WebSocket mode, the OneBot implementation (NapCat, Lagrange,
go-cqhttp, etc.) acts as the **WebSocket client** and connects TO your bridge
server. Your bridge runs a WebSocket server listening on a configured address.

### Connection Sequence

```
OneBot Client (NapCat)                    Bridge (WS Server)
       |                                        |
       |  ---- WebSocket Upgrade Request ----->  |
       |                                        |
       |  <--- 101 Switching Protocols --------  |
       |                                        |
       |  ---- lifecycle connect event -------->  |  (first event after connect)
       |                                        |
       |  <--- heartbeat events (periodic) ----  |
       |                                        |
       |  ---- message/notice/request events ->  |
       |                                        |
       |  <--- API action requests -------------  |  (bridge sends commands)
       |  ---- API action responses ----------->  |  (OneBot replies)
```

### Required HTTP Headers on Connect

| Header | Value | Notes |
|---|---|---|
| `User-Agent` | Implementation-defined | e.g. `CQHttp/6.0.0` |
| `Sec-WebSocket-Protocol` | `11.NapCat` or `11.go-cqhttp` | Format: `{version}.{impl_name}` |
| `Authorization` | `Bearer {access_token}` | Only if token is configured |

### Authentication via Access Token

If an `access_token` is configured on the OneBot client side:

1. **Primary method**: The client sends `Authorization: Bearer <token>` header.
   - Do NOT trim whitespace from the token value.
2. **Fallback method**: If headers cannot be set, the token is appended as a
   URL query parameter: `ws://host:port/?access_token=<token>`
3. **Server-side validation**:
   - Return HTTP `401` if the token is missing
   - Return HTTP `403` if the token is invalid

### Reconnection Behavior

- If the connection drops, the OneBot client **must** continuously retry.
- Delay between retries is configurable via `reconnect_interval` (milliseconds).
- The interval must be strictly greater than zero.

### Configuration on the OneBot Client Side (example NapCat JSON config)

```json
{
  "reverseWs": {
    "enable": true,
    "urls": [
      "ws://127.0.0.1:8080/onebot"
    ],
    "access_token": "your_shared_secret",
    "reconnect_interval": 3000
  }
}
```

---

## 2. Event Format (OneBot -> Bridge)

Every event is a single JSON object sent as one WebSocket message.

### Common Fields (present on ALL events)

| Field | Type | Description |
|---|---|---|
| `time` | number (int64) | Unix timestamp (seconds) |
| `self_id` | number (int64) | QQ number of the bot account |
| `post_type` | string | `"message"` \| `"notice"` \| `"request"` \| `"meta_event"` |

---

### 2.1 Meta Events (`post_type: "meta_event"`)

#### Lifecycle (sent immediately on WebSocket connect)

```json
{
  "time": 1610000000,
  "self_id": 1000000,
  "post_type": "meta_event",
  "meta_event_type": "lifecycle",
  "sub_type": "connect"
}
```

- `sub_type` values: `"connect"`, `"enable"`, `"disable"`
- The `connect` lifecycle event **must** be the first event sent on a new
  WebSocket connection.

#### Heartbeat (sent periodically)

```json
{
  "time": 1610000000,
  "self_id": 1000000,
  "post_type": "meta_event",
  "meta_event_type": "heartbeat",
  "status": {
    "app_initialized": true,
    "app_enabled": true,
    "app_good": true,
    "online": true,
    "good": true
  },
  "interval": 5000
}
```

- `interval`: milliseconds until the next heartbeat
- `status`: object reflecting the internal state of the OneBot implementation

---

### 2.2 Message Events (`post_type: "message"`)

#### Private Message

```json
{
  "time": 1610000000,
  "self_id": 1000000,
  "post_type": "message",
  "message_type": "private",
  "sub_type": "friend",
  "message_id": 12345,
  "user_id": 2000000,
  "message": [
    {
      "type": "text",
      "data": {
        "text": "Hello, world!"
      }
    }
  ],
  "raw_message": "Hello, world!",
  "font": 0,
  "sender": {
    "user_id": 2000000,
    "nickname": "Someone",
    "sex": "unknown",
    "age": 0
  }
}
```

| Field | Type | Description |
|---|---|---|
| `message_type` | string | Always `"private"` |
| `sub_type` | string | `"friend"`, `"group"`, `"other"` |
| `message_id` | int32 | Unique message ID |
| `user_id` | int64 | Sender's QQ number |
| `message` | array | Message segment array (see section 5) |
| `raw_message` | string | Plain text representation |
| `font` | int32 | Font (unused, always 0) |
| `sender` | object | Sender info object |
| `sender.sex` | string | `"male"`, `"female"`, `"unknown"` |
| `temp_source` | int32 | Temporary session source (optional) |
| `target_id` | int64 | Target QQ (optional) |

#### Group Message

```json
{
  "time": 1610000000,
  "self_id": 1000000,
  "post_type": "message",
  "message_type": "group",
  "sub_type": "normal",
  "message_id": 67890,
  "group_id": 3000000,
  "user_id": 2000000,
  "anonymous": null,
  "message": [
    {
      "type": "at",
      "data": {
        "qq": "1000000"
      }
    },
    {
      "type": "text",
      "data": {
        "text": " Hey there!"
      }
    }
  ],
  "raw_message": "[CQ:at,qq=1000000] Hey there!",
  "font": 0,
  "sender": {
    "user_id": 2000000,
    "nickname": "Someone",
    "card": "Group Nickname",
    "sex": "unknown",
    "age": 0,
    "area": "",
    "level": "1",
    "role": "member",
    "title": ""
  }
}
```

| Field | Type | Description |
|---|---|---|
| `message_type` | string | Always `"group"` |
| `sub_type` | string | `"normal"`, `"anonymous"`, `"notice"` |
| `group_id` | int64 | Group number |
| `sender.card` | string | Group card (nickname in group) |
| `sender.role` | string | `"owner"`, `"admin"`, `"member"` |
| `sender.level` | string | Member level |
| `sender.title` | string | Special title |
| `anonymous` | object\|null | Anonymous info if applicable |

Anonymous object format (when not null):
```json
{
  "id": 12345,
  "name": "Anonymous Name",
  "flag": "opaque_flag_string"
}
```

---

### 2.3 Notice Events (`post_type: "notice"`)

#### Friend Recall (撤回)

```json
{
  "time": 1610000000,
  "self_id": 1000000,
  "post_type": "notice",
  "notice_type": "friend_recall",
  "user_id": 2000000,
  "message_id": 12345
}
```

#### Group Recall

```json
{
  "time": 1610000000,
  "self_id": 1000000,
  "post_type": "notice",
  "notice_type": "group_recall",
  "group_id": 3000000,
  "user_id": 2000000,
  "operator_id": 4000000,
  "message_id": 12345
}
```

#### Group Member Increase

```json
{
  "time": 1610000000,
  "self_id": 1000000,
  "post_type": "notice",
  "notice_type": "group_increase",
  "sub_type": "approve",
  "group_id": 3000000,
  "operator_id": 4000000,
  "user_id": 2000000
}
```

- `sub_type`: `"approve"` (admin approved) or `"invite"` (admin invited)

#### Group Member Decrease

```json
{
  "time": 1610000000,
  "self_id": 1000000,
  "post_type": "notice",
  "notice_type": "group_decrease",
  "sub_type": "leave",
  "group_id": 3000000,
  "operator_id": 4000000,
  "user_id": 2000000
}
```

- `sub_type`: `"leave"` (voluntary), `"kick"` (kicked), `"kick_me"` (bot was kicked)

#### Group Ban

```json
{
  "time": 1610000000,
  "self_id": 1000000,
  "post_type": "notice",
  "notice_type": "group_ban",
  "sub_type": "ban",
  "group_id": 3000000,
  "operator_id": 4000000,
  "user_id": 2000000,
  "duration": 1800
}
```

- `sub_type`: `"ban"` or `"lift_ban"`
- `duration`: seconds of ban

#### Group Admin Change

```json
{
  "time": 1610000000,
  "self_id": 1000000,
  "post_type": "notice",
  "notice_type": "group_admin",
  "sub_type": "set",
  "group_id": 3000000,
  "user_id": 2000000
}
```

- `sub_type`: `"set"` (promoted) or `"unset"` (demoted)

#### Group File Upload

```json
{
  "time": 1610000000,
  "self_id": 1000000,
  "post_type": "notice",
  "notice_type": "group_upload",
  "group_id": 3000000,
  "user_id": 2000000,
  "file": {
    "id": "file_id_string",
    "name": "document.pdf",
    "size": 1048576,
    "busid": 1
  }
}
```

#### Friend Added

```json
{
  "time": 1610000000,
  "self_id": 1000000,
  "post_type": "notice",
  "notice_type": "friend_add",
  "user_id": 2000000
}
```

#### Poke (戳一戳)

Private poke:
```json
{
  "time": 1610000000,
  "self_id": 1000000,
  "post_type": "notice",
  "notice_type": "notify",
  "sub_type": "poke",
  "sender_id": 2000000,
  "user_id": 2000000,
  "target_id": 1000000
}
```

Group poke (adds `group_id`):
```json
{
  "time": 1610000000,
  "self_id": 1000000,
  "post_type": "notice",
  "notice_type": "notify",
  "sub_type": "poke",
  "group_id": 3000000,
  "user_id": 2000000,
  "target_id": 5000000
}
```

#### Group Lucky King (红包运气王)

```json
{
  "time": 1610000000,
  "self_id": 1000000,
  "post_type": "notice",
  "notice_type": "notify",
  "sub_type": "lucky_king",
  "group_id": 3000000,
  "user_id": 2000000,
  "target_id": 5000000
}
```

- `user_id`: red packet sender, `target_id`: lucky king

#### Group Honor Change

```json
{
  "time": 1610000000,
  "self_id": 1000000,
  "post_type": "notice",
  "notice_type": "notify",
  "sub_type": "honor",
  "group_id": 3000000,
  "user_id": 2000000,
  "honor_type": "talkative"
}
```

- `honor_type`: `"talkative"`, `"performer"`, `"legend"`, `"strong_newbie"`, `"emotion"`

#### Group Member Card Change

```json
{
  "time": 1610000000,
  "self_id": 1000000,
  "post_type": "notice",
  "notice_type": "group_card",
  "group_id": 3000000,
  "user_id": 2000000,
  "card_new": "New Nickname",
  "card_old": "Old Nickname"
}
```

#### Offline File

```json
{
  "time": 1610000000,
  "self_id": 1000000,
  "post_type": "notice",
  "notice_type": "offline_file",
  "user_id": 2000000,
  "file": {
    "name": "file.txt",
    "size": 1024,
    "url": "https://example.com/file"
  }
}
```

#### Essence Message

```json
{
  "time": 1610000000,
  "self_id": 1000000,
  "post_type": "notice",
  "notice_type": "essence",
  "sub_type": "add",
  "group_id": 3000000,
  "sender_id": 2000000,
  "operator_id": 4000000,
  "message_id": 12345
}
```

- `sub_type`: `"add"` or `"delete"`

---

### 2.4 Request Events (`post_type: "request"`)

#### Friend Request

```json
{
  "time": 1610000000,
  "self_id": 1000000,
  "post_type": "request",
  "request_type": "friend",
  "user_id": 2000000,
  "comment": "Please add me",
  "flag": "opaque_flag_for_approval"
}
```

#### Group Request / Invite

```json
{
  "time": 1610000000,
  "self_id": 1000000,
  "post_type": "request",
  "request_type": "group",
  "sub_type": "add",
  "group_id": 3000000,
  "user_id": 2000000,
  "comment": "I want to join",
  "flag": "opaque_flag_for_approval"
}
```

- `sub_type`: `"add"` (someone wants to join) or `"invite"` (bot is invited)
- `flag`: must be passed back to `set_group_add_request` / `set_friend_add_request`

---

## 3. API Call Format (Bridge -> OneBot)

### Request Structure

The bridge sends API calls as JSON objects over the WebSocket connection:

```json
{
  "action": "api_name",
  "params": {
    "param_name": "param_value"
  },
  "echo": "unique_request_id"
}
```

| Field | Type | Required | Description |
|---|---|---|---|
| `action` | string | Yes | The API endpoint name |
| `params` | object | Yes | Parameters (can be empty `{}`) |
| `echo` | any | No | Unique identifier for correlation |

### Response Structure

The OneBot client sends back a response:

```json
{
  "status": "ok",
  "retcode": 0,
  "data": {
    "result_key": "result_value"
  },
  "echo": "unique_request_id"
}
```

| Field | Type | Description |
|---|---|---|
| `status` | string | `"ok"` (success), `"failed"` (error), or `"async"` (queued) |
| `retcode` | number | `0` = success, `1` = async queued, other = error |
| `data` | object\|null | Response payload (null on failure) |
| `message` | string\|null | Error description (only on failure) |
| `wording` | string\|null | Additional error info (only on failure, go-cqhttp extension) |
| `echo` | any | Echoed back from the request |

---

## 4. Echo Mechanism

The `echo` field enables correlating asynchronous responses to their requests
on the shared WebSocket connection.

### Flow

```
Bridge sends:     {"action":"send_private_msg","params":{...},"echo":"req-001"}
Bridge sends:     {"action":"get_login_info","params":{},"echo":"req-002"}

OneBot replies:   {"status":"ok","retcode":0,"data":{...},"echo":"req-002"}
OneBot replies:   {"status":"ok","retcode":0,"data":{...},"echo":"req-001"}
```

### Implementation Strategy

1. Generate a unique `echo` value for each request (UUID, incrementing counter, etc.)
2. Store the request's promise/callback in a map keyed by the `echo` value
3. On receiving a WebSocket message, check if it contains an `echo` field
4. Look up and resolve the corresponding promise/callback
5. If no `echo` field is present, the message is an **event** (not a response)

### Distinguishing Events from Responses

```
Has "echo" field?     -> It's an API response
Has "post_type" field? -> It's an event
```

### TypeScript Pseudocode

```typescript
const pending = new Map<string, {resolve, reject}>();
let echoCounter = 0;

async function callApi(action: string, params: object): Promise<any> {
  const echo = `req-${++echoCounter}`;
  return new Promise((resolve, reject) => {
    pending.set(echo, {resolve, reject});
    ws.send(JSON.stringify({action, params, echo}));
  });
}

ws.on('message', (raw) => {
  const msg = JSON.parse(raw);

  if (msg.echo !== undefined) {
    // API response
    const handler = pending.get(msg.echo);
    if (handler) {
      pending.delete(msg.echo);
      if (msg.status === 'ok') {
        handler.resolve(msg.data);
      } else {
        handler.reject(new Error(msg.message || 'API call failed'));
      }
    }
  } else if (msg.post_type) {
    // Event
    handleEvent(msg);
  }
});
```

---

## 5. Message Segment Format

Messages use an **array of segment objects**. Each segment has a `type` string
and a `data` object.

### General Structure

```json
[
  {"type": "segment_type", "data": {"field": "value"}},
  {"type": "another_type", "data": {"field": "value"}}
]
```

### Complete Segment Type Reference

#### `text` - Plain Text

```json
{"type": "text", "data": {"text": "Hello, world!"}}
```

| Field | Type | Description |
|---|---|---|
| `text` | string | The text content |

#### `image` - Image

```json
{"type": "image", "data": {"file": "https://example.com/img.png"}}
```

Sending:
| Field | Type | Required | Description |
|---|---|---|---|
| `file` | string | Yes | URL, base64 (`base64://...`), or local file path |
| `type` | string | No | `"flash"` for flash image, `"show"` for show image |
| `cache` | string | No | `"0"` to disable cache, default `"1"` |

Receiving (additional fields):
| Field | Type | Description |
|---|---|---|
| `url` | string | The image download URL |
| `file` | string | The file name |

#### `at` - Mention

```json
{"type": "at", "data": {"qq": "2000000"}}
{"type": "at", "data": {"qq": "all"}}
```

| Field | Type | Description |
|---|---|---|
| `qq` | string | QQ number to mention, or `"all"` for @everyone |

#### `face` - QQ Emoji (表情)

```json
{"type": "face", "data": {"id": "178"}}
```

| Field | Type | Description |
|---|---|---|
| `id` | string | The QQ face emoji ID |

#### `reply` - Quote Reply

```json
{"type": "reply", "data": {"id": "12345"}}
```

| Field | Type | Required | Description |
|---|---|---|---|
| `id` | string | Yes | The message_id to reply to |

Custom reply (go-cqhttp extension):
| Field | Type | Description |
|---|---|---|
| `text` | string | Custom reply text content |
| `qq` | string | Custom sender QQ |
| `time` | string | Custom timestamp |
| `seq` | string | Custom sequence number |

#### `record` - Voice/Audio

```json
{"type": "record", "data": {"file": "https://example.com/audio.amr"}}
```

| Field | Type | Description |
|---|---|---|
| `file` | string | URL, base64, or local path |
| `magic` | string | `"1"` for magic voice (变声), `"0"` normal |
| `cache` | string | `"0"` to disable cache |
| `url` | string | (receiving) Audio file URL |

#### `video` - Video

```json
{"type": "video", "data": {"file": "https://example.com/video.mp4"}}
```

| Field | Type | Description |
|---|---|---|
| `file` | string | URL, base64, or local path |
| `cover` | string | Video cover image URL |
| `url` | string | (receiving) Video URL |

#### `share` - Link Share

```json
{"type": "share", "data": {
  "url": "https://example.com",
  "title": "Example Title",
  "content": "Description text",
  "image": "https://example.com/thumb.jpg"
}}
```

| Field | Type | Required | Description |
|---|---|---|---|
| `url` | string | Yes | Share URL |
| `title` | string | Yes | Share title |
| `content` | string | No | Share description |
| `image` | string | No | Share thumbnail URL |

#### `location` - Location

```json
{"type": "location", "data": {
  "lat": "39.9042",
  "lon": "116.4074",
  "title": "Beijing",
  "content": "Capital of China"
}}
```

| Field | Type | Required | Description |
|---|---|---|---|
| `lat` | string | Yes | Latitude |
| `lon` | string | Yes | Longitude |
| `title` | string | No | Location title |
| `content` | string | No | Location description |

#### `music` - Music Share

```json
{"type": "music", "data": {"type": "163", "id": "123456"}}
```

| Field | Type | Description |
|---|---|---|
| `type` | string | Platform: `"qq"`, `"163"`, `"xm"` (Xiami) |
| `id` | string | Music ID on the platform |

Custom music:
```json
{"type": "music", "data": {
  "type": "custom",
  "url": "https://example.com/song",
  "audio": "https://example.com/audio.mp3",
  "title": "Song Title",
  "content": "Artist name",
  "image": "https://example.com/cover.jpg"
}}
```

#### `contact` - Recommend Contact

```json
{"type": "contact", "data": {"type": "qq", "id": "2000000"}}
{"type": "contact", "data": {"type": "group", "id": "3000000"}}
```

| Field | Type | Description |
|---|---|---|
| `type` | string | `"qq"` or `"group"` |
| `id` | string | QQ number or group number |

#### `forward` - Merged Forward Message (receive only)

```json
{"type": "forward", "data": {"id": "forward_message_id"}}
```

#### `node` - Forwarded Message Node (send only, for forward messages)

Reference existing message:
```json
{"type": "node", "data": {"id": "12345"}}
```

Custom content:
```json
{"type": "node", "data": {
  "name": "Sender Name",
  "uin": "2000000",
  "content": [
    {"type": "text", "data": {"text": "Forwarded content"}}
  ]
}}
```

| Field | Type | Description |
|---|---|---|
| `id` | string | Existing message_id (reference mode) |
| `name` | string | Custom sender name (custom mode) |
| `uin` | string | Custom sender QQ number (custom mode) |
| `content` | array | Message segment array (custom mode) |

#### `xml` - XML Message

```json
{"type": "xml", "data": {"data": "<?xml ..."}}
```

#### `json` - JSON Message

```json
{"type": "json", "data": {"data": "{\"app\":\"com.tencent.miniapp\"...}"}}
```

#### `poke` - Poke (send only, group)

```json
{"type": "poke", "data": {"qq": "2000000"}}
```

#### `dice` - Dice (send only)

```json
{"type": "dice", "data": {}}
```

#### `rps` - Rock Paper Scissors (send only)

```json
{"type": "rps", "data": {}}
```

#### `shake` - Window Shake (send only, private)

```json
{"type": "shake", "data": {}}
```

#### `redbag` - Red Packet (receive only)

```json
{"type": "redbag", "data": {"title": "恭喜发财"}}
```

### Compound Message Example

A message with @mention, text, and image:

```json
[
  {"type": "at", "data": {"qq": "2000000"}},
  {"type": "text", "data": {"text": " Check this out: "}},
  {"type": "image", "data": {"file": "https://example.com/photo.png"}},
  {"type": "face", "data": {"id": "178"}}
]
```

### Shorthand (String Format)

Some implementations also accept a plain string for `message`, which is
treated as a single text segment:

```json
{
  "action": "send_private_msg",
  "params": {
    "user_id": 2000000,
    "message": "Hello, world!"
  }
}
```

This is equivalent to:
```json
{
  "action": "send_private_msg",
  "params": {
    "user_id": 2000000,
    "message": [
      {"type": "text", "data": {"text": "Hello, world!"}}
    ]
  }
}
```

---

## 6. Authentication: Access Token Mechanism

### Overview

OneBot v11 uses a simple shared-secret token for authentication across all
communication methods (HTTP, forward WS, reverse WS).

### Configuration

The token is configured on the OneBot client (NapCat/Lagrange) side:
```json
{
  "access_token": "your_shared_secret_here"
}
```

The bridge server must validate this same token.

### Token Transmission

| Transport | Method |
|---|---|
| **HTTP** | `Authorization: Bearer <token>` header on every request |
| **Forward WebSocket** | `Authorization: Bearer <token>` on the upgrade request |
| **Reverse WebSocket** | `Authorization: Bearer <token>` on the connect request |
| **Fallback** | `?access_token=<token>` query parameter on the URL |

### Server-Side Validation (Bridge Implementation)

```
if no token configured:
    accept connection
else:
    check Authorization header for "Bearer <token>"
    if header missing:
        return HTTP 401 Unauthorized
    if token mismatch:
        return HTTP 403 Forbidden
    accept connection
```

### Important Notes

- The token value must NOT be trimmed of whitespace before comparison
- If the `Authorization` header cannot be set (some client limitations),
  the token may be passed as a URL query parameter instead
- The same token is used for both sending events to the bridge and for
  the bridge to make API calls back

---

## 7. Key API Endpoints with Full Examples

### send_private_msg - Send Private Message

**Request:**
```json
{
  "action": "send_private_msg",
  "params": {
    "user_id": 2000000,
    "message": [
      {"type": "text", "data": {"text": "Hello!"}}
    ]
  },
  "echo": "1"
}
```

| Param | Type | Required | Description |
|---|---|---|---|
| `user_id` | int64 | Yes | Target QQ number |
| `message` | array\|string | Yes | Message content |
| `auto_escape` | boolean | No | If true, treat message as CQ code string |

**Response:**
```json
{
  "status": "ok",
  "retcode": 0,
  "data": {
    "message_id": 12345
  },
  "echo": "1"
}
```

### send_group_msg - Send Group Message

**Request:**
```json
{
  "action": "send_group_msg",
  "params": {
    "group_id": 3000000,
    "message": [
      {"type": "at", "data": {"qq": "2000000"}},
      {"type": "text", "data": {"text": " Welcome to the group!"}}
    ]
  },
  "echo": "2"
}
```

| Param | Type | Required | Description |
|---|---|---|---|
| `group_id` | int64 | Yes | Target group number |
| `message` | array\|string | Yes | Message content |
| `auto_escape` | boolean | No | If true, treat message as CQ code string |

**Response:**
```json
{
  "status": "ok",
  "retcode": 0,
  "data": {
    "message_id": 67890
  },
  "echo": "2"
}
```

### send_msg - Send Message (Auto-detect type)

**Request:**
```json
{
  "action": "send_msg",
  "params": {
    "message_type": "private",
    "user_id": 2000000,
    "message": "Hello via send_msg"
  },
  "echo": "3"
}
```

| Param | Type | Required | Description |
|---|---|---|---|
| `message_type` | string | Conditional | `"private"` or `"group"` |
| `user_id` | int64 | Conditional | Required when `message_type` is `"private"` |
| `group_id` | int64 | Conditional | Required when `message_type` is `"group"` |
| `message` | array\|string | Yes | Message content |
| `auto_escape` | boolean | No | Auto-escape CQ codes |

If `message_type` is omitted, the API auto-detects based on which of
`user_id`/`group_id` is provided.

**Response:**
```json
{
  "status": "ok",
  "retcode": 0,
  "data": {
    "message_id": 12346
  },
  "echo": "3"
}
```

### get_login_info - Get Bot Login Info

**Request:**
```json
{
  "action": "get_login_info",
  "params": {},
  "echo": "4"
}
```

**Response:**
```json
{
  "status": "ok",
  "retcode": 0,
  "data": {
    "user_id": 1000000,
    "nickname": "BotName"
  },
  "echo": "4"
}
```

| Response Field | Type | Description |
|---|---|---|
| `data.user_id` | int64 | Bot's QQ number |
| `data.nickname` | string | Bot's nickname |

### delete_msg - Recall Message

**Request:**
```json
{
  "action": "delete_msg",
  "params": {"message_id": 12345},
  "echo": "5"
}
```

### get_msg - Get Message by ID

**Request:**
```json
{
  "action": "get_msg",
  "params": {"message_id": 12345},
  "echo": "6"
}
```

**Response:**
```json
{
  "status": "ok",
  "retcode": 0,
  "data": {
    "time": 1610000000,
    "message_type": "private",
    "message_id": 12345,
    "real_id": 12345,
    "sender": {
      "user_id": 2000000,
      "nickname": "Someone",
      "sex": "unknown",
      "age": 0
    },
    "message": [
      {"type": "text", "data": {"text": "Hello!"}}
    ]
  },
  "echo": "6"
}
```

### get_group_list - Get Group List

**Request:**
```json
{
  "action": "get_group_list",
  "params": {},
  "echo": "7"
}
```

**Response:**
```json
{
  "status": "ok",
  "retcode": 0,
  "data": [
    {
      "group_id": 3000000,
      "group_name": "My Group",
      "member_count": 100,
      "max_member_count": 500
    }
  ],
  "echo": "7"
}
```

### get_friend_list - Get Friend List

**Request:**
```json
{
  "action": "get_friend_list",
  "params": {},
  "echo": "8"
}
```

**Response:**
```json
{
  "status": "ok",
  "retcode": 0,
  "data": [
    {
      "user_id": 2000000,
      "nickname": "FriendName",
      "remark": "Remark"
    }
  ],
  "echo": "8"
}
```

### set_group_kick - Kick Group Member

**Request:**
```json
{
  "action": "set_group_kick",
  "params": {
    "group_id": 3000000,
    "user_id": 2000000,
    "reject_add_request": false
  },
  "echo": "9"
}
```

### set_friend_add_request - Handle Friend Request

**Request:**
```json
{
  "action": "set_friend_add_request",
  "params": {
    "flag": "opaque_flag_from_request_event",
    "approve": true
  },
  "echo": "10"
}
```

### set_group_add_request - Handle Group Request

**Request:**
```json
{
  "action": "set_group_add_request",
  "params": {
    "flag": "opaque_flag_from_request_event",
    "sub_type": "add",
    "approve": true
  },
  "echo": "11"
}
```

### get_version_info - Get Version Info

**Response:**
```json
{
  "status": "ok",
  "retcode": 0,
  "data": {
    "app_name": "go-cqhttp",
    "app_version": "v1.0.0",
    "protocol_version": "v11",
    "nt_protocol": "Linux"
  },
  "echo": "12"
}
```

### send_group_forward_msg - Send Merged Forward Message (Group)

**Request:**
```json
{
  "action": "send_group_forward_msg",
  "params": {
    "group_id": 3000000,
    "messages": [
      {
        "type": "node",
        "data": {
          "name": "User A",
          "uin": "1000001",
          "content": [{"type": "text", "data": {"text": "Message 1"}}]
        }
      },
      {
        "type": "node",
        "data": {
          "name": "User B",
          "uin": "1000002",
          "content": [{"type": "text", "data": {"text": "Message 2"}}]
        }
      }
    ]
  },
  "echo": "13"
}
```

### mark_msg_as_read - Mark as Read

```json
{
  "action": "mark_msg_as_read",
  "params": {"message_id": 12345},
  "echo": "14"
}
```

### get_group_member_info - Get Group Member Info

**Request:**
```json
{
  "action": "get_group_member_info",
  "params": {
    "group_id": 3000000,
    "user_id": 2000000,
    "no_cache": false
  },
  "echo": "15"
}
```

**Response:**
```json
{
  "status": "ok",
  "retcode": 0,
  "data": {
    "group_id": 3000000,
    "user_id": 2000000,
    "nickname": "QQ Name",
    "card": "Group Card",
    "sex": "unknown",
    "age": 0,
    "area": "",
    "join_time": 1600000000,
    "last_sent_time": 1610000000,
    "level": "1",
    "role": "member",
    "unfriendly": false,
    "title": "",
    "title_expire_time": 0,
    "card_changeable": false
  },
  "echo": "15"
}
```

---

## 8. retcode Reference

| retcode | Meaning |
|---|---|
| `0` | Success |
| `1` | Async (operation queued, not yet completed) |
| `100` | Invalid parameter |
| `102` | Operation failed (generic) |
| `103` | Operation failed (timeout) |
| `104` | Operation failed (unknown error) |
| `200` | Bad token (for HTTP) |
| `201` | Bad token format |
| `202` | Bad content type |
| `203` | Not found (unknown action) |

Note: exact retcode values may vary between implementations (go-cqhttp, NapCat,
Lagrange). Always check `status` field (`"ok"` vs `"failed"`) as the primary
indicator.

---

## Sources

- [OneBot 12 Standard - Reverse WebSocket](https://12.onebot.dev/connect/communication/websocket-reverse/)
- [go-cqhttp API Documentation](https://docs.go-cqhttp.org/api/)
- [go-cqhttp Event Documentation](https://docs.go-cqhttp.org/event/)
- [go-cqhttp GitHub Repository](https://github.com/Mrs4s/go-cqhttp)
- [go-cqhttp CQ HTTP Documentation](https://github.com/Mrs4s/go-cqhttp/blob/master/docs/cqhttp.md)
- [OneBot 12 Meta Events](https://12.onebot.dev/interface/meta/events/)
- [go-cqhttp Go Package (pkg/onebot)](https://pkg.go.dev/github.com/Mrs4s/go-cqhttp@v1.0.0-rc5/pkg/onebot)
- [NapCatQQ GitHub](https://github.com/NapNeko/NapCatQQ)
- [Nekonekobot Dart Models](https://github.com/Parallel-SEKAI/nekonekobot_dart/blob/main/doc/models.md)
