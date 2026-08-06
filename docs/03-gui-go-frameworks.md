# 03. Go 原生 GUI 框架深挖（gotk4 / Fyne / Gio / Wails）

> 2026-08 现状核实。用途：消费级 Linux 桌面"应用商店"GUI（应用列表、安装/卸载、进度条、设置），需良好中文输入。

## 对比总表

| | **gotk4** (GTK4 绑定) | **Fyne** | **Gio** | **Wails v3** (webview) |
|---|---|---|---|---|
| 最新版本 | v0.3.1 (2024-08)；git 主线活跃（2026-07-30 对 GTK 4.22.4 再生成绑定） | **v2.8.0 (2026-07-08)**，月度节奏 | v0.10.1 (2026-07) | v3.0.0-beta.0 (2026-08)；v2.12.0 稳定线 |
| 维护状态 | 活跃但**两年无 tag**（滚动发布，需钉 commit） | 非常活跃 | 稳定 | v2 稳定，v3 beta |
| 范式 | 保留式（真 GTK4） | 保留式（自绘） | **即时模式**（自绘 GPU） | Web（HTML/CSS/JS） |
| Wayland | 原生（GTK4） | 经 GLFW（2.5 完成） | 原生（默认驱动） | 经 WebKitGTK |
| HiDPI | 原生分数缩放 | 自有缩放系统 | 自有缩放 | Web 层处理 |
| 系统托盘 | 经 appindicator 库 | 内置（StatusNotifierItem） | 社区插件 | 内置 |
| 通知 | libnotify/DBus | 内置（DBus + portal） | 社区插件/DBus | **roadmap，未内置** |
| **CJK 输入 (IME)** | **原生 GTK IM（fcitx5/ibus）——最佳** | **有问题**（GLFW 无 IME，open issues #4544/#4546/#618；PR #4614 需打 GLFW 补丁未合并；X11 有部分 XIM，Wayland 无 text-input-v3 路径） | 不明确/有风险（有 wayland text-input 绑定但无实证；XFilterEvent 路径） | 大概率可用（WebKitGTK 继承 GTK IM） |
| CJK 渲染 | Pango + 系统字体 | 2.5+ 字体回退，常需 `FYNE_FONT` | go-text，**必须捆绑 CJK 字体** | Web 字体，简单 |
| 构建 | **cgo + libgtk-4-dev + gobject-introspection**；首次编译慢（分钟级至 15min+） | 纯 Go（自带 GLFW） | 纯 Go（需 wayland/xkbcommon/GLES/EGL 开发包） | Go + WebKitGTK + 前端工具链 |
| 二进制 | 小（~5-10MB）需 GTK4 运行时 | ~14-18MB 自包含 | ~5-15MB 需 GL（**不支持 NVIDIA 闭源驱动**） | ~15MB 需 WebKitGTK |
| 无头 CI/devcontainer | apt 装依赖可构建；测试需 xvfb | **最佳：官方无显示 `test` 驱动** | E2E 无头测试存在（X11/Wayland/Sway） | 构建可无头；测试需 xvfb |
| 生态 | 小（687★，真应用少） | 最大（月度发布，大量应用） | 小但真实 | 最大（v2 应用多）；v3 年轻 |
| 许可 | 生成代码 **MPL-2.0**（生成器 AGPL-3.0 不影响产物） | BSD-3 | MIT | MIT |

## 逐个结论

### gotk4 — 唯一给出"中文输入 + 原生观感"的 Go 选项
- 自动生成自 GObject-Introspection；README 坦言"大部分 API 可用，某些部分内存泄漏/崩溃/缺失"；需 Go 1.21+
- 全套 GTK4 控件：`ListView`/`ColumnView` + factory 直接适配应用列表；gotk4-adwaita（63★）提供 libadwaita
- 分发：单二进制 + 系统 GTK4/libadwaita 运行时依赖；无内置打包工具（走 AppImage/deb/rpm/Flatpak 标准工具）
- **判决：Go 选项中最优**。代价：cgo 构建依赖、首次编译慢、滚动发布需钉 commit、社区小。GTK-only（本产品仅 Linux，可接受）

### Fyne — 最成熟纯 Go，但中文输入是硬伤
- 2.8.0 为"自 2.0 以来最大发布"（新 canvas 对象、硬件加速阴影、定时通知 API、无障碍、多显示器）
- 外观：自绘控件——干净一致但**明显非原生**（标准批评点）
- **CJK 输入是死穴**：GLFW 无 IME 支持；Wayland 无 text-input-v3 路径；社区 IME PR 需要打 GLFW 补丁（未合并）。对中文用户产品= 先做 spike，不过则出局
- `fyne package` 生成 .desktop + 图标；AppImage 支持；无原生 deb/rpm/flatpak 输出
- **判决：如果 cgo/GTK 系统依赖不可接受时的后备**，前提是中文输入 spike 通过

### Gio — 技术最炫，风险最高
- 即时模式，全部 Go；最陡学习曲线；自有材质组件（`gioui.org/x/component`）
- **已知限制：不支持 NVIDIA 闭源驱动**（README 明示）——消费 Linux 硬件的实际问题
- Linux IME 无文档/实证；CJK 字体需捆绑（NotoSansCJK ≈ 20MB+）
- 无消费级成品案例
- **判决：本产品排除**

### Wails v3 — 不是 Go 原生 UI
- v3 beta 未 GA；桌面 API 已稳定但官方仍推 v2
- 共享 WebKitGTK（与 Tauri 同栈）→ 中文输入同样有风险；通知未内置
- **判决：第二选择**；且按"Go 原生 UI"的定义不算数

### 其他 Go 选项
- **therecipe/qt：已死**（无 Qt6 适配，2022 后不更新）。继任者（cutego/gqtx/miqt）均早期/小众——**Go + Qt 在 2026 不可行**
- **puregotk**（纯 Go GTK 绑定，无 cgo，ChairLift 在用）：比 gotk4 小且年轻，不足为主导，可留意

## 综合判决

1. **首选 gotk4（GTK4 + gotk4-adwaita）**：免费获得消费 Linux 应用所需的一切——原生 Wayland/HiDPI、成熟 fcitx5/ibus 中文输入、Pango CJK 渲染、通知、标准 .desktop/AppStream 集成、libadwaita 原生观感（GNOME Software 同款）
2. **后备 Fyne 2.8**：若团队不接受 cgo/GTK 系统依赖（纯 Go、自包含 ~15MB、官方无头测试驱动最佳、月度发布）；前提是先过中文输入 spike
3. **无论选哪个，引擎/应用逻辑保持纯 Go 包 + 无头单测**——绑定层慢与怪不阻塞逻辑测试
