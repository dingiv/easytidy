# 09. M0 环境决策记录（2026-08-05）

## 环境事实

| 项 | 状态 |
|---|---|
| devcontainer | Debian 12 (bookworm)，node v20，systemd 252 二进制存在但 **user session offline** |
| Rust | rustup 1.97.1 stable 已装（M0） |
| podman | 4.3.1 + catatonit（tini 0.1.7_catatonit）已装（M0） |
| userns | `unshare --user` → EPERM；**devcontainer 的 seccomp 在容器层拦截 CLONE_NEWUSER**，容器内不可解锁（rootful 同样失败） |
| podman.socket | 依赖 systemd user session（offline）→ devcontainer 内不可用 |

## 决策

1. **devcontainer = 构建/单测环境；podman e2e = 真实宿主**（遵循实施计划风险表第 1 行）
   - M1/M2 的集成验证在真实 Linux 宿主跑（Fedora 41+/Ubuntu 24.04+）
   - devcontainer 内用 mock podman socket（compat API 小 HTTP server）做引擎单测
2. **CJK/IME spike 必须在真实宿主执行**（Debian 12 的 WebKitGTK 2.42 低于 2.44 门槛）
   - spike 应用：`spikes/cjk-spike/`，构建产物 + 测试矩阵清单交付用户在其 GNOME/Wayland 机器上跑
3. podman 版本注意：devcontainer 装的是 4.3.1（旧）；真实宿主建议 podman ≥5.x——events header 延迟 bug（#23712）只在 ≥5.2.1 复现，M1 的 events 重连逻辑以真实宿主为准

## 待用户确认

- [ ] easytidy/ 是否单独 `git init`（当前 workspace 无 git 仓库；distrobox 有自己的 .git）
