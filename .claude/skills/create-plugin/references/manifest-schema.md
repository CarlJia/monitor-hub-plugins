# plugin.toml — Manifest schema

Authoritative validator: `src/plugin/manifest.rs::Manifest::parse`. The panel's `POST /api/plugins` rejects any of the rules below with a 400 carrying the reason.

## Fields

| Field | Required | Type | Validation |
|-------|----------|------|------------|
| `plugin_id` | yes | string | non-empty, no `:` (it's the kv namespace separator `plugin.<plugin_id>:<key>`) |
| `name` | yes | string | non-empty (panel display name) |
| `version` | yes | string | non-empty |
| `abi_version` | yes | integer | must equal `2` |
| `subscribes` | no | string[] | ≤ 32 entries; host events must be `agent_offline` / `agent_online`, plugin events must start with `plugin_` followed by a non-empty suffix |
| `tick` | no | bool | when `true`, plugin must export `on_tick` |
| `[page].title` | no | string | non-empty; plugin must export `render_page` and `on_action` |
| `cleanup` | no | bool | when `true`, plugin must export `on_cleanup` |
| `wasm_entry` | no | string | default `plugin.wasm` |
| `[[kv]]` | no | array of `{key, label?, required?, hint?}` | ≤ 64 entries; keys unique, non-empty, no `:`, ≤ 128 bytes, no leading/trailing whitespace |

## Working-surface rule

At least one of these MUST be present:

- `subscribes` non-empty
- `tick = true`
- `[page]` with non-empty `title`
- `cleanup = true`

Declaring only `[[kv]]` is rejected — kv is the panel UI / preflight hint, not a runtime call site.

## Event vocabulary (v2)

**Host-emitted** (only these — `expiry_soon` was retired):

- `agent_offline` — `{"type":"agent_offline","node_id","name","observed_at","last_seen_at"}`
- `agent_online` — `{"type":"agent_online","node_id","name","observed_at"}`

**Plugin-emitted** (the host cannot know the full set):

- any name starting with `plugin_` followed by a non-empty suffix (no white-list, no length cap beyond "non-empty suffix")

Other plugins subscribe to your `plugin_*` events by listing them in their own `subscribes`.

## KV namespace

`plugin.<plugin_id>:<key>` — host adds the prefix automatically. The plugin reads with `host_kv_get("bot_token", ...)`, not `host_kv_get("plugin.com.example.tg-notify:bot_token", ...)`.

KV and `plugin_data` are separate tables. Use `kv` for operator-edited config (panel UI exposes it). Use `plugin_data` for plugin-owned state — host scopes by `plugin_id`, plugin uses raw keys.

## Working examples

Two real plugins cover the full field set:

```toml
# tg-notify/plugin.toml — pure notification
plugin_id = "com.example.tg-notify"
name = "Telegram 通知"
version = "0.3.0"
abi_version = 2
subscribes = ["agent_offline", "agent_online", "plugin_expiry_soon"]

[[kv]]
key = "bot_token"
label = "Telegram Bot Token"
required = true
hint = "向 @BotFather 申请，形如 123456:ABC-DEF"

[[kv]]
key = "chat_id"
label = "目标会话 ID"
required = true
```

```toml
# finance-stats/plugin.toml — composite (tick + page + cleanup + emit)
plugin_id = "io.github.monitor.finance-stats"
name = "财务统计"
version = "0.2.0"
abi_version = 2
subscribes = []          # 工作面在 tick/page/cleanup,这里允许空
tick = true
cleanup = true

[page]
title = "财务统计"
```

## Validation script

`scripts/validate.sh <plugin-dir>` runs the same shape checks the host does (well — the host checks more, but this catches the common mistakes before you build).