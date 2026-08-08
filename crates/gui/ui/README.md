# CJK/IME Spike（M0 门禁）

验证 Tauri（WebKitGTK）在中文输入法下的可用性。**这是 D1 决策门禁**：通过 → GUI 走 Tauri；失败 → 降级 gtk4-rs + VTE。

## 前置条件（必须在真实宿主机，不能是 devcontainer）

- **WebKitGTK ≥ 2.44**：Fedora 41+ / Ubuntu 24.04+ / openSUSE Tumbleweed（Debian 12 的 2.42 不合格）
- GNOME 会话（Wayland 优先，X11 为对照）
- 输入法已安装启用：**fcitx5**（fcitx5-gtk）或 **ibus**
- 构建依赖：node/pnpm、rust、libwebkit2gtk-4.1-dev 等（`pnpm tauri build` 会提示缺什么）

## 构建与运行

```bash
cd spikes/cjk-spike
pnpm install
pnpm tauri build          # 产物：src-tauri/target/release/spikescjk-spike
# 或开发模式：pnpm tauri dev

# 运行前记录环境（填进测试表）：
env | grep -E 'IM_MODULE|XMODIFIERS|XDG_SESSION_TYPE'
```

## 测试矩阵

测试目标：**A** `<textarea>`（普通输入框）、**B** contenteditable、**C** xterm.js 终端（组合期间不应吞键/幻影 Enter）。每行填 ✅/❌ + 备注。

| # | 会话 | IM | GTK_IM_MODULE 设置 | 规避措施 | A | B | C | 备注 |
|---|---|---|---|---|---|---|---|---|
| 1 | GNOME Wayland | fcitx5 | `GTK_IM_MODULE=fcitx` | 无 | | | | |
| 2 | GNOME Wayland | fcitx5 | `GTK_IM_MODULE=fcitx` | `GDK_BACKEND=x11`（XWayland） | | | | |
| 3 | GNOME Wayland | ibus | `GTK_IM_MODULE=ibus` | 无 | | | | |
| 4 | X11 会话 | fcitx5 | `GTK_IM_MODULE=fcitx` | 无 | | | | |
| 5 | Wayland | fcitx5 | 默认（未设置） | 无 | | | | 基线 |

### 逐项判定标准（每条测试行的 A/B/C 各自满足）

1. **候选窗**：候选窗出现且跟随光标
2. **可见性**：合成文本即时可见——**无需 resize 窗口**（WebKit bug 261795 的典型症状是"看不见，resize 后出现"）
3. **提交**：提交后文本完整进入目标
4. **终端专项（C）**：组合期间按 Enter 不产生幻影回车；组合完成后无 keyCode-229 泄漏（观察终端内的 `[KEY] keydown suppressed` 日志应为组合期间才有）；回显与输入一致
5. **焦点恢复**：窗口失焦再聚焦后，输入法状态正常

### 规避栈（按序尝试，记录哪条生效）

1. `GTK_IM_MODULE=fcitx`（或 `ibus`）+ `XMODIFIERS=@im=fcitx`（写进 .desktop Exec 的 env 或启动包装）
2. `GDK_BACKEND=x11`——对 no-repaint bug 的"最可靠修复"（接受 XWayland 代价）
3. `~/.config/gtk-3.0/settings.ini` 与 `gtk-4.0/settings.ini` 写 `gtk-im-module=fcitx`
4. JS 层：组合期间抑制 `keyCode===229` 的 keydown（spike 已内置日志，正式 GUI 在终端桥实现）
5. `WEBKIT_DISABLE_DMABUF_RENDERER=1`（残留绘制问题）；`WEBKIT_DISABLE_COMPOSITING_MODE=1`（最后手段）

## 门禁判定

**通过** = 至少存在一行测试中，**A 与 C** 在"无 XWayland 强制"（即纯 Wayland）下满足全部判定标准；或在 XWayland 强制下 A/C/B 全部满足且无 229 泄漏。

**失败** = 任何配置都无法让 Wayland 组合可用；文本不可见问题无法消除；终端 Enter/IME 冲突不可解。

## 报告模板（跑完填）

```markdown
## CJK Spike 结果（日期/宿主发行版/WebKitGTK 版本）
| # | 会话 | IM | A | B | C | 生效的规避措施 |
|---|---|---|---|---|---|---|
| 1 | Wayland | fcitx5 | | | | |
...
**判定**：✅ 通过 / ❌ 失败 —— 依据：……
```
