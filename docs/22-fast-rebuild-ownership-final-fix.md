# 22 · 快速重建属主漂移——最终根治（apply 字面提取）

> 2026-09-27 · 接续 docs/21。本文档是**最终结论与修复方案**。
> 状态：已修复、隔离 store 实证 4 轮零漂移、真实环境（demo 容器）手工验证通过。
> 前置阅读：docs/21（此前的探索与否定性结论仍有效）。

## 0 · 一句话结论

快速重建链路中，**commit 产出的 blob 与 apply 提取对它的解释使用了不同的 ID 口径**：
commit 直通产出「存储编码（swap toHost 形）」的 raw blob，而 apply 提取却按上游约定
把 blob 当作「容器视图（未映射）id」再做一次 `toHost(记录映射)` —— 对已是存储编码的
数据再翻译一次 = 每轮快速重建属主整体 +1 错位（ubuntu→999→root→…）。

修复：**apply 提取强制字面落盘（空映射）**，与 commit 直通组成闭环——
「存储编码进 → 存储编码出」，同映射循环逐轮恒等，跨映射代次由层记录 + 语义比对
回退完整翻译。

## 1 · 根因全链（探针逐阶段取证）

隔离 store（`/tmp/et-drift*`）+ 插桩二进制（`et-probe:` 日志、chown 子进程 trace）
逐阶段测量。以 keep-id swap 映射（`c1000↔U0, c999↔U1000, c0↔U1`）为例：

| 阶段 | /home | ubuntu 文件 | 说明 |
|---|---|---|---|
| gen1 upperdir（正常 create 后） | U1 | U0 | swap 编码 ✓（视图 0/1000 ✓）|
| 快速 commit → blob | U1 | U0 | **直通无偏移** ✓（`4fb1328156`）|
| **apply 提取 → 镜像层** | **U2** | **U1** | **+1 平移** ✗（toHost(swap)）|
| gen2（快速 create，信任磁盘） | 视图 1 | 视图 0 | **root 化** ✗ |
| gen2 commit → blob | U2 | U1 | 再 +1，误差逐轮累积 |

### 三层语义断层（历史叠加，缺一不复现）

1. **apply 提取按「父层继承的记录映射」做 toHost**（本次修复点）：
   `stageWithUnlockedStore` / `applyDiffWithOptions` 的 `Mappings` 取自新层记录，
   而新层记录从父层继承（`populateLayerOptions`：父记录非空且调用方未显式指定时继承）。
   第一轮 commit 的父层（pristine 基层）记录为空 → 提取字面 ✓ 不暴露；
   第二轮起父层记录 = 上一轮写入的 swap → toHost(swap) → **+1** ✗。
2. **crun userns 映射对存储层不可见**：keep-id 的 swap（`c1000↔U0`）是 crun 运行时
   映射，c/storage 的层记录从未包含它 → Diff/apply 无法据此做正确翻译，
   只能依赖「同映射直通」这一约定。
3. **pull 属主压平**：rootless pull 将 blob 属主统一压平为提取者，
   纯净镜像的用户文件（blob ubuntu=1000）落地编码与映像声明脱钩
   （实测基镜像层 `/home/ubuntu` 存为 U1000 = swap 视图 999）。

### 为什么历代修复都失败（对 docs/21 的最终回答）

- **路线 A（记录映射+翻译）**：方向正确，但当年只做到了「记录」，没有修复
  「apply 按记录再翻译直通 blob」这一步——记录反而成了 +1 的放大器。
- **prepare 自愈（chown）**：能掩盖症状，但被正确地否决了（重复存储层的职责）。
- **squash/慢速重建**：逐轮恒等，不解决跨代误差。
- 本次的突破：**插桩取证把「+1 发生在 apply 提取」从猜测变为实测**
  （upper U1/U0/U0 → blob U1/U0/U0 → 存储 U2/U1/U1，三次测量闭环）。

## 2 · 修复内容（podman fork，branch easytidy/engine）

### 2.1 `561d2073` — apply 提取强制字面落盘（本修复的核心）

两条提取路径的 `Mappings` 强制为空：

- `layerStore.stageWithUnlockedStore`（staged 提取，commit 走此路径）
- `layerStore.applyDiffWithOptions`（非 staged 提取，pull 走此路径）

语义：**blob 已是存储编码，字面落盘**。pull 路径行为不变（本就字面提取）。

### 2.2 `4fb1328156` — 快速 Diff raw 直通（保持）

fast Diff 直通 upperdir raw id（存储编码），与 2.1 的字面提取配对：
「存储编码进 → 存储编码出」。

### 2.3 `9aab52df27` — 层记录 + 语义比对（保持，用于跨映射）

- fast commit 后把提交容器的映射写入镜像顶层 layer 记录（`SetLayerIDMappings`）。
- `imageTopLayerForMapping`：记录映射与请求映射**语义比对**（`easyTidySameIDMappings`，
  忽略恒等 padding）：
  - 相同 → 信任磁盘（快速路径，零成本）；
  - 不同 → 回退完整翻译（mapped copy，跨映射代次正确归位，一次性成本）。
- 记录缺失（历史层）保持旧的信任行为（兼容）。

### 2.4 闭环证明（同映射循环净变换 = 恒等）

```
gen upper(raw, swap编码) ──commit 直通──> blob(raw) ──apply 字面──> 下代 raw = raw
视图 = swap(raw) 每代相同 ✓
```

跨映射：blob(raw, M1 编码) + 记录 M1 ≠ 请求 M2 → mapped copy 按记录翻译 ✓。

## 3 · easytidy 侧：首次创建悬空属主归位（保留）

pull 属主压平（根因 3）在「纯净镜像首次创建」时仍会产出错误属主文件
（root 属主骨架文件、跨代 999 悬空文件）。存储层无法翻译（docs/21 §6.3 的
恒等结论），由 easytidy prepare 在**首次创建**时做一次受限归位：

- 范围：仅容器用户自己的 home 子树（非 /home 全局）；
- 判定：owner **不在容器 /etc/passwd 任何账号**（悬空，如 999）
  **或 owner == root(0)**（骨架压平），两者均无合法来源；
- 跳过符号链接与挂载点（`/proc/self/mountinfo` 解析，宿主机 bind 内容不可触碰）；
- 仅 `--first-run`（首次创建）执行，重建/重启不重复。

提交：`crates/core/src/env/incontainer.rs`、`crates/dock/src/main.rs`、
`crates/core/src/podman/user.rs` 及 GUI/CLI 调用点（`first_run` 参数）。

## 4 · 验证

### 4.1 隔离 store（`/tmp/podman-probe`，含全部修复）

纯净 ubuntu:25.10 → 正常 create → 连续 **3 轮快速 commit + 快速 create**：

| 代次 | /home | /home/ubuntu | probe | 视图 |
|---|---|---|---|---|
| gen1 | 0:0 | 1000:1000 | 1000:1000 | ✓ |
| gen2 | 0:0 | 1000:1000 | 1000:1000 | ✓ 与 gen1 完全一致 |
| gen3 | 0:0 | 1000:1000 | 1000:1000 | ✓ 与 gen1 完全一致 |
| gen4 | 0:0 | 1000:1000 | 1000:1000 | ✓ 与 gen1 完全一致 |

（本轮属主基线 999 为纯净 pull 压平的历史产物，见 §3；关键指标是**逐轮零漂移**。）

### 4.2 真实环境

demo 容器：重启 podman 服务后全新创建 → 首次归位生效 → 快速重建 → 属主恒定。
（用户手工验证通过，2026-09-27。）

## 5 · 部署注意（重要陷阱）

**podman 服务进程常驻，升级 deb 不会更新运行中的进程。**

```
PID 5104 自 9-26 启动的 system service —— 升级后仍在跑旧二进制，
导致「装了新包但行为纹丝不动」的假象（本次排查耗时最长的坑）。
```

升级后必须：`kill <podman service pid>`（或重启 GUI）→ 服务重新拉起新二进制。

## 6 · 遗留

- 历史污染 store（含旧编码化石的镜像/快照链）不做自动迁移；
  新建容器 + 首次归位即可获得干净基线。
- 跨映射代次的完整翻译路径（record ≠ requested → mapped copy）已实现但
  未在真实跨映射场景实测；如出现「切换容器用户 uid」类场景需补测。
- 探针日志（`et-probe:` Info 级）保留在代码中，生产环境日志量可控
  （仅 create/commit 时各一条），后续可降为 debug。
