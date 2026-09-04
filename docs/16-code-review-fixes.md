# 16 — 代码检视修复清单（2026-09-05）

对 easytidy 全项目（core / dock / server / gui 后端 / 前端）做一轮检视后，
按严重度排序的 9 项问题及修复方案。前两项（#1 #2）是 `dock` crate 的实质
缺陷，其余为文档过时 / 边界 / 卫生问题。

| # | 严重度 | crate | 一句话 |
|---|--------|-------|--------|
| 1 | 中 | dock | daemon 退出路径不杀 bash 进程组，可能残留 root bash |
| 2 | 中 | dock | `rc.close` 两处语义不一致 + `kill()` 注释与实现矛盾 |
| 6 | 低 | dock | bootstrap/daemon 并发启动竞态（后启动者删先启动者 socket） |
| 5 | 低 | core | `.desktop` INI 值未清理换行，含换行的 app_name/title 破坏解析 |
| 3 | 低 | core | `userenv.rs` 文档描述已移除的 su 模型 |
| 4 | 低 | core | `dedup_mounts` 函数级 doc 与实现相反（先 vs 最后） |
| 7 | 低 | gui | `Terminal.tsx` 残留大量 `[DBG-Term]` 调试 log |
| 8 | 低 | gui | `Terminal.tsx` `memo` 注释「无 props」与实际 3 props 不符 |
| 9 | 低 | dock | `rc.resize` 注释「唯一 alive」与实现「首个 alive」不符 |

---

## #1 daemon 退出路径不杀 bash 进程组

**定位**：`crates/dock/src/daemon.rs:133-143`（SIGTERM/SIGINT 退出清理）

**现状**：
```rust
// 清理
cleanup_task.abort();
let sessions = state.sessions.read().await;
for s in sessions.values() {
    s.set_dead();          // 仅标记 alive=false
    s.broadcast_exited();  // 仅通知 client
}
drop(sessions);
let _ = tokio::fs::remove_file(DAEMON_SOCKET).await;
```

**根因**：退出时只 `set_dead()`（标记）+ `broadcast_exited()`（通知），**没有
`kill()`（SIGHUP 到 bash 进程组）**。daemon 是 bash 的直接父进程（portable-pty
spawn），daemon 死后 bash 变孤儿被容器 PID 1（catatonit）收养。

**影响**：实际靠「容器停止 = runc 向整个 cgroup 发 SIGKILL」兜底，所以**容器正常
停止时不残留**。但若 daemon 被单独 kill（容器仍存活，如 `kill <daemon_pid>` 或
daemon 崩溃），root bash 进程组残留为孤儿——占 PTY / fd，下次 `client list` 仍
显示 stale session（虽然 alive 已 false，但 bash 进程还在跑）。

**修复**：退出清理循环里对每个 session 调 `s.kill()`（发 SIGHUP 到 bash 进程组），
再 `set_dead()` + `broadcast_exited()`。与 `rc.close` 路径保持一致。

**验证**：`cargo test -p easytidy-dock`（session/daemon 既有单测）；人工验证可选——
容器内 `kill <daemon_pid>` 后 `ps aux | grep bash` 应无残留 root bash。

---

## #2 `rc.close` 两处语义不一致 + `kill()` 注释矛盾

**定位**：`crates/dock/src/daemon.rs`

**问题 A（语义不一致）**：`rc.close` 有两处处理，行为不同：
- 顶层 `handle_client`（`daemon.rs:347-375`）：`sessions.remove()` + `kill()` +
  立即 `broadcast_exited()`（**同步清理**，map 立即移除）。
- `attach_session` 内（`daemon.rs:466-469`）：只 `session.kill()`（**异步**——
  依赖 reader task 读到 EOF → `set_dead()` → SIGCHLD 触发 `reap_dead_sessions`
  清理 map + broadcast）。

同一条 `rc.close` 命令，从顶层发（standalone client close）vs 从 attach 桥流发
（GUI 终端 ✕ 按钮），清理时机/方式不同。standalone 立即清，attach 路径延迟清。

**问题 B（注释矛盾）**：`attach_session` 上方注释（`daemon.rs:438-439`）写：
> rc.close 杀 bash + 杀 daemon……kill 只标记 alive=false；bash 由 reader EOF
> 触发 set_dead

但 `RootSession::kill()`（`session.rs:188-201`）实际是 `killpg(SIGHUP)`（**真发
信号杀 bash 进程组**）+ `set_dead()`。注释说「kill 只标记 alive=false」与实现
矛盾——`kill()` 是「发 SIGHUP 杀进程组 + 标记」，不是「只标记」。

**修复**：
- 问题 B：改写 `attach_session` 注释，准确描述 `kill()` = `killpg(SIGHUP)` 杀
  bash 进程组 + `set_dead()` 标记；reader task 读到 EOF 后 broadcast + 由
  SIGCHLD 收割 zombie。
- 问题 A：统一两处语义。attach 路径的 `rc.close` 也改为「同步清理」——调用
  `kill()` 后由 daemon 主动从 map remove + broadcast（不依赖 reader EOF 时序），
  与顶层路径一致。保留 reader task 的 EOF 处理作为兜底（幂等：已 remove 的
  session 的 broadcast 无害）。

**验证**：`cargo test -p easytidy-dock`；GUI 终端 ✕ 按钮后 `client list` 应立即
空（不延迟）。

---

## #6 bootstrap/daemon 并发启动竞态

**定位**：`crates/dock/src/bootstrap.rs` + `crates/dock/src/daemon.rs:60-70`

**现状**：`run_daemon` 开头无条件 `remove_file(DAEMON_SOCKET)` 再 `bind`。两个
bootstrap 并发（React StrictMode 双 invoke / 用户快速连点）都 spawn daemon →
后启动的 daemon `remove_file` 会**删掉先启动 daemon 正在用的 socket**，然后
`bind` 占位 → 先启动 daemon 的 socket 文件被替换，新 client 连到后启动 daemon，
先启动 daemon 的 session 不可达（其 socket 已不在）。

**影响**：低——prod 无 StrictMode；`root_terminal_attach` 里 bootstrap 虽在
`root_attach_lock` 之外，但单容器模式 + 实际触发频率低。仍是真实竞态。

**修复**（`run_daemon` 启动防护，最小改动）：
1. 启动先 `connect(DAEMON_SOCKET)`：**可连 = 已有活 daemon → 本实例直接退出**
   （不 remove、不 bind）。
2. `bind` 失败时若 `ErrorKind::AddrInUse` → 说明另一 daemon 刚 bind 完 → 本实例
   退出（不 continue）。
3. `remove_file` 仅在 connect 失败（stale socket）后执行。

时序安全：A 已 bind（在跑）→ B connect 成功 → B 退出，不删 A socket；A 未 bind
→ B 删的是 stale/空，竞争 bind，输者退出。

**验证**：`cargo test -p easytidy-dock`；并发场景可选——同时跑两个 `bootstrap`，
最终只一个 daemon 存活，socket 可连。

---

## #5 `.desktop` INI 值未清理换行

**定位**：`crates/core/src/desktop.rs` `generate_desktop_entry`（34-50）/
`generate_passthrough_content`（381-434）/ `generate_gui_entry_content`（464-486）

**现状**：`app_name` / `title` / `comment` 等直接插值进 `.desktop` 的 INI 值
（`Name=` / `Comment=` / `GenericName=` / `Keywords=`）。若含换行 `\n`，INI
结构被破坏（`Name=` 后跟一行 → 该行被当作新键值或非法行）。

**来源**：`app_name` 来自容器内 `.desktop` 的 Name 字段（用户/应用可自定义，
理论上可含换行/控制字符）；`title` 来自调用方。容器名（`--container`）受 podman
字符集限制（`[a-zA-Z0-9][a-zA-Z0-9_.-]*`），安全；但 title/app_name 不受限。

**修复**：新增纯函数 `sanitize_ini_value(s) -> String`：把 INI 值里的换行
（`\n` / `\r\n` / `\r`）替换为空格，去除首尾空白，控制字符（`\0`-`\x1f`）丢弃。
应用于所有 INI 值字段（Name/Comment/GenericName/Keywords/Icon）。Exec 行是
shell 命令行（含空格合法），不处理。

**验证**：`cargo test -p easytidy-core`（新增 `sanitize_ini_value` 单测：含换行 /
纯空白 / 正常值）；`generate_passthrough_content` 既有测试不回归。

---

## #3 `userenv.rs` 文档过时（su 模型已移除）

**定位**：`crates/core/src/userenv.rs:1-5`

**现状**：
> 容器创建时…经 `EASYTIDY_USER_*` 环境变量注入容器；容器内 server 据此创建同名
> 用户并以 `su` 以该用户拉起应用——避免容器内 root 读写宿主挂载目录的权限问题。

**根因**：root/su 模型已移除（`server/src/setup.rs` 明确：server 即容器默认用户，
容器 `User` 字段直指配置 uid:gid，无降权/无 su）。本处模块 doc 仍描述旧模型。

**修复**：改写模块 doc，描述新模型——server 即容器默认用户，`User` 字段直指
配置 uid:gid，无 su/降权；`host_user()` 提供宿主身份供 keep-id / 路径变量展开
（`pathvars`）/ GUI 用户面板展示。

**验证**：`cargo test -p easytidy-core`（doc 改动，无逻辑变更）。

---

## #4 `dedup_mounts` 函数级 doc 与实现相反

**定位**：`crates/core/src/podman/mod.rs:480-487`（函数级 doc）vs 488-520（实现）

**现状**：函数级 doc 说「按 `container_path` 去重，**保留先出现的**」，但实现
（`reverse` + `retain`）和函数体内注释（491-494）是「**保留最后出现的**」
（2026-09-02 修复：用户手动项在最后，应覆盖模板同路径项）。

**根因**：2026-09-02 修复逻辑时更新了函数体注释和实现，漏更新函数级 doc。

**修复**：函数级 doc 改为「保留**最后**出现的一项（用户手动项在最后 → 覆盖模板
同路径项）」，与实现和体注释一致。

**验证**：`cargo test -p easytidy-core`（doc 改动，无逻辑变更；`test_dedup_mounts_*`
既有测试确认行为）。

---

## #7 `Terminal.tsx` 残留 `[DBG-Term]` 调试 log

**定位**：`crates/gui/ui/components/Terminal.tsx`

**现状**：多处 `console.log('[DBG-Term] …')`（约 190/195/205/237/250/305 等行），
是排障期调试输出，生产代码不应保留（每键输入都 log，量大且污染 console）。

**修复**：删除所有 `[DBG-Term]` `console.log`。保留 `console.error` /
`console.warn`（真错误路径）。

**验证**：`pnpm exec tsc --noEmit`。

---

## #8 `Terminal.tsx` `memo` 注释「无 props」与实际不符

**定位**：`crates/gui/ui/components/Terminal.tsx:436-437`

**现状**：
```tsx
/** memo 包裹：无 props，父组件重渲染不触发重建 */
export const Terminal = memo(TerminalInner);
```
但 `TerminalInner` 有 3 个 props（`streamId` / `onStream` / `onExit`）。memo 的
实际作用是：当父组件重渲染但 props 引用不变时跳过重建（`onStream`/`onExit` 是
回调，若父每次渲染都新建则 memo 失效——取决于父的用法）。

**修复**：注释改为准确描述——memo 按 props（含回调引用）比较，props 不变时
跳过重建；实际是否生效取决于父组件是否稳定传回调。

**验证**：`pnpm exec tsc --noEmit`。

---

## #9 `rc.resize` 注释「唯一 alive」与实现「首个 alive」不符

**定位**：`crates/dock/src/daemon.rs:378-392`

**现状**：`rc.resize` 注释说「找**唯一** alive session」，实现
`sessions.values().find(|s| s.alive())` 是「**首个** alive」（`.find` 取第一个
命中）。单 session 场景两者等价；多 session 时语义是「改第一个 alive 的」。

**修复**：注释改为「找**首个** alive session」（当前产品每容器单 root session，
多 session 时 resize 作用于首个）。

**验证**：`cargo test -p easytidy-dock`（doc 改动，无逻辑变更）。

---

## 验证总览

每项修复后跑：
- `cargo build --workspace`
- `cargo test --workspace -- --test-threads=1`（串行，规避 `XDG_RUNTIME_DIR` 并发 flaky）
- `cargo clippy --workspace --all-targets`（无新增 warning）
- `pnpm exec tsc --noEmit`（前端类型）

涉及容器内二进制的改动（#1 #2 #6 #9 在 `dock`）需 `cargo build -p easytidy-dock
--target x86_64-unknown-linux-musl` 后重建容器才生效；core/gui 改动重启进程即可。
