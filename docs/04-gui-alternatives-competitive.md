# 04. 非 Go 方案 + 竞品调研

> 2026-08 现状核实。评估"引擎用 Go，GUI 用别的"路线，并调查同类产品的技术选择与 GUI↔引擎通信架构。

## Part A — 非 Go 候选评估

### Tauri 2.x（Rust + Web）— 非 Go 首选，但有条件
- **状态**：Tauri 2 稳定（2024-10），tauri-cli 2.11（2026-06）；Linux webview = **WebKitGTK 4.1**（不捆绑浏览器引擎）
- **打包/更新**：内置 .deb/.rpm/.AppImage（+社区 Flatpak/Snap/AUR）；**Linux 上应用内自更新只对 AppImage 生效**（deb/rpm 须走系统包管理器；多格式 payload 解析到 updater 2.10.1 才修好）
- **体积/性能**：2–10MB 安装包，~30–50MB 空闲内存，亚秒启动
- **Wayland**：可用但有历史渲染 bug（`GDK_BACKEND=wayland` 对部分 WebKitGTK 版本有问题 #12361）；常用规避环境变量（`WEBKIT_DISABLE_COMPOSITING_MODE=1` 等）
- **CJK/中文输入——头号风险**：WebKitGTK 的 Wayland IME 有公开上游 bug（Tauri #8264 未关、WebKit #261795 合成文本不可见直至窗口 resize、keyCode-229 合成问题）；规避手段 = 逐用户配置（`GTK_IM_MODULE=fcitx`、GTK settings.ini、XWayland 回退、JS 抑制 229）——**不是开箱即用**
- **真实应用（2026）**：Kunobi（K8s 客户端）、GG（Jujutsu VCS GUI）、Restic Browser、大量 Linux 工具，多数 .deb/.AppImage/AUR

### Electron — 成熟但笨重
- Electron 43.2（2026-06，Chromium 150/Node 24），同时支持三代
- **代价**：80–250MB 安装包、~100–500MB 空闲内存、1–4s 冷启动、安全默认关闭
- **为何仍可辩护**：生态最成熟（electron-builder/updater 差分更新）、跨平台渲染一致、**本领域先例 Podman Desktop**
- **判决**：避免（重量），除非有特殊理由

### Rust 原生三剑客（Slint / Iced / egui）— 均无消费级成品
| | Slint 1.17 | Iced 0.14 | egui 0.34 |
|---|---|---|---|
| 状态 | 桌面官方仍 "in progress"，嵌入式是主战场 | 官方仍 "experimental"，**单一维护者**（README 劝退贡献）；System76 COSMIC/libcosmic 是其旗舰用户 | 工具/仪表盘观感 |
| Linux | winit X11/Wayland + 可选 Qt 后端 | wgpu/tiny-skia | winit，wayland/x11 显式 feature |
| 托盘/drag-drop | 1.17 才落地（NLnet 资助） | — | 工具级 |
| 判决 | 生态约 Qt 的 1/100，稀疏控件 | 官方 experimental + 单维护者风险（Bottles 正是因前栈失败而重写于此，且重写尚未完成） | CJK 需嵌字体，i18n 手动 |

三者 IME（winit 文本输入）都远不如 GTK/Qt 的 IM 路径久经考验。

### Qt（C++ / PySide6）— KDE 路线
- Qt 6.11 当前（2026-03），**6.8 五年 LTS（商业支持至 2029-10）**
- **业界最佳中文输入**：原生 Wayland text-input-v2/v3 + `fcitx5-qt` 插件；Qt 6.7 起 `QT_IM_MODULES="wayland;fcitx;ibus"` 自动 IME 回退
- PySide6 为官方绑定、与 Qt 同步发布；真实大产品在用（Krita 等）
- 消费分发需捆绑 Qt 库（PyInstaller/AppImage）——"最不愉快"的部分
- **应用商店先例：KDE Discover 本身**

### Wails v3（Go + webview）
- **未 GA**（v3.0.0-beta.0 2026-08）；v2 为稳定线
- 与 Tauri 共享 WebKitGTK → 同样的 IME 风险；通知在 roadmap
- 业界信号：Bottles 团队公开评估过 Go，"没有质量 GUI 工具包"而弃（转 Rust + libcosmic）

## Part B — 竞品技术选择

| 产品 | 技术栈 | 引擎通信方式 |
|---|---|---|
| **Podman Desktop** | Electron + Svelte 5 + TS + Vite | **容器引擎 REST API socket**（dockerode + 自定义 LibpodDockerode），`/events` 事件流推送 UI；`podman machine` 无 API 部分才 exec CLI |
| **Bottles**（当前版） | Python + GTK4/libadwaita 单体 | 进程内 Manager 单例 + 子进程调 Wine；`liverun` 助手在 prefix 内 |
| **Bottles Next**（重写中） | Rust 客户端（libcosmic）+ Rust 服务端（"the brain"，可第三方复用）+ C#/WineBridge 代理 | 客户端/服务端分离；曾明确弃 Electron（社区反弹）与 Go（工具包质量） |
| **Flatseal** | GJS (JS) + GTK4/libadwaita | 链接 **libflatpak** 库 |
| **GNOME Software** | C + GTK4 + libadwaita + libsoup3 | 插件架构：libflatpak / PackageKit / fwupd / snap / rpm-ostree；polkit |
| **KDE Discover** | C++ 逻辑 + QML/QtQuick (Kirigami) | libflatpak / PackageKitQt5 / libsnapd-qt / fwupd |
| **BoxBuddyRS / DistroShelf / Kontainer** | Rust+GTK4 / GNOME / Qt6 | **被迫 CLI 子进程**（distrobox 是脚本，无库）——见 05 |

**关键架构事实**：Flatpak 生态（GNOME Software、KDE Discover、Flatseal）全部**链接 libflatpak 库而非 CLI**——flatpak 维护者 smcv：CLI 是给高级用户的，不是给上层软件脚本化的（flatpak #5186）。

### Flatpak 架构速览（10 行版，供借鉴）
开发者写 **manifest**（JSON/YAML：app id、模块、带校验和的源、SDK/runtime、`finish-args` 权限）→ `flatpak-builder` 在干净环境按 SDK 构建 → 产出 **OSTree 仓库**提交（每应用一条分支 `app/arch/branch`）→ 用户加 **remote** 安装应用 + **共享 runtime**（平台），磁盘去重 + 原子更新/回滚 → 运行期 **bubblewrap**（namespace + seccomp）沙箱，默认无宿主文件/网络/设备 → 权限经 finish-args 与用户同意的 **portal**（xdg-desktop-portal：文件选择器、通知、截图…，D-Bus 中介）→ **AppStream** 元数据供软件中心展示 → 应用状态存 `~/.var/app/$APP_ID`。

## Part C — GUI↔引擎集成模式与取舍

| 模式 | 代表 | 取舍 |
|---|---|---|
| **CLI 子进程** | BoxBuddy/DistroShelf/Kontainer | 最简单、崩溃隔离；但每操作慢、人类输出解析脆弱、进度/事件流痛苦、终端所有权冲突（BoxBuddy 为 create/update 开终端窗口） |
| **链接库** | libflatpak 生态 | 类型化 API、快、无 IPC 开销；但引擎锁定单 ABI/语言（C-ABI 是逃生门） |
| **守护进程/Socket API** | Podman Desktop | 结构化 JSON、事件流、多客户端、长驻状态；代价：守护进程生命周期（socket 激活、升级、崩溃）、安全面（socket 须用户属主） |

**对我们的默认结论**：**子进程调自有 Go CLI（结构化 JSON/NDJSON 输出 + 事件流）**；守护进程（unix socket）留作未来选项（多客户端/后台拉取需要时）。引擎是我们的，可完全规避 distrobox GUI 前端的解析脆弱性。

## 综合推荐（本调研范围）

1. **Tauri 2.x 是非 Go 首选**——产品形态（应用商店 UI：富列表/搜索/设置/徽章）恰是 web 前端强项，体积最小、Linux 更新器最好（AppImage）；**两个前提**：① CJK 输入预算早期 spike（fcitx5/ibus under GNOME Wayland），须带上文档化规避方案（IM env、keyCode-229 抑制、问题 webview 的 XWayland 回退）；② AppImage 优先分发，deb/rpm 走包管理器更新
2. **次选 Qt（PySide6 或 C++/QML，KDE 路线）**——唯一同时有应用商店先例（Discover）与原生 Wayland + fcitx5 中文输入；若 IME 质量权重 > UI 迭代速度则选它；代价是 web 式 UI 开发慢 + Python/Qt 打包摩擦
3. **避免**：Electron（重量）、Wails v3（未 GA + 同 webkit IME 问题）、Slint/Iced/egui（无消费级成品；Iced 官方 experimental 单维护者；连最接近的 Bottles 都在重写中）
4. **架构照抄 Podman Desktop 混合模式**：GUI 经结构化 API 与引擎通信，绝不解析人类文本，绝不壳进交互式终端

## 主要来源

Tauri 2.11 发布 / Tauri IME #8264 / Tauri Wayland #12361 / readest updater PR #4897 / Electron 发布计划 / pkgpulse Electron-vs-Tauri / Slint 1.17 / Iced README / egui releases / endoflife.date/qt / fcitx Wayland 文档 / Wails v3 beta / DeepWiki Podman Desktop / Red Hat CNCF 捐赠公告 / Bottles 重写（Phoronix）/ usebottles.com/next / DeepWiki Bottles / flatpak #5186 / KDE HIG / gnome-software spec / BoxBuddy #125 / Universal Blue DistroShelf / LinuxLinks distrobox GUI 汇总 / Flatpak sandbox permissions / flatpak-manifest(5)
