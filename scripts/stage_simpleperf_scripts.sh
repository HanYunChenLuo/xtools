#!/usr/bin/env bash
# 把火焰图脚本集（report_html.py 及其依赖 + 指定平台的 report 库）暂存到目标目录，
# 供发布产物**随包分发**——运行时解析链的「随包资源 / 可执行文件旁」候选，
# 见 xperf-core/src/simpleperf.rs::scripts_dir（GUI 走 Tauri resource_dir 注入，
# CLI tarball 走与 xperf-cli 并排的 simpleperf_scripts/ 目录）。
#
# 用法: stage_simpleperf_scripts.sh <目标目录> <平台子目录: linux/x86_64 | darwin/x86_64>
# 在仓库根目录执行。
#
# LFS 未拉取时 bin 下只是 ~130B 的指针文本——判缺后**不带该平台库**（脚本本体照带），
# 免得把指针当库分发；运行时解析链发现不齐全就落到可写目录从 AOSP 引导下载补齐。
set -euo pipefail

dest="${1:?用法: stage_simpleperf_scripts.sh <目标目录> <平台子目录>}"
plat="${2:?缺少平台子目录（如 linux/x86_64）}"
src="xperf-core/simpleperf_scripts"

test -d "$src" || { echo "找不到 $src（须在仓库根执行）" >&2; exit 1; }
mkdir -p "$dest"
cp "$src"/*.py "$src"/*.js "$dest"/

lib="$src/bin/$plat"
big=0
if [ -d "$lib" ]; then
    for f in "$lib"/*; do
        [ -f "$f" ] || continue
        sz=$(wc -c < "$f")
        # 与 core 的 script_present 同判据：report 库 >1MB 才算存在（指针文本不算）
        if [ "${sz:-0}" -gt 1000000 ]; then big=1; break; fi
    done
fi
if [ "$big" = "1" ]; then
    mkdir -p "$dest/bin/$(dirname "$plat")"
    cp -R "$lib" "$dest/bin/$(dirname "$plat")/"
    echo "火焰图脚本集已暂存: ${dest}（含 bin/${plat}）"
else
    echo "WARN: $lib 无 >1MB 的真实 report 库（LFS 未拉取？）——只暂存脚本本体，" \
         "运行时会落到可写目录从 AOSP 引导下载补齐" >&2
fi
