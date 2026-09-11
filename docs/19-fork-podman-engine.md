# 19. 道路四：魔改 podman（fork + 减法）→ easytidy 专属引擎

> 状态：计划稿（2026-09-11）。是 docs/18 道路三（自研引擎）的**务实前置形态**：
> 不从零写引擎，而是先 fork podman 源码解决最痛的 commit/启动 chown 问题，
> 再做减法收敛成 easytidy 专属引擎，最终替换 podman 二进制依赖。
> 动机与实证见 docs/18 §0.1 / §0.15（create 期全层扫描 + commit 期全树属主翻译，
> 18G 容器 rebuild 两段合计可达 ~50s+）。

## 0. 核心洞察：慢的根源是 uidmap 元数据丢失，不是 chown 本身

docs/18 已实证的三段链条：

| 慢路径 | 触发 | 根因 |
|---|---|---|
| create 期全树扫描/翻译（~2s/GB） | 首次从 commit 镜像建容器 | commit 层 uidmap 元数据丢失（container-libs PR #734），驱动必须逐文件对账 |
| commit 期全树 fchownat（~350µs/文件） | keep-id 容器首次 commit | 容器层以映射形态落盘，commit 必须反向翻译成镜像 unmapped 形态 |

两者是**同一枚硬币**：层磁盘属主形态 ↔ 镜像声明形态之间没有随层携带的映射元数据，
导致每个边界（commit 出、create 入）都要用户态逐文件对账。

**魔改的关键不是把 chown 写快，而是让映射元数据随层走、让同映射路径零翻译**：
easytidy 的 rebuild 永远用同一 uidmap（keep-id 同 uid/gid），容器层形态 =
新容器期望形态——**翻译根本不需要发生**。这是 easytidy 场景独有的自由度，
上游 podman 必须支持任意映射所以做不到这个特化。

## 1. 阶段一：源码定位（~1-3 天）

目标：给三条路径钉出「文件:函数:行号」级路线图，形成魔改点清单。

| # | 定位目标 | 已知锚点 | 产出 |
|---|---|---|---|
| 1 | create 期 chown/扫描 | `vendor/go.podman.io/storage/drivers/chown.go`（`chownByMapsCmd` = reexec `storage-chown-by-maps`）；overlay driver `overlay.go` 的 `getLayerPermissions`/uidmap 判定路径 | 精确触发条件分支：什么元数据状态导致扫描 |
| 2 | commit 期全树翻译 | buildah `image.go` commit 层生成 + storage `archive` 的 IDMappingConverter；podman #27390 提到的 `srcHasher` 路径 | 翻译调用栈 + 可跳过条件 |
| 3 | commit 时 uidmap 元数据写入 | `overlay-images/images.json` 的 `mapped-layers` + `overlay-layers/layers.json` 的 `uidmap/gidmap`（docs/18 §0.1 第 7 点）；PR #734 说 container 层存空 map | 写入点 + 「空 map」的确切来源 |

方法：podman 源码在 `/home/div/Documents/codes/easy-tidy/podman`（main @ b6fb44a48a），
storage/buildah 是独立 Go module（`go.podman.io/storage` v1.64.1 等，已在 vendor/）。
必要时 clone containers/storage、containers/buildah 上游对照。

**关键预判（阶段一要证实/证伪）**：如果 commit 时把新层的 uidmap 如实记录为
「容器映射形态」（而不是翻译成 unmapped + 记空），那么同映射 create 时驱动应直接
走 fast path。PR #735（fuse "performs id map over the entire FS"）说明 fuse 守护
进程有完整视图——理论上 commit 直通是可行的。若证伪（fuse 层与磁盘形态耦合无法
直通），退回方案 R2（见阶段二）。

## 2. 阶段二：魔改 spike —— `podman rebuild` 快速通道（~1-2 周）

### 2.1 fork 工程策略

- **不 fork 整个 podman 仓库做重发布**：podman 对 storage/buildah 的依赖是独立
  Go module。建 `easy-podman` 仓库：薄壳仓库 + `go.mod replace` 指向改过的
  storage/buildah fork。podman 本体改动（新命令）尽量小、集中。
- 构建：`make binaries`（本地 Go 工具链），产物单二进制 `easy-podman`。
- 兼容策略：保留完整 podman CLI/HTTP 兼容面（阶段二不动接口），只加内部快通道。
  阶段三再做减法。

### 2.2 方案 R1「同映射直通」（首选）

新增内部命令（reexec 形态，对 easytidy 暴露为 `easy-podman rebuild --from <容器> ...`）：

1. **commit 直通**：跳过 buildah 的反向翻译；容器可写层以现有磁盘形态（映射 uid）
   直接固化为新镜像层，`layers.json` 如实记录 `uidmap/gidmap` = 容器映射。
   - 产物镜像标内部标记（如 annotation `io.easytidy.native-mapping=1`），
     只承诺被同映射消费（easytidy rebuild 链内自洽；对外的 snapshot 仍走原
     commit 路径，保证导出兼容）。
2. **create 直通**：easytidy rebuild 建新容器时 uidmap 不变（keep-id 同 uid），
   驱动看到层 uidmap 与目标映射一致 → 跳过 `storage-chown-by-maps` 与全树扫描，
   直接挂载。
3. **验收**：18G 容器 rebuild 端到端（commit+create+start+dock 就绪）从 ~50s+
   降到 ≤5s（目标 <2s）；`strace` 验证 0 次全树 fchownat；镜像可被快照/导出
   路径正常消费（或明确文档化限制）。

### 2.3 方案 R2「原地续命」（R1 证伪时备选）

不 commit：rebuild = 把旧容器可写层**过户**给新容器（create 新容器复用同一
layer id，更新 mounts/env/network 配置），零 IO。containers/storage 层对象本就
独立于容器存在，需评估 layer 引用计数/垃圾回收的改动面。数据保真等价于 commit
（层就是数据），但放弃镜像产物（rebuild 不再自动产生 commit 镜像——快照改为
显式操作）。

### 2.4 spike 退出判据

- R1 或 R2 任一达成验收 → 进阶段三。
- 双双证伪（例如 fuse-overlayfs 的磁盘形态使其无法直通且 R2 改动面失控）
  → 回到 docs/18 道路二（忍受 commit 期翻译）并重新评估真·自研（道路三）。

## 3. 阶段三：功能面盘点 + 减法（~1-2 周）

1. **盘点输入**：`crates/core/src/libpod.rs` + `podman/mod.rs` 的全部调用面 +
   CLI/GUI shell-out（`podman exec` 等）。预期集合：libpod create/start/stop/rm/
   exec/commit/inspect/list/images/events、bollard Docker-compat（rebuild 用）的
   等价物、info、version。
2. **减法清单**（从 podman 砍）：Docker-compat API 双轨、kube/play/farm/machine/
   secrets/volumes/registry 登录/远程 client/多 OS 构建/Shell 补全/man 生成等。
   每砍一块都要确认 easytidy 调用面无引用。
3. **接口收敛**：easytidy 侧 `libpod.rs` 的 raw HTTP 保持不变（fork 保留 libpod
   端点形状），加 `engine` 抽象：`podman`（现网）/`easy-podman`（fork）可切换，
   现网随时可退回。
4. **维护策略**：pin 到 podman 5.4.x 基线，只 cherry-pick 安全修复；
   不追上游（与 DistroBox 分叉哲学一致，docs/00）。

## 4. 阶段四：easytidy 集成迁移（~1-2 周）

1. **分发**：`easy-podman` musl 静态二进制（与 easytidy-dock 同策略），安装位
   `/usr/bin` 或 appdata；doctor 增加引擎检测（检测到 easy-podman 则自动切换）。
2. **集成测试**：
   - 现有 `cargo test --workspace` 全绿（core 144+）；
   - 端到端：create/rebuild（快通道验证）/snapshot（兼容 commit 路径）/GPU 透传/
     host|mapped 网络/keep-id 身份/dock daemon 引导/root 终端/存储健康诊断；
   - 故障注入：rebuild 中断回滚（docs/18 重建顺序语义不变）。
3. **迁移**：存量容器镜像经 `podman save` → `easy-podman load`（兼容 OCI 格式）；
   或首次 rebuild 时自然切换。GUI/CLI 不感知（HTTP 形状不变）。
4. **验收**：easytidy 全功能在 easy-podman 上回归通过；rebuild 快通道达标；
   podman 二进制不再被 easytidy 直接依赖。

## 5. 里程碑与总工期

| 里程碑 | 内容 | 工期 | 出口判据 |
|---|---|---|---|
| M0 | 源码定位路线图 | 1-3 天 | 三条路径 文件:行号 清单 + R1 可行性预判 |
| M1 | 魔改 spike | 1-2 周 | 18G rebuild ≤5s，0 全树 fchownat |
| M2 | 减法 fork | 1-2 周 | easytidy 调用面全绿 + 砍伐清单落地 |
| M3 | 集成迁移 | 1-2 周 | 全功能回归 + 分发就绪 |

合计 ~4-6 周（与 docs/18 道路三估算相当，但 M1 就能拿到 90% 收益，
后续减法/迁移可按需推进，风险前置于 M0/M1）。

## 6. 风险

| 风险 | 评估 | 缓解 |
|---|---|---|
| R1 证伪（fuse 磁盘形态与元数据耦合，直通不可行） | 中 | M0 先行预判；R2 原地续命备选 |
| storage/buildah 内部 API 演进，fork 维护成本 | 低（已 pin） | 冻结基线，只收安全修复 |
| 快通道镜像被外部工具误用（形态非标准） | 低 | 内部标记 + rebuild 链内自洽；snapshot 走兼容路径 |
| Go 工具链/构建环境（easytidy 是 Rust 栈） | 低 | 单二进制交付，CI 出 musl 产物 |
| 与 podman 现网行为漂移引入新 bug | 中 | 引擎切换开关 + 现网 podman 路径保留，可随时回退 |

## 7. 与 docs/18 三道路的关系

- 本道路 = 道路三的**增量实现策略**：M1（魔改）先解决最痛的 rebuild 慢，
  M2/M3（减法+迁移）正是道路三「把 podman 白送的引擎自己写出来」的落地路径——
  只不过从「fork 后做减法」而非「从零重写」开始，风险和工作量都小一档。
- 道路二的 fuse-overlayfs 配置继续保留（doctor 功能不撤）：非 easy-podman
  场景（用户想继续用原版 podman）仍是有效缓解。
