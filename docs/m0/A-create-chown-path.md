# M0 Lane A：create 期 chown/扫描的触发条件与 fast path（源码考古）

> 基线：podman main @ b6fb44a48a，vendor `go.podman.io/storage` v1.64.1。
> 实测背景见 docs/18 §0.1（native overlay ~2s/GB）/ §0.15（fuse 下 create 恒快）。

## 1. 全树 chown 的原语：reexec `storage-chown-by-maps`

- `drivers/chown.go:16` `chownByMapsCmd = "storage-chown-by-maps"`；`:20` 注册
  `chownByMapsMain`；`:23-77` 实现：chroot 进层目录 → `pwalkdir.Walk(".")` →
  `chowner.LChown(path, info, toHost, toContainer)`（`drivers/chown_unix.go:27`，
  逐文件 `fchownat(AT_SYMLINK_NOFOLLOW)`，并处理 overlay index=off 下 inode 断链的
  硬链接重建 `:57-73`）。
- 入口包装 `ChownPathByMaps`（chown.go:81）。
- 驱动侧封装：`drivers/chown.go:110` `naiveLayerIDMapUpdater.UpdateLayerIDMap` →
  `driver.Get(id)` 后 `ChownPathByMaps(layerFs, ...)`。
  overlay 驱动有自己的实现：`drivers/overlay/overlay.go:2508`
  `Driver.UpdateLayerIDMap`（idmapped mount 支持时走内核翻译，否则朴素 chown）。

## 2. create 期的触发点：`layerStore.create` 的映射变更检查

`layers.go:1680-1695`（`func (r *layerStore) create(...)`）：

```go
if oldMappings != nil &&
    (!reflect.DeepEqual(oldMappings.UIDs(), idMappings.UIDs()) ||
     !reflect.DeepEqual(oldMappings.GIDs(), idMappings.GIDs())) {
    if err = r.driver.UpdateLayerIDMap(id, oldMappings, idMappings, mountLabel); err != nil {
```

- `oldMappings` 来源：模板层（`templateIDMappings`，:1568）或父层（`parentMappings`，:1663）。
- `idMappings`：`moreOptions.IDMappingOptions`；**`HostUIDMapping && HostGIDMapping`
  时 = 空映射**（:1651-1654）——这就是 PR #734 所说"存空 map"的语义源头
  （shift 可用时不给层记映射）。
- 触发条件：父/模板层映射 ≠ 新层映射 → 对**新层**（模板场景 = 模板的全量拷贝）
  全树 chown。native overlay 从镜像建容器时由 `imageTopLayerForMapping`
  走 `CreateFromTemplate`（见 §3），这就是 docs/18 的 ~2s/GB。

## 3. fast path 的精确判定：`imageTopLayerForMapping`

`store.go:1822` `func (s *store) imageTopLayerForMapping(image, ristore, rlstore, lstores, options)`：

判定函数 `layerMatchesMappingOptions`（:1823-1836）：

```go
// 驱动支持 shifting 且层无映射 → 直接用
if s.canUseShifting(options.UIDMap, options.GIDMap) && len(layer.UIDMap) == 0 && len(layer.GIDMap) == 0 { return true }
// 要 host 映射而层有映射 → 不匹配
if options.HostUIDMapping && len(layer.UIDMap) != 0 { return false }
...
// 层的 UIDMap/GIDMap 与目标映射 DeepEqual → 完美匹配
return reflect.DeepEqual(layer.UIDMap, options.UIDMap) && reflect.DeepEqual(layer.GIDMap, options.GIDMap)
```

- 候选集 = `image.TopLayer + image.MappedTopLayers`（`images.go:68`
  `MappedTopLayers []string \`json:"mapped-layers"\``——同内容、不同 uidmap 的
  顶层副本；本 store 有写权限时（`createMappedLayer`，:1845）不匹配就**现场造一个
  映射副本**（:1886-1907，`TemplateLayer = layer.ID` + 目标映射 → 走 §2 的
  UpdateLayerIDMap 全树 chown）并 `addMappedTopLayer` 登记（images.go:779）。
- **同映射第二次 create 秒开的机制**：上次造的副本在 `MappedTopLayers` 里，
  `layerMatchesMappingOptions` 直接命中返回，零 IO。

## 4. fuse-overlayfs 下 create 恒快的代码证据

`drivers/overlay/overlay.go:2599` `SupportsShifting`：

```go
if d.options.mountProgram != "" {
    // fuse-overlayfs supports only contiguous mappings ...
    if !idtools.IsContiguous(uidmap) { return false }
    if !idtools.IsContiguous(gidmap) { return false }
    return true
}
return d.supportsIDmappedMounts()
```

keep-id 映射连续 → shifting=true → 标准镜像（空映射层）走 §3 第一条分支直接命中，
映射副本根本不创建 → 0 chown。与实测（fuse 下 hello-world 0.10s、commit 镜像
create 0.02-0.09s）完全一致。

## 5. 关键问题结论

**Q：commit 产物层以"容器映射形态"落盘且 layers.json 如实记录 uidmap = 容器映射，
同映射 create 会走 fast path 吗？**

**会，且这就是 storage 内建的机制**：`imageTopLayerForMapping` 的 DeepEqual 分支
不关心映射"应该"是什么，只比较层记录与目标映射。事实上 docs/18 已观测到
「commit 时自动记录自己的映射（layers.json de21fb2 条目）+ 同 commit 镜像后续
create 秒开」——即**该 fast path 在现网代码里就是通的**。fuse 下真正的问题不在
create，而在 commit 期（见 B 报告：commit 造整根 diff 层 + untar 逐文件 chown）。
