# Alma × OneBot v11 接入方案调研报告

> **调研日期**: 2026-06-19  
> **调研范围**: Alma 本地 REST API、CLI、插件系统、Skill 系统、Hooks 机制 × OneBot v11 协议  
> **结论**: **可以通过桥接服务实现**，但 Alma 不提供原生的渠道注册 API，需要自建中间层

---

## 一、Alma 渠道系统现状

### 1.1 现有渠道

Alma 内置了以下渠道，均为 **app 二进制内部实现**，无法通过 API 注册新渠道：

| 渠道 | 配置方式 | 实现方式 |
|------|----------|----------|
| Telegram | `settings.telegram` (botToken, allowedUserIds) | 内置 Bot 适配器 |
| Discord | `settings.discord` (botToken, allowedGuildIds) | 内置 Bot 适配器 + `/api/discord/*` REST 端点 |
| 微信 | `settings.weixin` | 内置适配器 (基于 weixin-agent-sdk) |
| GUI | - | Electron/Tauri 桌面应用 |
| CLI/TUI | - | `alma tui` / `alma run` |

### 1.2 关键发现：无渠道注册 API

通过逆向分析 `api-spec.md` 和 `AppSettings` 类型定义，确认：

- **没有** `/api/channels/register` 或类似的渠道注册接口
- **没有** inbound webhook endpoint 来接收外部消息
- **没有** 公开的 Channel Adapter 接口
- 渠道层是 **紧耦合的内置功能**，不可通过插件或 API 扩展

### 1.3 Alma 可用的集成点

| 集成点 | 能力 | 适用场景 |
|--------|------|----------|
| **REST API** (`localhost:23001`) | 创建/读取 threads、管理 settings | 外部进程与 Alma 交互 |
| **`alma run`** CLI | 完整 AI pipeline (SOUL + Memory + Tools + Plugins) | 获取 AI 回复 |
| **Plugin 系统** | TypeScript 插件，hook 消息事件 | 出站消息转发 |
| **Skill 系统** | SKILL.md + Bash 工具访问 | 让 AI 主动调用 OneBot API |
| **People Profiles** | `~/.config/alma/people/*.md` 用户画像 | 为每个 QQ 用户建立上下文 |
| **MCP 服务器** | 外部工具服务器 | 给 Alma 提供 OneBot 发送能力 |

### 1.4 关于 Thread 在 Alma GUI 中的表现

通过实际测试验证（`POST /api/threads` 创建了标题为 "test onebot" 的对话），**REST API 创建的 Thread 确实会出现在 Alma 侧边栏**。

但与原生渠道相比有以下区别：

| 特性 | 原生渠道 (Telegram/Discord) | 桥接创建的 Thread |
|------|--------------------------|-------------------|
| 出现在侧边栏 | ✅ | ✅ |
| 有专属设置面板 | ✅ (Settings 里有独立区块) | ❌ |
| 渠道内 Bot 命令 | ✅ (`/new`, `/stop`, `/model`) | ❌ |
| 自动关联渠道身份 | ✅ (TG user_id ↔ 对话) | ❌ (需要自建映射) |
| GUI 里显示来源标识 | ✅ ("来自 Telegram") | ❌ (就是普通 Thread) |
| Thread 标题可自定义 | - | ✅ 完全自定义 |
| 可在 GUI 中继续对话 | - | ✅ 打开 Thread 即可交互 |

**结论**: 桥接创建的 Thread 在 GUI 中可见、可交互，只是没有原生渠道的"特殊待遇"（如设置面板、Bot 命令）。日常使用完全够用。

---

## 二、OneBot v11 协议概要

### 2.1 通信方式

| 方式 | 方向 | 适用场景 | 双向能力 |
|------|------|----------|----------|
| **HTTP** | Bot → OneBot (API 调用) | 主动发送消息 | 仅出站 |
| **HTTP POST** | OneBot → Bot (Webhook 推送) | 接收事件 | 仅入站 |
| **HTTP + HTTP POST** | 双向 | 简单集成 | 出站 HTTP，入站 POST |
| **正向 WebSocket** | Bot 连接 OneBot | 双向通信 | 完整双向 |
| **反向 WebSocket** | OneBot 连接 Bot | **推荐**，双向通信 | 完整双向 |

### 2.2 核心 API

```
POST /send_msg          # 发送消息（自动判断私聊/群聊）
POST /send_private_msg  # 发送私聊消息
POST /send_group_msg    # 发送群消息
POST /delete_msg        # 撤回消息
GET  /get_login_info    # 获取机器人信息
GET  /get_friend_list   # 获取好友列表
GET  /get_group_list    # 获取群列表
GET  /get_stranger_info # 获取陌生人信息
GET  /get_group_member_info  # 获取群成员信息
```

### 2.3 事件类型

```json
{
  "time": 1630000000,
  "self_id": 10001000,
  "post_type": "message",        // message | notice | request | meta_event
  "message_type": "private",     // private | group
  "sub_type": "friend",
  "message_id": 12345,
  "user_id": 10001000,
  "message": [{"type": "text", "data": {"text": "hello"}}],
  "sender": {"nickname": "用户", "user_id": 10001000}
}
```

### 2.4 消息段格式（Array 格式）

```json
[
  {"type": "text", "data": {"text": "看看这张图"}},
  {"type": "image", "data": {"file": "ABC.jpg", "url": "https://..."}},
  {"type": "at", "data": {"qq": "10001000"}},
  {"type": "face", "data": {"id": "178"}}
]
```

---

## 三、可行方案：桥接服务架构

### 3.1 架构图

```
┌──────────────┐     OneBot v11      ┌──────────────────┐     REST API      ┌─────────────┐
│   QQ 用户     │◄──────────────────►│  OneBot 实现       │                   │             │
│              │     (QQ 协议)       │ (NapCat/Lagrange) │                   │   Alma App   │
└──────────────┘                     └────────┬─────────┘                   │             │
                                              │                             │  localhost   │
                                    WS / HTTP POST events                   │   :23001    │
                                    + WS / HTTP API calls                   │             │
                                              │                             └──────┬──────┘
                                              ▼                                    ▲
                                     ┌────────────────┐                            │
                                     │  桥接服务         │   POST /api/threads        │
                                     │  (Node.js)      │   alma run                 │
                                     │                 │────────────────────────────┘
                                     │  功能:           │
                                     │  - 事件接收       │
                                     │  - 会话管理       │
                                     │  - 消息转发       │
                                     │  - 回复投递       │
                                     │  - People Profile│
                                     └────────────────┘
```

### 3.2 数据流

```
1. QQ 用户发送消息
2. OneBot 实现 → (WS/HTTP POST) → 桥接服务
3. 桥接服务解析 OneBot 事件，提取文本和图片
4. 桥接服务加载/更新 People Profile (按 QQ user_id)
5. 桥接服务查找或创建对应的 Alma Thread
6. 桥接服务调用 alma run 获取 AI 回复（自动加载 SOUL + Memory + People Profile）
7. 桥接服务调用 OneBot API (send_msg) 发送回复
8. QQ 用户收到回复
```

---

## 四、具体实现方案

### 4.1 桥接服务：反向 WebSocket 模式（推荐）

这是**最推荐的方案**。桥接服务作为 WebSocket Server，OneBot 实现主动连过来，所有通信（事件推送 + API 调用）都在同一个连接上完成。

```typescript
// bridge-server-ws.ts — 反向 WebSocket 桥接服务
import { WebSocketServer, WebSocket } from 'ws';
import http from 'http';
import { execSync } from 'child_process';
import fs from 'fs';
import path from 'path';

// ===== 配置 =====
const ALMA_API = 'http://localhost:23001';
const BRIDGE_PORT = 8080;
const PEOPLE_DIR = path.join(process.env.HOME!, '.config/alma/people');
const STATE_FILE = './bridge-state.json';  // 持久化会话映射

// ===== 状态管理 =====
interface BridgeState {
  threadMap: Record<string, string>;    // sessionKey → threadId
  profileMap: Record<number, string>;   // QQ user_id → profile filename
}

let state: BridgeState = loadState();

function loadState(): BridgeState {
  try {
    return JSON.parse(fs.readFileSync(STATE_FILE, 'utf-8'));
  } catch {
    return { threadMap: {}, profileMap: {} };
  }
}

function saveState() {
  fs.writeFileSync(STATE_FILE, JSON.stringify(state, null, 2));
}

// ===== Pending API calls (echo → resolve) =====
const pendingCalls = new Map<string, (data: any) => void>();
let echoCounter = 0;

function generateEcho(): string {
  return `alma-bridge-${++echoCounter}-${Date.now()}`;
}

// ===== OneBot API 调用（通过 WS） =====
function callOneBotApi(ws: WebSocket, action: string, params: Record<string, any>): Promise<any> {
  const echo = generateEcho();
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      pendingCalls.delete(echo);
      reject(new Error(`OneBot API timeout: ${action}`));
    }, 30000);

    pendingCalls.set(echo, (data) => {
      clearTimeout(timeout);
      resolve(data);
    });

    ws.send(JSON.stringify({ action, params, echo }));
  });
}

// ===== 创建 HTTP Server (健康检查 + 手动触发) =====
const httpServer = http.createServer((req, res) => {
  if (req.url === '/health') {
    res.writeHead(200);
    res.end(JSON.stringify({ status: 'ok', connections: wss.clients.size }));
  } else {
    res.writeHead(404);
    res.end('Not Found');
  }
});

// ===== WebSocket Server =====
const wss = new WebSocketServer({ server: httpServer });

wss.on('connection', (ws, req) => {
  const selfId = req.headers['x-self-id'] as string;
  const role = req.headers['x-client-role'] as string;
  console.log(`[OneBot] Connected: QQ=${selfId}, Role=${role}`);

  ws.on('message', (raw) => {
    try {
      const msg = JSON.parse(raw.toString());

      // ----- API 响应 -----
      if (msg.echo && msg.retcode !== undefined) {
        const resolver = pendingCalls.get(msg.echo);
        if (resolver) {
          pendingCalls.delete(msg.echo);
          resolver(msg);
        }
        return;
      }

      // ----- 事件推送 -----
      if (msg.post_type) {
        handleEvent(msg, ws);
      }
    } catch (e) {
      console.error('[Parse Error]', e);
    }
  });

  ws.on('close', () => {
    console.log(`[OneBot] Disconnected: QQ=${selfId}`);
  });
});

// ===== 事件分发 =====
async function handleEvent(event: any, ws: WebSocket) {
  switch (event.post_type) {
    case 'message':
      await handleMessage(event, ws);
      break;
    case 'meta_event':
      if (event.meta_event_type === 'heartbeat') {
        // 心跳，忽略
      } else if (event.meta_event_type === 'lifecycle') {
        console.log(`[OneBot] Lifecycle: ${event.sub_type}`);
      }
      break;
    case 'notice':
      console.log(`[OneBot] Notice: ${event.notice_type}`);
      break;
    case 'request':
      console.log(`[OneBot] Request: ${event.request_type}`);
      break;
  }
}

// ===== 消息处理 =====
async function handleMessage(event: any, ws: WebSocket) {
  const { user_id, group_id, message, message_type, sender } = event;

  // 提取纯文本（过滤掉 at、image 等非文本段）
  const text = extractText(message);
  if (!text) {
    console.log(`[Message] No text content from ${user_id}, skipping`);
    return;
  }

  // 处理 @bot 前缀（群聊中通常需要 at 才响应）
  const botQQ = event.self_id;
  const cleanedText = text.replace(new RegExp(`\\[CQ:at,qq=${botQQ}\\]`), '').trim();
  if (message_type === 'group' && !text.includes(String(botQQ))) {
    // 群聊中没有 at bot，忽略
    return;
  }

  console.log(`[Message] ${message_type === 'group' ? `群${group_id}` : '私聊'} ${sender?.nickname || user_id}: ${cleanedText}`);

  // 1. 确保 People Profile 存在
  await ensurePeopleProfile(ws, user_id, sender);

  // 2. 生成会话 key
  const sessionKey = message_type === 'group'
    ? `group:${group_id}`
    : `private:${user_id}`;

  // 3. 查找或创建 Alma Thread
  let threadId = state.threadMap[sessionKey];
  if (!threadId) {
    const title = message_type === 'group'
      ? `QQ群 ${group_id}`
      : `QQ私聊 ${sender?.nickname || user_id}`;
    threadId = await createAlmaThread(title);
    state.threadMap[sessionKey] = threadId;
    saveState();
    console.log(`[Thread] Created: "${title}" → ${threadId}`);
  }

  // 4. 调用 Alma AI 获取回复
  const reply = await callAlma(threadId, cleanedText || text);

  // 5. 通过 OneBot WS 发送回复
  if (reply) {
    const sendParams = message_type === 'group'
      ? { message_type: 'group', group_id, message: reply }
      : { message_type: 'private', user_id, message: reply };

    try {
      const result = await callOneBotApi(ws, 'send_msg', sendParams);
      console.log(`[Reply] Sent to ${message_type === 'group' ? `群${group_id}` : user_id}, msg_id=${result?.data?.message_id}`);
    } catch (e) {
      console.error('[Reply] Failed:', e);
    }
  }
}

// ===== 提取文本 =====
function extractText(segments: any[]): string {
  return segments
    .filter(seg => seg.type === 'text')
    .map(seg => seg.data.text)
    .join(' ')
    .trim();
}

// ===== People Profile 管理 =====
async function ensurePeopleProfile(ws: WebSocket, userId: number, sender: any) {
  if (state.profileMap[userId]) return;

  // 尝试获取更详细的用户信息
  let nickname = sender?.nickname || `QQ用户${userId}`;
  try {
    const info = await callOneBotApi(ws, 'get_stranger_info', { user_id: userId });
    if (info?.data?.nickname) {
      nickname = info.data.nickname;
    }
  } catch { /* 忽略，用 sender 信息 */ }

  const safeName = nickname.replace(/[/\\:*?"<>|]/g, '_');
  const profilePath = path.join(PEOPLE_DIR, `${safeName}.md`);

  if (!fs.existsSync(profilePath)) {
    const content = `---
qq_id: "${userId}"
qq_nickname: "${nickname}"
---
# ${nickname}

- QQ 用户，ID: ${userId}
- 昵称: ${nickname}
- 首次互动: ${new Date().toISOString().split('T')[0]}
`;
    fs.writeFileSync(profilePath, content);
    console.log(`[People] Created profile: ${safeName}.md`);
  }

  state.profileMap[userId] = safeName;
  saveState();
}

// ===== 创建 Alma Thread =====
async function createAlmaThread(title: string): Promise<string> {
  const res = await fetch(`${ALMA_API}/api/threads`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ title })
  });
  const data = await res.json();
  return data.id;
}

// ===== 调用 Alma AI =====
async function callAlma(threadId: string, message: string): Promise<string> {
  try {
    const result = execSync(
      `ALMA_THREAD_ID=${threadId} alma run --raw ${shellEscape(message)}`,
      { timeout: 120000, encoding: 'utf-8', env: { ...process.env, ALMA_THREAD_ID: threadId } }
    );
    return result.trim();
  } catch (e: any) {
    console.error('[Alma] Run failed:', e.message);
    return '抱歉，我暂时无法回复 >_<';
  }
}

// ===== Shell 转义 =====
function shellEscape(s: string): string {
  return `'${s.replace(/'/g, "'\\''")}'`;
}

// ===== 启动 =====
httpServer.listen(BRIDGE_PORT, () => {
  console.log(`[Bridge] Listening on port ${BRIDGE_PORT}`);
  console.log(`[Bridge] Waiting for OneBot reverse WebSocket connection...`);
});
```

### 4.2 桥接服务：HTTP POST + HTTP API 模式（简单方案）

如果不想用 WebSocket，也可以用 HTTP POST 接收事件 + HTTP API 调用。更简单但功能受限。

```typescript
// bridge-server-http.ts — HTTP 模式桥接服务
import express from 'express';
import { execSync } from 'child_process';
import fs from 'fs';
import path from 'path';

const app = express();
app.use(express.json());

const ALMA_API = 'http://localhost:23001';
const ONEBOT_HTTP = 'http://localhost:3000';  // OneBot 实现的 HTTP API

// 会话映射（持久化到文件）
const STATE_FILE = './bridge-state.json';
let threadMap: Record<string, string> = {};
try { threadMap = JSON.parse(fs.readFileSync(STATE_FILE, 'utf-8')).threadMap || {}; } catch {}

function saveState() {
  fs.writeFileSync(STATE_FILE, JSON.stringify({ threadMap }, null, 2));
}

// ===== 接收 OneBot HTTP POST 事件 =====
app.post('/webhook/onebot', async (req, res) => {
  const event = req.body;

  // 快速响应 OneBot（避免阻塞）
  res.json({});

  if (event.post_type === 'message') {
    // 异步处理，不阻塞 OneBot 的 HTTP 响应
    handleMessage(event).catch(e => console.error('[Error]', e));
  }
});

async function handleMessage(event: any) {
  const { user_id, group_id, message, message_type, sender } = event;

  const text = message
    .filter((seg: any) => seg.type === 'text')
    .map((seg: any) => seg.data.text)
    .join(' ')
    .trim();
  if (!text) return;

  const sessionKey = message_type === 'group' ? `group:${group_id}` : `private:${user_id}`;

  // 查找或创建 Thread
  let threadId = threadMap[sessionKey];
  if (!threadId) {
    const title = message_type === 'group' ? `QQ群 ${group_id}` : `QQ私聊 ${sender?.nickname || user_id}`;
    const res = await fetch(`${ALMA_API}/api/threads`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ title })
    });
    const data = await res.json();
    threadId = data.id;
    threadMap[sessionKey] = threadId;
    saveState();
  }

  // 调用 Alma
  let reply: string;
  try {
    reply = execSync(
      `ALMA_THREAD_ID=${threadId} alma run --raw ${shellEscape(text)}`,
      { timeout: 120000, encoding: 'utf-8' }
    ).trim();
  } catch {
    reply = '抱歉，我暂时无法回复 >_<';
  }

  // 通过 OneBot HTTP API 发送回复
  const body = message_type === 'group'
    ? { message_type: 'group', group_id, message: reply }
    : { message_type: 'private', user_id, message: reply };

  await fetch(`${ONEBOT_HTTP}/send_msg`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body)
  });
}

function shellEscape(s: string): string {
  return `'${s.replace(/'/g, "'\\''")}'`;
}

app.listen(8080, () => console.log('[Bridge-HTTP] Listening on :8080'));
```

### 4.3 桥接服务：正向 WebSocket 模式

桥接服务主动连接 OneBot 的 WebSocket Server：

```typescript
// bridge-server-forward-ws.ts — 正向 WebSocket 模式
import WebSocket from 'ws';
import { execSync } from 'child_process';

const ONEBOT_WS = 'ws://127.0.0.1:3001';  // OneBot WS Server 地址
const ALMA_API = 'http://localhost:23001';

let ws: WebSocket;
let reconnectTimer: NodeJS.Timeout;

function connect() {
  ws = new WebSocket(ONEBOT_WS);

  ws.on('open', () => {
    console.log('[Forward-WS] Connected to OneBot');
  });

  ws.on('message', (raw) => {
    const msg = JSON.parse(raw.toString());

    // API 响应
    if (msg.echo && msg.retcode !== undefined) {
      const resolver = pendingCalls.get(msg.echo);
      if (resolver) { pendingCalls.delete(msg.echo); resolver(msg); }
      return;
    }

    // 事件
    if (msg.post_type === 'message') {
      handleMessage(msg);
    }
  });

  ws.on('close', () => {
    console.log('[Forward-WS] Disconnected, reconnecting in 3s...');
    reconnectTimer = setTimeout(connect, 3000);
  });

  ws.on('error', (err) => {
    console.error('[Forward-WS] Error:', err.message);
    ws.close();
  });
}

// ... handleMessage / callAlma 等与反向 WS 相同 ...

connect();
```

### 4.4 OneBot 实现侧配置

#### NapCat 配置（反向 WebSocket — 推荐）

```yaml
# NapCat/config/onebot11_<qq号>.yml

ws_reverse:
  enable: true
  url: "ws://127.0.0.1:8080"
  use_universal_client: true    # 使用 Universal 模式（API + Event 共用一个连接）
  reconnect_interval: 3000      # 断线重连间隔 (ms)

# HTTP API 也需要开启（桥接服务可能 fallback 到 HTTP 调用）
http:
  enable: true
  host: 127.0.0.1
  port: 3000
  secret: ""
```

#### Lagrange 配置（反向 WebSocket）

```toml
# Lagrange/appsettings.json 中 OneBot 配置段

{
  "Implementations": [
    {
      "Type": "ReverseWebSocket",
      "Host": "127.0.0.1",
      "Port": 8080,
      "Suffix": "/onebot/v11/ws",
      "ReconnectInterval": 3000,
      "HeartbeatInterval": 5000
    }
  ]
}
```

#### HTTP POST 模式配置（简单方案）

```yaml
# NapCat — 仅 HTTP 模式
http:
  enable: true
  host: 127.0.0.1
  port: 3000

http_post:
  enable: true
  url: "http://127.0.0.1:8080/webhook/onebot"
  secret: ""
  max_retries: 3
  retries_interval: 1500
```

### 4.5 Alma Skill（让 AI 主动发消息到 QQ）

创建 `~/.config/alma/skills/onebot-send/SKILL.md`：

```yaml
---
name: onebot-send
description: "Send messages to QQ via OneBot v11 API. Use when asked to send a message to a QQ user or group, or when you need to proactively reach someone on QQ."
allowed-tools:
  - Bash
---

# OneBot v11 发送技能

通过 OneBot v11 HTTP API 向 QQ 用户或群组发送消息。

OneBot HTTP API 地址: http://localhost:3000

## 发送消息

```bash
# 发送私聊消息
curl -s http://localhost:3000/send_private_msg \
  -H "Content-Type: application/json" \
  -d '{"user_id": QQ_USER_ID, "message": "消息内容"}'

# 发送群消息
curl -s http://localhost:3000/send_group_msg \
  -H "Content-Type: application/json" \
  -d '{"group_id": QQ_GROUP_ID, "message": "消息内容"}'

# 发送消息（自动判断类型）
curl -s http://localhost:3000/send_msg \
  -H "Content-Type: application/json" \
  -d '{"message_type": "private", "user_id": 12345, "message": "hello"}'

# 发送带 @的消息
curl -s http://localhost:3000/send_group_msg \
  -H "Content-Type: application/json" \
  -d '{"group_id": QQ_GROUP_ID, "message": [{"type":"at","data":{"qq":"12345"}},{"type":"text","data":{"text":" 你好"}}]}'
```

## 撤回消息

```bash
curl -s http://localhost:3000/delete_msg \
  -H "Content-Type: application/json" \
  -d '{"message_id": MESSAGE_ID}'
```

## 查询信息

```bash
# 获取机器人信息
curl -s http://localhost:3000/get_login_info

# 获取好友列表
curl -s http://localhost:3000/get_friend_list

# 获取群列表
curl -s http://localhost:3000/get_group_list

# 获取群成员列表
curl -s http://localhost:3000/get_group_member_list \
  -H "Content-Type: application/json" \
  -d '{"group_id": QQ_GROUP_ID}'

# 获取陌生人信息
curl -s http://localhost:3000/get_stranger_info \
  -H "Content-Type: application/json" \
  -d '{"user_id": QQ_USER_ID}'
```
```

### 4.6 Plugin 方案（可选：Alma GUI 回复后自动转发到 QQ）

创建 `~/.config/alma/plugins/onebot-forward/`：

**manifest.json**:
```json
{
  "id": "onebot-forward",
  "name": "OneBot Forward",
  "version": "1.0.0",
  "description": "Forward Alma GUI responses to QQ via OneBot v11",
  "author": { "name": "star" },
  "main": "main.js",
  "engines": { "alma": "^0.1.0" },
  "type": "transform",
  "permissions": ["chat:read", "chat:write"],
  "activationEvents": ["onStartup"],
  "contributes": {
    "configuration": {
      "title": "OneBot Forward",
      "properties": {
        "onebotForward.enabled": {
          "type": "boolean",
          "default": true,
          "description": "Enable auto-forwarding to QQ"
        },
        "onebotForward.onebotHttp": {
          "type": "string",
          "default": "http://localhost:3000",
          "description": "OneBot HTTP API address"
        }
      }
    }
  }
}
```

**main.ts**:
```typescript
import { PluginContext } from 'alma-plugin-api';

export function activate(context: PluginContext) {
  const onebotHttp = context.settings.get('onebotForward.onebotHttp', 'http://localhost:3000');

  // 监听 AI 回复完成
  context.events.on('chat.message.didReceive', async (event) => {
    if (!context.settings.get('onebotForward.enabled', true)) return;

    const { threadId, response } = event;
    const threads = await context.chat.listThreads();
    const thread = threads.find(t => t.id === threadId);

    // 检查是否是 QQ 相关 Thread（通过标题前缀判断）
    if (!thread?.title?.match(/^QQ(群|私聊) \d+/)) return;

    const match = thread.title.match(/^QQ(群|私聊) (\d+)/);
    if (!match) return;

    const [, type, id] = match;
    const body = type === '群'
      ? { message_type: 'group', group_id: parseInt(id), message: response }
      : { message_type: 'private', user_id: parseInt(id), message: response };

    try {
      await fetch(`${onebotHttp}/send_msg`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body)
      });
      context.logger.info(`Forwarded to QQ: ${thread.title}`);
    } catch (e) {
      context.logger.error(`Forward failed: ${e}`);
    }
  });

  context.logger.info('OneBot Forward plugin activated');
}
```

> **Plugin 的使用场景**: 当你直接在 Alma GUI 的 QQ Thread 里打字对话时，Plugin 会自动把你的 AI 回复转发到 QQ。如果不安装 Plugin，在 GUI 里的对话只在本地可见，不会发到 QQ。

---

## 五、People Profiles 集成

### 5.1 Alma People Profiles 是什么

Alma 在 `~/.config/alma/people/` 目录下存储用户画像文件，格式为 YAML frontmatter + Markdown：

```markdown
---
qq_id: "12345678"
qq_nickname: "小明"
---
# 小明

- QQ 用户，ID: 12345678
- 昵称: 小明
- 首次互动: 2026-06-19
```

当 `alma run` 执行时，Alma 会自动检索相关的 People Profile 并注入到 AI 的上下文中，让 AI "记住"这个人。

### 5.2 桥接服务如何集成

桥接服务可以在以下时机操作 People Profiles：

| 时机 | 操作 |
|------|------|
| **首次遇到用户** | 创建 `people/{昵称}.md`，写入 QQ ID、昵称、首次互动时间 |
| **后续交互** | 可选：追加互动记录、偏好标签等 |
| **群聊首次发言** | 创建或更新对应用户的 profile |

```typescript
// 桥接服务中的 People Profile 管理
async function ensurePeopleProfile(ws: WebSocket, userId: number, sender: any) {
  // 1. 获取用户详细信息
  let info: any = {};
  try {
    const result = await callOneBotApi(ws, 'get_stranger_info', { user_id: userId });
    info = result?.data || {};
  } catch {}

  const nickname = sender?.nickname || info?.nickname || `QQ用户${userId}`;
  const safeName = nickname.replace(/[/\\:*?"<>|]/g, '_');
  const profilePath = path.join(PEOPLE_DIR, `${safeName}.md`);

  // 2. 如果 profile 不存在，创建
  if (!fs.existsSync(profilePath)) {
    const content = `---
qq_id: "${userId}"
qq_nickname: "${nickname}"
---
# ${nickname}

- QQ 用户，ID: ${userId}
- 昵称: ${nickname}
- 首次互动: ${new Date().toISOString().split('T')[0]}
`;
    fs.writeFileSync(profilePath, content);
    console.log(`[People] Created profile: ${safeName}.md`);
  }
}
```

### 5.3 效果

当 QQ 用户 "小明" (ID: 12345) 发消息时：

1. 桥接服务收到 OneBot 事件
2. 确保 `~/.config/alma/people/小明.md` 存在
3. 调用 `alma run`，Alma 自动加载小明的 profile 到上下文
4. AI 回复时会知道"小明是 QQ 上 ID 为 12345 的用户"
5. 随着交互增多，profile 可以积累更多信息

---

## 六、方案对比

| 维度 | 反向 WS 桥接 (推荐) | HTTP POST 桥接 | 纯 Skill | 纯 Plugin |
|------|---------------------|---------------|---------|----------|
| **入站消息** | ✅ 完整支持 | ✅ 完整支持 | ❌ 无法接收 | ❌ 无法接收 |
| **出站消息** | ✅ 完整支持 | ✅ 完整支持 | ✅ AI 主动发送 | ✅ 自动转发 |
| **双向通信** | ✅ 同一 WS 连接 | ⚠️ 分开的通道 | ❌ 仅出站 | ⚠️ 有限 |
| **会话管理** | ✅ 自定义映射 | ✅ 自定义映射 | ❌ 无 | ⚠️ 有限 |
| **SOUL/Memory** | ✅ alma run | ✅ alma run | ✅ alma run | ✅ 原生支持 |
| **People Profile** | ✅ 自动创建 | ✅ 自动创建 | ❌ 手动 | ❌ 手动 |
| **实时性** | ⚠️ 受 alma run 延迟 | ⚠️ 同上 | ⚠️ 同上 | ⚠️ 异步 |
| **连接稳定性** | ✅ 自动重连 | ✅ 无状态 | - | - |
| **复杂度** | 中等 | 低 | 最低 | 中等 |
| **流式输出** | ❌ 不支持 | ❌ 不支持 | ❌ 不支持 | ❌ 不支持 |
| **GUI 可见** | ✅ Thread 在侧边栏 | ✅ Thread 在侧边栏 | - | ✅ |

**推荐方案**: 反向 WebSocket 桥接服务 + Skill + 可选 Plugin

---

## 七、核心局限性（基于事实）

### 7.1 无法做到的

| 局限 | 原因 | 影响 |
|------|------|------|
| **无法注册为一等公民渠道** | Alma 没有公开的 Channel Registration API | 没有专属设置面板、Bot 命令、来源标识 |
| **无法流式输出** | `alma run` 需要等待完整生成 | 用户需要等待完整回复生成（通常 5-30s） |
| **无法双向同步 GUI 操作** | REST API 不支持 GUI 实时同步 | 在 Alma GUI 里的操作不会自动反映到 QQ（除非安装 Plugin） |
| **插件无法创建入站消息** | Plugin 只有 `chat.message.willSend/didReceive` 等 hook | 无法模拟 "用户发来消息" 让 GUI 自动刷新 |
| **无原生 session 管理** | Alma 没有 session context API | 需要自建用户→线程映射并持久化 |

### 7.2 可以 workaround 的

| 局限 | Workaround |
|------|-----------|
| 富媒体消息 (image/record) | 桥接服务下载图片 → 转为 base64 → 通过 `alma run` 的 vision 能力处理 |
| 长消息分段 | 桥接服务按 QQ 限制 (4500 字) 拆分，分段发送 |
| 多用户并发 | 桥接服务用 Promise.all 或队列并发调用 `alma run` |
| 会话持久化 | 桥接服务本地 JSON 文件存储 threadMap，启动时加载 |
| People Profile 同步 | 桥接服务自动创建/更新 profile 文件 |
| GUI 对话同步到 QQ | 安装 Plugin 监听 `chat.message.didReceive` |

---

## 八、推荐的 OneBot 实现选择

| 实现 | 语言 | 状态 | 说明 |
|------|------|------|------|
| **NapCat** | Node.js | ✅ 活跃 | 推荐，轻量，配置简单，社区活跃 |
| **Lagrange** | C# | ✅ 活跃 | 功能完整，性能好，跨平台 |
| **LLOneBot** | JS | ✅ 活跃 | 基于 LiteLoader，NTQQ 插件式 |
| **Chronocat** | TS | ⚠️ 活跃 | Satori 协议兼容，也支持 OneBot |
| **go-cqhttp** | Go | ❌ 已归档 | 不再维护但仍有用户 |

---

## 九、完整部署步骤

### 9.1 前置条件

- macOS 上 Alma 正在运行 (`alma status` 确认)
- Node.js 18+ 已安装
- 一个 QQ 账号用于 Bot

### 9.2 安装 OneBot 实现（以 NapCat 为例）

```bash
# 参考 NapCat 官方文档: https://napneko.github.io/
# 安装完成后，配置 onebot11_<qq号>.yml:

cat > napcat/config/onebot11_你的QQ号.yml << 'EOF'
ws_reverse:
  enable: true
  url: "ws://127.0.0.1:8080"
  use_universal_client: true
  reconnect_interval: 3000

http:
  enable: true
  host: 127.0.0.1
  port: 3000
  secret: ""
EOF
```

### 9.3 创建桥接服务

```bash
mkdir alma-onebot-bridge && cd alma-onebot-bridge
npm init -y
npm install ws express
npm install -D typescript @types/node @types/ws ts-node
npx tsc --init

# 将 4.1 节的代码保存到 bridge-server-ws.ts
# 将 4.5 节的 SKILL.md 保存到 ~/.config/alma/skills/onebot-send/SKILL.md
```

### 9.4 安装 Alma Skill

```bash
mkdir -p ~/.config/alma/skills/onebot-send
# 写入 SKILL.md（见 4.5 节）
```

### 9.5 可选：安装 Plugin

```bash
mkdir -p ~/.config/alma/plugins/onebot-forward
# 写入 manifest.json 和 main.js（见 4.6 节）
```

### 9.6 启动顺序

```bash
# 终端 1: 确认 Alma 运行中
alma status

# 终端 2: 启动桥接服务
cd alma-onebot-bridge
npx ts-node bridge-server-ws.ts
# 输出: [Bridge] Listening on port 8080
# 输出: [Bridge] Waiting for OneBot reverse WebSocket connection...

# 终端 3: 启动 OneBot 实现 (NapCat)
# 启动后会看到:
# 输出: [Bridge] [OneBot] Connected: QQ=12345678, Role=Universal
```

### 9.7 测试

1. 用另一个 QQ 号给 Bot 号发私聊消息 "你好"
2. 桥接服务应输出: `[Thread] Created: "QQ私聊 xxx" → <threadId>`
3. 然后输出: `[Reply] Sent to 12345678, msg_id=...`
4. QQ 上收到 AI 回复
5. 打开 Alma GUI，侧边栏应出现 "QQ私聊 xxx" Thread

---

## 十、总结

| 问题 | 回答 |
|------|------|
| **Alma 能否原生接入 OneBot v11？** | ❌ 不能，Alma 没有公开的渠道注册 API |
| **能否通过 CLI/Hooks 实现？** | ✅ 可以，通过桥接服务 + `alma run` + REST API |
| **Thread 会出现在侧边栏吗？** | ✅ 会，通过 REST API 创建的 Thread 正常显示在 GUI |
| **支持 WebSocket 吗？** | ✅ 支持正向/反向 WS，推荐反向 WS |
| **支持 People Profiles 吗？** | ✅ 桥接服务可自动创建 profile，`alma run` 自动加载 |
| **实现复杂度？** | 中等，需要编写一个独立的桥接服务 (~200 行代码) |
| **功能完整度？** | 约 80%（文本消息完整，People Profile 可用，富媒体需额外处理，无流式输出） |
| **生产可用性？** | 可作为个人/小团队方案 |

**核心结论**: Alma 的渠道系统是封闭的，但其 CLI (`alma run`)、REST API (`POST /api/threads`) 和插件系统提供了足够的扩展点。通过桥接服务（推荐反向 WebSocket 模式）可以实现 OneBot v11 的双向通信，并且支持 People Profiles、SOUL、Memory 等所有 Alma 上下文增强能力。最大的限制是缺乏流式输出和原生渠道身份。
