# 15. Socket 协议 v1（GUI ↔ 容器 server）

> 规范化后的前后端通信契约（2026-08-25 定稿）。GUI/CLI 为**前端**，容器内
> `easytidy-server` 为**后端**。线格式沿用既有实现：长度前缀分帧 + 1 字节判别符
> （`0x01` JSON 消息 / `0x02` 原始流），见 `crates/protocol`。

## 1. 连接状态机（客户端）

前端对后端的连接有显式三态，驱动所有请求门控：

```
        连接请求              握手成功
Unconnected ──────▶ Connecting ──────▶ Connected
      ▲                │                  │
      └── 失败/断开 ────┘                  │
      └──────────────── 断开 ──────────────┘
```

| 状态 | 含义 | 转换 |
|---|---|---|
| `Unconnected` | 初始；未连接 / 连接失败后 | 发起连接 → `Connecting` |
| `Connecting` | socket connect + 握手进行中 | 握手成功 → `Connected`；失败/超时 → `Unconnected` |
| `Connected` | 握手完成，可发请求/监听推送 | 传输断开 → `Unconnected`（重连走 `Connecting`） |

- 客户端并发安全：`GuiSession.socket` 持 tokio Mutex 串行化「连接中」，同一时刻
  只有一个请求在建立连接。
- 断线重连：请求遇传输错 → 丢弃旧连接回 `Unconnected` → 再次 `Connecting` 重连
  重试一次（幂等请求场景）。

## 2. 握手与会话

前端连接后第一帧必须是 `hello`：

```
前端 ── hello{Handshake: v, client, wants} ──▶ 后端
前端 ◀── resp hello{HandshakeAck: v, server, capabilities, session_id} ── 后端
```

- `Handshake.v`：协议版本（当前 `1`）。不匹配 → `version_mismatch` 错误。
- `Handshake.wants`：前端申请的能力列表（`pty` / `fs` / `apps` / `passthrough` /
  `config` / `lifecycle`）。
- **`session_id`**：后端为**每个连接**创建（UUID v4）并随 ack 返回，前端凭此标识
  该会话。旧后端（无此字段）→ 空串，前后端版本互解析不破。
- 握手完成前发其它 op → `not_handshaked` 错误。
- 后端的「会话」即连接本身：断开即释放该连接的资源。

## 3. 消息模型

所有 JSON 帧承载 `Message`：

```jsonc
{ "id": 1, "kind": "req", "op": "pty.open", "payload": {…}, "err": null }
```

| 字段 | 语义 |
|---|---|
| `id` | 请求↔响应关联键（客户端递增；事件由服务端生成） |
| `kind` | `req` 请求 / `resp` 响应 / `evt` 服务端主动推送 |
| `op` | 操作名（`hello` / `pty.open` / `fs.read` / `apps.list` …） |
| `payload` | 各 op 的载荷（见 `crates/protocol/src/ops.rs`） |
| `err` | 响应错误（`{code, message}`；`op_failed` 等）——错误是**数据回包**，不断连 |

请求/响应模式：前端发 `req`，后端回同 `id` 的 `resp`。handler 失败转显式
`err` 响应（带根因），**不断连**——客户端解析 `err` 而非把传输层断开当业务错。

## 4. 双向信道（PTY 终端）

普通连接是「请求-响应」。**特殊前端**（终端面板）可向后端申请**双向信道**：
一条独立连接上，后端持续**反向推送**输出，前端可回写输入。

```
终端面板                         容器 server
    │  专用连接 connect + hello（同第 2 节握手）   │
    │  pty.open{persistent/attach} ────────────▶ │  创建/附接会话
    │ ◀── resp{stream_id}                        │
    │  ──── Raw{stream_id, input} ─────────────▶ │  输入 → PTY master
    │ ◀── Raw{stream_id, output} ─────────────── │  PTY 输出 → 订阅连接
    │ ◀── Evt pty.exited / pty.cwdChanged ────── │  生命周期/事件推送
```

- 信道语义：`pty.open{persistent}` 新建独立持久会话（不随连接断开清理）；
  `pty.open{attach_stream}` 附接既有会话（server 清屏 + 环形缓冲回放，供重开窗口
  恢复）；`attach` 登记为身份默认终端。**幂等由客户端保证**：一个面板一次
  `pty.open`，remount 只 attach 不复建（见 `Terminal.tsx` 的 StrictMode 处理）。
- 输入：前端把按键编码成 `Raw{stream_id, bytes}` 沿同一条连接发回，后端写入
  PTY master。
- 输出：后端 reader 线程读 PTY，经 `Raw{stream_id, bytes}` 帧广播给该会话的所有
  订阅连接；前端 reader 任务收到后经 Tauri `Channel` 反向送到 xterm 渲染。
- 事件：`pty.exited{stream_id, code}`、`pty.cwdChanged{stream_id, cwd}` 由后端主动推
  `Evt`。

## 5. 资源生命周期（后端）

后端对每个连接维护会话状态，断开即回收：

| 资源 | 断开时行为 |
|---|---|
| 连接自身 | 关闭；`Client disconnected` 日志 |
| PTY 订阅 | 按连接 token 退订该连接的会话订阅（`退订 N 个常驻终端`） |
| 本连接打开的 PTY 会话 | 非持久 → remove + 关闭 writer → EOF → 回收子进程 |
| 持久会话（persistent/attach） | **保留**（不随连接断开清理；供重开窗口 attach） |
| 空闲超时 | 无 PTY 连接 30s；有 PTY 连接放宽到 1h（交互 shell 长时间无输入） |

## 6. 实现对照

| 环节 | 位置 |
|---|---|
| 分帧/消息/载荷 | `crates/protocol/src/{frame,message,ops}.rs` |
| 后端连接循环 + 会话 ID | `crates/server/src/connection.rs` |
| 路由/握手 | `crates/server/src/router.rs` |
| PTY 服务（信道） | `crates/server/src/services/pty.rs` |
| 客户端连接 + 状态机 | `crates/gui/src/commands/socket.rs`、`crates/gui/src/state.rs` |
| 前端信道消费 | `crates/gui/ui/components/Terminal.tsx` |
