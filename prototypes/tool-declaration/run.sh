#!/usr/bin/env bash
# 构建工具组件的各个变体，打包自定义段，运行全部核对。
# 构建产物写入 target/artifacts（不入库），结果写入 results/。
set -euo pipefail
cd "$(dirname "$0")"

ART=target/artifacts
rm -rf "$ART"
mkdir -p "$ART" results

# 工具随源码携带的宿主接口副本必须与节点的一致（stale-host 故意不一致）。
cmp wit/lancet-host/host.wit tools/notes/wit/deps/lancet-host/host.wit

componentize() { # <核心模块> <输出>
  wasm-tools component new "$1" -o "$2"
}

cargo build --quiet --release -p notes-tool --target wasm32-unknown-unknown
componentize target/wasm32-unknown-unknown/release/notes_tool.wasm "$ART/notes.component.wasm"

cargo build --quiet --release -p notes-tool --target wasm32-unknown-unknown --features embed-manifest
componentize target/wasm32-unknown-unknown/release/notes_tool.wasm "$ART/notes.linksection.component.wasm"

# 在另一个目标目录从头重建，比较产物与声明摘要。
CARGO_TARGET_DIR=target/rebuild cargo build --quiet --release -p notes-tool --target wasm32-unknown-unknown
componentize target/rebuild/wasm32-unknown-unknown/release/notes_tool.wasm "$ART/notes.rebuild.component.wasm"

cargo build --quiet --release -p notes-tool --target wasm32-wasip2
cp target/wasm32-wasip2/release/notes_tool.wasm "$ART/notes.wasip2.wasm"

cargo build --quiet --release -p stale-host-tool --target wasm32-unknown-unknown
componentize target/wasm32-unknown-unknown/release/stale_host_tool.wasm "$ART/stale.component.wasm"

# 带文档的二进制 WIT 包（含 package-docs 段）。
wasm-tools component wit tools/notes/wit --wasm -o "$ART/notes.witpkg.wasm"

cargo build --quiet --release -p lancet-decl
NODE=target/release/lancet-decl
"$NODE" pack "$ART/notes.component.wasm" -o "$ART/notes.packed.wasm" \
  --effects tools/notes/effects.json --wit "$ART/notes.witpkg.wasm"
wasm-tools validate "$ART/notes.packed.wasm"
wasm-tools strip "$ART/notes.packed.wasm" -o "$ART/notes.packed.strip-default.wasm"
wasm-tools strip --all "$ART/notes.packed.wasm" -o "$ART/notes.packed.strip-all.wasm"

{
  echo "# wasm-tools component wit target/artifacts/notes.packed.wasm"
  wasm-tools component wit "$ART/notes.packed.wasm"
} > results/notes.packed.wit
{
  echo "# wasm-tools objdump target/artifacts/notes.packed.wasm（节选：模块与自定义段）"
  wasm-tools objdump "$ART/notes.packed.wasm" | grep -E 'module|custom'
  echo
  echo "# wasm-tools objdump target/artifacts/notes.linksection.component.wasm（节选）"
  wasm-tools objdump "$ART/notes.linksection.component.wasm" | grep -E 'module|custom'
} > results/sections.txt
(cd "$ART" && sha256sum ./*.wasm | sed 's| \./| |') > results/artifacts.sha256
ls -l "$ART" | awk 'NR > 1 { print $5, $9 }' > results/artifacts.size

"$NODE" suite --artifacts "$ART" --manifest tools/notes/effects.json --out results
