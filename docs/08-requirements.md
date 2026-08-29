# 08. 需求阐述 v0.6（2026-08-22）

> 状态：需求 v0.6（2026-08-27 身份模型勘误）——命名收敛：原"中心化 GUI"=**Master GUI**、
> 原"per-容器 GUI"=**Worker GUI**；
> 身份模型（勘误取代 v0.6 的 init 镜像烘焙）：容器直接以配置 `<uid>:<gid>` 运行
> （默认 = 宿主登录用户，可自定义 uid/gid/用户名；keep-id 锁 1:1），**无 init 镜像烘焙**
> （user_name 建号改由创建后宿主侧 root exec 幂等完成），server 以容器默认用户同身份
> 运行（无 root、无 su 降权）；root 终端走宿主 `easytidy-root-channel` 进程（每容器
> 共享 root shell，`exec --user 0`，不再经容器内 server su 桥接）；
> 数据目录统一到 `~/.easytidy` 下子目录（flavors / logs / icons / …）。
> 调研前置：docs/01-07。

## 产品形态总述

- **三种管理面（同一应用族 easytidy，全部 Rust）**：
  - ① **Master GUI**（容器管理 + 模板 + 镜像 + 配置入口；原称"中心化 GUI"）
  - ② **Worker GUI**（单容器作用域的浏览器式管理界面；原称"per-容器 GUI"），从宿主桌面图标打开
  - ③ **easytidy 无头 one-shot CLI**（一等公民：管理、脚本、自启动单元都用它）
- ① 与 ② 是**同一个 GUI 应用**：不常驻、多实例、经**配置文件互通**、不同 CLI 参数启动、支持私有配置文件
- **默认无头容器 + 有头管理界面封装**；容器 GUI 透传保持 DistroBox 哲学（共享显示 / GPU）。家目录**不共享**：跟随容器默认用户的 passwd home（容器层持久，2026-08-28 定案）
- **容器内零 systemd**；与宿主 systemd 的交互仅发生在"开机自启动"场景
- **与 DistroBox 的关系声明**：仅参考其架构与产品语义（docs/01），**不依赖其代码**；不调用 podman CLI（改走 **podman socket API**，调研第 5 轮证据：版本化 socket API 决定工具生死，CLI 解析在每次版本升级中破裂）

## 技术栈（已定）

| 组件 | 语言/栈 | 说明 |
|---|---|---|
| 宿主 GUI manager core（三入口） | **Rust**（GUI 壳 = **Tauri 2 + React**，D1 已定） | Master、Worker、无头 CLI 共用同一核心 crate |
| 容器内 server | **Rust**，静态二进制（musl，容器内零依赖） | 容器 entry（默认用户 node），经 unix socket 服务 GUI |
| 引擎层 | Rust 直连 **podman socket API**：**bollard**（Docker compat 面）+ 手写 libpod 扩展端点 + `/events` 事件流 + 宿主侧 exec（一次性 `exec_oneshot` 建号/装包 + root-channel 进程持有 root shell） | 生命周期/快照/网络/mount/exec/stats/events 全覆盖；**GUI 与 CLI 三入口零 podman CLI 依赖** |
| 状态/配置 | `~/.easytidy/` 下子目录（容器注册表 configfile、flavors、icons、logs 等） + `flock` 进程锁（每 GUI 实例一把） | GUI 多实例与 CLI 的互通通道；进程锁防多实例 |
| 数据目录布局 | `~/.easytidy/{flavors,icons,logs}/` + 容器注册表 + 配置 + socket 目录（`$XDG_RUNTIME_DIR/easytidy/`） | 不依赖系统目录，便于打包与卸载 |

**工作量两块**：① 宿主 GUI manager core（三入口）；② 容器内 server。

## 进程模型（两个命名空间）

```
宿主命名空间：
  easytidy GUI（Master / Worker，同二进制不同参数，不常驻，多实例）
   ├─► Master：管理所有容器 + 模板 + 镜像；持有 PodmanState（延迟连接）
   └─► Worker：单容器作用域；持有 GuiSession（与容器内 server 的 socket 会话）
        └─► Rust 引擎核心 ──podman socket API──► podman system service
             （socket 激活：宿主 systemd 按需拉起，无常驻守护进程）
             └─► conmon（容器运行期间常驻）★宿主侧锚点
                  └─► 容器 PID 1（= easytidy server）──进入容器命名空间──
                       ├─► 容器内 GUI 应用（passthrough 导出到宿主）
                       └─► 容器内无头应用

容器命名空间：
  PID 1 / 容器默认用户 = 配置 <uid>:<gid>（默认 = 宿主登录用户；keep-id 锁 1:1）
  └─► easytidy server（以容器默认用户同身份运行；身份自发现，无 root）
   ├─► entry 应用（链式拉起）
   ├─► passthrough 应用（server 拉起；apps.launch / apps.ps / apps.logs / apps.kill）
   ├─► 终端会话（默认用户 PTY，经 socket；server 持久持有，多终端面板 attach 复用）
   ├─► root shell（每容器一个共享，宿主 root-channel 进程 exec --user 0 持有，
   │    容器内父 = conmon；root 终端经其 unix socket attach/回放/detach/close）
   └─► 应用子进程（默认用户身份运行，无降权）
```

- GUI ↔ server：unix socket（bind-mount 进容器，`$XDG_RUNTIME_DIR/easytidy/<name>/socket`）
- **生命周期边界【O2 已决】**：conmon 管理"容器"这个能力实体；server 管理容器内业务；**GUI 生命周期与容器生命周期不绑定**——GUI 退出容器不退出，除非 GUI 显式关闭容器
- 跨命名空间无 OS 父子：逻辑父子经 socket 协议（`shutdown`/`childExited`）
- **root 通道不走容器内 server**：宿主 `easytidy-root-channel` 进程（rootless，按需拉起、
  flock 防多实例）经 podman socket `exec --user 0` 持有**每容器一个共享 root shell**
  （容器内 exec 进程父 = conmon，容器 stop 时被 conmon 回收），GUI/CLI 经其 unix socket
  （`$XDG_RUNTIME_DIR/easytidy-root/`，独立于 bind 进容器的 socket 目录）attach/detach/close；
  流 ID 常量 `1 << 30` 防与 server PTY id 撞；detach = 仅退订（会话续存），close = kill shell

## easytidy server 职责清单（PID 1 身份 = 容器默认用户）

容器内进程树：PID 1 = server（容器 entry；容器默认用户）——静态 Rust 二进制，容器内零依赖；
**容器直接以配置 `<uid>:<gid>` 运行**（libpod create `User` 字段；默认 = 宿主登录用户，
用户可自定义 uid/gid/用户名；配置 `user_name` 时创建后由宿主侧 root exec 幂等 useradd
建号——**init 镜像烘焙方案已废弃**）。keep-id 下容器 uid = 宿主登录 uid（**实测文件
属主，非字面 uid_map**，见 docs/12）。**server 以容器默认用户同身份运行**（经
`/proc/self/status` + `/etc/passwd` 自发现身份，无 root、无 su 降权；euid==0 仅旧
测试容器兼容警告）。

server 职责（3 项）：
1. **业务级 setup 自实现**：server 即容器 entrypoint 命令；用户创建/挂载/环境等 setup 由我们自己的逻辑实现（不复用 distrobox-init）
2. **容器内应用生命周期管理**：entry/passthrough 应用由 server 拉起（server 持有 ChildInfo，`apps.launch / apps.ps / apps.logs / apps.kill`，含 stdio 环形缓冲）并跟踪退出事件（上抛 GUI）
3. 服务面：**node PTY 通道**（多终端实例，持久会话 attach 复用，TTY 事件驱动 cwd 推送）、文件系统 API（文件浏览器 + 文本/图片预览，base64 分块读避免 IPC 膨胀）、桌面应用枚举（passthrough）、配置查询/应用

优雅关闭链路：`podman stop` → SIGTERM → server 收尾（关 socket、给子进程发 SIGTERM）→ 退出 → conmon 同码退出 → 容器停止。

## 需求 1：Master GUI 控制界面（原"中心化 GUI"）

- 新建容器：基于 **podman**（经 socket API；走 libpod 端点建容器 + 走 bollard Docker compat 端点兼容补齐）
- 容器创建成功后，**在宿主桌面生成该容器的桌面图标**（`~/.local/share/applications/easytidy-gui-<name>.desktop` + 桌面同文件 + `gio metadata::trusted`）
- 顶部三 tab：**容器 / 模板 / 镜像**（2026-08-22 用户决定：模板与容器管理分离，不合并）

## 需求 2：无头容器封装有头界面（架构性需求）

容器进程组成：
- **easytidy server**（PID 1 / 容器 entry = node；管理 GUI 的容器内数据/通道服务）
- 受容器限制的其他 GUI 应用（经 passthrough 导出到宿主启动器）
- 受容器限制的其他无头应用

## 需求 3：Master GUI 控制界面功能

- **3.1 容器生命周期管理**（环境语义，docs/13）：创建 / 销毁 / 快照 / fork（从快照派生）/ 启动 / 停止
- **3.2 桌面图标创建 + 容器内 GUI 应用使能**（导出/使能容器内 GUI 应用到宿主启动器）
- **3.3 模板管理**：flavor 增删改 + 同步派生（按模板当前声明重新展开并重建全部派生容器）
- **3.4 镜像管理**：拉取 / 列表 / 批量删除（被引用自动 force）

## 需求 4：Worker GUI（单容器 GUI，原"per-容器 GUI"）

- **4.1** 继承 Master GUI 的能力（容器配置、文件浏览器、终端、passthrough），作用域限定于单个容器
- **4.2** 通过宿主机桌面图标打开该 GUI（`Exec = easytidy open --container <name>`——CLI 垫片：保活容器后按 `silent_boot` 决定是否弹 GUI，容器不存在时弹终端友好报错）
- **4.3** 内置多终端（node / root 双身份）：
  - **node 终端**：经容器内 server PTY（持久会话，attach 重连回放屏幕）
  - **root 终端**：经宿主侧 bollard exec 通道（不经 server；stream_id 偏移 `1<<30`）
- **4.4** 布局：菜单栏（折叠侧栏 + 终端下拉 + Passthrough + 配置 + 关闭容器 + 收藏栏）；左侧边栏 = 文件浏览器（容器内文件系统，经 server 文件 API）；右方主 panel = 多面板 tab
- **4.5** 左侧边栏 = 文件资源浏览器（容器内文件系统，经 server 文件 API）+ 跟随终端开关（订阅 server cwd 推送）
- **4.6** 右方主 panel = 多 tab：终端（多实例）/ 桌面应用 passthrough 管理器 / 容器配置管理器 / 内嵌文本编辑器（CodeMirror 6）/ 图片预览
- **4.7 passthrough 管理器**：控制容器内桌面文件夹中哪些应用快捷方式可导出到宿主；以及开机自启动透传（容器启动时经 server `apps.launch` 拉起）；自定义应用（容器固定目录之外的命令）；收藏到工具栏（pin）；容器自启动模式（off / silent / gui）
- **4.8 容器配置管理器**：mount 管理、网络映射管理、容器重建等；**保存即重建**（不存在"已保存未生效"状态）；血缘提示（来源 flavor 存在 / 漂移 / 同步派生）

## GUI 多实例与配置文件通信

- `easytidy-gui`（Master）/ `easytidy-gui --container <name>`（Worker），同一二进制
- **不常驻**：按需启动，启动时刷新状态
- **进程锁防多实例**：`core::guilock`（flock）
  - Master：`$XDG_RUNTIME_DIR/easytidy/gui.lock`，单实例
  - Worker：`$XDG_RUNTIME_DIR/easytidy/gui-<name>.lock`，每容器单实例
- **配置文件互通**（无 IPC 守护进程）：共享状态文件（容器注册表 configfile + settings + 自启动配置 + 自定义应用）
  - 数据竞争处理：原子写（临时文件 + rename）、`flock` 临界区
  - 容器注册表：`~/.easytidy/configfile`（`ContainerConfig` 全量序列化）
- 支持 `--config <file>` 私有配置

## 静默启动（4.7 语义）

- 容器配置项：**entry 应用** + **静默启动标志**
- 宿主自启机制：**宿主 systemd user unit**，`ExecStart = easytidy boot --container <name> [--gui]`
- 链路：宿主开机 → systemd unit → easytidy 无头启动容器 → server 拉起 entry 应用
- `--gui`：非静默（启动容器后拉起 Worker GUI 窗口）
- 默认：仅静默启动（登录后容器后台就绪，不弹窗口）
- entry 无头 → 无头应用开机静默透传；entry 有头 → 经 passthrough 出现在宿主桌面

## 已确认决策汇总

| # | 决策 | 结论 |
|---|---|---|
| L1 | 语言 | **全栈 Rust**（GUI core 三入口 + 容器内 server） |
| L2 | 引擎通信 | **podman socket API**（bollard compat 面 + libpod 端点 + `/events` 事件流 + bollard exec 通道）；**三入口零 podman CLI 依赖**；podman.socket 一次性启用由安装器/首启引导做（systemctl，socket 激活无常驻） |
| D1 | GUI 壳 | **Tauri 2 + React**（CJK/IME spike 已通过，见 docs/10）；NVIDIA 宿主需 `WEBKIT_DISABLE_DMABUF_RENDERER=1`（正式 GUI 在 main.rs 内置，类 clash-verge-rev 的 NVIDIA 检测 + 运行时注入） |
| O1 | 终端通道 | server 提供默认用户 PTY（多终端持久会话 attach 复用，TTY 事件驱动 cwd 推送）；root 终端走宿主 root-channel 进程（每容器共享 root shell，不经 server） |
| O2 | 生命周期边界 | conmon 管容器能力、server 管容器内业务；**GUI 与容器生命周期不绑定**；GUI 退出容器不退出，除非显式关闭 |
| O3 | 容器根进程（2026-08-27 身份模型勘误） | **server = PID 1 / 容器 entry / 容器默认用户**（配置 `<uid>:<gid>`，默认宿主登录用户；keep-id 锁 1:1）；**无 init 镜像烘焙**（user_name 建号 = 创建后宿主侧 root exec 幂等 useradd）；server 与默认用户同身份运行（无 root） |
| Q3 | 快照范围 | 仅容器文件系统层 |
| Q4 | 开机自启动 | entry 应用 + 静默标志 + 宿主 systemd unit（`easytidy boot --container <name> [--gui]`） |
| Q5 | Master / Worker GUI | 同一二进制，不同参数；不常驻、多实例、配置文件 + 进程锁通信；新命名替换原"中心化/per-容器" |
| Q6 | 快照实现 | **podman commit / export / import 简单封装**（版本化管理后置） |
| Q7 | 命名 | **Master GUI** = 中心化容器管理界面；**Worker GUI** = 单容器管理窗口（经桌面图标拉起，`--container <name>`） |

## 架构含义与风险

1. **常驻语义**：server（PID 1 / 容器默认用户）存活期间容器常驻；容器停止 = server 退出（显式关闭或 podman stop）
2. **PID 1 义务工程化**（v0.6 已大幅简化）：server 直接承担 PID 1 角色（僵尸回收 + 信号转发 + 优雅关闭）；测试覆盖 SIGTERM 广播 + 子进程升级
3. **keep-id 真实身份**：容器 uid 1000 = 宿主登录用户（**文件属主实证**，docs/12）；容器 uid 0 = 宿主 subuid 100000（**不是**宿主默认用户）—— 误判易致理解偏差
4. **root 终端语义**：走宿主 root-channel 进程（每容器一个共享 root shell，容器内父 = conmon），**持久化 + 多客户端 attach**（128KB 回放，2026 同步包裹）；detach = 仅退订（GUI/CLI 退出不影响会话），close = kill shell；无 cwd 跟随
5. **引擎层重写**：Rust 实现 podman socket API 客户端（create/start/stop/exec/commit/export/import/inspect/events + 标签管理）；容器配置 schema（entry 应用/静默标志/挂载/网络/passthrough/用户一致性映射）；宿主 systemd unit 生成；.desktop 图标生成
6. **配置文件竞争**：多实例 GUI + CLI 并发读写共享状态——原子写 + flock 是硬性设计约束；不再依赖 schema 版本化（`~/.easytidy` 集中布局 + 进程锁足够）
7. **网络映射（4.8）**：仅对 bridge 网络容器有意义
8. **终端组件**：Tauri → xterm.js（DOM 渲染器；CJK 字体回退；rAF 批量写；WebKitGTK 焦点恢复；Ctrl+Shift+C 捕获阶段监听；30s 心跳保活）
9. **CJK/IME spike（D1 前置，已通过）**：原生 Wayland + `WEBKIT_DISABLE_DMABUF_RENDERER=1` + fcitx5，渲染稳定、IME 可用、终端可用；见 docs/10-m0-spike-report.md

## 开放问题

- 【D1】GUI 壳最终确认：Tauri（CJK spike 通过）vs gtk4-rs + VTE（spike 失败）
- 【新】entry 退出语义：默认容器不停止（server 常驻）；可选"随 entry 退出"模式是否首版提供
- 【新】root 终端恢复：现 exec 通道无 attach 语义，重开 Worker GUI 不会恢复 root 终端会话——是否需要持久化宿主 exec 进程或接受现状

## 配置 flavor 概念（2026-08-06 修正，2026-08-22 沿用）

**哲学**：flavor = "如何启动一个**能够拉起 GUI 应用**的容器配置"，即 **GUI 透传底座**（显示环境注入 + entry 应用）。flavor **不含特定应用的安装步骤**——应用及依赖由使用者手动安装（经 `easytidy run --container <n> -- bash -c '<install>'`，即 run 机制本身是安装的执行通道）。应用安装/分发属后续"应用定义/商店"范畴（M5）。

**GUI 透传配方**（参考 docs/11-gui-container.md，宿主实测验证）：
- env：DISPLAY / WAYLAND_DISPLAY / XAUTHORITY / XDG_RUNTIME_DIR（取宿主值），`XDG_DATA_DIRS` **追加**而不是覆盖（docs/14 教训）
- 挂载：`/tmp/.X11-unix`（X11 socket）、`$XDG_RUNTIME_DIR`（Wayland/dbus/XAUTHORITY）
- P1 待做：GPU 透传（`--gpus=all` + NVIDIA_* env，需宿主 nvidia-container-toolkit）、apparmor=unconfined、`--pid=host`

**无头安装纪律**：setup 必须以非交互执行（`DEBIAN_FRONTEND=noninteractive` + `TZ=UTC`）——tzdata 等 debconf 提示会卡死无头安装（实测教训）。

**flavor 清单**：`~/.easytidy/flavors/<name>.toml`（统一到 easytidy 数据目录）；`easytidy flavor list/save/delete/expand`。