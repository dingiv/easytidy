# 21 · 权限漂移根治设计（podman 层）

> 2026-09-14 · 快速重建链路的 id 空间翻译缺失问题——根因定位与实施设计。
> **状态修订（同日深挖后）**：累积漂移论不成立——overlay apply 对 tar id 是
> 恒等落盘，重建后状态机自洽稳定（chrome/desk_pilot02 各 2 轮实测零漂移）。
> 真正的问题是**旧世代层的编码差**（5.4.2 编码 h1000 ↔ 6.2 编码 h999 表示
> 同一容器用户），首轮快速重建瞬态错位、prepare 自愈兜住。
> 本文 §2 的 podman 层改造为**可选根治项**（消除首轮瞬态），待新会话实施；
> 实施落地并验证后，退役 prepare 的 ensure_home_ownership 自愈。
>
> **2026-09-15 终审：§2 路线 A 已实施并回退（否定性结论），详见 §6。**
> 核心发现：本系统的 fuse-overlayfs 对非连续映射实际未生效移位，磁盘编码
> 本来就是容器视图恒等，快速 commit 的 passthrough 已经正确；首轮瞬态仅
> 来自 5.4.2 旧世代层，prepare 自愈是唯一正确机制，**不退役**。

## 1 · 根因（实证）

**快速 commit 的 tar 头写的是宿主原始 id，而非容器视图 id。**

证据链：

1. `podman save` 出的 layer.tar：`home/ubuntu/.bashrc uid=1000 gid=1001`
   / `home/node uid=999 gid=999`——这些是**宿主空间**数值（div = 宿主
   uid 1000；999 = 宿主 1000 在容器视图的错位投影）；
2. overlay 层的 `overlay-layers/layers.json` 记录**只有 uidset/gidset，
   没有 UIDMap/GIDMap** → `layerStore.layerMappings()` 返回空映射 →
   `archive.TarOptions.UIDMaps/GIDMaps` 为空 → `tarWriter` 的
   `ToContainer()` 翻译（archive.go:657）被 `IDMappings.Empty()` 跳过；
3. 对比上游：naive diff 走 merged（idmapped 挂载，容器视图）打 tar，头是
   容器视图 id；create 侧把 tar id 当容器 id 编码进新层——**语义自洽**。
   我们的快速路径绕过挂载直接 tar 原始 diff 目录，但 create 侧语义没变
   → 宿主 id 被当作容器 id 再编码 → **每轮重建系统性漂移**
   （999→998→…，即"权限不规律错位"）。

## 2 · 实施设计

### 2.1 正确的 id 翻译来源

容器内 `/proc/self/gid_map`（自洽视图）+ 磁盘编码对照可推出：驱动给
容器建 idmerged 挂载时用的是**一套确定的映射**（uidset/gidset + 容器
映射的配对算法，代码在 overlay.go 挂载路径）。Diff 时驱动可以构建
**同一个翻译**：对 tar 头做 `storage-id → container-id`。

### 2.2 实现落点（fork，vendor/storage）

`overlay.go` 的 `Driver.Diff`（mount-program && parent != nil 分支）：

1. 复用驱动现有的 idmapped 挂载基建，把该层的 diff 目录以**容器视图**
   挂出来（与容器 merged 同源翻译）；
2. tar 该视图（内容不变，仅元数据口径变为容器 id）；
3. 其余管线（WhiteoutConverter / SuppressWhiteoutDuplicates）保持。

白名单注意：char 设备白出标记在原始 diff 目录，挂载视图里是"被隐藏的
文件"——白出逻辑继续从原始 diff 目录取**文件清单**，元数据取挂载视图。

### 2.3 create 侧

`easytidy_fast` 的 skip 保持：tar id 已是容器视图，apply 按新层映射编码
落盘——语义与上游一致，**映射信息随层记录（uidset/gidset）天然持久化，
无需 sidecar**。

### 2.4 easytidy 层

- prepare 的 `ensure_home_ownership` 自愈**保留至 2.2 落地并验证**，
  随后退役（它就是换地方的 chown）；
- `easytidy_fast` commit/create 参数语义不变。

## 3 · 验证方案

1. 探针：容器内 `touch` + 宿主 `stat` upper 层，标定编码函数；
2. 快速重建 × N：`stat` 显示恒为 `1000:1000`，`find ! -uid/-gid` 零残留；
3. `podman save` 后 layer.tar 头 uid/gid = 容器视图值；
4. 多容器（不同 user_name/镜像）回归。

## 4 · 迁移

- 已损坏镜像/容器：手动处理（用户已接受）；或每容器一轮慢速重建
  （squash 全量重写，自带映射归位）；
- prepare 自愈在 2.2 验证通过后移除。

## 5 · 实施规格（新会话执行清单）

### 5.1 已核实的代码锚点

| 锚点 | 位置 | 现状 |
|---|---|---|
| 快速 Diff 分支 | `vendor/.../drivers/overlay/overlay.go` `Driver.Diff`（`mountProgram && parent != ""` 走 raw diff tar） | `TarOptions.UIDMaps/GIDMaps = idMappings.UIDs()/GIDs()`，但 **overlay 层的 idMappings 恒为空** |
| 翻译机制 | `vendor/.../storage/pkg/archive/archive.go:657` | `!ta.IDMappings.Empty()` 时 `ToContainer()` 翻译 tar 头——**机制现成，缺的只是非空映射** |
| 层元数据 | `overlay-layers/layers.json` | 只有 `uidset/gidset`（容器视图 id 集合），**无完整映射对** |
| 层映射来源 | `layers.go` `layerMappings(layer)` = `layer.UIDMap/GIDMap` | overlay 驱动从不填充 → 空 |
| easytidy 侧映射 | create body 的 `uidmappings/gidmappings`（crun userns，如 c1000 ↔ h999） | commit 时**不可达**（容器已停，映射随进程消失） |

### 5.2 核心难点

tar 需要"容器视图 id"，但容器视图 = crun userns 映射（随容器进程存在），
commit 时容器已停。两条候选路线：

- **路线 A（推荐）：驱动侧持久化编码映射**。`Driver.get/create` 建 idmapped
  挂载时，把"storage id ↔ 容器 id"配对表写入层目录（如 `diff.map` JSON）。
  commit 时读取配对表 → 构造 `idtools.IDMappings` → 现有 ToContainer 机制
  自动翻译。配对表在层创建/首次挂载时生成，天然持久化。
- **路线 B：挂载视图打 tar**。Diff 时临时以同参数 idmapped 挂载 diff 目录，
  tar 挂载视图。依赖驱动挂载基建复用（overlay.go `get()` 的 needsIDMapping
  分支），且白出标记在原始目录需双源组装——工程量更大。

### 5.3 实施步骤（路线 A）

1. 定位驱动建 idmapped 挂载的配对算法（overlay.go `get()` needsIDMapping
   分支 + `idmap.CreateIDMappedMount` 调用点），确认 storage↔容器 的配对
   规则（实测编码 c1000 ↔ h100998、c999 ↔ h999 与 uidset 的关系）；
2. 层目录新增配对表持久化（`overlay-layers/<id>/idmap.json` 或复用
   layers.json 扩展字段）；旧层无配对表 → 视为"无翻译需求"（行为不变）；
3. `Driver.Diff` 快速分支：读配对表 → 构造 `idtools.IDMappings` → 传入
   `TarOptions`；无配对表时行为与现状完全一致（零回归风险）；
4. 验证（docs/21 §3 全套）+ prepare 自愈退役 + docs 更新。

### 5.4 验收标准

- 新镜像首轮快速重建后 `find ! -uid/-gid` 零残留（无瞬态）；
- 连续 5 轮快速重建 id 恒定（不再依赖 prepare）；
- 退役 ensure_home_ownership 后同样通过；
- 旧世代镜像不做迁移也只表现首轮瞬态（行为不劣化）。

## 6 · 实施结果（2026-09-15）：路线 A 否定，回退

### 6.1 实施过程

按 §5.3 完整实现了路线 A：`Driver.get()` mount-program 分支挂载成功后把
`options.UidMaps/GidMaps` 持久化到层目录 `idmap`（unshifted 挂载删除）；
`Driver.Diff` 快速分支读配对表构造 `idtools.IDMappings` 交给现成
ToContainer 机制；`UpdateLayerIDMap` 失效化配对表。构建通过，
keep-id rootless 探针 + 生产 store 探针双路验证。

### 6.2 实证：映射是虚构，磁盘编码恒等

| 探针 | 观测 |
|---|---|
| probe2：容器 c1000 新写文件（干净单世代层） | 磁盘 storage 视图 = 1000（恒等） |
| chrome home/ubuntu（c1000） | h1000（恒等） |
| gui_container /tmp（c0 root，1777） | h0（恒等） |
| docs 记录 node（c999）6.2 编码 | h999（恒等） |
| 而记录的 mount 映射声称 | c1000↔h0、c0↔h1..1000 |

即：**fuse-overlayfs 对非连续映射（keep-id 型 swap，如
`[0↔1×1000, 1000↔0]`）实际未生效移位，磁盘编码 = 容器视图恒等**。
`options.UidMaps` 描述的是一种"假如移位生效"的虚构编码。

后果：路线 A 的翻译把正确的 h1000（=c1000）错译成 c999、把 root 的 h0
错译成 c1000——比现状更有害。且 §5.2 的前提"commit 时映射不可达"虽属实，
但补上映射也无济于事：映射本身不描述磁盘。

### 6.3 修正后的完整模型

- 6.2 磁盘编码：恒等（container id == storage id），快速 commit 的
  passthrough + apply 恒等落盘 = **语义自洽，现状已正确**；
- 首轮瞬态唯一来源：5.4.2 旧世代层按旧（移位式）方案编码（如 node 用户编
  码 h1000），与恒等 h1000（c1000）**逐文件不可区分**，commit 时无法翻译；
- prepare 的 ensure_home_ownership（容器内内核态 chown，把编码重写为恒等）
  是旧数据的一次性、幂等、正确的迁移机制，**永久保留**；
- 镜像层的旧世代编码可由一轮慢速重建（squash）归位（用户已接受现状）。

### 6.4 处置

- podman fork `vendor/.../overlay.go` 全部回退至 88304bffb8（已安装的
  /usr/local/bin/podman 即该代码构建，无需重装）；
- §5.4 验收标准作废；prepare 不退役；easytidy 侧无需改动。

### 6.5 重要更正：以上实证均发生在 fuse-overlayfs 上

后续发现（同日）：尽管 storage.conf 已注释 mount_program（"switched to
native"），实际由于 `storage/overlay/.has-mount-program` 标记文件残留
"true" + rootless 下 `/usr/bin/fuse-overlayfs` 自动回选，**系统一直在用
fuse-overlayfs**（运行中容器 mountinfo 实锤 `fuse.fuse-overlayfs`）。
§6.2/§6.3 的恒等实证对 fuse 场景成立（非连续映射被 disableShifting）。
随后已真正切到 native，见 §7。

### 6.6 遗留核对清单（旧议题，与权限漂移无关）

GUI 全链路复验、labwc 构建链固化、PipeWire/IM/portal env、
`/usr/bin/easytidy` 旧二进制处置、target/debug 覆盖来源。

## 7 · native 迁移：已实施并紧急回退（2026-09-15/16）

> **状态：已回退到 fuse-overlayfs。** 本节保留完整教训。

### 7.1 迁移实施（09-15）

- flag 文件置 false → 新容器实锤 native 挂载（mountinfo `- overlay`，无移位）；
- fork 快速 Diff 扩展到 native + 层目录 `idmap` 配对表持久化
  （7050a8148a，打包 +easytidy4）。

### 7.2 事故与回退（09-16 00:2x–01:3x）

用户按新二进制重启容器后「全部容器权限错位」（home 拒绝访问、/etc/passwd
显示 999:1001）。实证根因：

- **fuse 挂载自带 id 口径翻译**：磁盘层的编码差（旧世代 999 系 vs 现行
  1000 系）在容器视图中被掩盖，一切正常；
- **native（本机 rootless，无 idmapped overlay）不移位**，直接按容器
  userns 呈现 inode 真实 id → 同一份磁盘编码在容器视图全面错位
  （/home/ubuntu 磁盘 real 100999 = 容器 c999，而用户是 c1000）；
- 即 fuse 在此系统上一直承担着「视图语义适配器」角色，数据本身无损
  （无 chown 证据；desk 层曾观察到 187k 文件"近期 ctime"——复查为当晚
  数小时容器内正常活动（openclaw 构建/chrome profile/nvidia-ctk+ldconfig
  触发）的累计，并非一次性批量重写，初判有误）。

回退步骤（已执行）：`.has-mount-program` 置 true + storage.conf 显式
mount_program → 杀 socket 服务（驱动选择在服务生命周期内缓存）→ 重新拉起
→ 重启容器 → gui/chrome/desk 三容器 fuse 挂载 + 权限视图全部恢复；
desk_pilot02 的 /home/node 曾被改为 root 属主（00:34，来源未最终定论，
非存储切换所致），已在容器内 `chown -R node:node` 修复并验证可写。

### 7.3 若未来要真切 native（前置条件）

1. 每容器先一轮全量重编码归位（慢速重建 squash，或宿主侧按容器映射 chown
   整个 upperdir），使磁盘 id 与容器视图恒等；
2. 确认 easytidy prepare 在 native 下正常生效（本次观察到未兜底）；
3. 再切 flag；fork 的 idmap 配对表机制在真移位环境下自动启用。

### 7.4 遗留核对清单（旧议题，与权限漂移无关）

GUI 全链路复验、labwc 构建链固化、PipeWire/IM/portal env、
`/usr/bin/easytidy` 旧二进制处置、target/debug 覆盖来源。
