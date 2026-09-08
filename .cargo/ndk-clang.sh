#!/bin/sh
# xperf-agent 交叉编译链接器：探测本机 NDK 的 aarch64 clang（macOS/Linux 双平台）。
#
# 版本策略：NDK >= 25.1.8937393 均可用；安装了多个时取与基线最相近的版本
# （即满足下限的最小版本）。显式环境变量 ANDROID_NDK_HOME / ANDROID_NDK_ROOT /
# NDK_HOME 优先且不做版本过滤（用户显式选择）。
# 探测顺序：显式环境变量 → ANDROID_HOME / ANDROID_SDK_ROOT → $HOME 常见 SDK 位置。
# API 级别固定 android26（二进制需兼容 Android 11 车机；NDK r21+ 均自带该 wrapper）。

TRIPLE=aarch64-linux-android
API=26
case "$(uname -s)" in
    Darwin) PREBUILT=darwin-x86_64 ;;
    *)      PREBUILT=linux-x86_64 ;;
esac

# 版本目录名（如 25.2.9519653）→ 可比较整数（major*1e12 + minor*1e9 + build）；
# 非三段纯数字的目录名返回失败
version_key() {
    _v="$1"
    _major=${_v%%.*}
    _rest=${_v#*.}
    [ "$_rest" = "$_v" ] && return 1
    _minor=${_rest%%.*}
    _build=${_rest#*.}
    [ "$_build" = "$_rest" ] && return 1
    case "${_major}${_minor}${_build}" in ''|*[!0-9]*) return 1 ;; esac
    # 拒绝前导零段（"08" 会被 shell 算术当八进制、dash 下直接致命中止）
    case "$_major$_minor$_build" in 0[0-9]*) return 1 ;; esac
    case "$_minor$_build" in 0[0-9]*) return 1 ;; esac
    case "$_build" in 0[0-9]*) return 1 ;; esac
    echo $((_major * 1000000000000 + _minor * 1000000000 + _build))
}

wrapper_for() {  # $1 = NDK 根目录；wrapper 存在则输出路径
    w="$1/toolchains/llvm/prebuilt/$PREBUILT/bin/$TRIPLE$API-clang"
    [ -x "$w" ] && echo "$w"
}

# 1) 显式环境变量：直接采用
for d in "$ANDROID_NDK_HOME" "$ANDROID_NDK_ROOT" "$NDK_HOME"; do
    [ -n "$d" ] || continue
    w=$(wrapper_for "$d") && exec "$w" "$@"
done

# 2) SDK 目录下 ndk/ 多版本：取 >= 基线中最相近的
BASELINE_KEY=$((25 * 1000000000000 + 1 * 1000000000 + 8937393))  # 25.1.8937393
best_dir=""; best_key=""; found=""
for sdk in "$ANDROID_HOME" "$ANDROID_SDK_ROOT" "$HOME/Android/Sdk" "$HOME/Library/Android/sdk"; do
    [ -n "$sdk" ] || continue
    [ -d "$sdk/ndk" ] || continue
    for dir in "$sdk/ndk"/*; do
        [ -d "$dir" ] || continue
        ver=$(basename "$dir")
        key=$(version_key "$ver") || continue
        found="$found $ver"
        if [ "$key" -ge "$BASELINE_KEY" ]; then
            if [ -z "$best_key" ] || [ "$key" -lt "$best_key" ]; then
                best_dir="$dir"; best_key="$key"
            fi
        fi
    done
done

w=$(wrapper_for "$best_dir") && exec "$w" "$@"

echo "ndk-clang.sh: 未找到可用的 NDK clang（要求版本 >= 25.1.8937393，发现:$found）" >&2
echo "  请安装 NDK 或用 ANDROID_NDK_HOME 显式指定" >&2
exit 1
