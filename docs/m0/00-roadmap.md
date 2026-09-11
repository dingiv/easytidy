# M0 路线图：三条慢路径代码级归因 + R1 可行性结论（2026-09-11）

> 分报告：A-create-chown-path.md / B-commit-translation.md / C-uidmap-metadata.md
> 结论：**R1「同映射直通」可行，storage 元数据机制原生支持，fork 改动 = 绕开 blob 往返**。

## 1. 机制总图（修正 docs/18 的两处理解）

```
                    ┌─ create ─────────────────────────────────────────────┐
镜像(layers.json    │ imageTopLayerForMapping (store.go:1822)              │
 uidmap 记录) ──────┤ fast: 层映射==目标映射 或 (shifting&&空映射) → 秒开   │
                    │ slow: CreateFromTemplate + UpdateLayerIDMap 全树chown │
                    └──────────────────────────────────────────────────────┘
                    ┌─ commit（fuse 下 naive diff）────────────────────────┐
容器(RW层,映射形态) ┤ buildah Diff("", layerID) → parent=="" 分支 →        │
                    │ 整根 tar → PutBlob → storage-untar 整树解包+chown    │
                    │ （层 blob = 整根文件系统，故 rebuild 镜像≈整根大小） │
                    └──────────────────────────────────────────────────────┘
```

- **修正 ①**：fuse 下 commit 期的全树 chown 不是"属主反向翻译"，
  而是 **naive diff 的整根 tar 往返**（`from=""` → `NaiveDiffDriver.Diff` 的
  `parent==""` 分支）+ 解包侧逐文件 chown。执行者是 reexec `storage-untar`
  （strace 实证），不是 `storage-chown-by-maps`。
- **修正 ②**：docs/18 §0.15 说的「每次重建都慢」在 fuse 下精确为
  **commit 期整根 tar（~18G IO）+ 整树 chown（~350µs/文件）**；
  create 期在 fuse 下恒快（`SupportsShifting` 对连续映射恒 true，
  overlay.go:2599）。

## 2. 关键代码锚点

| # | 锚点 | 意义 |
|---|---|---|
| 1 | `store.go:1822` `imageTopLayerForMapping` + `layerMatchesMappingOptions` | fast path 判定：层 uidmap 与目标 DeepEqual |
| 2 | `images.go:68` `MappedTopLayers` | 同内容不同映射的顶层副本登记（复用机制已内建） |
| 3 | `layers.go:1651-1654` + `:175 omitempty` | PR #734 空 map 语义源头（shift 可用时不记映射） |
| 4 | `layers.go:1680-1695` | create 期映射变更 → `UpdateLayerIDMap` 全树 chown |
| 5 | `overlay.go:2449` `Driver.Diff` + `fsdiff.go:45` `parent==""` 分支 | fuse 下 commit 生成整根 tar |
| 6 | `image.go:1099` `store.Diff("", layerID)` | buildah 只取 blob、丢父层语义的入口 |
| 7 | reexec `storage-untar`（strace 实证） | 解包侧逐文件 chown 的执行者 |

## 3. R1 结论与 M1 魔改点（最小补丁面）

storage 层**零发明**：层记录 uidmap、镜像指定 TopLayer、mapped-layers 登记、
fast path 判定全部现成。魔改集中在**绕开 blob 往返**：

1. buildah `image.go` blob 生成处识别直通层 → 不走 `store.Diff` tar；
2. containers/image `storage_dest.go` 落地路径识别直通层 → 跳过 ApplyDiff
   （避开 storage-untar）；
3. podman/libpod 新内部命令 `easy-podman rebuild` 胶水：直通参数 + 显式
   uidmap 记录 + annotation 标记。

预期收益：rebuild 的 commit 从「整根 tar + 整树 chown」变为
「驱动级层引用 + 元数据登记」≈ 秒级。18G 容器 ≤5s 验收目标可达。

## 4. 遗留确认项（M1 开工前顺手核对）

- `storage_dest.go` PutBlob→ApplyDiff 精确调用链（写补丁时通读）；
- `--squash` 快照走 `extractRootfs`（copier.Get 单次整根读），
  暂不魔改，快照维持现状；
- 同映射直通层被**外部工具**（原版 podman）消费的行为：层磁盘形态 =
  映射形态 + uidmap 如实记录，原版 podman 按 uidmap 正常解读，理论兼容，
  M1 用原版 podman 交叉验证。

## 5. M0 出口判定：**通过，进 M1**
