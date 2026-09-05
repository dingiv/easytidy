# 17. AMD GPU 透传与 ROCm 容器内可用性（rootless 实测）

> 2026-09-05 实测沉淀。涉及：easytidy CLI 的 `extra_opts` 适配现状、AMD 设备
> 透传（`/dev/kfd` + `/dev/dri/renderD*`）、rootless 下 `/dev/kfd` 不可用的
> **真正根因（DAC 层：`/dev/kfd` 无当前用户 ACL + rootless keep-id 剥离宿主
> render 补充组，并非 cgroup eBPF 设备过滤器）**、以及两种可用的解法。
>
> **勘误（2026-09-05）**：早期版本把根因归为「rootless cgroup v2 eBPF 设备
> 过滤器单独拦 `508:0`」。实测 errno 是 `EACCES`（DAC）而非 `EPERM`（eBPF/
> capability），且同权限的 `/dev/dri/renderD129` 能开、`/dev/kfd` 不能，唯一
> 差异是 ACL 里有没有 `user:div:rw-`——已证伪 eBPF 归因，本文全部改写。
>
> 环境：rootless podman 5.4.2 / runc 1.3.4 / cgroup v2 / 双显卡宿主（NVIDIA
> card1 0x10de + AMD card2 0x1002）/ 宿主已装 amd-container-toolkit 生成 CDI
> spec（`/etc/cdi/amd.json`）。

## 1. 结论（先说结果）

| 目标                                                         | rootless 下   | 说明                                                                  |
| ------------------------------------------------------------ | ------------- | --------------------------------------------------------------------- |
| `/dev/dri/renderD129`（Mesa / VAAPI / OpenGL / Chrome 硬解） | ✅ 可用        | easytidy `gpu_amd: true` 透传后即可用                                 |
| `/dev/kfd`（ROCm / HIP 的**唯一入口**）                      | ❌ 默认不可用 | `/dev/kfd` 无当前用户 ACL + rootless keep-id 剥掉宿主 render 组 → DAC 拒 |
| 完整 ROCm 栈（`rocminfo` / `rocm-smi` / HIP 计算）           | ❌ 默认不可用 | 缺 `/dev/kfd` 即不可用                                                |
| 完整 ROCm 栈（**解法 2a**：宿主 `chmod 666 /dev/kfd`）       | ✅ 可用（**推荐**）| 多轮实测最有效，和 NVIDIA 设备同理（全局可访问），见 §6    |
| 完整 ROCm 栈（**解法 2b**：宿主 `setfacl` 给当前用户加 ACL） | ✅ 可用        | 只授权当前用户，比 666 安全，与 renderD 现状一致，见 §6    |
| 完整 ROCm 栈（**解法 1**：自定义 gidmap 把 render 映进容器） | ❌ rootless 不可行 | runc 1.3.4 rootless 下手动 `--uidmap` 走不通（`mapping tool not present` → EOF），`newuidmap` 形状限制又映射不进 991（仅 rootful 可行） |
| 完整 ROCm 栈（rootful podman）                               | ✅ 预期可用   | root 容器不剥补充组、无 subuid 范围限制，可任意 gidmap；本环境无免密 sudo，未实测 |

**一句话**：rootless 容器里 AMD「渲染」默认可用，「计算」（ROCm）默认打不通。
根因**不是** cgroup eBPF 设备过滤器，而是 **DAC 层**：`/dev/kfd` 是
`0660 root:render` 且**没有**当前用户的 ACL，而 rootless keep-id 把宿主 render
补充组（GID 991）剥掉了（991 不在 keep-id 的 gidmap 范围内），于是容器进程既
不是 owner、也不属于 render 组、又无 ACL → DAC 拒绝（`EACCES`）。
**最干净的出路是宿主侧放行**（§6 解法 2a：`chmod 666 /dev/kfd`，多轮实测
最有效，与 NVIDIA 设备现状一致；2b `setfacl` 只授权当前用户同样可用）；
「配置容器用户组」（解法 1）在 rootless 下做不到（runc 1.3.4
rootless 下手动 `--uidmap` 走不通，见 §6），仅 rootful 可行。

## 2. 宿主侧事实（实测）

```
/dev/kfd                  crw-rw----  root render  508, 0    ← ROCm 入口 (0660, 无 div ACL)
/dev/dri/renderD129       crw-rw----+ root render  226,129   ← AMD 渲染 (0660, 有 user:div:rw- ACL)
/dev/dri/card2            crw-rw----+ root video   226, 2    ← AMD card (0660, 有 user:div:rw- ACL)
/dev/dri/renderD128       crw-rw----  root render  226,128   ← NVIDIA 渲染
/dev/nvidia*              crw-rw-rw-  root root    ...       ← NVIDIA (0666 全局可访问)
render 组 GID = 991（宿主 div 用户在组内）
div subuid / subgid = 100000:65536
```

**ACL 是关键差异**（`getfacl` 实测）：

```
# /dev/kfd —— 只有 owner/group/other，没有给 div 的 ACL 条目
user::rw-  group::rw-  other::---

# /dev/dri/renderD129 —— 多了一条 user:div:rw-
user::rw-  user:div:rw-  group::rw-  mask::rw-  other::---

# /dev/dri/card2 —— 同样多一条 user:div:rw-
user::rw-  user:div:rw-  group::rw-  mask::rw-  other::---
```

宿主 `div`（在 render 组）直接 open `/dev/kfd` OK（DAC 没问题）。NVIDIA 设备是
`0666 root:root`（全局可访问），所以 NVIDIA 在 rootless 下也能通——**与组/ACL
无关**。这从侧面解释了「为什么 NVIDIA 能、AMD kfd 不能」。

## 3. easytidy CLI 侧的实测

### 3.1 `extra_opts` 的适配现状（部分适配）

`extra_opts`（替代旧 `security_opts`，见 `ContainerParams`）能透传到
`keep_id_create_body`，但**解析器只认三类 `key=value`**
（`crates/core/src/libpod.rs`）：

- `apparmor=<profile>` → `apparmor_profile`
- `label=<opts>` → `selinux_opts`
- `seccomp=<profile|path>` → `seccomp_profile_path`

其余条目（无 `=` 的、或未知 key 的）**静默丢弃**。因此配置里写

```yaml
extra_opts:
  - label=disable
  - apparmor=unconfined
  - --group-add      # ← 被丢弃（无 '='）
  - render           # ← 被丢弃（无 '='）
```

`--group-add render` **不会生效**——它不是 `key=value`，也不是已知 key。

### 3.2 `gpu_amd: true` 的设备透传（正常）

easytidy 的 AMD 路径不走 CDI（`amd.com/gpu=all`），而是创建期探测宿主裸设备
（`detect_amd_gpu_devices`：`/dev/kfd` + vendor=0x1002 的 `renderD*`）注入
libpod body 的 `devices`。实测容器 OCI 配置：

```
devices 含: /dev/kfd, /dev/dri/renderD129   ← 透传成功
linux.seccomp: default profile
```

即**设备挂载这步是通的**，问题出在挂载之后的 **DAC 访问控制**（§4）。

## 4. 根因锁定：DAC（ACL + 补充组剥除），不是 eBPF

### 4.1 决定性证据：errno 是 `EACCES`，不是 `EPERM`

容器内 open `/dev/kfd` 的失败错误：

```
bash: line 1: /dev/kfd: Permission denied      ← EACCES = DAC 拒绝
```

若是 cgroup eBPF 设备过滤器 / capability 拦截，错误会是
`Operation not permitted`（**EPERM**）。实测是 `Permission denied`（**EACCES**），
定性为 **DAC 层**（owner/group/ACL 权限），不是 eBPF。

### 4.2 同权限设备一开一拒 → 排除 eBPF 设备号白名单假设

keep-id 容器内，三个设备**容器内视角都是 `0660 65534:65534`**（nobody:nogroup），
进程身份 `uid 1000 / gid 1000`。若是「按设备号的 eBPF 白名单（放行 `226:*`、
拦 `508:0`）」，三设备该被一致裁决。但实测：

| 设备                   | 宿主权限        | 宿主 ACL              | 容器内 (uid 1000) |
| ---------------------- | --------------- | --------------------- | ----------------- |
| `/dev/kfd`             | 0660 root:render | **无 div ACL**        | ❌ EACCES         |
| `/dev/dri/renderD129`  | 0660 root:render | **有 user:div:rw-**   | ✅ OK             |
| `/dev/dri/card2`       | 0660 root:video  | **有 user:div:rw-**   | ✅ OK             |

kfd 与 renderD129 **同属主、同权限（0660 root:render）**，唯一差异是 ACL 里
有没有 `user:div:rw-`，结果一拒一放——裁决依据是 **DAC（owner/group/ACL）**，
不是设备号白名单。

### 4.3 为什么 DAC 拒 kfd：render 补充组被 rootless keep-id 剥除

容器 1 号进程在**宿主侧**的真实身份（`/proc/<hostpid>/status` 实测）：

```
Uid:    1000  1000  1000  1000
Gid:    1000  1000  1000  1000
Groups: 1000           ← 只有主组 div(1000)，render(991) 被剥掉了
CapEff: 0000000000000000
Seccomp: 0
```

keep-id 的 gid_map 是 `0→100000(1000个) / 1000→1000(1个) / 1001→101000(64536个)`。
宿主 render GID **991 < 100000，不在任何映射段内**，无法映射进容器，rootless
运行时重建 userns 凭证时把 render 补充组**剥除**了。于是容器进程（宿主身份
uid 1000）对 `/dev/kfd`（root:render 0660，无 div ACL）：既非 owner（root）、
又非 group（render 被剥）、又无 ACL → **DAC 拒绝**。而 renderD129 / card2 因有
`user:div:rw-` ACL，直接放行 div（uid 1000）→ 能开。

### 4.4 对照实验矩阵（修正版）

| #   | 环境 / 手段                                            | `/dev/kfd`  | `/dev/dri/renderD129` | 说明                                       |
| --- | ------------------------------------------------------ | ----------- | --------------------- | ------------------------------------------ |
| A   | `podman unshare`（保留宿主完整凭证，含 render 组）     | ✅ OPEN OK  | ✅                    | DAC 放行（render 组在）                    |
| B   | 容器，`--userns=keep-id`                               | ❌ EACCES   | ✅                    | render 组被剥 + kfd 无 div ACL             |
| C   | 容器 + `--group-add 991`（显式 render）                | ❌          | ✅                    | 容器内 991 映射到宿主 100991（非 render）   |
| D   | 容器 + `--group-add keep-groups`                       | ❌          | ✅                    | rootless runc 下补充组不进 userns（宿主侧 Groups 仍 1000） |
| E   | 容器 + `--security-opt seccomp=unconfined`             | ❌          | ✅                    | seccomp 非拦截层                            |
| F   | 容器 + `--privileged`                                  | ❌          | ✅                    | 非 capability 问题                          |
| G   | 容器内 `exec --user 0`（容器 root = 宿主 subuid 100000）| ❌          | ❌                    | 容器 root 无 render 组也无 div ACL         |
| H   | 容器内自建 render 组(gid 991)+用户加入                 | ❌          | ✅                    | 容器 gid 991 → 宿主 100991，非 render 991   |

**决定性对照 A vs B**：同一设备、同一宿主用户、DAC 条件相同，唯一差异是「render
补充组是否保留」。unshare 保留（能开），keep-id 容器剥除（不能开）——**根因 =
rootless keep-id 剥除宿主 render 补充组 + `/dev/kfd` 无当前用户 ACL（DAC）**。

## 5. 网传方案勘误

常见文章给出的 rootless AMD 方案：

```
podman run --userns=keep-id --group-add keep-groups \
  --device /dev/kfd --device /dev/dri \
  --security-opt seccomp=unconfined <rocm-image> rocm-smi
```

在本环境**逐参数实测均无效**（§4 矩阵 C/D/E/F），且其归因（「组不匹配 + cgroup
设备白名单 + seccomp」）已被证伪。实测：

- **组不是钥匙**：`--group-add keep-groups`（D）在 rootless runc 下宿主补充组根本
  不透传进 userns（宿主侧 `Groups` 实测仍 `1000`）；即便透传，991 也不在 keep-id
  的 gidmap 范围内，无意义。
- **`--group-add 991` 无效**：容器内 gid 991 经 keep-id 段1（容器 `[0,999]` →
  宿主 `[100000,100999]`）映射到宿主 **100991**，不是 render 991，DAC 不认。
- **seccomp 非拦截层**：unconfined（E）仍拒，且 errno 是 EACCES 非 EPERM。
- 真正缺的是「让容器进程的宿主身份**真正属于 render 991**」，两条路见 §6。

## 6. 出路

### 解法 2（推荐，rootless 下最干净）：宿主侧放行

容器进程在宿主侧的身份是**当前登录用户**（uid 1000，keep-id 下与宿主锁死）。
既然宿主 `div` 能开 kfd，把宿主的 DAC 授权对齐即可——容器侧零改动。两种粒度：

**2b：只授权当前用户 —— 与 renderD 现状一致**

`/dev/dri/renderD*` 之所以 rootless 下能开，正是 udev 给当前用户加了
`user:div:rw-` ACL（`getfacl` 实测）。kfd 缺的就是这一条。给它补上：

```bash
sudo setfacl -m u:div:rw /dev/kfd        # rootless 下需宿主 root 执行
```

- **优点**：只授权 `div`，不影响其它用户（比 666 安全）；与 renderD 的处理方式
  完全一致，行为统一。
- **局限**：**重启后失效**（设备节点被 udev 重建、ACL 被清），需 udev rule 持久化：

  ```
  # /etc/udev/rules.d/99-kfd-acl.rules
  # 注意: udev 没有直接设 ACL 的键, 需用 RUN 调 setfacl
  KERNEL=="kfd", RUN+="/usr/bin/setfacl -m u:%E{SUDO_USER}:rw /dev/kfd"
  # 或更简单(单用户宿主): KERNEL=="kfd", MODE="0666" 走 2a
  ```

**2a（推荐）：全局放行 —— 和 NVIDIA 设备同理（多轮实测最有效）**

把 `/dev/kfd` 权限改成 `0666`（全局可读写），和 NVIDIA 设备（`0666 root:root`）
一致——任何进程都能开，不再依赖 render 组或 ACL：

```bash
sudo chmod 666 /dev/kfd
# 持久化:
# /etc/udev/rules.d/99-kfd.rules
# KERNEL=="kfd", MODE="0666"
```

- **实测结论（2026-09-05）**：与 2b（setfacl）及解法 1（gidmap）多轮对照
  实测，2a 是落地最快、效果最稳的解法，定为推荐。

- **安全**：kfd 是 GPU compute 入口，0666 后本机**任何用户**都能提交 GPU 任务。
  rootless 本机单用户场景可接受；多用户共享宿主请优先 2b。
- **两者共同点**：easytidy 现有 `gpu_amd: true` 配置**无需任何改动**，rootless /
  rootful 通吃；都需宿主 root 执行 + udev rule 持久化。

### 解法 1：配置容器用户组 —— rootless 下**不可行**（仅 rootful）

思路：用**自定义 `--gidmap`** 把宿主 render GID 991 映射进容器，让容器进程的
宿主身份**真正属于 render 991**，从而通过 kfd 的 group 权限。

**本环境实测（podman 5.4.2 + runc 1.3.4，2026-09-05）：rootless 下走不通**，
根因是 **runc 1.3.4 的 rootless 路径对手动 `--uidmap`/`--gidmap` 的 mapping tool
机制失败**，与 991 在不在 subgid 无关：

1. **手动 `--uidmap` 直接 EOF**：声明任意显式映射（**即使形状完全等于
   `--userns=keep-id`**）都报：

   ```
   nsexec-0[400011]: mapping tool not present: operation not permitted
   runc create failed: ... can't get final child's PID from pipe: EOF
   ```

   runc 在 rootless 下处理显式映射需调用 `newuidmap`/`newgidmap`（mapping tool）
   写 `/proc/self/uid_map`，本环境该调用被拒（`operation not permitted`）→
   nsexec 子进程崩溃 → EOF。对照 `--userns=keep-id`（内置路径，不依赖 mapping
   tool）**能跑通**——问题就在「显式映射 + rootless + runc 1.3.4」这条组合。
2. **`newuidmap` 本身对映射形状有硬限制**（手动调 `newuidmap` 验证）：只允许
   「第一段 = 用户自身 uid + 第二段 = **全量 subuid 连续段**」的形状
   （如 `0 1000 1 / 1 100000 131072`，即 keep-id 实际形状）。全量段只覆盖
   `[100000, 231071]`，**无法插入 render 991**（991 < 100000，不在该连续段内；
   即便 `/etc/subgid` 单独加 `div:991:1`，全量段仍是 `1 100000 131072`，不含
   991）。所以即使 runc 能用 mapping tool，也映射不进 991。
3. **补充组剥除**：`--group-add render`/`keep-groups` 在 rootless 下也不透传宿主
   render 补充组（§4.3 宿主侧 `Groups: 1000`）。

- **结论**：「配置容器用户组」在 **rootless 下做不到**——显式映射被 runc 1.3.4
  的 mapping tool 机制挡住，`newuidmap` 形状限制又映射不进 991，补充组也被剥。
  它**只在 rootful 下成立**：root 容器无 subuid 范围限制、无 mapping tool 限制、
  不剥补充组，可 `--gidmap 991:991:1` 直接映射任意宿主 gid，再容器内加组。
- **可能的解锁（未在本环境验证）**：升级 runc 到修复 rootless 显式映射的版本，
  或换用 `crun`（本环境未装）。两者都属宿主侧变更，不改变 rootful 才是稳妥解法
  的结论。

### 解法 3（兜底）：rootful podman

root 容器：①不剥宿主补充组（render 991 保留）；②无 subuid 范围限制，可任意
`--gidmap` 映射。两条路（解法 1 的容器用户组 / 解法 2 的宿主放行）都通，`/dev/kfd`
预期可直接访问。本环境无免密 sudo，未实测。

## 7. 对 easytidy 的配置语义（落地结论）

- AMD 透传开关 = `gpu_amd: true`（存意图，创建期探测宿主裸设备注入；
  **不落盘设备路径**——render 节点号随重启漂移）。
- `extra_opts` 目前只支持 `apparmor=` / `label=` / `seccomp=` 三类 `key=value`；
  **不支持原始 flag**（`--group-add` 等会被静默丢弃）。即便支持，对 kfd 也
  无济于事（§4/§5）——真正要落地的是 §6 的**解法 2a（宿主 `chmod 666 /
  dev/kfd`，多轮实测最有效；2b `setfacl` 同样可用）**，在 easytidy 配置层之外
  （宿主 root 操作）。easytidy 只负责设备透传 + create 期 kfd 预检 warn（kfd 被
  DAC 拒时提示修复命令，不阻断创建、不代执行宿主 root 命令）；解法 1（容器
  用户组）在 rootless 下不可行（§6），无需在配置层考虑。
- **配置一次，多种宿主都正确**：`gpu_amd: true` 在 rootless 宿主下 kfd 默认打
  不开但**无害**（设备在、权限拒，不影响容器其余部分），renderD 可用；在「解法 2
  已落地」或 rootful 宿主上则 ROCm 完整可用。无需区分宿主。
- 容器内验证脚本（无 python3 的最小镜像同样可跑）：

  ```bash
  podman exec <c> bash -c '
    for d in /dev/kfd /dev/dri/renderD129; do
      if (exec 3<>"$d") 2>/dev/null; then echo "$d: OK"; else echo "$d: DENIED"; fi
    done'
  ```

  rootless 宿主（未落地解法）预期输出：`kfd: DENIED`、`renderD129: OK`；
  落地解法 2 后：`kfd: OK`。

## 8. 脱离 easytidy 的手工测试（仅用 podman CLI）

> 本节不依赖 easytidy，任何人拿到一台带 AMD GPU 的 Linux + rootless podman，
> 照抄即可判断「这台机器的 rootless 容器到底能不能用 ROCm」。

### 8.1 前置检查（宿主侧，30 秒）

```bash
# 1) AMD 设备节点在不在
ls -l /dev/kfd /dev/dri/renderD* 2>&1
#   期望: /dev/kfd (508:0) + 至少一个 /dev/dri/renderD*
#   哪个 renderD 是 AMD 的? 看 vendor:
for f in /sys/class/drm/card*/device/vendor; do echo "$f: $(cat $f)"; done
#   0x1002 = AMD, 0x10de = NVIDIA; 记下 AMD 那个 card 对应的 renderD 号

# 2) 当前用户能不能直接开 kfd（排除 DAC 这一层）
( exec 3<>/dev/kfd ) 2>/dev/null && echo "宿主直开 kfd: OK" \
  || echo "宿主直开 kfd: DENIED(当前用户不在 render 组, 先 usermod -aG render \$USER 重登)"
#   期望: OK

# 3) 关键: 看 /dev/kfd 有没有当前用户的 ACL（决定 rootless 下能不能通）
getfacl -p /dev/kfd
#   若只有 user::/group::/other:: 而无 user:<你>:rw- → rootless keep-id 下必 DENIED
#   （因为 keep-id 会剥掉 render 补充组, kfd 又没给你 ACL）

# 4) 运行时
podman --version
podman info --format '{{.Host.OCIRuntime}}'
```

### 8.2 判定 rootless 下 kfd 能否用

**A. 纯 userns 基线**——保留宿主完整凭证（含 render 组），DAC 应放行：

```bash
podman unshare bash -c '
  id
  for d in /dev/kfd /dev/dri/renderD129; do
    ( exec 3<>"$d" ) 2>/dev/null && echo "$d: OK" || echo "$d: FAIL"
  done'
```

**B. 容器（keep-id，补充组被剥）**：

```bash
podman run --rm \
  --userns=keep-id \
  --device /dev/kfd --device /dev/dri \
  docker.io/library/ubuntu:24.04 \
  bash -c '
    id
    for d in /dev/kfd /dev/dri/renderD129; do
      ( exec 3<>"$d" ) 2>/dev/null && echo "$d: OK" || echo "$d: FAIL"
    done'
```

**判定**：

- A 里 kfd `OK` 而 B 里 kfd `FAIL` → **DAC 层**：rootless keep-id 剥掉了宿主
  render 补充组，而 kfd 无当前用户 ACL（§4）。走 §8.3 落解法 2（宿主侧放行，
  rootless 即可）。解法 1（容器用户组）rootless 下不可行（§6/§8.4）。
- A 里 kfd 就 `FAIL` → 当前用户不在 render 组（宿主侧 DAC 问题），先
  `usermod -aG render $USER` 重登。
- B 里 kfd `OK` → 这台机器 rootless 可用 kfd（可能当前用户有 kfd 的 ACL，
  或运行时版本不剥补充组），直接进 §8.5 跑 ROCm。

### 8.3 落解法 2（推荐）：宿主侧放行后跑 ROCm

```bash
# 2b(推荐, 只授权当前用户, 与 renderD 一致):
sudo setfacl -m u:$(id -un):rw /dev/kfd
# 或 2a(最省事, 全局):
sudo chmod 666 /dev/kfd
# 持久化见 §6 解法 2 的 udev rule

podman run --rm \
  --userns=keep-id \
  --device /dev/kfd --device /dev/dri \
  rocm/dev-ubuntu-22.04:latest \
  bash -c 'rocminfo | head -40; echo ---; rocm-smi'
```

### 8.4 落解法 1（仅 rootful）：自定义 gidmap 把 render 映进容器

> **rootless 下不可行**（§6 解法 1 实测：runc 1.3.4 rootless 下手动 `--uidmap`
> 触发 `mapping tool not present` → EOF，`newuidmap` 形状限制又映射不进 991）。
> 以下命令**只在 rootful podman**（`sudo podman`）下成立——root 容器无 subuid
> 范围限制、无 mapping tool 限制、不剥补充组。

```bash
sudo podman run --rm -it \
  --uidmap 0:1000:1 --uidmap 1:100000:65536 \
  --gidmap 0:1000:1 --gidmap 991:991:1 --gidmap 1000:100000:65536 \
  --group-add render \
  --device /dev/kfd --device /dev/dri \
  rocm/dev-ubuntu-22.04:latest \
  bash -c 'id; rocm-smi'
```

### 8.5 跑 ROCm（kfd 已可用时）

```bash
podman run --rm \
  --userns=keep-id \
  --device /dev/kfd --device /dev/dri \
  rocm/dev-ubuntu-22.04:latest \
  bash -c 'rocminfo | head -40; echo ---; rocm-smi'
```

`rocminfo` 能列出 GPU（name/chip/驱动版本）= 完整 ROCm 栈通了。若 kfd 可用但
仍报 HSA 错误，通常是 `/dev/dri` 没给全（card 节点缺失）——把 8.1 记下的 AMD
card 号显式补上：`--device /dev/dri/cardN`。

### 8.6 只想用渲染（Mesa/VAAPI，不需要 ROCm）

kfd 打不通不影响这条。只需 renderD：

```bash
podman run --rm -it \
  --userns=keep-id \
  --device /dev/dri/renderD129 \
  <含 mesa 的镜像> \
  bash -c 'vulkaninfo --summary 2>/dev/null | grep -i device'
```

### 8.7 一句话速判（不想跑全套时）

```bash
# rootless 宿主上, 先看 kfd 有没有当前用户 ACL:
getfacl -p /dev/kfd | grep -q "user:$(id -un):" \
  && echo "kfd 有 \$USER 的 ACL, rootless 大概率可" \
  || echo "kfd 无 \$USER ACL: rootless keep-id 下默认 DENIED (走 §8.3 setfacl/chmod 666 宿主放行)"
```
