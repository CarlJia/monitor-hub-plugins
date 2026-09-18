# Examples — five minimal compilable skeletons

Each section below is the **minimum** needed for one shape. Real plugins grow from here. The bump allocator + bytes helpers are universal — every plugin needs them.

## Universal bits — copy into every plugin

```rust
// src/lib.rs (top — every plugin needs this bump allocator + bytes helpers)

use std::ptr;

static mut HEAP_NEXT: usize = 1024;

/// 简单 bump 分配器。每次派发重建 Store 与此全局,本插件零泄漏到下一次调用。
pub extern "C" fn __alloc(cap: i32) -> i32 {
    if cap <= 0 {
        return 0;
    }
    unsafe {
        let p = HEAP_NEXT;
        let size = (cap as usize + 3) & !3; // 4 字节对齐
        HEAP_NEXT += size;
        p as i32
    }
}

fn write_bytes(bytes: &[u8]) -> (i32, i32) {
    let ptr = __alloc(bytes.len().max(1) as i32);
    if ptr <= 0 { return (0, 0); }
    unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len()) };
    (ptr, bytes.len() as i32)
}

fn write_str(s: &str) -> (i32, i32) { write_bytes(s.as_bytes()) }

/// 把宿主写入的字节当 UTF-8 读回;非法 UTF-8 走有损转换。
fn bytes_to_string(buf: &[u8]) -> String {
    String::from_utf8_lossy(buf).into_owned()
}

unsafe fn read_slice<'a>(ptr: i32, len: i32) -> Option<&'a [u8]> {
    if ptr <= 0 || len <= 0 { return None; }
    Some(std::slice::from_raw_parts(ptr as *const u8, len as usize))
}
```

## 1. Notification-only — subscribes to host events

`Cargo.toml` — copy from `tg-notify/Cargo.toml`, change `name` and `lib.name`.

```toml
plugin_id = "com.example.my-notify"
name = "My Notify"
version = "0.1.0"
abi_version = 2
subscribes = ["agent_offline", "agent_online"]

[[kv]]
key = "webhook_url"
required = true
hint = "https://...,必须是公网 https"
```

```rust
// src/lib.rs — notification only
use serde::Deserialize;
use serde_json::json;

#[link(wasm_import_module = "host")]
extern "C" {
    #[link_name = "log"] fn host_log(level: i32, ptr: i32, len: i32);
    #[link_name = "kv_get"] fn host_kv_get(kp: i32, kl: i32, op: i32, oc: i32) -> i32;
    #[link_name = "http_post"] fn host_http_post(
        mp: i32, ml: i32, up: i32, ul: i32, bp: i32, bl: i32, rp: i32, rc: i32,
    ) -> i32;
}

// ... universal bits above ...

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    AgentOffline { node_id: i64, name: String, last_seen_at: i64 },
    AgentOnline  { node_id: i64, name: String },
}

fn kv_string(key: &str) -> Option<String> {
    let (kp, kl) = write_str(key);
    let mut buf = [0u8; 512];
    let n = unsafe { host_kv_get(kp, kl, buf.as_mut_ptr() as i32, buf.len() as i32) };
    if n <= 0 { return None; }
    Some(bytes_to_string(&buf[..n as usize]))
}

fn log(level: i32, msg: &str) {
    let (p, l) = write_str(msg);
    unsafe { host_log(level, p, l) };
}

#[no_mangle]
pub extern "C" fn on_event(ptr: i32, len: i32) -> i32 {
    let Some(payload) = (unsafe { read_slice(ptr, len) }) else { return 1 };
    let event: Event = match serde_json::from_slice(payload) {
        Ok(e) => e,
        Err(_) => { log(3, "事件载荷不是合法 JSON"); return 1; }
    };
    let Some(url) = kv_string("webhook_url") else {
        log(2, "kv 里没有 webhook_url");
        return 2;
    };
    // 拼 body 并 http_post
    let body = json!({"event": event});
    let body = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".into());
    let (mp, ml) = write_str("POST");
    let (up, ul) = write_str(&url);
    let (bp, bl) = write_bytes(&body);
    let resp_ptr = __alloc(4096);
    let n = unsafe { host_http_post(mp, ml, up, ul, bp, bl, resp_ptr, 4096) };
    if n < 0 { log(3, &format!("http_post 失败 {n}")); return 10 - n; }
    0
}
```

## 2. Tick — periodic background work

```toml
plugin_id = "io.github.<user>.hourly-thing"
name = "Hourly Thing"
version = "0.1.0"
abi_version = 2
subscribes = []
tick = true
```

```rust
#[link(wasm_import_module = "host")]
extern "C" {
    #[link_name = "log"] fn host_log(level: i32, ptr: i32, len: i32);
    #[link_name = "now"] fn host_now() -> i64;
}

fn log(level: i32, msg: &str) {
    let (p, l) = write_str(msg);
    unsafe { host_log(level, p, l) };
}

#[no_mangle]
pub extern "C" fn on_tick() -> i32 {
    let now = unsafe { host_now() };
    log(1, &format!("hourly-thing: tick at {now}"));
    0
}

/// tick 插件不需要订阅事件,保留导出满足 ABI 契约。
#[no_mangle]
pub extern "C" fn on_event(_ptr: i32, _len: i32) -> i32 { 0 }
```

## 3. Page — custom panel UI

The full finance-stats `render_page` / `on_action` is the reference. Minimal:

```toml
plugin_id = "io.github.<user>.hello-page"
name = "Hello Page"
version = "0.1.0"
abi_version = 2
subscribes = []
[page]
title = "Hello"
```

```rust
use serde_json::{json, Value};

#[link(wasm_import_module = "host")]
extern "C" {
    #[link_name = "log"] fn host_log(level: i32, ptr: i32, len: i32);
    #[link_name = "resp_alloc"] fn host_resp_alloc(cap: i32) -> i32;
}

fn build_page() -> Value {
    json!({
        "title": "Hello",
        "blocks": [
            { "type": "stat", "items": [
                { "label": "欢迎", "value": "这是一个示例页面" }
            ]}
        ]
    })
}

fn respond(v: &Value) -> i32 {
    let bytes = serde_json::to_vec(v).unwrap_or_else(|_| b"{}".into());
    let cap = bytes.len() as i32;
    let ptr = unsafe { host_resp_alloc(cap) };
    if ptr <= 0 { return -1; }
    unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len()) };
    cap
}

#[no_mangle]
pub extern "C" fn on_event(_ptr: i32, _len: i32) -> i32 { 0 }

#[no_mangle]
pub extern "C" fn render_page(_ptr: i32, _len: i32) -> i32 {
    respond(&build_page())
}

#[no_mangle]
pub extern "C" fn on_action(_ptr: i32, _len: i32) -> i32 {
    respond(&build_page())
}
```

## 4. Cleanup

```toml
plugin_id = "io.github.<user>.prune"
name = "Prune"
version = "0.1.0"
abi_version = 2
subscribes = []
cleanup = true
```

```rust
use serde_json::json;

#[link(wasm_import_module = "host")]
extern "C" {
    #[link_name = "data_list"] fn host_data_list(pp: i32, pl: i32, op: i32, oc: i32) -> i32;
    #[link_name = "data_delete"] fn host_data_delete(kp: i32, kl: i32) -> i32;
    #[link_name = "resp_alloc"] fn host_resp_alloc(cap: i32) -> i32;
}

fn list(prefix: &str) -> Vec<String> {
    let (pp, pl) = write_str(prefix);
    let mut buf = vec![0u8; 64 * 1024];
    let n = unsafe { host_data_list(pp, pl, buf.as_mut_ptr() as i32, buf.len() as i32) };
    if n <= 0 { return vec![]; }
    let v: serde_json::Value = serde_json::from_slice(&buf[..n as usize]).unwrap_or_default();
    v.as_array().map(|a| a.iter().filter_map(|x| x.get("key").and_then(|k| k.as_str()).map(String::from)).collect()).unwrap_or_default()
}

#[no_mangle]
pub extern "C" fn on_event(_ptr: i32, _len: i32) -> i32 { 0 }

#[no_mangle]
pub extern "C" fn on_cleanup(_ptr: i32, _len: i32) -> i32 {
    let mut pruned = 0i64;
    for key in list("stale:") {
        let (kp, kl) = write_str(&key);
        if unsafe { host_data_delete(kp, kl) } == 0 { pruned += 1; }
    }
    let body = json!({ "pruned": pruned });
    let bytes = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".into());
    let cap = bytes.len() as i32;
    let ptr = unsafe { host_resp_alloc(cap) };
    if ptr <= 0 { return -1; }
    unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len()) };
    cap
}
```

## 5. Composite (emit_event + data_* + tick)

Read `finance-stats/src/lib.rs` directly — it's the canonical example. The flow:

1. `on_tick`: refresh data + scan + `host_emit_event("plugin_xxx", payload)` for items needing attention.
2. `data_put` / `data_get` for plugin-owned state.
3. `nodes_query` for host node list.
4. `render_page` reads `data_get` and emits a UI; `on_action` mutates data.
5. `on_cleanup` reconciles `data_list("node:")` against `nodes_query` and prunes orphans.

Key constraints:

- `data_put` keys are plugin-scoped — host adds the `plugin_id` scope for you. Don't include it yourself.
- `data_list("node:")` returns a JSON array of `{key, data}` strings — `data` is itself a JSON-encoded string, parse again.
- `nodes_query` returns `{id, name, online}` — that's it. No financial columns, no expiry dates — those are the plugin's job.
- `emit_event` name MUST start with `plugin_` (else -7). Payload must be JSON-serializable.

## Cargo.toml skeleton (any shape)

```toml
[package]
name = "my_plugin"          # 用 `_`,不是 `-`(lib name 必须合法 Rust 标识符)
version = "0.1.0"
edition = "2021"
license = "MIT"
description = "..."
publish = false

[lib]
crate-type = ["cdylib"]
name = "my_plugin"          # 必须与 [package].name 一致;wasm 文件名按这个拼

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[dev-dependencies]
wasmtime = { version = "48", default-features = false, features = ["cranelift", "runtime", "std", "anyhow"] }

[profile.release]
opt-level = "s"
lto = true
codegen-units = 1
strip = true
```

## build.sh skeleton

```sh
#!/bin/sh
set -e
cd "$(dirname "$0")"
cargo build --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/my_plugin.wasm plugin.wasm   # 用 [lib].name
tar czf plugin.tar.gz plugin.toml plugin.wasm
echo "built plugin.tar.gz (含 plugin.toml + plugin.wasm)"
```

One-time prerequisite: `rustup target add wasm32-unknown-unknown`.