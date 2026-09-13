# 20 · GUI 容器直通：依赖全景与实现设计

> 2026-09-14 · 梳理 GUI 容器的完整依赖清单，给出 easytidy 的实现映射与新增
> labwc 嵌套桌面场景的方案。现状实现见 `gui-passthrough.yaml`（三段规则）+
> `core/env/gui.rs`（运行时注入）。

## 1 · 依赖全景（按层级）

一个 GUI 应用在容器里"看起来像原生跑"，依赖从底到顶七层。**缺顶层只损失
功能，缺底层直接起不来**。

### L1 显示协议（硬需求，二选一或都要）

| 传递物 | 覆盖 |
|---|---|
| `WAYLAND_DISPLAY` env + socket（在 `$XDG_RUNTIME_DIR` 下） | Wayland 原生应用 |
| `DISPLAY` env + `/tmp/.X11-unix/X*` + `XAUTHORITY` | X11 应用（含 Wayland 会话下的 XWayland 客户端） |

现代应用（Chrome/GTK4/Qt6）双协议自适应；老应用（Electron 旧版、Java、Wine）
只有 X11。**除非明确限定，双协议都传**是省心解。

### L2 运行时目录（Wayland 生态的隐形依赖）

- `$XDG_RUNTIME_DIR` **整目录挂载**而非单个 socket：PipeWire、pulse、dbus
  会话总线、wayland-1 都是它的兄弟节点，且应用运行中可能动态发现；
- **DBus 会话总线**（`$XDG_RUNTIME_DIR/bus`）：单实例激活
  （`org.freedesktop.Application`）、通知、托盘、portal——现代 GUI 的粘合剂；
- `/run/dbus/system_bus_socket`（可选）：NetworkManager 网络状态、UPower、
  logind。socket 权限 `srw-rw-rw-`（others 可连，属主显示 nobody 属正常——
  宿主 uid 0 不在容器映射内），连接后按 uid 1000 过总线策略。

### L3 用户身份与配置

- **keep-id**（uid/gid = 宿主用户）：否则宿主挂载目录全是"别人"的；
- **passwd/group 条目 + home 存在**：GTK/Chrome 启动时 getent 解析；缺失则
  VS Code attach、dconf 等连锁失败（见 2026-09-13 两次修复）；
- **GSettings/dconf**：GTK 应用的设置存储，含 `button-layout` 这类细节键
  （prepare 恒写 `:minimize,maximize,close`，见 `ensure_button_layout`）。

### L4 渲染与输入

- GPU：`/dev/dri/renderD*`（AMD/Intel Mesa + VAAPI）；NVIDIA 走 CDI
  （`nvidia.com/gpu=all`）+ `NVIDIA_DRIVER_CAPABILITIES`；
- 输入类设备按需：`/dev/input/*`、`/dev/hidraw*`（手柄）、`/dev/kfd`（ROCm）。

### L5 多媒体

- **PipeWire** socket（`$XDG_RUNTIME_DIR/pipewire-0`）：音频 + 屏幕共享底层；
- legacy **PulseAudio**（`$XDG_RUNTIME_DIR/pulse/native`）：大量应用仍只认它；
  宿主 pipewire-pulse 兼容两者，挂 `$XDG_RUNTIME_DIR` 即天然可达；
- **字体/图标**：不挂宿主的 → 豆腐块 + 托盘图标缺失（已挂 `/mnt/host/*` +
  fontconfig local.conf + `XDG_DATA_DIRS` 追加，见 yaml 注）。

### L6 桌面集成（可选，"像原生"的关键）

- **xdg-desktop-portal + 实现**（gtk/wlr）：文件选择对话框、屏幕共享选择器、
  截图。浏览器屏幕共享的必经之路；
- 应用登记：`.desktop` + 图标（easytidy passthrough 导出已覆盖）；
- 语言/locale：`LANG` + 容器内 locale 生成（否则输入法/日期格式异常）。

### L7 输入法（中文场景）

- fcitx5 经会话总线 + `GTK_IM_MODULE=fcitx` / `QT_IM_MODULE` env 工作；
- 同 uid + `$XDG_RUNTIME_DIR` 挂载时，容器内应用天然可达宿主 fcitx5 的
  dbus 服务——多数情况无需额外动作，只需补 IM module env。

## 2 · 场景矩阵与注入策略

| 场景 | gui_x11 | gui_wayland | 注入内容 | 典型用途 |
|---|---|---|---|---|
| **Wayland 单协议** | ✗ | ✓ | shared + wayland 段 | 宿主 Wayland 会话 + 纯 Wayland 应用（现代 Chrome `--ozone-platform=wayland`） |
| **X11 单协议** | ✓ | ✗ | shared + x11 段 | 宿主 X11 会话（旧发行版/NVIDIA 老驱动）或纯 X11 应用 |
| **双协议** | ✓ | ✓ | shared + 两段 | 默认推荐：应用自适应选择，X11 兜底 Wayland |
| **单应用直通** | 按需 | 按需 | 上三行之一 + entry 定向 | 容器只跑一个 entry 应用（chrome 模板形态），无桌面 |

补充规则：

- **单应用直通**不需要容器内合成器/托盘——应用窗口由宿主合成器直接管理
  （Wayland）或经 XWayland（X11）。通知/文件对话框如需 portal，容器内要装
  `xdg-desktop-portal-<impl>` 且能连到宿主总线；
- **双协议会话下的 X11 兜底**：宿主 Wayland 会话的 XWayland 由宿主管理，
  容器挂 `/tmp/.X11-unix` 即可达；`XAUTHORITY` 用稳定间接路径
  `/run/easytidy/xauthority`（server 维护软链，见 gui-passthrough.yaml 注）；
- env 缺失跳过语义：`${DISPLAY}`/`${WAYLAND_DISPLAY}` 宿主没设 → 该挂载/env
  自动跳过（yaml 规则已实现），因此**同一模板天然适配 X11 宿主与 Wayland
  宿主**。

## 3 · 新场景：labwc 嵌套桌面（容器内合成器）

### 形态

容器内跑完整 Wayland 桌面（labwc + waybar + pcmanfm），**对宿主只暴露一个
Wayland 连接**——labwc 本身是宿主合成器（Mutter/Sway）的一个嵌套客户端。

```
宿主 Mutter ──(wayland socket)── 容器 labwc ──┬── 容器内应用（chrome/foot/pcmanfm…）
                                              └── labwc 自绘标题栏（libdecor CSD）
```

### 约束与收益

- **只传 Wayland**（gui_wayland=true，gui_x11=false）：labwc 作为 toplevel
  客户端连宿主；容器内 X11 应用可由**容器内 XWayland**（labwc 配置
  `xwayland` 或独立 launch）服务——**不挂宿主 `/tmp/.X11-unix`**，避免两条
  X 服务打架；
- **标题栏**：Mutter 不给嵌套 toplevel 画 SSD → wlroots 需 libdecor CSD 补丁
  （`container-gui-docs/patches/wlroots-0.18.2-libdecor-csd.patch`，构建链
  wlroots→labwc 全在容器内）；
- **窗口按钮**：GNOME `button-layout` 默认只有 close → prepare 已恒写
  `:minimize,maximize,close`（`ensure_button_layout`）；
- **收益**：容器内应用获得**容器内合成器**的完整桌面体验（多窗口平铺/托盘/
  壁纸），宿主只看到一个 labwc 大窗口；隔离边界更清晰。

### 注入差异（对照单应用直通）

| 项 | 单应用直通 | labwc 嵌套 |
|---|---|---|
| Wayland socket | ✓（应用连宿主） | ✓（**labwc** 连宿主） |
| X11 socket（宿主） | 按需 | ✗（容器内自管 XWayland） |
| 字体/图标/keep-id/XDG_RUNTIME_DIR | ✓ | ✓（同 shared 段） |
| GPU | ✓ | ✓（labwc 与应用都要） |
| entry | 应用本体 | labwc（+ autostart 拉起应用） |
| 依赖包 | 按应用 | labwc/wlroots(带补丁)/libdecor-gtk/waybar/pcmanfm |

### 实现落点

1. **flavor 模板 `labwc`**（已加 `crates/gui/assets/labwc.eg.yaml`）：
   `gui_wayland: true` + `gui_x11: false` + entry=labwc + GPU + XDG_RUNTIME_DIR；
   setup 段预留 wlroots/labwc 构建与 autostart 安装（补丁未固化前可挂载
   宿主预构建产物）；
2. passthrough 规则无需改：三段规则天然支持"只开 wayland 半"；
3. 待办：wlroots/libdecor 补丁与构建链固化（base 镜像或 setup 脚本）；
   labwc autostart 模板（waybar/pcmanfm）。

## 4 · 依赖 × 场景对照表

| 依赖 | 单应用 Wayland | 单应用 X11 | 双协议 | labwc 嵌套 |
|---|---|---|---|---|
| WAYLAND_DISPLAY + socket | ✓ | — | ✓ | ✓（labwc） |
| DISPLAY + X11 socket + XAUTHORITY | — | ✓ | ✓ | 容器内 XWayland |
| $XDG_RUNTIME_DIR 挂载 | ✓ | ✓ | ✓ | ✓ |
| 会话总线（bus） | ✓ | ✓ | ✓ | ✓（容器内进程用） |
| 系统总线（可选） | 按需 | 按需 | 按需 | 按需 |
| keep-id + passwd/home | ✓ | ✓ | ✓ | ✓ |
| GPU renderD / CDI | 按需 | 按需 | 按需 | ✓ |
| 字体/图标挂载 | ✓ | ✓ | ✓ | ✓ |
| PipeWire/Pulse | 音频应用 | 音频应用 | 音频应用 | 容器内 pipewire |
| xdg-desktop-portal | 按需 | 按需 | 按需 | ✓（wlr 实现） |
| IM module env | 中文 | 中文 | 中文 | 容器内 fcitx5 |

## 5 · 后续待办（按优先级）

1. **PipeWire/Pulse 直通规则**：`$XDG_RUNTIME_DIR` 已挂载即天然可达，补
   `PULSE_SERVER=unix:$XDG_RUNTIME_DIR/pulse/native` env 于 shared 段（缺失
   自动跳过）；
2. **labwc flavor 实测**：容器内构建 wlroots(+补丁)/labwc，setup 固化；
3. **IM module env**：shared 段补 `GTK_IM_MODULE`/`QT_IM_MODULE`/`XMODIFIERS`
   （值经宿主探测或模板给定）；
4. **xdg-desktop-portal**：容器内装 wlr 实现 + `PORTAL_DIR` 配置，屏幕共享
   走容器内 screencast（labwc 场景）。
