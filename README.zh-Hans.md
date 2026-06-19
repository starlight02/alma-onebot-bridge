# Alma OneBot Bridge

将 [Alma](https://github.com/anthropics/alma)（本地 AI 聊天助手）通过 [OneBot v11](https://github.com/botuniverse/onebot-11) 协议接入 QQ 的桥接服务。采用反向 WebSocket 架构，让 Alma 可以作为 QQ 私聊和群聊中的机器人使用。

## 特性

- **完整 Alma 管线** — 消息通过 Alma WebSocket 协议处理，SOUL、Memory、People Profiles、Skills 全部生效
- **双向同步** — Alma GUI 中发送的消息会转发到 QQ，反之亦然
- **群聊支持** — 群聊中 @bot 才响应，使用群名片作为显示名称
- **群聊历史** — 将最近的群聊消息注入 AI 上下文，让 bot 了解群内讨论内容
- **富消息处理** — QQ 表情转换为可读文本（`[emoji:斜眼笑]`），图片/语音/视频以标签描述，转发消息提取内容摘要
- **回复 & @提及** — 完整的引用/回复协议（incoming 引用上下文 + outgoing 回复引用），群聊回复自动 @用户
- **People Profiles** — 自动为每个 QQ 用户创建 Alma People Profile 文件，包含 `qq_id` 前置标识，支持跨平台身份匹配
- **消息分段** — 长回复按段落和 QQ 的 4500 字限制自动拆分
- **状态持久化** — 线程映射和用户资料存储在 Turso（libsql）数据库中
- **安全认证** — 可选的 WebSocket 访问令牌认证（`Bearer` 头）
- **灵活配置** — TOML 配置文件 + 环境变量覆盖

## 架构

```
QQ 用户 ──► snowluma/NapCat ──WS──► 桥接服务 ──WS──► Alma
                  (OneBot v11)          │               │
                                        ├── REST: 创建线程
                                        └── WS:   generate_response
```

桥接服务同时作为 OneBot 客户端的 **WebSocket 服务器** 和 Alma 内部聊天管线的 **WebSocket 客户端**（`ws://localhost:23001/ws/threads`）。

## 快速开始

### 前置条件

- [Alma](https://github.com/anthropics/alma) 在本地运行（`alma status` 验证）
- OneBot v11 客户端（如 [snowluma](https://github.com/nickyc975/snowluma) 或 NapCat），配置为反向 WebSocket
- Rust 工具链（1.85+，edition 2024）

### 编译

```bash
git clone <repo-url>
cd alma-onebot-bridge
cargo build --release
```

### 配置

复制示例配置并按需编辑：

```bash
cp config.toml.example config.toml
# 编辑 config.toml 设置你的参数
```

主要配置项：

```toml
[bridge]
port = 8090

[alma]
api = "http://localhost:23001"
# model = "anthropic:claude-sonnet-4-20250514"  # 覆盖默认模型
timeout = 120

[onebot]
api_timeout = 30
# access_token = ""  # 取消注释以要求 WS 连接携带 Bearer 令牌

[chat]
group_history_size = 30        # 群聊历史上下文条数（0 = 禁用）
# thinking_message = "思考中..."  # AI 生成前发送的提示消息（可选）
```

> **注意**：`config.toml` 已在 `.gitignore` 中，不会被提交到 git。仓库只追踪 `config.toml.example` 模板。

环境变量优先级高于配置文件（如 `ALMA_MODEL`、`BRIDGE_PORT`）。

### 配置 OneBot 客户端

在 OneBot 客户端配置中添加反向 WebSocket 连接。以 snowluma 为例，编辑 `/app/snowluma-data/config/onebot_<qq_id>.json`：

```json
{
  "networks": {
    "wsClients": [
      {
        "name": "Alma",
        "url": "ws://<bridge-host>:8090/ws",
        "messageFormat": "array",
        "reportSelfMessage": false,
        "role": "Universal",
        "reconnectIntervalMs": 5000
      }
    ]
  }
}
```

如果 OneBot 客户端运行在 Docker 中，`<bridge-host>` 使用 `host.docker.internal`。

### 运行

```bash
# 启动桥接服务
./target/release/alma-onebot-bridge

# 开启调试日志
RUST_LOG=debug ./target/release/alma-onebot-bridge
```

启动顺序：Alma → 桥接服务 → OneBot 客户端。

## 配置参考

| 环境变量 | TOML 键 | 默认值 | 说明 |
|----------|---------|--------|------|
| `BRIDGE_PORT` | `bridge.port` | `8090` | 监听端口 |
| `ALMA_API` | `alma.api` | `http://localhost:23001` | Alma API 地址 |
| `ALMA_MODEL` | `alma.model` | *(Alma 设置)* | 覆盖 AI 模型 |
| `ALMA_TIMEOUT` | `alma.timeout` | `120` | 生成超时（秒） |
| `ALMA_MAX_RETRIES` | `alma.max_retries` | `2` | 生成失败重试次数 |
| `ALMA_RETRY_DELAY` | `alma.retry_delay_ms` | `3000` | 重试基础延迟（毫秒，指数退避） |
| `DB_PATH` | `database.path` | `bridge-state.db` | 数据库文件路径 |
| `PEOPLE_DIR` | `people.dir` | `~/.config/alma/people` | People Profiles 目录 |
| `ONEBOT_API_TIMEOUT` | `onebot.api_timeout` | `30` | OneBot API 超时（秒） |
| `ACCESS_TOKEN` | `onebot.access_token` | *(无)* | WS 连接 Bearer 令牌认证 |
| `GROUP_HISTORY_SIZE` | `chat.group_history_size` | `30` | 群聊历史上下文条数（0 = 禁用） |
| `THINKING_MESSAGE` | `chat.thinking_message` | *(无)* | AI 生成前的提示消息 |
| `RUST_LOG` | — | `info` | 日志级别（env-filter 语法） |

## 工作原理

### 消息流（QQ → Alma → QQ）

1. QQ 用户发送消息（群聊中 @bot）
2. OneBot 客户端通过反向 WebSocket 推送事件给桥接服务
3. 桥接服务提取文本、表情、媒体信息，记录到群聊历史
4. 桥接服务处理引用/回复上下文和转发消息提取
5. 桥接服务为用户创建 People Profile（如不存在）
6. 桥接服务查找或创建 Alma 线程（按 `private:{user_id}` 或 `group:{group_id}` 匹配）
7. 桥接服务通过 Alma WebSocket 发送 `generate_response`，附带发送者身份和上下文信息
8. Alma 使用完整管线处理（SOUL + Memory + People Profiles）
9. 桥接服务收集回复并发送回 QQ（群聊首条消息附带回复引用和 @提及）

### 双向同步（Alma GUI → QQ）

在 Alma GUI 中为已跟踪的线程发送消息时，回复会转发到对应的 QQ 会话。去重机制（前 100 字符比较）防止桥接服务自身生成的回复被重复转发。

### 发送者身份

消息格式遵循 Alma 渠道桥接协议（Telegram 风格）：

- 群聊：`[From: Alice | id:12345678]\n\n[msg:12345] 消息内容`
- 私聊：`[From: Bob | id:12345678]\n\n[msg:67890] 消息内容`
- 引用回复：`[From: Alice | id:12345678]\n\n[msg:12346] [Replying to Bob's message: "之前的话"]\n这是回复`

`[msg:N]` 使用真实的 OneBot 消息 ID。QQ 表情会转换为文本（如 `[emoji:斜眼笑]`），图片/语音/视频以标签描述。QQ 号作为稳定标识（昵称经常变动）。

## WebSocket 路径

桥接服务接受以下路径的 OneBot 连接：

- `/` — 通用
- `/ws` — NapCat / snowluma 默认路径
- `/onebot/v11/ws` — Lagrange 默认路径

## 开发

```bash
# Debug 构建
cargo build

# 完整调试日志运行
RUST_LOG=debug cargo run

# Release 构建
cargo build --release
```

详细技术文档（包括 Alma WebSocket 协议发现、事件时序、常见坑点）请参阅 [DEVELOPMENT_KNOWLEDGE_BASE.md](./DEVELOPMENT_KNOWLEDGE_BASE.md)。

## 许可证

[AGPL-3.0](./LICENSE) — GNU Affero General Public License v3.0
