# 01. DistroBox 代码库架构图谱

> 基于对 `/workspaces/easy-tidy/distrobox`（DistroBox v2 Go 移植，Go 1.25.3，module `github.com/89luca89/distrobox`）的直接分析。依赖：`urfave/cli/v3 v3.10.0`、`gopkg.in/ini.v1`、`testify`。单二进制，无 GUI/daemon/IPC，无长驻进程。

## 1. 顶层布局

| 路径 | 内容 |
|---|---|
| `cmd/distrobox/` | 唯一 `main` 包，入口 `main.go` |
| `internal/cli/` | urfave/cli v3 命令定义（标志解析 + 装配），薄翻译层转 `pkg/commands` |
| `internal/inside-distrobox/` | **嵌入的 shell 脚本**（`assets/`）：`distrobox-init`(2912行)、`distrobox-export`(710行)、`distrobox-host-exec`(234行)；`scripts.go` 负责部署到宿主机 |
| `internal/rootful/` | sudo 提权校验（`sudo -v` 每进程一次，记忆化） |
| `internal/userenv/` | 宿主用户发现：env → `getent` → `os/user` |
| `pkg/commands/` | 业务逻辑：Create/Enter/List/Rm/Stop/Upgrade/Ephemeral/Assemble/GenerateEntry |
| `pkg/containermanager/` | `ContainerManager` 接口 + 选项结构；`providers/` 有 podman、podman-launcher、docker、autodetect |
| `pkg/config/` | INI + env 配置解析（XDG 路径、`DBX_*` 环境变量） |
| `pkg/manifest/` | assemble 清单（.ini）解析器（支持 `include=`） |
| `pkg/ui/` | `Printer`/`Progress`/`Prompter`/ANSI 颜色（全部 `io.Writer` 注入） |
| `pkg/version/` | 构建期版本变量（`-ldflags -X`） |
| `pkg/internal/testutil/` | `MockContainerManager` + spy |
| `extras/` | 辅助脚本：`docker-host`、`podman-host`、`install-podman`、`vscode-distrobox`、示例清单 |
| `completions/` | bash + zsh 补全（手写） |
| `man/` | `man/man1/*.1` 生成页 + `man/gen-man`（pandoc） |
| `docs/` | Jekyll 站点 + `docs/usage/`（man 页源） + `docs/compatibility.md`（运行时网络拉取） |
| `icons/` | 终端图标 SVG + hicolor PNG |
| `install`/`uninstall` | POSIX shell 安装器 |

## 2. 命令入口与 CLI 装配

- **入口**：`cmd/distrobox/main.go:14-35` — `run()` 载入 `config.LoadValues()`，设 SIGINT/SIGTERM context，`cli.NewRootCommand(cfg)` + `cmd.Run(ctx, cli.ResolveArgs(os.Args))`（主分发 `main.go:31-34`）
- **v1 风格符号链接分发**：`internal/cli/root.go:35-48` `ResolveArgs` — argv[0] 以 `distrobox-` 开头时改写为 `distrobox <sub> ...`（Makefile 创建符号链接）
- **命令树**：`root.go:106-182`，9 个命令，经 `CommandComposer`（`root.go:358-372`）叠加中间件：`withSudoGuard`（拒绝 sudo 调用）、`withRoot`（`--root` 声明 + 校验）、`withContainerManager`（Before 钩子构建管理器，存 context `containerManagerKey`）
- **Provider 选择**：`root.go:326-356` — docker / podman / podman-launcher / autodetect（优先级 podman > podman-launcher > docker）
- **命令定义**：`internal/cli/assemble.go`、`create.go`、`enter.go`、`ephemeral.go`、`generate-entry.go`、`list.go`(别名 ls)、`rm.go`、`stop.go`、`upgrade.go`；每个 Action 从 context 取管理器 → 映射选项 → `commands.NewXCommand(...).Execute(ctx, opts)`
- **CLI 特殊机制**：`enter`/`ephemeral` 用 `StopOnNthArg: 1` + `SkipFlagParsing`（容器名后全部为自定义命令）；`-e/--exec` 标记靠 `findExecMarkerIndex`（`parse.go:10-17`）从位置参数尾部找回——**urfave/cli 的解析缺陷补丁，反映了 CLI 天生不适合做 GUI 传输层**

## 3. 引擎：容器管理器调用

- **接口**：`pkg/containermanager/containermanager.go:137-152` — `Enter`/`ListContainers`/`Create`/`Remove`/`Exists`/`ImageExists`/`Stop`/`InspectContainer`/`PullImage`/`Commit`/`CloneAsRoot`
- **podman 执行**：`providers/podman.go:474-519` `Podman.run` — `exec.CommandContext`；rootful 包 sudo（`podman.go:476-479`）；dry-run 打印命令（`:481-485`）；交互模式直连 stdin/stdout/stderr（`:489-499`）。docker 孪生：`docker.go:461-506`
- **命令生成是纯 Go `[]string`**（宿主机侧无 shell 生成）：`makeCreateCommand`（`podman.go:143-466`，约 320 行选项追加）；`docker.go:143-453` 近似平行拷贝；`generateEnterCommand`（`podman.go:809-903`，exec 参数：`--user`、`--workdir`、PATH/XDG env 透传 `FilterEnvVars`）
- **shell 脚本在容器内部运行**（`go:embed` 于 `internal/inside-distrobox/scripts.go:11-18`，`ProvisionScripts` 部署到 `~/.local/bin` 等目录，`scripts.go:22-90`，bind-mount 为容器入口/辅助二进制）

## 4. 宿主机集成逻辑——分散在三层三个语言（最大重构风险）

**A. 创建期挂载（Go，宿主机侧）** `podman.go:200-434`：
- Home bind：`--volume $HOME:$HOME`（`podman.go:233`）；自定义 home（`:326-334`）；ostree `/var/home`（`:338-341`）
- `/run/host` 整根挂载（`:244`；runc 按目录 `:241-242`，`hostRootMountsForRunc` `:712-743`）
- `/dev`、`/sys`、`/dev/pts`、`/tmp`、journal、selinux、`/dev/shm`：`:226-320`
- **XDG_RUNTIME_DIR**（Wayland/dbus/pipewire socket 目录）仅无 init 容器挂载：`:346-349`
- **无显式 GPU 标志**：`--privileged`（`:182`）+ `/dev:/dev` 挂载涵盖 `/dev/dri` 等
- 标签：`--label manager=distrobox`、`--label distrobox.unshare_groups=N`（`:211-216`）
- init：podman `--systemd=always`（`:409-411`）；docker 对应 tmpfs/cgroupns 标志（`docker.go:254-262`）

**B. 首启初始化（shell，容器内）** `assets/distrobox-init`：
- 静态宿主挂载（`/etc/host.conf`、`/run/libvirt`、`/media`…）：`HOST_MOUNTS*` 变量（`:35-66`），应用点 `:1924-1959`（`mount_bind()` `:346`）
- **Socket 集成**（dbus 等）：`:1962-1997` — `find /run/host/run ... -type s` 逐个符号链接进容器
- **NVIDIA/GPU**：`:2000-2180` — 绑定 nvidia 配置、二进制、`lib*.so*`（含 glvnd/vulkan ICD json）
- 主题/图标/字体：`:2184-2200`
- **X11/Wayland 环境**：写 `/etc/profile.d/distrobox_profile.sh`（`:2202-2267`）+ fish 等价物（`:2270-2319`）——经 `host-spawn` 从宿主机拉取并导出 `XAUTHORITY`/`WAYLAND_DISPLAY`/`DISPLAY`（`:2233-2249`）；X11 本身靠 `/tmp:/tmp` + `--ipc host`/`--network host`/`--pid host`（`podman.go:188-198`）+ XAUTHORITY 共享
- devpts `:1903`；sudo `:2323`；用户创建/组 `:2388-2563`；skel `:2639`；init hooks `:2668`；**`container_setup_done` 哨兵输出 `:2696`**（宿主机轮询它）

**C. 进入期（Go，宿主机侧）**：env 过滤 `FilterEnvVars`（`containermanager.go:253-311`）、PATH 合并 `BuildContainerPath`（`:174-236`）、XDG 目录 `BuildXDGPaths`（`:238-251`）、workdir 映射 `GetWorkDir`（`:325-348`，宿主 cwd → `/run/host<path>`）

## 5. 创建流程（`distrobox create`）

1. `internal/cli/create.go:19-184` 定义标志（image/name/home/volumes/unshare-*/init/nvidia/hooks…）；`createAction`（`:186-276`）映射为 `commands.CreateOptions`（含 `--unshare-all`/`--init` 交互 `:208-212`、位置参数名覆盖 `:247-249`）
2. `pkg/commands/create.go:101-182` `CreateCommand.Execute`：`makeContainerImage`/`makeContainerName`/`makeContainerHostname`/`makeContainerUserCustomHome`（`:192-271`）；重名检查 `Exists`（`:111-113`）；clone 走 `Commit`（`:273-291`）；`askPullImage` 提示（`:305-323`）；`containerManager.Create`（`:129-153`）；创建后生成桌面入口 `generateEntryCmd.Execute`（`:162-175`）
3. Provider `Create`（`podman.go:87-138`）：加载用户 env → 部署脚本 → 建自定义 home → `makeCreateCommand` + `p.run`；入口为宿主机 `distrobox-init` bind-mount 于 `/usr/bin/entrypoint`（`:433-434`），参数追加 name/uid/gid/home/init/nvidia/hooks（`:443-454`）
4. **真正初始化发生在首次启动**：`distrobox-init` 作为 PID 1 在容器内运行，安装包、建用户/挂载、打印 `container_setup_done`（`distrobox-init:2696`）

## 6. 进入/运行流程（宿主 → 容器）

1. `internal/cli/enter.go:79-158`：解析名字+自定义命令（`:92-116`）；**缺容器时提示自动创建** `offerCreateMissing`（`:163-198`）
2. `pkg/commands/enter.go:46-64` — 薄透传至 `containerManager.Enter`
3. `podman.go:521-575` `Podman.Enter`：`generateEnterCommand`（`podman.go:809-903`）构建 `podman exec --interactive [--tty] --user=$USER --workdir ... --env=... <name>`；容器信息来自 `InspectContainer`（`podman.go:772-807`，读 HOME/PATH env + `distrobox.unshare_groups` 标签）；默认命令为用户登录 shell（`getent passwd`）或 unshare_groups 下的 `su` 包装（`BuildCommandArgs` `containermanager.go:350-369`）
4. **启动/设置握手**：未运行则 `startContainer`（`podman.go:905-938`，`podman start` 交互式；建 `$XDG_CACHE_HOME/distrobox`）；然后 `waitForSetup`（`podman.go:940-1006`）**每 500ms 轮询 `podman logs --since`**，找 `distrobox:` 进度行、`Error:` 失败、`container_setup_done` 哨兵
5. 真正的 `podman exec` 交互式执行（`podman.go:570`），stdio 继承替换 distrobox 进程——应用看起来"原生"运行
6. **"运行 GUI 应用" = `distrobox enter <name> -- <app>`**。产品面 = `distrobox-export`（容器内）：`--app`/`--bin` 写宿主 `.desktop`/包装脚本，`Exec=` 为 `distrobox enter <name> <cmd>`（`distrobox-export:359-428`；模板 `pkg/commands/assets/desktop_entry.toml.tmpl:1-17`）；rm 时清理（`pkg/commands/rm.go:220-337`，按 `# distrobox_binary` + `# name:` 标记匹配）
7. `distrobox-host-exec`（容器内资产）经 `host-spawn`/podman socket 在宿主执行命令——profile 的 X/Wayland env 发现就靠它

## 7. 状态/配置存储

- **无自有容器状态库**——容器管理器即真相源：`podman ps -a --no-trunc --format json`（`podman.go:78-85`）/`docker ps`（`docker.go:80-87`）；按 `Container.IsDistrobox`（`containermanager.go:117-127`）过滤：`manager=distrobox` 标签**或任何 `distrobox.` 前缀标签**（修复 `94d62f5`）；按名字排序（`pkg/commands/list.go:42-45`）
- **标签即标注**：创建时 `manager=distrobox` + `distrobox.unshare_groups=<0|1>`（`podman.go:211-216`）；进入时读回（`podman.go:793-795`）。`InspectResult`（`containermanager.go:56-62`）携带 home/path/status/unshare-groups
- **自有配置**（`pkg/config/config.go`）：INI 文件优先级 `config.go:116-152`（`/etc/distrobox/distrobox.conf` → `$XDG_CONFIG_HOME/distrobox/distrobox.conf` → `~/.distroboxrc` 等），叠加 `DBX_*` env（`:190-243`）；缺省值 `:36-52`。优先级：缺省 < 文件 < env
- **缓存/状态目录**：`$XDG_CACHE_HOME/distrobox`（compat 列表 `internal/cli/compatibility.go:177-187`、脚本缓存 `podman.go:926-937`）；`~/.local/bin`（导出二进制）；`~/.local/share/applications` + icons（桌面入口与下载的发行版 logo，`generate_entry.go:202-213`、`:289-311`）

## 8. 测试基建

- **27 个测试文件**（7 internal / 20 pkg），约 160 个测试函数；`package cli_test` 外部 + `_internal_test.go` 内部
- 覆盖：CLI 参数解析（`enter_internal_test.go` 测 `-e` 标记、`compatibility_internal_test.go` 43 测 markdown 解析）；config 合并/env；**命令编排经 `MockContainerManager` + `ContainerManagerSpy`**（`pkg/internal/testutil/mock_containermanager.go:41-79`）；provider 参数构建单测（`podman_internal_test.go` 18 测、`docker_internal_test.go` 7 测等）；manifest 解析 13 测；ui 提示 9 测；嵌入脚本健全性 3 测；userenv 7 测
- 无真实启动 podman 的集成测试。`make test` = vet + `go test -v ./...`；`make lint` = golangci-lint（严格配置 `.golangci.yml` 22KB）

## 9. 版本与身份

- 版本：`pkg/version/version.go:4` `var Version = "dev"`，`Makefile:4` 经 `-ldflags -X github.com/89luca89/distrobox/pkg/version.Version=$(VERSION)` 注入（`git describe --tags --always`）
- **身份字符串全库硬编码**：二进制名、标签（`manager=distrobox`）、`distrobox.` 标签前缀、`distrobox-*` 脚本、`~/.distroboxrc`、`distrobox/` 配置/缓存目录、`CONTAINER_ID` env、`distrobox_profile.sh`、桌面入口 `Exec` 路径、上游 URL（`distro_icons.go:7`、`compatibility.go:23`）
- 嵌入脚本硬编码 `version="1.8.2.5"`（`distrobox-init:77` 等）
- ⚠️ **`manager=distrobox` 标签是与既有容器的互操作契约**：分叉沿用则继承所有用户老容器；更换则孤儿化

## 10. 许可证

- `COPYING.md` = **GPL-3.0**（GPLv3 全文）
- **Go 源文件无许可证头**（仅嵌入 shell 脚本与顶层安装器带 GPL-3.0-only SPDX 头）；无 NOTICE 文件
- 分叉必须保持 GPL-3.0 分发

## 11. 重构面分析

**已库化（原样可导入，零 CLI 依赖）：**
- `pkg/containermanager` + `providers/` — 唯一外部耦合是 dry-run 的 `fmt.Println`（`podman.go:483/549`）与 `ui.*`/`userenv`/`insidedistrobox` 导入；`Enter` 需 `Progress`/`Printer`（可用 `ui.NewDevNullProgress` mock）。**这是要提取的核心引擎**
- `pkg/config`、`internal/userenv`、`internal/rootful`、`internal/inside-distrobox`（脚本部署）、`pkg/manifest`、`pkg/ui`

**需中度手术：**
- `pkg/commands/*` — 每命令已注入 `cfg, containerManager, progress/printer/prompter`（刻意可测设计）；但交互式命令（Enter/Upgrade 经 `podman exec -it` 继承 stdio）是进程形态——GUI 消费者需要非 TTY exec API（provider `run()` 已支持 `runOptions{Interactive:false}`，`podman.go:501-518`，**但 `ContainerManager.Enter` 硬编码 `Interactive: true` 于 `podman.go:570`**）
- `GenerateEntryCommand` — 可直接复用，但模板写死 `Exec=<distrobox> enter ...`，容器内 `distrobox-export` 侧须改指新产品

**紧耦合 CLI（保持 CLI-only 或重写）：**
- `internal/cli/*`（urfave/cli、os.Args、stdin/out）— context 存管理器的小技巧在库 API 中应改为显式构造参数
- `internal/cli/compatibility.go` — 拉取上游 GitHub markdown（硬编码 URL，`compatibility.go:23`），须改指或删除
- `cmd/distrobox/main.go` — 改名时 go.mod module 路径 + 全库 import 路径须重写
- 身份字符串（标签/目录/env/URL）遍布全库，是最早要决策的兼容性契约

**最大重构风险：** 宿主机集成逻辑分散三层三语言（创建期 Go 挂载 `podman.go:200-434` + init 脚本挂载 `distrobox-init:1903-2319` + 进入期 env `containermanager.go:253-311`、`podman.go:809-903`）。GUI 沙盒产品需要声明式"集成档案"（X11/Wayland/GPU/音频/dbus）在创建期求值——即把 init 脚本挂载段移入 Go，init 脚本缩为纯用户设置。podman/docker 参数构建器重复（`podman.go:143-466` vs `docker.go:143-453`）是合并首选，即最高杠杆重构目标。
