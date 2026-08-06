# 07. 相关项目全景：distrobox GUI 前端普查 + 容器 GUI 与沙盒生态

> 第 5 轮调研（2026-08-05）。两个并行课题：① distrobox GUI/TUI 前端全量普查（GitHub topic/Flathub/AUR/源码直查）；② 容器管理 GUI 与沙盒类工具全景（桌面/Web/TUI + toolbox 系 + 应用沙盒生态 + 死亡项目尸检）。除已深挖的 Kontainer/BoxBuddyRS/Podman Desktop 外的一切相关项目。

## 一、distrobox GUI 前端普查（主表）

| 项目 | 仓库 | 许可 | 技术栈 | 2025-26 状态 | 功能覆盖 | 分发 |
|---|---|---|---|---|---|---|
| **DistroShelf** | ranfdev/DistroShelf | GPL-3.0+ | Rust + GTK4/libadwaita | **活跃** v1.5.2 (2026-08-04) | create(镜像选择/文件/URL)/assemble/start-stop-clone-rm-upgrade/装包/导出应用/**集成终端**/**命令日志** | Flathub/AUR/Alpine/nixpkgs |
| **BoxBuddyRS** | Dvlv/BoxBuddyRS | MIT | Rust + GTK4/libadwaita | **活跃** v2.5.8 (2026-04) | list/open/upgrade/clone/rm/stop/run 导出应用/装 .deb/.rpm/CPU 统计 | Flathub/AUR |
| **Kontainer (KDE)** | KDE/kontainer（原 invent.kde.org/sitter/k-box，Harald Sitter 2025-06 起） | GPL-2.0+ | C++ + Qt 6.9/QML Kirigami | **停滞** v1.0.1 (2025-09-18) | create/delete/enter/upgrade/export（自称与 BoxBuddy 持平） | AUR(git) |
| **Kontainer (DenysMb)** | DenysMb/Kontainer（2026-02-28 起） | GPL-3.0+ | C++ + Qt/QML Kirigami | **非常活跃** v1.6.1 (2026-08-04)，89★ | 高级创建（自定义镜像/参数/home/卷）、clone、start/stop/reboot、装包文件、assemble、**创建命令字符串实时展示** | Flathub/AUR/ALT |
| **Atoms** | AtomsDevs/Atoms | GPL-3.0 | Python + GTK4 | **休眠** v1.1.2 (2023-06)，最后提交 2024-06 | chroot 优先；distrobox 仅开 shell；作者转向 Vanilla OS/apx | Flathub |
| **DistroRack** | BLumia/distro-rack | MIT | C++ + Qt/QML 6 | **低-中** v0.2.1 (2025-11) | create/import/clone/rm、导出应用、装 .deb、桌面入口、DDE/KDE 主题 | AUR/deepin 25 仓库 |
| **Apx GUI**（相邻） | Vanilla-OS/apx-gui | GPL-3.0 | Python + GTK4/libadwaita | **活跃**（2026-07-01） | apx (Go, 仍依赖 distrobox) 的 GUI | Flathub |
| **EnvStation** | Kubaguette/envstation | GPL-3.0 | Rust + **Tauri v2** + React | **活跃/新** v1.1.1 (2026-05) | Distrobox + Devcontainers 同步的"控制中心"、项目脚手架、podman 存储迁移 | 暂无 |
| **TiffinBox** | BiscuitBobby/TiffinBox | MIT | Tauri (JS+Rust) | **低/休眠**（最后提交 2025-06） | "Docker Desktop 式"distrobox 管理器 | 无 |
| **DistroGUI** | fearlessgeekmedia/DistroGUI | GPL-3.0+ | Qt 时代 | **废弃**（2023-07） | 基础容器管理 | 无 |
| **distrobox-tui** 等小项目 | silwal25 / noyzen(distrobear) / bitsk 等 | — | 杂 | 全部休眠/存疑 | — | 无 |

## 二、TUI / 终端前端

| 项目 | 技术栈 | 状态 | 备注 |
|---|---|---|---|
| **distrobox-tui** | Go + Bubbletea | 低（v0.2.0 2025-03） | hyperreal64 版（55★）的续作；**自述不能创建容器**——"create 的交互性 TUI 不想复刻" |
| **DbxSmith** | 纯 bash TUI | **活跃** v1.5.4 (2026-06) | 异步面板、六种隔离级别（含 RAM 版 "ghost" home）、原子拆除 |
| **disbox.sh** | bash+gum | **废弃**（2023） | "If only there was a GUI for it!"；只有创建能用 |
| **Magnet** | bash CLI | 活跃 v0.6 | 非前端：统一包管理器（pacman/yay/apt/dnf 经两个 distrobox 容器委托） |
| **Dockshade** | TUI（西语） | 活跃 | Kali 黑客工具场景，小众 |

**阴性结果**：无 Podman Desktop distrobox 扩展；无 GNOME Shell 扩展；无 GNOME Circle 应用；OBS 只带 distrobox 本体。

## 三、重点参照：DistroShelf（最完整的 API 消费方案例）

- **`CmdFactory`/`CommandRunner` 抽象 + faker/mock 命令层**（`src/fakers/command.rs`）——Flatpak 包装由 CommandRunner 实现注入，**自述"故意全走命令行以保证 flatpak 沙箱内也能访问文件与 env"**；**这行代码社区里最好的可测性实践，我们的 exec 层第一天就该注入化**
- 设置项可选 host 版 / **捆绑版 distrobox 可执行文件**（`distrobox_executable.rs`）——已有人考虑自带引擎
- v1.4.8 **移除了 rootful（特权）容器支持，"为更安全地创建"**——rootful 双重否决（+BoxBuddy ROADMAP）
- **可复制的命令日志**：显示它执行的每条 distrobox 命令——透明性当特性

## 四、Flatpak 边界模式（社区标准答案，四家 Flathub 应用一致）

| 应用 | finish-args 关键项 | 逃逸方式 |
|---|---|---|
| DistroShelf | **无任何 filesystem 授权** | `flatpak-spawn --host distrobox ...`（连宿主 `ls` 都走它） |
| BoxBuddy | `--filesystem=home` | `FLATPAK_ID`/`/.flatpak-info` 存在即前置 `flatpak-spawn --host` |
| Kontainer | `--filesystem=~/.local/share/applications:ro` + icons:ro | `flatpak-spawn --host /usr/bin/env <cmd>` |
| Apx GUI | `--filesystem=xdg-run/podman:ro` | flatpak-spawn + **python3-podman 直连 socket（只读）** |

模式：`--talk-name=org.freedesktop.Flatpak` + `flatpak-spawn --host` + 最小 filesystem 授权；权限缺失时功能优雅降级（BoxBuddy 文档明示）。

## 五、CLI 痛点引文（支撑结构化 API 设计的活证据）

- **BoxBuddy ROADMAP**："Needs External Help: Create Assemble ini files via GUI; Parse assemble .ini files and show confirmation pop-up; **Stream output of distrobox commands to the GUI (particularly during creation of a new box)**; Uninstall application from box."；"Rejected Ideas: **Rootful** — far too many password popups"
- **DistroShelf 设计注**："We do everything with the command line to ensure we can access the files and environment variables even when inside a flatpak sandbox"（无 API，只有 CLI + 文件系统约定——它们直接读 `~/.local/share/applications`）
- **distrobox-tui**："Currently it is not possible to create Distroboxes in the TUI"
- 上游：distrobox 1.8.2.x → **2.0.0-rc.4**（结构化 API 得追着移动靶）

---

## 六、容器 GUI 与沙盒生态全景（桌面/Web/TUI）

### A. 桌面容器/VM GUI

| 项目 | 栈 | 2025-26 状态 | 对我们的意义 |
|---|---|---|---|
| **Rancher Desktop** | Electron + Go | 活跃 v1.23.1；**2.0 重写 alpha**：单一守护进程 `rdd` 直接说 Kubernetes API，GUI 只是客户端，Lima VM 全平台 | 架构参照："GUI 是首等引擎 API 的薄客户端"最强实例 |
| **Pods** | Rust + GTK4/libadwaita | 活跃 **3.0.0 (2026-04)**（新多引擎后端 + 实验 Docker 支持） | **UX 参照/最近 GNOME 亲缘**：消费级非开发者容器 GUI；**unix socket REST 通信**；Flatpak 分发 |
| **Qocker** | Python + Qt | 低活跃 | 极简主义参照；**CLI 解析反面教材**（作者为支持 podman 而故意 shell out） |
| **DockerVue / Dockerman** | Tauri + Rust + React | 2025-26 新入 | 桌面容器 GUI 的新 Tauri 路线；Dockerman 借 **Portainer/CasaOS 模板格式**做应用市场 |
| **GNOME Boxes** | C + GTK4/libadwaita（重写 beta 2026-08） | 活跃 | **打包模式教科书**：单维护者靠"只发一个自包含 Flatpak"逃离发行版打包地狱；GNOME 认为消费级虚拟化=VM 而非容器 |
| **Kapsule** (KDE) | C++/QML | 早期 | **KDE 版 Incus 底座 distrobox 系**（共享 home/Wayland/PipeWire/D-Bus，Konsole OSC 777 集成） |
| **GNOME Containers 扩展** | JS | 活跃 v1.3.0a (2025-06) | **架构铁证**：明确从解析 `podman` CLI 输出迁移到 socket API——"socket API 是版本化且稳定的，每版本生成 OpenAPI spec" |
| **Incus** | Go | 活跃 **7.0 LTS（支持至 2031-06）+ OCI 容器支持** | 引擎替代者/相邻竞品：一个工具管 VM+系统容器+OCI 镜像，带 Web UI |
| **Lima/limactl** (+lima-gui) | Go CLI | 活跃 v2.2.0 | 引擎替代者（Rancher Desktop 2.0 全平台采用）；lima-gui 是薄壳 |

### B. Web 容器管理

- **Portainer**：活跃，Docker v29 事件后的幸存者；**其博客就是"谁活过 Docker 29"普查**
- **Cockpit + cockpit-podman**：活跃，**Quadlet（systemd 管理 Podman）已完全集成 RHEL Web 控制台 (2026-04)**——潜在集成伙伴
- **Dockge**：活跃 v1.5.0（Docker 29 修复合入）；Compose 栈 UX 参照
- **CapRover**：活跃慢节奏；**一键应用商店（~50-100+ 模板）——商店 UX 参照**，但服务端
- **Yacht**：**废弃**（2023 起）；"服务器去中心化应用商店"模板文化的开创者——也是尸检对象
- **CasaOS**：**停滞**（90 天 0 提交，团队转向闭源 ZimaOS）；34k★——"容器应用商店"需求证明 + 开源核心僵死案例
- **Olares**：活跃；k8s 级"Olares Market"（Helm OAC 格式 + 沙箱 ACL）——商店 UX 的 power-user 端证据

### C. TUI

- **Lazydocker**：Go，活跃 v0.25.2，~50k★；Docker 29 打爆硬编码 API 1.25 后修复
- **podman-tui**（containers 官方）：Go，活跃 v1.11.x；**走 `podman.socket` + SSH**——Red Hat 自家 TUI 也选 socket
- **k9s**：k8s-only；活跃——"专注 TUI 可成默认 UI"的基准
- 新入：**ducker / DockrTUI**（Rust）

### D. toolbox 系（引擎替代者/参照）

- **container-toolbox**：Go，活跃 0.3 (2025-09)；Fedora 官方；2026 有 usermod 回归事故——引擎基线须超越的对手
- **distrobox**：上游 1.8.2.x→**2.0.0-rc.4**；`assemble` 已加入
- **dbox**：**新入 (2025-12)**，Go，直呼 crun/runc、静态二进制、JSON 输出、Android——"更轻更少功能"信号，distrobox 系在碎片化
- **distrobox-plus**：Python 重实现，低活跃
- **unbox**：Rust 直用 namespace（进入 2ms vs 153-249ms），**2023-03 停滞**——架构好奇心，不可依赖
- **lupin**：无法核实（404，疑已删除/更名）
- **Bazzite/uBlue**：**distrobox 预装 + 自动更新 + DistroShelf 预装**；2025 起 -dx 变体捆绑 Docker/Podman/Incus/devbox；**百万级装机 = 我们最可能的分发渠道**

### E. 应用沙盒/商店生态（商店 UX 拼图）

- **Warehouse**（Python/GTK，活跃 2.2.0，~547k Flathub 安装）：**回滚/固定 runtime/掩蔽/快照/数据清理**——GNOME Software 缺的 power-user 操作全集，UX 直接抄
- **PinApp/Pins**（GTK，活跃 2.4.7）：**Flatpak/AppImage/Snap 三格式 .desktop 编辑器**——"让沙盒应用变原生"管道参照
- **FlatRun**（Rust，低活跃）："**不安装先运行**"（临时跑 Flathub 应用）——try-before-install 交互模式可偷
- **Flatpak Manager**（2025 新，Python）：终端用户向 Flatpak GUI
- **Snap Store GUI**：Flutter 版 App Center 有 track 混乱史——**商店 UX 翻车的警示**
- **AppImage 系**：**Gear Lever**（GTK，活跃 4.1.1）继承 **AppImageLauncher**（停滞）；in-place 更新 + 多版本 = 成熟模式；AM/AppMan/Zap = CLI 系
- **Nix/Guix GUI**：nix-software-center 近废弃（Nix 构建破碎）；guix-gui (2026-06 新, Rust+Iced)——**又一个数据点："引擎强但商店 GUI 死于维护"在每个生态重复**
- **Endless OS**：Flatpak-only 应用模型 + 离线内容库——"不可变 OS + 沙盒应用 + 商店"的消费级证明（但基座是 Flatpak 不是容器）

### F. 死亡项目尸检（死因高度一致：单维护者）

| 项目 | 死期 | 死因 |
|---|---|---|
| Kitematic | 2025-07 归档 | Docker 吸收进 Docker Desktop，停止资助 |
| DockStation | 2025-09 归档 | 单维护者 burnout；实际死 6 年 |
| Yacht | 2023 起废弃 | 单维护者 + Docker 29 API 上调终击；继任仓库未完成 |
| Watchtower | 2025-12 归档 | 维护者失趣；Docker 29 API 1.25→1.44 击杀 |
| Dockge | 2025-12 濒死 | Docker 29；1.5.0 修复 |
| CasaOS | 2026 停滞 | 团队转闭源 ZimaOS |
| AppImageLauncher | ~2021 停滞 | 被 Gear Lever 取代 |
| unbox / nix-software-center | 2023/2025 | 过早/单维护者 |
| DockerUI/Shipyard/Panamax | 2015-2018 | 被取代/无继任 |

## 七、"最接近我们产品"排名

1. **BoxBuddy / DistroShelf / Kontainer**：引擎 + 宿主启动器集成正好，**零商店面**（"安装应用"= 创建容器）
2. **Pods**：消费级 GNOME 容器 GUI + 正确通信模式（socket REST）+ Flatpak 分发；无商店、无启动器集成
3. **Yacht / CasaOS / CapRover**：唯一真有"容器一键应用商店"的项目，但**全是服务端 Web UI**，且三个里两个已死/停滞
4. **Podman Desktop**：桌面容器 GUI 在位者，但开发者工具定位
5. **GNOME Software / Snap App Center / Warehouse**：消费级商店 UX 应整段照抄，但绑 Flatpak/snap
6. **Olares / Endless OS**：OS 级"商店+沙盒+不可变基座"完整产品，基座不对
7. **Rancher Desktop 2.0 / Incus**：架构参照非 UX 竞品

**裁决：截至 2026-08，我们的产品不存在。** 最接近的活祖先 = BoxBuddy/DistroShelf/Kontainer（有引擎无商店）+ CasaOS/Yacht/CapRover（有商店无桌面集成）。空白真实存在。

## 八、教训（对 EasyTidy）

1. **引擎通信方式决定生死**：每个健康项目都走**版本化 socket REST API**（Pods、podman-tui、GNOME Containers 扩展；扩展明确从 CLI 解析迁走）。CLI shell-out 工具在每次版本升级时破裂。Docker v29 是 2025 大灭绝事件实证。**我们的 GUI 契约应建立在版本化 socket/结构化协议上，永不解析 CLI 文本**
2. **单维护者风险是第一产品风险**：死者全死于一人退出；生者全有组织背书。GNOME Boxes 路线：**Flatpak-first 首日分发**（只发一个自包含 Flatpak）是单人存活模式
3. **商店需求已被证明，供给全在服务端**：CasaOS 34k★、CapRover 一键商店、Olares Market；桌面宿主集成变体空缺——机会与警告并存：**商店需要目录维护（manifest/模板）作为一等工程承诺**
4. **引擎层已解决且更便宜**：Toolbox、distrobox、dbox、Incus OCI 都在做；**别写引擎，包 distrobox + `distrobox-export`**；搭 Bazzite/uBlue 分发便车（引擎+GUI 现成装机）
5. **宿主集成是消费者能感受到的差异化**：`distrobox-export --app` 让容器应用"与原生无异"、Gear Lever in-place 更新、Pins 跨格式 .desktop、Warehouse 回滚——活下来的工具都解决了**启动器/更新/清理**这三块无聊的胶水
6. **消费级商店 UX 有成熟拼图可抄**：GNOME Software 策展 + Warehouse 操作 + FlatRun 先试后装 + CapRover 两击目录
7. **安全叙事是卖点**：Yacht/CasaOS 挂 Docker socket（等同 root）是衰落诱因之一；rootless podman = "无 root 守护进程、无 socket 挂载"可验证差异化
8. **可测性经命令抽象**：DistroShelf 的 `CommandRunner` + faker 层是社区最佳实践；透明性（DistroShelf 命令日志、Kontainer 命令预览）当特性
9. **rootful 双否决**：BoxBuddy（密码弹窗）+ DistroShelf 1.4.8（"更安全地创建"移除特权容器）——默认 rootless，rootful 做显式警告的 opt-in
10. **为引擎变动做准备**：Rancher Desktop 2.0 标准化的 Lima、Incus 加 OCI、namespace 系 unbox——契约锁在一个稳定 socket API 上，产品才能扛住基座变动

## 来源

GitHub topic:distrobox 全部 86 仓库；Flathub 搜索 API；AUR RPC；四家 Flatpak manifest（flathub org 下 com.ranfdev.DistroShelf / io.github.dvlv.boxbuddyrs / io.github.DenysMb.Kontainer / org.vanillaos.ApxGUI）；DistroShelf/BoxBuddy/Kontainer 源码直读；Rancher Desktop 2.0 公告；Pods 3.0（ubuntuhandbook）；GNOME Boxes 重写（Phoronix）；GNOME Containers 扩展迁移（Fedora Discussion）；Incus 7.0（linuxcontainers forum）；Portainer Docker 29 尸检博客；CasaOS 停滞（zimaspace forum）；Bazzite 文档与 DX 公告；Warehouse（Flathub/FOSS Force）；Gear Lever/AppImageLauncher（CachyOS）；nix-software-center（NixOS Discourse）；guix-gui（pantherx）；BoxBuddy ROADMAP/tips；distrobox 上游 releases（2.0.0-rc.4）；LinuxLinks distrobox GUI 汇总；This Week in GNOME #206
