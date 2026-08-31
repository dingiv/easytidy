#

## 开发约定

### Tauri 命令参数必须用驼峰（camelCase）

前端 `invoke` 调用 `#[tauri::command]` 时，**参数名必须用驼峰**（与 Rust 侧 snake_case 自动对应）：

```ts
// ✓ 正确：Rust 参数 `snapshot_name` ← 前端 `snapshotName`
invoke('env_snapshot', { name, snapshotName: snapshotName || null });

// ✗ 错误：前端用 `snapshot_name` —— Tauri 按 camelCase 提取参数，
//   取不到 → Option 参数静默变 None（不报错！）
invoke('env_snapshot', { name, snapshot_name: snapshotName || null });
```

**踩坑记录（2026-08-31 快照命名丢失）：** 前端误传 snake_case 的 `snapshot_name`，
后端 `Option<String>` 静默收到 `None`，走了默认名兜底，用户输入的快照名丢失。
`Option` 参数收不到时**没有任何报错**，排查成本极高——传参前确认键名是驼峰。

（Tauri v2 `#[tauri::command]` 默认按 camelCase 解析 JS 侧参数，Rust 参数保持 snake_case 即可。）
