# Common Pitfalls Reference

Detailed explanations and fixes for the most time-consuming issues encountered during development of the Alma OneBot bridge.

## Table of Contents

1. [Warp 0.4.x server Feature](#1-warp-04x-server-feature)
2. [message_added Empty Assistant Text](#2-message_added-empty-assistant-text)
3. [message_updated Fires Multiple Times](#3-message_updated-fires-multiple-times)
4. [Turso Statement Takes mutable self Reference](#4-turso-statement-takes-mutable-self-reference)
5. [No REST Endpoint for Message Sending](#5-no-rest-endpoint-for-message-sending)
6. [WebSocket Split Pattern](#6-websocket-split-pattern)
7. [Per-Thread Generation Guards](#7-per-thread-generation-guards)
8. [QQ Message Length Limit](#8-qq-message-length-limit)
9. [Edition 2024 Compatibility](#9-edition-2024-compatibility)
10. [OneBot Message Segment Format](#10-onebot-message-segment-format)
11. [WS Path Must Match Client Config](#11-ws-path-must-match-client-config)
12. [Port 8080 Often Occupied](#12-port-8080-often-occupied)
13. [Container-to-Host Networking](#13-container-to-host-networking)
14. [Dedup Comparison Precision](#14-dedup-comparison-precision)
15. [Alma WS Reconnect](#15-alma-ws-reconnect)
16. [Do Not Write Alma Database From the External Bridge](#16-do-not-write-alma-database-from-the-external-bridge)

## 1. Warp 0.4.x server Feature

The `warp::serve()` function requires the `server` feature flag. In warp 0.4.x this is NOT enabled by default, unlike 0.3.x where it was.

Fix in `Cargo.toml`:
```toml
warp = { version = "0.4.3", features = ["server", "websocket"] }
```

Without `server`, you get compile errors like "no function `serve` in warp". Without `websocket`, `warp::ws()` is unavailable.

## 2. message_added Empty Assistant Text

This is the number one pitfall for bidirectional forwarding. When Alma creates an assistant message, it fires `message_added` immediately with an empty `parts` array (text = ""). The text is populated later via `message_delta` events, and the complete text arrives in the final `message_updated` event.

Fix: Use `message_updated` (not `message_added`) for bidirectional forwarding, and filter by generation state to avoid forwarding partial updates.

## 3. message_updated Fires Multiple Times

A single assistant message generates multiple `message_updated` events:
- During generation: partial text (skip these)
- After `thread_generating {isGenerating: false}`: full final text (forward this)

Fix: Track generating threads in a `HashSet<String>`. Only forward `message_updated` for assistant messages when the thread is NOT in the generating set.

```rust
// In reader task:
let mut generating_threads: HashSet<String> = HashSet::new();

// On thread_generating event:
if is_generating {
    generating_threads.insert(thread_id.to_string());
} else {
    generating_threads.remove(thread_id);
    // resolve pending generation
}

// On message_updated for assistant:
if role == "assistant" && generating_threads.contains(&thread_id) {
    return; // skip — still generating
}
// safe to forward
```

## 4. Turso Statement Takes mutable self Reference

The `turso` crate's `Statement::query()` and `Statement::execute()` take `&mut self`.
Declare prepared statements as mutable before calling either method.

Correct usage:
```rust
let mut stmt = conn.prepare("SELECT ...").await?;
let rows = stmt.query(params).await?;
```

## 5. No REST Endpoint for Message Sending

Do NOT try to send messages via `POST /api/threads/:id/messages` — this endpoint does not exist. The Electron app uses IPC/internal channels for message sending.

Use the Alma WebSocket protocol (`generate_response`) for the preferred approach, or the `alma run` CLI as a fallback:
```bash
ALMA_THREAD_ID="<thread_id>" alma run --no-stream --raw "user message here"
```

## 6. WebSocket Split Pattern

In Warp, `ws.split()` returns `(SplitSink, SplitStream)`. You cannot use the `SplitSink` from multiple tasks directly — it's not `Clone` and not `Send`-safe for concurrent access.

The correct pattern:
1. Split the WS into sink + stream
2. Create an `mpsc::unbounded_channel`
3. Spawn a dedicated writer task that reads from the channel and writes to the sink
4. Any task sends to the channel (the `Sender` is `Clone`)

```rust
let (sink, stream) = ws.split();
let (tx, rx) = mpsc::unbounded_channel::<Message>();

tokio::spawn(async move {
    let mut sink = sink;
    while let Some(msg) = rx.next().await {
        if let Err(e) = SinkExt::send(&mut sink, msg).await { break; }
    }
});
```

## 7. Per-Thread Generation Guards

Concurrent `generate()` calls for the same Alma thread would corrupt the pending generation map — the second call would overwrite the first call's oneshot sender.

Fix: A `HashMap<String, Arc<Mutex<()>>>` serializes generations per thread. Acquire the per-thread guard before sending `generate_response` and hold it until the response arrives.

```rust
type GenerationGuards = Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>;

let guard = {
    let mut guards = self.guards.lock().await;
    guards.entry(thread_id.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
};
let _guard_lock = guard.lock().await; // serializes per-thread
```

## 8. QQ Message Length Limit

QQ has a ~4500 character limit per message. The bridge splits long Alma replies using a two-pass strategy:
1. Split by paragraphs (double newline `\n\n`)
2. Split each paragraph by the character limit, preferring line breaks

## 9. Edition 2024 Compatibility

Rust edition 2024 is the default for `cargo init` on Rust 1.85+. All dependencies (warp 0.4.3, tokio 1.52, reqwest 0.13) are compatible. Be aware of edition 2024 changes like the `unsafe` attribute requirements and `gen` keyword reservation.

## 10. OneBot Message Segment Format

Always use the array format for messages, not the CQ string format:
```json
{"type": "text", "data": {"text": "hello"}}
```

The CQ string format (`[CQ:text,text=hello]`) is legacy and not used by modern OneBot implementations in array mode.

## 11. WS Path Must Match Client Config

The bridge accepts connections at three paths:
- `/` — generic root
- `/ws` — NapCat/snowluma default
- `/onebot/v11/ws` — Lagrange default

If the OneBot client is configured for a path the bridge doesn't listen on, the connection will be silently rejected. Listen on all three to avoid this.

## 12. Port 8080 Often Occupied

On systems running Docker containers with nginx-ui or similar reverse proxies, port 8080 is commonly taken. The bridge defaults to 8090 to avoid this conflict.

## 13. Container-to-Host Networking

When the OneBot client (e.g., snowluma) runs in an OrbStack or Docker container, use `host.docker.internal` as the hostname to reach the bridge on the Mac host. The container bridge network (e.g., 192.168.148.0/24) cannot directly reach other LAN devices.

## 14. Dedup Comparison Precision

The dedup mechanism compares the first 100 characters of text using bidirectional prefix matching: `sent.starts_with(prefix) || prefix.starts_with(sent_prefix)`. The buffer keeps the last 20 entries per thread. This is sufficient for most messages but could theoretically miss duplicates that diverge after 100 characters.

## 15. Alma WS Reconnect

If the Alma WebSocket connection drops, the bridge should reconnect before the next
generation attempt. A stale `mpsc::UnboundedSender` can remain in state after the reader
task exits, so generation code must check `client.is_connected()` and replace the stored
client when it is closed.

## 16. Do Not Write Alma Database From the External Bridge

Alma's built-in bridges may update `channel_mappings` because they run inside Alma's own
process boundary. This external reverse-WS bridge must not open or write
`chat_threads.db`; Alma commonly keeps that file locked while running, and cross-process
access can fail with "File is locked by another process".

Keep QQ session-to-thread mappings in the bridge's local Turso database. Use Alma REST for
thread creation/existence checks, and use `source = "telegram"` / `"telegram-group"` plus
Telegram-style text and `ephemeralContext` to get Telegram-like prompt behavior.
