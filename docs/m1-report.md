# M1 阶段报告：fuse-overlayfs 快速 commit 落地 + native overlay 验证（2026-09-11）

> fork 分支：podman `easytidy/engine` @ 2f31e3e662（4 文件 +64/-6）。
> docs/19 阶段二（魔改 spike）首个里程碑达成。

## 1. 补丁内容（三处，全在 vendor 内）

1. **buildah `image.go`**：容器顶层 blob 用真实父层调 `store.Diff(from=parent)`
   （原传 `""` → naive diff 走 `parent==""` 分支对整个合并根文件系统打 tar，
   产物层≈整根大小，落库时 storage-untar 全树 chown）。
2. **storage `overlay.go` `Driver.Diff`**：mountProgram（fuse）且有父层时走
   非 naive 路径（diff 目录直 tar，O(diff)）；whiteout 用 OverlayWhiteoutFormat
   解释磁盘字符设备（getWhiteoutFormat 对 mountProgram 返回 AUFS，会漏 whiteout
   导致删除丢失）。
3. **archive `archive_linux.go` + `archive.go`**：fuse 同时落盘 AUFS 风格 char
   marker（`.wh..opq`）与 overlay 风格 regular marker（`.wh..wh..opq`），char
   marker 经 ConvertWrite 转换后与后者撞名（apply 侧拒绝重复路径）。新增空名
   header 抑制机制跳过 char marker。

## 2. 实测：fuse-overlayfs（mount_program，隔离 ext4 store）

| 指标 | 原版 5.4.2 | fork |
|---|---|---|
| commit 期 fchownat | 549（全树） | ~12（仅变更文件） |
| commit 产物顶层 | 8.73MB（整根） | ~42kB |
| 3 轮 rebuild 镜像尺寸 | +8.7MB/轮 | 恒定 8.71MB |
| rebuild create+start | — | ~0.05s |
| 内容正确性（新增/嵌套/文件删/目录删/跨轮持久） | ✓ | ✓ |

## 3. 实测：native overlay（ext4，无 mount_program）—— 主目标场景

| 路径 | fchownat | 结论 |
|---|---|---|
| 标准镜像 keep-id create | 8 | 无 chown-by-maps（idmapped mount shift 生效） |
| **commit** | **1** | 增量 blob，顶层 ~kB，镜像尺寸 3 轮恒定 8.71MB |
| **commit 镜像 keep-id create** | **8** | **docs/18 的 ~2s/GB 全树校验没有出现** |
| 3 轮 rebuild 循环 | — | 内容跨轮持久（ext4 + r1/r2/r3 累积），删除保持 |

**关键推断**：补丁①让 commit 产物层变成小增量层且按 keep-id 映射如实记录
（apply 时 layers.json 记录目标映射）→ `imageTopLayerForMapping` 的
DeepEqual fast path 直接命中 → create 免造映射副本、免 chown-by-maps。
即 **快速 commit 顺带消灭了 native overlay 的 create 期全树校验**
（docs/18 道路一的 44s 场景）。

## 4. 排障记录（重要教训）

1. **`remote` build tag**：带它构建 = 远程客户端，实际工作在 distro podman
   5.4.2 的 socket service 进程里，本地补丁全部不生效。本地引擎构建必须去掉。
2. **strace 与 `-d` 容器**：`strace podman run -d` 会跟随 detached 的
   sleep 进程直到退出，wall time 全是假象；测量 create 用容器状态判断。
3. **tmpfs store 假象**：tmpfs 上 idmapped-mounts 特性检测失败 → create 期
   全树 chown（1001 次）。真实 store 在 ext4 上无此问题；但 easytidy doctor
   可考虑增加「graphroot 所在文件系统」检测。
4. **naive 视图对比死穴**：同一文件在容器视图/父层视图 uid 呈现不同（fuse
   daemon 命名空间视角，实测 1 vs 0），ChangesDirs 判全树变更且映射参数无法
   归一——native 视图对比在 fuse 下根本不可用，diff-dir 直 tar 是唯一正解。

## 6. 真实 18G 容器验证（2026-09-11，native overlay，主目标场景）

真实 store 切 native overlay（storage.conf 注释 mount_program，备份
storage.conf.fuse-backup），fork 二进制（2f31e3e662 + 去重补丁 29 行）对运行中的
desk_pilot02（19.5GB）验证：

| 环节 | 原版 5.4.2 | fork |
|---|---|---|
| commit（运行中容器） | ~50s + 全树 chown | **0.89s**，1549 次 fchownat（仅变更文件），顶层 **195kB**（原版整根 ~19.5GB） |
| create（commit 镜像） | ~44s 全树校验 | **0.03s，0 chown** |
| start（rootfs 挂载+准备） | — | **1.27s，6 chown** |
| 镜像挂载内容 | ✓ | ✓（完整根文件系统，uid 呈现正常） |

**合计 rebuild 核心链路 ~50s+44s ≈ 94s → ~2.2s（~40 倍）**。

附加修复：通用重复路径抑制（tarWriter.SuppressDupPaths，仅 mountProgram 场景启用）——
真实容器的 diff 中存在除 fuse 双 marker 外的其他撞名模式，逐个修不如通用去重。

遗留产物：镜像 localhost/easytidy-rebuild:desk_pilot02-verify1（即最新快照，可留作备份）。
Go 工具链注意：GOROOT 需 export ~/tools/go（系统有 gvm 残留 env）；构建 tag：
`exclude_graphdriver_btrfs containers_image_openpgp`（**无 remote**，本地引擎；
且 fork 构建无 libseccomp，create 需 CONTAINERS_CONF 指 seccomp_profile=unconfined
—— 正式分发需装 libseccomp-dev 重编）。

## 7. 遗留事项

- 18G 真实 desk_pilot 容器端到端 rebuild 验证（需在用户确认的时间窗执行）。
- storage/buildah 上游单测回归（fork 内 `go test ./...` 受影响包）。
- 探针/调试代码已清除，正式补丁 4 文件已提交（2f31e3e662）。
- Go 工具链：~/tools/go（GOROOT 需显式 export，系统有 gvm 残留 GOROOT env）。
