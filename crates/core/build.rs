fn main() {
    // 让 shared::loader!() 能读到 [package.metadata.shared] SERVER_BIN。
    // core 内部用 SERVER_BIN namespace 解析 server_binary_path()（dev/prod 自动切）。
    easytidy_shared::emit_namespaces();
}
