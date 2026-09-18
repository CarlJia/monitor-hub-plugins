---
name: create-plugin
description: "Scaffold a new monitor-hub WASM plugin (ABI v2): pick the right shape (notification / tick / page / cleanup / emit / data), write plugin.toml + Cargo.toml + src/lib.rs, build and package plugin.tar.gz."
---

# Create Plugin

Scaffold a new monitor-hub WASM plugin (ABI v2). The plugin is a Rust crate targeting `wasm32-unknown-unknown`, imports 14 host functions from `"host"`, exports `__alloc` + `on_event` (always) plus conditional exports, and is packaged as `plugin.tar.gz` for `POST /api/plugins`.

> **ABI truth source lives in the monitor repo**, not here: the ABI field table, host-function implementation, and manifest validation are at https://github.com/CarlJia/monitor (`README.md` → 「插件开发」, `src/plugin/host_funcs.rs`, `src/plugin/manifest.rs`). This skill's `references/` are a convenience copy — when they disagree with the monitor repo, the monitor repo wins.

Two reference implementations cover the full surface — start by reading one:

- [`tg-notify`](../../../tg-notify) — pure notification: subscribes to host events
- [`finance-stats`](../../../finance-stats) — composite: tick + page + cleanup + `emit_event` + `data_*`

## Branches (composable, not exclusive)

| Shape | What it does | Manifest | Required extra export |
|-------|--------------|----------|------------------------|
| Notification | Subscribe to host events, send alert | `subscribes = ["agent_offline", ...]` | — |
| Tick | Periodic background work | `tick = true` | `on_tick` |
| Page | Custom panel UI | `[page]\ntitle = "..."` | `render_page`, `on_action` |
| Cleanup | Operator-triggered housekeeping | `cleanup = true` | `on_cleanup` |
| Emit | Publishes events for other plugins | uses `host_emit_event` | — |
| Data | Plugin-owned persistent state | uses `data_put/get/delete/list` | — |

**Working-surface rule:** at least one of `subscribes` (non-empty) / `tick` / `page` / `cleanup` MUST be present. Declaring only `[[kv]]` is rejected — kv is the panel config UI hint, it has no runtime call site.

## Workflow

1. **Anchor the use case.** Ask: what triggers this plugin (event / tick / page), what does it do, where does it store state, does it talk to the outside (`http_get`/`http_post`), does it notify?
2. **Pick the closest reference.** `tg-notify` for notification-only, `finance-stats` for everything else.
3. **Choose `plugin_id`.** Reverse-domain (`io.github.<user>.<name>`). Non-empty, no `:` (it's the kv namespace separator).
4. **Write `<id>/plugin.toml`.** See [`references/manifest-schema.md`](references/manifest-schema.md). Run [`scripts/validate.sh`](scripts/validate.sh) `<id>` early.
5. **Write `<id>/Cargo.toml`.** Copy from the matching reference. Change `name` (use `_` not `-` — must be a valid Rust identifier) and `[lib].name`.
6. **Write `<id>/src/lib.rs`.** Compose from [`references/examples.md`](references/examples.md) — pick one section per shape you need, plus the always-on allocator + bytes helpers.
7. **Add `<id>/build.sh`** — copy from `tg-notify/build.sh`, change the wasm output filename (must match `[lib].name` with `-` → `_`, plus `.wasm`).
8. **Build.** `cd <id> && ./build.sh` → produces `plugin.tar.gz` (contains `plugin.toml` + `plugin.wasm`).
9. **Verify.** `scripts/validate.sh <id>` for manifest checks, then `cargo test` inside the plugin dir for the wasmtime smoke test.
10. **Done when:** `plugin.tar.gz` exists, manifest passes validation, smoke test passes, every declared export exists with the right signature.

## Hard rules (don't break these)

- **Events you emit** MUST start with `plugin_` (else `host_emit_event` returns -7).
- **Events the host emits** are only `agent_offline` / `agent_online`. The host retired `expiry_soon`; expiry notifications come from `finance-stats` via `plugin_expiry_soon`.
- **HTTP targets** MUST be `https://` AND must not resolve to private / reserved ranges (else -2 / -9). Only plugin-originated requests are gated — the host's own GitHub / theme proxy is not.
- **Plugin owns its data.** Store under `plugin_data` via `data_*`. The host's node table is read-only to plugins.
- **Page UI strings** live in the plugin. The panel doesn't recognize business semantics — define one source of truth (e.g. `CURRENCIES` / `CYCLE_MONTHS`) and derive labels, values, and totals from it. Don't replicate per-renderer.
- **kv vs plugin_data.** `kv` is shared with the panel config editor (`plugin.<plugin_id>:<key>` namespace; plugin reads with raw key, host adds the prefix); `plugin_data` is the plugin's own store (raw key, host scopes by `plugin_id`).

## References

- [`references/abi-v2.md`](references/abi-v2.md) — host functions, error codes, exports, limits
- [`references/manifest-schema.md`](references/manifest-schema.md) — `plugin.toml` fields with validation rules
- [`references/page-protocol.md`](references/page-protocol.md) — JSON UI blocks for `render_page` / `on_action`
- [`references/examples.md`](references/examples.md) — five minimal compilable skeletons
- [`scripts/validate.sh`](scripts/validate.sh) — manifest parse + sanity