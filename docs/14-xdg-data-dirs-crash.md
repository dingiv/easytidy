# 14. XDG_DATA_DIRS 覆盖导致容器内 GTK 崩溃（Chrome 保存图片）

> 2026-08-07 实测。根因是 flavor 注入 `XDG_DATA_DIRS=/usr/share/easytidy-host`
> **纯覆盖**了系统默认值，容器内 gdk-pixbuf 找不到系统 loader 注册表，
> PNG 图标无法解码，GTK 文件选择器**断言失败直接 abort**（非优雅降级）。

## 1. 现象

google-chrome 中"保存图片"时崩溃（core dumped）：

```
Gtk-WARNING: Could not load a pixbuf from icon theme.
  This may indicate that pixbuf loaders or the mime database could not be found.
Gtk:ERROR:gtkiconhelper.c:495:ensure_surface_for_gicon:
  assertion failed (error == NULL):
  Failed to load /usr/share/easytidy-host/icons/Yaru/16x16/status/image-missing.png:
  Unrecognized image file format (gdk-pixbuf-error-quark, 3)
Bail out! ... Aborted (core dumped)
```

报错路径指向 easytidy 挂载的宿主图标目录，一度怀疑是挂载/图标损坏——
**实际两者都无辜**（见下）。

## 2. 排查链（教训：逐层排除，别停在表面）

| 步骤 | 结论 |
|---|---|
| `file` 宿主 `image-missing.png` | 有效 PNG（16x16 colormap） |
| 容器内外 md5 对比 | **一致**——挂载无损坏 |
| 容器 loaders/ 目录 | 无 `libpixbufloader-png.so`（12 个 loader，缺 png/jpeg） |
| 官方 noble 包 filelist | **官方本来就不含 png/jpeg loader**——PNG 支持内建主库 |
| 拷出主库宿主侧 `nm -D` | **40 个 png 符号**——主库内建 PNG，库没坏 |
| `--env XDG_DATA_DIRS=/usr/local/share:/usr/share` 重测 thumbnailer | **成功** ← 根因落点 |

> 注意：容器内可能没有 binutils（`nm` 不存在会静默失败），**先在宿主侧
> 分析拷出的库文件**，或在容器内 `dpkg -V` 校验完整性。

## 3. 根因机制

gdk-pixbuf 2.38+ 查找 loader 注册表（`gdk-pixbuf-io.c`）按序尝试：

1. `$XDG_DATA_HOME/gdk-pixbuf-2.0/2.10.0/loaders.cache`
2. **`$XDG_DATA_DIRS/gdk-pixbuf-2.0/2.10.0/loaders.cache`**（逐目录）
3. 编译路径 `/usr/lib/.../gdk-pixbuf-2.0/2.10.0/loaders.cache`

**PNG 解码虽内建在主库，但格式注册仍走 loaders.cache**。flavor 注入
`XDG_DATA_DIRS=/usr/share/easytidy-host`（纯覆盖）后，步骤 2 只查
easytidy-host（无此目录），步骤 3 为兜底本可命中——但实测纯覆盖下
**全部失败**（thumbnailer 复现），原因可能是 XDG_DATA_DIRS 非空时 glib
仅使用其值（`g_get_system_data_dirs()`），并影响 mime 数据库
（`$XDG_DATA_DIRS/mime/mime.cache`）一并丢失——首行 warning 同时点名
"pixbuf loaders **or the mime database**"。

GTK 侧：`gtkiconhelper.c` 对 gicon 加载失败是 **`g_assert(error == NULL)`
直接 abort**，没有降级路径——图标损坏/解码失败即崩溃，无挽回余地。

## 4. 修复（三处，双保险）

1. **flavor.rs（新建/重建容器）**：注入改为追加系统默认——
   `XDG_DATA_DIRS=/usr/share/easytidy-host:/usr/local/share:/usr/share`
   （glib 默认值；容器内缺失路径无害）。
2. **server 启动（存量容器即时生效）**：`fixup_xdg_data_dirs()` 在
   main 开头检测并追加系统默认目录（server 自身 env → entry 子进程继承）。
3. **server PTY spawn（存量容器即时生效）**：PTY 请求 env 中的
   `XDG_DATA_DIRS` 经 `fixup_xdg_data_dirs_value()` 同样修正
   （su 会保留非安全敏感变量，须在 spawn 前修）。

> 调用点位于 tracing init 之前时 `info!` 会被丢弃（看不到"已修正"日志），
> 用子进程 env 验证，别依赖日志。

## 5. 验证

- 经 server PTY：`easytidy run --container chrome -- gdk-pixbuf-thumbnailer -s 16 <png>`
  → exit=0（修复前报 "Couldn't recognize the image file format"）
- 容器**配置 env**（`podman inspect` 的 `.Config.Env`）仍是旧值——create 时
  固化，**存量容器需重建**（rebuild：commit 层 + 新 config）才彻底修；
  经 easytidy 终端/run 启动的应用不受影响（server 已修正）。

## 6. 通用教训

- **向容器注入 `XDG_*`（及 PATH/LD_*）必须保留系统默认**：追加而不是覆盖；
  宿主路径与容器路径语义不同，缺失路径无害、覆盖则破坏系统查找。
- GTK 的 gicon 加载失败是**硬崩溃**（assertion），图标类问题优先级高。
- 容器内 GUI 应用验证"图标/图片/缩略图"类功能时，先验证 gdk-pixbuf
  基础解码（`gdk-pixbuf-thumbnailer` 一条命令），再排查上层。
