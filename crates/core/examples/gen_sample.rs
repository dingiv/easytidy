//! 验证：容器入口兜底图标（identicon + 品牌包装）真实落盘。
fn main() {
    let p = easytidy_core::desktop::ensure_container_entry_icon("chrome", "chrome").unwrap();
    println!("{p}");
}
