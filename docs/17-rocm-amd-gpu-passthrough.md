# 17. AMD GPU 透传与 ROCm 容器内可用性（rootless 实测）

> 2026-09-05 实测沉淀（多轮测试；结论更新：**宿主侧 `chmod 666 /dev/kfd` 是
> 最有效的解法**）。环境：rootless podman 5.4.2 / runc 1.3.4 / cgroup v2 /
> 双显卡宿主（NVIDIA card1 0x10de + AMD card2 0x1002）。
>
> **勘误**：早期版本把根因归为「rootless cgroup v2 eBPF 设备过滤器」。实测
> errno 是 `EACCES`（DAC）而非 `EPERM`（eBPF/capability），且同权限的
> renderD129 能开、kfd 不能，唯一差异是 ACL 里有没有 `user:div:rw-`——eBPF
> 归因已证伪。

## 1. 结论

| 目标                                              | rootless                  | 说明                                             |
| ------------------------------------------------- | ------------------------- | ------------------------------------------------ |
| `/dev/dri/renderD*`（Mesa / VAAPI / OpenGL / 硬解） | ✅ 可用                  | easytidy `gpu_amd: true` 透传后即可用           |
| `/dev/kfd`（ROCm / HIP 的**唯一入口**）           | ❌ 默认不可用             | DAC 拒（根因见 §2）                              |
| 完整 ROCm 栈（`rocminfo` / `rocm-smi` / HIP）      | ✅ 宿主 `chmod 666` 后    | **推荐、多轮实测最有效**，和 NVIDIA 设备同理（§3） |
| 同上（多用户宿主变体）                            | ✅ 宿主 `setfacl` 后      | 只授权当前用户，比 666 安全，与 renderD 现状一致（§3） |

**一句话**：rootless 容器里 AMD「渲染」默认可用，「计算」（ROCm）默认打不通。
根因在 **DAC 层**：`/dev/kfd` 是 `0660 root:render` 且**没有**当前用户的 ACL，
而 rootless keep-id 剥掉宿主 render 补充组（GID 991 不在 gidmap 范围内）→
容器进程非 owner、非 render 组、又无 ACL → 拒（`EACCES`）。解法是**宿主侧
放行**：`sudo chmod 666 /dev/kfd`（§3）。

## 2. 根因：DAC（ACL + 补充组剥除），不是 eBPF

### 2.1 宿主侧事实（实测）

```
/dev/kfd                  crw-rw----  root render  508, 0    ← ROCm 入口 (0660, 无 div ACL)
/dev/dri/renderD129       crw-rw----+ root render  226,129   ← AMD 渲染 (0660, 有 user:div:rw- ACL)
/dev/dri/card2            crw-rw----+ root video   226, 2    ← AMD card (0660, 有 user:div:rw- ACL)
/dev/nvidia*              crw-rw-rw-  root root    ...       ← NVIDIA (0666 全局可访问)
render 组 GID = 991（宿主 div 在组内）；div subuid/subgid = 100000:65536
```

ACL 是关键差异：kfd 只有 owner/group/other 条目；renderD129 / card2 多一条
`user:div:rw-`。宿主 `div`（render 组）直接 open kfd 无碍；NVIDIA 是 `0666`
全局可访问所以 rootless 下「能」——**与组/ACL 无关**（解释「为什么 NVIDIA 能、
AMD kfd 不能」）。

### 2.2 决定性证据

1. **errno 是 `EACCES` 不是 `EPERM`**：eBPF 设备过滤器 / capability 拦截报
   `Operation not permitted`；容器内 open kfd 实测报 `Permission denied` →
   DAC 层。
2. **同权限设备一开一拒**：容器内 kfd 与 renderD129 都是 `0660 65534:65534`、
   进程身份 uid/gid 1000。若是设备号白名单应一致裁决，实测 kfd 拒、renderD129
   放——唯一差异是 `user:div:rw-` ACL → 裁决依据是 DAC（owner/group/ACL）。
3. **render 组被剥除**：容器 PID 1 的宿主侧身份实测 `Groups: 1000`（只有主组，
   render 991 没了）。keep-id 的 gid_map `0→100000 / 1000→1000 / 1001→101000`
   不覆盖 991，rootless 重建 userns 凭证时把补充组剥除 → 容器进程对 kfd 的
   三项 DAC 检查（owner/group/ACL）全不过。

**决定性对照 A vs B**：`podman unshare`（保留宿主完整凭证含 render 组）能开
kfd；keep-id 容器（补充组被剥）被拒——同设备、同用户，唯一差异是「render 补充
组是否保留」。

### 2.3 试过且无效的手段（本环境实测，均失败）

| 手段                                       | 结果          | 原因                                                        |
| ------------------------------------------ | ------------- | ----------------------------------------------------------- |
| `--group-add keep-groups`                  | ❌            | rootless runc 宿主补充组不透传进 userns（宿主侧 Groups 仍 1000） |
| `--group-add 991`                          | ❌            | 容器 gid 991 映射到宿主 100991（keep-id 段 1），非 render 991 |
| `--security-opt seccomp=unconfined`        | ❌            | seccomp 非拦截层（errno 是 EACCES 非 EPERM）                |
| `--privileged`                             | ❌            | 非 capability 问题                                          |
| 自定义 `--uidmap`/`--gidmap` 把 991 映进容器 | ❌ rootless   | runc 1.3.4 rootless 的 mapping tool 失败（`mapping tool not present` → EOF）；`newuidmap` 形状只允许「用户自身 uid + 全量 subuid 连续段」，991 插不进。**仅 rootful 可行**（本环境无免密 sudo，不作产品路径） |

> 「网传方案」（`--userns=keep-id --group-add keep-groups --device /dev/kfd
> --device /dev/dri --security-opt seccomp=unconfined`）在本环境逐参数实测均
> 无效，其归因（「组不匹配 + cgroup 白名单 + seccomp」）已被证伪。

## 3. 解法：宿主侧放行（推荐 `chmod 666`）

容器进程的宿主身份是**当前登录用户**（keep-id 下与宿主锁死）；宿主用户能开
kfd，把宿主 DAC 授权对齐即可——容器侧零改动。

### 3.1 `chmod 666 /dev/kfd`（推荐，多轮实测最有效）

和 NVIDIA 设备（`0666 root:root`）同理——任何进程都能开，不再依赖 render 组
或 ACL：

```bash
sudo chmod 666 /dev/kfd
# 持久化（重启后设备节点被 udev 重建、权限复位）：
# /etc/udev/rules.d/99-kfd.rules
# KERNEL=="kfd", MODE="0666"
```

- **实测结论（2026-09-05）**：与 setfacl、显式映射多轮对照，此法落地最快、
  效果最稳，定为推荐。
- **安全**：kfd 是 GPU compute 入口，0666 后本机**任何用户**都能提交 GPU
  任务。rootless 单用户宿主可接受；多用户共享宿主用 §3.2。

### 3.2 `setfacl` 给当前用户加 ACL（多用户宿主变体）

`/dev/dri/renderD*` 之所以 rootless 下能开，正是 udev 给当前用户加了
`user:div:rw-` ACL；kfd 缺的就是这一条：

```bash
sudo setfacl -m u:$(id -un):rw /dev/kfd
# 持久化（udev 无直接设 ACL 的键，RUN 调 setfacl）：
# /etc/udev/rules.d/99-kfd-acl.rules
# KERNEL=="kfd", RUN+="/usr/bin/setfacl -m u:%E{SUDO_USER}:rw /dev/kfd"
```

只授权当前用户，比 666 安全，行为与 renderD 一致。

### 3.3 共同点

- easytidy `gpu_amd: true` 配置**无需任何改动**，rootless / rootful 通吃；
  两者都需宿主 root 执行 + udev rule 持久化。
- 容器侧「配置容器用户组」（显式映射）rootless 下不可行（§2.3），配置层无需
  考虑。

## 4. easytidy 的落地语义

- AMD 透传开关 = `gpu_amd: true`（存意图，创建期探测宿主裸设备注入；
  **不落盘设备路径**——render 节点号随重启漂移）。
- `extra_opts` 只支持 `apparmor=` / `label=` / `seccomp=` 三类 `key=value`
  （`keep_id_create_body` 解析）；原始 flag（`--group-add` 等）**静默丢弃**——
  对 kfd 也无济于事（§2.3）。
- **easytidy 边界**：只做设备透传 + 创建期预检——kfd 存在但当前用户被 DAC 拒
  时 `tracing::warn!` 提示修复命令（`sudo chmod 666 /dev/kfd`），不阻断创建
  （renderD 仍可用，无害）、不代执行宿主 root 命令。
- **配置一次，多种宿主都正确**：未落 §3 的 rootless 宿主 kfd 默认打不开但无
  害、renderD 可用；落地后 ROCm 完整可用。无需区分宿主。

## 5. 验证

容器内验证脚本（无 python3 的最小镜像同样可跑）：

```bash
podman exec <c> bash -c '
  for d in /dev/kfd /dev/dri/renderD129; do
    if (exec 3<>"$d") 2>/dev/null; then echo "$d: OK"; else echo "$d: DENIED"; fi
  done'
# 未落 §3：kfd: DENIED / renderD129: OK；落地后：kfd: OK
```

宿主侧 30 秒速判（「这台机器的 rootless 能不能用 ROCm 计算」）：

```bash
# 1) kfd 有没有当前用户 ACL（没有 → rootless keep-id 默认 DENIED）
getfacl -p /dev/kfd | grep -q "user:$(id -un):" \
  && echo "kfd 有 $USER ACL, 大概率可" \
  || echo "kfd 无 $USER ACL: rootless keep-id 默认 DENIED (宿主侧放行见 §3)"
# 2) 当前用户是否在 render 组（宿主直开 kfd 的前提）
id -nG | grep -q render || echo "不在 render 组: usermod -aG render $USER 后重登"
# 3) 修复后跑 ROCm 验证
podman run --rm --userns=keep-id \
  --device /dev/kfd --device /dev/dri \
  rocm/dev-ubuntu-22.04:latest \
  bash -c 'rocminfo | head -40; echo ---; rocm-smi'
# rocminfo 列出 GPU = 完整栈通；若 HSA 错误，补 AMD card 节点: --device /dev/dri/cardN
```
