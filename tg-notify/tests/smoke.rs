//! 端到端冒烟测试（U8）：先 `cargo build` 出真实的 wasm 产物，再用 wasmtime
//! 复刻 hub 的宿主环境（本插件用到的那几个宿主函数 + fuel 限额，语义照抄
//! monitor 仓 `src/plugin/host_funcs.rs` 的 host_linker），把模块加载起来驱动
//! 三种事件。
//!
//! 这是插件级的真实验证：不 mock 插件代码，只 mock 宿主——与生产路径的差别
//! 仅在于 http 请求不走网络、kv 不落 SQLite。
//!
//! 嵌套 cargo build 用独立的 `--target-dir`（target/smoke）：外层 `cargo test`
//! 持有 target/ 的构建锁，共用一个目录会互相等待。

use std::collections::HashMap;
use std::process::Command;
use std::sync::Mutex;

use wasmtime::{Caller, Config, Engine, Extern, Linker, Memory, Module, Store};

/// 一段合法的 sendMessage 成功响应，桩宿主把它写回 resp 缓冲。
const FAKE_RESPONSE: &[u8] = br#"{"ok":true,"result":{"message_id":42}}"#;

/// 一次被记录的 http 调用。
#[derive(Debug, Clone)]
struct HttpCall {
    method: String,
    url: String,
    body: String,
}

/// 桩宿主的全部状态：kv 表、http 调用记录、日志记录。
#[derive(Default)]
struct Host {
    kv: HashMap<String, String>,
    http_calls: Mutex<Vec<HttpCall>>,
    logs: Mutex<Vec<(i32, String)>>,
}

/// 与 src/plugin.rs 的默认限额一致：正常插件应远用不完。
const FUEL_LIMIT: u64 = 1_000_000;

/// 构建真实的 wasm 产物并读回字节。
fn build_wasm() -> Vec<u8> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let target_dir = format!("{manifest_dir}/target/smoke");
    let out = Command::new("cargo")
        .args(["build", "--release", "--target", "wasm32-unknown-unknown", "--target-dir", &target_dir])
        .current_dir(manifest_dir)
        .output()
        .expect("cargo build 应能启动");
    assert!(out.status.success(), "wasm 构建失败：\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::read(format!("{target_dir}/wasm32-unknown-unknown/release/tg_notify.wasm"))
        .expect("应产出 tg_notify.wasm")
}

fn engine() -> Engine {
    // fuel 必须在 Config 上开启，与生产引擎一致（src/plugin.rs::new_engine）。
    let mut config = Config::new();
    config.consume_fuel(true);
    Engine::new(&config).unwrap()
}

// ---- 内存读写工具：先检查后拷贝，与 src/plugin.rs 的 read_mem/write_mem 同思路 ----

fn read_mem(caller: &mut Caller<'_, Host>, ptr: i32, len: i32) -> Option<Vec<u8>> {
    if ptr <= 0 || len < 0 {
        return None;
    }
    let mem = caller.get_export("memory")?.into_memory()?;
    let data = mem.data(&*caller);
    let start = ptr as usize;
    let end = start.checked_add(len as usize)?;
    Some(data.get(start..end)?.to_vec())
}

fn read_text(caller: &mut Caller<'_, Host>, ptr: i32, len: i32) -> Option<String> {
    String::from_utf8(read_mem(caller, ptr, len)?).ok()
}

fn write_mem(caller: &mut Caller<'_, Host>, ptr: i32, bytes: &[u8]) -> bool {
    if ptr <= 0 {
        return false;
    }
    let Some(mem) = caller.get_export("memory").and_then(Extern::into_memory) else {
        return false;
    };
    let start = ptr as usize;
    let Some(end) = start.checked_add(bytes.len()) else {
        return false;
    };
    let data = mem.data_mut(&mut *caller);
    let Some(target) = data.get_mut(start..end) else {
        return false;
    };
    target.copy_from_slice(bytes);
    true
}

/// 注册 6 个宿主函数并实例化，返回 (store, instance)。on_event 由调用方经
/// `send_event` 驱动。
fn instantiate(engine: &Engine, wasm: &[u8], host: Host) -> (Store<Host>, wasmtime::Instance) {
    let module = Module::new(engine, wasm).expect("wasm 应能编译");
    let mut store = Store::new(engine, host);
    store.set_fuel(FUEL_LIMIT).unwrap();

    let mut linker: Linker<Host> = Linker::new(engine);

    linker
        .func_wrap("host", "log", |mut caller: Caller<'_, Host>, level: i32, ptr: i32, len: i32| {
            let text = read_text(&mut caller, ptr, len).unwrap_or_default();
            caller.data().logs.lock().unwrap().push((level, text));
        })
        .unwrap();

    linker.func_wrap("host", "now", || -> i64 { 1_800_000_000 }).unwrap();

    linker
        .func_wrap(
            "host",
            "kv_get",
            |mut caller: Caller<'_, Host>, key_ptr: i32, key_len: i32, out_ptr: i32, out_cap: i32| -> i32 {
                let Some(key) = read_text(&mut caller, key_ptr, key_len) else { return -1 };
                // 先把值 clone 出来：write_mem 需要 &mut caller，与 data() 的
                // 不可变借用冲突。
                let value = caller.data().kv.get(&key).cloned();
                let Some(value) = value else { return 0 };
                if out_ptr <= 0 || out_cap < 0 {
                    return -1;
                }
                let bytes = &value.as_bytes()[..value.len().min(out_cap as usize)];
                if !write_mem(&mut caller, out_ptr, bytes) {
                    return -1;
                }
                bytes.len() as i32
            },
        )
        .unwrap();

    linker
        .func_wrap(
            "host",
            "kv_set",
            |mut caller: Caller<'_, Host>, key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32| -> i32 {
                let Some(key) = read_text(&mut caller, key_ptr, key_len) else { return -1 };
                let Some(value) = read_text(&mut caller, val_ptr, val_len) else { return -1 };
                caller.data_mut().kv.insert(key, value);
                0
            },
        )
        .unwrap();

    // 与生产宿主相同：回调模块自己的 __alloc 拿响应缓冲。
    linker
        .func_wrap("host", "resp_alloc", |mut caller: Caller<'_, Host>, cap: i32| -> i32 {
            if cap <= 0 {
                return -1;
            }
            let Some(func) = caller.get_export("__alloc").and_then(Extern::into_func) else {
                return -1;
            };
            let Ok(typed) = func.typed::<(i32,), i32>(&caller) else { return -1 };
            typed.call(&mut caller, (cap,)).unwrap_or(-1)
        })
        .unwrap();

    linker
        .func_wrap(
            "host",
            "http_post",
            |mut caller: Caller<'_, Host>,
             method_ptr: i32,
             method_len: i32,
             url_ptr: i32,
             url_len: i32,
             body_ptr: i32,
             body_len: i32,
             resp_ptr: i32,
             resp_cap: i32|
             -> i32 {
                let Some(method) = read_text(&mut caller, method_ptr, method_len) else { return -1 };
                let Some(url) = read_text(&mut caller, url_ptr, url_len) else { return -1 };
                let Some(body) = read_mem(&mut caller, body_ptr, body_len) else { return -1 };
                if method != "POST" {
                    return -3;
                }
                if !url.starts_with("https://") {
                    return -2;
                }
                caller.data().http_calls.lock().unwrap().push(HttpCall {
                    method,
                    url,
                    body: String::from_utf8_lossy(&body).into_owned(),
                });
                // 把假响应写回 resp 缓冲，截断到容量。
                let n = FAKE_RESPONSE.len().min(resp_cap.max(0) as usize);
                if resp_ptr > 0 && n > 0 && !write_mem(&mut caller, resp_ptr, &FAKE_RESPONSE[..n]) {
                    return -1;
                }
                n as i32
            },
        )
        .unwrap();

    let instance = linker.instantiate(&mut store, &module).expect("应满足导出契约并实例化");
    (store, instance)
}

/// 按生产路径驱动一次 on_event：经 __alloc 分配载荷缓冲、写入、调用。
fn send_event(store: &mut Store<Host>, instance: &wasmtime::Instance, payload: &str) -> i32 {
    let alloc = instance.get_typed_func::<(i32,), i32>(&mut *store, "__alloc").unwrap();
    let on_event = instance.get_typed_func::<(i32, i32), i32>(&mut *store, "on_event").unwrap();
    let mem: Memory = instance.get_memory(&mut *store, "memory").unwrap();
    let bytes = payload.as_bytes();
    let ptr = alloc.call(&mut *store, (bytes.len() as i32,)).unwrap();
    mem.data_mut(&mut *store)[ptr as usize..ptr as usize + bytes.len()].copy_from_slice(bytes);
    on_event.call(&mut *store, (ptr, bytes.len() as i32)).unwrap()
}

/// 三种事件的载荷样例与期望文案片段。
const CASES: &[(&str, &str)] = &[
    (
        r#"{"type":"plugin_expiry_soon","node_id":7,"name":"edge-1","expires_at":"2026-10-01","days_left":7,"threshold_days":7}"#,
        "⏰ 节点 edge-1 将于 2026-10-01 到期（剩 7 天）",
    ),
    (
        r#"{"type":"agent_offline","node_id":5,"name":"edge-1","observed_at":100,"last_seen_at":90}"#,
        "🔴 节点 edge-1 已离线",
    ),
    (r#"{"type":"agent_online","node_id":5,"name":"edge-1","observed_at":300}"#, "🟢 节点 edge-1 已恢复在线"),
];

/// 三种事件都渲染成中文文案发到 Telegram，URL 带上 kv 里的 bot token。
#[test]
fn delivers_all_three_events_to_telegram() {
    let wasm = build_wasm();
    let mut host = Host::default();
    host.kv.insert("bot_token".into(), "123456:TEST-TOKEN".into());
    host.kv.insert("chat_id".into(), "-100200300".into());
    let (mut store, instance) = instantiate(&engine(), &wasm, host);

    for (payload, _) in CASES {
        let code = send_event(&mut store, &instance, payload);
        assert_eq!(code, 0, "事件 {payload} 应成功，实际返回 {code}");
    }

    let calls = store.data().http_calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 3, "三种事件各发一条 sendMessage");
    for (call, (_, expected_text)) in calls.iter().zip(CASES) {
        assert_eq!(call.method, "POST", "v1 宿主只接受 POST");
        assert_eq!(call.url, "https://api.telegram.org/bot123456:TEST-TOKEN/sendMessage");
        assert!(call.body.contains(r#""chat_id":"-100200300""#), "body 应带 chat_id：{}", call.body);
        assert!(call.body.contains(expected_text), "body 应含事件文案：{}", call.body);
    }
}

/// kv 没配置时返回明确的错误码、不发 http：缺 bot_token → 2，缺 chat_id → 3。
#[test]
fn missing_config_is_reported_without_an_http_call() {
    let wasm = build_wasm();

    // 两个配置都缺：bot_token 先被检查。
    let (mut store, instance) = instantiate(&engine(), &wasm, Host::default());
    assert_eq!(send_event(&mut store, &instance, CASES[0].0), 2, "缺 bot_token 应返回 2");
    assert!(store.data().http_calls.lock().unwrap().is_empty(), "没配置就不该发请求");

    // 只缺 chat_id。
    let mut host = Host::default();
    host.kv.insert("bot_token".into(), "123456:TEST-TOKEN".into());
    let (mut store, instance) = instantiate(&engine(), &wasm, host);
    assert_eq!(send_event(&mut store, &instance, CASES[0].0), 3, "缺 chat_id 应返回 3");
    assert!(store.data().http_calls.lock().unwrap().is_empty());
    // warn 级日志应已打到桩宿主。
    let logs = store.data().logs.lock().unwrap();
    assert!(logs.iter().any(|(level, text)| *level == 2 && text.contains("chat_id")), "{logs:?}");
}

/// 非法 JSON 载荷返回 1 而不是 trap：坏输入是错误码，不是崩溃。
#[test]
fn a_broken_payload_returns_an_error_code() {
    let wasm = build_wasm();
    let mut host = Host::default();
    host.kv.insert("bot_token".into(), "t".into());
    host.kv.insert("chat_id".into(), "c".into());
    let (mut store, instance) = instantiate(&engine(), &wasm, host);
    assert_eq!(send_event(&mut store, &instance, "not json"), 1);
    assert!(store.data().http_calls.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// 模板（v1.1.0）：读 kv 模板渲染、没配就用内置文案
// ---------------------------------------------------------------------------

/// 最后一条 sendMessage 的 JSON 体。模板里可能有换行，比对整个 body 字符串会被
/// JSON 的 `\n` 转义绕进去，所以解出来再断言。
fn last_body(store: &Store<Host>) -> serde_json::Value {
    let calls = store.data().http_calls.lock().unwrap();
    let call = calls.last().expect("应当发过一次 sendMessage");
    serde_json::from_str(&call.body).expect("body 是 JSON")
}

/// 最后一条消息的文本。
fn last_text(store: &Store<Host>) -> String {
    last_body(store)["text"].as_str().expect("text 是字符串").to_owned()
}

/// plugin.toml 里某个 `[[kv]]` 声明的 default（面板预填给操作员的那份基准）。
fn manifest_default(key: &str) -> String {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/plugin.toml"))
        .expect("读 plugin.toml");
    let parsed: toml::Value = toml::from_str(&text).expect("plugin.toml 是合法 TOML");
    parsed["kv"]
        .as_array()
        .expect("[[kv]] 是数组")
        .iter()
        .find(|decl| decl["key"].as_str() == Some(key))
        .and_then(|decl| decl["default"].as_str())
        .unwrap_or_else(|| panic!("plugin.toml 里 `{key}` 应当声明 default"))
        .to_owned()
}

/// 一个只配了渠道、没配模板的桩宿主。
fn bare_host() -> Host {
    let mut host = Host::default();
    host.kv.insert("bot_token".into(), "t".into());
    host.kv.insert("chat_id".into(), "c".into());
    host
}

/// 漂移守卫：不配模板时真发出去的文案，必须与 plugin.toml 声明的那份 default
/// 渲染出完全一样的结果。
///
/// 两处文案各有用途——manifest 的 default 是面板预填的基准，代码里的常量是没配
/// 模板时真会发的东西——它们一旦漂移，操作员「改一个字」改的就不是插件真会发的
/// 那段话，而这件事没有任何编译期信号。桩宿主的时钟是常量，两次运行看到的是同一
/// 时刻，所以离线文案里算出来的静默时长也一致。
#[test]
fn built_in_text_behaves_exactly_like_the_manifest_default() {
    let wasm = build_wasm();
    let cases = [
        (CASES[0].0, "template_expiry_soon"),
        (CASES[1].0, "template_agent_offline"),
        (CASES[2].0, "template_agent_online"),
    ];
    for (payload, key) in cases {
        let (mut store, instance) = instantiate(&engine(), &wasm, bare_host());
        assert_eq!(send_event(&mut store, &instance, payload), 0, "{key}");
        let builtin = last_text(&store);

        let mut declared = bare_host();
        declared.kv.insert(key.into(), manifest_default(key));
        let (mut store, instance) = instantiate(&engine(), &wasm, declared);
        assert_eq!(send_event(&mut store, &instance, payload), 0, "{key}");
        assert_eq!(
            last_text(&store),
            builtin,
            "{key}：plugin.toml 的 default 与插件内置文案漂移了"
        );
    }
}

/// 自定义模板：多行原样保留，插值进去的字段值做 HTML 转义，解析模式是 HTML。
#[test]
fn a_custom_template_is_rendered_with_escaped_values() {
    let wasm = build_wasm();
    let mut host = bare_host();
    host.kv.insert(
        "template_agent_offline".into(),
        "<b>{name}</b> 掉了\n已静默 {silent_for} 秒".into(),
    );
    let (mut store, instance) = instantiate(&engine(), &wasm, host);

    // 节点名是别人给的: `<` 与 `&` 会改坏消息结构; `"` 在属性里(如
    // `<a href="…{name}…">`)也会提前闭合属性——三种都要转。
    let payload = r#"{"type":"agent_offline","node_id":5,"name":"<script>&\"x","observed_at":1,"last_seen_at":1799999700}"#;
    assert_eq!(send_event(&mut store, &instance, payload), 0);
    assert_eq!(last_text(&store), "<b>&lt;script&gt;&amp;&quot;x</b> 掉了\n已静默 300 秒");
    assert_eq!(last_body(&store)["parse_mode"], "HTML");
}

/// 认不出的占位符原样留在消息里、仍然派发成功，并打一条 warn——模板写错一个名字
/// 不该让整条通知消失，那是「什么都没收到」级别的故障。
#[test]
fn an_unknown_placeholder_stays_and_warns() {
    let wasm = build_wasm();
    let mut host = bare_host();
    host.kv.insert("template_agent_online".into(), "{nmae} 上线了".into());
    let (mut store, instance) = instantiate(&engine(), &wasm, host);

    assert_eq!(send_event(&mut store, &instance, CASES[2].0), 0, "写错占位符不该让派发失败");
    assert_eq!(last_text(&store), "{nmae} 上线了");
    let logs = store.data().logs.lock().unwrap();
    assert!(
        logs.iter().any(|(level, text)| *level == 2 && text.contains("nmae")),
        "要有一条点名该占位符的 warn：{logs:?}"
    );
}

/// 模板是纯空白 = 没配：回退内置文案，与「kv 里没有这一行」完全一样（操作员清空
/// 模板就是回到内置，不是发一条空白消息）。
#[test]
fn a_blank_template_falls_back_to_built_in() {
    let wasm = build_wasm();
    let mut host = bare_host();
    host.kv.insert("template_agent_online".into(), "  \n ".into());
    let (mut store, instance) = instantiate(&engine(), &wasm, host);

    assert_eq!(send_event(&mut store, &instance, CASES[2].0), 0);
    assert_eq!(last_text(&store), CASES[2].1);
}
