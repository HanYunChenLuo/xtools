#!/bin/bash

# xrm 安装脚本
# 用于安装安全的文件删除工具

set -e

echo "🔧 开始安装 xrm..."

# 检查是否安装了 Rust
if ! command -v cargo &> /dev/null; then
    echo "❌ 错误: 未找到 cargo 命令"
    echo "请先安装 Rust: https://rustup.rs/"
    exit 1
fi

# 构建项目
echo "📦 构建 xrm..."
cargo build --release

# 检查构建是否成功
if [ ! -f "target/release/xrm" ]; then
    echo "❌ 构建失败"
    exit 1
fi

# 安装到系统路径
INSTALL_DIR="/usr/local/bin"
if [ -w "$INSTALL_DIR" ]; then
    echo "📋 安装到 $INSTALL_DIR..."
    cp target/release/xrm "$INSTALL_DIR/"
    chmod +x "$INSTALL_DIR/xrm"
else
    echo "📋 需要 sudo 权限安装到 $INSTALL_DIR..."
    sudo cp target/release/xrm "$INSTALL_DIR/"
    sudo chmod +x "$INSTALL_DIR/xrm"
fi

echo "✅ xrm 安装成功!"
echo ""
echo "使用方法:"
echo "  xrm --help                    # 查看帮助"
echo "  xrm file.txt                  # 删除文件"
echo "  xrm directory/                # 自动递归删除目录"
echo "  xrm -v -f file1 file2         # 详细模式强制删除多个文件"
echo ""
echo "安全特性:"
echo "  ✓ 拒绝执行 sudo xrm /"
echo "  ✓ 拒绝执行 sudo xrm /*"
echo "  ✓ 保护系统关键目录"
echo "  ✓ 详细的错误提示"
