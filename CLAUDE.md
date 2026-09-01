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
