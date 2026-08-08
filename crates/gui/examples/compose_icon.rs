//! 图标合成效果示例。
//!
//! 使用 `crates/gui/icons/Square284x284Logo.png` 作为基础图标,
//! `easytidy256x256.png` 作为水印,合成 256×256 品牌化图标。
//!
//! 运行:cargo run -p easytidy-gui --example compose_icon
//! 输出:workspace 根 target/composed-icon.png

use std::path::Path;

fn main() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let base_path = manifest_dir.join("icons/Square284x284Logo.png");
    let wm_path = manifest_dir.join("icons/easytidy256x256.png");
    // gui → crates → workspace 根（easytidy/target）
    let out_path = manifest_dir.join("../../target/composed-icon.png");

    let base = std::fs::read(&base_path)
        .unwrap_or_else(|e| panic!("读取基础图标失败（{}）：{e}", base_path.display()));
    let wm = std::fs::read(&wm_path)
        .unwrap_or_else(|e| panic!("读取水印失败（{}）：{e}", wm_path.display()));

    let out = easytidy_core::icon::compose_app_icon(&base, &wm).expect("合成失败");
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).expect("创建输出目录失败");
    }
    std::fs::write(&out_path, &out).expect("写入输出失败");

    println!("合成完成: {}", out_path.display());
}
