#!/usr/bin/env bash
# 发布物组装与冒烟验收（docs/rules/packaging.md §1 布局 + §2 清单第 1/5 项）。
#
# 产出：dist/mem-v<version>-<target>/（mem.exe + builtin-types/ exe 同级，原样整目录）
#       及同名 .zip。禁止只发裸 exe（AGENTS.md 触发映射）。
# 冒烟：在发布物目录内以 exe 旁播种建临时店——验证"外部在场优先"的生产形态
#（asset_source=external）与店可用；类型内容零知识，冒烟不解释资产字节（红线1）。
#
# 未覆盖（如实）：MSI/NSIS 安装器（本机无工具链）、macOS tarball/Homebrew/
# .app（外部验收项）、正式分发渠道承诺（设计文档开放问题40 未收口）。
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
target="x86_64-pc-windows-msvc"
name="mem-v${version}-${target}"
out="dist/${name}"

echo "== cargo build --release"
cargo build --release --quiet

echo "== 组装发布物目录 $out（mem.exe + builtin-types/ 同级）"
rm -rf "$out"
mkdir -p "$out"
cp target/release/mem.exe "$out/"
cp -r assets/builtin-types "$out/builtin-types"

echo "== 冒烟：exe 旁播种（外部优先）+ 店可用"
smoke="$(mktemp -d)"
trap 'rm -rf "$smoke"' EXIT
"$out/mem.exe" --store "$smoke/store" init "$smoke/store" --principal smoke_test > "$smoke/init.json"
cat "$smoke/init.json"
grep -q '"asset_source": "external"' "$smoke/init.json" || {
    echo "冒烟失败：init 未走 exe 旁外部播种（asset_source != external）" >&2
    exit 1
}
"$out/mem.exe" --store "$smoke/store" list --limit 5 > /dev/null
echo "冒烟通过：外部播种 + 店可用"

echo "== 打 zip（PowerShell Compress-Archive）"
( cd dist && powershell -NoProfile -Command "Compress-Archive -Path '$name' -DestinationPath '${name}.zip' -Force" )

echo "完成：$out/ 与 dist/${name}.zip"
ls -la "$out" | head -5
ls -la "dist/${name}.zip"
