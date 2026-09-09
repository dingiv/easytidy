//! e2e（`#[ignore]`，需要真实 podman）：验证 rebuild 后 mounts 与端口映射生效。
//!
//! 运行方式：
//! ```bash
//! systemctl --user start podman.socket
//! cargo test -p easytidy --test rebuild_e2e -- --ignored --nocapture
//! ```
//!
//! 流程：create（临时 configfile + 假 server 二进制）→ 手写一条 bind mount
//! （宿主临时目录 → /data）与 mapped 端口（随机空闲宿主端口 → 容器 80/tcp）
//! 到配置 → `Podman::rebuild` → 用独立 bollard 连接（等价 `podman inspect`）
//! 验证 HostConfig.Mounts 与 NetworkSettings.Ports → 清理容器。

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use bollard::Docker;
use easytidy_core::configfile::ConfigFile;
use easytidy_core::models::{
    ContainerConfig, MountConfig, NetworkConfig, NetworkMode, PortMapping,
};
use easytidy_core::podman::Podman;

fn unique_name(prefix: &str) -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    format!("{prefix}-{ts}")
}

/// 获取一个当前空闲的宿主端口（bind :0 探测后释放；竞争窗口可忽略）。
fn free_host_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// 断言辅助（返回 Err 而非 panic，保证测试清理段始终执行）。
fn check(cond: bool, msg: &str) -> Result<(), String> {
    if cond {
        Ok(())
    } else {
        Err(msg.to_string())
    }
}

/// 假 dock 脚本（仅 e2e）：最小模拟 `prepare`（alpine/busybox 工具集）：
/// 幂等（同名已存在即退出）→ 清运行时占位条目（home=/ 的该 uid 条目）→
/// adduser 建号 → mkdir home。真实 dock 是纯 Rust 二进制（零命令依赖），
/// 此脚本只用于不构建 musl 二进制的 e2e 环境。
const FAKE_CTOOL_SCRIPT: &str = r#"#!/bin/sh
set -u
# e2e 最小模拟 `client ping`（rebuild 的 dock daemon 存活确认，start_and_confirm 依赖）
if [ "${1:-}" = "client" ] && [ "${2:-}" = "ping" ]; then
  echo "alive: true"
  exit 0
fi
uid=1000; gid=1000; name=""
while [ $# -gt 0 ]; do
  case "$1" in
    prepare) shift ;;
    --uid) uid="$2"; shift 2 ;;
    --gid) gid="$2"; shift 2 ;;
    --name) name="$2"; shift 2 ;;
    *) shift ;;
  esac
done
if [ -n "$name" ]; then
  [ -n "$(getent passwd "$name")" ] && exit 0
  ph="$(awk -F: -v u="$uid" '$3+0==u+0 && $6=="/" {print $1; exit}' /etc/passwd)"
  if [ -n "$ph" ]; then
    sed -i "/^${ph}:/d" /etc/passwd /etc/group
  fi
  adduser -D -u "$uid" -h "/home/$name" -s /bin/sh "$name"
  mkdir -p "/home/$name"
fi
exit 0
"#;

/// 假容器内二进制（e2e 专用）：server = sleep 保活；dock = 最小 prepare
/// 模拟。测试直接传假路径，不依赖任何环境变量解析。
fn make_fake_bins(dir: &std::path::Path) -> easytidy_core::ContainerBins {
    use std::os::unix::fs::PermissionsExt;
    let fake_server = dir.join("easytidy-server");
    fs::write(&fake_server, "#!/bin/sh\nsleep 300\n").unwrap();
    fs::set_permissions(&fake_server, std::fs::Permissions::from_mode(0o755)).unwrap();
    let fake_dock = dir.join("easytidy-dock");
    fs::write(&fake_dock, FAKE_CTOOL_SCRIPT).unwrap();
    fs::set_permissions(&fake_dock, std::fs::Permissions::from_mode(0o755)).unwrap();
    easytidy_core::ContainerBins {
        server: fake_server,
        dock: fake_dock,
        ets: None, // e2e 不覆盖 ets 挂载（可选分发链）
    }
}

#[tokio::test]
#[ignore = "需要真实 podman socket（systemctl --user start podman.socket）"]
async fn rebuild_applies_mounts_and_ports() {
    let podman = Podman::connect()
        .await
        .expect("无法连接 podman socket（请先启用 podman.socket user unit）");

    let tmp = tempfile::TempDir::new().expect("创建临时目录失败");
    let tmp_path = tmp.path();
    let name = unique_name("easytidy-e2e");

    // 宿主侧挂载源目录（bind mount 要求宿主路径已存在）
    let src_dir = tmp_path.join("src");
    fs::create_dir_all(&src_dir).expect("创建挂载源目录失败");
    fs::write(src_dir.join("hello.txt"), "hello from host").unwrap();

    // 假容器内二进制（rebuild/create 显式接收路径——测试直接传假路径，
    // 不依赖任何环境变量解析，比旧的 XDG_DATA_HOME hack 更隔离）
    let bins = make_fake_bins(tmp_path);

    // 配置：一条 bind mount + 一个 mapped 端口（8080 类随机空闲端口 → 80/tcp）
    // + 显式容器默认用户（新身份模型：uid/gid 缺省 = 宿主登录用户；
    // 此处显式设 1000:1000 断言 inspect 回显——与宿主 uid 解耦，
    // 保证无登录用户环境下 e2e 仍成立）
    let host_port = free_host_port();
    let config = ContainerConfig {
        name: name.clone(),
        params: easytidy_core::models::ContainerParams {
            image: "docker.io/library/alpine:latest".to_string(),
            user_uid: Some(1000),
            user_gid: Some(1000),
            mounts: vec![MountConfig {
                host_path: src_dir.to_string_lossy().to_string(),
                container_path: "/data".to_string(),
                read_only: false,
            }],
            network: NetworkConfig {
                mode: NetworkMode::Mapped,
                ports: vec![PortMapping {
                    host_port,
                    container_port: 80,
                    protocol: "tcp".to_string(),
                }],
            },
            ..Default::default()
        },
        ..Default::default()
    };

    // 私有容器配置根目录（不碰用户真实配置）
    let config_file = ConfigFile::with_base_dir(tmp_path.to_path_buf());
    config_file
        .register_container(config.clone())
        .expect("注册容器配置失败");

    // 验证过程（返回 Err 而非 panic，保证清理段始终执行）
    let result: Result<(), String> = async {
        // create（初始状态，含旧配置）
        let id = podman
            .create_with_config(&name, &config.params.image, &bins, &config)
            .await
            .map_err(|e| format!("创建容器失败：{e}"))?;
        println!("[create] id = {id}");

        // 修改配置（模拟用户编辑 config.toml）：新增第二条 mount
        let config2 = {
            let mut c = config.clone();
            c.params.mounts.push(MountConfig {
                host_path: tmp_path.to_string_lossy().to_string(),
                container_path: "/tmp-e2e".to_string(),
                read_only: true,
            });
            c
        };
        config_file
            .register_container(config2.clone())
            .map_err(|e| format!("更新配置失败：{e}"))?;

        // rebuild：commit → rename 保留旧 → stop 旧 → create（同名，新配置）→
        // start + 确认就绪（running + dock ping）→ 删旧
        let new_id = podman
            .rebuild(&name, &config2, &bins)
            .await
            .map_err(|e| format!("rebuild 失败：{e}"))?;
        println!("[rebuild] new id = {new_id}");
        check(id != new_id, "rebuild 后容器 ID 应变化")?;

        // 独立 bollard 连接直接 inspect（等价 podman inspect，独立于引擎实现）
        let socket_path = format!(
            "{}/podman/podman.sock",
            std::env::var("XDG_RUNTIME_DIR").map_err(|e| e.to_string())?
        );
        let docker = Docker::connect_with_unix(&socket_path, 120, bollard::API_DEFAULT_VERSION)
            .map_err(|e| format!("连接 podman socket 失败：{e}"))?;
        let info = docker
            .inspect_container(&name, None)
            .await
            .map_err(|e| format!("podman inspect 失败：{e}"))?;

        // 验证 1：mounts 生效（/data → src_dir rw；/tmp-e2e → tmpdir ro；含 engine 内部挂载）。
        // podman 在顶层 Mounts（MountPoint 列表）报告生效挂载；HostConfig.Mounts 不回显。
        let mounts = info.mounts.clone().unwrap_or_default();
        println!(
            "[inspect] top-level Mounts = {}",
            serde_json::to_string_pretty(&mounts).unwrap()
        );
        let data_mount = mounts
            .iter()
            .find(|m| m.destination.as_deref() == Some("/data"))
            .ok_or("mount /data 未出现在顶层 Mounts")?;
        check(
            data_mount.source.as_deref() == Some(src_dir.to_str().unwrap()),
            "mount /data 的宿主源路径不匹配",
        )?;
        check(
            data_mount.rw.unwrap_or(true),
            "mount /data 应为可写",
        )?;
        let tmp_mount = mounts
            .iter()
            .find(|m| m.destination.as_deref() == Some("/tmp-e2e"))
            .ok_or("mount /tmp-e2e 未出现在顶层 Mounts")?;
        check(
            !tmp_mount.rw.unwrap_or(true),
            "mount /tmp-e2e 应为只读",
        )?;

        // 验证 2：端口映射生效（NetworkSettings.Ports: "80/tcp" → HostPort）
        let ports = info
            .network_settings
            .as_ref()
            .and_then(|n| n.ports.clone())
            .unwrap_or_default();
        println!(
            "[inspect] NetworkSettings.Ports = {}",
            serde_json::to_string_pretty(&ports).unwrap()
        );
        let binding = ports
            .get("80/tcp")
            .ok_or("端口 80/tcp 未出现在 NetworkSettings.Ports")?
            .as_ref()
            .ok_or("80/tcp 无绑定")?
            .first()
            .ok_or("80/tcp 无绑定条目")?;
        check(
            binding.host_port.as_deref() == Some(&host_port.to_string()),
            "宿主端口不匹配",
        )?;

        // 验证 3：rebuild 后容器应运行中（假 server sleep 300）
        let running = info
            .state
            .as_ref()
            .and_then(|s| s.running)
            .unwrap_or(false);
        println!("[inspect] State.Running = {running}");
        check(running, "rebuild 后容器应处于运行状态")?;

        // 验证 4：新身份模型——容器默认用户 = 配置的 <uid>:<gid>（inspect
        // Config.User 回显；不再恒为 "0:0"）。路径同 core inspect_config
        let user = info
            .config
            .as_ref()
            .and_then(|c| c.user.as_deref())
            .unwrap_or_default();
        println!("[inspect] Config.User = {user}");
        check(user == "1000:1000", "inspect Config.User 应回显配置的 1000:1000")?;

        Ok(())
    }
    .await;

    // 清理（尽力）：删除容器 + 注销配置（configfile 为临时文件，随 TempDir 自动清理）
    if let Err(e) = podman.remove(&name, true).await {
        println!("[cleanup] 删除容器失败（忽略）：{e}");
    }

    result.expect("e2e 验证失败");
}

/// e2e（`#[ignore]`，需真实 podman + 可运行容器）：新身份模型的容器内
/// 用户准备与 root 通道。
///
/// 验证点：
/// 1. 命名用户（`user_name` 有值）经 `prepare_container` 建号 → 容器内
///    `getent passwd <name>` 回显正确 uid/gid/home；**幂等**（二次 prepare
///    不报错、不重复建号）
/// 2. root 通道一次性 exec（`exec_oneshot` user="0"）：容器内 `id -u` == 0
///    （root 身份可用）
///
/// 运行方式：
/// ```bash
/// systemctl --user start podman.socket
/// cargo test -p easytidy --test rebuild_e2e -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "需要真实 podman socket + 可运行容器（alpine）"]
async fn identity_prepare_user_and_root_exec() {
    let podman = Podman::connect()
        .await
        .expect("无法连接 podman socket（请先启用 podman.socket user unit）");

    let tmp = tempfile::TempDir::new().expect("创建临时目录失败");
    let name = unique_name("easytidy-identity");
    let user_name = "tidye2e";
    let uid: u32 = 1011;
    let gid: u32 = 1011;

    // 假容器内二进制（server = sleep 保活；dock = 最小 prepare 模拟——
    // 真实 dock 是 musl 静态二进制，e2e 不构建它）
    let bins = make_fake_bins(tmp.path());

    // 配置：命名用户 + 显式 uid/gid（keep-id 开——与宿主 uid 无关，
    // 直接断言容器内 uid；GUI 容器才需要与宿主对齐，此处验证建号本身）
    let config = ContainerConfig {
        name: name.clone(),
        params: easytidy_core::models::ContainerParams {
            image: "docker.io/library/alpine:latest".to_string(),
            keep_id: false,
            user_uid: Some(uid),
            user_gid: Some(gid),
            user_name: Some(user_name.to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    let config_file = ConfigFile::with_base_dir(tmp.path().to_path_buf());
    config_file.register_container(config.clone()).expect("注册容器配置失败");

    let result: Result<(), String> = async {
        podman
            .create_with_config(&name, &config.params.image, &bins, &config)
            .await
            .map_err(|e| format!("创建容器失败：{e}"))?;
        println!("[create] 容器 {name}");
        podman.start(&name).await.map_err(|e| format!("启动容器失败：{e}"))?;
        // 等待 server/容器就绪（alpine 镜像首启需拉取/解压）
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;

        // 1. 容器内准备（fontconfig + useradd 建号）
        podman
            .prepare_container(&name, &config.params)
            .await
            .map_err(|e| format!("prepare_container 失败：{e}"))?;
        println!("[prepare] 容器内用户准备完成");

        // 2. 验证建号：getent passwd <name>（root exec，非 tty 一次性）
        let getent = podman
            .exec_oneshot(
                &name,
                "0",
                vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    format!("getent passwd {user_name}"),
                ],
            )
            .await
            .map_err(|e| format!("getent exec 失败：{e}"))?;
        println!("[getent] 退出码={} stdout={:?}", getent.code, getent.stdout.trim());
        check(getent.code == 0, "getent passwd 退出码应为 0（用户已建号）")?;
        // getent 行格式：name:passwd:uid:gid:gecos:home:shell
        let fields: Vec<&str> = getent.stdout.trim().split(':').collect();
        check(fields.len() >= 7, "getent 行格式应至少 7 段")?;
        check(fields[0] == user_name, "passwd 条目名应匹配 user_name")?;
        check(fields[2] == uid.to_string(), "passwd 条目 uid 应匹配配置")?;
        check(fields[3] == gid.to_string(), "passwd 条目 gid 应匹配配置")?;
        check(fields[5] == format!("/home/{user_name}"), "passwd 条目 home 应为 /home/<name>")?;

        // 3. 幂等：二次 prepare 不报错、不改变建号结果
        podman
            .prepare_container(&name, &config.params)
            .await
            .map_err(|e| format!("二次 prepare_container（幂等）失败：{e}"))?;
        let getent2 = podman
            .exec_oneshot(
                &name,
                "0",
                vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    format!("getent passwd {user_name} | wc -l"),
                ],
            )
            .await
            .map_err(|e| format!("二次 getent exec 失败：{e}"))?;
        check(
            getent2.stdout.trim() == "1",
            "二次 prepare 后该用户仍应恰好 1 条 passwd 记录（幂等，不重复建号）",
        )?;

        // 4. root 通道一次性 exec：容器内 id -u == 0（root 身份可用）
        let idu = podman
            .exec_oneshot(&name, "0", vec!["id".to_string(), "-u".to_string()])
            .await
            .map_err(|e| format!("id -u exec 失败：{e}"))?;
        println!("[id -u] 退出码={} stdout={:?}", idu.code, idu.stdout.trim());
        check(idu.code == 0, "id -u 退出码应为 0")?;
        check(idu.stdout.trim() == "0", "容器内 root exec 的 id -u 应为 0")?;

        Ok(())
    }
    .await;

    // 清理（尽力）
    if let Err(e) = podman.remove(&name, true).await {
        println!("[cleanup] 删除容器失败（忽略）：{e}");
    }

    result.expect("identity e2e 验证失败");
}
