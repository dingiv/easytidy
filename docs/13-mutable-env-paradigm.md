# 13. 可变容器使用范式（环境语义）

> 2026-08-07 确立。产品核心语义：**"环境"（environment）是一等概念**，
> 面向普通用户提供简单快捷的语义化操作——与开发者用 Dockerfile 保证
> 可复现性的心智不同，普通用户不需要可复现性，需要的是清晰的操作语义。

## 1. 五种环境语义

| # | 语义 | 用户心智 | 机制 |
|---|---|---|---|
| 1 | **新环境** | 创建一个干净的环境，我要开始操作了 | 创建容器（keep-id / 用户映射 / GUI 透传） |
| 2 | **删除环境** | 我不想要了，删干净，不留垃圾 | 容器 + 注册配置 + 桌面图标 + socket 目录全清理 |
| 3 | **快照** | 我要做破坏性动作了，先留个保险 | commit 容器层 → 快照镜像（命名 + 标签） |
| 4 | **fork** | 环境弄坏了，或想快速起一个已配置好的环境 | 从快照镜像创建新容器（继承配置） |
| 5 | **运行/关闭** | 细粒度控制，与创建/销毁分开 | start / stop（环境可停止而保留） |

## 2. 命令面（CLI）

```
easytidy env new       --name <n> [--flavor <f>] [--image <img>]
easytidy env rm        <n>                     # 删干净（容器+配置+图标+socket）
easytidy env snapshot  <n> [--tag <t>]         # 快照（默认标签=时间戳）
easytidy env fork      <n> --snapshot <t> --name <new>   # 从快照派生新环境
easytidy env start     <n>
easytidy env stop      <n>
easytidy env list                             # 环境列表（含快照）
```

GUI 等价操作：环境卡片上的 新建 / 删除（确认框）/ 快照 / fork / 启动 / 停止。

## 3. 语义细则

### 快照（snapshot）
- 范围：**仅容器文件系统层**（`podman commit`）——bind mount（宿主 home、
  显示透传、字体）不入快照，符合"环境配置"与"环境状态"分离
- 快照 = 命名镜像（`easytidy/snapshot/<env>-<tag>`），**独立资产**
- 删除环境**不删除**其快照（快照可被 fork 复用；删除环境时提示快照去向）

### fork
- 从快照镜像创建新容器：**继承环境的全部配置**（mounts / 网络 / entry /
  用户映射 / GUI 透传），仅镜像换成快照
- 语义：环境坏了回滚（fork 出坏之前的快照）、快速复用已配置好的环境

### 删除（rm）的"不留垃圾"清单
1. 容器（含 force 停止运行中容器）
2. 注册配置（configfile）
3. 宿主桌面图标（.desktop）
4. socket 目录（$XDG_RUNTIME_DIR/easytidy/<n>）
5. 快照镜像：**保留**（资产），rm 输出提示可用快照
6. 环境镜像（基础镜像）：共享资产，**不删**

### 运行/关闭
- start/stop 与 create/rm 完全分离：环境可长期停止、随时恢复
- stop 走优雅关闭链（podman stop → SIGTERM → server 收尾 → catatonit 退出）

## 4. 与 Docker 心智的对比

| Docker | easytidy 环境 | 差异 |
|---|---|---|
| Dockerfile build（可复现） | env new（开箱即用） | 普通用户不写构建文件 |
| run/exec | start/stop + run | 环境生命周期与运行分离 |
| commit/tag | snapshot | 语义化命名（快照=保险） |
| （无直接对应） | fork | 快照派生新环境，回滚/复用一体 |
| docker rm 残留镜像 | env rm 干净 | 垃圾零残留（快照除外，明确提示） |

## 5. 快照与既有机制的关系

- 快照机制复用 `rebuild` 已验证的 commit 路径（commit → 镜像）
- fork = `create_with_config`（image 换成快照镜像）+ 配置继承
- Q6（快照版本管理）后置：首版单点快照 + 标签，多版本列表化后续
