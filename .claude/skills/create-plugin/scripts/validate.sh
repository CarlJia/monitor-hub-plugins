#!/bin/sh
# validate.sh — sanity-check a plugin's manifest before building.
# Usage: scripts/validate.sh <plugin-dir>
#
# Runs the same shape checks the host's Manifest::parse runs. Not a
# substitute for cargo build (catches typos, missing exports, mismatched
# signatures), but it catches the manifest mistakes in seconds rather than
# after a 30 s wasm build.
set -e

dir="${1:?usage: $0 <plugin-dir>}"
toml="$dir/plugin.toml"

if [ ! -f "$toml" ]; then
    echo "FAIL: $toml 不存在"
    exit 1
fi

fail() { echo "FAIL: $1"; exit 1; }

# abi_version 必须为 2
abi=$(grep -E '^abi_version[[:space:]]*=' "$toml" | sed -E 's/.*=[[:space:]]*([0-9]+).*/\1/')
if [ "$abi" != "2" ]; then
    fail "abi_version 必须为 2（当前 ${abi:-空}）"
fi

# plugin_id 非空、不含 ':'
pid=$(grep -E '^plugin_id[[:space:]]*=' "$toml" | sed -E 's/.*=[[:space:]]*"([^"]+)".*/\1/')
if [ -z "$pid" ]; then
    fail "plugin_id 为空"
fi
case "$pid" in
    *:*) fail "plugin_id 不能包含 ':'（$pid）" ;;
esac

# name / version 非空
for k in name version; do
    v=$(grep -E "^$k[[:space:]]*=" "$toml" | sed -E 's/.*=[[:space:]]*"([^"]+)".*/\1/')
    if [ -z "$v" ]; then
        fail "$k 为空"
    fi
done

# subscribes 里宿主事件必须是 agent_offline/agent_online，其他必须 plugin_ 前缀。
# 内联数组以行尾 ']' 收尾，多行数组以单独的 ']' 行收尾,两种都得识别。
subs=$(awk '
    /^subscribes[[:space:]]*=/ { start = 1 }
    start { print }
    start && (/^]/ || /\]$/) { exit }
' "$toml" | grep -oE '"[a-zA-Z_][a-zA-Z0-9_]*"' | sed 's/"//g')
for s in $subs; do
    case "$s" in
        agent_offline|agent_online) ;;
        plugin_*) ;;
        *) fail "subscribes 含未知事件 '$s'" ;;
    esac
done

# tick / page / cleanup 至少要有一个工作面
has_work=0
[ -n "$subs" ] && has_work=1
grep -qE '^tick[[:space:]]*=[[:space:]]*true' "$toml" && has_work=1
grep -qE '^\[page\]' "$toml" && has_work=1
grep -qE '^cleanup[[:space:]]*=[[:space:]]*true' "$toml" && has_work=1
if [ "$has_work" = "0" ]; then
    fail "subscribes / tick / page / cleanup 至少要有一个工作面"
fi

# [[kv]] key 形状检查:非空、不含 ':'、不重复(同一份 manifest)
keys=$(awk '/^\[\[kv\]\]/{flag=1;next} /^\[/{flag=0} flag==1' "$toml" \
    | grep -E '^key[[:space:]]*=' \
    | sed -E 's/.*=[[:space:]]*"([^"]+)".*/\1/')
dup=$(echo "$keys" | sort | uniq -d)
if [ -n "$dup" ]; then
    fail "[[kv]] 含重复 key:$dup"
fi
for k in $keys; do
    case "$k" in
        *:*) fail "[[kv]] key '$k' 不能包含 ':'" ;;
    esac
    len=${#k}
    if [ "$len" -gt 128 ]; then
        fail "[[kv]] key '$k' 长度 $len 超过 128 字节"
    fi
done

# page.title 非空(若 [page] 存在)
if grep -qE '^\[page\]' "$toml"; then
    title=$(grep -E '^title[[:space:]]*=' "$toml" | sed -E 's/.*=[[:space:]]*"([^"]+)".*/\1/')
    if [ -z "$title" ]; then
        fail "[page].title 为空"
    fi
fi

echo "OK: $pid manifest 通过校验"