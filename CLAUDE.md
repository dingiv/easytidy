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

### 启动容器时清空 image 继承的 labels（2026-09-03）

症状：`podman inspect <container>` 看到容器带一堆镜像继承的 labels：

```
"Labels": {
    "devcontainer.config_file": "/home/div/.devcontainer/devcontainer.json",
    "devcontainer.local_folder": "/home/div",
    "devcontainer.metadata": "[{\"mounts\":[...],\"containerEnv\":{\"DISPLAY\":\":0\",...}}]",
    "easytidy.name": "chrome",
    "io.buildah.version": "1.39.3",
    "manager": "easytidy"
}
```

这些 label 里 `devcontainer.*` 含宿主绝对路径 + env + 端口，`io.buildah.version`
是镜像构建机残留——都不是 easytidy 想往容器上挂的。`easytidy.name` /
`manager=easytidy` 是 easytidy 自己的（识别 / GUI 维护用），要保留。

**关键坑（2026-09-03 实测）**：podman 5.4.2 libpod SpecGenerator 的
`unsetLabels` 字段**未生效**（直接 HTTP 调用，image labels 仍透传）；
libpod CLI 也无 `--unsetlabel` flag。第一版按 unsetLabels 思路实现，
GUI 拉新容器实测 labels 完整保留，**无效**。

**唯一可控路径**：把 image labels 的 keys 用**空串**覆盖——label key
仍存在但值清空，敏感内容（路径 / env / 端口）消失；随后 easytidy 自己的
labels（manager / easytidy.name）正常写入，互不干扰。实测 podman 5.4.2
HTTP API 直调 + libpod SpecGenerator 均生效：

```jsonc
// image Config.Labels: {"devcontainer.config_file": "/path/...", ...}
// create body: labels: {
//   "devcontainer.config_file": "",  ← 空串覆盖
//   "devcontainer.local_folder": "",
//   "devcontainer.metadata": "",
//   "io.buildah.version": "",
//   "manager": "easytidy",             ← easytidy 自己
//   "easytidy.name": "chrome"
// }
// podman inspect 容器 Labels: 同上（key 保留，敏感值清空）
```

修复：

- `core/src/podman/mod.rs::create_with_config`：`image_exists` 之后多调一次
  `inspect_image`，从 `img.config.labels` 取 key 列表；用空串构造一个
  `HashMap<key, "">`（占位 image labels），随后 `labels.insert("manager", "easytidy")`
  / `labels.insert("easytidy.name", name)` 用真值覆盖自己的两个 key。
- `keep_id_create_body` 签名不变（labels map 一路下传即可）。
- inspect 失败 best-effort：warn 后用空 map（容器照常创建，仅 image labels 保留）。
- 重建（`rebuild` → `create_with_config`）路径自动复用同一逻辑。

测试：新增 `test_libpod_body_image_labels_override`：构造 image 4 个
key 空值 + easytidy 2 个 key 真值，验证 body labels 字段全部正确。
（第一版 `unsetLabels` 测试已删除。）

验证：`podman inspect <container>` 后容器 Labels 的 `devcontainer.*` /
`io.buildah.*` 值为空串，敏感内容清空；easytidy 自己的 labels 不受影响。

**已知限制**：label key 仍存在（仅值清空）。podman 5.4.2 无 API 真正
删除 label key（CLI 无 `--unsetlabel`，SpecGenerator 的 `unsetLabels`
字段被忽略，post-create 也无修改 label 的子命令）。若以后 podman 修了
这一路，可以无缝切回 unsetLabels（仅移除 `for k in img_labels.keys()`
那行 + 复用之前的 `unsetLabels` 字段）。

重建容器注意：本改动只动 host 端 core（podman/mod.rs / libpod.rs），不动
easytidy-dock / easytidy-server；GUI/CLI 改动只需重启对应进程。

### 快照 / 重建 commit 默认清空继承 labels（2026-09-03）

症状：环境快照（`env_snapshot`）或重建（`rebuild`）时，commit 出的镜像
`localhost/easytidy-rebuild:<tag>` / `easytidy/snapshot/<name>` 同样带
`devcontainer.*` / `io.buildah.*` 等 image 标签——下次用此镜像 create
容器（rebuild 路径）或被外部工具 inspect 时仍泄露宿主信息。

修复路径与启动时一致：commit 时传 `LABEL=foo=` 清空值。

- `core/src/libpod.rs::commit_squash`：新增 `changes: &[&str]` 参数，
  每个 change 作为重复 `changes=` query 参数附加到 URL。**注意** libpod
  commit 的 schema 字段是 `changes`（**复数**），单数 `change` 不生效
  （实测验证：handler `schema:"changes"` 累积为 `[]string`；URL
  `change=A&change=B` 被忽略，`changes=A&changes=B` 生效）。
- `core/src/podman/mod.rs::Podman`：
  - 新增 `build_label_clear_changes(name)`：`inspect_container` 读容器
    `Config.Labels`，对每个 key 生成 `LABEL=foo=`（空值覆盖）。inspect
    失败 best-effort（warn + 空 vec，commit 照常跑）。
  - `snapshot()`：先 `build_label_clear_changes(name)`，再 `commit_squash` 附带。
  - `rebuild()` 走的 `commit_container()`（bollard Docker compat）：把
    `Vec<String>` join `\n` 塞进 `CommitContainerOptions.changes`（Docker
    协议要求多指令 `\n` 分隔）。空切片 = `None`（原行为）。
- 两个 commit 路径都应用同一逻辑——快照 + 重建的镜像都不带敏感 label 值。

**前置 base image 重建**（2026-09-03 操作）：`localhost/desk_pilot:9.3.2`
已基于 `localhost/desk_pilot:9.3.1` 用同样手法构建 —— 所有 label value 清空，
仅 key 保留（podman 5.4.2 硬限制）。后续若 `desk_pilot:9.3.1` 重建并添加
devcontainer labels，可重新跑同样 commit 一次生成新的 `<ver>.N`。

验证（端到端实测）：
- 容器 `localhost/desk_pilot:9.3.1` 起容器：labels 含完整 devcontainer 路径/env
- snapshot（携带 `changes=LABEL=foo=`）：快照镜像 labels 全 `""`
- 用快照镜像起的容器：labels 全 `""`，敏感内容消失

测试：113 项 core 单测全过（含原 `test_libpod_body_image_labels_override`）。
clippy 无新增 warning。

重建容器注意：本改动只动 host 端 core；GUI/CLI 改动只需重启对应进程。
`localhost/desk_pilot:9.3.2` 已构建在本地，无需重建。

### 快照名 `name:version` 感知（2026-09-04）

症状：用户在 CLI / GUI 输入 `myimage:v1` 时，原实现简单 `format!("easytidy/snapshot/{snapshot_name}")`
得到 `easytidy/snapshot/myimage:v1` 后整个塞进 libpod commit 的 `repo`
参数，**podman 内部对 `repo` 自动追加 `:latest`，拼成 `repo:v1:latest` →
500 `parsing reference "<repo>:<tag>:latest": invalid reference format`**。

实测确认 libpod `/v5.0.0/libpod/commit` 行为：
- `repo=test/snap-3`（无 `:`）→ 镜像 = `localhost/test/snap-3:latest` ✓
- `repo=test/snap-3&tag=tag1` → 镜像 = `localhost/test/snap-3:tag1` ✓
- `repo=test/snap-3:tag1`（一个 `:`）→ 500 invalid reference format ✗

所以 `repo` 与 `tag` 必须**分开**传，podman 不会从带 `:` 的 repo 切 tag。

修复：
- `core/src/libpod.rs::commit_squash` 签名改：`container`, `repo: &str`,
  `tag: Option<&str>`, `message`, `changes`；`tag=None` 时不附加 `tag=` 参数
  （podman 走默认 `:latest`）。
- `core/src/podman/mod.rs::Podman::snapshot` 调 `parse_snapshot_ref` 拆分用户
  输入：`name:version` → `(name, Some(version))`；纯 name → `(name, None)`；
  非法字符（含 `/` / 空 tag）走原样透传（让 podman 自己报错）。
- GUI placeholder / CLI help 同步：`name[:tag]` 提示。

测试：新增 `test_parse_snapshot_ref` 覆盖纯 name / `name:v1` / `desk_pilot:9.3.2`
/ `foo/bar` / `foo:` / 空字符串 6 种 case。113 项 core 单测全过。
clippy 无新增 warning。

重建容器注意：本改动只动 host 端 core + CLI + GUI；不动 easytidy-dock。
`cargo build -p easytidy-core` + 重启 GUI / CLI 进程。

### GPU 透传：拆成 AMD / NVIDIA 两个独立按钮（2026-09-04）

症状：旧实现 `ContainerParams.gpu: Option<String>` 单字段（值格式
`<vendor>[=<spec>]`），UI 用 Select（关闭 / NVIDIA / AMD）+ spec Input 表达。
用户场景：核显 + NVIDIA 独显的笔记本（或双 vendor 工作站）想同时透传，
旧 Select 只能二选一。

修复：
- `core/src/models.rs::ContainerParams`：`gpu: Option<String>` → 两个 bool
  `gpu_nvidia: bool` / `gpu_amd: bool`（默认均 false）。spec 输入框能力整体
  丢掉（勾选即全设备直通；要选具体设备走 `devices: Vec<String>` 裸设备）。
- `core/src/models.rs::ContainerParams` 改**手动 Serialize / Deserialize**：
  - 写出：固定字段顺序 + 默认跳过 false / 空 / None（与原
    `#[derive(Serialize)]` 行为对齐）。
  - 读入：仅新字段 `gpu_nvidia` / `gpu_amd`；保留 `user_home` 作为
    `keep_id` 别名（避免破坏老手写 yaml/flavor）。
  - **无 GPU 迁移**——程序未发布，旧 `gpu: "nvidia"` 字段直接报错忽略。
- **vendor 设备注入路径分叉（2026-09-04 desk_pilot 实测定案）**：
  - NVIDIA → libpod body 拼 `nvidia.com/gpu=all` CDI 引用（nvidia-container-
    toolkit 自动生成 nvidia.yaml，可解析）。
  - AMD → **不走 CDI**：AMD 生态无自动 CDI 工具链，`amd.com/gpu=all` 在真实
    宿主恒 `unresolvable CDI devices`（desk_pilot 启动实测 500）。改为
    `env::host::detect_amd_gpu_devices()` 裸设备探测：`/dev/kfd`（ROCm 入口）
    + `/dev/dri/renderD*` 中 sysfs PCI vendor = `0x1002` 的 render 节点（VAAPI/
    Mesa/ROCm 够用，不抢宿主 DRM master）。探测纯函数 `collect_amd_devices`
    （目录注入，tempdir 单测）。gpu_amd 开但探测为空 → create 前报可读中文错误。
  - libpod `keep_id_create_body` 签名：`gpu: Option<&str>` →
    `gpu_nvidia: bool, amd_gpu_devices: Vec<String>`（AMD 由 create_with_config
    先探测再传入；设备串与用户手动 devices 去重）。双开 = kfd+renderD + nvidia CDI。
- `core/src/env/host.rs`：删 `GpuVendor` enum / `parse_gpu_value`；新增
  `detect_amd_gpu_devices`。`inject_gpu_passthrough` NVIDIA 开注入
  `NVIDIA_VISIBLE_DEVICES=all` + `NVIDIA_DRIVER_CAPABILITIES=all`；AMD 无
  vendor env。`inject_passthrough(config)` 改看两个 bool。
- `core/src/podman/mod.rs::create_with_config`：`gpu_amd` 时调
  `detect_amd_gpu_devices()`（不落盘——render 节点号随重启漂移，配置只存意图
  bool），探测空 → `Error::Config`。
- GUI：`ContainerConfig.gpu` → `gpu_nvidia` / `gpu_amd`。
  `ContainerPane.tsx` GPU 段（Select + Input → 2 行 Switch）；Types 镜像。
- `crates/gui/assets/chrome.eg.yaml` / `crates/gui/conf/chrome.yaml`：
  `gpu: nvidia` → `gpu_nvidia: true`。
- 模板 `conf_template_gpu_expand` 测试 4 case（nvidia / amd / both / none）。
- 测试：core 112 全过（env::host 新增 `collect_amd_devices` 2 case 用 tempdir
  造 fake dri+sysfs，不依赖真 AMD 宿主；`test_libpod_body_device_fields` 覆盖
  amd 裸设备落位 + 与手动设备去重）。

验证：`cargo test --workspace --lib` 全过（core 112 + gui 13 + cli 25 + shared 12）；
clippy 无新增 warning。libpod 直调端到端（keep-id + host 网）：devices
`kfd+renderD129` 与 `kfd+renderD129+nvidia.com/gpu=all` 两种 body 均创建成功，
容器内 /dev/kfd + /dev/dri/renderD129 + /dev/nvidia-* 齐全。

重建容器注意：本改动只动 host 端 core + CLI + GUI；不动 easytidy-dock / server。
`cargo build -p easytidy-core` + 重启 GUI / CLI 进程。

### 快照支持选择 squash / 普通 commit（2026-09-04）

需求：原「快照」恒走 `commit --squash`（单层扁平镜像），用户希望能选——
squash（单层、体积小）或普通 commit（保留源镜像分层历史）。GUI 快照按钮
改为**下拉选择按钮**：主按钮走默认 squash，右侧箭头菜单显式二选一。

修复：
- `core/src/libpod.rs::commit_squash` → 改名 **`commit`**，新增 `squash: bool`
  参数：query 从硬编码 `squash=true` 改为 `squash={squash}`（true/false 都
  附加——libpod handler 按 bool 解析，缺省等价 false，显式传更清楚）。
- `core/src/podman/mod.rs::Podman::snapshot` 签名加 `squash: bool`；OCI history
  `message` 按形态区分（`commit --squash` / `commit`）便于 `podman inspect` 溯源。
  默认 squash=true 保持既有行为不变。
- `gui/src/commands/containers.rs::env_snapshot` 加 `squash: Option<bool>`
  （未传按默认 true；前端 camelCase `squash`）。
- `cli/src/main.rs::EnvCmd::Snapshot` 加 `--no-squash` flag（默认 squash）；
  `cmd_env_snapshot` 透传 `squash: bool`。
- 前端 `ContainersPanel.tsx`：快照按钮 `Popconfirm` → `Dropdown.Button`
  （主键=默认 squash 单层，菜单=[squash 单层（默认，体积小） / 普通 commit
  （保留分层历史）]）；选定形态弹 `Modal` 填可选快照名再确认（`snapshotModal`
  state 记录 {name, mode}，`handleSnapshotConfirm` 调 `env_snapshot` 传
  `squash: mode === 'squash'`）。原 `handleSnapshot` + `Popconfirm` 移除。

验证：`cargo build/test --workspace` 全过 + clippy 无新增 + `tsc --noEmit` 通过。
squash 路径与既有快照完全等价（query 从 `squash=true` 到 `squash=true`），
普通 commit 路径走同一 libpod 端点 `squash=false`。

重建容器注意：本改动只动 host 端 core + CLI + GUI；不动 easytidy-dock / server。
`cargo build -p easytidy-core` + 重启 GUI / CLI 进程。

### 重建顺序改造：新容器确认就绪后才删旧（2026-09-04）

需求：原 rebuild 顺序「commit → stop → 删旧 → 建新 → 启动」，删旧发生在**确认新容器
起来之前**——若建新/启动失败，旧容器已删，环境只剩 commit 镜像可手动恢复。改为「**新
容器确认就绪后才删旧**」：旧容器全程保留到确认，失败自动回滚，环境不中断。

两个硬约束（决定方案）：
- **podman 同名唯一**：新容器要用正式名 `<name>`（label `easytidy.name` / socket 目录
  / server 注册全按它绑定），创建前旧 `<name>` 名字必须先释放（rename 走）。
- **socket 目录按容器名派生**：新旧容器都 bind `socket_dir_for(<name>)`，两个 dock
  daemon 抢同一 `dock.sock` → 旧容器至少要 stop 释放 socket，**重建必有短暂中断**
  （零中断共存做不到）。

修复（`core/src/podman/mod.rs::Podman`）：
- `rebuild` 改「保留旧容器」流程：
  1. commit 当前层 → 镜像（数据保险）
  2. `rename` 旧 → `<name>-old-<tag>`（**保留**，释放正式名；数据在容器层+镜像双保存）
  3. `stop` 旧（释放 socket——旧 dock daemon 退出）
  4. `create_with_config`（正式名 `<name>`，label/socket/server 全按 `<name>`）
  5. `start_and_confirm`（start + 确认 running + dock daemon 就绪）
  6. **确认就绪** → `remove` 旧
  7. 第 4/5 步任一失败 → `rollback`（删未就绪新容器，旧 rename 回正式名 + start，环境恢复）
- 新增 `rename`（bollard `rename_container`）、`dock_alive`（容器内 `easytidy-dock
  client ping`，解析 `alive: true`；exec 不通/无响应/非 alive 均 false）、
  `start_and_confirm`（轮询 is_running 最多 ~15s + 轮询 dock_alive 最多 ~15s，两级都过
  才 Ok）、`rollback`（rename 回 + start，尽力而为，任一步失败仅 warn）。
- `commit_container` 的 `changes: None` **保持不变**——rebuild 镜像是给自己容器当 base
  的，create 时 `easytidy.name`/`manager` 会重新注入，**不需要清继承 labels**（清 label
  是**快照**的语义，不适用于 rebuild）。

其他同步：
- GUI `env_rebuild` 注释 + 前端 `ContainersPanel.tsx` 重建按钮 description + CLI
  `cmd_rebuild` doc 均更新为新语义。
- `crates/cli/tests/rebuild_e2e.rs`：假 dock 脚本（`FAKE_CTOOL_SCRIPT`）加 `client
  ping` → `alive: true` 分支（rebuild 的 `start_and_confirm` 依赖 dock 存活确认）。

验证：`cargo test --workspace -- --test-threads=1` 全过 + clippy 无新增 + `tsc --noEmit`
通过。（core 并发跑偶发 `test_remove_socket_dirs_removes_all_generations` flaky——该测试
改 `XDG_RUNTIME_DIR` 全局 env，串行即稳定，与本改动无关。）

重建容器注意：本改动只动 host 端 core + CLI + GUI；不动 easytidy-dock / server。
`cargo build -p easytidy-core` + 重启 GUI / CLI 进程。

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
