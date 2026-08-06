# EasyTidy 调研文档库

基于 DistroBox 分叉开发的 **GUI 程序沙盒工具**（类 Flatpak，面向个人 Linux PC 用户）的调研沉淀。

> 调研周期：2026-08，共 4 轮。本目录所有结论均来自对 DistroBox 源码的直接分析 + 网络调研（2025–2026 现状核实），并附来源。

## 文档导航

| 文档 | 内容 | 调研轮次 |
|---|---|---|
| [00-overview.md](00-overview.md) | 项目背景、产品定位、调研结论速览 | — |
| [01-engine-architecture.md](01-engine-architecture.md) | DistroBox 代码库架构图谱（引擎可复用性评估 + 重构面） | R1 |
| [02-gui-landscape.md](02-gui-landscape.md) | Linux GUI 技术成熟度版图（含长尾技术 2026 现状核实） | R2+R4 |
| [03-gui-go-frameworks.md](03-gui-go-frameworks.md) | Go 原生 GUI 框架深挖（gotk4 / Fyne / Gio / Wails） | R1 |
| [04-gui-alternatives-competitive.md](04-gui-alternatives-competitive.md) | 非 Go 方案（Tauri/Electron/Slint/Iced）+ 竞品调研 | R1 |
| [05-reference-kontainer-boxbuddy.md](05-reference-kontainer-boxbuddy.md) | 参照实现解剖：Kontainer (Qt) / BoxBuddyRS (GTK4) | R3 |
| [06-conclusions.md](06-conclusions.md) | 收敛结论、待决决策清单、后续调研/实施路线 | — |
| [07-related-projects.md](07-related-projects.md) | 相关项目全景：distrobox GUI 前端普查 + 容器 GUI/沙盒生态 + 死亡项目尸检 | R5 |

## 一句话结论

**引擎（DistroBox Go 分叉）已具备库化基础；GUI 与引擎之间必须走结构化协议（JSON/NDJSON + 事件流）——这是避开 BoxBuddy/Kontainer 全部痛点（1/3 代码用于解析人类输出、被 1.8→2.0 升级打爆）的唯一路线。GUI 栈收敛为 GTK4 与 Qt 二选一（+Flutter 外卡），待最终决策。**
