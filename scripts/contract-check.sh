#!/bin/sh
# 用一个插件的 wasm 过 monitor 的**真宿主契约**：读 plugin.toml 的 abi_version，
# 取 monitor 仓上对应主版本的 monitor-plugin-contract tag，把那版契约 crate 加成本
# 插件的 dev-dependency，跑 tests/contract.rs，然后还原 Cargo.toml。
#
# ci.yml（每个 PR）与 release.yml（每次发版）都只调这一个脚本——两条路径的校验
# 命令逐字相同，不再靠两处手抄保持一致。tag 规则也只有这一份。
#
# 用法（在仓库根执行）：scripts/contract-check.sh <plugin-dir>
# 前置：<plugin-dir>/plugin.wasm 已由 `./build.sh` 产出。
set -eu

plugin=${1:?用法: scripts/contract-check.sh <plugin-dir>}
[ -f "$plugin/plugin.toml" ] || { echo "::error::$plugin/plugin.toml 不存在"; exit 1; }
[ -f "$plugin/plugin.wasm" ] || { echo "::error::$plugin/plugin.wasm 不存在——先跑 $plugin/build.sh"; exit 1; }

cd "$plugin"

abi=$(grep -oE '^abi_version *= *[0-9]+' plugin.toml | grep -oE '[0-9]+$' || true)
[ -n "$abi" ] || { echo "::error::$plugin/plugin.toml 没有数字 abi_version"; exit 1; }

# 唯一的契约 tag 真源。只接受严格的 `v<abi>[.<n>...]`：`sort -V` 会把带后缀的
# 预发布 tag 排在同版本正式 tag 之后（v2.0.0-rc1 > v2.0.0），所以不过滤的话，
# 一个被 monitor 的 publish-contract.yml 判失败、又不会回滚的 -rc tag 会永远
# 压住正式 tag 被选走。monitor 侧要求 tag 与 crate version 逐字相等，正式 tag
# 天然是纯数字点分形式。
tag=$(git ls-remote --tags --refs https://github.com/CarlJia/monitor.git \
        "monitor-plugin-contract-v${abi}.*" \
      | awk -F/ '{print $NF}' \
      | grep -E "^monitor-plugin-contract-v${abi}\.[0-9]+(\.[0-9]+)*$" \
      | sort -V | tail -1)
[ -n "$tag" ] || { echo "::error::ABI ${abi} 没有正式的 monitor-plugin-contract tag（只认 monitor-plugin-contract-v${abi}.<数字>.<数字>）"; exit 1; }

echo "validating $plugin against $tag"
cargo add --git https://github.com/CarlJia/monitor monitor-plugin-contract --tag "$tag" --dev

# 不加 --release：被测物是 build.sh 产出的 release wasm，这里编的是宿主侧测试
# 二进制，优化级别不改变被测对象，却要在冷缓存下多花约 150s/插件。Cargo.lock 在
# 插件目录下被 gitignore，只有 Cargo.toml 是 tracked，所以只还原它——无论测试
# 成败都还原，走后一条路径就是脏树。
status=0
cargo test --features contract --test contract || status=$?
git checkout -- Cargo.toml
exit $status
