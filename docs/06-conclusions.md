# 06. 调研收敛结论与决策清单

## 一、铁定的架构方向（四轮调研共同收敛）

### 1. 引擎与 GUI 之间走结构化协议
- 两个成熟参照实现（Kontainer/BoxBuddyRS）约 **1/3 代码只用于解析 distrobox 的人类输出**，且这 1/3 正是 v1.8→v2.0 升级中损坏的部分
- 我们的引擎是自有 Go 分叉 → **JSON/NDJSON 结构化输出 + 生命周期事件流**是第一性要求
- 通信形态：GUI 子进程调用引擎（结构化协议）；守护进程（unix socket）留作未来选项
- 交互式操作（终端）由引擎提供干净的非交互式替代（流式输出 + 退出契约），"打开终端"是显式用户动作

### 2. 引擎重构优先级（源自 01）
| 优先级 | 事项 | 理由 |
|---|---|---|
| P0 | 提供 `list`/`create`(进度+结果)/`enter --non-interactive`/`export inventory`/`apps`/`stats` 的结构化输出 | 全部 GUI 痛点的下游 |
| P0 | 身份标签决策（沿用 vs 新命名空间） | 兼容性契约，越早定越省 |
| P1 | podman/docker 参数构建器合并（`podman.go:143-466` vs `docker.go:143-453`） | 最高杠杆重构目标 |
| P1 | 宿主机集成逻辑三层归一（创建期 Go / init shell / 进入期 env）→ 声明式"集成档案" | 最大重构风险，GUI 沙盒核心能力 |
| P1 | 错误对象化（code+message+stderr） | 替换布尔 only |
| P2 | 引擎拥有镜像目录（离线缓存） | 灭网络抓取依赖 |
| P2 | 引擎拥有生命周期事件 | 灭轮询/重绘 |
| P2 | rootful 一等公民化（若支持） | BoxBuddy 因 UX 拒绝，Kontainer 只有自由参数洞 |

### 3. GUI 技术栈收敛
- **候选收敛为 GTK4 与 Qt 二选一，Flutter 为外卡**。唯一三个同时满足：中文输入（Wayland IME）、同形态先例（应用商店）、原生 Wayland
- 各自代表路线：**gotk4**（全 Go 单语言，引擎进程内直连）vs **Qt/PySide6**（最佳 IME，引擎走子进程协议）vs **Flutter**（Canonical 接管，XWayland 天花板）
- GTK4 IME 有已知粗糙点（Chromium 2025-06 回退 GTK3 实证）；Qt Wayland IME 仍领先
- 出局：Tauri（中文输入已知 bug + 双语言）、Fyne（中文输入未解决）、Gio（IME 未证实 + NVIDIA 限制）、Electron（重量）、Rust 三剑客（无消费级成品）、Wails v3（beta + 共享 webkit IME 问题）

## 二、产品设计要点（源自参照实现解剖）

1. **应用 = 一个容器**，容器管理器为真相源；镜像层缓存提供天然磁盘去重（类 Flatpak 共享 runtime 效果）
2. **应用清单（manifest）格式**是产品核心资产（Flatpak manifest / Nix 风格声明式）——**尚未调研，属后续轮次**
3. **GUI 形态**：应用商店（浏览/搜索/安装/卸载/更新/运行）+ 容器管理（状态/统计/终端）
4. **创建选项结构化 + 校验 + 命令预览**（Kontainer 已验证用户喜爱），不做自由参数透传
5. **Flatpak 分发首日考虑**，沙箱边界放引擎
6. **差异化空间**：现有 distrobox GUI 都只是"容器管理器"；无人做"应用沙盒商店"——我们是第一个

## 三、待决决策清单

| # | 决策 | 选项 | 影响 |
|---|---|---|---|
| D1 | **GUI 技术栈** | a) gotk4（全 Go） b) Qt/PySide6 c) Flutter | 项目语言结构、引擎通信形态 |
| D2 | **容器身份标签** | a) 沿用 `manager=distrobox` + `distrobox.*` b) 新命名空间 | 是否继承用户既有 distrobox 容器（沿用=免费迁移；新=干净但孤儿化） |
| D3 | **rootful 支持** | a) 首版支持 b) 首版拒绝（BoxBuddy 路线） | 引擎提权架构、UX 设计 |
| D4 | **Flatpak 分发** | a) 首日支持 b) 原生包先行 | 引擎需承担 flatpak-spawn/portal 边界 |
| D5 | **产品命名/品牌** | 待定（仓库名 easy-tidy） | 身份字符串全局替换范围 |

## 四、后续路线（用户可选轮次）

- **A. 收敛技术栈**（D1 决策）→ 进入方案规划（plan mode）
- **B. UX 对标**：GNOME Software / KDE Discover / Bottles 界面与交互深挖（应用商店 UI 形态）
- **C. App 清单格式调研**：Flatpak manifest / Nix / OCI 生态设计应用描述与分发
- **D. 引擎协议设计**：起草结构化输出 + 事件流接口草案（P0 项）
- **E. 继续扩圈**：用户点名新领域

## 五、第 5 轮调研补充（相关项目全景，2026-08-05，详见 07）

### 新增的架构级证据
- **引擎通信方式决定生死（最猛实证）**：2025 年 Docker v29 API 最低版本上调（1.25→1.44）成批量灭绝事件（Watchtower/Yacht 死、CasaOS 停滞、Dockge 濒死、lazydocker 被爆）。健康项目全部收敛于**版本化 socket REST API**（Pods、podman-tui、GNOME Containers 扩展 2025-06 明确从 CLI 解析迁走，原话："socket API 是版本化且稳定的，每版本生成 OpenAPI spec"）；Rancher Desktop 2.0 重写为"守护进程说引擎 API，GUI 只是客户端"。
  → **D1 决策的新输入：GUI↔引擎契约应建立在版本化 socket/结构化协议之上**（我们引擎对外暴露之，而非让 GUI 解析 CLI 文本）；这同时使"引擎基座变动"（Lima/Incus OCI/dbox namespace 等）不影响产品存活。
- **rootful 双否决**：BoxBuddy ROADMAP（密码弹窗毁体验）+ **DistroShelf v1.4.8 移除特权容器支持**（"为更安全地创建"）→ D3 强烈倾向首版 rootless。
- **Flatpak 边界模式是社区标准答案**：`--talk-name=org.freedesktop.Flatpak` + `flatpak-spawn --host` + 最小 filesystem 授权，权限缺失时优雅降级；Apx GUI 还演示了**只读直连 podman socket** → D4 首日支持的成本比预期低，且是单维护者存活路线（GNOME Boxes 模式）。
- **可测性最佳实践**：DistroShelf 的 `CommandRunner` 抽象 + faker/mock 命令层（flatpak 沙箱测试模式）——我们的 exec 层第一天注入化。
- **空白确认**：截至 2026-08，"消费级桌面商店 UX + 容器引擎 + 宿主启动器集成"三合一无人做。最接近组合 = BoxBuddy/DistroShelf/Kontainer（有引擎无商店）+ CasaOS/Yacht/CapRover（有商店无桌面集成，且服务端、两死一停）。
- **分发渠道**：Bazzite/uBlue 预装 distrobox + DistroShelf（百万级装机）；DistroShelf 已有"捆绑版引擎"设想。
- **商店目录维护是硬承诺**：CasaOS/Yacht 目录随维护者而死；商店需要 manifest/模板目录作为一等工程资产。

### 待决决策增补
- D3 rootful：社区证据一致否决 → 首版默认 rootless，rootful 留作显式 opt-in（引擎层提供，GUI 警示）
- D4 Flatpak：社区模式已验证（flatpak-spawn + 最小授权）→ 支持成本低，建议首日支持
- D6（新）GUI↔引擎契约形态：版本化 socket API（对标 podman.socket 模式）而非 CLI 文本解析——从 D1 技术栈决策中独立出来单列

## 六、来源索引

- 01：DistroBox 源码直接分析（file:line 见正文）
- 02：网络调研（GTK4/Qt/Flutter/Avalonia/JavaFX/wxWidgets/EFL/Tk/Kivy/ImGui/LVGL/SDL/Xilem/Dioxus/Servo/Neutralino 官方源 + 2025-2026 新闻）
- 03：gotk4/Fyne/Gio/Wails 官方仓库、release notes、issues
- 04：Tauri/Electron 官方文档、Podman Desktop/Bottles/Flatpak 生态文档与新闻
- 05：Kontainer v1.6.1 与 BoxBuddyRS v2.5.8 源码直接分析 + GitHub issues
- 07：GitHub topic:distrobox 全量 + Flathub/AUR + 四家 Flatpak manifest + Rancher Desktop 2.0/Pods 3.0/GNOME Boxes/GNOME Containers 扩展/Incus 7.0/Portainer 尸检/CasaOS 停滞/Bazzite/Warehouse 等
