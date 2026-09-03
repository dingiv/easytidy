#

## 开发约定

### 挂载去重 / 模板展开不丢手动挂载（2026-09-02）

「从模板创建 + 手动加挂载，创建后手动挂载没生效」三层修复：

1. **前端模板展开合并手动挂载**：`ContainerCreateForm.handleTemplateSelect` 曾
   `setConfig(expanded)` 整体覆盖 → 用户在表单里手动加的 mount 被模板重新展开静默
   抹掉。修复：展开后按 container_path 合并——模板已声明的以模板为准，其余保留
   用户手动项。模板 Select 也改用文件 stem（`t.id ?? t.name`）作身份。
2. **gui.rs 注入判重按展开后路径**：原实现拿规则原文（`${XDG_RUNTIME_DIR}`）与已
   展开的 `/run/user/1000` 比对恒不等 → 注入重复项 → 后续被 dedup 静默丢。
   修复：两侧展开后再判重。回归测试 `apply_skips_existing_expanded_target`。
3. **`dedup_mounts` 保留最后一项**：挂载顺序 = [模板…, GUI 注入…, 用户手动…]，
   用户手动在最后。同 container_path 去重时保留最后 → 用户手动覆盖模板生效（原来
   保留先出现的，模板/GUI 静默压过用户）。测试改 `test_dedup_mounts_keeps_last`。

backend create 路径实测（含 keep-id / gui / host|mapped 网络 / 模板+手动挂载）全部
正确落挂载；问题出在「模板展开覆盖」与「注入/去重顺序」。

### 模板身份 = 文件 stem，不是 config.name（2026-09-02）

conf 模板的稳定身份是**文件名 stem**（`chrome.yaml` → `chrome`），**不是** YAML 内
`config.name`（= 默认容器名）。

- 曾踩坑：`conf_duplicate_template` 逐字节拷贝 → `chrome-copy.yaml` 内部 `name: chrome`
  仍与源相同 → 列表两个「chrome」、删「带 copy 的」按名定位删到**源文件**。
- 修复：`ConfTemplateInfo` 新增 `id`（文件 stem）；GUI 增删改/展开/血缘一律用 `id`
  （`tplId(t) = t.id ?? t.name`）；后端 `ConfTemplateStore` 全量按 stem 定位并加
  `stem()` 规范化（剥 `.yaml`/`.yml`）；**复制时改写内部 `config.name` = 新 stem**
  （复制品「使用」预填容器名不冲突）。
- 编辑器 `nameLocked`（编辑既有模板禁改名）保证 `config.name` == stem 不分离；
  唯一破坏者就是复制（已修）。
- 回归测试：`test_delete_by_stem_does_not_hit_source` /
  `test_duplicate_rewrites_internal_name_to_stem`（tempdir 注入，不碰真实 conf 目录）。

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

### easytidy-dock 容器内 spawn 的三个坑（2026-09-02 已修）

root 终端通道 daemon 由 bootstrap 在容器内 `setsid` 拉起，**三个叠加坑**导致
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
  inode 后**已有容器仍跑旧二进制**，必须重建容器。dev 改 easytidy-dock 后记得重建容器。
- **GUI 丢 exec input 句柄**：`open_root_session` 曾 `let _input = exec.input` 直接 drop
  → client stdin 立即 EOF → client 退出 → 终端没反应。input 必须存入 `root_sink`。
- **exec 必须用容器内路径**：GUI/CLI 曾传宿主 `dock_binary_path()`（dev 相对路径
  `crates/core/../../target/...`），runc 在容器命名空间 stat 不到 → "no such file or
  directory: OCI runtime attempted to invoke a command that was not found"。一律 exec
  `Podman::DOCK_TARGET`（`/run/easytidy-bin/easytidy-dock`），bind-mount 阶段才用
  宿主路径。
- **`rc.ping` 的 alive 语义**：daemon 曾把 alive 当作"是否有存活 session"，无 session 时返回
  false → GUI `probe_dock` 误判 daemon 未就绪 → "启动后 1s 内未就绪"误报。ping 能
  收到响应就说明 daemon 活着 → `alive` 恒 true；session 存活看 `rc.list`。
- **root 终端 attach 幂等**：`root_terminal_attach` 曾每次 `client new` 新建 session——React
  StrictMode dev 双 invoke / 断线重连会生成多个 root bash（`ps aux` 见多个 `client new` +
  多个 bash）。修复：持 `root_attach_lock` 串行「查 `client list` → 有 alive session 则
  `client attach <id>` 复用（daemon fan-out）、无则 `client new`」。保证每容器**一个** root
  bash。纯 GUI 改动，无需重建容器。
- **client 日志不进 stderr**：client 模式 stderr 被 podman exec 捕获桥进终端（残留日志
  污染）。`main.rs` 按模式决定：client 只写共享文件、daemon/bootstrap/prepare 才写 stderr。

### root 终端生命周期语义（detach vs close，2026-09-02）

- **detach**（新按钮 / 关面板断连）：只断开面板与 root 会话的桥（drop exec input →
  client 退出），**后台 bash 会话保留**在 daemon，可重新 attach 复用。GUI = `root_terminal_detach`。
- **close**（✕ 按钮）：**真杀会话**。GUI = `root_terminal_close` → 断开桥 + `client close
  --session-id <sid>` → daemon `rc.close` → `session.kill()` 发 **SIGHUP 到 bash 进程组**
  （bash 是 PTY slave 的 session leader，PGID==PID）→ session 从 map 移除。此前 `kill()`
  只置 dead 不真杀（bash 残留）。
- 验证：close 后 `client list` 空、无 pts bash；detach 后 session 仍 alive。
- **zombie 收割**：daemon 是 bash 的直接父进程（portable-pty spawn），bash 被 SIGHUP
  kill 后若 daemon 不 waitpid 会残留 `<defunct>` zombie。SIGCHLD 处理器现在先
  `waitpid(-1, WNOHANG)` 循环收割所有子进程，再清理 session map。
- **client 进程残留（close 后不退出）**：client 桥循环结束（收到 rc.exited）后，靠
  tokio runtime 自然退出会卡住——`tokio::io::stdin()` 的阻塞读线程不随 runtime 回收，
  进程 hang 在 `futex_do_wait`（实测日志已打 "main returning" 仍不退出）。修复：client
  模式桥结束后**显式 `std::process::exit()`**（0 成功 / 1 失败）。daemon/bootstrap 保持
  自然返回（长驻/一次性，无 stdin 桥）。
- 重建容器注意：easytidy-dock 二进制改动后，需 `cargo build -p
  easytidy-dock --target x86_64-unknown-linux-musl` 后**重建容器**（bind-mount 钉
  住 inode）。GUI/协议（`rc.rs` RcCloseReq）改动只需重启 GUI。

### root 终端提示符 wrap：关闭 worker GUI 后再次进入排版错乱（2026-09-03）

症状：worker GUI 里开着 root 终端 → 整个 worker GUI 窗口关掉 → 重开
worker GUI（自动恢复 root pane）→ root 终端的 bash 提示符从中间折断
显示成 `root ➜ ~` 换行 ` $ `，并且 `ls` 等输出也跟着按错误宽度换行。

两层根因：

1. **`root_terminal_resize` 是 no-op**：`commands/root.rs` 该命令原本只
   `Ok(())` 占位（注释留了 TODO）。xterm.fit() 改完 `term.cols` 后调
   backend 想同步 PTY 大小，结果什么也没发生——bash 继续按上次 attach
   时的宽度排版。关 worker GUI 重开时，新 xterm 首次 fit 拿到偏小
   cols，attach 把小 cols 推给 daemon 改了 PTY，bash 立即按小宽度
   重绘提示符并 wrap；后续 ResizeObserver 二次 fit 把 xterm 调大也
   不回溯已渲染内容，bash 也没新 SIGWINCH 触发，wrap 视觉持续。

2. **初始 fit 取到过渡态 cols**：WorkerView 挂载瞬间 `main-panel` 从
   `pane-empty` 切到含 RootTerminal 的 `tab-pane`（`height: 100%`），
   flex 链 `.app > .per-layout > .per-right > .main-panel > .tab-pane
   > .terminal-wrapper > .terminal-container` 在算高度。useEffect 同步
   `fitAddon.fit()` 时容器可能还在 `0 → 部分 → 全` 过渡，拿到偏小值
   立刻传给 attach。attach 走的是「**已有 session** → 改 PTY 尺寸 +
   回放历史 ring」路径——历史 ring 在新宽度下渲染就 wrap，xterm 不
   回溯，bug 持久化。

修复（两层都堵）：

- **Backend（dock + GUI）**：
  - `dock/client.rs`：`ClientCmd` 加 `Resize`，独立 exec 一句话发
    `rc.resize` 到 daemon 后退出（不进入桥流）。
  - `dock/daemon.rs::handle_client`：顶层加 `rc.resize` 分支（找唯一
    alive session → `master.resize` → 内核给 bash 进程组 SIGWINCH →
    bash 重绘）。attach 内消息循环的 `rc.resize` 分支保持原样。
  - `commands/root.rs::root_terminal_resize`：从 no-op 改为 spawn
    `podman exec easytidy-dock client resize --cols X --rows Y`，失
    败 best-effort 仅记 warning（resize 不同步不阻断 GUI 输入/输出）。
- **Frontend（RootTerminal.tsx）**：
  - `term.open` 后**不立即** `fitAddon.fit()`；把首次 `attach(null)`
    推到双 rAF + 150ms 防抖后（同 ResizeObserver 节奏），attach 前
    再 fit 一次取稳定 cols。
  - 卸载时清理 `initialAttachTimer`，避免 unmount 后回调触发已
    dispose 的 term。

验证：关 worker GUI → 重开 → root 终端提示符单行不折；手动拖窗口
改宽度 → bash 提示符实时按新宽度重绘（SIGWINCH 路径走通）。

为什么用户终端（Terminal.tsx）同款 fit 时序问题没爆出来：用户终端
attach 的是 **新会话**（或死会话换新），没有遗留 ring 内容；新会话
创建时 bash 第一次画 prompt 就以新 cols 出，加上 server 端
`pty_resize` 是真活，SIGWINCH 自动重绘，wrap 视觉不会持久化。root
走 attach 到已有 session + 回放历史 ring——wrap 视觉持久化，必须
「初始 fit 取稳定值 + 后续 resize 真同步 PTY」双管齐下。

重建容器注意：`easytidy-dock` 改动（client Resize / daemon 顶层
rc.resize）需要 `cargo build -p easytidy-dock --target
x86_64-unknown-linux-musl` 后重建容器（bind-mount 钉 inode）。
`commands/root.rs::root_terminal_resize` 与 `RootTerminal.tsx` 改动
只需重启 GUI。

## 容器内二进制：server + easytidy-dock（2026-09-02）

容器内两个二进制（server + **easytidy-dock**）bind-mount 到 **`/run/easytidy-bin/`**
（tmpfs，学 podman-init 的 `/run/podman-init`）。进程命令行统一归到 `/run` 下，
容器镜像不污染 `/usr/bin`。

- **2026-09-02 合并**：`easytidy-root-channel`（root 终端通道 daemon/client/bootstrap）
  与 `easytidy-ctool`（容器准备 prepare）合并为单二进制 **`easytidy-dock`**。子命令：
  `prepare` / `daemon` / `bootstrap` / `client {new|attach|ping|list|close}`。
  两个旧 crate 删除，新 crate `crates/dock`（git mv crates/root-channel）。
- `Podman::SERVER_TARGET` / `DOCK_TARGET` / `BIN_DIR`
- **不是** `/run/easytidy/bin`——`/run/easytidy` 已被宿主 socket 目录 bind-mount 占住，
  bin 放其下会落成宿主侧残留文件。
- exec 一律用 `DOCK_TARGET`（容器内路径），bind-mount 阶段才用宿主路径。
- 宿主侧二进制安装位（`/usr/bin`）与容器内 `/run/easytidy-bin` 无冲突，`Cargo.toml`
  BIN namespace 的 prod `/usr/bin` 指**宿主**安装位置，不变。core Cargo.toml namespace：
  `SERVER_BIN` / `DOCK_BIN`（原 CTOOL_BIN + ROOT_CHANNEL_BIN 合并）。
- 内部 socket 改名为 `/run/easytidy/dock.sock`，日志文件 `/run/easytidy/dock.log`。

**诊断手段**：daemon stderr 被 /dev/null，全部日志进容器内 `/run/easytidy/dock.log`
（daemon/bootstrap/client 都写）。查看：CLI `easytidy dock-logs --container <n>` 或
GUI `dock_logs` 命令 / bootstrap 失败时错误信息里附日志尾。

## 架构：core `env` 模块族（2026-09-01 已收敛）

「运行时环境适配」（env 探测 + 配置生成）统一收敛到 `core::env` 模块族，作单一
事实源。两个进程/两生命周期阶段不混：

```
core/src/env/
  mod.rs           —— 模块声明 + 便捷再导出（inject_passthrough / resolve_identity / Identity / ...）
  host.rs          —— 宿主侧（创建期）：inject_passthrough / inject_gui_passthrough / inject_gpu_passthrough
  gui.rs           —— GUI 透传规则（ASSETS_DIR::gui-passthrough.yaml 资源驱动：load_rule / apply）
  incontainer.rs   —— 容器侧：resolve_identity（server/dock 共用身份单一事实源）
                      + self_uid_gid / fixup_xdg_data_dirs_value / probe_xauthority（纯探测）
                      + 容器内准备（passwd/group/home/fontconfig，dock prepare 执行）
```

**关键约束**（重构时守住的边界）：
- 宿主进程（GUI/CLI 创建容器）→ `env::host` / `env::gui`；容器内进程（server/dock
  启动）→ `env::incontainer`。
- server 的**进程级副作用**（`std::env::set_var`、`USER_MAP`/`INJECTED_ENV` static、
  `finalize_injected_env`）留在 `server/src/setup.rs`，**不下沉**；server 只调 core 纯
  函数 + 做 set_var 薄壳（`ensure_xauthority` / `fixup_xdg_data_dirs`）。
- `userenv`（宿主用户探测）、`pathvars`（路径变量展开）作为支撑工具保持独立（非 env
  生成主体），按需被 host / incontainer / podman 引用。

**新增 env 逻辑的去处**：宿主侧生成 → `env/host.rs`（或 `env/gui.rs` 若资源驱动）；
容器侧探测 → `env/incontainer.rs`（纯函数），server 侧只做副作用薄壳。
