#!/usr/bin/env bash

set -euo pipefail

# 1. 检查是否以 root 权限运行
if [[ "${EUID}" -ne 0 ]]; then
    echo "错误：此脚本必须以 root 权限运行！" >&2
    exit 1
fi

# 2. 检查参数输入
if [[ $# -lt 1 ]]; then
    echo "用法: $0 <用户名>" >&2
    echo "示例: $0 ubuntu" >&2
    exit 1
fi

TARGET_USER="$1"

# 3. 检查目标用户是否存在
if ! id "${TARGET_USER}" &>/dev/null; then
    echo "错误：用户 '${TARGET_USER}' 不存在！" >&2
    exit 1
fi

echo "[1/2] 配置 /etc/pam.d/sudo 绕过 sudo-rs 校验逻辑..."
cat << 'EOF' > /etc/pam.d/sudo
#%PAM-1.0
auth       sufficient   pam_permit.so
auth       include      common-auth
account    sufficient   pam_permit.so
account    include      common-account
password   include      common-password
session    include      common-session
EOF

echo "[2/2] 为用户 '${TARGET_USER}' 配置免密 sudo 权限..."
SUDOERS_FILE="/etc/sudoers.d/99-nopasswd-${TARGET_USER}"
SUDOERS_RULE="${TARGET_USER} ALL=(ALL:ALL) NOPASSWD: ALL"

# 写入独立配置文件并验证语法，确保系统安全
echo "${SUDOERS_RULE}" > "${SUDOERS_FILE}"
chmod 0440 "${SUDOERS_FILE}"

if visudo -cf "${SUDOERS_FILE}"; then
    echo "配置成功！用户 '${TARGET_USER}' 现在可以免密使用 sudo。"
else
    echo "错误：sudoers 语法检查失败，删除无效配置文件！" >&2
    rm -f "${SUDOERS_FILE}"
    exit 1
fi