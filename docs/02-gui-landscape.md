# 02. Linux GUI 技术成熟度版图（2026）

> 第 2 轮（版图框架）+ 第 4 轮（长尾核实，2025–2026 来源）合并定稿。

## 判据：什么算"成熟"

1. **同形态生产级应用数量**：有没有消费级产品用它做成过事（对我们是"应用商店/管理器"形态）
2. **近两年维护活跃度**：发布节奏、上游生命力
3. **桌面集成成熟度**：Wayland、HiDPI、**中文输入法（CJK/IME）**、托盘、通知、全局菜单
4. **工具链与分发**：调试器、打包（deb/rpm/AppImage/Flatpak）、CI

## 定稿版图

| 梯队 | 成员 | 判决 |
|---|---|---|
| **T1 原生主流** | **GTK4/libadwaita、Qt 6** | 唯一真 T1。同形态先例 + 中文输入 + 原生 Wayland 全占 |
| **T1 准入门** | **Flutter (Dart)**、**Avalonia (.NET)** | 有真实生产消费应用；但 Flutter 走 GTK3 embedder + XWayland（GTK5 移除 X11 后面临天花板），Avalonia Linux Wayland 仍预览 |
| **T2 Web 混合** | Electron、Tauri 2、Wails、Neutralino | 成熟可用；webview 系中文输入有已知 bug |
| **T3 新兴** | Slint、Iced/libcosmic、egui、Xilem、Dioxus、LVGL | **2026 均无消费级成品**；Xilem/Dioxus 为 2027 观望对象 |
| **T4 长尾/衰退** | JavaFX、wxWidgets、EFL、Tk、Kivy、Dear ImGui、SDL+自绘、Fyne、Gio | 全部排除（详见下） |

## T1 详表：GTK4 vs Qt 6

| | **GTK（现役 GTK4）** | **Qt 6** |
|---|---|---|
| 血统 | GNOME 官方，C 核心（GIMP Toolkit 起源） | Qt/KDE，C++/QML |
| 语言生态 | 官方 C；绑定：Python(PyGObject 官方)、Rust(gtk-rs 官方)、Vala(官方)、Go(gotk4)、JS(GJS) | 官方 C++/QML；绑定：PySide6(官方)、PyQt6、cxx-qt(Rust) |
| 同形态先例 | **GNOME Software**、Flatseal、**BoxBuddyRS**（distrobox 前端） | **KDE Discover**、**Kontainer**（distrobox 前端）、VLC、OBS |
| 中文输入 | GTK IM 框架（fcitx5/ibus）；**有已知粗糙点**（见下） | Wayland text-input-v3 + fcitx5-qt 插件，**业界最佳** |
| 2026 状态 | GTK 4.22；GNOME 生态全面 GTK4；GTK3 维护模式（GIMP 3/Cinnamon/XFCE 仍在用）；**GTK5 ≈2027 移除 X11** | 6.11 当前；**6.8 五年 LTS（商业支持至 2029-10）**；KDE Plasma 6 全家桶 |
| 代价 | 绑定多为社区维护；观感锁定 GNOME | LGPL 授权（动态链接）；C++/Python 团队包袱 |

**⚠️ 关键事实（第 4 轮核实）：GTK4 输入法并非无瑕。** Chromium 于 2025-06 在 GNOME/X11 下**回退到 GTK3**，因 fcitx/ibus 在 GTK4 上仍不成熟。GNOME 自家应用（GTK4）中文输入正常，问题集中在特定控件/webview。GTK4 仍是原生最佳路径，但"开箱即用"要打折；Qt 的 Wayland IME 仍领先。

## T1-borderline 详表

### Flutter (Dart) — "准第一梯队，外卡选手"
- **版本/活力**：Flutter 3.44 当前；Linux 是官方 CI 支持目标。**重大治理事件：Google I/O 2026 上 Canonical 成为 Flutter 桌面主维护者**（自 2021 年就发 Flutter 应用）
- **生产消费应用**：Ubuntu App Center、Ubuntu Firmware Updater、Ubuntu Security Center（Ubuntu 24.04/26.04 实装）
- **Linux 集成**：官方 embedder **基于 GTK3**（GtkBox/GtkGLArea）；无 GTK4 移植（ATK→AT-SPI 迁移被承认为"重大工程"）；**无原生 Wayland（XWayland 运行）**；输入法经 `GtkIMContext`（X11/XWayland 下 fcitx5/ibus 可用）；HiDPI 好
- **分发**：无官方 deb/rpm 流水线；Canonical 推 Snap；社区工具（flutter_to_debian、AppImage）可用
- **判决**：成熟（T1 准入门）；深度系统集成（托盘/通知/原生 Wayland）仍需额外工作

### Avalonia (.NET/C#) — 生产记录扎实，Linux Wayland 弱
- **版本/活力**：11.3.x 活跃
- **生产应用**：Lunacy、JetBrains dotMemory/dotTrace、Unity Plastic SCM、PixiEditor、Stability Matrix、OpenUtau、Beutl 等（比 JavaFX/wxWidgets 今天更多）
- **Linux 集成**：X11/framebuffer 官方支持；**Wayland 私有预览**；CJK 字体需配置（HarfBuzz 塑形）；IME 仅 X11 成熟
- **判决**：成熟（T1 准入门，边缘）；Linux Wayland 是各家最弱

## T2 详表：Web 混合

- **Electron**：Chromium 完整打包（80–250MB，~100–500MB 空闲内存，1–4s 冷启动）。**Podman Desktop 用它**（容器 GUI 领域最接近我们的竞品）；生态最成熟（electron-builder/updater 差分更新）；Wayland 原生度弱
- **Tauri 2.x**：Rust + 系统 webview（Linux = WebKitGTK 4.1），2–10MB 体积、~30–50MB 内存、亚秒启动；AppImage 自更新成熟（deb/rpm 走系统包管理器）。**硬伤：WebKitGTK Wayland IME 上游 bug 未修**（Tauri issue #8264 open、WebKit bug #261795 合成文本不可见直至窗口 resize、keyCode-229 问题）；需逐用户配置规避（`GTK_IM_MODULE=fcitx`、`GDK_BACKEND=x11`、JS 层抑制 229 等）
- **Wails v3**：Go+webview；v3.0.0-beta.0（2026-08）未 GA；与 Tauri 共享 WebKitGTK 的 IME 问题；通知仍在 roadmap
- **Neutralinojs**：v6.8 维护中，无知名消费应用，小众

## T3 新兴（2026 均不可用于消费级成品）

- **Slint 1.17**：桌面仍"in progress"（嵌入式为主战场）；系统托盘/drag-drop 1.17 才落地
- **Iced/libcosmic**：Iced 0.14 仍官方"experimental"，单一维护者风险；**Bottles Next 重写选了 libcosmic——但那是重写进行时，无成品**；COSMIC DE 1.3 已发（旗舰案例）
- **egui 0.34**：工具/仪表盘观感；CJK 需嵌入字体；无消费级应用
- **Xilem 0.4**（2025-10）：Linebender，显式 alpha；2027 再看
- **Dioxus 0.7**（2025-10）：React 式 Rust + Blitz WGPU 渲染器；2026 年真正的新入局者，CSS 矩阵不完整，"性能非重点"——2027 观望
- **LVGL 9.5**：2025 大踏步（Wayland 驱动重写、EGL、NanoVG），但消费桌面生态为零，HMI/嵌入式定位
- **Servo**：转向"可嵌入 web 引擎"（0.0.1 2025-10），servo-gtk 原型存在；2026 不可用于生产

## T4 长尾（全部排除，含理由）

| 技术 | 2026 状态 | 排除理由 |
|---|---|---|
| **JavaFX/OpenJFX** | 活跃，25 LTS（2025-09），Oracle/Gluon 支撑 | Linux IME 历来最弱（fcitx/ibus 候选窗问题）；消费心智份额极小 |
| **wxWidgets** | 3.3.0（2025-06）维护中 | **无 wxGTK4 移植**（核心开发者亲述"we don't have a working wxGTK4 port"）；新开发无未来 |
| **EFL** | 1.28.0（2025-01）维护中 | 无第三方消费应用；嵌入式/Tizen 定位 |
| **Tk/ttk** | Tk 9.0 维护中，stdlib 承诺"至少再 15 年" | 观感过时；Wayland 无原生故事；无消费应用 |
| **Kivy/KivyMD** | 维护中（KivyMD 2025 有 Cairo 依赖断裂 issue #1842） | 移动/触控定位；桌面观感弱；无消费应用 |
| **Dear ImGui** | 1.92 极活跃，游戏引擎工具/overlay 海量采用 | **无 IME、无无障碍、非终端用户 UI**（明确）；仅工具/调试场景 |
| **SDL+自绘** | SDL 3.4 健康，Wayland text-input-v3 默认（IME bug #13086 已修） | 100% 自建 UI（控件/主题/IME/无障碍/HiDPI）；仅游戏类 UI 合理（RetroArch、MAME）。注：OBS 是 Qt6 不是 SDL 自绘 |
| **Fyne 2.8 / Gio 0.10** | 活跃 | 见 03 专文：Fyne 中文输入未解决；Gio Linux IME 未证实 + NVIDIA 限制 |

## 版图结论

1. **Linux 消费级 GUI 的"成熟"= GTK 与 Qt 两选一，加 webview 变体（Electron/Tauri），Flutter 为第三外卡**——其余都是研究性选项
2. **中文用户是两个硬指标之一**（另一个是消费级观感）：原生 GTK/Qt 输入法路径成熟（GTK4 有已知粗糙点，Qt 最佳）；webview 系有已知 bug
3. **同形态先例只有三家**：GNOME Software (GTK4)、KDE Discover (Qt)、Podman Desktop (Electron)
4. **我们领域的参照实现恰在两阵营**：BoxBuddyRS (GTK4/Rust)、Kontainer (Qt/C++)（见 05）

## 主要来源

- gotk4 repo（github.com/diamondburned/gotk4）；Fyne 2.8.0 发布、IME issues #4544/#4546/#618、PR #4614；Gio v0.10；Wails v3 beta
- Tauri 2.11 发布、IME #8264、Wayland #12361；Electron 43 发布计划；endoflife.date/qt（Qt 6.8 LTS → 2029-10）
- Flutter 3.44 + Canonical 接管（OMG Ubuntu/Thurrott）；Avalonia v11 supported platforms；OpenJFX 25 GA；wxWidgets 3.3.0 + #26324；EFL 1.28；Tk 9.0；KivyMD #1842；ImGui wiki；SDL #13086；Xilem 0.4；Dioxus 0.7；Servo 0.0.3（Phoronix）；Neutralino 6.8；Chromium GTK3 回退（2025-06）
