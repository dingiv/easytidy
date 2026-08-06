# 10. M0 CJK/IME Spike 报告（2026-08-06）

## 结论：门禁通过 —— D1 = Tauri 2 + React

**判定**：✅ 通过。在 NVIDIA 宿主上需禁用 WebKitGTK 的 DMABUF 渲染器（运行时检测注入，类 clash-verge-rev）。原生 Wayland + 该规避下，渲染稳定、中文输入可用、终端可用。

> 历程注记：调查中途一度因 XWayland 路径稳定卡死而误判失败、拟降级 gtk4-rs；后以 clash-verge-rev（本机同后端稳定运行）为证重测原生 Wayland + DMABUF 禁用组合，并通过。最终采用该组合。过程中的错误教训（见下）已沉淀。

## 环境

- 宿主：Ubuntu 25.10，GNOME Wayland，NVIDIA GeForce RTX 5070 Ti（GB203，闭源驱动），AMD 核显
- WebKitGTK 2.52.3；fcitx5（+ ibus 备用）
- spike：`spikes/cjk-spike/`（Tauri 2 vanilla-ts + xterm.js 6）；生产构建为 AppImage

## 通过配置（正式 GUI 直接采用）

```
WEBKIT_DISABLE_DMABUF_RENDERER=1   # NVIDIA 必须；运行时检测注入（非环境变量）
GTK_IM_MODULE=fcitx                # 输入法（ibus 同理）
XMODIFIERS=@im=fcitx
# 不设 GDK_BACKEND → 走原生 Wayland（XWayland 会卡死）
```

正式 GUI 在 `main.rs` 入口处（Tauri init 前）做 NVIDIA 检测 + `set_var("WEBKIT_DISABLE_DMABUF_RENDERER","1")`，参考 clash-verge-rev `src-tauri/src/utils/linux/workarounds.rs`（检测路径：`/proc/driver/nvidia/version`、`/sys/module/nvidia*`、`/sys/class/drm/card*/device/vendor == 0x10de`）。

## 测试矩阵与结果

| # | 后端 | IME | DMABUF | 渲染 | 终端 | 备注 |
|---|---|---|---|---|---|---|
| 1 | 原生 Wayland | fcitx | 禁 | ✅ 稳定 | ✅ | **通过组合** |
| 2 | XWayland (`GDK_BACKEND=x11`) | fcitx | 禁 | ❌ 卡死（多次） | — | XWayland + WebKitGTK + NVIDIA 死锁 |
| 3 | 原生 Wayland（早期） | fcitx | 未禁 | 候选框漂移 | Enter 回显需显式 \r\n | 未达可用，故引入 DMABUF 禁用 |

## 关键事实

1. **渲染稳定性只取决于后端 + DMABUF**：原生 Wayland + DMABUF 禁用 = 稳定（clash-verge-rev 同配方同机实证 + spike 长测通过）
2. **XWayland 是禁区**：XWayland + WebKitGTK + NVIDIA 闭源驱动 = 渲染线程冻结，`WEBKIT_DISABLE_*`、`LIBGL_ALWAYS_SOFTWARE` 均无效。正式 GUI 绝不可设 `GDK_BACKEND=x11`
3. **IME**：fcitx5 在原生 Wayland 下工作；候选框左右跟随光标，竖直方向有小 offset（WebKit Wayland IME 已知偏差，可接受/可微调）。终端内组合选词 Enter 不双发
4. **终端**：xterm.js 6（WebGL 渲染器 + context 丢失降级 DOM，canvas 渲染器 6.0 已移除）；onData 回显需显式 `\r→\r\n` 或 `convertEol`
5. **dev vs prod 不影响**：卡死与构建模式无关，仅与后端/DMABUF 有关

## 工程教训（调查中的错误）

- **HMR 会污染 xterm 状态**：dev 模式下编辑 main.ts 触发热重载，对同一元素二次 `term.open()` 导致终端空白。修复：HMR `dispose` 时先 `term.dispose()` + 清空 DOM（spike 已实现，正式 GUI 同理需处理）
- **`tauri build` 会 patch 裸二进制**：打包后 `target/release/<bin>` 被注入 bundle 标记，脱离 AppImage 直接跑会自检失败。直接分发用 AppImage，开发用 `tauri dev`
- **不要因单一后端失败就否定平台**：XWayland 卡死不等于 WebKitGTK 不可用——需在多后端/多配置下系统排查，并以同机生产应用（clash-verge）作为可用性反证

## 受影响面

- **GUI 壳：Tauri 2 + React**（保留）；正式 main.rs 内置 NVIDIA → DMABUF 禁用
- **core / protocol / server：零影响**（门禁前置在 GUI 开发前，正是为此）
