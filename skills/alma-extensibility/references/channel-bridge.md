# Custom Channel Bridge Patterns

For integrating unsupported platforms (QQ/OneBot, Slack, Matrix, etc.), there are
**two approaches** with different tradeoffs.

## Approach A: WebSocket Bridge (Native — Recommended)

Alma's built-in channels (Telegram, Discord, Feishu, WeChat) all use a WebSocket
protocol to communicate with the Alma server. You can build a custom bridge using
the same protocol for **much better integration**.

```
External Platform → Custom Bridge → ws://127.0.0.1:23001/ws/threads → Alma Server
                    (generate_response, ephemeralContext, SENDER PROFILE injection)
```

**Advantages over REST+CLI approach:**
- Streaming responses (real-time delta events)
- Native SENDER PROFILE matching (bridge scans `~/.config/alma/people/*.md`)
- Reply/quoting support (`buildReplyContext` pattern)
- Same processing pipeline as built-in channels (SOUL + Memory + Tools + Plugins)
- Messages appear in GUI with proper formatting

For the full WebSocket protocol details, use the **`alma-channel-protocol`** skill.
It covers:
- `generate_response` message format and all optional fields
- Event sequence (`message_delta`, `message_updated`, `thread_generating`)
- `ephemeralContext` assembly (SENDER PROFILE, group history, people summary)
- Profile matching algorithm (bridge-side scanning of `~/.config/alma/people/*.md`)
- Reply/quoting protocol (incoming `buildReplyContext` + outgoing reply segments)
- `channel_mappings` table and platform routing

## Approach B: REST API + `alma run` (External — Simpler)

For quick prototypes or when WebSocket integration is too complex:

```
External Platform → Adapter (OneBot/Slack API) → Bridge Service
                                                       ↕
                                           REST API + alma run
                                                       ↕
                                                   Alma App
```

**Implementation Steps:**
1. **Inbound**: Bridge receives events from external platform (HTTP POST or WebSocket)
2. **Session mapping**: Maintain `{platform}:{chatId}` → Alma Thread ID (persist to file or DB)
3. **Thread creation**: `POST /api/threads {"title": "QQ群 123456"}` — appears in GUI
4. **AI completion**: `ALMA_THREAD_ID=<id> alma run --raw '<message>'`
5. **Outbound**: Call external platform API to send reply
6. **People Profile**: Auto-create `~/.config/alma/people/<name>.md` for each user

For REST endpoints, see [rest-api.md](rest-api.md). For `alma run` flags and
environment variables, see [cli-reference.md](cli-reference.md).

## Comparison

| Feature | Approach A (WebSocket) | Approach B (REST + CLI) |
|---------|----------------------|------------------------|
| Streaming output | Real-time deltas | Must wait for full response |
| GUI integration | Native-level | Basic thread visibility |
| Profile matching | Automatic (SENDER PROFILE) | Manual (write profile files) |
| Reply/quoting | Supported | Not supported |
| Implementation effort | Higher (WebSocket protocol) | Lower (HTTP calls) |
| Dependencies | None (direct WS connection) | Requires `alma` CLI available |

## What Works vs What Doesn't (Both Approaches)

| Works | Doesn't Work |
|-------|-------------|
| Threads visible in GUI sidebar | No native channel settings panel |
| Full AI pipeline (SOUL + Memory + Tools + Plugins) | No channel registration API |
| Bidirectional messaging | No GUI ↔ external sync without Plugin |
| Per-user/group conversation history | No channel identity badges |
| People Profiles auto-loaded | |

## Related References

- [rest-api.md](rest-api.md) — Thread creation and management endpoints
- [cli-reference.md](cli-reference.md) — `alma run` command and all CLI subcommands
- [plugin-system.md](plugin-system.md) — Plugin hooks for reacting to AI replies
- [people-profiles.md](people-profiles.md) — Profile file format and cross-platform identity
- The **`alma-channel-protocol`** skill — full WebSocket protocol for Approach A
