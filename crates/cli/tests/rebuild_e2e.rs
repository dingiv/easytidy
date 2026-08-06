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

    // 假 server 二进制（带 shebang，容器内可执行；sleep 让容器保持运行）
    let data_home = tmp_path.join("data");
    let bin_dir = data_home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_server = bin_dir.join("easytidy-server");
    fs::write(&fake_server, "#!/bin/sh\nsleep 300\n").unwrap();
    fs::set_permissions(&fake_server, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .unwrap();
    // rebuild() 内部经 server_binary_path() 解析 → 指向假二进制（测试环境隔离）
    std::env::set_var("XDG_DATA_HOME", &data_home);

    // 配置：一条 bind mount + 一个 mapped 端口（8080 类随机空闲端口 → 80/tcp）
    let host_port = free_host_port();
    let config = ContainerConfig {
        name: name.clone(),
        image: "docker.io/library/alpine:latest".to_string(),
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
    };

    // 私有 configfile（不碰用户真实配置）
    let config_file = ConfigFile::with_path(tmp_path.join("config.toml"));
    config_file
        .register_container(config.clone())
        .expect("注册容器配置失败");

    // 验证过程（返回 Err 而非 panic，保证清理段始终执行）
    let result: Result<(), String> = async {
        // create（初始状态，含旧配置）
        let id = podman
            .create_with_config(&name, &config.image, &fake_server, &config)
            .await
            .map_err(|e| format!("创建容器失败：{e}"))?;
        println!("[create] id = {id}");

        // 修改配置（模拟用户编辑 config.toml）：新增第二条 mount
        let config2 = {
            let mut c = config.clone();
            c.mounts.push(MountConfig {
                host_path: tmp_path.to_string_lossy().to_string(),
                container_path: "/tmp-e2e".to_string(),
                read_only: true,
            });
            c
        };
        config_file
            .register_container(config2.clone())
            .map_err(|e| format!("更新配置失败：{e}"))?;

        // rebuild：commit → stop → rm → create（同名，新配置）→ start
        let new_id = podman
            .rebuild(&name, &config2)
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

        Ok(())
    }
    .await;

    // 清理（尽力）：删除容器 + 注销配置（configfile 为临时文件，随 TempDir 自动清理）
    if let Err(e) = podman.remove(&name, true).await {
        println!("[cleanup] 删除容器失败（忽略）：{e}");
    }

    result.expect("e2e 验证失败");
}
