// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use easytidy_gui_lib::AppMode;
use std::env;

fn main() {
    // 在 Tauri 初始化之前解析命令行参数
    let args: Vec<String> = env::args().collect();
    let mut mode = AppMode::Master;
    let mut config_file: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--container" => {
                if i + 1 < args.len() {
                    mode = AppMode::Worker {
                        name: args[i + 1].clone(),
                    };
                    i += 2;
                } else {
                    eprintln!("error: --container requires a container name");
                    std::process::exit(1);
                }
            }
            "--config" => {
                if i + 1 < args.len() {
                    config_file = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!("error: --config requires a file path");
                    std::process::exit(1);
                }
            }
            _ => {
                eprintln!("error: unknown argument: {}", args[i]);
                std::process::exit(1);
            }
        }
    }

    // 进程锁（$XDG_RUNTIME_DIR/easytidy/ 下 flock）：
    // - Master GUI：gui.lock，单实例
    // - Worker GUI：gui-<name>.lock，每容器实例单实例
    // 锁由进程持有 fd，退出/崩溃自动释放；已存在实例时直接退出。
    let _lock = {
        let container = match &mode {
            AppMode::Worker { name } => Some(name.as_str()),
            AppMode::Master => None,
        };
        match easytidy_core::guilock::acquire(container) {
            Ok(lock) => lock,
            Err(e) => {
                eprintln!("easytidy: {e}");
                std::process::exit(1);
            }
        }
    };

    // 调用 lib.rs 的 run 函数
    easytidy_gui_lib::run(mode, config_file);
}
