# M0 Lane B：commit 期全树 fchownat 的调用栈与 R1 直通可行性（源码考古 + 定向实验）

> 实测背景：keep-id 容器首次 commit 全树 fchownat（~350µs/文件），二次 commit 0 次，
> 非 keep-id 容器 0 次（docs/18 §0.15）。基线 podman main @ b6fb44a48a。

## 1. 定向实验（本机复测，定位 chown 执行者）

`strace -f -e trace=execve,fchownat podman commit`（alpine keep-id 容器，30 文件 diff）：

- fchownat 549 次，**全部来自 reexec 子进程 `storage-untar`**：
  `execve("/proc/self/exe", ["storage-untar", "/", "/proc/self/fd/4"])`。
- 即 commit 期间发生了一次**整根文件系统的 tar → 解包**，解包时逐文件
  fchownat 到映射形态（host 1,1 = keep-id 下容器 0:0）。
- 不是 `storage-chown-by-maps`（lane A 的原语），而是 **untar 提取路径的 chown**。

## 2. 调用栈（代码锚点）

1. buildah `commit.go:338` `Builder.Commit` → `image.go:1622`
   `makeContainerImageRef` → `image.go:1099` `i.store.Diff("", layerID, diffOptions)`
   ——注意 **`from=""`**（buildah 只想要该层的 blob，不带父层语义）。
2. storage `store.go:3230` `store.Diff` → `layers.go:2370` `layerStore.Diff`：
   ```go
   if from != toLayer.Parent {   // "" != alpine顶层 → true
       diff, err := r.driver.Diff(to, r.layerMappings(toLayer), from, ..., ...)
   ```
3. overlay `drivers/overlay/overlay.go:2449` `Driver.Diff`：
   ```go
   if d.useNaiveDiff() || !d.isParent(id, parent) {   // isParent(id,"")=false
       return d.naiveDiff.Diff(id, idMappings, parent, parentMappings, mountLabel)
   ```
   fuse-overlayfs（mount_program）下 `useNaiveDiff()` 恒 true
   （overlay.go:824-851，`Native Overlay Diff: "false"`）。
4. `drivers/fsdiff.go:45` `NaiveDiffDriver.Diff`：`parent == ""` 分支（:66-77）→
   **`archive.TarWithOptions(layerFs, ...)` 对整个合并根文件系统打 tar**
   （layerFs = `driver.Get(id)` = 合并视图），UIDMaps/GIDMaps = 层映射
   （tar 头写容器视图属主，此阶段无 syscall）。
5. containers/image storage transport（buildah `commit.go:512` 的
   `retryCopyImage`）把 blob 放进 storage：新层 `ApplyDiff`（store.go:3359）→
   **reexec `storage-untar`** 解包，按层记录的 keep-id 映射逐文件 fchownat
   （`pkg/archive/archive.go` untar 的 chown 逻辑）→ 549 次全树 chown。

**结论链**：fuse 下 `podman commit` 的层 diff 不是"容器可写层增量"，而是
**naive diff 的整根 tar**（这也解释了为什么 rebuild 产物镜像总是 ~整根大小：
media 7.12GB、desk_pilot02 19.5GB）；落库时再整树解包 + chown。
双重成本：18G tar 的读写 + 每文件 fchownat。

## 3. R1「同映射直通」可行性

**可行，且改动面比预想小**。两个可选切入点：

- **切入点 ①（推荐，buildah 侧）**：buildah `makeContainerImageRef` 对容器顶层
  blob 不走 `store.Diff("", layerID)` 的 blob 往返，改为 storage 驱动级
  **层直通**：新镜像顶层 = 容器 RW 层（磁盘形态已是目标映射形态，无需 tar/chown），
  layers.json 记录 uidmap = 容器映射（lane A 已证 fast path 认这种记录）。
  easytidy 场景（同映射 rebuild 链内自洽）完全满足。
- **切入点 ②（storage 侧，更通用）**：给 `DiffOptions`/`layerStore.Diff` 增加
  "同映射直通"语义：`to` 层映射 == 目标映射时返回**层引用**而非 tar 流，
  storage transport 侧识别后直接登记层对象（相当于 R2 的受控版）。

两案共同前提：产物镜像标注内部标记（annotation），只承诺同映射消费；
对外导出（snapshot/push）仍走原路径（届时再做真实 tar，按需付成本）。

## 4. 需后续确认的细节（M1 开工前）

- `storage-untar` 的确切调用方（containers/image `storage_dest.go` PutBlob →
  ApplyDiff 链），vendor 下 `vendor/go.podman.io/image/v5/storage/storage_dest.go`
  未逐行核对——不影响切入点选择，但 M1 写补丁时要通读。
- `podman commit --squash`（easytidy 快照默认）走 `extractRootfs`
  （image.go:274，copier.Get 读合并视图 → tar），没有 untar 往返，
  成本=单次整根读+压缩。快照路径可暂不魔改。
