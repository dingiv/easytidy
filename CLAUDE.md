#

## 开发约定

### Tauri 命令参数必须用驼峰（camelCase）

前端 `invoke` 调用 `#[tauri::command]` 时，**参数名必须用驼峰**（与 Rust 侧 snake_case 自动对应）：

```ts
// ✓ 正确：Rust 参数 `snapshot_name` ← 前端 `snapshotName`
invoke('env_snapshot', { name, snapshotName: snapshotName || null });

// ✗ 错误：前端用 `snapshot_name` —— Tauri 按 camelCase 提取参数，
//   取不到 → Option 参数静默变 None（不报错！）
invoke('env_snapshot', { name, snapshot_name: snapshotName || null });
```

**踩坑记录（2026-08-31 快照命名丢失）：** 前端误传 snake_case 的 `snapshot_name`，
后端 `Option<String>` 静默收到 `None`，走了默认名兜底，用户输入的快照名丢失。
`Option` 参数收不到时**没有任何报错**，排查成本极高——传参前确认键名是驼峰。

（Tauri v2 `#[tauri::command]` 默认按 camelCase 解析 JS 侧参数，Rust 参数保持 snake_case 即可。）

### root-channel 容器内 spawn 的三个坑（2026-09-02 已修）

root-channel daemon 由 bootstrap 在容器内 `setsid` 拉起，**三个叠加坑**导致
"root 终端点了没反应、静默失败"：

1. **busybox `setsid` 无 `-f`**：`setsid -f` 在 busybox 直接报 `unrecognized option: f`
   退出，daemon 从未启动。正确形式 `setsid <exe> daemon`（setsid 自身 exec 成 daemon，
   天然新 session）。
2. **clap subcommand 不带 `--`**：`--daemon`/`--bootstrap` 非法；正确是 `daemon`/
   `bootstrap`（positional subcommand）。GUI/CLI 调用路径也要同步。
3. **`process_group(0)` 不可靠**：daemon 子进程继承 exec session，exec 流关闭被连带杀；
   必须 `setsid` 建全新 session。

另两个根因：
- **bind-mount 钉死 inode**：容器创建时 bind-mount 单文件钉住宿主 inode，cargo 重建换新
  inode 后**已有容器仍跑旧二进制**，必须重建容器。dev 改 root-channel 后记得重建容器。
- **GUI 丢 exec input 句柄**：`open_new_root_session` 曾 `let _input = exec.input` 直接 drop
  → client stdin 立即 EOF → client 退出 → 终端没反应。input 必须存入 `root_sink`。
- **exec 必须用容器内路径**：GUI/CLI 曾传宿主 `root_channel_binary_path()`（dev 相对路径
  `crates/core/../../target/...`），runc 在容器命名空间 stat 不到 → "no such file or
  directory: OCI runtime attempted to invoke a command that was not found"。一律 exec
  `Podman::ROOT_CHANNEL_TARGET`（`/usr/bin/easytidy-root-channel`），bind-mount 阶段才用
  宿主路径。
- **`rc.ping` 的 alive 语义**：daemon 曾把 alive 当作"是否有存活 session"，无 session 时返回
  false → GUI `probe_root_channel` 误判 daemon 未就绪 → "启动后 1s 内未就绪"误报。ping 能
  收到响应就说明 daemon 活着 → `alive` 恒 true；session 存活看 `rc.list`。
- **root 终端 attach 幂等**：`root_terminal_attach` 曾每次 `client new` 新建 session——React
  StrictMode dev 双 invoke / 断线重连会生成多个 root bash（`ps aux` 见多个 `client new` +
  多个 bash）。修复：持 `root_attach_lock` 串行「查 `client list` → 有 alive session 则
  `client attach <id>` 复用（daemon fan-out）、无则 `client new`」。保证每容器**一个** root
  bash。纯 GUI 改动，无需重建容器。
- **client 日志不进 stderr**：client 模式 stderr 被 podman exec 捕获桥进终端（残留日志
  污染）。`main.rs` 按模式决定：client 只写共享文件、daemon/bootstrap 才写 stderr。

**诊断手段**：daemon stderr 被 /dev/null，全部日志进容器内 `/run/easytidy/root-channel.log`
（daemon/bootstrap/client 都写）。查看：CLI `easytidy root-channel-logs --container <n>` 或
GUI `root_channel_logs` 命令 / bootstrap 失败时错误信息里附日志尾。

## 架构：core `env` 模块族（2026-09-01 已收敛）

「运行时环境适配」（env 探测 + 配置生成）统一收敛到 `core::env` 模块族，作单一
事实源。两个进程/两生命周期阶段不混：

```
core/src/env/
  mod.rs           —— 模块声明 + 便捷再导出（inject_passthrough / resolve_identity / Identity / ...）
  host.rs          —— 宿主侧（创建期）：inject_passthrough / inject_gui_passthrough / inject_gpu_passthrough
  gui.rs           —— GUI 透传规则（ASSETS_DIR::gui-passthrough.yaml 资源驱动：load_rule / apply）
  incontainer.rs   —— 容器侧：resolve_identity（server/ctool 共用身份单一事实源）
                      + self_uid_gid / fixup_xdg_data_dirs_value / probe_xauthority（纯探测）
                      + 容器内准备（passwd/group/home/fontconfig，ctool 执行）
```

**关键约束**（重构时守住的边界）：
- 宿主进程（GUI/CLI 创建容器）→ `env::host` / `env::gui`；容器内进程（server/ctool
  启动）→ `env::incontainer`。
- server 的**进程级副作用**（`std::env::set_var`、`USER_MAP`/`INJECTED_ENV` static、
  `finalize_injected_env`）留在 `server/src/setup.rs`，**不下沉**；server 只调 core 纯
  函数 + 做 set_var 薄壳（`ensure_xauthority` / `fixup_xdg_data_dirs`）。
- `userenv`（宿主用户探测）、`pathvars`（路径变量展开）作为支撑工具保持独立（非 env
  生成主体），按需被 host / incontainer / podman 引用。

**新增 env 逻辑的去处**：宿主侧生成 → `env/host.rs`（或 `env/gui.rs` 若资源驱动）；
容器侧探测 → `env/incontainer.rs`（纯函数），server 侧只做副作用薄壳。
