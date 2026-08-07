# 12. 容器用户权限模型（distrobox 式用户一致性 + keep-id）

> 2026-08-06 ~ 08-07 实测沉淀。涉及：为什么默认用户非 root、rootless userns 现实、
> keep-id 的真实映射语义（实证）、最终方案与实现踩坑。

## 1. 为什么默认用户必须是普通用户（非 root）

浏览器类软件（Google Chrome 等）会检测当前执行用户：**若为 root 则自动掉沙盒**
（--no-sandbox 语义），随后被网站按"机器人"处理；部分软件直接拒绝运行。

因此：**容器内应用默认以普通用户运行**（容器内 `node`，uid/gid 与宿主当前
登录用户对齐），root 仅用于装包/管理场景（经免密 sudo 或显式 `--root`）。

## 2. rootless podman 的 userns 现实（背景）

rootless podman 下每个容器运行在自己的 user namespace 中。默认映射：

```
容器 uid 0        → 宿主 1000（调用者，即当前登录用户）
容器 uid 1..65536 → 宿主 100000..165536（/etc/subuid 子映射）
```

推论（均经实测证实）：

| 容器内身份 | 宿主身份 | 能力 |
|---|---|---|
| root（0） | 宿主当前登录用户 | 写宿主 home、访问显示 socket（=宿主用户权限） |
| 普通用户（如 uid 1000） | 宿主 100000+（subuid） | **无法**访问宿主 home（0700 属主 1000）与 /run/user/1000 |

**结论**：默认 rootless 映射下，"容器内 uid 与宿主 uid 对齐"不成立——
容器 uid 1000 ≠ 宿主 uid 1000。这是"uid 对齐"方案失败的根因。

## 3. 三个方案的实测对比

| 方案 | 容器 uid 1000 = 宿主 1000 | 装包（apt/dpkg） | GUI 窗口 | 结论 |
|---|---|---|---|---|
| 默认 rootless + 建 uid-1000 用户 | ❌（映射 subuid 101000） | ✅ | ❌（读不了 /run/user/1000） | 失败 |
| keep-id + 用户 1000 | ✅ | ❌（容器 root 写不了镜像 root 文件） | ✅ | 装包挂 |
| keep-id + server 以 uid 0 + 应用 su node | ✅ | ✅（server uid 0 = 容器层文件属主，宿主侧 subuid 100000） | ✅ | **采用** |

## 4. keep-id 的真实映射语义（实证）

`--userns=keep-id` 的 /proc/self/uid_map 字面：

```
0     1     1000     容器 0..999   → 宿主 1..1000
1000  0     1        容器 1000    → 宿主 0
1001  1001  64536    容器 1001+   → 宿主 1001+
```

**但字面映射 ≠ 实际身份**（实测文件属主法，2026-08-07）：

- **容器内 uid 1000（node 用户）写宿主 home 的文件，宿主侧属主 = 当前登录
  用户（1000）**——即容器 uid 1000 的真实身份就是宿主登录用户
- node 可直接写宿主 home（无需 sudo）、可读宿主 /run/user/1000（显示 socket）
- 容器 root（uid 0）是**装包身份**：能写容器系统文件（apt/sudo 可用）；
  对宿主 home 的写文件以 subuid(100000) 属主呈现

> 教训：**判断 userns 身份以实际文件属主行为为准，不要只看 /proc/self/uid_map**
> （keep-id 层有额外身份处理，map 表不反映最终文件属主）。

## 5. 最终方案

### 创建（keep-id 容器，经 libpod 端点）

```
userns: keep-id（容器 uid 1000 = 宿主登录用户）
user:   "0:0"（容器进程以 uid 0 运行——server 才有装包权）
mounts: $HOME→$HOME（rw）、/tmp/.X11-unix（ro）、$XDG_RUNTIME_DIR、
        /usr/share/easytidy-host/{fonts,icons}（ro，非覆盖容器自身目录）、
        server 二进制（ro）、socket 目录（rw）
env:    DISPLAY/WAYLAND_DISPLAY/XAUTHORITY/XDG_RUNTIME_DIR（宿主值）、
        EASYTIDY_USER_UID/GID
network: host（默认，端口映射可选 mapped）
```

### 运行（server 启动时）

1. server 以容器内 uid 0 运行（容器层文件属主，宿主侧 subuid 100000；**不是宿主
   默认用户 1000**）→ 装包/setup 可用
2. 用户映射 setup：创建容器内用户 `node`（uid/gid = 宿主值，名字不同）；
   ubuntu 镜像 uid 1000 默认用户经 usermod 重命名对齐
3. 免密 sudo：写 `/etc/sudoers.d/easytidy-node`（`NOPASSWD: ALL`），
   **先 `create_dir_all /etc/sudoers.d`**（基础镜像装 sudo 前无此目录，实测）
4. 宿主字体接入 fontconfig：写 `/etc/fonts/local.conf` 指向
   `/usr/share/easytidy-host/{fonts,.local/share/fonts}`
5. 应用层：PTY/entry 经 `su node` 以容器用户运行（chrome 等沙盒正常）

### 身份矩阵（最终，实测）

| 场景 | 容器内身份 | 宿主身份 | 备注 |
|---|---|---|---|
| 应用默认 | node（uid 1000） | **宿主登录用户（1000）** | chrome 沙盒完整、宿主 home 直读写（属主 1000）、显示可用 |
| setup/装包 | root（uid 0） | **宿主 subuid 100000**（容器文件系统属主；⚠️ 不是宿主默认用户） | `--root` 或免密 sudo；能写容器系统文件；写宿主 home 属主呈现 100000 |
| 提升通道 | sudo → root | 宿主 subuid 100000 | 免密 |

> ⚠️ 2026-08-07 修正：曾误写"容器 root = 宿主登录用户"。准确语义：
> keep-id 下 **容器 uid 1000 = 宿主 1000**（文件属主实证），而 **容器 uid 0 =
> 宿主 subuid 100000**——容器 root 能装包是因为它正好是容器层文件（镜像+可写层，
> rootless 下宿主侧统一 subuid 属主）的属主，与宿主默认用户无关。

## 6. 实现要点与踩坑（libpod 端点）

- keep-id 仅在 **libpod 端点**可用（`POST /v<ApiVersion>/libpod/containers/create`），
  Docker compat 端点不支持；裸 `/libpod/...` 返回 404，需版本前缀
- **env 是 map**（`{"K":"V"}`），Docker compat 才是数组
- **mount 用 podman 格式**：`type/source/destination/options`；
  Docker 式 `Type/Source/Target/ReadOnly` 报 "container directory cannot be empty"
- **命令字段是 `command`**（Docker compat 是 `cmd`）
- **`userns` 放顶层**（`"userns":{"nsmode":"keep-id"}`），
  `namespaces.userns` 被忽略（实测）
- **`user:"0:0"`**：keep-id 下不设则进程默认为 uid 1000，无装包权
- 环境变量注入：`EASYTIDY_USER_UID/GID` 必须随 keep-id body 一起传
  （曾因传了 config.env 而非局部 env 导致 server 未收到，用户映射被跳过）
- HTTP 客户端：hyper-util legacy client + `tower_service::Service`（unix 连接器；
  hyper 1 的 `hyper::service::Service` 是封死 trait，外部不可实现）

## 7. 安全模型与边界

- node = 宿主登录用户权限（不是 root）→ 应用沙盒完整、权限模型干净
- root（容器 uid 0）= 装包身份，对宿主 home 写文件属主呈现 subuid——**尽量
  经 node 或 sudo 操作宿主文件**（root 直接写宿主 home 会留下 100000 属主的文件）
- 宿主 home 的**读写不需要 sudo**（node 即宿主用户）；sudo 是提升通道
- ⚠️ **绝不 chown 宿主挂载的 home**（2026-08-06 事故：改写宿主属主致宿主
  用户失权、宿主环境崩溃）——权限一致性靠 keep-id 映射，不动宿主属主
- 遗留：GUI 透传中的 GPU（--gpus=all + NVIDIA_* env）与 apparmor=unconfined
  属 P1（需宿主 nvidia-container-toolkit）

## 8. 相关命令

```bash
easytidy flavor apply gui --container <name>   # 创建 keep-id GUI 底座容器
easytidy run --container <name> -- <cmd>       # 以 node（宿主用户）运行
easytidy run --container <name> --root -- <cmd> # 以容器 root（装包）运行
easytidy pull --image <img>                    # 显式拉取镜像（create 不再自动拉）
```
