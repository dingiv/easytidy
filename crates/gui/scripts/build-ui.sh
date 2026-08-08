#!/bin/bash
# 前端生产构建（tauri beforeBuildCommand）。
# ⚠️ tauri-cli 执行该命令时的 cwd 在不同版本/布局下有差异（曾出现 ui/ui
# 相对路径错位），脚本内用自身路径绝对定位，cwd 无关。
set -e
cd "$(dirname "$0")/../ui"
pnpm build
