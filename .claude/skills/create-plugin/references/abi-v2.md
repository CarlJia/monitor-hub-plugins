# ABI v2 — host functions, exports, limits

## Module target

- `wasm32-unknown-unknown`, `crate-type = ["cdylib"]`
- `std` works (no system calls: no network, no filesystem, no clock)
- Release profile: `opt-level = "s"`, `lto = true`, `codegen-units = 1`, `strip = true`

## Required exports

| Export | Signature | Notes |
|--------|-----------|-------|
| `memory` | linear memory | All pointers land here |
| `__alloc` | `(cap: i32) -> i32` | Bump allocator. Start heap at 1024, return pointer. Return 0 when `cap <= 0` |
| `on_event` | `(ptr: i32, len: i32) -> i32` | Event entry. Return 0 on success, non-zero = plugin-defined error |

## Conditional exports (declared by manifest)

| Export | Signature | Required when |
|--------|-----------|---------------|
| `on_tick` | `() -> i32` | `tick = true` |
| `render_page` | `(ptr: i32, len: i32) -> i32` | `[page].title = "..."` |
| `on_action` | `(ptr: i32, len: i32) -> i32` | `[page].title = "..."` |
| `on_cleanup` | `(ptr: i32, len: i32) -> i32` | `cleanup = true` |

`render_page` / `on_action` write JSON to a buffer via `host_resp_alloc(cap)` and return the byte count. JSON shape: see `references/page-protocol.md`.

## Host functions (import from `"host"`)

Rust side uses `#[link(wasm_import_module = "host")]` + `#[link_name = "..."]`.

| Function | Signature | Returns |
|----------|-----------|---------|
| `host_log` | `(level: i32, ptr, len)` | — |
| `host_now` | `() -> i64` | Unix seconds |
| `host_kv_get` | `(key_ptr, key_len, out_ptr, out_cap) -> i32` | bytes written (0 = no value, -1 = bounds/UTF-8, -8 = db error) |
| `host_kv_set` | `(key_ptr, key_len, val_ptr, val_len) -> i32` | 0 ok, -1 bounds/> 8 KiB, -2 / -8 db error |
| `host_resp_alloc` | `(cap: i32) -> i32` | pointer into plugin memory |
| `host_http_post` | `(method_ptr, method_len, url_ptr, url_len, body_ptr, body_len, resp_ptr, resp_cap) -> i32` | bytes written |
| `host_http_get` | `(url_ptr, url_len, resp_ptr, resp_cap) -> i32` | bytes written |
| `host_nodes_query` | `(out_ptr, out_cap) -> i32` | bytes (JSON array of `{id, name, online}`) |
| `host_emit_event` | `(name_ptr, name_len, payload_ptr, payload_len) -> i32` | 0 ok, -7 if name doesn't start with `plugin_`, -8 db error |
| `host_data_put` | `(key_ptr, key_len, val_ptr, val_len) -> i32` | 0 ok, -6 record>256 KiB or plugin>16 MiB, -8 db error |
| `host_data_get` | `(key_ptr, key_len, out_ptr, out_cap) -> i32` | bytes written (0 = no record, -8 db error) |
| `host_data_delete` | `(key_ptr, key_len) -> i32` | 0 ok |
| `host_data_list` | `(prefix_ptr, prefix_len, out_ptr, out_cap) -> i32` | bytes (JSON array of `{key, data}`) |

## Uniform error codes

| Code | Meaning |
|------|---------|
| -1 | Memory bounds / invalid UTF-8 / exceeded / `__alloc` missing or failed |
| -2 | `http_*`: not https. `kv_set`: db write failed |
| -3 | `http_post`: method not POST |
| -4 | `http_*`: failed network / timeout / not in async runtime context |
| -5 | `http_*`: non-2xx response |
| -6 | `data_*`: per-record 256 KiB or per-plugin 16 MiB quota exceeded |
| -7 | `emit_event`: name doesn't start with `plugin_` |
| -8 | db error: `kv_get` / `nodes_query` / `emit_event` / `data_*` |
| -9 | `http_*`: target resolves to private / reserved range (SSRF guard, plugins only) |

Successful reads (`kv_get`, `http_*`, `data_get`, `data_list`, `nodes_query`) return the byte count written. Other successful calls return 0.

## Resource limits

| Limit | Value |
|-------|-------|
| Fuel per dispatch | 1,000,000 (hook: 20,000,000) |
| KV key length | ≤ 128 bytes, non-empty, no `:`, no leading/trailing whitespace |
| KV value length | ≤ 8 KiB |
| HTTP timeout | 4 s wall-clock |
| HTTP response | ≤ 64 KiB (excess truncated by host) |
| `plugin_data` single record | ≤ 256 KiB |
| `plugin_data` per plugin | ≤ 16 MiB |
| `manifest.subscribes` entries | ≤ 32 |
| `manifest.[[kv]]` entries | ≤ 64 |

## Lifecycle

Every event / tick / page / cleanup call builds a fresh `Store` with a fresh `host_resp_alloc` buffer and a fresh bump-allocator base. A plugin that runs out of fuel mid-call gets cut off, but the next call starts clean — no leak across calls.