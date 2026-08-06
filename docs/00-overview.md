# 00. 项目背景与调研概览

## 产品定位

- **形态**：GUI 程序沙盒（类 Flatpak）——用户通过图形界面安装、运行、卸载容器化的 GUI 应用
- **底座**：DistroBox（Go v2 版）独立分叉，作为容器引擎
- **目标用户**：个人 Linux PC 普通用户
- **沙盒模型**：**宽松共享**（DistroBox 哲学）——默认共享用户目录、X11/Wayland、GPU、音频；隔离的是系统层；安装/删除不触碰用户数据
- **与上游关系**：独立分叉（不跟踪上游），伴随工程化重构

## 调研问题

1. DistroBox 代码库的架构与可复用性？（→ 01）
2. Linux 平台上有哪些成熟的 GUI 技术？（→ 02）
3. Go 原生 GUI 框架的成熟度？（→ 03）
4. 非 Go 方案与同类竞品怎么选型、GUI 与引擎怎么通信？（→ 04）
5. 现有 distrobox GUI 前端（Kontainer/BoxBuddyRS）的架构、痛点与启示？（→ 05）

## 调研结论速览（详见 06）

| 结论 | 要点 |
|---|---|
| 引擎可库化 | `pkg/containermanager`、`pkg/config`、`pkg/manifest`、`pkg/ui` 已 CLI 无关；最高杠杆重构目标 = podman/docker 参数构建器合并 + 宿主机集成逻辑三层归一 |
| 结构化协议是命门 | 两个成熟 GUI 的 1/3 代码只用于解析 distrobox 人类输出，且正是这部分在 v1.8→v2.0 升级中损坏；引擎必须输出 JSON/NDJSON + 生命周期事件 |
| GUI 栈收敛 | GTK4 与 Qt 二选一（中文输入 + 同形态先例 + 原生 Wayland 全占）；Flutter（Canonical 接管后）为外卡；webview 系因中文输入已知 bug 出局 |
| 参照实现 | BoxBuddyRS (GTK4/Rust)、Kontainer (Qt/C++) 证明形态可行；两者都只做了"容器管理器"，无人做"应用沙盒商店"——差异化空间 |
| 许可证 | GPL-3.0-only，分叉必须保持 GPL 分发 |

## 待决决策（详见 06）

- [ ] GUI 技术栈最终选择（gotk4 vs Qt/PySide6，Flutter 外卡）
- [ ] 容器身份标签：沿用 `manager=distrobox`（继承老容器）vs 新命名空间（孤儿化）
- [ ] rootful 支持策略（BoxBuddy 因 UX 拒绝；需引擎层一等公民化）
- [ ] Flatpak 分发是否首日支持（参照实现均为 full Flatpak 支持）
