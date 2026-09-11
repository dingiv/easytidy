# M0 Lane C：层 uidmap 元数据的读写点与最小改动集（源码考古）

> 基线：vendor `go.podman.io/storage` v1.64.1（podman main @ b6fb44a48a）。

## 1. 数据结构

- `layers.go:170-175` `Layer`：
  ```go
  // UIDMap and GIDMap are the on-disk ID mappings for this layer ...
  // mapping is applied at mount time instead (see Container.UIDMap/GIDMap).
  UIDMap []idtools.IDMap `json:"uidmap,omitempty"`
  GIDMap []idtools.IDMap `json:"gidmap,omitempty"`
  ```
  持久化在 `overlay-layers/layers.json`。`omitempty` —— **空映射（shift 可用时）
  序列化后字段消失，即 PR #734 的"存空 map"**。
- `images.go:63-68` `Image.MappedTopLayers []string \`json:"mapped-layers"\``：
  同内容、不同 uidmap 的顶层副本 ID 列表（持久化在 `overlay-images/images.json`）。

## 2. 写入点

| 位置 | 写什么 |
|---|---|
| `layers.go:1606-1607`（`layerStore.create`） | 新层记录 `UIDMap/GIDMap = copySlicePreferringNil(moreOptions.IDMappingOptions.UIDMap/GIDMap)` —— 层映射的唯一落盘点 |
| `layers.go:1651-1654` | **关键语义**：`HostUIDMapping && HostGIDMapping` → `idMappings = 空映射`（shift 可用时不给层记映射，PR #734） |
| `store.go` `imageTopLayerForMapping` 末尾（store.go:1905 附近） | `s.imageStore.addMappedTopLayer(image.ID, mappedLayer.ID)`（实现在 `images.go:779`）—— mapped-layers 登记 |
| `images.go:788-791` | `removeMappedTopLayer`（镜像/层删除时） |

没有独立的 `SetLayerIDMappings` 公开 API——层映射只在 **create 时**一次性记录。

## 3. 读取点

| 位置 | 用途 |
|---|---|
| `store.go:1823-1836` `layerMatchesMappingOptions` | create 时与目标映射 DeepEqual（fast path 判定，见 A 报告 §3） |
| `layers.go:2290` `layerMappings(layer)` | Diff/mount 时把层映射传给驱动 |
| `layers.go:1892` `putPath`/tar-split 相关 | 空映射判断 |
| `store.go:1845/2307/2735/...` | `MappedTopLayers` 的遍历（候选顶层、删除、GC） |

## 4. "如实记录"需要改什么

R1 直通方案（B 报告切入点①）下，新镜像顶层复用容器 RW 层（磁盘已是映射形态），
需要：

1. **层记录**：`layers.go` `create()` 已支持——传入
   `IDMappingOptions{UIDMap: 容器映射, GIDMap: 容器映射, HostUIDMapping: false,
   HostGIDMapping: false}` 即可如实落盘（**无需改 storage**，绕开 :1651 的空映射
   归一即可，或加一个 `KeepIDMapping` 开关跳过该归一）。
2. **镜像登记**：`store.CreateImage(..., TopLayer: 容器RW层ID, ...)`——
   现有 API 已支持指定 TopLayer（images.go 结构公开）。若希望同镜像未来可被
   其他映射消费，再把基础栈顶层挂 `MappedTopLayers`。
3. **跳过 blob 往返**：改动在 buildah `makeContainerImageRef`（image.go:1099
   一带）+ containers/image `storage_dest.go` 的 PutBlob 路径——识别
   "直通层"标记（如 blob 上的 annotation/私有 reference 字段），跳过
   Diff/ApplyDiff，直接 `CreateImage` 指定层 ID。

## 5. 最小改动函数集合（fork 补丁面）

| 文件 | 函数 | 改动 |
|---|---|---|
| buildah `image.go` | `containerImageRef` blob 生成（:1060-1130 区域） | 直通层不走 Diff tar |
| containers/image `storage_dest.go` | blob/层落地路径 | 识别直通层 → 跳过 ApplyDiff（避开 storage-untar 全树 chown） |
| storage `layers.go` | `create()` 的映射归一（:1651-1654） | 可选：保留调用方给的显式映射（`omitempty` 问题：容器映射非空，无需改） |
| podman `libpod` commit 入口 | 传直通选项 | 新内部命令 `easy-podman rebuild` 的胶水 |

**结论**：storage 的元数据机制（层 uidmap 记录 + mapped-layers + fast path 判定）
**已经原生支持同映射直通**，不需要发明新元数据；fork 的核心改动在
buildah/containers-image 的 blob 往返跳过，属于"绕开"而非"重构"。
