#!/bin/bash
# 前端 dev server（tauri beforeDevCommand；cwd 无关定位，见 build-ui.sh）
cd "$(dirname "$0")/../ui"
exec pnpm dev
