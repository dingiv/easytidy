//! 数据卷端到端实测（**真实 podman socket**，覆盖 Rust 侧 CRUD 全路径——
//! curl 级验证见 AGENTS「数据卷 named volume」第一轮记录）。
//!
//! 默认忽略；手动跑：
//! ```sh
//! podman system service --time=0 unix://$XDG_RUNTIME_DIR/podman/podman.sock &
//! cargo test -p easytidy-core --test volume_live -- --ignored --nocapture
//! ```

use easytidy_core::podman::Podman;

#[tokio::test]
#[ignore = "需要运行中的 podman socket（podman system service）"]
async fn volume_crud_live() {
    let sock = std::env::var("XDG_RUNTIME_DIR")
        .map(|d| std::path::PathBuf::from(d).join("podman/podman.sock"))
        .unwrap_or_default();
    assert!(sock.exists(), "podman socket 不存在：{}（先起 service）", sock.display());

    let podman = Podman::connect().await.expect("连接 podman 失败");
    let name = "et-live-vol-test";

    // 清理上一轮残留（幂等起步）
    let _ = podman.volume_remove(name, true).await;

    // create：label + Pascal→snake 反序列化
    let created = podman.volume_create(name).await.expect("volume_create 失败");
    assert_eq!(created.name, name);
    assert_eq!(created.driver, "local");
    assert_eq!(
        created.labels.get("manager").map(String::as_str),
        Some("easytidy"),
        "easytidy 创建的卷应带接管 label"
    );

    // list：全量包含它
    let list = podman.volume_list().await.expect("volume_list 失败");
    let me = list.iter().find(|v| v.name == name).expect("list 应含新建卷");
    assert_eq!(me.mount_count, 0, "新建卷应空闲");
    assert!(list.iter().any(|v| v.name == "vscode" || v.name != name),
        "全量列出应含宿主其它卷（定案：不分接管）");

    // exists
    assert!(podman.volume_exists(name).await.unwrap(), "exists 应为 true");

    // ensure 幂等：已存在 → 不报错、label 不变
    podman.volume_ensure(name).await.expect("volume_ensure 幂等失败");
    let again = podman.volume_create(name).await;
    assert!(again.is_err(), "同名再 create 应报错（实测 500 already exists；volume_ensure 才是幂等入口）");

    // remove（空闲 → 204）
    podman.volume_remove(name, false).await.expect("volume_remove 失败");
    assert!(!podman.volume_exists(name).await.unwrap(), "删除后应不存在");
}
