#!/usr/bin/env bash
# build-server.sh - 构建 easytidy-server musl 静态二进制
#
# 此脚本构建 easytidy-server 的 musl 静态二进制，并将其安装到：
# $XDG_DATA_HOME/easytidy/bin/easytidy-server（默认）
# 或 ~/.local/share/easytidy/bin/easytidy-server（fallback）
#
# 用法：
#   ./scripts/build-server.sh
#
# 要求：
#   - rustup（已安装 x86_64-unknown-linux-musl target）
#   - cargo

set -euo pipefail

# 脚本目录
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(dirname "$SCRIPT_DIR")"

# 颜色输出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

info() {
    echo -e "${GREEN}[INFO]${NC} $*"
}

warn() {
    echo -e "${YELLOW}[WARN]${NC} $*"
}

error() {
    echo -e "${RED}[ERROR]${NC} $*" >&2
    exit 1
}

# 检查 musl target
check_musl_target() {
    info="检查 musl target..."
    if ! rustup target list --installed 2>/dev/null | grep -q "x86_64-unknown-linux-musl"; then
        info="添加 musl target..."
        rustup target add x86_64-unknown-linux-musl || error "添加 musl target 失败"
    fi
    info "musl target 已就绪"
}

# 构建 server 二进制
build_server() {
    info "开始构建 easytidy-server（musl static）..."
    cd "$WORKSPACE_ROOT"

    cargo build \
        -p easytidy-server \
        --release \
        --target x86_64-unknown-linux-musl \
        || error "构建失败"

    info "构建成功"
}

# 安装二进制
install_server() {
    info "安装 server 二进制..."

    # 确定安装目录
    if [ -n "${XDG_DATA_HOME:-}" ]; then
        INSTALL_DIR="$XDG_DATA_HOME/easytidy/bin"
    else
        INSTALL_DIR="$HOME/.local/share/easytidy/bin"
    fi

    # 创建目录
    mkdir -p "$INSTALL_DIR" || error "创建安装目录失败：$INSTALL_DIR"

    # 源文件和目标文件
    SOURCE="$WORKSPACE_ROOT/target/x86_64-unknown-linux-musl/release/easytidy-server"
    TARGET="$INSTALL_DIR/easytidy-server"

    # 检查源文件
    [ -f "$SOURCE" ] || error "源文件不存在：$SOURCE"

    # 复制文件
    cp -f "$SOURCE" "$TARGET" || error "复制文件失败：$SOURCE → $TARGET"

    # 设置可执行权限
    chmod +x "$TARGET" || error "设置权限失败：$TARGET"

    info "安装成功：$TARGET"
}

# 验证静态链接
verify_static() {
    info "验证静态链接..."

    TARGET="$INSTALL_DIR/easytidy-server"

    if command -v file >/dev/null 2>&1; then
        file "$TARGET" | grep -q "statically linked" && \
            info "✓ 静态链接验证通过" || \
            warn "⚠ 可能不是静态链接，但仍可使用"
    else
        warn "未找到 file 命令，跳过验证"
    fi
}

# 主流程
main() {
    info "开始构建 easytidy-server..."

    check_musl_target
    build_server
    install_server
    verify_static

    echo ""
    info "构建完成！server 二进制已安装到：$INSTALL_DIR/easytidy-server"
    echo "现在可以运行：easytidy create --image <image> --name <name>"
}

main "$@"
