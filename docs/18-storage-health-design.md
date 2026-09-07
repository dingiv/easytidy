# 容器存储性能问题：三道路决策与方案（设计）

> 状态：三道路决策稿（2026-09-07）。§0.2 为道路总览；§0.3-§8 是道路二的设计稿；§9 是道路三的初步拆解。
> 背景：rootless podman + native overlay + keep-id 场景，**首次从镜像建容器**触发
> `storage-chown-by-maps` 逐文件递归 chown，~2s/GB（20GB 镜像 ≈ 44s）。
> 当前状态：已在**本机**手工配置 `~/.config/containers/storage.conf`（fuse-overlayfs），
> 重建实测 44s → 0.09s。但这是机器级手工操作，且 podman 依赖本身仍在。

## 0.1 机制详解（2026-09-07 本机实证）

完整因果链（每条均有本机实测/源码引用）：

1. **rootless 硬约束**：rootless 用户（uid 1000，subuid 100000+）只能创建/改属主
   自己范围内的文件，**无法在宿主磁盘上产生 uid 0 的文件**。实测：pull 后
   镜像层文件在磁盘上属主是 `1000:1000`（当前用户），而非镜像声明的 `0:0`。
2. **容器看到的属主 = 磁盘 uid 经容器 uid_map 翻译**。实测 keep-id 容器的
   uid_map：`容器0-999 ↔ 宿主100000-100999，容器1000 ↔ 宿主1000，容器1001+ ↔ 宿主101000+`。
   要让容器语义正确（镜像 root 文件→容器 0，用户文件→容器 1000），磁盘属主必须
   相对目标映射处于特定状态。
3. **native overlay 不做挂载期属主翻译**（rootless 多段 uidmap 下无 idmapped mount；
   container-libs PR #717："rootless solved it within fuse-overlayfs ... native
   overlay does not support idmapped mounts"）。唯一出路：存储驱动在用户态**物理改写
   磁盘属主**（`storage-chown-by-maps`，全层递归 chown，~2s/GB；ext4 上每文件都是
   元数据重写，btrfs 因 CoW chown 是纯元数据操作所以不受影响，见 podman #16830）。
4. **chown 是首次使用触发、就地持久化**（共享层只 chown 一次）→ "第二次快"。
   实测：全存储已无任何 uid-0 文件层；44s 过的镜像再次 create 仅 0.02s；
   二次 create strace 0 次 chown。
5. **commit 破坏状态**（用户直觉确认）：容器可写层文件以"已映射"的宿主 uid 写入；
   commit 把它固化成新镜像层，但**层的 uidmap 元数据丢失/为空**（container-libs
   PR #734："for container layers created when shifting is possible, we store an
   empty map there"；containers/storage #2396 同款老问题）。下次从该镜像建容器，
   native overlay 路径无法解释磁盘属主 → 对整个新层重跑 chown 同步。每次重建 =
   新 commit 层 = 重新付校验开销（精确机制见第 7 点修正）。
6. **fuse-overlayfs 为什么可以**：守护进程运行在**自己的独立 userns**（实测
   daemon-0 = 宿主用户 1000，daemon-1+ = subuid 段）；属主翻译由 FUSE 层
   （daemon + 内核 FUSE idmap）在**挂载视图**完成（PR #735：“the fuse driver
   performs id map over the entire FS”）。实测：镜像文件容器内呈现 `0:0`、
   用户文件呈现 `1000:1000`，磁盘文件**从不改写**（strace 全程 0 chown）。
   因此不依赖“磁盘属主 == 目标映射”前提，commit 层直接可用。
7. **缓存机制与 44s 的精确归因（第二轮实证，修正第 3/5 点）**：
   - **标准镜像 + keep-id 永远秒开**：全新 pull 的 hello-world 在 native overlay
     下首次 keep-id create = 0.10s，0 次 fchownat。标准镜像从不付准备开销。
   - **44s = commit 镜像特有的“一次性全栈校验扫描”**：首次从 commit 镜像建容器时
     驱动对整个层栈做全树遍历（~30µs/文件，17.9GB ≈ 44s，与镜像总尺寸成正比：
     20.6GB→44s，1.16GB→2.7-4.3s）。证据：44s 窗口内 base 层 0 个文件 ctime
     变化（纯读扫描），commit 层 1156 文件仅少量真实 chown。
   - **同一 commit 镜像的后续 create 永远秒开**（0.02-0.08s 多次实测），状态
     持久化在 podman 元数据。
   - **“缓存”的精确形态**：`overlay-images/images.json` 每镜像 `mapped-layers`
     （已 prepared 层 ID 列表）+ `overlay-layers/layers.json` 每层 `uidmap`/`gidmap`
     （该层按哪个映射写的）。commit 新层在 commit 时**自动记录自己的映射**
     （实测 de21fb2 条目）；base 层记录不随镜像继承。
   - **“把旧镜像缓存复制给新镜像”的评估**：共享 base 层本就是同一层对象，无需
     复制；直接编辑 images.json **不建议**（socket 激活 daemon 有内存态会覆盖，
     字段语义随版本漂移，无 API 保障）；**安全等价做法 = 预热（prewarm）**：
     commit 后立即后台跑一次 `podman create --userns keep-id <新镜像> /true && rm`，
     44s 由 podman 自己记录，之后的重建 create 秒开。

## 0.2 三道路决策（2026-09-07）

| 维度 | 道路一：保持现状 | 道路二：fuse-overlayfs | 道路三：自研引擎 |§
|---|---|---|---|
| 重建速度（20GB 环境） | ~44s，每次重建都慢 | **<1s**（实测 0.09s） | **<1s**（设计上免疫） |
| easytidy 代码量 | 0 | ~1 周（存储健康检测/修复，3 提交） | 4-8 周 MVP + 长期维护 |
| 用户安装成本 | 0 | `apt install fuse-overlayfs`（检测+引导） | crun（大概率已有）+ fuse-overlayfs |
| 消除"podman 实现问题"这一类 | ❌ | ❌（只绕开本例） | ✅（podman 依赖整体移除） |
| 风险 | 0 | 低（发行版标准 rootless 推荐） | 中高（新核心技术，安全面） |
| 长期维护负担 | 0 | 0 | 高（内核/cgroup/镜像格式演进都归我们） |

### 道路一：保持现状（忍受缓慢）

- 不动宿主配置，不写代码。
- 代价量化（第 7 点修正后）：仅「commit 镜像首次使用」付一次全栈校验扫描：
  ~2s/GB（按文件数计）。20GB 环境 ≈ 44s；1GB ≈ 2-4s。**标准镜像与同镜像
  第二次起均秒开**——即每次重建（新 commit 镜像）付一次。
- 可选缓解：**预热（prewarm）**——commit 后立即后台跑 throwaway keep-id
  create，把 44s 移到"镜像准备"阶段（可后台/可显示进度），用户点重建时秒开。
  这是"复制缓存"想法的安全产品化形式（让 podman 自己记录，不碰内部元数据）。
- 适用：用户环境镜像普遍 <1GB，或用户可接受重建等待。
- 定位：**保底选项**，不是推荐归宿。

### 道路二：fuse-overlayfs（§0.3-§8 详设）

- 用户安装 fuse-overlayfs（一条 apt 命令），easytidy 提供检测 + 一键配置修复 + 回滚。
- 已实证：44s → 0.09s；fuse 生命周期归 crun/systemd 管（实测正常 stop 零残留），
  easytidy 无需管理（只需事前检查二进制 + 错误透传）。
- 代价：用户多装一个组件；easytidy ~1 周。
- 局限：**podman 仍是黑盒依赖**——本次是绕开它的一个实现问题，不是消除这一类问题
  （libpod API 怪癖、socket 激活复杂性、commit 层元数据丢失等仍在）。

### 道路三：自研引擎（§9 拆解）

- 动机：已多次遭遇 podman 自身实现问题（见 §9.1 清单）。
- 关键判断：easytidy 已具备"管理层"~80%（GUI、配置、systemd 自启、协议、
  server daemon），缺的是"引擎层"（镜像/层存储 + 运行时驱动）——**这不是
  "重写 podman"，而是"把 podman 白送的引擎自己写出来"**，且可以复用 crun
  （podman 自己也在用的底层 OCI 运行时）。
- 定位：长期战略选项；投入大，但把"podman 类问题"整类消除。

**推荐组合（待拍板）**：短期上道路二立即止痛；道路三以 1-2 周 spike 验证最不确定
环节（直接驱动 crun 跑通 create/exec/pty），再决定全量投入。道路一作为保底。

## 0.3 道路二：目标 / 非目标

**目标**
1. 检测：easytidy 能诊断出「rootless + overlay 驱动 + 未配 mount_program」
   这一高危组合（对用户体验是"首次重建/创建慢 ~44s"）。
2. 修复：用户确认后一键写入配置（备份 + 验证 + 可回滚）。
3. 指导：fuse-overlayfs 未安装时给出安装命令。

**非目标**
- 不自动执行（不静默改宿主配置）——必须用户显式触发。
- 不处理 btrfs/zfs 底层存储等其他 overlay 挂载问题（另案）。
- 不替代 podman 自身的 `podman info` 能力，只做「检测 + 引导 + 一键修复」。

## 1. 诊断模型

```rust
// crates/core/src/storage_health.rs（新模块）

pub enum Verdict {
    /// 已配置 fuse-overlayfs（或已满足其他快速路径）
    Ok,
    /// rootless + overlay + 未配 mount_program + fuse-overlayfs 已安装 → 可一键修复
    Recommended,
    /// 同上但 fuse-overlayfs 未安装（或 /dev/fuse 缺失）→ 需先安装
    NeedsInstall,
    /// 不适用（非 rootless、驱动非 overlay 等）
    NotApplicable,
}

pub struct StorageDiagnosis {
    pub rootless: bool,                    // 非 root 且走 rootless 存储路径
    pub driver: String,                    // "overlay" | "vfs" | ...
    pub mount_program: Option<String>,     // GraphOptions 里 overlay.mount_program
    pub fuse_overlayfs_path: Option<PathBuf>,  // which / 常见路径探测结果
    pub dev_fuse: bool,                    // /dev/fuse 存在
    pub verdict: Verdict,
}
```

判定表：

| rootless | driver | mount_program | fuse 可用 | verdict |
|---|---|---|---|---|
| ✗ | any | any | any | NotApplicable（root 场景无此问题） |
| ✓ | vfs | — | any | NotApplicable（vfs 无 chown 问题，但本就慢，不在本案范围） |
| ✓ | overlay | 已设 | any | Ok |
| ✓ | overlay | 未设 | ✓ | **Recommended** |
| ✓ | overlay | 未设 | ✗ | **NeedsInstall** |

数据源：
- `GET /v<ver>/info`（Docker 兼容端点，`libpod.rs` 新增方法）→
  `Store.GraphDriverName` / `Store.GraphOptions["overlay.mount_program"]`
- `which fuse-overlayfs` + 兜底路径 `/usr/bin` `/usr/local/bin`
- `/dev/fuse` 存在性
- rootless 判定：`uid != 0` 且 XDG_RUNTIME_DIR 指向 `/run/user/<uid>`

诊断是**只读**操作，开销一次 HTTP + 几次 stat，可随时调用（不缓存也行；
GUI 侧建议启动时调一次 + 修复后重调）。

## 2. 修复动作（apply_fix）

```rust
pub struct ApplyReport {
    pub backup_path: Option<PathBuf>,  // 原 storage.conf 备份（无原文件则 None）
    pub config_path: PathBuf,
    pub verified: bool,               // 写后重新 diagnose 确认 GraphOptions 生效
    pub note_daemon: bool,            // 若用户跑常驻 podman system service 需手动重启
}

pub async fn apply_fix(podman: &Podman) -> Result<ApplyReport>;
pub fn restore_backup(backup: &Path, config: &Path) -> Result<()>;
```

步骤：
1. `config_path = $XDG_CONFIG_HOME/containers/storage.conf`（不存在则视为空）
2. **备份**：存在则复制为 `storage.conf.bak-<YYYYmmdd-HHMMSS>`
3. **TOML 合并写**（不覆盖已有内容）：
   - 解析现有 TOML → 确保 `[storage] driver="overlay"`（已有其他 driver 值则
     **中止并报错**，不擅改驱动）
   - 合并 `[storage.options.overlay] mount_program = "/usr/bin/fuse-overlayfs"`
     （fuse-overlayfs 实际路径用探测结果，不写死）
   - 已存在相同值 → 幂等，直接返回 verified=true
4. **验证**：重新 `diagnose()`，确认 `mount_program` 出现在 GraphOptions
5. **daemon 提示**：检测 `systemctl --user is-active podman.socket`
   - socket-activated（本机形态）→ 无需重启，新连接自动生效
   - 常驻 daemon → `note_daemon=true`，提示用户手动重启

回滚 = `restore_backup()` + （同上 daemon 提示）。

## 3. CLI

```
easytidy doctor              # 只读诊断，输出人类可读报告
easytidy doctor --fix        # 诊断 + 执行修复（交互确认）
easytidy doctor --rollback   # 用最近一份备份回滚
```

- 加到 `Commands` 枚举；输出风格对齐现有命令（plain text + 明确的成功/失败标记）
- `--fix` 非交互场景（CI）加 `--yes` 跳过确认

## 4. GUI

### 4.1 入口：MasterView 顶部健康横幅

- 启动时（及每次进入主界面）调一次 `diagnose()`
- `verdict == Recommended` → 琥珀色横幅：
  > 检测到 rootless + 原生 overlay：首次从镜像建容器会慢约 44 秒（~2s/GB）。
  > 可一键切换到 fuse-overlayfs（已有配置自动备份，可回滚）。
  > **[一键修复]**　[安装说明]　[忽略]
- `verdict == NeedsInstall` → 同位置横幅，主按钮变 **[查看安装命令]**
  （`sudo apt install fuse-overlayfs`，按发行版给命令）
- `verdict == Ok / NotApplicable` → 不显示
- **[忽略]**：写用户级 dismissed 标记（`~/.easytidy/data/` 下小配置文件），
  横幅不再出现；设置页保留「重新检查存储健康」入口
- 横幅不阻塞任何操作，纯提示

### 4.2 一键修复流程

```
[一键修复]
  → Modal 确认：说明影响面
      - 现有容器/镜像不受影响，运行中容器下次重启后自动切到 fuse-overlayfs
      - 已备份原配置：<backup_path>
      - 若你手动跑着常驻 podman system service，需重启它
    [确认修复] [取消]
  → apply_fix()
  → 成功：message.success + 横幅消失 + 显示「已验证生效」
  → 失败：modal.error 给具体原因 + [恢复备份] 按钮
```

## 5. 边界与风险

| 场景 | 处理 |
|---|---|
| 已有 storage.conf 含其他 overlay 选项 | TOML 合并保留，只加 mount_program |
| 已有 storage.conf 但 driver ≠ overlay | 中止，提示人工处理 |
| mount_program 已配 | verdict=Ok，幂等 |
| fuse-overlayfs 装了但路径非常规 | which + 常见路径探测；都找不到按 NeedsInstall |
| 老版本 podman GraphOptions key 不同 | 5.x 稳定（`overlay.mount_program`）；4.x 尽力解析，解析失败降级为「未知」不误导 |
| 并发修复 | core 侧 mutex，同时只允许一个 apply |
| 应用写宿主配置的接受度 | 显式按钮 + 备份 + 回滚 + 文案透明；绝不静默 |
| 运行中容器 | 不受影响（rootfs 挂载在容器自身 mount ns），下次重启切换 |

## 6. 测试

- **单测**：verdict 判定表（全组合）；TOML 合并（空文件/无 overlay 节/有
  其他选项/幂等/driver 冲突中止）
- **集成**：mock `/info` 响应（含/不含 mount_program）
- **手工**：本机已验证全链路（2026-09-07：44s → 0.09s，keep-id 与 bind mount 均正常）

## 7. 实施顺序（建议拆 3 个提交）

1. `feat(core): 存储健康诊断（libpod /info + storage_health 只读判定）` + `easytidy doctor`
2. `feat(core): fuse-overlayfs 一键修复（备份/合并写/验证/回滚）` + `doctor --fix/--rollback`
3. `feat(gui): 存储健康横幅 + 一键修复 Modal`

## 8. 关键文件落点

| 改动 | 文件 |
|---|---|
| 诊断/修复逻辑 | `crates/core/src/storage_health.rs`（新） |
| `/info` 端点 | `crates/core/src/libpod.rs`（加方法） |
| 模块导出 | `crates/core/src/lib.rs` |
| doctor 子命令 | `crates/cli/src/main.rs`（Commands 枚举） |
| 横幅 + Modal | `crates/gui/ui/components/MasterView.tsx`（+ 必要时新组件 `StorageHealthBanner.tsx`） |
| dismissed 标记 | `crates/core/src/configfile.rs` 或新的小持久化点（待定，倾向用户级 JSON） |

## 9. 道路三：自研引擎（easytidy-native）初步拆解

### 9.1 动机：已遭遇的 podman 实现问题清单

1. **storage-chown-by-maps**（本文主题）：rootless + keep-id 首次从镜像建容器全层 chown，
   ~2s/GB；且 commit 层 uidmap 元数据丢失（container-libs PR #734）导致**每次重建都是
   新层、每次都慢**。
2. **libpod API 集成怪癖**：`libpod.rs` 被迫手写 raw HTTP——端点要 API 版本前缀
   （裸 `/libpod/...` 不可用）、请求体是 "Docker-compat 形状 + libpod 扩展" 的混血、
   不同 podman 版本行为漂移。
3. **socket 激活复杂性**：`podman.socket`/`podman.service` 按需拉起、空闲自杀，
   配置变更时机、谁持有 socket、重启语义都要绕着它的行为设计（本会话实测踩到：
   长驻 daemon 会缓存旧存储配置）。
4. **commit 语义与预期不符**：`--squash` 不带 `-all` 的层数行为、镜像元数据与磁盘
   属主的不一致（§0.1 机制链），排查成本极高。

### 9.2 Rust 生态调研（2026-07）：已有的 vs 缺的

| 层 | Rust 生态现状 | 成熟度 |
|---|---|---|
| OCI 运行时 | **youki**（youki-dev/containers 组织，7.5K★，2026-07 发 0.7.0，runc drop-in，支持 rootless）；crun 是 C 但 podman 实战验证 | youki 成熟但 **rootless 较新**（2025-11 报 bug，2026-04 关闭）；crun 更稳 |
| 镜像拉取 | **oci-client**（原 oci-distribution，oras-project）：540 万下载、67 依赖方（wasmCloud 在用）；完整 pull（auth/多平台/并发下载/digest 校验） | 成熟，直接用 |
| 管理层（libpod 等价：镜像存储+状态机+exec/pty+commit） | **没有成熟的**。podup（Rust）只是 libpod REST 客户端；qcker/Containust/hippobox 均为早期/实验项目 | 缺口 = 我们自研的部分 |

结论：最难最脏的两块（运行时、拉镜像）已有成熟实现，**当二进制/库复用即可**；
真正要写的是管理层里的"层存储 + 状态机 + exec/pty + commit"。

### 9.3 关键判断：缺的是"引擎层"，不是"重写 podman"

easytidy 现有资产（管理层）：GUI/CLI、容器配置（每容器 toml）、systemd user
自启 unit、私有协议（protocol crate：Frame/pty/事件）、server daemon（已在容器内跑
easytidy-server）、dock 引导。

### 9.4 核心组件清单（道路三要实现/复用的全部）

**A. 直接复用（零自研）**

| 组件 | 选型 | 说明 |
|---|---|---|
| OCI 运行时 | **crun**（求稳）/ **youki**（求纯 Rust） | 当子进程驱动：生成 OCI bundle（config.json + rootfs）直接 exec。spike 阶段双选验证 |
| 镜像拉取 | **oci-client** crate | registry v2 全功能；初期若遇边界问题可 shell 出 skopeo 兑底 |
| rootfs 挂载 | **fuse-overlayfs**（rootless） | 道路二已验证；属主语义由我们设计，44s 问题设计上免疫 |
| 容器监控 | **conmon 复用**（暂定）/ 自研进程监控（备选） | 决策点：conmon 处理 console 接线/退出码/收割（C，containers 组织）；自研 = 直接 fork 容器 init 自己监控，少一个依赖但 pty 接线自己写 |
| cgroup / 自启 | systemd user | 已有集成（systemd crate） |
| tar/解压 | tar/flate2/zstd（或 oci-client 自带） | |

**B. 自研（核心新代码，按依赖顺序）**

1. **LayerStore 层存储**（1-2 周）
   - 内容寻址层目录（diff-digest 去重，镜像间共享）
   - tar 解包 + **OCI whiteout**（`.wh.` 文件、opaque 目录）
   - **属主语义设计**：一开始就存成正确形态（fuse 友好 / 一致映射）——这是 podman
     44s 问题被连根拔的地方
   - **commit**：容器 diff → 新层，记录完整 uidmap 元数据（不学 podman 丢）
   - 未引用层 GC
2. **BundleAssembler OCI bundle 组装**（~1 周）
   - rootfs：lowerdirs+upperdir → fuse-overlayfs 挂载
   - /proc、/dev（tmpfs+mknod）、/dev/pts
   - easytidy 的 bind mount 集（X11/Wayland/XAUTHORITY、fonts/icons、
     Downloads、.pi、gui_agent、/run/easytidy 等，复用现有 mount 配置逻辑）
   - nvidia 设备直通（CDI 或 device 直通）
   - **config.json 生成**：userns keep-id、uidmap/gidmap、capabilities、cgroup——
     安全面核心，集中一处 + 单测
3. **ExecManager exec/pty**（1-2 周，**最重最不确定**）
   - 多 exec 会话（crun exec / youki exec）
   - pty 分配/resize（protocol crate 协议已就绪，对接即可）
   - stdin/stdout/stderr 复用与分流、exit 收割
   - 对接 GUI Terminal/RootTerminal 与容器内 easytidy-server
4. **ContainerStateMachine 容器状态机**（~1 周）
   - create/start/stop/restart/rm/rebuild 生命周期
   - 运行时状态持久化（pid/退出码/时间戳）——现有每容器 toml 扩展
   - 事件发布（protocol 事件机制已有，事件源从"轮询 podman events"换成自己的状态机）
5. **ImageStore 镜像存储**（~1 周，拉取部分基本免费）
   - manifest/config/层栈/tag 元数据（SQLite 或 toml，与现有配置同风格）
   - pull → 去重入 LayerStore（oci-client）
   - commit → 新镜像；save/load（兼容 podman save 格式，供迁移）
   - （可选）`easytidy import <podman-save-tar>` 迁移命令

**C. 决策点（spike 阶段定）**

1. crun vs youki（rootless 稳定性、多 exec、console 行为实测对比）
2. conmon 复用 vs 自研进程监控（依赖数 vs pty 接线工作量）
3. 存储布局：fuse-only（简单）vs kernel overlay + fuse 回退（快但复杂）
4. 状态存储：SQLite（与现有 libpod db 同风格、查询方便）vs toml（与现有容器配置同风格）

### 9.5 能砍掉的 vs 要背上的

**砍掉**：podman 二进制依赖、`podman system service`/socket 激活、`libpod.rs` raw HTTP、
containers/storage 全部 legacy 问题、podman 版本漂移测试矩阵。

**背上**：镜像格式边界（zstd、多架构、digest 校验）、crun 版本兼容、cgroup v1/v2 差异、
内核演进关注；安全面（namespace/mount 配置由我们生成——执行者是 crun，但 spec 是我们的
责任）；与 podman CLI 不再同宿主互操作（镜像可经 registry / `podman save` 迁移，
可出 `easytidy import` 迁移命令，MVP 非必须）。

### 9.6 工作量与风险估算

- **MVP**（pull/create/start/stop/rm/exec/pty/commit/rebuild，host net，单用户 rootless）：
  4-8 周专注投入。分配参考：spike 1-2 周 → LayerStore 1-2 周 → ExecManager 1-2 周
  → BundleAssembler 1 周 → StateMachine 1 周 → ImageStore 1 周（与 LayerStore 并行）
  → 集成测试 1 周。两大不确定块：ExecManager、crun/youki rootless 行为。
- **Spike（建议先做，1-2 周）**：手工 rootfs + 手写 config.json → 直接驱动 crun/
  youki 跑通 create/exec/pty/stop，验证：rootless 下的 cgroup/console 集成、我们的
  pty 协议对接、fuse-overlayfs rootfs 挂载、多 exec 会话。spike 通过 → 全量投入；
  不通过 → 回到道路二。
- **长期**：MVP 后进入"慢速长尾"（镜像格式边界、错误路径、性能），并变成核心维护项。

### 9.7 风险

| 风险 | 评估 | 缓解 |
|---|---|---|
| pty/exec 复杂度超预期 | 中 | spike 优先验证；可先只做单 exec 会话 |
| 镜像拉取边界情况 | 中 | 初期 shell 出 skopeo（成熟工具），层存储自研先行 |
| 安全面（namespace/mount spec） | 中 | spec 生成集中在一处 + 单测；crun 是已审计执行者 |
| 团队带宽冲突 | 高 | 与产品路线排期冲突；建议 spike 后再决策全量 |
| 与 podman 生态分叉 | 低（可控） | 镜像格式兼容（registry 同源）；save/import 迁移命令 |

### 9.8 决策记录（待填写）

- [ ] 道路二是否先行落地（短期止痛）
- [ ] 道路三 spike 是否立项（1-2 周）
- [ ] spike 结果 → 全量投入 / 终止
- [ ] 运行时：crun / youki
- [ ] 容器监控：conmon 复用 / 自研进程监控
- [ ] 存储布局：fuse-only / kernel overlay + fuse 回退
- [ ] 状态存储：SQLite / toml

> 注：道路二三不互斥——道路二可独立上线；道路三若立项，其引擎层设计（层存储属主
> 语义、rootfs 挂载）可直接借鉴道路二验证过的 fuse-overlayfs 经验。
