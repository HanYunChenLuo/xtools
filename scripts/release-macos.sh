#!/bin/bash
# macOS 发布产物构建 + 上传（内部 GitLab 无 macOS runner 的补偿路径，配合 tag 流水线）：
#   1. 本机构建 xperf-cli（arm64 + x86_64 双架构）、xperf-agent（aarch64-linux-android）
#      与 xperf-gui（两个 macOS 架构的 DMG）；agent 经本机 NDK 自动探测
#   2. 打包 CLI tar.gz；GUI DMG 内置同一份预编译 agent
#   3. 经 scripts/release_upload.py 上传 package registry 并挂到 tag 的 Release
#
# 用法（tag 流水线完成后，在仓库根目录执行）：
#   GITLAB_TOKEN=<api 权限 PAT> scripts/release-macos.sh v0.2.0
#
# 前置：Xcode CLT、rustup、cargo-tauri 2.4.1、Android NDK >= 25.1.8937393；Release 已由 tag 流水线创建。
# 默认生成完整 ad-hoc 签名的 .app（修复资源未封印导致的“已损坏”提示）；有 Developer ID 时
# 设置 APPLE_SIGNING_IDENTITY 覆盖发布配置，并另行配置公证凭据。
# 重复执行幂等（同名资产链接先删后建）。
set -euo pipefail

TAG="${1:?用法: GITLAB_TOKEN=<token> scripts/release-macos.sh vX.Y.Z}"
VER="${TAG#v}"
ROOT_DIR=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT_DIR"

CARGO_VER=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
if [ "$VER" != "$CARGO_VER" ]; then
    echo "错误: tag 版本 $VER 与 xperf-cli/Cargo.toml 的 $CARGO_VER 不一致"; exit 1
fi
if [ -z "${GITLAB_TOKEN:-}" ]; then
    echo "错误: 需要 GITLAB_TOKEN（api 权限的 Personal Access Token）"; exit 1
fi
export RELEASE_TAG="$TAG"

echo "==> rustup targets"
rustup target add aarch64-apple-darwin x86_64-apple-darwin aarch64-linux-android

GUI_CONFIG="tauri.release.json"
GUI_RESOURCES="xperf-gui/release-resources"
trap 'rm -rf "$GUI_RESOURCES"' EXIT

prepare_gui_agent() {
    if ! cargo tauri --version >/dev/null 2>&1; then
        echo "==> 安装 cargo-tauri 2.4.1"
        cargo install tauri-cli --version 2.4.1 --locked
    fi
    rm -rf "$GUI_RESOURCES"
    mkdir -p "$GUI_RESOURCES/agent"
    cp target/aarch64-linux-android/release/xperf-agent "$GUI_RESOURCES/agent/xperf-agent"
    chmod 755 "$GUI_RESOURCES/agent/xperf-agent"
}

validate_dmg() {
    local dmg="$1" mount_dir app
    mount_dir=$(mktemp -d "${TMPDIR:-/tmp}/xperf-dmg.XXXXXX")
    if ! hdiutil attach -readonly -nobrowse -mountpoint "$mount_dir" "$dmg" >/dev/null 2>&1; then
        rm -rf "$mount_dir"
        return 1
    fi
    app="$mount_dir/xperf-gui.app"
    if ! test -d "$app" || ! codesign --verify --deep --strict --verbose=2 "$app"; then
        hdiutil detach "$mount_dir" >/dev/null 2>&1 || true
        rm -rf "$mount_dir"
        return 1
    fi
    hdiutil detach "$mount_dir" >/dev/null 2>&1
    rm -rf "$mount_dir"
}

build_gui() {  # build_gui <Apple target> <DMG architecture>
    local target="$1" arch="$2"
    echo "==> cargo tauri build --target $target --bundles dmg"
    (cd xperf-gui && CI=true cargo tauri build --config "$GUI_CONFIG" --target "$target" --bundles dmg)
    local dmg
    dmg=$(find "target/$target/release/bundle/dmg" -type f -name '*.dmg' -print -quit)
    test -n "$dmg" && test -s "$dmg"
    validate_dmg "$dmg"
    cp "$dmg" "artifacts/xperf-v$VER-macos-$arch-gui.dmg"
}

build() {  # build <apple 三元组> <包架构名>
    local target="$1" arch="$2"
    echo "==> cargo build --release -p xperf-cli --target $target"
    cargo build --release -p xperf-cli --target "$target"
    local pkg="xperf-$TAG-macos-$arch"
    rm -rf "artifacts/$pkg" "artifacts/$pkg.tar.gz"
    mkdir -p "artifacts/$pkg/agent"
    cp "target/$target/release/xperf-cli" "artifacts/$pkg/"
    cp target/aarch64-linux-android/release/xperf-agent "artifacts/$pkg/agent/"
    cp README.md README_zh.md LICENSE CHANGELOG.md "artifacts/$pkg/"
    # COPYFILE_DISABLE=1：bsdtar 默认写入 ._ AppleDouble 元数据文件与 xattr，污染包
    COPYFILE_DISABLE=1 tar -C artifacts -czf "artifacts/$pkg.tar.gz" "$pkg"
}

echo "==> cargo build --release -p xperf-agent --target aarch64-linux-android"
cargo build -p xperf-agent --target aarch64-linux-android --release
prepare_gui_agent

build aarch64-apple-darwin arm64
build x86_64-apple-darwin x86_64
build_gui aarch64-apple-darwin arm64
build_gui x86_64-apple-darwin x86_64

python3 scripts/release_upload.py "artifacts/xperf-$TAG-macos-arm64.tar.gz"  "macOS ARM64 (CLI + agent)"
python3 scripts/release_upload.py "artifacts/xperf-$TAG-macos-x86_64.tar.gz" "macOS x86_64 (CLI + agent)"
python3 scripts/release_upload.py "artifacts/xperf-v$VER-macos-arm64-gui.dmg"  "macOS ARM64 GUI (DMG + agent)"
python3 scripts/release_upload.py "artifacts/xperf-v$VER-macos-x86_64-gui.dmg" "macOS x86_64 GUI (DMG + agent)"
echo "==> 完成：macOS CLI 双架构 + GUI 双架构 DMG 已挂到 Release $TAG"
