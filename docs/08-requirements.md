# 08. 需求阐述 v0.5（2026-08-05）

> 状态：需求 v0.5——O3 修订：catatonit（podman `--init`）兼任 PID 1，server 降为普通主进程，PID 1 义务外包；接口策略确认：三入口零 podman CLI 依赖，全走 socket API；D1 待 CJK/IME spike。调研前置：docs/01-07。

## 产品形态总述

- **三种管理面（同一应用族 easytidy，全部 Rust）**：
  - ① 中心化 GUI 管理器（容器创建与生命周期管理 + 桌面图标生成）
  - ② per-容器 GUI 管理器（去中心化，单容器作用域，经宿主桌面图标打开）
  - ③ **easytidy 无头 one-shot CLI**（一等公民：管理、脚本、自启动单元都用它）
- ① 与 ② 是**同一个 GUI 应用**：不常驻、多实例、经**配置文件互通**（需处理文件数据竞争）、不同 CLI 参数启动、支持私有配置文件
- **默认无头容器 + 有头管理界面封装**
- **容器内零 systemd**；与宿主 systemd 的交互仅发生在"开机自启动"场景
- **与 DistroBox 的关系声明**：仅参考其架构与产品语义（docs/01），**不依赖其代码**；不调用 podman CLI（改走 **podman socket API**，调研第 5 轮证据：版本化 socket API 决定工具生死，CLI 解析在每次版本升级中破裂）

## 技术栈（已定）

| 组件 | 语言/栈 | 说明 |
|---|---|---|
| 宿主 GUI manager core（三入口） | **Rust**（GUI 壳 = **Tauri 2 + React**，D1 已定） | 中心化、per-容器、无头 CLI 共用同一核心 crate |
| 容器内 server | **Rust**，静态二进制（musl，容器内零依赖） | 容器 entry（PID 1），经 unix socket 服务 GUI |
| 引擎层 | Rust 直连 **podman socket API**：**bollard**（Docker compat 面）+ 手写 libpod 扩展端点 + `/events` 事件流 | 生命周期/快照/网络/mount/exec/stats/events 全覆盖；**GUI 与 CLI 三入口零 podman CLI 依赖** |
| 状态/配置 | 共享配置文件（原子写 + flock + schema 版本化） | GUI 多实例与 CLI 的互通通道 |

**工作量两块**：① 宿主 GUI manager core（三入口）；② 容器内 server。

## 进程模型（两个命名空间）

```
宿主命名空间：
  easytidy GUI（中心化 / per-容器，同二进制不同参数，不常驻，多实例）
   └─► Rust 引擎核心 ──podman socket API──► podman system service
        （socket 激活：宿主 systemd 按需拉起，无常驻守护进程）
        └─► conmon（容器运行期间常驻）★宿主侧锚点
             └─► 容器 PID 1（= easytidy server）──进入容器命名空间──
                  ├─► 容器内 GUI 应用（passthrough 导出到宿主）
                  └─► 容器内无头应用

容器命名空间：
  PID 1: catatonit（podman `--init` / `HostConfig.Init` 注入，零依赖；收僵尸 + 信号转发 + 优雅升级）
  └─► easytidy server（主进程 = 容器 entrypoint 命令，静态 Rust 二进制）
   ├─► entry 应用（链式拉起）
   ├─► passthrough 应用（server 拉起）
   └─► 终端会话（server 提供 PTY，经 socket）
```

- GUI ↔ server：unix socket（bind-mount 进容器）
- **生命周期边界【O2 已决】**：conmon 管理"容器"这个能力实体；server 管理容器内业务；**GUI 生命周期与容器生命周期不绑定**——GUI 退出容器不退出，除非 GUI 显式关闭容器
- 跨命名空间无 OS 父子：逻辑父子经 socket 协议（`shutdown`/`childExited`）

## easytidy server 职责清单（PID 1 义务外包给 catatonit）

容器内进程树：PID 1 = **catatonit**（podman `--init` / `HostConfig.Init` 注入，零依赖）——僵尸回收、信号转发、优雅升级 SIGKILL、退出码传播全由它承担；**server 是普通主进程，无 PID 1 义务**【O3 修订】

server 职责（3 项）：
1. **业务级 setup 自实现**：server 即容器 entrypoint 命令；用户创建/挂载/环境等 setup 由我们自己的逻辑实现（不复用 distrobox-init）
2. **容器内应用生命周期管理**：entry/passthrough 应用由 server 拉起（server 是它们的父进程，正常 waitpid 拿退出码）并跟踪退出事件（上抛 GUI）
3. 服务面：**PTY 通道【O1 已决】**（终端 4.3，portable-pty 类实现）、文件系统 API（文件浏览器 4.5）、桌面应用枚举（passthrough 4.7）、配置查询/应用（4.8）

优雅关闭链路：`podman stop` → SIGTERM → catatonit 转发 server → server 收尾（关 socket、给子进程发 SIGTERM）→ 退出 → catatonit 同码退出 → 容器停止。SIGKILL 升级由 catatonit 按等待窗口处理。

## 需求 1：中心化 GUI 控制界面

- 新建容器：基于 **podman**（经 socket API）
- 容器创建成功后，**在宿主桌面生成该容器的桌面图标**

## 需求 2：无头容器封装有头界面（架构性需求）

容器进程组成：
- **easytidy server**（PID 1，容器 entry，管理 GUI 的容器内数据/通道服务）
- 受容器限制的其他 GUI 应用（经 passthrough 导出）
- 受容器限制的其他无头应用

## 需求 3：中心化 GUI 控制界面功能

- **3.1 有头容器生命周期管理**：创建、销毁、快照、重启、重建
- **3.2 桌面图标创建 + 容器内 GUI 应用使能**（导出/使能容器内 GUI 应用到宿主启动器）

## 需求 4：去中心化 per-容器 GUI

- **4.1** 继承中心化 GUI 的能力，作用域限定于单个容器
- **4.2** 通过宿主机桌面图标打开该 GUI
- **4.3** 内置一个终端，终端进程启动于容器内（server 提供 PTY）
- **4.4** 布局：菜单栏；下方为左侧边栏 + 右方主 panel
- **4.5** 左侧边栏 = 文件资源浏览器（容器内文件系统，经 server 文件 API）
- **4.6** 右方主 panel = tab panel：终端 / 桌面应用 passthrough 管理器 / 容器配置管理器
- **4.7 passthrough 管理器**：控制容器内桌面文件夹中哪些应用快捷方式可导出到宿主；以及开机自启动透传
- **4.8 容器配置管理器**：mount 管理、网络映射管理、容器重建等

## GUI 多实例与配置文件通信

- `easytidy-gui`（中心化）/ `easytidy-gui --container <name>`（单容器），同一二进制
- **不常驻**：按需启动，启动时刷新状态
- **配置文件互通**（无 IPC 守护进程）：共享状态文件（容器注册表、设置、自启动配置）
  - 数据竞争处理：原子写（临时文件 + rename）、`flock` 临界区、schema 版本化
- 支持 `--config <file>` 私有配置

## 静默启动（4.7 语义）

- 容器配置项：**entry 应用** + **静默启动标志**
- 宿主自启机制：**宿主 systemd user unit**，`ExecStart = easytidy --config <cfg>`（无头 one-shot，不弹 GUI）
- 链路：宿主开机 → systemd unit → easytidy 无头启动容器 → server（PID 1）链式拉起 entry 应用
- entry 无头 → 无头应用开机静默透传；entry 有头 → 经 passthrough 出现在宿主桌面

## 已确认决策汇总

| # | 决策 | 结论 |
|---|---|---|
| L1 | 语言 | **全栈 Rust**（GUI core 三入口 + 容器内 server） |
| L2 | 引擎通信 | **podman socket API**（bollard compat 面 + libpod 端点 + `/events` 事件流）；**三入口零 podman CLI 依赖**；podman.socket 一次性启用由安装器/首启引导做（systemctl，socket 激活无常驻） |
| D1 | GUI 壳 | **Tauri 2 + React**（CJK/IME spike 已通过，见 docs/10）；NVIDIA 宿主需 `WEBKIT_DISABLE_DMABUF_RENDERER=1`（正式 GUI 在 main.rs 内置，类 clash-verge-rev 的 NVIDIA 检测 + 运行时注入） |
| O1 | 终端通道 | server 提供 PTY（portable-pty 类实现），经 socket 流式转发 |
| O2 | 生命周期边界 | conmon 管容器能力、server 管容器内业务；**GUI 与容器生命周期不绑定**；GUI 退出容器不退出，除非显式关闭 |
| O3 | 容器根进程（修订） | **catatonit = PID 1**（podman `--init` 注入）；**server = 主进程 / entrypoint 命令**；业务 setup 自实现；PID 1 义务（僵尸/信号/升级）外包 |
| Q3 | 快照范围 | 仅容器文件系统层 |
| Q4 | 开机自启动 | entry 应用 + 静默标志 + 宿主 systemd unit（easytidy 无头） |
| Q5 | 中心化/单容器 GUI | 同一二进制，不同参数；不常驻、多实例、配置文件通信 |
| Q6 | 快照实现 | **podman commit / export / import 简单封装**（版本化管理后置） |

## 架构含义与风险

1. **PID 1 义务工程化**：僵尸回收、信号转发（SIGTERM 广播 + 超时升级）、优雅关闭序列——server 核心工程质量，测试须覆盖
2. **常驻语义**：server（主进程）存活期间容器常驻；容器停止 = server 退出 → catatonit 退出（显式关闭或 podman stop）
3. **引擎层重写**：Rust 实现 podman socket API 客户端（create/start/stop/exec/commit/export/import/inspect/events + 标签管理）；容器配置 schema（entry 应用/静默标志/挂载/网络/passthrough）；宿主 systemd unit 生成；.desktop 图标生成
4. **配置文件竞争**：多实例 GUI + CLI 并发读写共享状态——原子写/flock/版本化是硬性设计约束
5. **网络映射（4.8）**：仅对 bridge 网络容器有意义
6. **终端组件**：Tauri → xterm.js 6（WebGL + DOM 降级）+ portable-pty（server 侧 PTY）
7. **CJK/IME spike（D1 前置，已通过）**：原生 Wayland + `WEBKIT_DISABLE_DMABUF_RENDERER=1` + fcitx5，渲染稳定、IME 可用、终端可用；见 docs/10-m0-spike-report.md

## 开放问题

- 【D1】GUI 壳最终确认：Tauri（CJK spike 通过）vs gtk4-rs + VTE（spike 失败）
- 【新】entry 退出语义：默认容器不停止（server 常驻）；可选"随 entry 退出"模式是否首版提供
