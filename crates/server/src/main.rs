//! easytidy 容器内 server（M2 实现）。
//!
//! 部署形态：静态 musl 二进制，bind-mount 进容器，
//! 作为容器 entrypoint 主进程（PID 1 = catatonit，经 podman `--init` 注入）。
//! 职责（v0.5 需求）：业务 setup、容器内应用生命周期、socket 服务面
//! （PTY / 文件 / 应用枚举 / 配置）。

fn main() {
    eprintln!("easytidy-server: not implemented yet (M2)");
    std::process::exit(1);
}
