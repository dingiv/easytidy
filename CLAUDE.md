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

## 架构规划：env 模块族收敛（待执行，2026-09-01 定）

「运行时环境适配」逻辑（宿主侧 env/路径探测注入 + 容器内启动时探测/配置生成）
现在散在 5 处，应收敛到 core 的 `env` 模块族，作单一事实源。

**现状分布**

宿主侧（创建时，core）：
- `flavor.rs` — `inject_passthrough` / `inject_gui_passthrough` / `inject_gpu_passthrough`（透传注入入口）
- `gui_passthrough.rs` — GUI 透传规则加载 + 执行（assets 资源驱动）
- `pathvars.rs` — `${HOME}` 等路径变量展开
- `userenv.rs` — `host_user()` 宿主用户探测
- `desktop.rs` — `xdg-user-dir` / `host_user_resource_dirs`

容器内（启动时，server 自持）：
- `server/src/setup.rs` — `ensure_xauthority`（XAUTHORITY 探测）、`fixup_xdg_data_dirs`（XDG_DATA_DIRS 修正）、`setup_user_identity`（身份自发现）、`finalize_injected_env`
- `incontainer.rs` — `resolve_identity`（身份单一事实源，已在 core）、fontconfig `local.conf` 生成、passwd/group 写入

**关键约束**

- 分属**两个进程、两个生命周期阶段**（宿主创建时 vs 容器内启动时），不能塞进一个大文件。
- server 有**进程级副作用**（`std::env::set_var`、`USER_MAP`/`injected_env` static）——这部分是 server 职责，**不下沉**。能下沉的只是**纯函数**（探测逻辑、值计算）。
- server 已依赖 core（`easytidy_core::incontainer::resolve_identity`），下沉方向（server → core）可行。

**目标结构**

```
core/src/env/
  mod.rs         —— 统一入口 + 共享类型（EnvRule / InjectedEnv / Identity 等）
  host.rs        —— 宿主侧：透传注入（gui/gpu）、pathvars 展开、host_user 探测
  incontainer.rs —— 容器内：身份自发现、XAUTHORITY 探测、XDG_DATA_DIRS 修正、fontconfig 生成
```

server 退成「调 core `env::incontainer` 纯函数 + 保留进程副作用薄壳」。

**分步执行（按风险递增）**

1. 先 `git commit` 当前未提交改动（/mnt/host 挂载根、GUI 透传外置 assets、用户资源映射等），与本轮重构隔离。
2. 低风险：把 `server/src/setup.rs` 的**纯函数**（`ensure_xauthority` 探测、`fixup_xdg_data_dirs_value`、身份自发现）下沉到 `core::env::incontainer`；server 保留 `set_var`/static 薄壳。
3. 归组宿主侧：把 `flavor` 注入入口 + `gui_passthrough` + `pathvars` + `userenv` 探测归到 `core::env::host`（主要命名空间整理，行为不变）。
4. 每步跑 `cargo check --workspace` + `cargo test -p easytidy-core -p easytidy-gui -p easytidy-server` 验证零回归。

**注意**：当前功能全绿，本轮到此为止不重构；另有一处非本轮遗留删除 `crates/gui/conf/chrome-copy.yaml` 提交前需确认。
