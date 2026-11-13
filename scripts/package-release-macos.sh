#!/usr/bin/env bash
# macOS 发布物组装与冒烟验收（mem + exe 同级 builtin-types/ + tar.gz）。
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ "$(uname -s)" != Darwin ]]; then
    echo "此脚本只能在 macOS 上运行" >&2
    exit 2
fi

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
target=$(rustc -vV | sed -n 's/^host: //p')
case "$target" in
    aarch64-apple-darwin|x86_64-apple-darwin) ;;
    *) echo "不支持的 macOS 构建目标：${target}" >&2; exit 2 ;;
esac

name="mem-v${version}-${target}"
dist="${DIST_DIR:-dist}"
out="$dist/$name"
archive="$dist/$name.tar.gz"
mkdir -p "$dist"
if [[ -e "$out" || -e "$archive" ]]; then
    echo "发布物已存在；为避免覆盖，请先移走：$out 或 $archive" >&2
    exit 2
fi

stage=$(mktemp -d "${TMPDIR:-/tmp}/mem-release.XXXXXX")
trap 'rm -r "$stage"' EXIT
package="$stage/$name"
mkdir -p "$package"

echo "== cargo build --release --bin mem (${target})"
cargo build --release --bin mem --quiet

echo "== 组装发布物 ${name}（mem + builtin-types/）"
cp target/release/mem "$package/mem"
cp -R assets/builtin-types "$package/builtin-types"

echo "== 冒烟：外部资产优先 + 初始化后的店可用"
"$package/mem" --store "$stage/store" init "$stage/store" --principal smoke_test > "$stage/init.json"
cat "$stage/init.json"
grep -q '"asset_source": "external"' "$stage/init.json" || {
    echo "冒烟失败：init 未走包内 builtin-types/（asset_source != external）" >&2
    exit 1
}
"$package/mem" --store "$stage/store" list --limit 5 > /dev/null

(cd "$stage" && tar -czf "$name.tar.gz" "$name")
mv "$package" "$out"
mv "$stage/$name.tar.gz" "$archive"

echo "完成：${out}/ 与 ${archive}"
ls -lh "$out/mem" "$archive"
