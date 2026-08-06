# 05. 参照实现解剖：Kontainer (Qt) / BoxBuddyRS (GTK4)

> 基于对 `/workspaces/easy-tidy/Kontainer`（Kontainer v1.6.1，GPL-3.0，Qt 6/C++/QML + Kirigami）与 `/tmp/BoxBuddyRS`（v2.5.8，MIT，Rust + GTK4/libadwaita）源码的直接分析。

## 架构

### Kontainer：三层清晰分离

```
src/main.cpp                      — 引导：创建 DistroboxManager，注册为 QML context 属性
src/core/distroboxcli.{h,cpp}     — 子进程执行 + 原始输出解析（distrobox 边界）
src/core/distroboxmanager.{h,cpp} — 功能 API：Q_INVOKABLE 槽，JSON 字符串 / QVariantList
src/core/terminallauncher.{h,cpp} — 终端应用发现 + 启动
src/core/packageinstallcommand.{h,cpp} — 镜像名 → 包管理器命令映射
src/qml/…                         — Kirigami UI
```

- 跨边界状态全是 JSON 字符串（`listContainers()` 返回 JSON，QML 侧 `JSON.parse`）
- **所有 distrobox 调用经单一函数**（`distroboxcli.cpp:27-49`）：

```cpp
QString runCommand(const QString &command, bool &success)
{
    QString actualCommand = u"/usr/bin/env "_s + command;
    if (isFlatpakRuntime()) actualCommand = u"flatpak-spawn --host /usr/bin/env "_s + command;
    ...
    QEventLoop loop;
    process.start(u"sh"_s, QStringList() << QLatin1String("-c") << actualCommand);
    loop.exec();   // ← 同步阻塞 UI 线程
    return output;
}
```

success 仅凭 `exitCode == 0`；stderr 整体丢弃；QML 用 100ms Timer 假异步（`Main.qml:69-83`）

### BoxBuddyRS：三文件，巨型单文件 UI

```
src/main.rs               — 1763 行：全部 UI 构建 + 回调
src/distrobox_handler.rs  — 750 行：所有 distrobox 交互 + 输出解析
src/utils.rs              — 732 行：进程助手、终端表、flatpak 检测
```

- 纯命令式 GTK4 控件构建（无 .ui/Blueprint）；每 box 一个 `Notebook` 标签页；每次刷新**整窗销毁重建**（`delayed_rerender` `main.rs:1324-1329`）
- 进程模型（`utils.rs:53-115`）：`std::process::Command` + flatpak 时 `flatpak-spawn --host` 前缀；`get_command_output` **stdout/stderr 拼接**（错误文本变成可解析"输出"）；spawn 失败返回字面量 `"fail"`
- **异步正确**：`gio::spawn_blocking` + `async_channel` + `glib::spawn_future_local`（UI 线程不阻塞）——但整个窗口先渲染后重建

## 命令清单与解析方式（对照）

| 操作 | Kontainer | BoxBuddyRS |
|---|---|---|
| 容器列表 | `distrobox list \| tail -n +2 \| cut -d'\|' -f2` 等**跑 3 次**（name/status/image 列）再按索引 zip 成 JSON（`distroboxcli.cpp:70-102`）；**列位硬编码**，丢 ID 列 | `distrobox list --no-color` 单次，**嗅探表头列索引**（`distrobox_handler.rs:53-106`）；`is_running = !status.contains("Exited") && !status.contains("Created")`（`:92`）字符串启发 |
| 可用镜像 | `distrobox create -C`（v2 Go 引擎 = **网络拉取 compatibility.md，15s 超时 + 磁盘缓存**——GUI 静默继承网络依赖） | `--compatibility` + `podman images` 合并；distro 前缀靠 URL 子串嗅探（27 个硬编码发行版名，含 "bazzite before arch" 排序注释 `:110-164`）；已下载镜像加 `✦` 后缀再 `.replace(" ✦ ", "")` |
| 创建 | `distrobox create --name %1 --image %2 --yes` + **自由参数未加引号拼接**（`distroboxmanager.cpp:273-284`，注入/引号风险） | `-n -i -Y` + 自动 `--nvidia`（**lspci 探测** `utils.rs:528-547`）+ `--init --additional-packages systemd` + `--home` + `--volume`×n（`distrobox_handler.rs:323-354`） |
| 进入 | `distrobox enter %1` 于终端 | 终端（flatpak×终端 4 分支矩阵 `:167-215`） |
| 运行应用 | 无 | `distrobox enter %1 -- <cmd>` 分离执行（`main.rs:1158-1160`） |
| 停止/启动/重启 | 三者全有（启动甚至直接 `podman start %1`） | **无 start 按钮**（`main.rs:388-390` 仅在 is_running 时渲染） |
| 升级 | 单容器 + 全部（终端内 `&& echo ... && read -s -n 1` 保活） | 单 + 全部；**升级后追加 `distrobox enter` 保持终端打开**（`:266-315`） |
| 应用列表 | `distrobox enter %1 -- sh -c '<find /usr/share/applications …>'` 后**每应用一次 `distrobox enter -- cat`（N+1）**，行前缀解析 Name/Icon/GenericName（`:576-660`） | 同样 N+1（grep NoDisplay + cat，`:396-467`）；解析 `Name=`/`Exec=`/`Icon=`，剥 `%F/%U` |
| 导出应用 | `distrobox-export --app`；**卸载 3 级回退级联**（basename→全路径→**手动删宿主 .desktop 文件**，`:811-923`） | `--app [--delete]` |
| 二进制导出 | export + unexport + 容器内列表（`--list-binaries` 行切分） | 仅列已导出 + 删除；无新导出 |
| 安装包文件 | 任意发行版（正则表 12 族，`packageinstallcommand.cpp:16-53`）；未知发行版 → 终端"请手动" | 仅 .deb/.rpm，点击时**重列所有容器**决定 rpm/zypper（`:615-617`，带 TODO） |
| 统计 | `podman stats --no-stream --format json`（**全程唯一结构化输出**），5s 轮询 | go-template `;` 切分（`utils.rs:467-495`），仅渲染时取一次 |
| 版本嗅探 | `--version` 末 token（兼容 v1/v2 两种格式） | 无 |

## 特性覆盖对比

- Kontainer 独有：start/reboot、generate-entry（单/全部）、任意创建参数透传、**创建命令实时预览**（`CreateDialog.qml:663-671`，用户好评）、镜像搜索 + 自定义镜像、KDE 配置终端发现
- BoxBuddy 独有：run app、文件关联安装（`HANDLES_OPEN`）、NVIDIA 自动探测、16 终端表 + GSettings
- 两者：rootful 均无 UI（Kontainer 仅靠自由参数洞；**BoxBuddy ROADMAP 明确拒绝**："too many password popups"）；无 ephemeral/host-exec/init-hooks；无日志/流式输出视图

## 痛点实证

1. **版本回归**：Kontainer #73/#76 — distrobox 2.0.0-rc.3 破坏应用列表（用户实测 1.8.2.5 正常）；修复 = stdout 噪音剥离 hack（`distroboxmanager.cpp:54-76`）
2. **输出污染**：distrobox 2.0 自动启动停止容器时向 stdout 打印容器名 → 所有解析器需剥噪音
3. **CLI 即 UI 心智错位**：BoxBuddy #163 用户装命令行工具后期待"查看应用"里出现它——`.desktop` 应用与已装二进制语义混淆，引擎应区分"可导出 GUI 应用"与"已安装二进制"
4. **错误即布尔**：Kontainer 通用错误弹窗无 stderr（`CreateDialog.qml:314-322`）；BoxBuddy `let _ =` 吞错 + 成功 toast 照弹（`distrobox_handler.rs:501-514`）
5. **终端嫁接**：`read -s -n 1` 保活、升级后追加 enter 保活、`bash -c` 硬编码链式命令、16 项终端表、konsole `--workdir`/xterm `-hold` 特例——全是"CLI 从未为无头设计"的症状
6. **引擎侧 CLI 缺陷**：`internal/cli/parse.go:1-17` 为 urfave/cli 无法正确解析 `distrobox enter <box> -e cmd` 而存在的补丁
7. **BoxBuddy ROADMAP 直言需要外援**：*"Stream output of `distrobox` commands to the GUI (particularly during creation)"*；"Parse assemble .ini files and show confirmation pop-up"；"Uninstall application from box"（现只有 `--delete`）

## 引擎结构化输出解锁清单（GUI 痛点 → 引擎能力）

1. **`list` → JSON**：灭 3×cut hack、表头嗅探、版本脆弱性、丢 ID 列、`status.contains("Exited")` 启发
2. **create/upgrade/rm 进度事件**（引擎状态机 + 事件流）：灭 1s 轮询创建、整窗重绘、`read -s -n 1` 与追加 enter 保活——即 BoxBuddy ROADMAP 所请
3. **应用清单 + 图标来自引擎**：灭 N+1 `cat` 循环与手写 .desktop 解析（含 Kontainer 的 base64 图标搬运 `distroboxmanager.cpp:206-221`）；修复 #163 语义混淆
4. **结构化导出清单**（含所属容器）：灭脚本正则解析（`distroboxmanager.cpp:956-963`）与"卸载时扫描是否其他容器共享"的文件系统手术（`:750-809`）
5. **错误对象**（code+message+stderr 尾部）：替换布尔 only 的一切
6. **引擎拥有镜像目录**（本地+远端、离线缓存、非网络）：替换 `create -C` 网络抓取与 `✦` 子串 hack
7. **引擎拥有终端/exec 层**（或显式非交互模式 + 流式）：灭 flatpak×终端 4 分支矩阵、16 终端表、flatpak-spawn 重复管道
8. **结构化 stats**：灭 `;` 切分 go-template 解析
9. **稳定 ID + 事件**（containerStarted/Exited/Created）：替换整窗重绘与 5s 盲轮询

## 启示（对 EasyTidy 产品）

1. **结构化引擎 = 全部重点**。两个 GUI 约 1/3 代码只用于解析人类输出，且这 1/3 正是 1.8→2.0 升级中坏掉的部分。引擎首个里程碑就交付 `list`/`create`(进度+结果)/`enter --non-interactive`/`export inventory`/`apps`/`stats` 的结构化输出
2. **抄 Kontainer 分层，不抄 BoxBuddy**：单一 Manager 对象 + 命令式方法 = 好 GUI API 形状；1763 行单文件 = 反面教材。**抄 BoxBuddy 异步**：`spawn_blocking` + channel + 主循环回调，绝不学 Kontainer 的 `QEventLoop` 阻塞
3. **引擎发出生命周期事件，GUI 停止轮询**："每秒轮询 list 直到名字出现"和"重列以数容器"都是缺完成事件的症状
4. **终端是特性不是传输层**：引擎给非交互式流式操作 + 干净退出契约；"打开终端"保留为显式、良好封装的用户动作（引擎给命令，GUI 发现应用）
5. **错误一等对象**：code + message + stderr 尾部
6. **Flatpak 首日规划**，但沙箱边界放引擎：`flatpak-spawn --host`、portal 路径解析（xattr 技巧 `distroboxmanager.cpp:85-106`）、只读宿主目录全是引擎职责
7. **GUI 永不解析 .desktop**：引擎给应用元数据（name/icon/exported/所属容器）
8. **稳定 ID 与元数据进 API**：BoxBuddy 保留 container_id，Kontainer 丢弃 → 只能靠名字字符串到处匹配
9. **rootful 提前决策**：BoxBuddy 因 UX 拒绝；要支持就引擎层一等公民化（提权管理 + 每容器开关）
10. **护栏优于自由参数**：Kontainer 未加引号透传（`distroboxmanager.cpp:276-279`）是安全坑 + CLI 缺陷泄入 GUI bug（#65 custom home）；结构化创建选项 + 校验 + **保留命令预览**（用户已被证明喜欢）
