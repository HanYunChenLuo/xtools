#!/bin/bash
# macOS 发布产物构建 + 上传（内部 GitLab 无 macOS runner 的补偿路径，配合 tag 流水线）：
#   1. 本机构建 xperf-cli（arm64 + x86_64 双架构）与 xperf-agent（aarch64-linux-android，
#      经本机 NDK，.cargo/ndk-clang.sh 自动探测）
#   2. 打包 tar.gz（版本目录：CLI + agent + 文档，与 Linux 包同构）
#   3. 经 scripts/release_upload.py 上传 package registry 并挂到 tag 的 Release
#
# 用法（tag 流水线完成后，在仓库根目录执行）：
#   GITLAB_TOKEN=<api 权限 PAT> scripts/release-macos.sh v0.2.0
#
# 前置：Xcode CLT、rustup、Android NDK >= 25.1.8937393；Release 已由 tag 流水线创建。
# 重复执行幂等（同名资产链接先删后建）。
set -euo pipefail

TAG="${1:?用法: GITLAB_TOKEN=<token> scripts/release-macos.sh vX.Y.Z}"
VER="${TAG#v}"

CARGO_VER=$(sed -n 's/^version = "\(.*\)"$/\1/p' xperf-cli/Cargo.toml | head -1)
if [ "$VER" != "$CARGO_VER" ]; then
    echo "错误: tag 版本 $VER 与 xperf-cli/Cargo.toml 的 $CARGO_VER 不一致"; exit 1
fi
if [ -z "${GITLAB_TOKEN:-}" ]; then
    echo "错误: 需要 GITLAB_TOKEN（api 权限的 Personal Access Token）"; exit 1
fi
export RELEASE_TAG="$TAG"

echo "==> rustup targets"
rustup target add aarch64-apple-darwin x86_64-apple-darwin aarch64-linux-android

build() {  # build <apple 三元组> <包架构名>
    local target="$1" arch="$2"
    echo "==> cargo build --release -p xperf-cli --target $target"
    cargo build --release -p xperf-cli --target "$target"
    local pkg="xtools-$TAG-macos-$arch"
    rm -rf "artifacts/$pkg" "artifacts/$pkg.tar.gz"
    mkdir -p "artifacts/$pkg/agent"
    cp "target/$target/release/xperf-cli" "artifacts/$pkg/"
    cp target/aarch64-linux-android/release/xperf-agent "artifacts/$pkg/agent/"
    cp README.md README_zh.md LICENSE CHANGELOG.md "artifacts/$pkg/"
    tar -C artifacts -czf "artifacts/$pkg.tar.gz" "$pkg"
}

echo "==> cargo build --release -p xperf-agent --target aarch64-linux-android"
cargo build -p xperf-agent --target aarch64-linux-android --release

build aarch64-apple-darwin arm64
build x86_64-apple-darwin x86_64

python3 scripts/release_upload.py "artifacts/xtools-$TAG-macos-arm64.tar.gz"  "macOS ARM64 (CLI + agent)"
python3 scripts/release_upload.py "artifacts/xtools-$TAG-macos-x86_64.tar.gz" "macOS x86_64 (CLI + agent)"
echo "==> 完成：macOS 双架构产物已挂到 Release $TAG"
