# 21 · 权限漂移根治设计（podman 层）

> 2026-09-14 · 快速重建链路的 id 空间翻译缺失问题——根因定位与实施设计。
> 本文取代"prepare 自愈"作为长期方案；prepare 自愈保留为过渡期兜底。

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
