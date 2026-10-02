# 23 · 属主安全重建（归位 → 固化一体）设计

> 状态：**已否决**（2026-09-27 用户决策：不在重建流程中做任何归位修复；
> 归位职责回归「首启 home 子树 repair」既有行为，根目录树化石由用户全量重建自行处理。
> 本文档保留作为机制分析与事故记录）
> 关联：docs/21-permission-drift-fix.md、docs/22-fast-rebuild-ownership-final-fix.md
> 前置事实：fork 跳层 bug 已修（`imageTopLayerForMapping` 强制 TopLayer，探针验证链深 11→14 单调递增）

## 1 · 问题定性（同源双症）

家目录权限错乱与根目录树错乱是**同一时代 bug 的两个侧面**，机制已全部取证闭环：

### 1.1 化石的制造机：pull 解包带映射

系统引擎 pull 解包时套了外层 userns 映射翻译，落盘编码错乱（demo 实测）：

| 上游 blob | 应落盘（easytidy 存储编码） | 实际落盘 | 容器 view |
|---|---|---|---|
| root 文件 uid=0 | 100000（容器 0 段） | **1000** | ubuntu ✗ |
| ubuntu 文件 uid=1000 | 1000（恒等段） | **100999** | 999 ✗ |

化石一旦落进 base 层即永久存在——overlay 只认最上层副本。

### 1.2 化石的放大器：快速重建无法清除/传递修正

1. **commit 增量只打包 RW 层实际存在的副本**——base 化石里没被碰过的路径（/bin、/root、
   .bashrc…）永远不在增量里，每轮重建原样透出
2. **变更检测不比属主**——只改属主（chown-only）的文件被判"未变更"不进增量：
   归位成果无法跨代传递；`Documents` 目录丢失事故同机制（chown-only 的目录条目被漏）
3. **（已修复）层选择漂移/跳层**——曾使中间 commit 层被整层绕过，见 23 前 fork 提交

### 1.3 为什么"第一次对、第二次错"

首次启动时归位（first-run repair）以容器 root chown 家目录 → copy_up 写入当届 RW 层 →
**视图正确**。快速重建后新容器从 commit 镜像重建，而 commit 增量漏掉了这些 chown-only
副本 → 新容器穿透到化石 → **第二次就错**。第一次的正确是"写出来的临时副本"，不是基线。

## 2 · 目标与非目标

**目标**：提供一条重建命令，使**一次重建后属主永久正确**——后续任意次快速重建零回退、
零扫描成本收敛、零修改（符合"别修改、保持正确"哲学）。

**非目标**：
- 不改 pull 解包（用户明确要求；化石镜像照常可用，见 §5.4）
- 不做运行时动态扫描（归位只发生在 rebuild 时点）
- 不恢复已 GC 的历史数据（demo/Documents 事故已单独报告）

## 3 · 方案设计：`归位 → fast commit 固化` 一体重建

### 3.1 流程

```
rebuild --fix（或新命令）:
  ① 容器运行态下，以容器 root 全树归位（见 §3.2）
  ② stop 原容器
  ③ fast commit（plain + easytidy_fast）——此时 RW upperdir 包含归位的
     全部 copy_up 副本 → blob 完整，不依赖变更检测是否识别 chown-only
  ④ fast create 新容器（同映射直通，字面落盘）
  ⑤ start + 确认 + 原子替换（现有 rebuild_with_commit 安全流程原样复用）
```

**核心洞察**：归位与 commit **同代执行**——归位写进 RW 的副本就是 commit 的
upperdir 本身，blob 天然完整。这完全绕开「chown-only 不进增量」的检测缺陷，
③ 的代码缺陷对**本命令**不再构成威胁（对其他流程的修复另立任务，见 §7）。

### 3.2 归位执行（容器内 root，排除 bind）

现成通道：`Podman::exec_oneshot(name, "0", cmd)`（`cmd_run_root` 已验证可用，
docs：CLI `run --root` 路径）。归位命令（零新依赖，find+chown）：

```sh
EXCL=""
for m in $(grep -vE 'proc|sysfs|cgroup|devpts|mqueue|tmpfs|overlay ' \
           /proc/self/mountinfo | awk '{print $5}'); do
  [ "$m" = "/" ] && continue
  EXCL="$EXCL -path $m -prune -o"
done
eval find / -xdev $EXCL -exec chown -h 0:0 {} + 2>/dev/null
chown -hR 1000:1000 $HOME
```

语义：
- **挂载点排除**：bind 挂载（宿主 Downloads、X11 socket、nvidia 库、/run/user/1000）
  整棵 prune——**宿主内容绝不可触碰**（demo 事故教训，见 §6）
- **`-h`**：符号链接本体归位，不穿透目标
- **两段式**：全树归 root → 家目录归容器用户（uid/gid 取自 `resolve_identity`）
- **EXCL 构造必须在同一 shell**（历史事故：管道子 shell 中变量丢失导致排除清单为空，
  见 §6）；实现时应改为**参数数组传递**（`exec_oneshot` argv 直传不经 shell，天然
  规避 eval 注入/子 shell 问题——推荐将 find 命令拆为 argv：
  `["/usr/bin/find", "/", "-xdev", "-path", m1, "-prune", "-o", ..., "-exec",
  "chown", "-h", "0:0", "{}", "+"]`）

### 3.3 收敛性（成本分析）

- **首轮**：全树 copy_up（一次性，分钟级上限；大目录实测 demo 全树 <60s）
- **次轮起**：文件属主已正确 → `chown` 同值为内核 no-op（不触发 copy_up）→
  归位退化为纯 lstat 遍历（秒级）→ fast commit 增量≈空 → **回归亚秒级快速重建**
- **稳态**：零扫描成本、零修改、永久正确

### 3.4 与既有命令的关系

| 命令 | 行为 | 用途 |
|---|---|---|
| `rebuild`（现有，squash） | 全量扁平 commit | 换基线/瘦身后；同样会固化当前属主状态 |
| `rebuild --quick`（现有） | fast commit，**无归位** | 日常迭代（属主已正确时） |
| `rebuild --fix`（**新增**） | **① 归位 → ② fast commit 固化** | 属主异常时的一次性手术；此后回到 `--quick` |

`--fix` 也可理解为：把"全量重建"的属主修复能力，以快速重建的成本提供。

### 3.5 接口设计

- **CLI**：`easytidy rebuild --container <name> --fix`
- **GUI**：重建按钮下拉菜单新增「修复属主并重建（--fix）」
- **core**：`Podman::rebuild_with_commit` 前插入可选步骤
  `repair_tree_ownership(name, uid, gid)`（exec_oneshot root，cmd 由
  mountinfo 挂载点动态构造，§3.2）

## 4 · 实现要点

1. **exec 身份**：必须 `User="0"`（`exec_oneshot` 既有参数）；**启动前校验
   `id -u` 返回 0**，否则中止并报错（历史上出现过以 uid1000 执行静默失败的
   事故——chown 全部 EPERM 但脚本照常"成功"）
2. **挂载点清单**：容器内 `mountinfo` 动态生成（bind 点随配置变化，不可硬编码）；
   argv 数组直传，禁止 eval 拼接
3. **归位后启动前**：不做额外校验（commit blob 即事实）；重建完成后由调用方
   （CLI/GUI）抽验关键路径属主并日志记录
4. **失败语义**：归位 exec 失败（非零退出）→ **中止重建**（区别于现有
   prepare 的 best-effort——属主手术必须全有或全无，半途 commit 会固化半成品）
5. **幂等**：重复执行无害（chown 同值 no-op）

## 5 · 解决矩阵

| 问题 | 是否解决 | 机制 |
|---|---|---|
| home 目录权限错乱 | ✅ | 全树归位含家目录；且随 commit 固化不再回退 |
| 根目录树权限错乱（/bin、/root…） | ✅ | 同上——归位范围=全树 |
| 归位成果跨重建丢失 | ✅ | 归位与 commit 同代，blob 完整 |
| 新拉镜像 base 化石 | ✅（容器级） | 容器首建后一次 `--fix` 即清除并固化；镜像层本身仍带化石（不影响使用） |
| 快速重建速度 | ✅ | 次轮起收敛为 no-op + 空增量（§3.3） |
| Documents 已丢数据 | ❌ | 层已被 GC，任何方案无法恢复 |
| pull 镜像本身干净 | ❌ | 见非目标；如需，另立任务 |

## 6 · 事故教训（写入实现的硬性约束）

2026-09-27 demo 容器事故（本设计的直接动因）：

1. **排除清单子 shell 丢失**：`grep | while read` 管道中赋值的 EXCL 在循环外为空
   → find 无排除全树 chown → **穿透 /run/user/1000 bind，宿主 runtime 目录整体
   属主被改为 100000，用户桌面功能损坏**。→ 实现必须 argv 数组直传挂载点，
   禁止 shell 变量拼接
2. **exec 身份未校验**：曾以容器用户（uid1000）执行 chown 全树，全部 EPERM 但
   脚本照常成功退出。→ 实现必须先校验 `id -u == 0`
3. **在生产容器上跑带缺陷脚本**：属操作纪律事故，实验必须先在隔离 store 验证

## 7 · 遗留（本设计范围外）

- `commit 变更检测补属主比对`（c/storage changes 判定）：修复"用户终端内手动
  chown/chmod 后快速重建丢失"的通用缺陷——独立任务
- pull 解包字面化：让镜像本身不带化石——独立任务（用户暂缓）
- demo `Documents` 历史数据：无副本，无法恢复

## 8 · 回归计划（实现完成后）

1. **demo（ubuntu:25.10 + 化石 base）**：`--fix` 一次 → 断言 view：全树 root +
   home 1000 → 快速重建 ×3 → 断言 view 不变、链深度单调递增（探针确认无跳层）
2. **fresh1（debian:12 干净基线）**：`--fix` → 断言 no-op（全树已正确、增量≈空、
   耗时 <2s）→ 快速重建 ×2 → 数据完整
3. **bind 保护**：`--fix` 前后对宿主 `/home/div/Downloads`、`/run/user/1000`、
   `/tmp/.X11-unix` 属主快照比对 → 断言零变化
4. **exec 身份**：构造非 root exec 环境 → 断言中止并报错
