fn main() {
    tauri_build::build();
    // 让 shared::loader!() 能读到 GUI crate 的 [package.metadata.shared]
    // namespace（虽然 GUI 自己没声明，但保持调用形式一致，便于日后 GUI
    // 加自己的 namespace 时 build.rs 不用改）。
    easytidy_shared::emit_namespaces();
}
